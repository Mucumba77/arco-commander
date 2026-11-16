use crate::files::{self, Directory, Entry, Kind};
use crate::ignore::IgnoreList;
use crate::tools;
use chrono::{DateTime, Local, NaiveDate, NaiveDateTime};
use regex::{Regex, RegexBuilder, bytes};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use walkdir::WalkDir;

const CONTENT_LIMIT: u64 = 64 * 1024 * 1024;
mod content;
mod locations;
mod names;
mod partner;
pub use locations::{list_marker, search_locations};
use names::SearchMasks;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_counts_candidates_and_hits_and_can_cancel_before_reading() {
        let fixture = tempfile::tempdir().unwrap();
        fs::write(fixture.path().join("found.txt"), b"needle").unwrap();
        fs::write(fixture.path().join("other.txt"), b"nothing").unwrap();
        let roots = [fixture.path().to_path_buf()];
        let query = Options {
            names: "*.txt".into(),
            content_mode: 1,
            content: "needle".into(),
            ..Default::default()
        }
        .compile()
        .unwrap();
        let cancel = AtomicBool::new(false);
        let mut counts = Vec::new();
        let found = search_with_progress(&roots, false, &query, true, None, &cancel, |update| {
            assert!(update.path.starts_with(fixture.path()));
            counts.push((update.examined, update.matched, update.errors));
        });
        assert_eq!(counts.first(), Some(&(0, 0, 0)));
        assert_eq!(counts.last(), Some(&(2, 1, 0)));
        assert!(counts.windows(2).all(|pair| pair[0].0 <= pair[1].0));
        assert_eq!(found.directory.entries.len(), 1);
        let aborted = search_with_progress(&roots, false, &query, true, None, &cancel, |_| {
            cancel.store(true, Ordering::Relaxed);
        });
        assert!(aborted.cancelled);
        assert!(aborted.directory.entries.is_empty());
    }

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
    pub whole_words: bool,
    pub content_not: bool,
    pub office_xml: bool,
    pub archives: bool,
    pub partner_extensions: String,
    pub partner_not: bool,
    pub minimum_bytes: String,
    pub maximum_bytes: String,
    pub modified_from: String,
    pub modified_to: String,
    pub minimum_age: String,
    pub maximum_age: String,
    pub attributes: String,
    pub depth: String,
    pub roots: String,
    pub selected_only: bool,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub wdx_profile: String,
}

#[derive(Clone)]
enum Name {
    Masks(SearchMasks),
    Regex(Regex),
}

#[derive(Clone)]
enum Content {
    Text(Regex, u8),
    Hex(bytes::Regex),
}

#[derive(Clone)]
pub struct Query {
    name: Name,
    content: Option<Content>,
    minimum_bytes: Option<u64>,
    maximum_bytes: Option<u64>,
    from: Option<NaiveDateTime>,
    to: Option<NaiveDateTime>,
    minimum_age: Option<std::time::Duration>,
    maximum_age: Option<std::time::Duration>,
    now: std::time::SystemTime,
    content_not: bool,
    office_xml: bool,
    root: Option<PathBuf>,
    match_case: bool,
    only_roots: bool,
    partner: Option<partner::Partner>,
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
            Name::Masks(SearchMasks::new(&self.names)?)
        };
        let content = match (self.content.is_empty(), self.content_mode) {
            (true, _) | (_, 0) => None,
            (false, 3) => Some(Content::Hex(hex_pattern(&self.content, !self.match_case)?)),
            _ => {
                let expression = if self.content_mode == 1 {
                    regex::escape(&content::unescape(&self.content)?)
                } else {
                    self.content.clone()
                };
                let expression = if self.whole_words {
                    format!(r"(?:^|[^\p{{L}}])(?:{expression})(?:$|[^\p{{L}}])")
                } else {
                    expression
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
                .then(|| content::quantity(text, false))
                .transpose()
        };
        let parse_date = |text: &str, end: bool| -> io::Result<Option<NaiveDateTime>> {
            let text = text.trim();
            (!text.is_empty())
                .then(|| {
                    NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M:%S")
                        .or_else(|_| NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M"))
                        .or_else(|_| {
                            NaiveDate::parse_from_str(text, "%Y-%m-%d").map(|date| {
                                if end {
                                    date.and_hms_nano_opt(23, 59, 59, 999999999).unwrap()
                                } else {
                                    date.and_hms_opt(0, 0, 0).unwrap()
                                }
                            })
                        })
                        .map_err(|_| {
                            io::Error::other(
                                "Verwende JJJJ-MM-TT, optional mit HH:MM oder HH:MM:SS.",
                            )
                        })
                })
                .transpose()
        };
        let minimum_bytes = parse_bytes(&self.minimum_bytes)?;
        let maximum_bytes = parse_bytes(&self.maximum_bytes)?;
        let from = parse_date(&self.modified_from, false)?;
        let to = parse_date(&self.modified_to, true)?;
        let minimum_age = content::age(&self.minimum_age)?;
        let maximum_age = content::age(&self.maximum_age)?;
        if minimum_bytes
            .zip(maximum_bytes)
            .is_some_and(|(minimum, maximum)| minimum > maximum)
            || from.zip(to).is_some_and(|(from, to)| from > to)
            || minimum_age
                .zip(maximum_age)
                .is_some_and(|(min, max)| min > max)
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
            minimum_age,
            maximum_age,
            now: std::time::SystemTime::now(),
            content_not: self.content_not,
            office_xml: self.office_xml,
            root: None,
            match_case: self.match_case,
            only_roots: false,
            partner: partner::Partner::compile(&self.partner_extensions, self.partner_not)?,
            required,
            excluded,
            max_depth,
        })
    }
}

impl Query {
    pub fn only_roots(&self) -> Self {
        let mut query = self.clone();
        query.only_roots = true;
        query.name = Name::Masks(SearchMasks::new("*").expect("fixed mask"));
        query
    }
    pub fn searches_only_roots(&self) -> bool {
        self.only_roots
    }
    pub fn scoped(&self, root: &Path) -> Self {
        let mut query = self.clone();
        query.root = Some(root.to_path_buf());
        query
    }

    pub fn filter_empty_directories(&self, entries: &mut Vec<Entry>) {
        if !matches!(&self.name, Name::Masks(masks) if masks.remove_empty) {
            return;
        }
        let parents = entries
            .iter()
            .filter(|entry| entry.kind == Kind::File)
            .flat_map(|entry| entry.path.parent().into_iter().flat_map(Path::ancestors))
            .map(Path::to_path_buf)
            .collect::<std::collections::HashSet<_>>();
        entries.retain(|entry| entry.kind != Kind::Directory || parents.contains(&entry.path));
    }

    pub fn requires_content(&self) -> bool {
        self.content.is_some()
    }

    pub fn matches_metadata(&self, entry: &Entry) -> bool {
        let flags = entry.attributes | if entry.kind == Kind::Directory { 16 } else { 0 };
        let named = match &self.name {
            Name::Masks(masks) => masks.matches(entry, self.root.as_deref()),
            Name::Regex(regex) => regex.is_match(&entry.name.to_string_lossy()),
        };
        if !named
            || self
                .minimum_bytes
                .is_some_and(|size| entry.size.is_none_or(|actual| actual < size))
            || self
                .maximum_bytes
                .is_some_and(|size| entry.size.is_none_or(|actual| actual > size))
            || flags & self.required != self.required
            || flags & self.excluded != 0
        {
            return false;
        }
        if self.from.is_some() || self.to.is_some() {
            let date = entry
                .modified
                .map(|time| DateTime::<Local>::from(time).naive_local());
            if self
                .from
                .is_some_and(|from| date.is_none_or(|date| date < from))
                || self.to.is_some_and(|to| date.is_none_or(|date| date > to))
            {
                return false;
            }
        }
        if self.minimum_age.is_some() || self.maximum_age.is_some() {
            let age = entry
                .modified
                .map(|date| self.now.duration_since(date).unwrap_or_default());
            if self
                .minimum_age
                .is_some_and(|min| age.is_none_or(|age| age < min))
                || self
                    .maximum_age
                    .is_some_and(|max| age.is_none_or(|age| age > max))
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
        if !self.matches_partner(entry, |path| {
            files::check_cancel(cancel)?;
            match files::regular_metadata(path) {
                Ok(metadata) => Ok(metadata.is_file()),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
                Err(error) => Err(error),
            }
        })? {
            return Ok(false);
        }
        self.matches_content(entry, cancel)
    }

    pub fn matches_content(&self, entry: &Entry, cancel: &AtomicBool) -> io::Result<bool> {
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
        let found = if self.office_xml && content::is_office(&entry.path) {
            content::office_matches(
                &data,
                |text| match content {
                    Content::Text(regex, _) => regex.is_match(text),
                    Content::Hex(regex) => regex.is_match(text.as_bytes()),
                },
                cancel,
            )?
        } else {
            match content {
                Content::Hex(regex) => regex.is_match(&data),
                Content::Text(regex, encoding) => {
                    content::decoded_matches(&data, *encoding, |text| regex.is_match(text))
                }
            }
        };
        files::check_cancel(cancel)?;
        Ok(found != self.content_not)
    }
}

#[derive(Clone)]
pub struct Found {
    pub directory: Directory,
    pub cancelled: bool,
}

pub struct Progress<'a> {
    pub path: &'a Path,
    pub examined: usize,
    pub matched: usize,
    pub errors: usize,
}

pub fn search(
    roots: &[PathBuf],
    include_roots: bool,
    query: &Query,
    show_hidden: bool,
    ignore: Option<&IgnoreList>,
    cancel: &AtomicBool,
) -> Found {
    search_with_progress(
        roots,
        include_roots,
        query,
        show_hidden,
        ignore,
        cancel,
        |_| {},
    )
}

pub fn search_with_progress(
    roots: &[PathBuf],
    include_roots: bool,
    query: &Query,
    show_hidden: bool,
    ignore: Option<&IgnoreList>,
    cancel: &AtomicBool,
    mut progress: impl FnMut(Progress<'_>),
) -> Found {
    let mut found = Found {
        directory: Directory {
            entries: Vec::new(),
            errors: Vec::new(),
        },
        cancelled: false,
    };
    let mut seen = BTreeSet::new();
    let mut empty_candidates = std::collections::HashSet::new();
    let mut examined = 0;
    let (locations, errors) = locations::expand(roots, include_roots, query.max_depth, cancel);
    found.directory.errors.extend(errors);
    for location in locations {
        let root = &location.path;
        let override_query = location
            .names
            .as_ref()
            .map(|names| {
                let mut scoped = query.clone();
                scoped.name = if let Some(regex) = names.strip_prefix('<') {
                    Name::Regex(
                        RegexBuilder::new(regex)
                            .case_insensitive(!query.match_case)
                            .build()
                            .map_err(io::Error::other)?,
                    )
                } else {
                    Name::Masks(SearchMasks::new(names)?)
                };
                Ok::<_, io::Error>(scoped)
            })
            .transpose();
        let override_query = match override_query {
            Ok(value) => value,
            Err(error) => {
                found.directory.errors.push(error.to_string());
                continue;
            }
        };
        let mut scoped = override_query.unwrap_or_else(|| query.clone());
        scoped.root = Some(if root.is_dir() {
            root.clone()
        } else {
            root.parent().unwrap_or(root).to_path_buf()
        });
        let query = &scoped;
        let mut walk = WalkDir::new(root)
            .follow_links(false)
            .follow_root_links(false);
        if !location.include_self && root.is_dir() {
            walk = walk.min_depth(1);
        }
        if location.self_only || query.only_roots {
            walk = walk.max_depth(0);
        } else if let Some(depth) = query.max_depth {
            walk = walk.max_depth(depth.saturating_add(1));
        }
        for item in walk.into_iter().filter_entry(|item| {
            item.depth() == 0
                || (!ignore.is_some_and(|list| list.local(item.path(), item.file_type().is_dir()))
                    && (!item.file_type().is_dir() || !matches!(&query.name, Name::Masks(masks) if !masks.visits(item.path(), query.root.as_deref())))
                    && item
                        .metadata()
                        .is_ok_and(|metadata| !files::is_link(&metadata)))
        }) {
            if cancel.load(Ordering::Relaxed) {
                found.cancelled = true;
                return found;
            }
            let current = item
                .as_ref()
                .ok()
                .map(|entry| entry.path())
                .unwrap_or(root)
                .to_path_buf();
            progress(Progress {
                path: &current,
                examined,
                matched: found.directory.entries.len(),
                errors: found.directory.errors.len(),
            });
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
                Ok(Some(entry)) => {
                    if entry.kind == Kind::Directory && matches!(&query.name, Name::Masks(masks) if masks.remove_empty) { empty_candidates.insert(entry.path.clone()); }
                    found.directory.entries.push(entry);
                },
                Ok(None) => (),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                    found.cancelled = true;
                    return found;
                }
                Err(error) => found.directory.errors.push(error.to_string()),
            }
            examined += 1;
            progress(Progress {
                path: &current,
                examined,
                matched: found.directory.entries.len(),
                errors: found.directory.errors.len(),
            });
        }
    }
    found.cancelled |= cancel.load(Ordering::Relaxed);
    if !empty_candidates.is_empty() {
        let parents = found
            .directory
            .entries
            .iter()
            .filter(|entry| entry.kind == Kind::File)
            .flat_map(|entry| entry.path.parent().into_iter().flat_map(Path::ancestors))
            .map(Path::to_path_buf)
            .collect::<std::collections::HashSet<_>>();
        found.directory.entries.retain(|entry| {
            !empty_candidates.contains(&entry.path) || parents.contains(&entry.path)
        });
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
                'd' => 16,
                'c' => 2048,
                'e' => 16384,
                'o' => 4096,
                't' => 256,
                _ => {
                    return Err(io::Error::other(
                        "Attributregeln verwenden r, h, s, a, d (Ordner), c (komprimiert), e (verschlüsselt), o (offline) oder t (temporär).",
                    ));
                }
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

fn hex_pattern(value: &str, ignore_case: bool) -> io::Result<bytes::Regex> {
    if value.len() > 8192 || value.trim().is_empty() {
        return Err(io::Error::other(
            "Gib Hex-Bytes oder Text in Anführungszeichen ein.",
        ));
    }
    let mut chars = value.chars().peekable();
    let mut data = Vec::new();
    while let Some(ch) = chars.next() {
        if ch.is_whitespace() {
            continue;
        }
        if ch == '"' {
            let mut text = String::new();
            let mut closed = false;
            for ch in chars.by_ref() {
                if ch == '"' {
                    closed = true;
                    break;
                }
                text.push(ch);
            }
            if !closed {
                return Err(io::Error::other(
                    "Ein Anführungszeichen in der Hex-Suche ist offen.",
                ));
            }
            let (bytes, _, errors) = encoding_rs::WINDOWS_1252.encode(&text);
            if errors {
                return Err(io::Error::other(
                    "Text der Hex-Suche ist nicht in Windows-1252 darstellbar.",
                ));
            }
            data.extend_from_slice(&bytes);
        } else {
            let first = ch
                .to_digit(16)
                .ok_or_else(|| io::Error::other("Ungültiges Hex-Byte."))?;
            while chars.peek().is_some_and(|ch| ch.is_whitespace()) {
                chars.next();
            }
            let second = chars
                .next()
                .and_then(|ch| ch.to_digit(16))
                .ok_or_else(|| io::Error::other("Ein Hex-Byte ist unvollständig."))?;
            data.push((first * 16 + second) as u8);
        }
    }
    if data.is_empty() {
        return Err(io::Error::other("Die Hex-Suche ist leer."));
    }
    let expression: String = data.iter().map(|byte| format!("\\x{byte:02x}")).collect();
    bytes::RegexBuilder::new(&expression)
        .unicode(false)
        .case_insensitive(ignore_case)
        .build()
        .map_err(io::Error::other)
}
