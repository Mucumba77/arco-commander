use super::{App, check_workspace, open_edit};
use crate::commands::{Dialog, Pending};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use tempfile::NamedTempFile;

const MAX_TEXT: u64 = 4 * 1024 * 1024;

#[derive(Clone, Copy)]
enum Encoding {
    Utf8,
    Utf8Bom,
    Utf16Le,
    Utf16Be,
}

pub(crate) struct TextEdit {
    pub(crate) path: PathBuf,
    original: Vec<u8>,
    encoding: Encoding,
    crlf: bool,
}

impl TextEdit {
    fn load(path: &Path) -> io::Result<(Self, String)> {
        let original = read_text_bytes(path)?;
        let (encoding, text) = if let Some(rest) = original.strip_prefix(b"\xef\xbb\xbf") {
            (Encoding::Utf8Bom, utf8(rest)?)
        } else if let Some(rest) = original.strip_prefix(b"\xff\xfe") {
            (Encoding::Utf16Le, utf16(rest, u16::from_le_bytes)?)
        } else if let Some(rest) = original.strip_prefix(b"\xfe\xff") {
            (Encoding::Utf16Be, utf16(rest, u16::from_be_bytes)?)
        } else {
            (Encoding::Utf8, utf8(&original)?)
        };
        if text
            .chars()
            .any(|ch| ch.is_control() && !matches!(ch, '\n' | '\r' | '\t'))
        {
            return Err(invalid_text());
        }
        let pairs = text.matches("\r\n").count();
        let crlf = pairs != 0
            && text.matches('\n').count() == pairs
            && text.matches('\r').count() == pairs;
        let text = if crlf {
            text.replace("\r\n", "\n")
        } else {
            text
        };
        Ok((
            Self {
                path: path.to_owned(),
                original,
                encoding,
                crlf,
            },
            text,
        ))
    }

    pub(crate) fn save(&self, text: &str) -> io::Result<()> {
        let converted;
        let text = if self.crlf {
            converted = text.replace("\r\n", "\n").replace('\n', "\r\n");
            converted.as_str()
        } else {
            text
        };
        let mut bytes = Vec::new();
        match self.encoding {
            Encoding::Utf8 => bytes.extend_from_slice(text.as_bytes()),
            Encoding::Utf8Bom => {
                bytes.extend_from_slice(b"\xef\xbb\xbf");
                bytes.extend_from_slice(text.as_bytes());
            }
            Encoding::Utf16Le => {
                bytes.extend_from_slice(b"\xff\xfe");
                bytes.extend(text.encode_utf16().flat_map(u16::to_le_bytes));
            }
            Encoding::Utf16Be => {
                bytes.extend_from_slice(b"\xfe\xff");
                bytes.extend(text.encode_utf16().flat_map(u16::to_be_bytes));
            }
        }
        if bytes == self.original {
            return Ok(());
        }
        check_size(bytes.len() as u64)?;
        let folder = self
            .path
            .parent()
            .ok_or_else(|| io::Error::other("Arbeitsordner fehlt."))?;
        check_workspace(&self.path)?;
        let mut staged = NamedTempFile::new_in(folder)?;
        staged.write_all(&bytes)?;
        staged.as_file().sync_all()?;
        if read_text_bytes(&self.path)? != self.original {
            return Err(io::Error::other(
                "Die Arbeitskopie wurde außerhalb des Texteditors geändert.",
            ));
        }
        staged.persist(&self.path).map_err(|error| error.error)?;
        Ok(())
    }
}

impl App {
    pub(super) fn open_ftp_text_editor(&mut self, path: &Path) -> io::Result<()> {
        let Some(draft) = self.ftp_drafts.iter().find(|draft| draft.path == path) else {
            return Ok(());
        };
        let (edit, body) = TextEdit::load(path)?;
        let title = draft.entry.name.clone();
        self.dialog(
            "multiline",
            Pending::FtpTextEdit(Box::new(edit)),
            Dialog {
                title: &title,
                description: "Änderungen werden in der lokalen Server-Arbeitskopie gespeichert.",
                placeholder: "Dateiinhalt",
                value: &body,
                ..Default::default()
            },
        );
        Ok(())
    }
}

fn read_text_bytes(path: &Path) -> io::Result<Vec<u8>> {
    let file = open_edit(path)?;
    check_size(file.metadata()?.len())?;
    let mut bytes = Vec::new();
    file.take(MAX_TEXT + 1).read_to_end(&mut bytes)?;
    check_size(bytes.len() as u64)?;
    Ok(bytes)
}

fn check_size(len: u64) -> io::Result<()> {
    if len > MAX_TEXT {
        Err(io::Error::other(
            "Der integrierte Texteditor unterstützt Dateien bis 4 MiB.",
        ))
    } else {
        Ok(())
    }
}

fn utf8(bytes: &[u8]) -> io::Result<String> {
    std::str::from_utf8(bytes)
        .map(str::to_owned)
        .map_err(|_| invalid_text())
}

fn utf16(bytes: &[u8], convert: fn([u8; 2]) -> u16) -> io::Result<String> {
    let (pairs, rest) = bytes.as_chunks::<2>();
    if !rest.is_empty() {
        return Err(invalid_text());
    }
    String::from_utf16(&pairs.iter().copied().map(convert).collect::<Vec<_>>())
        .map_err(|_| invalid_text())
}

fn invalid_text() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "Der integrierte Texteditor benötigt UTF-8 oder UTF-16 mit BOM.",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn text_round_trip_preserves_utf16_and_windows_line_endings() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("copy.txt");
        let encode = |text: &str| {
            b"\xff\xfe"
                .iter()
                .copied()
                .chain(text.encode_utf16().flat_map(u16::to_le_bytes))
                .collect::<Vec<_>>()
        };
        let original = encode("alte Zeile\r\nPrüfung\r\n");
        fs::write(&path, &original).unwrap();
        let (draft, body) = TextEdit::load(&path).unwrap();
        assert_eq!(body, "alte Zeile\nPrüfung\n");
        draft.save(&body).unwrap();
        assert_eq!(fs::read(&path).unwrap(), original);
        draft.save("neue Zeile\nPrüfung\n").unwrap();
        assert_eq!(
            fs::read(&path).unwrap(),
            encode("neue Zeile\r\nPrüfung\r\n")
        );
    }

    #[test]
    fn external_edit_conflicts_and_binary_input_keep_the_working_copy() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("copy.txt");
        fs::write(&path, b"initial text").unwrap();
        let (draft, _) = TextEdit::load(&path).unwrap();
        fs::write(&path, b"saved externally").unwrap();
        assert!(draft.save("draft from the internal editor").is_err());
        assert_eq!(fs::read(&path).unwrap(), b"saved externally");
        fs::write(&path, b"binary\x00content").unwrap();
        assert!(TextEdit::load(&path).is_err());
    }
}
