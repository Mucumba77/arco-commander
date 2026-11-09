use crate::files;
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use tempfile::NamedTempFile;

mod binhex;
mod mime;

const XX: &[u8; 64] = b"+-0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
const LINE_BYTES: usize = 45;
const MAX_LINE: u64 = 4096;

#[derive(Clone, Copy)]
pub enum Format {
    Uue,
    Xxe,
    Mime,
    BinHex,
}

impl Format {
    pub fn from_path(path: &Path) -> Option<Self> {
        match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
            "uue" | "uu" => Some(Self::Uue),
            "xxe" => Some(Self::Xxe),
            "b64" | "mime" | "mim" | "eml" => Some(Self::Mime),
            "hqx" => Some(Self::BinHex),
            _ => None,
        }
    }

    fn extension(self) -> &'static str {
        match self {
            Self::Uue => ".uue",
            Self::Xxe => ".xxe",
            Self::Mime => ".b64",
            Self::BinHex => ".hqx",
        }
    }

    fn encode_char(self, value: u8) -> u8 {
        match self {
            Self::Uue if value == 0 => b'`',
            Self::Uue => b' ' + value,
            Self::Xxe => XX[value as usize],
            _ => unreachable!("separate MIME and BinHex implementations"),
        }
    }

    fn decode_char(self, byte: u8) -> io::Result<u8> {
        match (self, byte) {
            (Self::Uue, b'`') => Some(0),
            (Self::Uue, b' '..=b'_') => Some(byte - b' '),
            (Self::Xxe, b'+') => Some(0),
            (Self::Xxe, b'-') => Some(1),
            (Self::Xxe, b'0'..=b'9') => Some(2 + byte - b'0'),
            (Self::Xxe, b'A'..=b'Z') => Some(12 + byte - b'A'),
            (Self::Xxe, b'a'..=b'z') => Some(38 + byte - b'a'),
            _ => None,
        }
        .ok_or_else(|| invalid("Ungültiges Zeichen im kodierten Datenblock."))
    }
}

pub fn encode(
    source: &Path,
    directory: &Path,
    format: Format,
    cancel: &AtomicBool,
) -> io::Result<PathBuf> {
    match format {
        Format::Mime => return mime::encode(source, directory, cancel),
        Format::BinHex => {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "BinHex steht zum Dekodieren bereit.",
            ));
        }
        _ => {}
    }
    files::check_cancel(cancel)?;
    let metadata = source_file(source)?;
    let mut input = BufReader::new(File::open(source)?);
    let directory = fs::canonicalize(directory)?;
    let name = source
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or_else(|| invalid("Der Dateiname ist kein Unicode-Name."))?;
    files::leaf_name(OsStr::new(name))?;
    let target = directory.join(format!("{name}{}", format.extension()));
    let mut staged = NamedTempFile::new_in(&directory)?;
    {
        let mut output = BufWriter::new(staged.as_file_mut());
        output.write_all(b"begin 644 ")?;
        output.write_all(name.as_bytes())?;
        output.write_all(b"\r\n")?;
        let mut remaining = metadata.len();
        let mut plain = [0; LINE_BYTES];
        let mut encoded = [0; 1 + 4 * (LINE_BYTES / 3) + 2];
        while remaining > 0 {
            files::check_cancel(cancel)?;
            let len = remaining.min(LINE_BYTES as u64) as usize;
            input.read_exact(&mut plain[..len])?;
            encoded[0] = format.encode_char(len as u8);
            for (index, bytes) in plain[..len].chunks(3).enumerate() {
                let [a, b, c] = [
                    bytes[0],
                    *bytes.get(1).unwrap_or(&0),
                    *bytes.get(2).unwrap_or(&0),
                ];
                let code = [
                    a >> 2,
                    ((a & 3) << 4) | (b >> 4),
                    ((b & 15) << 2) | (c >> 6),
                    c & 63,
                ];
                let offset = 1 + index * 4;
                encoded[offset..offset + 4]
                    .copy_from_slice(&code.map(|value| format.encode_char(value)));
            }
            let end = 1 + len.div_ceil(3) * 4;
            encoded[end..end + 2].copy_from_slice(b"\r\n");
            output.write_all(&encoded[..end + 2])?;
            remaining -= len as u64;
        }
        output.write_all(&[format.encode_char(0)])?;
        output.write_all(b"\r\nend\r\n")?;
        output.flush()?;
    }
    files::ensure_unchanged(source, &metadata)?;
    publish(staged, &target, cancel)?;
    Ok(target)
}

pub fn decode(
    source: &Path,
    directory: &Path,
    format: Format,
    cancel: &AtomicBool,
) -> io::Result<Vec<PathBuf>> {
    match format {
        Format::Mime => mime::decode(source, directory, cancel),
        Format::BinHex => binhex::decode(source, directory, cancel),
        Format::Uue | Format::Xxe => {
            decode_armored(source, directory, format, cancel).map(|(path, _)| vec![path])
        }
    }
}

pub fn decode_mime_batch(
    sources: &[PathBuf],
    directory: &Path,
    cancel: &AtomicBool,
) -> Vec<(PathBuf, io::Result<Vec<PathBuf>>)> {
    mime::decode_batch(sources, directory, cancel)
}

pub fn decode_armored_batch(
    sources: &[(PathBuf, Format)],
    directory: &Path,
    cancel: &AtomicBool,
) -> Vec<(PathBuf, io::Result<()>)> {
    let mut sources = sources.iter().collect::<Vec<_>>();
    sources.sort_by(|left, right| left.0.cmp(&right.0));
    let mut consumed = std::collections::HashSet::new();
    let key = |path: &Path| {
        let key = path.to_string_lossy();
        if cfg!(windows) {
            key.to_lowercase()
        } else {
            key.into_owned()
        }
    };
    let mut results = Vec::new();
    for (source, format) in sources {
        if consumed.contains(&key(source)) {
            continue;
        }
        let result = decode_armored(source, directory, *format, cancel).map(|(_, parts)| {
            consumed.extend(parts.iter().skip(1).map(|path| key(path)));
        });
        let cancelled = result
            .as_ref()
            .is_err_and(|error| error.kind() == io::ErrorKind::Interrupted);
        results.push((source.clone(), result));
        if cancelled {
            break;
        }
    }
    results.retain(|(source, result)| result.is_ok() || !consumed.contains(&key(source)));
    results
}

fn decode_armored(
    source: &Path,
    directory: &Path,
    format: Format,
    cancel: &AtomicBool,
) -> io::Result<(PathBuf, Vec<PathBuf>)> {
    if !matches!(format, Format::Uue | Format::Xxe) {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    files::check_cancel(cancel)?;
    let mut input = ArmoredParts::open(source)?;
    let mut line = Vec::new();
    let name = read_header(&mut input.reader, &mut line, cancel)?;
    let directory = fs::canonicalize(directory)?;
    let target = directory.join(name);
    let mut staged = NamedTempFile::new_in(&directory)?;
    {
        let mut output = BufWriter::new(staged.as_file_mut());
        loop {
            files::check_cancel(cancel)?;
            if !input.read_line(&mut line)? {
                return Err(invalid("Der kodierte Datenblock ist unvollständig."));
            }
            if line.is_empty() || line == b" " || line == [format.encode_char(0)] {
                break;
            }
            let length = format.decode_char(line[0])? as usize;
            let end = 1 + length.div_ceil(3) * 4;
            if !(1..=LINE_BYTES).contains(&length)
                || line.len() < end
                || line[end..].iter().any(|byte| !matches!(byte, b' ' | b'\t'))
            {
                return Err(invalid("Die Länge einer kodierten Zeile ist ungültig."));
            }
            let mut bytes = [0; LINE_BYTES];
            for (index, group) in line[1..end].chunks_exact(4).enumerate() {
                let [a, b, c, d] = [
                    format.decode_char(group[0])?,
                    format.decode_char(group[1])?,
                    format.decode_char(group[2])?,
                    format.decode_char(group[3])?,
                ];
                let offset = index * 3;
                bytes[offset..offset + 3].copy_from_slice(&[
                    (a << 2) | (b >> 4),
                    (b << 4) | (c >> 2),
                    (c << 6) | d,
                ]);
            }
            output.write_all(&bytes[..length])?;
        }
        if !input.read_line(&mut line)? || line != b"end" {
            return Err(invalid(
                "Der kodierte Datenblock endet ohne Abschlusszeile.",
            ));
        }
        output.flush()?;
    }
    for (path, metadata) in &input.sources {
        files::ensure_unchanged(path, metadata)?;
    }
    publish(staged, &target, cancel)?;
    Ok((
        target,
        input.sources.into_iter().map(|(path, _)| path).collect(),
    ))
}

struct ArmoredParts {
    sources: Vec<(PathBuf, fs::Metadata)>,
    reader: BufReader<io::Take<File>>,
}

impl ArmoredParts {
    fn open(source: &Path) -> io::Result<Self> {
        let metadata = source_file(source)?;
        let reader = BufReader::new(File::open(source)?.take(metadata.len()));
        Ok(Self {
            sources: vec![(source.to_path_buf(), metadata)],
            reader,
        })
    }

    fn read_line(&mut self, line: &mut Vec<u8>) -> io::Result<bool> {
        if read_line(&mut self.reader, line)? {
            return Ok(true);
        }
        if self.sources.len() >= 2048 {
            return Err(invalid("Zu viele UUE-/XXE-Teildateien."));
        }
        let Some(next) = next_armored_part(&self.sources.last().expect("initial source").0) else {
            return Ok(false);
        };
        let metadata = match source_file(&next) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error),
        };
        self.reader = BufReader::new(File::open(&next)?.take(metadata.len()));
        self.sources.push((next, metadata));
        read_line(&mut self.reader, line)
    }
}

fn next_armored_part(path: &Path) -> Option<PathBuf> {
    let stem = path.file_stem()?.to_str()?;
    let digits = stem
        .as_bytes()
        .iter()
        .rev()
        .take_while(|digit| digit.is_ascii_digit())
        .count();
    let mut next = if digits != 0 {
        let (base, ordinal) = stem.split_at(stem.len() - digits);
        let successor = ordinal.parse::<u64>().ok()?.checked_add(1)?;
        format!("{base}{successor:0digits$}")
    } else {
        let suffix = stem.as_bytes().get(stem.len().checked_sub(2)?..)?;
        if !suffix.iter().all(|letter| letter.is_ascii_alphabetic())
            || suffix[0].is_ascii_lowercase() != suffix[1].is_ascii_lowercase()
        {
            return None;
        }
        let offset = if suffix[0].is_ascii_lowercase() {
            b'a'
        } else {
            b'A'
        };
        let successor = u16::from(suffix[0] - offset) * 26 + u16::from(suffix[1] - offset) + 1;
        if successor >= 26 * 26 {
            return None;
        }
        format!(
            "{}{}{}",
            &stem[..stem.len() - 2],
            char::from(offset + (successor / 26) as u8),
            char::from(offset + (successor % 26) as u8)
        )
    };
    if let Some(extension) = path.extension() {
        next.push('.');
        next.push_str(extension.to_str()?);
    }
    Some(path.with_file_name(next))
}

fn source_file(source: &Path) -> io::Result<fs::Metadata> {
    let metadata = files::regular_metadata(source)?;
    if metadata.is_file() {
        Ok(metadata)
    } else {
        Err(invalid("Wähle eine reguläre Datei."))
    }
}

fn read_header(
    input: &mut impl BufRead,
    line: &mut Vec<u8>,
    cancel: &AtomicBool,
) -> io::Result<String> {
    let mut scanned = 0;
    while scanned < 64 * 1024 {
        files::check_cancel(cancel)?;
        if !read_line(input, line)? {
            break;
        }
        scanned += line.len() + 1;
        let line = line.strip_prefix(b"\xef\xbb\xbf").unwrap_or(line);
        if let Some(header) = line.strip_prefix(b"begin ") {
            let index = header
                .iter()
                .position(|byte| *byte == b' ')
                .ok_or_else(|| invalid("Der Dateiname im Kodier-Header fehlt."))?;
            let (mode, name) = header.split_at(index);
            if !(3..=4).contains(&mode.len())
                || !mode.iter().all(|byte| matches!(byte, b'0'..=b'7'))
            {
                return Err(invalid("Der Dateimodus im Kodier-Header ist ungültig."));
            }
            let name = &name[1..];
            let name = match std::str::from_utf8(name) {
                Ok(name) => name.to_owned(),
                Err(_) => encoding_rs::WINDOWS_1252
                    .decode_without_bom_handling(name)
                    .0
                    .into_owned(),
            };
            files::leaf_name(OsStr::new(&name))?;
            return Ok(name);
        }
    }
    Err(invalid("Kein UUE-/XXE-Header gefunden."))
}

fn read_line(input: &mut impl BufRead, line: &mut Vec<u8>) -> io::Result<bool> {
    line.clear();
    let count = input.take(MAX_LINE + 1).read_until(b'\n', line)?;
    if count as u64 > MAX_LINE {
        return Err(invalid("Eine kodierte Zeile ist zu lang."));
    }
    if line.last() == Some(&b'\n') {
        line.pop();
        if line.last() == Some(&b'\r') {
            line.pop();
        }
    }
    Ok(count != 0)
}

fn publish(staged: NamedTempFile, target: &Path, cancel: &AtomicBool) -> io::Result<()> {
    staged.as_file().sync_all()?;
    files::check_cancel(cancel)?;
    staged
        .persist_noclobber(target)
        .map_err(|error| error.error)?;
    Ok(())
}

fn publish_group(
    staged: Vec<(PathBuf, NamedTempFile)>,
    cancel: &AtomicBool,
) -> io::Result<Vec<PathBuf>> {
    for (target, file) in &staged {
        files::check_cancel(cancel)?;
        if target.try_exists()? {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("Zieldatei existiert: {}", target.display()),
            ));
        }
        file.as_file().sync_all()?;
    }
    files::check_cancel(cancel)?;
    let mut published = Vec::with_capacity(staged.len());
    for (target, file) in staged {
        if let Err(error) = file.persist_noclobber(&target) {
            return if published.is_empty() {
                Err(error.error)
            } else {
                Err(io::Error::other(format!(
                    "{} · Bereits veröffentlichte Dateien: {published:?}",
                    error.error
                )))
            };
        }
        published.push(target);
    }
    Ok(published)
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbered_and_lettered_armored_parts_are_decoded_once_and_require_every_part() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path();
        let cancel = AtomicBool::new(false);
        let first = root.join("mail001.uue");
        let second = root.join("mail002.uue");
        let third = root.join("mail003.uue");
        fs::write(&first, b"begin 644 cat.txt\n#0V%T\n").unwrap();
        fs::write(&third, b"`\nend\n").unwrap();
        assert!(decode(&first, root, Format::Uue, &cancel).is_err());
        assert!(!root.join("cat.txt").exists());
        fs::write(&second, b"#0V%T\n").unwrap();
        let result = decode_armored_batch(
            &[
                (third, Format::Uue),
                (first.clone(), Format::Uue),
                (second, Format::Uue),
            ],
            root,
            &cancel,
        );
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].0, first);
        assert!(result[0].1.is_ok());
        assert_eq!(fs::read(root.join("cat.txt")).unwrap(), b"CatCat");

        let first = root.join("postaaz");
        let second = root.join("postaba");
        fs::write(&first, b"begin 644 xxe.txt\n1Eq3o\n").unwrap();
        fs::write(&second, b"+\nend\n").unwrap();
        assert_eq!(
            decode(&first, root, Format::Xxe, &cancel).unwrap(),
            [root.canonicalize().unwrap().join("xxe.txt")]
        );
        assert_eq!(fs::read(root.join("xxe.txt")).unwrap(), b"Cat");
    }

    #[test]
    fn transport_encoding_matches_known_vectors_and_round_trips_binary() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path();
        let original = root.join("Katze.txt");
        let out = root.join("out");
        fs::create_dir(&out).unwrap();
        fs::write(&original, b"Cat").unwrap();
        let cancel = AtomicBool::new(false);
        for (format, body) in [
            (Format::Uue, "#0V%T\r\n`\r\n"),
            (Format::Xxe, "1Eq3o\r\n+\r\n"),
        ] {
            let encoded = encode(&original, root, format, &cancel).unwrap();
            let expected = format!("begin 644 Katze.txt\r\n{body}end\r\n");
            assert_eq!(fs::read(encoded).unwrap(), expected.as_bytes());
            let binary = root.join("Größe 🐈.bin");
            let bytes: Vec<u8> = (0..=255).collect();
            fs::write(&binary, &bytes).unwrap();
            let encoded = encode(&binary, root, format, &cancel).unwrap();
            let decoded = decode(&encoded, &out, format, &cancel).unwrap().remove(0);
            assert_eq!(decoded.file_name(), binary.file_name());
            assert_eq!(fs::read(&decoded).unwrap(), bytes);
            assert_eq!(
                decode(&encoded, &out, format, &cancel).unwrap_err().kind(),
                io::ErrorKind::AlreadyExists
            );
            fs::remove_file(decoded).unwrap();
        }
    }

    #[test]
    fn transport_decoding_rejects_traversal_and_incomplete_payloads() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path();
        let source = root.join("mail.uue");
        let cancel = AtomicBool::new(false);
        for encoded in [
            b"begin 644 ../escaped.txt\n#0V%T\n`\nend\n".as_slice(),
            b"begin 644 missing.txt\n#0V%T\n`\n".as_slice(),
            b"begin 644 missing.txt\n#0V\n`\nend\n".as_slice(),
        ] {
            fs::write(&source, encoded).unwrap();
            assert!(decode(&source, root, Format::Uue, &cancel).is_err());
        }
        assert_eq!(fs::read_dir(root).unwrap().count(), 1);
        fs::write(
            &source,
            b"Subject: test\r\nbegin 644 Gr\xf6\xdfe.txt\r\n#0V%T\r\n \r\nend\r\n",
        )
        .unwrap();
        let decoded = decode(&source, root, Format::Uue, &cancel)
            .unwrap()
            .remove(0);
        assert_eq!(decoded.file_name().unwrap(), "Größe.txt");
        assert_eq!(fs::read(decoded).unwrap(), b"Cat");
    }
}
