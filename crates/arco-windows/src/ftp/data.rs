use super::{Session, TransferMode, Url, check_cancel, run, run_observed};
use flate2::{Compression, Decompress, FlushDecompress, Status, write::ZlibEncoder};
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::Path;
use std::sync::atomic::AtomicBool;
use tempfile::NamedTempFile;

impl Session {
    pub(super) fn receive_file(
        &self,
        url: &Url,
        output: &File,
        mode: TransferMode,
        limit: Option<u64>,
        cancel: &AtomicBool,
        mut observe: impl FnMut() -> io::Result<()>,
    ) -> io::Result<()> {
        if self.is_sftp() {
            if mode == TransferMode::Text {
                return Err(super::sftp::binary_only());
            }
            return self.receive_sftp_file(url, output, 0, limit, cancel, |_| observe());
        }
        if !self.security().is_ftp() {
            if mode == TransferMode::Text {
                return Err(super::sftp::binary_only());
            }
            if self.security() == super::Security::Scp {
                return self
                    .receive_scp(url, output, limit, cancel, |_, _| observe())
                    .map(|_| ());
            }
            return self.receive_http(url, output, limit, cancel, observe);
        }
        let limit_argument = limit.map(|bytes| bytes.max(1).to_string());
        let mut arguments = Vec::new();
        if self.connection.options.mode_z {
            let packed = NamedTempFile::new()?;
            arguments.extend(["--quote", "MODE Z", "--ignore-content-length"]);
            if mode == TransferMode::Text {
                // Set the server representation after curl's default TYPE I.
                // The client must transport the compressed bytes unchanged.
                arguments.extend(["--quote", "+TYPE A"]);
            }
            run_observed(
                self.transfer_command(url)?,
                &arguments,
                Some(packed.reopen()?),
                cancel,
                || {
                    if let Some(limit) = limit
                        && packed.as_file().metadata()?.len()
                            > limit.saturating_add(limit / 8).saturating_add(1024)
                    {
                        return Err(io::Error::other(
                            "Der komprimierte FTP-Transfer überschreitet das Limit.",
                        ));
                    }
                    Ok(())
                },
            )?;
            return inflate(
                BufReader::new(packed.reopen()?),
                output,
                mode,
                limit,
                cancel,
                observe,
            );
        }
        if mode == TransferMode::Text {
            if !self.connection.options.otp {
                // curl switches stdout to text mode on Windows for TYPE A.
                // Use its binary file output to avoid an extra CR before LF.
                let text = NamedTempFile::new()?;
                let path = text
                    .path()
                    .to_str()
                    .ok_or_else(|| io::Error::other("curl benötigt einen Unicode-Dateipfad."))?;
                run_observed(
                    self.transfer_command(url)?,
                    &["--use-ascii", "--ignore-content-length", "--output", path],
                    None,
                    cancel,
                    &mut observe,
                )?;
                let mut incoming = text.reopen()?;
                let mut output = output;
                let mut buffer = [0; 64 * 1024];
                loop {
                    check_cancel(cancel)?;
                    let count = incoming.read(&mut buffer)?;
                    if count == 0 {
                        break;
                    }
                    output.write_all(&buffer[..count])?;
                    observe()?;
                }
                return observe();
            }
            arguments.extend(["--use-ascii", "--ignore-content-length"]);
        }
        if let Some(limit) = &limit_argument {
            arguments.extend(["--max-filesize", limit]);
        }
        run_observed(
            self.transfer_command(url)?,
            &arguments,
            Some(output.try_clone()?),
            cancel,
            || {
                if let Some(limit) = limit
                    && output.metadata()?.len() > limit
                {
                    return Err(io::Error::other(
                        "Der FTP-Download überschreitet das Größenlimit.",
                    ));
                }
                observe()
            },
        )?;
        Ok(())
    }

    pub(super) fn send_file(
        &self,
        url: &Url,
        source: &Path,
        mode: TransferMode,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        check_cancel(cancel)?;
        let mut arguments = Vec::new();
        let mut packed = None;
        if self.connection.options.mode_z {
            let staged = NamedTempFile::new()?;
            let mut encoder = ZlibEncoder::new(staged.reopen()?, Compression::default());
            let mut source_file = File::open(source)?;
            let mut buffer = [0; 64 * 1024];
            let mut converted = Vec::new();
            let mut previous_cr = false;
            loop {
                check_cancel(cancel)?;
                let count = source_file.read(&mut buffer)?;
                if count == 0 {
                    break;
                }
                if mode == TransferMode::Text {
                    encode_nvt(&buffer[..count], &mut previous_cr, &mut converted);
                    encoder.write_all(&converted)?;
                } else {
                    encoder.write_all(&buffer[..count])?;
                }
            }
            if previous_cr {
                encoder.write_all(b"\r\0")?;
            }
            encoder.finish()?;
            packed = Some(staged);
            arguments.extend(["--quote", "MODE Z"]);
            if mode == TransferMode::Text {
                arguments.extend(["--quote", "+TYPE A"]);
            }
        } else if mode == TransferMode::Text {
            arguments.push("--use-ascii");
        }
        let source = packed
            .as_ref()
            .map_or(source, |file| file.path())
            .to_str()
            .ok_or_else(|| io::Error::other("curl benötigt einen Unicode-Dateipfad."))?;
        arguments.extend(["--upload-file", source]);
        run(self.transfer_command(url)?, &arguments, None, cancel).map(|_| ())
    }
}

fn inflate(
    mut input: impl BufRead,
    mut output: impl Write,
    mode: TransferMode,
    limit: Option<u64>,
    cancel: &AtomicBool,
    mut observe: impl FnMut() -> io::Result<()>,
) -> io::Result<()> {
    let mut decoder = Decompress::new(true);
    let mut buffer = [0; 64 * 1024];
    let mut converted = Vec::new();
    let mut previous_cr = false;
    loop {
        check_cancel(cancel)?;
        let chunk = input.fill_buf()?;
        let consumed = decoder.total_in();
        let written = decoder.total_out();
        let status = decoder
            .decompress(chunk, &mut buffer, FlushDecompress::None)
            .map_err(io::Error::other)?;
        let consumed = (decoder.total_in() - consumed) as usize;
        let written = (decoder.total_out() - written) as usize;
        if limit.is_some_and(|limit| decoder.total_out() > limit) {
            return Err(io::Error::other(
                "Die entpackte FTP-Datei überschreitet das Größenlimit.",
            ));
        }
        if mode == TransferMode::Text {
            decode_nvt(&buffer[..written], &mut previous_cr, &mut converted);
            output.write_all(&converted)?;
        } else {
            output.write_all(&buffer[..written])?;
        }
        input.consume(consumed);
        if status == Status::StreamEnd {
            if !input.fill_buf()?.is_empty() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Zusätzliche Daten nach FTP-MODE-Z-Stream.",
                ));
            }
            if previous_cr {
                output.write_all(b"\r")?;
            }
            return observe();
        }
        if consumed == 0 && written == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "Unvollständiger FTP-MODE-Z-Stream.",
            ));
        }
        observe()?;
    }
}

pub(super) fn encode_nvt(input: &[u8], previous_cr: &mut bool, output: &mut Vec<u8>) {
    output.clear();
    for &byte in input {
        if *previous_cr && byte != b'\n' {
            output.extend_from_slice(b"\r\0");
        }
        if byte == b'\n' {
            output.extend_from_slice(b"\r\n");
        } else if byte != b'\r' {
            output.push(byte);
        }
        *previous_cr = byte == b'\r';
    }
}

pub(super) fn decode_nvt(input: &[u8], previous_cr: &mut bool, output: &mut Vec<u8>) {
    output.clear();
    for &byte in input {
        if *previous_cr {
            output.push(b'\r');
        }
        if byte != b'\r' && !(*previous_cr && byte == 0) {
            output.push(byte);
        }
        *previous_cr = byte == b'\r';
    }
}
