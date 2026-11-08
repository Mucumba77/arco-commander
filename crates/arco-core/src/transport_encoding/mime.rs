use super::{invalid, publish, publish_group, read_line, source_file};
use crate::{files, tools};
use base64::{Engine, engine::general_purpose::STANDARD, read::DecoderReader};
use mailparse::{MailHeaderMap, body::Body};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use tempfile::NamedTempFile;

const MAX_INPUT: u64 = 64 * 1024 * 1024;
const MAX_FRAGMENTS: usize = 4096;

struct Fragment {
    source: PathBuf,
    metadata: fs::Metadata,
    offset: u64,
    number: usize,
    total: Option<usize>,
}

pub(super) fn decode_batch(
    sources: &[PathBuf],
    directory: &Path,
    cancel: &AtomicBool,
) -> Vec<(PathBuf, io::Result<Vec<PathBuf>>)> {
    let mut groups = BTreeMap::<String, Vec<Fragment>>::new();
    let mut output = Vec::new();
    for source in sources {
        match inspect_fragment(source, cancel) {
            Ok(Some((id, fragment))) => groups.entry(id).or_default().push(fragment),
            Ok(None) => output.push((source.clone(), decode(source, directory, cancel))),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                output.push((source.clone(), Err(error)));
                return output;
            }
            Err(error) => output.push((source.clone(), Err(error))),
        }
    }
    for (_, fragments) in groups {
        let label = fragments[0].source.clone();
        let decoded = reassemble(fragments, directory, cancel);
        let interrupted = decoded
            .as_ref()
            .is_err_and(|error| error.kind() == io::ErrorKind::Interrupted);
        output.push((label, decoded));
        if interrupted {
            break;
        }
    }
    output
}

fn inspect_fragment(source: &Path, cancel: &AtomicBool) -> io::Result<Option<(String, Fragment)>> {
    let metadata = source_file(source)?;
    let mut reader = BufReader::new(File::open(source)?);
    let mut data = Vec::new();
    let mut line = Vec::new();
    loop {
        files::check_cancel(cancel)?;
        match read_line(&mut reader, &mut line) {
            Ok(true) if data.len() + line.len() <= 64 * 1024 => (),
            // The regular MIME decoder also accepts some non-standard header layouts.
            _ => return Ok(None),
        }
        data.extend_from_slice(&line);
        data.extend_from_slice(b"\r\n");
        if line.is_empty() {
            break;
        }
    }
    let (headers, _) = mailparse::parse_headers(&data).map_err(io::Error::other)?;
    let content =
        mailparse::parse_content_type(&headers.get_first_value("Content-Type").unwrap_or_default());
    if !content.mimetype.eq_ignore_ascii_case("message/partial") {
        return Ok(None);
    }
    if headers
        .get_first_value("Content-Transfer-Encoding")
        .is_some_and(|value| !value.trim().eq_ignore_ascii_case("7bit"))
    {
        return Err(invalid("MIME-Nachrichtenteile benötigen 7bit-Kodierung."));
    }
    let id = content
        .params
        .get("id")
        .filter(|id| !id.is_empty())
        .ok_or_else(|| invalid("MIME-Nachrichtenteil ohne ID."))?;
    let part_number = |value: &str| {
        value
            .parse::<usize>()
            .ok()
            .filter(|number| (1..=MAX_FRAGMENTS).contains(number))
    };
    let number = content
        .params
        .get("number")
        .and_then(|value| part_number(value))
        .ok_or_else(|| invalid("Ungültige MIME-Teilnummer."))?;
    let total = content
        .params
        .get("total")
        .map(|value| part_number(value).ok_or_else(|| invalid("Ungültige MIME-Gesamtzahl.")))
        .transpose()?;
    let offset = reader.stream_position()?;
    files::ensure_unchanged(source, &metadata)?;
    Ok(Some((
        id.clone(),
        Fragment {
            source: source.to_path_buf(),
            metadata,
            offset,
            number,
            total,
        },
    )))
}

fn reassemble(
    mut parts: Vec<Fragment>,
    directory: &Path,
    cancel: &AtomicBool,
) -> io::Result<Vec<PathBuf>> {
    files::check_cancel(cancel)?;
    parts.sort_by_key(|part| part.number);
    let count = parts.len();
    if parts.last().and_then(|part| part.total) != Some(count)
        || parts.iter().enumerate().any(|(index, part)| {
            part.number != index + 1 || part.total.is_some_and(|total| total != count)
        })
    {
        return Err(invalid(
            "MIME-Nachrichtenteile fehlen, sind doppelt oder haben widersprüchliche Teilzahlen.",
        ));
    }
    let mut data = Vec::new();
    for part in &parts {
        files::check_cancel(cancel)?;
        files::ensure_unchanged(&part.source, &part.metadata)?;
        let size = part
            .metadata
            .len()
            .checked_sub(part.offset)
            .ok_or_else(|| invalid("Ungültiger MIME-Nachrichtenteil."))?;
        if size > MAX_INPUT - data.len() as u64 {
            return Err(invalid(
                "Zusammengefügte MIME-Nachrichten dürfen höchstens 64 MiB groß sein.",
            ));
        }
        let mut file = File::open(&part.source)?;
        file.seek(SeekFrom::Start(part.offset))?;
        if tools::copy_cancelled(file.take(size), &mut data, cancel)? != size {
            return Err(invalid("Die MIME-Teilgröße hat sich geändert."));
        }
    }
    decode_message(&data, &parts[0].source, directory, cancel, || {
        for part in &parts {
            files::ensure_unchanged(&part.source, &part.metadata)?;
        }
        Ok(())
    })
}

pub(super) fn encode(source: &Path, directory: &Path, cancel: &AtomicBool) -> io::Result<PathBuf> {
    files::check_cancel(cancel)?;
    let metadata = source_file(source)?;
    let mut input = BufReader::new(File::open(source)?);
    let directory = fs::canonicalize(directory)?;
    let name = source
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or_else(|| invalid("Der Dateiname ist kein Unicode-Name."))?;
    files::leaf_name(OsStr::new(name))?;
    let target = directory.join(format!("{name}.b64"));
    let mut staged = NamedTempFile::new_in(&directory)?;
    {
        let mut output = BufWriter::new(staged.as_file_mut());
        output.write_all(b"MIME-Version: 1.0\r\nContent-Type: application/octet-stream\r\nContent-Transfer-Encoding: base64\r\nContent-Disposition: attachment")?;
        if name.is_ascii() {
            write!(output, ";\r\n filename=\"{name}\"")?;
        } else {
            for (index, chunk) in name.as_bytes().chunks(18).enumerate() {
                write!(output, ";\r\n filename*{index}*=")?;
                if index == 0 {
                    output.write_all(b"utf-8''")?;
                }
                for byte in chunk {
                    write!(output, "%{byte:02X}")?;
                }
            }
        }
        output.write_all(b"\r\n\r\n")?;
        let mut plain = [0; 57];
        let mut encoded = [0; 76];
        let mut remaining = metadata.len();
        while remaining > 0 {
            files::check_cancel(cancel)?;
            let size = remaining.min(57) as usize;
            input.read_exact(&mut plain[..size])?;
            let written = STANDARD
                .encode_slice(&plain[..size], &mut encoded)
                .map_err(io::Error::other)?;
            output.write_all(&encoded[..written])?;
            output.write_all(b"\r\n")?;
            remaining -= size as u64;
        }
        output.flush()?;
    }
    files::ensure_unchanged(source, &metadata)?;
    publish(staged, &target, cancel)?;
    Ok(target)
}

pub(super) fn decode(
    source: &Path,
    directory: &Path,
    cancel: &AtomicBool,
) -> io::Result<Vec<PathBuf>> {
    let metadata = source_file(source)?;
    if metadata.len() > MAX_INPUT {
        return decode_large_single(source, directory, &metadata, cancel).map(|path| vec![path]);
    }
    let mut input = Vec::new();
    tools::copy_cancelled(File::open(source)?.take(MAX_INPUT + 1), &mut input, cancel)?;
    if input.len() as u64 != metadata.len() {
        return Err(invalid("Die MIME-Dateigröße hat sich geändert."));
    }
    decode_message(&input, source, directory, cancel, || {
        files::ensure_unchanged(source, &metadata)
    })
}

fn decode_message(
    input: &[u8],
    source: &Path,
    directory: &Path,
    cancel: &AtomicBool,
    verify: impl FnOnce() -> io::Result<()>,
) -> io::Result<Vec<PathBuf>> {
    let message = mailparse::parse_mail(input).map_err(io::Error::other)?;
    if message
        .ctype
        .mimetype
        .eq_ignore_ascii_case("message/partial")
    {
        return Err(invalid(
            "Markiere alle Teile der MIME-Nachricht zum gemeinsamen Dekodieren.",
        ));
    }
    let directory = fs::canonicalize(directory)?;
    let mut targets = BTreeSet::new();
    let mut staged = Vec::new();
    for part in message.parts() {
        files::check_cancel(cancel)?;
        if part.ctype.mimetype.starts_with("multipart/") {
            let boundary = part
                .ctype
                .params
                .get("boundary")
                .ok_or_else(|| invalid("MIME-Multipart ohne Trennmarke."))?;
            let end = format!("--{boundary}--");
            if !part
                .raw_bytes
                .rsplit(|&byte| byte == b'\n')
                .any(|line| line.trim_ascii_end() == end.as_bytes())
            {
                return Err(invalid("Die abschließende MIME-Trennmarke fehlt."));
            }
            continue;
        }
        let disposition = part.get_content_disposition();
        let name = disposition
            .params
            .get("filename")
            .or_else(|| part.ctype.params.get("name"))
            .map(String::as_str);
        let name = name.or_else(|| {
            if std::ptr::eq(part, &message) && matches!(part.get_body_encoded(), Body::Base64(_)) {
                source.file_stem().and_then(OsStr::to_str)
            } else {
                None
            }
        });
        let Some(name) = name else { continue };
        if staged.len() == 512 {
            return Err(invalid("Zu viele MIME-Anhänge."));
        }
        files::leaf_name(OsStr::new(name))?;
        if !targets.insert(if cfg!(windows) {
            name.to_lowercase()
        } else {
            name.to_owned()
        }) {
            return Err(invalid("Mehrere MIME-Anhänge haben denselben Namen."));
        }
        let encoding = part
            .headers
            .get_first_value("Content-Transfer-Encoding")
            .unwrap_or_default();
        let mut file = NamedTempFile::new_in(&directory)?;
        match part.get_body_encoded() {
            Body::Base64(body) => {
                decode_base64(body.get_raw(), &mut file, cancel)?;
            }
            _ if matches!(
                encoding.trim().to_ascii_lowercase().as_str(),
                "" | "7bit" | "8bit" | "binary" | "quoted-printable"
            ) =>
            {
                let bytes = part.get_body_raw().map_err(io::Error::other)?;
                tools::copy_cancelled(bytes.as_slice(), &mut file, cancel)?;
            }
            _ => {
                return Err(invalid(
                    "Die Übertragungskodierung des MIME-Anhangs wird nicht unterstützt.",
                ));
            }
        }
        staged.push((directory.join(name), file));
    }
    if staged.is_empty() {
        return Err(invalid("Keine dekodierbaren MIME-Anhänge gefunden."));
    }
    verify()?;
    publish_group(staged, cancel)
}

fn decode_large_single(
    source: &Path,
    directory: &Path,
    metadata: &fs::Metadata,
    cancel: &AtomicBool,
) -> io::Result<PathBuf> {
    let mut reader = BufReader::new(File::open(source)?.take(metadata.len()));
    let mut headers = Vec::new();
    let mut line = Vec::new();
    loop {
        files::check_cancel(cancel)?;
        if !read_line(&mut reader, &mut line)? || headers.len() + line.len() > 64 * 1024 {
            return Err(invalid("Der MIME-Header ist unvollständig oder zu lang."));
        }
        headers.extend_from_slice(&line);
        headers.push(b'\n');
        if line.is_empty() {
            break;
        }
    }
    let (headers, _) = mailparse::parse_headers(&headers).map_err(io::Error::other)?;
    let content =
        mailparse::parse_content_type(&headers.get_first_value("Content-Type").unwrap_or_default());
    let disposition = mailparse::parse_content_disposition(
        &headers
            .get_first_value("Content-Disposition")
            .unwrap_or_default(),
    );
    if content.mimetype.starts_with("multipart/")
        || !headers
            .get_first_value("Content-Transfer-Encoding")
            .is_some_and(|value| value.trim().eq_ignore_ascii_case("base64"))
    {
        return Err(invalid(
            "Über 64 MiB wird ein einzelner MIME-Anhang mit Base64 unterstützt.",
        ));
    }
    let name = disposition
        .params
        .get("filename")
        .or_else(|| content.params.get("name"))
        .map(String::as_str)
        .or_else(|| source.file_stem().and_then(OsStr::to_str))
        .ok_or_else(|| invalid("Kein MIME-Dateiname gefunden."))?;
    files::leaf_name(OsStr::new(name))?;
    let directory = fs::canonicalize(directory)?;
    let target = directory.join(name);
    let mut staged = NamedTempFile::new_in(directory)?;
    decode_base64(reader, &mut staged, cancel)?;
    files::ensure_unchanged(source, metadata)?;
    publish(staged, &target, cancel)?;
    Ok(target)
}

fn decode_base64(input: impl Read, output: &mut impl Write, cancel: &AtomicBool) -> io::Result<()> {
    let input = SkipWhitespace { input, cancel };
    tools::copy_cancelled(DecoderReader::new(input, &STANDARD), output, cancel)?;
    Ok(())
}

struct SkipWhitespace<'a, R> {
    input: R,
    cancel: &'a AtomicBool,
}

impl<R: Read> Read for SkipWhitespace<'_, R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        loop {
            files::check_cancel(self.cancel)?;
            let read = self.input.read(buffer)?;
            let mut kept = 0;
            for index in 0..read {
                if !buffer[index].is_ascii_whitespace() {
                    buffer[kept] = buffer[index];
                    kept += 1;
                }
            }
            if kept > 0 || read == 0 {
                return Ok(kept);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mime_partial_reassembles_by_id_and_number_before_publishing() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path();
        let one = root.join("one.eml");
        let two = root.join("two.eml");
        let output = root.join("out");
        fs::create_dir(&output).unwrap();
        fs::write(
            &one,
            concat!(
                "Content-Type: message/partial; id=\"example@arco\"; number=1\r\n\r\n",
                "MIME-Version: 1.0\r\nContent-Type: application/octet-stream\r\n",
                "Content-Disposition: attachment; filename=sample.bin\r\n",
                "Content-Transfer-Encoding: base64\r\n\r\nY2",
            ),
        )
        .unwrap();
        fs::write(&two, "Content-Type: message/partial; number=2; total=2;\r\n id=\"example@arco\"\r\n\r\nF0\r\n").unwrap();
        let cancel = AtomicBool::new(false);
        for paths in [
            vec![two.clone()],
            vec![one.clone(), one.clone(), two.clone()],
        ] {
            let result = decode_batch(&paths, &output, &cancel);
            assert_eq!(result.len(), 1);
            assert!(result[0].1.is_err());
            assert!(!output.join("sample.bin").exists());
        }
        let result = decode_batch(&[two, one], &output, &cancel);
        assert_eq!(result.len(), 1);
        assert_eq!(
            result[0].1.as_ref().unwrap(),
            &[fs::canonicalize(output.join("sample.bin")).unwrap()]
        );
        assert_eq!(fs::read(output.join("sample.bin")).unwrap(), b"cat");
    }

    #[test]
    fn mime_round_trips_unicode_filename_and_base64_using_both_readers() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let source = root.join("Lange Grüße mit Unicode 🌲 und Semikolon;.bin");
        let output = root.join("output");
        fs::create_dir(&output).unwrap();
        let data: Vec<u8> = (0..=255).collect();
        fs::write(&source, &data).unwrap();
        let cancel = AtomicBool::new(false);
        let encoded = encode(&source, root, &cancel).unwrap();
        let files = decode(&encoded, &output, &cancel).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].file_name(), source.file_name());
        assert_eq!(fs::read(&files[0]).unwrap(), data);
        fs::remove_file(&files[0]).unwrap();
        let metadata = fs::metadata(&encoded).unwrap();
        let streamed = decode_large_single(&encoded, &output, &metadata, &cancel).unwrap();
        assert_eq!(streamed.file_name(), source.file_name());
        assert_eq!(fs::read(streamed).unwrap(), data);
    }

    #[test]
    fn mime_extracts_multipart_and_rejects_unterminated_or_unsafe_attachments() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let input = root.join("message.eml");
        let message = concat!(
            "Content-Type: multipart/mixed; boundary=example\r\n\r\n",
            "--example\r\nContent-Type: text/plain\r\n\r\nMail text\r\n",
            "--example\r\nContent-Disposition: attachment; filename=a.txt\r\n",
            "Content-Transfer-Encoding: base64\r\n\r\nY2F0\r\n",
            "--example\r\nContent-Disposition: attachment; filename=b.txt\r\n",
            "Content-Transfer-Encoding: quoted-printable\r\n\r\nd=F6g\r\n--example--\r\n",
        );
        let cancel = AtomicBool::new(false);
        for variant in [
            message.replace("filename=b.txt", "filename=../escape.txt"),
            message.replace("--example--\r\n", ""),
        ] {
            fs::write(&input, variant).unwrap();
            assert!(decode(&input, root, &cancel).is_err());
            assert!(!root.join("a.txt").exists());
        }
        fs::write(&input, message).unwrap();
        assert_eq!(decode(&input, root, &cancel).unwrap().len(), 2);
        assert_eq!(fs::read(root.join("a.txt")).unwrap(), b"cat");
        assert_eq!(fs::read(root.join("b.txt")).unwrap(), b"d\xf6g");
    }
}
