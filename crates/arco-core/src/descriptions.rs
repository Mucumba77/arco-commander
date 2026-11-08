use crate::files;
use encoding_rs::{UTF_8, UTF_16BE, UTF_16LE, WINDOWS_1252};
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs::{self, File, Metadata};
use std::io::{self, Read, Write};
use std::path::Path;
use tempfile::NamedTempFile;

const FILE_NAME: &str = "descript.ion";
const MAX_FILE: u64 = 16 * 1024 * 1024;
const EXTENSION: char = '\u{4}';
const MULTILINE: char = '\u{00c2}';

pub type ApplyAttributes = fn(&File, Option<&Metadata>) -> io::Result<()>;

#[derive(Clone, Copy)]
enum Encoding {
    Utf8,
    Utf16Le,
    Utf16Be,
    Legacy,
}

pub fn read(directory: &Path) -> io::Result<BTreeMap<String, String>> {
    Ok(records(&load(&directory.join(FILE_NAME))?.0))
}

pub fn key(name: &OsStr) -> String {
    name.to_string_lossy().to_lowercase()
}

pub fn is_sidecar(entry: &Path) -> bool {
    entry
        .file_name()
        .is_some_and(|name| name.eq_ignore_ascii_case(FILE_NAME))
}

pub(crate) fn copy_entry(
    source: &Path,
    target: &Path,
    apply_attributes: ApplyAttributes,
) -> io::Result<Option<String>> {
    if is_sidecar(source) || is_sidecar(target) {
        return Ok(None);
    }
    let (source_dir, source_name) = entry_name(source)?;
    let Some(value) = read(source_dir)?.remove(&key(source_name.as_ref())) else {
        return Ok(None);
    };
    if value.is_empty() {
        return Ok(None);
    }
    let (target_dir, target_name) = entry_name(target)?;
    let expected = read(target_dir)?.remove(&key(target_name.as_ref()));
    if expected.as_deref() != Some(&value) {
        update(
            target_dir,
            target_name,
            expected.as_deref(),
            &value,
            false,
            apply_attributes,
        )?;
    }
    Ok(Some(value))
}

pub(crate) fn remove_moved(
    entry: &Path,
    comment: &str,
    apply_attributes: ApplyAttributes,
) -> io::Result<()> {
    let (directory, name) = entry_name(entry)?;
    update(directory, name, Some(comment), "", true, apply_attributes)
}

fn entry_name(entry: &Path) -> io::Result<(&Path, &str)> {
    let directory = entry
        .parent()
        .ok_or_else(|| io::Error::other("Der Elternordner fehlt."))?;
    let name = files::leaf_name(entry.file_name().unwrap_or_default())?
        .to_str()
        .ok_or_else(|| io::Error::other("Der Dateiname ist kein Unicode-Name."))?;
    Ok((directory, name))
}

pub fn set(
    entry: &Path,
    expected: Option<&str>,
    value: &str,
    apply_attributes: impl FnOnce(&File, Option<&Metadata>) -> io::Result<()>,
) -> io::Result<()> {
    files::regular_metadata(entry)?;
    let (directory, name) = entry_name(entry)?;
    update(directory, name, expected, value, false, apply_attributes)
}

fn update(
    directory: &Path,
    name: &str,
    expected: Option<&str>,
    value: &str,
    remove_empty: bool,
    apply_attributes: impl FnOnce(&File, Option<&Metadata>) -> io::Result<()>,
) -> io::Result<()> {
    if name.eq_ignore_ascii_case(FILE_NAME) {
        return Err(io::Error::other(
            "Die Kommentardatei kann sich nicht selbst beschreiben.",
        ));
    }
    if value
        .chars()
        .any(|character| character.is_control() && !matches!(character, '\r' | '\n' | '\t'))
    {
        return Err(io::Error::other("Der Kommentar enthält Steuerzeichen."));
    }
    let target = directory.join(FILE_NAME);
    let (current, encoding, snapshot) = load(&target)?;
    let entry_key = key(OsStr::new(name));
    if records(&current).get(&entry_key).map(String::as_str) != expected {
        return Err(io::Error::other("Der Kommentar wurde inzwischen geändert."));
    }
    let value = value.replace("\r\n", "\n").replace('\r', "\n");
    let mut unknown = Vec::new();
    let mut lines = Vec::new();
    for line in current.split_terminator(['\r', '\n']) {
        if line.is_empty() {
            continue;
        }
        if let Some((existing, _, suffix)) = record(line)
            && key(OsStr::new(existing)) == entry_key
        {
            unknown.extend(
                suffix
                    .split(EXTENSION)
                    .filter(|part| !part.is_empty() && !part.starts_with(MULTILINE)),
            );
        } else {
            lines.push(line.to_owned());
        }
    }
    if !value.is_empty() || !unknown.is_empty() {
        let mut line = if name.contains(char::is_whitespace) {
            format!("\"{name}\" ")
        } else {
            format!("{name} ")
        };
        if value.contains('\n') {
            line.push_str(&value.replace('\\', "\\\\").replace('\n', "\\n"));
            line.extend([EXTENSION, MULTILINE]);
        } else {
            line.push_str(&value);
        }
        for part in unknown {
            line.push(EXTENSION);
            line.push_str(part);
        }
        if line.len() > 4094
            || (matches!(encoding, Encoding::Utf16Le | Encoding::Utf16Be)
                && line.encode_utf16().count() > 2047)
        {
            return Err(io::Error::other(
                "Der Kommentar überschreitet die Länge einer descript.ion-Zeile.",
            ));
        }
        lines.push(line);
    }
    if lines.is_empty() {
        match &snapshot {
            None => return Ok(()),
            Some(snapshot) if remove_empty => {
                files::ensure_unchanged(&target, snapshot)?;
                return fs::remove_file(&target);
            }
            _ => (),
        }
    }
    let mut text = lines.join("\r\n");
    if !text.is_empty() {
        text.push_str("\r\n");
    }
    let mut staged = NamedTempFile::new_in(directory)?;
    staged.write_all(&encode(&text, encoding))?;
    staged.as_file().sync_all()?;
    let published = if let Some(snapshot) = &snapshot {
        files::ensure_unchanged(&target, snapshot)?;
        staged.persist(&target)
    } else {
        staged.persist_noclobber(&target)
    }
    .map_err(|error| error.error)?;
    // Persist clears Windows temporary-file attributes, so set the final attributes on the open file.
    apply_attributes(&published, snapshot.as_ref())
}

fn load(path: &Path) -> io::Result<(String, Encoding, Option<Metadata>)> {
    let metadata = match files::regular_metadata(path) {
        Ok(metadata) if metadata.is_file() => metadata,
        Ok(_) => return Err(io::Error::other("descript.ion ist keine reguläre Datei.")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok((String::new(), Encoding::Utf8, None));
        }
        Err(error) => return Err(error),
    };
    let mut bytes = Vec::new();
    File::open(path)?
        .take(MAX_FILE + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_FILE {
        return Err(io::Error::other("descript.ion überschreitet 16 MiB."));
    }
    files::ensure_unchanged(path, &metadata)?;
    let (format, codec, data) = if let Some(data) = bytes.strip_prefix(&[0xff, 0xfe]) {
        (Encoding::Utf16Le, UTF_16LE, data)
    } else if let Some(data) = bytes.strip_prefix(&[0xfe, 0xff]) {
        (Encoding::Utf16Be, UTF_16BE, data)
    } else if let Some(data) = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]) {
        (Encoding::Utf8, UTF_8, data)
    } else if std::str::from_utf8(&bytes).is_ok() {
        (Encoding::Utf8, UTF_8, bytes.as_slice())
    } else {
        (Encoding::Legacy, WINDOWS_1252, bytes.as_slice())
    };
    let (text, invalid) = codec.decode_without_bom_handling(data);
    if invalid {
        return Err(io::Error::other("Ungültige Zeichen in descript.ion."));
    }
    Ok((text.into_owned(), format, Some(metadata)))
}

fn encode(text: &str, encoding: Encoding) -> Vec<u8> {
    match encoding {
        Encoding::Utf16Le => [0xfeff]
            .into_iter()
            .chain(text.encode_utf16())
            .flat_map(u16::to_le_bytes)
            .collect(),
        Encoding::Utf16Be => [0xfeff]
            .into_iter()
            .chain(text.encode_utf16())
            .flat_map(u16::to_be_bytes)
            .collect(),
        Encoding::Legacy => {
            let (data, _, lost) = WINDOWS_1252.encode(text);
            if lost {
                encode(text, Encoding::Utf8)
            } else {
                data.into_owned()
            }
        }
        Encoding::Utf8 => [0xef, 0xbb, 0xbf].into_iter().chain(text.bytes()).collect(),
    }
}

fn record(line: &str) -> Option<(&str, &str, &str)> {
    let (name, remaining) = if let Some(quoted) = line.strip_prefix('"') {
        let (name, remaining) = quoted.split_once('"')?;
        (name, remaining.strip_prefix(' ')?)
    } else {
        line.split_once([' ', '\t'])?
    };
    let (text, suffix) = remaining.split_once(EXTENSION).unwrap_or((remaining, ""));
    Some((name, text, suffix))
}

fn records(data: &str) -> BTreeMap<String, String> {
    data.split(['\r', '\n'])
        .filter_map(record)
        .map(|(name, text, suffix)| {
            let mut comment = String::new();
            if suffix
                .split(EXTENSION)
                .any(|part| part.starts_with(MULTILINE))
            {
                let mut characters = text.chars().peekable();
                while let Some(character) = characters.next() {
                    match (character, characters.peek()) {
                        ('\\', Some('n')) => {
                            characters.next();
                            comment.push('\n');
                        }
                        ('\\', Some('\\')) => {
                            characters.next();
                            comment.push('\\');
                        }
                        _ => comment.push(character),
                    }
                }
            } else {
                comment.push_str(text);
            }
            (key(OsStr::new(name)), comment)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn edits_a_tc_comment_without_losing_encoding_or_extensions() {
        let fixture = tempfile::tempdir().unwrap();
        let directory = fixture.path();
        let entry = directory.join("Datei ä.txt");
        let descriptions = directory.join(FILE_NAME);
        fs::write(&entry, b"test").unwrap();
        fs::write(
            &descriptions,
            encode(
                "\"Datei ä.txt\" Zeile\\nzwei\u{4}\u{c2}\u{4}Xforeign\r\nandere.txt bleibt\r\n",
                Encoding::Utf16Le,
            ),
        )
        .unwrap();
        let name = key(entry.file_name().unwrap());
        assert_eq!(read(directory).unwrap()[&name], "Zeile\nzwei");
        assert!(set(&entry, None, "falsch", |_, _| Ok(())).is_err());
        set(&entry, Some("Zeile\nzwei"), "neu\nC:\\Daten", |_, _| Ok(())).unwrap();
        let (text, encoding, _) = load(&descriptions).unwrap();
        assert!(matches!(encoding, Encoding::Utf16Le));
        assert!(text.contains("\u{4}Xforeign"));
        let records = read(directory).unwrap();
        assert_eq!(records[&name], "neu\nC:\\Daten");
        assert_eq!(records["andere.txt"], "bleibt");
    }
}
