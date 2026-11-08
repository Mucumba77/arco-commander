use crate::files;
use encoding_rs::{Encoding, UTF_8, UTF_16BE, UTF_16LE, WINDOWS_1252};
use similar::{DiffTag, TextDiff};
use std::fs::{self, File, Metadata};
use std::io::{self, Read, Write};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::Duration;

const FILE_LIMIT: u64 = 4 * 1024 * 1024;
const LINE_LIMIT: usize = 200_000;

#[derive(Clone, Copy, Debug, Default)]
pub struct Options {
    pub ignore_case: bool,
    pub ignore_whitespace: bool,
    pub ignore_line_endings: bool,
}

struct Document {
    path: PathBuf,
    bytes: Vec<u8>,
    metadata: Metadata,
    text: String,
    offsets: Vec<usize>,
    encoding: &'static Encoding,
    bom: usize,
}

impl Document {
    fn load(path: &Path, cancel: &AtomicBool) -> io::Result<Self> {
        let metadata = files::regular_metadata(path)?;
        if !metadata.is_file() || metadata.len() > FILE_LIMIT {
            return Err(io::Error::other(
                "Der Textvergleich benötigt reguläre Dateien bis je 4 MiB.",
            ));
        }
        let bytes = read_bytes(path, cancel)?;
        files::ensure_unchanged(path, &metadata)?;
        if bytes.len() as u64 != metadata.len() {
            return Err(io::Error::other("Die Dateigröße hat sich geändert."));
        }
        let (encoding, bom) = Encoding::for_bom(&bytes).unwrap_or_else(|| {
            (
                if std::str::from_utf8(&bytes).is_ok() {
                    UTF_8
                } else {
                    WINDOWS_1252
                },
                0,
            )
        });
        let (text, errors) = encoding.decode_without_bom_handling(&bytes[bom..]);
        if errors
            || text.chars().any(|character| {
                character.is_control() && !matches!(character, '\t' | '\r' | '\n' | '\x0c')
            })
        {
            return Err(io::Error::other(
                "Die Datei enthält Binärdaten oder ungültigen Text. Verwende den Binärvergleich.",
            ));
        }
        let text = text.into_owned();
        let mut offsets = vec![0];
        for line in text.split_inclusive('\n') {
            offsets.push(offsets.last().unwrap() + line.len());
            if offsets.len() > LINE_LIMIT + 1 {
                return Err(io::Error::other(
                    "Der Textvergleich verarbeitet bis zu 200.000 Zeilen pro Datei.",
                ));
            }
        }
        Ok(Self {
            path: path.into(),
            bytes,
            metadata,
            text,
            offsets,
            encoding,
            bom,
        })
    }

    fn region(&self, lines: &Range<usize>) -> Range<usize> {
        self.offsets[lines.start]..self.offsets[lines.end]
    }

    fn encode(&self, text: &str) -> io::Result<Vec<u8>> {
        let mut output = self.bytes[..self.bom].to_vec();
        if self.encoding == UTF_16LE || self.encoding == UTF_16BE {
            for unit in text.encode_utf16() {
                output.extend(if self.encoding == UTF_16LE {
                    unit.to_le_bytes()
                } else {
                    unit.to_be_bytes()
                });
            }
        } else {
            let (bytes, _, errors) = self.encoding.encode(text);
            if errors {
                return Err(io::Error::other(
                    "Der Text ist in der Kodierung der Zieldatei nicht darstellbar.",
                ));
            }
            output.extend(&*bytes);
        }
        if output.len() as u64 > FILE_LIMIT {
            return Err(io::Error::other(
                "Die bearbeitete Textdatei überschreitet 4 MiB.",
            ));
        }
        Ok(output)
    }
}

fn read_bytes(path: &Path, cancel: &AtomicBool) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    crate::tools::copy_cancelled(File::open(path)?.take(FILE_LIMIT + 1), &mut bytes, cancel)?;
    if bytes.len() as u64 > FILE_LIMIT {
        return Err(io::Error::other("Die Textdatei überschreitet 4 MiB."));
    }
    Ok(bytes)
}

pub struct Session {
    documents: [Document; 2],
    pub changes: Vec<[Range<usize>; 2]>,
    pub options: Options,
}

impl Session {
    pub fn load(paths: [&Path; 2], options: Options, cancel: &AtomicBool) -> io::Result<Self> {
        let documents = [
            Document::load(paths[0], cancel)?,
            Document::load(paths[1], cancel)?,
        ];
        files::check_cancel(cancel)?;
        let normalized = documents.each_ref().map(|document| {
            document
                .text
                .split_inclusive('\n')
                .map(|line| normalize(line, options))
                .collect::<Vec<_>>()
        });
        let lines = normalized
            .each_ref()
            .map(|lines| lines.iter().map(String::as_str).collect::<Vec<_>>());
        let diff = TextDiff::configure()
            .timeout(Duration::from_secs(2))
            .diff_slices(&lines[0], &lines[1]);
        let changes = diff
            .ops()
            .iter()
            .filter(|op| op.tag() != DiffTag::Equal)
            .map(|op| [op.old_range(), op.new_range()])
            .collect();
        files::check_cancel(cancel)?;
        Ok(Self {
            documents,
            changes,
            options,
        })
    }

    pub fn paths(&self) -> [&Path; 2] {
        self.documents
            .each_ref()
            .map(|document| document.path.as_path())
    }

    pub fn change_text(&self, change: usize, side: usize) -> Option<&str> {
        let lines = self.changes.get(change)?.get(side)?;
        let document = &self.documents[side];
        Some(&document.text[document.region(lines)])
    }

    pub fn save_change(
        &self,
        change: usize,
        side: usize,
        edited: Option<&str>,
        cancel: &AtomicBool,
        publish: impl FnOnce(&Path, &Path, &Path) -> io::Result<()>,
    ) -> io::Result<()> {
        let lines = self
            .changes
            .get(change)
            .and_then(|change| change.get(side))
            .ok_or_else(|| io::Error::other("Die ausgewählte Änderung fehlt."))?;
        let document = &self.documents[side];
        let other = &self.documents[1 - side];
        if fs::canonicalize(&document.path)? == fs::canonicalize(&other.path)? {
            return Err(io::Error::other("Wähle zwei unterschiedliche Dateien."));
        }
        let value = edited.unwrap_or_else(|| self.change_text(change, 1 - side).unwrap_or(""));
        if value.chars().any(|character| {
            character.is_control() && !matches!(character, '\t' | '\r' | '\n' | '\x0c')
        }) {
            return Err(io::Error::other(
                "Der bearbeitete Text enthält Steuerzeichen.",
            ));
        }
        let range = document.region(lines);
        let mut text = document.text.clone();
        let dominant_crlf =
            text.contains("\r\n") && text.matches("\r\n").count() * 2 >= text.matches('\n').count();
        let normalized = value.replace("\r\n", "\n");
        let inserted = if dominant_crlf {
            normalized.replace('\n', "\r\n")
        } else {
            normalized
        };
        text.replace_range(range, &inserted);
        let output = document.encode(&text)?;
        if output == document.bytes {
            return Ok(());
        }
        let parent = document
            .path
            .parent()
            .ok_or_else(|| io::Error::other("Der Zielordner fehlt."))?;
        let temp = tempfile::tempdir_in(parent)?;
        let staged = temp.path().join("edited-file");
        let backup = temp.path().join("original-file");
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&staged)?;
        file.write_all(&output)?;
        file.sync_all()?;
        drop(file);
        if read_bytes(&document.path, cancel)? != document.bytes {
            return Err(io::Error::other(
                "Die Zieldatei wurde seit dem Vergleich geändert.",
            ));
        }
        files::ensure_unchanged(&document.path, &document.metadata)?;
        files::check_cancel(cancel)?;
        if let Err(error) = publish(&staged, &document.path, &backup) {
            if backup.exists() {
                let saved = temp.keep().join("original-file");
                return Err(io::Error::other(format!(
                    "{error}. Sicherung liegt unter {}",
                    saved.display()
                )));
            }
            return Err(error);
        }
        Ok(())
    }
}

fn normalize(line: &str, options: Options) -> String {
    let (body, eol) = if let Some(body) = line.strip_suffix("\r\n") {
        (
            body,
            if options.ignore_line_endings {
                "\n"
            } else {
                "\r\n"
            },
        )
    } else if let Some(body) = line.strip_suffix('\n') {
        (body, "\n")
    } else {
        (line, "")
    };
    let mut value = if options.ignore_whitespace {
        body.split_whitespace().collect::<Vec<_>>().join(" ")
    } else {
        body.to_owned()
    };
    if options.ignore_case {
        value = value.to_lowercase();
    }
    value.push_str(eol);
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merges_one_change_and_preserves_target_utf16_and_newlines() {
        let fixture = tempfile::tempdir().unwrap();
        let left = fixture.path().join("left.txt");
        let right = fixture.path().join("right.txt");
        let utf16 = |text: &str| -> Vec<u8> {
            [0xff, 0xfe]
                .into_iter()
                .chain(text.encode_utf16().flat_map(u16::to_le_bytes))
                .collect()
        };
        fs::write(&left, utf16("gleich\r\nlinks\r\ngleich\r\nalt\r\n")).unwrap();
        fs::write(&right, "gleich\nrechts\ngleich\nneu\n").unwrap();
        let cancel = AtomicBool::new(false);
        let options = Options {
            ignore_line_endings: true,
            ..Options::default()
        };
        let session = Session::load([&left, &right], options, &cancel).unwrap();
        assert_eq!(session.changes.len(), 2);
        assert_eq!(session.change_text(0, 1), Some("rechts\n"));
        session
            .save_change(0, 0, None, &cancel, |staged, target, _| {
                fs::rename(staged, target)
            })
            .unwrap();
        assert_eq!(
            fs::read(&left).unwrap(),
            utf16("gleich\r\nrechts\r\ngleich\r\nalt\r\n")
        );
        assert_eq!(
            fs::read_to_string(&right).unwrap(),
            "gleich\nrechts\ngleich\nneu\n"
        );
        assert!(
            session
                .save_change(1, 0, None, &cancel, |_, _, _| panic!("stale document"))
                .is_err()
        );
    }
}
