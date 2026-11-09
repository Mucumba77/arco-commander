use crate::files::{self, Directory, Entry, Kind};
use crate::ignore::IgnoreList;
use crate::{sync::FileMasks, tools};
use chrono::{DateTime, Local, NaiveDate};
use regex::{Regex, RegexBuilder, bytes};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use walkdir::WalkDir;

const CONTENT_LIMIT: u64 = 64 * 1024 * 1024;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_encoded_text_across_roots_with_masks_and_depth() {
        let fixture = tempfile::tempdir().unwrap();
        let left = fixture.path().join("left");
        let right = fixture.path().join("right");
        fs::create_dir_all(left.join("nested")).unwrap();
        fs::create_dir(&right).unwrap();
        let expected = [left.join("a.txt"), right.join("b.txt")];
        let mut data = vec![0xff, 0xfe];
        for unit in "Grüße".encode_utf16() {
            data.extend(unit.to_le_bytes());
        }
        for path in expected.iter().cloned().chain([
            left.join("skip.txt"),
            left.join("nested/c.txt"),
            right.join("not-text.bak"),
        ]) {
            fs::write(path, &data).unwrap();
        }
        let roots = [left, right];
        let query = Options {
            names: "*.txt | skip*".into(),
            content: "Grüße".into(),
            content_mode: 1,
            encoding: 2,
            depth: "0".into(),
            minimum_bytes: "2".into(),
            ..Options::default()
        }
        .compile()
        .unwrap();
        let found = search(&roots, false, &query, true, None, &AtomicBool::new(false));
        assert!(found.directory.errors.is_empty());
        assert!(!found.cancelled);
        assert_eq!(
            found
                .directory
                .entries
                .into_iter()
                .map(|entry| entry.path)
                .collect::<Vec<_>>(),
            expected
        );
        assert!(search(&roots, false, &query, true, None, &AtomicBool::new(true)).cancelled);
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Options {
    pub names: String,
    pub content: String,
    pub content_mode: u8,
    pub encoding: u8,
    pub match_case: bool,
    pub minimum_bytes: String,
    pub maximum_bytes: String,
    pub modified_from: String,
    pub modified_to: String,
    pub attributes: String,
    pub depth: String,
    pub roots: String,
    pub selected_only: bool,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub wdx_profile: String,
}

enum Name {
    Masks(FileMasks),
    Regex(Regex),
}

enum Content {
    Text(Regex, u8),
    Hex(bytes::Regex),
}

pub struct Query {
    name: Name,
    content: Option<Content>,
    minimum_bytes: Option<u64>,
    maximum_bytes: Option<u64>,
    from: Option<NaiveDate>,
    to: Option<NaiveDate>,
    required: u32,
    excluded: u32,
    pub max_depth: Option<usize>,
}

impl Options {
    pub fn everything(&self) -> Option<(bool, &str)> {
        let (prefix, expression) = self.names.trim().split_once(':')?;
        match prefix.to_ascii_lowercase().as_str() {
            "ev" => Some((false, expression.trim())),
            "ed" => Some((true, expression.trim())),
            _ => None,
        }
    }

    pub fn compile(&self) -> io::Result<Query> {
        let name = if let Some(expression) = self.names.trim().strip_prefix('~') {
            Name::Regex(
                RegexBuilder::new(expression)
                    .case_insensitive(!self.match_case)
                    .build()
                    .map_err(io::Error::other)?,
            )
        } else {
            Name::Masks(FileMasks::new(&self.names)?)
        };
        let content = match (self.content.is_empty(), self.content_mode) {
            (true, _) | (_, 0) => None,
            (false, 3) => Some(Content::Hex(hex_pattern(&self.content)?)),
            _ => {
                let expression = if self.content_mode == 1 {
                    regex::escape(&self.content)
                } else {
                    self.content.clone()
                };
                let regex = RegexBuilder::new(&expression)
                    .case_insensitive(!self.match_case)
                    .build()
                    .map_err(io::Error::other)?;
                Some(Content::Text(regex, self.encoding))
            }
        };
        let parse_bytes = |text: &str| -> io::Result<Option<u64>> {
            let text = text.trim();
            (!text.is_empty())
                .then(|| {
                    text.parse()
                        .map_err(|_| io::Error::other("Gib Größen als ganze Byte-Anzahl ein."))
                })
                .transpose()
        };
        let parse_date = |text: &str| -> io::Result<Option<NaiveDate>> {
            let text = text.trim();
            (!text.is_empty())
                .then(|| {
                    NaiveDate::parse_from_str(text, "%Y-%m-%d")
                        .map_err(|_| io::Error::other("Verwende für das Datum JJJJ-MM-TT."))
                })
                .transpose()
        };
        let minimum_bytes = parse_bytes(&self.minimum_bytes)?;
        let maximum_bytes = parse_bytes(&self.maximum_bytes)?;
        let from = parse_date(&self.modified_from)?;
        let to = parse_date(&self.modified_to)?;
        if minimum_bytes
            .zip(maximum_bytes)
            .is_some_and(|(minimum, maximum)| minimum > maximum)
            || from.zip(to).is_some_and(|(from, to)| from > to)
        {
            return Err(io::Error::other(
                "Der untere Grenzwert liegt über dem oberen.",
            ));
        }
        let (required, excluded) = attributes(&self.attributes)?;
        let max_depth = self.depth.trim();
        let max_depth = (!max_depth.is_empty())
            .then(|| {
                max_depth
                    .parse::<usize>()
                    .map_err(|_| io::Error::other("Gib die Ordnertiefe als ganze Zahl ein."))
            })
            .transpose()?;
        Ok(Query {
            name,
            content,
            minimum_bytes,
            maximum_bytes,
            from,
            to,
            required,
            excluded,
            max_depth,
        })
    }
}

impl Query {
    pub fn requires_content(&self) -> bool {
        self.content.is_some()
    }

    pub fn matches_metadata(&self, entry: &Entry) -> bool {
        let named = match &self.name {
            Name::Masks(masks) => masks.includes(&entry.path),
            Name::Regex(regex) => regex.is_match(&entry.name.to_string_lossy()),
        };
        if !named
            || self
                .minimum_bytes
                .is_some_and(|size| entry.size.is_none_or(|actual| actual < size))
            || self
                .maximum_bytes
                .is_some_and(|size| entry.size.is_none_or(|actual| actual > size))
            || entry.attributes & self.required != self.required
            || entry.attributes & self.excluded != 0
        {
            return false;
        }
        if self.from.is_some() || self.to.is_some() {
            let date = entry
                .modified
                .map(|time| DateTime::<Local>::from(time).date_naive());
            if self
                .from
                .is_some_and(|from| date.is_none_or(|date| date < from))
                || self.to.is_some_and(|to| date.is_none_or(|date| date > to))
            {
                return false;
            }
        }
        true
    }

    pub fn matches(&self, entry: &Entry, cancel: &AtomicBool) -> io::Result<bool> {
        if !self.matches_metadata(entry) {
            return Ok(false);
        }
        let Some(content) = &self.content else {
            return Ok(true);
        };
        if entry.kind != Kind::File {
            return Ok(false);
        }
        let metadata = files::regular_metadata(&entry.path)?;
        if metadata.len() > CONTENT_LIMIT {
            return Err(io::Error::other(format!(
                "Datei ist für die Inhaltssuche größer als 64 MiB: {}",
                entry.path.display()
            )));
        }
        let mut data = Vec::new();
        tools::copy_cancelled(
            File::open(&entry.path)?.take(CONTENT_LIMIT + 1),
            &mut data,
            cancel,
        )?;
        files::ensure_unchanged(&entry.path, &metadata)?;
        let found = match content {
            Content::Hex(regex) => regex.is_match(&data),
            Content::Text(regex, encoding) => {
                use encoding_rs::{UTF_8, UTF_16BE, UTF_16LE, WINDOWS_1252};
                let text = match encoding {
                    1 => UTF_8.decode_with_bom_removal(&data).0,
                    2 => UTF_16LE.decode_with_bom_removal(&data).0,
                    3 => UTF_16BE.decode_with_bom_removal(&data).0,
                    4 => WINDOWS_1252.decode_without_bom_handling(&data).0,
                    _ => tools::decode_text(&data).into(),
                };
                regex.is_match(&text)
            }
        };
        files::check_cancel(cancel)?;
        Ok(found)
    }
}

pub struct Found {
    pub directory: Directory,
    pub cancelled: bool,
}

pub fn search(
    roots: &[PathBuf],
    include_roots: bool,
    query: &Query,
    show_hidden: bool,
    ignore: Option<&IgnoreList>,
    cancel: &AtomicBool,
) -> Found {
    let mut found = Found {
        directory: Directory {
            entries: Vec::new(),
            errors: Vec::new(),
        },
        cancelled: false,
    };
    let mut seen = BTreeSet::new();
    for root in roots {
        let mut walk = WalkDir::new(root)
            .follow_links(false)
            .follow_root_links(false);
        if !include_roots && root.is_dir() {
            walk = walk.min_depth(1);
        }
        if let Some(depth) = query.max_depth {
            walk = walk.max_depth(depth.saturating_add(1));
        }
        for item in walk.into_iter().filter_entry(|item| {
            item.depth() == 0
                || (!ignore.is_some_and(|list| list.local(item.path(), item.file_type().is_dir()))
                    && item
                        .metadata()
                        .is_ok_and(|metadata| !files::is_link(&metadata)))
        }) {
            if cancel.load(Ordering::Relaxed) {
                found.cancelled = true;
                return found;
            }
            let result = (|| {
                let item = item.map_err(io::Error::other)?;
                let path = item.into_path();
                let name = path.file_name().unwrap_or_default().to_os_string();
                let entry = Entry::read(path, name)?;
                if entry.kind == Kind::Link
                    || (!show_hidden && entry.hidden)
                    || ignore.is_some_and(|ignore| {
                        ignore.local(&entry.path, entry.kind == Kind::Directory)
                    })
                    || !query.matches(&entry, cancel)?
                {
                    return Ok(None);
                }
                let key = fs::canonicalize(&entry.path)?;
                Ok::<_, io::Error>(seen.insert(key).then_some(entry))
            })();
            match result {
                Ok(Some(entry)) => found.directory.entries.push(entry),
                Ok(None) => (),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                    found.cancelled = true;
                    return found;
                }
                Err(error) => found.directory.errors.push(error.to_string()),
            }
        }
    }
    found
}

pub fn attributes(value: &str) -> io::Result<(u32, u32)> {
    let mut required = 0;
    let mut excluded = 0;
    for term in value.split_whitespace() {
        let flags = match term.as_bytes().first() {
            Some(b'+') => &mut required,
            Some(b'-') => &mut excluded,
            _ => return Err(io::Error::other("Verwende Attributregeln wie +r -hs.")),
        };
        for character in term[1..].chars() {
            *flags |= match character.to_ascii_lowercase() {
                'r' => 1,
                'h' => 2,
                's' => 4,
                'a' => 32,
                _ => return Err(io::Error::other("Attributregeln verwenden r, h, s oder a.")),
            };
        }
    }
    if required & excluded != 0 {
        return Err(io::Error::other(
            "Ein Attribut kann nicht zugleich Pflicht und ausgeschlossen sein.",
        ));
    }
    Ok((required, excluded))
}

fn hex_pattern(value: &str) -> io::Result<bytes::Regex> {
    let value: String = value.chars().filter(|c| !c.is_whitespace()).collect();
    if value.len() > 8192
        || value.is_empty()
        || value.len() % 2 != 0
        || !value.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(io::Error::other(
            "Gib vollständige Hex-Bytes wie 4D 5A ein.",
        ));
    }
    let mut expression = String::with_capacity(value.len() * 2);
    for index in (0..value.len()).step_by(2) {
        expression.push_str("\\x");
        expression.push_str(&value[index..index + 2]);
    }
    bytes::RegexBuilder::new(&expression)
        .unicode(false)
        .build()
        .map_err(io::Error::other)
}
