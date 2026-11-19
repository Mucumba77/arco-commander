use super::{
    path, runtime,
    transport::{SshConnection, bounded},
};
use crate::ftp::rate::Rate;
use crate::ftp::{
    DownloadPhase, DownloadProgress, Entry, LIST_LIMIT, ListFormat, Listing, Session, Url,
    check_cancel, check_segment, invalid_input,
};
use russh::{Channel, ChannelMsg, client};
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::Path;
use std::sync::atomic::AtomicBool;
use tempfile::NamedTempFile;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

const BUFFER: usize = 32 * 1024;

impl Session {
    pub fn scp_download(
        &self,
        directory: &Url,
        remote: &str,
        destination: &Path,
        cancel: &AtomicBool,
        mut progress: impl FnMut(DownloadProgress),
    ) -> io::Result<()> {
        if destination.try_exists()? {
            return Err(io::ErrorKind::AlreadyExists.into());
        }
        let expected = if self.preserve_transfer_time(false) {
            Some(
                self.list_sync(directory, cancel)?
                    .regular_file(remote)?
                    .clone(),
            )
        } else {
            None
        };
        let remote = self.file(directory, remote)?;
        let parent = destination
            .parent()
            .ok_or_else(|| invalid_input("Lokaler Zielordner fehlt."))?;
        let temporary = NamedTempFile::new_in(parent)?;
        let size = self.receive_scp(
            &remote,
            temporary.as_file(),
            None,
            cancel,
            |received, size| {
                report(&mut progress, received, size, DownloadPhase::Receiving);
                Ok(())
            },
        )?;
        report(&mut progress, size, size, DownloadPhase::Verifying);
        if let Some(expected) = expected {
            self.preserve_download_time(directory, &expected, temporary.as_file(), cancel)?;
        }
        temporary.as_file().sync_all()?;
        check_cancel(cancel)?;
        temporary
            .persist_noclobber(destination)
            .map_err(|error| error.error)?;
        Ok(())
    }

    pub(in crate::ftp) fn receive_scp(
        &self,
        url: &Url,
        mut output: &File,
        limit: Option<u64>,
        cancel: &AtomicBool,
        mut observe: impl FnMut(u64, u64) -> io::Result<()>,
    ) -> io::Result<u64> {
        self.validate(url)?;
        let timeout = self.scp_timeout()?;
        let remote = path(url)?;
        let name = remote.rsplit('/').next().unwrap_or_default();
        self.with_scp(cancel, async |ssh| {
            let command = format!("scp -f -- {}", shell_arg(&remote));
            let mut channel = ssh.exec(command, timeout, cancel).await?;
            let result = async {
                let mut writer = channel.make_writer();
                let mut reader = channel.make_reader();
                let mut buffer = [0; BUFFER];
                bounded(cancel, timeout, async { writer.write_u8(0).await }).await?;
                let size = bounded(cancel, timeout, header(&mut reader, name)).await?;
                if limit.is_some_and(|limit| size > limit) {
                    return Err(invalid_input(
                        "Die SCP-Datei überschreitet das Größenlimit.",
                    ));
                }
                bounded(cancel, timeout, async { writer.write_u8(0).await }).await?;
                let mut received = 0;
                let rate = self.transfer_rate();
                while received < size {
                    let count = (size - received).min(rate.chunk(buffer.len()) as u64) as usize;
                    bounded(cancel, timeout, async {
                        reader.read_exact(&mut buffer[..count]).await.map(|_| ())
                    })
                    .await?;
                    received += count as u64;
                    rate.wait_async(received, cancel).await?;
                    output.write_all(&buffer[..count])?;
                    observe(received, size)?;
                }
                bounded(cancel, timeout, ack(&mut reader)).await?;
                bounded(cancel, timeout, async { writer.write_u8(0).await }).await?;
                drop(reader);
                finish(&mut channel, timeout, cancel).await?;
                Ok(size)
            }
            .await;
            if result.is_err() {
                close_scp_channel(&channel, timeout).await;
            }
            result
        })
    }

    pub fn scp_upload(
        &self,
        directory: &Url,
        source: &Path,
        remote: &str,
        cancel: &AtomicBool,
        mut progress: impl FnMut(DownloadProgress),
    ) -> io::Result<()> {
        let timeout = self.scp_timeout()?;
        let mut file = File::open(source)?;
        let initial = file.metadata()?;
        if !initial.is_file() {
            return Err(invalid_input("Wähle eine reguläre lokale Datei."));
        }
        let size = initial.len();
        let modified = if self.preserve_transfer_time(false) {
            Some(chrono::DateTime::<chrono::Utc>::from(initial.modified()?).timestamp())
        } else {
            None
        };
        let destination = shell_arg(&path(&self.file(directory, remote)?)?);
        let reservation = tempfile::Builder::new().prefix(".arco-scp-").tempfile()?;
        let stage_name = reservation
            .path()
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| invalid_input("Temporärer SCP-Dateiname fehlt."))?;
        let stage = path(&self.file(directory, stage_name)?)?;
        let staged_file = shell_arg(&format!("{stage}/payload"));
        let stage = shell_arg(&stage);
        self.with_scp(cancel, async |ssh| {
            shell(ssh, format!("test ! -e {destination} && test ! -L {destination}"), timeout, cancel).await
                .map_err(|error| if error.kind() == io::ErrorKind::Other {
                    io::Error::new(io::ErrorKind::AlreadyExists, "Das SCP-Ziel ist bereits vorhanden.")
                } else { error })?;
            shell(ssh, format!("umask 077; mkdir -- {stage}"), timeout, cancel).await?;
            let transferred = async {
                put(ssh, &stage, &mut file, size, cancel, timeout, self.transfer_rate(), &mut progress).await?;
                if file.metadata()?.len() != size || file.metadata()?.modified()? != initial.modified()? {
                    return Err(invalid_input("Die lokale SCP-Quelle wurde während der Übertragung geändert."));
                }
                if let Some(time) = modified {
                    shell(ssh, format!("touch -h -c -m -d @{time} -- {staged_file}"), timeout, cancel).await?;
                }
                report(&mut progress, size, size, DownloadPhase::Verifying);
                // A hard link in the same directory tree publishes the file atomically
                // and fails if any entry already occupies the destination name.
                shell(
                    ssh,
                    format!("test -f {staged_file} && test ! -L {staged_file} && ln -T -- {staged_file} {destination}"),
                    timeout, cancel,
                ).await
            }.await;
            let cleanup = shell(
                ssh, format!("rm -f -- {staged_file} && rmdir -- {stage}"),
                timeout.min(5), &AtomicBool::new(false),
            ).await;
            match (transferred, cleanup) {
                (Ok(()), Ok(())) => Ok(()),
                (Ok(()), Err(_)) => Err(io::Error::other(format!("SCP-Datei wurde veröffentlicht. Temporären Ordner {stage_name} auf dem Server prüfen."))),
                (Err(error), Ok(())) => Err(error),
                (Err(error), Err(_)) => Err(io::Error::new(error.kind(), format!("{error} Temporären SCP-Ordner {stage_name} auf dem Server prüfen."))),
            }
        })
    }

    fn scp_timeout(&self) -> io::Result<u64> {
        self.connection
            .ssh
            .as_ref()
            .map(|settings| settings.timeout)
            .ok_or_else(|| invalid_input("SCP benötigt ein SSH-Profil mit Hostschlüssel."))
    }

    fn with_scp<T>(
        &self,
        cancel: &AtomicBool,
        work: impl AsyncFnOnce(&SshConnection) -> io::Result<T>,
    ) -> io::Result<T> {
        let mut cached = loop {
            check_cancel(cancel)?;
            match self.scp.try_lock() {
                Ok(value) => break value,
                Err(std::sync::TryLockError::WouldBlock) => {
                    std::thread::sleep(std::time::Duration::from_millis(25))
                }
                Err(std::sync::TryLockError::Poisoned(_)) => {
                    return Err(io::Error::other("SCP-Sitzung beendet."));
                }
            }
        };
        runtime()?.block_on(async {
            if cached.as_ref().is_none_or(SshConnection::is_closed) {
                *cached = Some(self.open_ssh(cancel).await?);
            }
            work(cached.as_ref().expect("initialized SCP session")).await
        })
    }

    pub(in crate::ftp) fn list_scp(
        &self,
        directory: &Url,
        cancel: &AtomicBool,
    ) -> io::Result<Listing> {
        self.validate(directory)?;
        let target = shell_arg(&path(directory)?);
        let command = format!(
            "LC_ALL=C find {target} -mindepth 1 -maxdepth 1 -printf '%f\\0%y\\0%s\\0%T@\\0%i\\0'"
        );
        let timeout = self.scp_timeout()?;
        let mut data = self.with_scp(cancel, async |ssh| {
            let mut channel = ssh.exec(command, timeout, cancel).await?;
            let mut data = Vec::new();
            bounded(cancel, timeout, async {
                let mut status = None;
                while let Some(message) = channel.wait().await {
                    match message {
                        ChannelMsg::Data { data: bytes } => {
                            if data.len().saturating_add(bytes.len()) > LIST_LIMIT {
                                return Err(invalid_input(
                                    "Der SCP-Ordner enthält zu viele Daten.",
                                ));
                            }
                            data.extend_from_slice(&bytes);
                        }
                        ChannelMsg::ExitStatus { exit_status } => status = Some(exit_status),
                        ChannelMsg::Close => break,
                        _ => (),
                    }
                }
                if status != Some(0) {
                    return Err(io::Error::other(
                        "Der SCP-Server konnte den Ordner mit GNU find nicht auflisten.",
                    ));
                }
                Ok(())
            })
            .await?;
            Ok(data)
        })?;
        if data.is_empty() {
            return Ok(Listing {
                entries: Vec::new(),
                format: ListFormat::Scp,
            });
        }
        if data.pop() != Some(0) {
            return Err(invalid_input("Unvollständige SCP-Verzeichnisliste."));
        }
        let text = std::str::from_utf8(&data).map_err(io::Error::other)?;
        let fields: Vec<_> = text.split('\0').collect();
        if fields.len() % 5 != 0 {
            return Err(invalid_input("Ungültige SCP-Verzeichnisliste."));
        }
        let mut entries = Vec::new();
        for row in fields.chunks_exact(5) {
            check_cancel(cancel)?;
            let directory = match row[1] {
                "d" => true,
                "f" => false,
                _ => continue,
            };
            check_segment(row[0])?;
            let stamp = row[3]
                .split('.')
                .next()
                .and_then(|value| value.parse().ok())
                .and_then(|value| chrono::DateTime::from_timestamp(value, 0))
                .map(|value| value.format("%Y%m%d%H%M%S").to_string());
            entries.push(Entry {
                name: row[0].to_owned(),
                directory: Some(directory),
                size: (!directory).then(|| row[2].parse().ok()).flatten(),
                modified: stamp,
                unique: Some(row[4].to_owned()),
            });
        }
        Ok(Listing {
            entries,
            format: ListFormat::Scp,
        })
    }

    pub(in crate::ftp) fn scp_mkdir(&self, url: &Url, cancel: &AtomicBool) -> io::Result<()> {
        self.scp_command(format!("mkdir -- {}", shell_arg(&path(url)?)), cancel)
    }

    pub(in crate::ftp) fn scp_remove(
        &self,
        url: &Url,
        directory: bool,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        self.scp_command(
            format!(
                "{} -- {}",
                if directory { "rmdir" } else { "rm" },
                shell_arg(&path(url)?)
            ),
            cancel,
        )
    }

    pub(in crate::ftp) fn scp_rename(
        &self,
        from: &Url,
        to: &Url,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        let from = shell_arg(&path(from)?);
        let to = shell_arg(&path(to)?);
        self.scp_command(
            format!("mv -T -n -- {from} {to} && test ! -e {from} && test ! -L {from}"),
            cancel,
        )
        .map_err(|error| {
            if error.kind() == io::ErrorKind::Other {
                io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "SCP-Name belegt oder Umbenennen auf dem Server fehlgeschlagen.",
                )
            } else {
                error
            }
        })
    }

    pub(in crate::ftp) fn scp_metadata(
        &self,
        url: &Url,
        permissions: Option<u16>,
        stamp: Option<&str>,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        let target = shell_arg(&path(url)?);
        if let Some(mode) = permissions {
            self.scp_command(format!("chmod {mode:o} -- {target}"), cancel)?;
        }
        if let Some(stamp) = stamp {
            let time = chrono::NaiveDateTime::parse_from_str(stamp, "%Y%m%d%H%M%S")
                .map_err(io::Error::other)?
                .and_utc()
                .timestamp();
            self.scp_command(format!("touch -h -c -m -d @{time} -- {target}"), cancel)?;
        }
        Ok(())
    }

    fn scp_command(&self, command: String, cancel: &AtomicBool) -> io::Result<()> {
        let timeout = self.scp_timeout()?;
        self.with_scp(cancel, async |ssh| {
            shell(ssh, command, timeout, cancel).await
        })
    }
}

async fn put(
    ssh: &SshConnection,
    stage: &str,
    source: &mut File,
    size: u64,
    cancel: &AtomicBool,
    timeout: u64,
    rate: Rate,
    progress: &mut impl FnMut(DownloadProgress),
) -> io::Result<()> {
    let mut channel = ssh
        .exec(format!("scp -t -- {stage}"), timeout, cancel)
        .await?;
    let result = async {
        let mut writer = channel.make_writer();
        let mut reader = channel.make_reader();
        bounded(cancel, timeout, ack(&mut reader)).await?;
        bounded(cancel, timeout, async {
            writer
                .write_all(format!("C0600 {size} payload\n").as_bytes())
                .await
        })
        .await?;
        bounded(cancel, timeout, ack(&mut reader)).await?;
        let mut buffer = [0; BUFFER];
        let mut sent = 0;
        while sent < size {
            check_cancel(cancel)?;
            let count = (size - sent).min(rate.chunk(buffer.len()) as u64) as usize;
            source.read_exact(&mut buffer[..count])?;
            rate.wait_async(sent + count as u64, cancel).await?;
            bounded(cancel, timeout, async {
                writer.write_all(&buffer[..count]).await
            })
            .await?;
            sent += count as u64;
            report(progress, sent, size, DownloadPhase::Receiving);
        }
        bounded(cancel, timeout, async { writer.write_u8(0).await }).await?;
        bounded(cancel, timeout, ack(&mut reader)).await?;
        drop(reader);
        finish(&mut channel, timeout, cancel).await
    }
    .await;
    if result.is_err() {
        close_scp_channel(&channel, timeout).await;
    }
    result
}

async fn close_scp_channel(channel: &Channel<client::Msg>, timeout: u64) {
    let _ = bounded(&AtomicBool::new(false), timeout.min(5), async {
        channel.close().await.map_err(io::Error::other)
    })
    .await;
}

fn report(
    progress: &mut impl FnMut(DownloadProgress),
    received: u64,
    size: u64,
    phase: DownloadPhase,
) {
    progress(DownloadProgress {
        received,
        total: Some(size),
        phase,
    });
}

pub(super) fn shell_arg(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

async fn header(input: &mut (impl AsyncRead + Unpin), expected: &str) -> io::Result<u64> {
    let mut line = Vec::with_capacity(128);
    for _ in 0..4096 {
        let byte = input.read_u8().await?;
        if byte == b'\n' {
            return parse_header(&line, expected);
        }
        line.push(byte);
    }
    Err(invalid_input("Der SCP-Dateikopf ist zu lang."))
}

fn parse_header(line: &[u8], expected: &str) -> io::Result<u64> {
    let invalid = || invalid_input("Der SCP-Server hat einen unerwarteten Dateikopf gesendet.");
    let line = std::str::from_utf8(line).map_err(|_| invalid())?;
    let mut parts = line.splitn(3, ' ');
    let (Some(mode), Some(size), Some(name)) = (parts.next(), parts.next(), parts.next()) else {
        return Err(invalid());
    };
    if mode.len() != 5
        || !mode.starts_with('C')
        || !mode
            .bytes()
            .skip(1)
            .all(|byte| (b'0'..=b'7').contains(&byte))
        || name != expected
        || size.is_empty()
        || !size.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(invalid());
    }
    size.parse().map_err(|_| invalid())
}

async fn ack(input: &mut (impl AsyncRead + Unpin)) -> io::Result<()> {
    match input.read_u8().await? {
        0 => Ok(()),
        _ => Err(io::Error::other(
            "Der SCP-Server hat die Dateiübertragung abgelehnt.",
        )),
    }
}

async fn shell(
    ssh: &SshConnection,
    command: String,
    timeout: u64,
    cancel: &AtomicBool,
) -> io::Result<()> {
    let mut channel = ssh.exec(command, timeout, cancel).await?;
    finish(&mut channel, timeout, cancel).await
}

async fn finish(
    channel: &mut Channel<client::Msg>,
    timeout: u64,
    cancel: &AtomicBool,
) -> io::Result<()> {
    bounded(cancel, timeout, async {
        channel.eof().await.map_err(io::Error::other)?;
        let mut exit = None;
        while let Some(message) = channel.wait().await {
            match message {
                ChannelMsg::ExitStatus { exit_status } => exit = Some(exit_status),
                ChannelMsg::Close => break,
                _ => {}
            }
        }
        match exit {
            Some(0) => Ok(()),
            Some(status) => Err(io::Error::other(format!(
                "Der SCP-Server meldet Status {status}."
            ))),
            None => Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "Der SSH-Server hat den SCP-Status nicht bestätigt.",
            )),
        }
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(windows)]
    #[ignore = "requires scripts/test-sftp-reference.py and Windows OpenSSH"]
    fn reference_scp_only_transfer() {
        use crate::ftp::{Connection, Security};
        use std::fs;
        let cancel = AtomicBool::new(false);
        let session = Connection::new(
            &std::env::var("ARCO_SCP_TEST_URL").unwrap(),
            "arco",
            Security::Sftp,
        )
        .unwrap()
        .with_ssh(&std::env::var("ARCO_SCP_TEST_FINGERPRINT").unwrap(), None)
        .unwrap()
        .authenticate("fixture-only")
        .unwrap();
        let directory = session.initial_directory();
        assert!(session.list(&directory, &cancel).is_err());
        let folder = tempfile::tempdir().unwrap();
        let source = folder.path().join("source");
        let destination = folder.path().join("download");
        let body: Vec<_> = (0..230_000).map(|i| (i % 231) as u8).collect();
        fs::write(&source, &body).unwrap();
        let remote = "Arco's report #1.txt";
        session
            .scp_upload(&directory, &source, remote, &cancel, |_| {})
            .unwrap();
        session
            .scp_download(&directory, remote, &destination, &cancel, |_| {})
            .unwrap();
        assert_eq!(fs::read(&destination).unwrap(), body);
        assert_eq!(
            session
                .scp_upload(&directory, &source, remote, &cancel, |_| {})
                .unwrap_err()
                .kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(
            session
                .scp_download(&directory, remote, &destination, &cancel, |_| {})
                .unwrap_err()
                .kind(),
            io::ErrorKind::AlreadyExists
        );
    }

    #[test]
    #[cfg(windows)]
    #[ignore = "requires scripts/test-sftp-reference.py and Windows OpenSSH"]
    fn reference_scp_panel() {
        use crate::ftp::{Connection, Security};
        use std::fs;
        let address = std::env::var("ARCO_SCP_TEST_URL")
            .unwrap()
            .replacen("sftp://", "scp://", 1);
        let pin = std::env::var("ARCO_SCP_TEST_FINGERPRINT").unwrap();
        let session = Connection::new(&address, "arco", Security::Scp)
            .unwrap()
            .with_ssh(&pin, None)
            .unwrap()
            .authenticate("fixture-only")
            .unwrap();
        let cancel = AtomicBool::new(false);
        let root = session.initial_directory();
        assert_eq!(
            session.list(&root, &cancel).unwrap().format,
            ListFormat::Scp
        );
        session
            .create_sync_directory(&root, "Arco's folder", &cancel)
            .unwrap();
        let folder = session
            .relative_directory(&root, Path::new("Arco's folder"))
            .unwrap();
        assert_eq!(session.parent_directory(&folder).unwrap(), root);
        let local = tempfile::tempdir().unwrap();
        let input = local.path().join("input");
        let output = local.path().join("output");
        fs::write(&input, b"native SCP panel transfer").unwrap();
        let name = "example report.txt";
        session
            .upload(&folder, &input, name, false, &cancel)
            .unwrap();
        let old = session
            .list_sync(&folder, &cancel)
            .unwrap()
            .entries
            .pop()
            .unwrap();
        assert_eq!(old.name, name);
        session
            .set_ssh_modified(&folder, &old, "20250102030405", &cancel)
            .unwrap();
        let current = session
            .list_sync(&folder, &cancel)
            .unwrap()
            .entries
            .pop()
            .unwrap();
        assert_eq!(current.modified.as_deref(), Some("20250102030405"));
        session
            .rename_sync_entry(&folder, &current, "result.txt", &cancel)
            .unwrap();
        session
            .download(&folder, "result.txt", &output, &cancel)
            .unwrap();
        assert_eq!(fs::read(output).unwrap(), b"native SCP panel transfer");
        let entry = session
            .list_sync(&folder, &cancel)
            .unwrap()
            .entries
            .pop()
            .unwrap();
        session.delete_sync_entry(&folder, &entry, &cancel).unwrap();

        let timestamp = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_031);
        File::options()
            .write(true)
            .open(&input)
            .unwrap()
            .set_modified(timestamp)
            .unwrap();
        let settings = crate::ftp::TransferSettings {
            preserve_time: Some(true),
            ..Default::default()
        };
        let preset = session.with_transfer_snapshot(settings);
        preset
            .scp_upload(&folder, &input, name, &cancel, |_| {})
            .unwrap();
        let output = local.path().join("timed output");
        preset
            .scp_download(&folder, name, &output, &cancel, |_| {})
            .unwrap();
        assert_eq!(fs::metadata(output).unwrap().modified().unwrap(), timestamp);
        let entry = session
            .list_sync(&folder, &cancel)
            .unwrap()
            .entries
            .pop()
            .unwrap();
        session.delete_sync_entry(&folder, &entry, &cancel).unwrap();

        let entry = session
            .list_sync(&root, &cancel)
            .unwrap()
            .entries
            .into_iter()
            .find(|entry| entry.name == "Arco's folder")
            .unwrap();
        session.delete_sync_entry(&root, &entry, &cancel).unwrap();
    }

    #[test]
    fn parses_only_the_requested_regular_file() {
        assert_eq!(
            parse_header(b"C0600 192 notes.txt", "notes.txt").unwrap(),
            192
        );
        for header in [
            b"D0755 0 notes.txt".as_slice(),
            b"C0600 -1 notes.txt",
            b"C0600 1 ../notes.txt",
            b"C0600 1 other.txt",
        ] {
            assert!(parse_header(header, "notes.txt").is_err());
        }
        assert_eq!(shell_arg("/home/a b's.txt"), "'/home/a b'\\''s.txt'");
    }
}
