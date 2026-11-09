use crate::files;
use regex::{Captures, Regex, RegexBuilder};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

mod times;
use times::TimeState;
pub use times::Times;

pub struct PreviewContext<'a> {
    pub times: Times,
    pub cancel: Option<&'a AtomicBool>,
}

#[derive(Clone, Debug)]
pub struct Plan {
    entries: Vec<(PathBuf, PathBuf)>,
    before: Vec<fs::Metadata>,
    skipped_capture: usize,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SearchOptions {
    pub find: String,
    pub replace: String,
    pub regex: bool,
    pub wildcards: bool,
    pub match_case: bool,
    pub first_only: bool,
    pub include_extension: bool,
    pub substitute: bool,
}

pub struct Search {
    pattern: Regex,
    replacement: Replacement,
    first_only: bool,
    include_extension: bool,
    substitute: bool,
}

enum Replacement {
    Regex(String),
    Literal(Vec<String>),
    Wildcard(Vec<WildcardRule>),
}

struct WildcardRule {
    group: usize,
    replacement: Vec<WildcardPart>,
}

enum WildcardPart {
    Text(String),
    Capture(usize),
}

impl SearchOptions {
    pub fn from_legacy(value: &str) -> io::Result<Self> {
        let (find, replace) = if value.is_empty() {
            ("", "")
        } else {
            value
                .split_once("=>")
                .ok_or_else(|| io::Error::other("Ersetzung benötigt Muster=>Ersatz."))?
        };
        Ok(Self {
            find: find.into(),
            replace: replace.into(),
            regex: !value.is_empty(),
            match_case: !value.is_empty(),
            include_extension: !value.is_empty(),
            ..Self::default()
        })
    }

    pub fn compile(&self) -> io::Result<Option<Search>> {
        if self.find.len() > 4096 || self.replace.len() > 4096 {
            return Err(io::Error::other(
                "Such- und Ersetzungstext sind auf 4096 Bytes begrenzt.",
            ));
        }
        if self.find.is_empty() && (!self.regex || self.replace.is_empty()) {
            if self.replace.is_empty() {
                return Ok(None);
            }
            return Err(io::Error::other("Gib einen Suchtext ein."));
        }
        let (pattern, replacement) = if self.regex {
            (self.find.clone(), Replacement::Regex(self.replace.clone()))
        } else {
            let terms = self.find.split('|').collect::<Vec<_>>();
            if terms.contains(&"") || terms.len() > 256 {
                return Err(io::Error::other(
                    "Gib bis zu 256 nicht leere Suchtexte getrennt durch | ein.",
                ));
            }
            let mut replacements: Vec<_> = self.replace.split('|').map(str::to_owned).collect();
            if replacements.len() == 1 {
                replacements.resize(terms.len(), replacements[0].clone());
            }
            if replacements.len() != terms.len() {
                return Err(io::Error::other(
                    "Gib einen gemeinsamen Ersatz oder je Suchtext einen Ersatz an.",
                ));
            }
            if self.wildcards {
                wildcard_rules(&terms, &replacements)?
            } else {
                let pattern = terms
                    .iter()
                    .map(|term| format!("({})", regex::escape(term)))
                    .collect::<Vec<_>>()
                    .join("|");
                (pattern, Replacement::Literal(replacements))
            }
        };
        let pattern = RegexBuilder::new(&pattern)
            .case_insensitive(!self.match_case)
            .build()
            .map_err(io::Error::other)?;
        Ok(Some(Search {
            pattern,
            replacement,
            first_only: self.first_only,
            include_extension: self.include_extension,
            substitute: self.substitute,
        }))
    }
}

fn wildcard_rules(terms: &[&str], replacements: &[String]) -> io::Result<(String, Replacement)> {
    let mut pattern = String::new();
    let mut rules = Vec::new();
    let mut group = 1;
    for (term, replacement) in terms.iter().zip(replacements) {
        if !pattern.is_empty() {
            pattern.push('|');
        }
        pattern.push('(');
        let rule_group = group;
        group += 1;
        let mut stars = Vec::new();
        let mut characters = Vec::new();
        let mut start = 0;
        for (offset, marker) in term.match_indices(['*', '?']) {
            pattern.push_str(&regex::escape(&term[start..offset]));
            if marker == "*" {
                pattern.push_str("(.*)");
                stars.push(group);
            } else {
                pattern.push_str("(.)");
                characters.push(group);
            }
            group += 1;
            start = offset + 1;
        }
        pattern.push_str(&regex::escape(&term[start..]));
        pattern.push(')');

        let mut stars = stars.into_iter();
        let mut characters = characters.into_iter();
        let mut parts = Vec::new();
        start = 0;
        for (offset, marker) in replacement.match_indices(['*', '?']) {
            let capture = if marker == "*" {
                stars.next()
            } else {
                characters.next()
            };
            let capture = capture.ok_or_else(|| io::Error::other(
                "Jeder Platzhalter im Ersatz benötigt einen gleichartigen Platzhalter im Suchmuster.",
            ))?;
            parts.push(WildcardPart::Text(replacement[start..offset].into()));
            parts.push(WildcardPart::Capture(capture));
            start = offset + 1;
        }
        parts.push(WildcardPart::Text(replacement[start..].into()));
        rules.push(WildcardRule {
            group: rule_group,
            replacement: parts,
        });
    }
    Ok((pattern, Replacement::Wildcard(rules)))
}

impl Search {
    fn apply(&self, name: &str) -> String {
        let write_replacement = |captures: &Captures<'_>, output: &mut String| match &self
            .replacement
        {
            Replacement::Regex(replacement) => captures.expand(replacement, output),
            Replacement::Literal(replacements) => {
                if let Some(index) = captures.iter().skip(1).position(|group| group.is_some()) {
                    output.push_str(&replacements[index]);
                }
            }
            Replacement::Wildcard(rules) => {
                if let Some(rule) = rules.iter().find(|rule| captures.get(rule.group).is_some()) {
                    for part in &rule.replacement {
                        match part {
                            WildcardPart::Text(text) => output.push_str(text),
                            WildcardPart::Capture(group) => {
                                if let Some(matched) = captures.get(*group) {
                                    output.push_str(matched.as_str());
                                }
                            }
                        }
                    }
                }
            }
        };
        let mut output = String::new();
        let mut end = 0;
        for captures in self.pattern.captures_iter(name) {
            let matched = captures.get(0).expect("regex group zero");
            if self.substitute {
                write_replacement(&captures, &mut output);
                return output;
            }
            output.push_str(&name[end..matched.start()]);
            write_replacement(&captures, &mut output);
            end = matched.end();
            if self.first_only {
                break;
            }
        }
        output.push_str(&name[end..]);
        output
    }
}

fn join_name(mut stem: String, extension: Option<String>) -> String {
    if let Some(extension) = extension.filter(|extension| !extension.is_empty()) {
        stem.push('.');
        stem.push_str(&extension);
    }
    stem
}

impl Plan {
    pub fn single(path: &Path, name: &OsStr) -> io::Result<Self> {
        let target = path.with_file_name(files::leaf_name(name)?);
        let entries = if path.as_os_str() == target.as_os_str() {
            Vec::new()
        } else {
            vec![(path.to_path_buf(), target)]
        };
        Self::from_entries(entries)
    }

    pub fn new(
        paths: &[PathBuf],
        mask: &str,
        replacement: Option<(&Regex, &str)>,
    ) -> io::Result<Self> {
        Self::new_with_fields(paths, mask, replacement, |_, _| None)
    }

    pub fn new_with_fields(
        paths: &[PathBuf],
        mask: &str,
        replacement: Option<(&Regex, &str)>,
        field: impl FnMut(&Path, &str) -> Option<String>,
    ) -> io::Result<Self> {
        Self::new_with_clipboard(paths, mask, replacement, None, field)
    }

    pub fn new_with_clipboard(
        paths: &[PathBuf],
        mask: &str,
        replacement: Option<(&Regex, &str)>,
        clipboard: Option<&str>,
        field: impl FnMut(&Path, &str) -> Option<String>,
    ) -> io::Result<Self> {
        Self::new_with_masks(paths, mask, None, replacement, clipboard, field)
    }

    pub fn new_with_masks(
        paths: &[PathBuf],
        mask: &str,
        extension_mask: Option<&str>,
        replacement: Option<(&Regex, &str)>,
        clipboard: Option<&str>,
        field: impl FnMut(&Path, &str) -> Option<String>,
    ) -> io::Result<Self> {
        Self::build(
            paths,
            mask,
            extension_mask,
            PreviewContext {
                times: Times::now(),
                cancel: None,
            },
            clipboard,
            field,
            |stem, extension| {
                let name = join_name(stem, extension);
                match replacement {
                    Some((pattern, replacement)) => {
                        pattern.replace_all(&name, replacement).into_owned()
                    }
                    None => name,
                }
            },
        )
    }

    pub fn new_with_search(
        paths: &[PathBuf],
        mask: &str,
        extension_mask: Option<&str>,
        search: Option<&Search>,
        clipboard: Option<&str>,
        field: impl FnMut(&Path, &str) -> Option<String>,
    ) -> io::Result<Self> {
        Self::new_with_search_at(
            paths,
            mask,
            extension_mask,
            search,
            PreviewContext {
                times: Times::now(),
                cancel: None,
            },
            clipboard,
            field,
        )
    }

    pub fn new_with_search_at(
        paths: &[PathBuf],
        mask: &str,
        extension_mask: Option<&str>,
        search: Option<&Search>,
        context: PreviewContext<'_>,
        clipboard: Option<&str>,
        field: impl FnMut(&Path, &str) -> Option<String>,
    ) -> io::Result<Self> {
        Self::build(
            paths,
            mask,
            extension_mask,
            context,
            clipboard,
            field,
            |stem, extension| match search {
                None => join_name(stem, extension),
                Some(search) if search.include_extension || extension.is_none() => {
                    search.apply(&join_name(stem, extension))
                }
                Some(search) => join_name(search.apply(&stem), extension),
            },
        )
    }

    fn build(
        paths: &[PathBuf],
        mask: &str,
        extension_mask: Option<&str>,
        context: PreviewContext<'_>,
        clipboard: Option<&str>,
        mut field: impl FnMut(&Path, &str) -> Option<String>,
        transform: impl Fn(String, Option<String>) -> String,
    ) -> io::Result<Self> {
        let parts = mask_parts(mask)?;
        let suffix = extension_mask.map(mask_parts).transpose()?;
        let tc = suffix.is_some();
        let mut entries = Vec::new();
        let mut skipped_capture = 0;
        for (index, path) in paths.iter().enumerate() {
            if let Some(cancel) = context.cancel {
                files::check_cancel(cancel)?;
            }
            let stem = path
                .file_stem()
                .and_then(OsStr::to_str)
                .ok_or_else(|| io::Error::other("invalid name"))?;
            let extension = path.extension().and_then(OsStr::to_str).unwrap_or("");
            let file = path
                .file_name()
                .and_then(OsStr::to_str)
                .ok_or_else(|| io::Error::other("invalid name"))?;
            let mut time = TimeState::new(context.times);
            let mut ignore_dir_extension = false;
            let mut render = |parts: &[(&str, Option<&str>)]| -> io::Result<Option<String>> {
                let mut output = String::new();
                let mut case = Case::Original;
                let mut word_start = true;
                for (literal, placeholder) in parts {
                    case.push(&mut output, literal, &mut word_start);
                    let Some(placeholder) = *placeholder else {
                        continue;
                    };
                    let (stem, extension) = if ignore_dir_extension {
                        (file, "")
                    } else {
                        (stem, extension)
                    };
                    let value = match placeholder {
                        "U" => {
                            case = Case::Upper;
                            continue;
                        }
                        "L" => {
                            case = Case::Lower;
                            continue;
                        }
                        "F" if tc => {
                            case = Case::Title;
                            word_start = true;
                            continue;
                        }
                        "I" => {
                            if files::regular_metadata(path)?.is_dir() {
                                ignore_dir_extension = !ignore_dir_extension;
                            }
                            continue;
                        }
                        "n" => {
                            case = Case::Original;
                            continue;
                        }
                        "N" => stem.into(),
                        "c" | "c1" => paths.len().to_string(),
                        "S" => files::regular_metadata(path)?.len().to_string(),
                        "E" => extension.into(),
                        "A" | "F" => file.into(),
                        "B" | "P" | "G" => ancestor_name(path, placeholder == "G").into_owned(),
                        "[" | "]" => placeholder.into(),
                        "X" | "X+" => {
                            let text = clipboard.unwrap_or_default();
                            if placeholder == "X" && text.is_empty() {
                                return Err(io::Error::other(
                                    "Die Zwischenablage enthält keinen Text für [X].",
                                ));
                            }
                            if tc {
                                text.replace(['*', '?', ':', '|', '"', '<', '>', '/', '\\'], "_")
                            } else {
                                text.into()
                            }
                        }
                        selector if selector.starts_with('T') => {
                            time.select(selector)?;
                            continue;
                        }
                        "Y" | "y" | "M" | "D" | "h" | "H" | "i" | "i1" | "m" | "s" | "d" | "t"
                        | "YMD" | "hms" | "v" => {
                            let format = match placeholder {
                                "Y" => "%Y",
                                "y" => "%y",
                                "M" => "%m",
                                "D" => "%d",
                                "h" => "%H",
                                "H" => "%I",
                                "i" | "i1" => "%P",
                                "m" => "%M",
                                "s" => "%S",
                                "d" => "%Y-%m-%d",
                                "YMD" => "%Y%m%d",
                                "hms" => "%H%M%S",
                                "v" => "%3f",
                                _ => "%H.%M.%S",
                            };
                            let Some(timestamp) = time.value(path)? else {
                                return Ok(None);
                            };
                            let value = timestamp.format(format).to_string();
                            if placeholder == "i1" {
                                value[..1].to_owned()
                            } else {
                                value
                            }
                        }
                        key if key.starts_with('C') => counter(&key[1..], index, paths.len())?,
                        key if key.starts_with('B')
                            && key.len() == 2
                            && key.as_bytes()[1].is_ascii_digit() =>
                        {
                            path.ancestors()
                                .nth(usize::from(key.as_bytes()[1] - b'0') + 1)
                                .and_then(Path::file_name)
                                .map(OsStr::to_string_lossy)
                                .unwrap_or_default()
                                .into_owned()
                        }
                        key if key.starts_with(['N', 'E', 'A', 'P', 'G']) => {
                            let text = match &key[..1] {
                                "N" => stem.into(),
                                "E" => extension.into(),
                                "A" => file.into(),
                                "P" => ancestor_name(path, false),
                                _ => ancestor_name(path, true),
                            };
                            name_range(&text, &key[1..])?
                        }
                        key if key.starts_with(|ch: char| ch.is_ascii_digit() || ch == '-') => {
                            name_range(file, key)?
                        }
                        key if key.starts_with('=') => field(path, &key[1..]).ok_or_else(|| {
                            io::Error::other(format!(
                                "WDX-Feld [{key}] fehlt für {}",
                                path.display()
                            ))
                        })?,
                        _ => {
                            return Err(io::Error::other(format!(
                                "unknown placeholder [{placeholder}]"
                            )));
                        }
                    };
                    case.push(&mut output, &value, &mut word_start);
                }
                Ok(Some(output))
            };
            let Some(stem) = render(&parts)? else {
                skipped_capture += 1;
                continue;
            };
            let extension = if let Some(suffix) = &suffix {
                let Some(extension) = render(suffix)? else {
                    skipped_capture += 1;
                    continue;
                };
                Some(extension)
            } else {
                None
            };
            let output = transform(stem, extension);
            let target = path.with_file_name(files::leaf_name(OsStr::new(&output))?);
            if path.as_os_str() != target.as_os_str() {
                entries.push((path.clone(), target));
            }
        }
        let mut plan = Self::from_entries(entries)?;
        plan.skipped_capture = skipped_capture;
        Ok(plan)
    }

    pub fn from_names(paths: &[PathBuf], names: &[&str]) -> io::Result<Self> {
        if paths.len() != names.len() {
            return Err(io::Error::other(
                "Die Namensliste muss genau eine Zeile pro Eintrag enthalten.",
            ));
        }
        let mut entries = Vec::new();
        for (path, name) in paths.iter().zip(names) {
            let target = path.with_file_name(files::leaf_name(OsStr::new(name))?);
            if path.as_os_str() != target.as_os_str() {
                entries.push((path.clone(), target));
            }
        }
        Self::from_entries(entries)
    }

    fn from_entries(entries: Vec<(PathBuf, PathBuf)>) -> io::Result<Self> {
        let before = entries
            .iter()
            .map(|(source, _)| files::regular_metadata(source))
            .collect::<io::Result<_>>()?;
        let plan = Self {
            entries,
            before,
            skipped_capture: 0,
        };
        plan.validate()?;
        Ok(plan)
    }

    pub fn entries(&self) -> &[(PathBuf, PathBuf)] {
        &self.entries
    }

    pub fn skipped_capture(&self) -> usize {
        self.skipped_capture
    }

    pub fn inverse(&self) -> io::Result<Self> {
        Self::from_entries(
            self.entries
                .iter()
                .map(|(source, target)| (target.clone(), source.clone()))
                .collect(),
        )
    }

    fn validate(&self) -> io::Result<()> {
        let Some((first, _)) = self.entries.first() else {
            return Ok(());
        };
        let folder = first
            .parent()
            .ok_or_else(|| io::Error::other("invalid source"))?;
        let key = |path: &Path| path.to_string_lossy().to_lowercase();
        let sources: HashSet<_> = self.entries.iter().map(|(source, _)| key(source)).collect();
        let mut targets = HashSet::new();
        for ((source, target), metadata) in self.entries.iter().zip(&self.before) {
            if source.parent() != Some(folder) || target.parent() != Some(folder) {
                return Err(io::Error::other("batch rename requires a single directory"));
            }
            files::ensure_unchanged(source, metadata)?;
            if !targets.insert(key(target)) {
                return Err(io::Error::other(format!(
                    "multiple entries target {}",
                    target.display()
                )));
            }
            match fs::symlink_metadata(target) {
                Ok(_) if !sources.contains(&key(target)) => {
                    return Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        target.display().to_string(),
                    ));
                }
                Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
                _ => (),
            }
        }
        Ok(())
    }

    pub fn apply(&self, rename: impl Fn(&Path, &Path) -> io::Result<()>) -> io::Result<()> {
        self.validate()?;
        let Some((first, _)) = self.entries.first() else {
            return Ok(());
        };
        let staging = tempfile::Builder::new()
            .prefix(".arco-rename-")
            .tempdir_in(first.parent().unwrap())?;
        let stage = |index: usize| staging.path().join(index.to_string());
        let mut staged = 0;
        let mut published = 0;
        let operation = (|| {
            for (index, (source, _)) in self.entries.iter().enumerate() {
                files::ensure_unchanged(source, &self.before[index])?;
                rename(source, &stage(index))?;
                staged += 1;
            }
            for (index, (_, target)) in self.entries.iter().enumerate() {
                rename(&stage(index), target)?;
                published += 1;
            }
            Ok(())
        })();
        if let Err(error) = operation {
            let mut recovery = Vec::new();
            for index in (0..published).rev() {
                if let Err(error) = rename(&self.entries[index].1, &stage(index)) {
                    recovery.push(error.to_string());
                }
            }
            for index in (0..staged).rev() {
                if let Err(error) = rename(&stage(index), &self.entries[index].0) {
                    recovery.push(error.to_string());
                }
            }
            if !recovery.is_empty() {
                let path = staging.keep();
                return Err(io::Error::other(format!(
                    "{error}. Rollback incomplete in {}: {}",
                    path.display(),
                    recovery.join("; ")
                )));
            }
            return Err(error);
        }
        staging.close()
    }
}

fn ancestor_name(path: &Path, grandparent: bool) -> std::borrow::Cow<'_, str> {
    let parent = path.parent();
    let ancestor = if grandparent {
        parent.and_then(Path::parent)
    } else {
        parent
    };
    ancestor
        .and_then(Path::file_name)
        .unwrap_or_default()
        .to_string_lossy()
}

pub fn name_range(text: &str, range: &str) -> io::Result<String> {
    fn position(input: &str) -> Option<(i64, &str)> {
        let digits = input.strip_prefix('-').unwrap_or(input);
        let count = digits.bytes().take_while(u8::is_ascii_digit).count();
        let length = count + input.len() - digits.len();
        let number = input[..length].parse().ok()?;
        (number != 0).then_some((number, &input[length..]))
    }
    let invalid = || io::Error::other(format!("Ungültiger Zeichenbereich [{range}]."));
    let characters: Vec<_> = text.chars().collect();
    let length = characters.len() as i128;
    let offset = |position: i64| {
        let position = i128::from(position);
        if position < 0 {
            length + position
        } else {
            position - 1
        }
    };
    let (range, pad) =
        if range.starts_with('0') && range.as_bytes().get(1).is_some_and(u8::is_ascii_digit) {
            (&range[1..], Some('0'))
        } else if range.starts_with(' ') {
            (range.trim_start_matches(' '), Some(' '))
        } else {
            (range, None)
        };
    let (first, rest) = position(range).ok_or_else(invalid)?;
    let start = offset(first);
    let end = match rest {
        "" => start + 1,
        "-" => length,
        value if value.starts_with(',') => {
            let count = value[1..].parse::<u64>().map_err(|_| invalid())?;
            start + i128::from(count)
        }
        value if value.starts_with('-') => {
            let (end, tail) = position(&value[1..]).ok_or_else(invalid)?;
            if !tail.is_empty() {
                return Err(invalid());
            }
            // TC counts both ends from the right in ranges such as [N-8-5].
            offset(if first < 0 && end > 0 { -end } else { end }) + 1
        }
        _ => return Err(invalid()),
    };
    let requested = (end - start).max(0);
    let start = start.clamp(0, length) as usize;
    let end = end.clamp(0, length) as usize;
    let range = characters.get(start..end).unwrap_or_default();
    let mut output = String::new();
    if let Some(pad) = pad {
        let width = usize::try_from(requested)
            .ok()
            .filter(|width| *width <= 4096)
            .ok_or_else(invalid)?;
        output.extend(std::iter::repeat_n(pad, width.saturating_sub(range.len())));
    }
    output.extend(range);
    Ok(output)
}

#[derive(Clone, Copy)]
enum Case {
    Original,
    Upper,
    Lower,
    Title,
}

impl Case {
    fn push(self, output: &mut String, value: &str, word_start: &mut bool) {
        match self {
            Self::Original => output.push_str(value),
            Self::Upper => output.extend(value.chars().flat_map(char::to_uppercase)),
            Self::Lower => output.extend(value.chars().flat_map(char::to_lowercase)),
            Self::Title => {
                for character in value.chars() {
                    if *word_start {
                        output.extend(character.to_uppercase());
                    } else {
                        output.extend(character.to_lowercase());
                    }
                    *word_start = !character.is_alphanumeric();
                }
            }
        }
    }
}

fn counter(value: &str, index: usize, count: usize) -> io::Result<String> {
    let (value, width) = value
        .split_once(':')
        .map_or((value, None), |(value, width)| (value, Some(width)));
    let invalid = || io::Error::other("Ungültiger Zähler, zum Beispiel [C10+5:3] oder [Caa].");
    let step_at = value
        .char_indices()
        .find(|(index, ch)| *ch == '+' || (*index > 0 && *ch == '-'))
        .map(|(index, _)| index);
    let (start, step) = step_at.map_or((value, "1"), |position| value.split_at(position));
    let (step, group) = step.split_once('/').unwrap_or((step, "1"));
    let step = step.parse::<i128>().map_err(|_| invalid())?;
    let group = group
        .parse::<usize>()
        .ok()
        .filter(|group| *group > 0)
        .ok_or_else(invalid)?;
    let letters = (!start.is_empty() && start.bytes().all(|byte| byte.is_ascii_alphabetic()))
        .then_some(start.as_bytes());
    let start = match letters {
        Some(letters) if letters.len() <= 24 => letters.iter().fold(0i128, |number, digit| {
            number * 26 + i128::from(digit.to_ascii_lowercase() - b'a')
        }),
        Some(_) => return Err(invalid()),
        None if start.is_empty() => 1,
        None => start.parse::<i128>().map_err(|_| invalid())?,
    };
    let at = |index: usize| {
        ((index / group) as i128)
            .checked_mul(step)
            .and_then(|offset| start.checked_add(offset))
            .ok_or_else(invalid)
    };
    let width = match width {
        Some("a") => {
            let end = at(count.saturating_sub(1))?;
            match letters {
                Some(letters) => letter_count(start.max(end)).max(letters.len()),
                None => start.to_string().len().max(end.to_string().len()),
            }
        }
        Some(width) => width
            .parse::<usize>()
            .ok()
            .filter(|width| (1..=24).contains(width))
            .ok_or_else(invalid)?,
        None => letters.map_or(1, <[u8]>::len),
    };
    let offset = at(index)?;
    let Some(letters) = letters else {
        return Ok(format!("{offset:0width$}"));
    };
    if offset < 0 {
        return Err(invalid());
    }
    let length = letter_count(offset).max(width);
    let mut output = vec![0; length];
    let mut value = offset;
    for (index, digit) in output.iter_mut().rev().enumerate() {
        let format = letters[letters.len().saturating_sub(index + 1)];
        let first = if format.is_ascii_uppercase() {
            b'A'
        } else {
            b'a'
        };
        *digit = first + (value % 26) as u8;
        value /= 26;
    }
    String::from_utf8(output).map_err(io::Error::other)
}

fn letter_count(mut number: i128) -> usize {
    let mut digits = 1;
    while number >= 26 {
        number /= 26;
        digits += 1;
    }
    digits
}

pub fn uses_capture_time(mask: &str) -> io::Result<bool> {
    Ok(mask_parts(mask)?
        .iter()
        .any(|(_, placeholder)| *placeholder == Some("T4")))
}

pub fn uses_clipboard(mask: &str) -> io::Result<bool> {
    Ok(mask_parts(mask)?
        .iter()
        .any(|(_, token)| matches!(token, Some("X" | "X+"))))
}

fn mask_parts(mut mask: &str) -> io::Result<Vec<(&str, Option<&str>)>> {
    let mut result = Vec::new();
    while let Some((literal, rest)) = mask.split_once('[') {
        if let Some(next) = rest.strip_prefix("]]") {
            result.push((literal, Some("]")));
            mask = next;
            continue;
        }
        let (placeholder, next) = rest
            .split_once(']')
            .ok_or_else(|| io::Error::other("unterminated placeholder"))?;
        result.push((literal, Some(placeholder)));
        mask = next;
    }
    result.push((mask, None));
    Ok(result)
}

pub fn wdx_fields(mask: &str) -> io::Result<Vec<&str>> {
    let mut seen = HashSet::new();
    Ok(mask_parts(mask)?
        .into_iter()
        .filter_map(|(_, placeholder)| placeholder?.strip_prefix('='))
        .filter(|field| seen.insert(*field))
        .collect())
}

#[cfg(test)]
mod tests {
    #[test]
    fn tc_name_ranges_work_with_unicode_and_reverse_positions() {
        use super::{mask_parts, name_range};
        let name = "aÜ𐐀cdé";
        assert_eq!(name_range(name, "2-4").unwrap(), "Ü𐐀c");
        assert_eq!(name_range(name, "-4-2").unwrap(), "𐐀cd");
        assert_eq!(name_range(name, "2--2").unwrap(), "Ü𐐀cd");
        assert_eq!(name_range(name, "-3,2").unwrap(), "cd");
        assert_eq!(name_range(name, "-2-").unwrap(), "dé");
        assert!(name_range(name, "0-5").is_err());
        assert_eq!(
            mask_parts("[[][N][]]").unwrap(),
            vec![
                ("", Some("[")),
                ("", Some("N")),
                ("", Some("]")),
                ("", None)
            ]
        );
    }
    #[test]
    fn tc_counter_carries_letters_and_groups_numeric_sequences() {
        use super::counter;
        assert_eq!(counter("a", 26, 30).unwrap(), "ba");
        assert_eq!(counter("aAz", 1, 2).unwrap(), "aBa");
        assert_eq!(counter("aaZ", 1, 2).unwrap(), "abA");
        assert_eq!(counter("aa", 1, 2).unwrap(), "ab");
        assert_eq!(counter("z:a", 0, 2).unwrap(), "az");
        assert_eq!(counter("+1/100:a", 99, 1_000).unwrap(), "01");
        assert_eq!(counter("+1/100:a", 100, 1_000).unwrap(), "02");
        assert!(counter("+1/0", 0, 10).is_err());
    }

    #[test]
    fn counter_case_and_optional_clipboard_work_without_name_collisions() {
        use super::*;
        let fixture = tempfile::tempdir().unwrap();
        let paths = [
            fixture.path().join("Foto.txt"),
            fixture.path().join("Dokument.txt"),
        ];
        for path in &paths {
            fs::write(path, b"test").unwrap();
        }
        let plan = Plan::new_with_clipboard(
            &paths,
            "[C10+5:3]-[X+][U][N][n].[L][E]",
            None,
            None,
            |_, _| None,
        )
        .unwrap();
        assert!(plan.entries()[0].1.ends_with("010-FOTO.txt"));
        assert!(plan.entries()[1].1.ends_with("015-DOKUMENT.txt"));
        assert!(Plan::new(&paths, "[X]-[F]", None).is_err());
        assert!(Plan::from_names(&paths, &["gleich.txt", "gleich.txt"]).is_err());
        assert!(Plan::from_names(&paths, &["../Ziel.txt", "Ziel2.txt"]).is_err());
    }

    use super::*;

    #[test]
    fn wildcard_replacements_keep_captures_and_literal_rules_stay_literal() {
        let options = SearchOptions {
            find: "entwurf-??-*.txt|Alternative-?".into(),
            replace: "Foto-??-*.md|Zahl-?".into(),
            wildcards: true,
            ..Default::default()
        };
        let search = options.compile().unwrap().unwrap();
        assert_eq!(
            search.apply("Entwurf-界𐐀-Übersicht.txt"),
            "Foto-界𐐀-Übersicht.md"
        );
        assert_eq!(search.apply("Alternative-7"), "Zahl-7");
        let literal = SearchOptions {
            wildcards: false,
            ..options
        }
        .compile()
        .unwrap()
        .unwrap();
        assert_eq!(literal.apply("Alternative-7"), "Alternative-7");
        assert!(
            SearchOptions {
                find: "entwurf-?".into(),
                replace: "Foto-*".into(),
                wildcards: true,
                ..Default::default()
            }
            .compile()
            .is_err()
        );
    }

    #[test]
    fn date_selectors_switch_between_dialog_times_and_offset_the_calendar() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("foto.txt");
        fs::write(&source, b"").unwrap();
        let date = |text| chrono::NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M:%S").unwrap();
        let times = Times {
            opened: date("2024-01-31 21:10:11"),
            edited: date("2025-12-31 23:59:50"),
        };
        let plan = Plan::new_with_search_at(
            &[source],
            "[T2+1M][YMD]-[T3+20s][YMD][hms]-[T2-1Y][YMD]",
            Some("[E]"),
            None,
            PreviewContext {
                times,
                cancel: None,
            },
            None,
            |_, _| None,
        )
        .unwrap();
        assert!(
            plan.entries()[0]
                .1
                .ends_with("20240229-20260101000010-20230131.txt")
        );
    }

    #[test]
    fn capture_time_reads_exif_and_leaves_files_without_capture_dates_untouched() {
        let directory = tempfile::tempdir().unwrap();
        let photo = directory.path().join("foto.jpg");
        let other = directory.path().join("notiz.txt");
        let mut jpeg = Vec::from(b"\xff\xd8\xff\xe1\x00\x48Exif\0\0".as_slice());
        jpeg.extend_from_slice(b"II\x2a\0\x08\0\0\0");
        jpeg.extend_from_slice(b"\x01\0\x69\x87\x04\0\x01\0\0\0\x1a\0\0\0\0\0\0\0");
        jpeg.extend_from_slice(b"\x01\0\x03\x90\x02\0\x14\0\0\0\x2c\0\0\0\0\0\0\0");
        jpeg.extend_from_slice(b"2022:07:08 13:14:15\0\xff\xd9");
        fs::write(&photo, jpeg).unwrap();
        fs::write(&other, b"ohne Aufnahmedatum").unwrap();
        let plan = Plan::new(&[photo, other], "[T4][YMD]-[hms].[E]", None).unwrap();
        assert_eq!(plan.skipped_capture(), 1);
        assert_eq!(plan.entries().len(), 1);
        assert!(plan.entries()[0].1.ends_with("20220708-131415.jpg"));
    }

    #[test]
    fn capture_time_reads_the_mp4_movie_header_as_utc() {
        let directory = tempfile::tempdir().unwrap();
        let movie = directory.path().join("movie.mp4");
        let unix_seconds = 1_700_000_000;
        let mut bytes = Vec::from(b"\0\0\0\x20moov\0\0\0\x18mvhd\x01\0\0\0".as_slice());
        bytes.extend_from_slice(&(unix_seconds as u64 + 2_082_844_800).to_be_bytes());
        bytes.extend_from_slice(&[0; 4]);
        fs::write(&movie, bytes).unwrap();
        let expected = chrono::DateTime::from_timestamp(unix_seconds, 0)
            .unwrap()
            .with_timezone(&chrono::Local)
            .format("%Y%m%d-%H%M%S.mp4")
            .to_string();
        let plan = Plan::new(&[movie], "[T4][YMD]-[hms].[E]", None).unwrap();
        assert_eq!(plan.skipped_capture(), 0);
        assert!(plan.entries()[0].1.ends_with(expected));
    }

    #[test]
    fn literal_replacements_are_simultaneous_and_keep_the_extension() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("FÖÖ-ö-Ö.txt");
        fs::write(&source, b"").unwrap();
        let options = SearchOptions {
            find: "ö|x|txt".into(),
            replace: "x|$1|md".into(),
            ..Default::default()
        };
        let search = options.compile().unwrap().unwrap();
        let plan = Plan::new_with_search(
            std::slice::from_ref(&source),
            "[N]",
            Some("[E]"),
            Some(&search),
            None,
            |_, _| None,
        )
        .unwrap();
        assert!(plan.entries()[0].1.ends_with("Fxx-x-x.txt"));
        let first = SearchOptions {
            match_case: true,
            first_only: true,
            ..options
        }
        .compile()
        .unwrap()
        .unwrap();
        let plan =
            Plan::new_with_search(&[source], "[N]", Some("[E]"), Some(&first), None, |_, _| {
                None
            })
            .unwrap();
        assert!(plan.entries()[0].1.ends_with("FÖÖ-x-Ö.txt"));
        assert_eq!(search.apply("x"), "$1");
    }

    #[test]
    fn regex_substitution_can_use_captures_and_the_extension() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("foto-2025.TXT");
        fs::write(&source, b"").unwrap();
        let options = SearchOptions {
            find: r"(\d+)\.txt$".into(),
            replace: "${1}.md".into(),
            regex: true,
            include_extension: true,
            substitute: true,
            ..Default::default()
        };
        let search = options.compile().unwrap().unwrap();
        let plan = Plan::new_with_search(
            std::slice::from_ref(&source),
            "[N]",
            Some("[E]"),
            Some(&search),
            None,
            |_, _| None,
        )
        .unwrap();
        assert!(plan.entries()[0].1.ends_with("2025.md"));
        let legacy = SearchOptions::from_legacy("TXT=>txt")
            .unwrap()
            .compile()
            .unwrap()
            .unwrap();
        let plan = Plan::new_with_search(&[source], "[F]", None, Some(&legacy), None, |_, _| None)
            .unwrap();
        assert!(plan.entries()[0].1.ends_with("foto-2025.txt"));
        assert!(
            SearchOptions {
                find: "a||b".into(),
                ..Default::default()
            }
            .compile()
            .is_err()
        );
    }

    #[test]
    fn separate_masks_preserve_directory_names_and_use_independent_case() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("mixed CaSE.TxT");
        let folder = directory.path().join("old.folder");
        fs::write(&source, b"test").unwrap();
        fs::create_dir(&folder).unwrap();
        let plan = Plan::new_with_masks(
            &[source.clone(), folder],
            "[I][F][N]-[C]",
            Some("[U][E]"),
            None,
            None,
            |_, _| None,
        )
        .unwrap();
        assert!(plan.entries()[0].1.ends_with("Mixed Case-1.TXT"));
        assert!(plan.entries()[1].1.ends_with("Old.Folder-2"));
        let plan = Plan::new_with_masks(
            &[source],
            "[X]-[S]",
            Some(""),
            None,
            Some("T:1/2"),
            |_, _| None,
        )
        .unwrap();
        assert!(plan.entries()[0].1.ends_with("T_1_2-4"));
        assert_eq!(name_range("abc", "02-9").unwrap(), "000000bc");
    }

    #[test]
    fn wdx_values_are_literal_and_missing_values_stop_the_preview() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("one.txt");
        fs::write(&path, b"content").unwrap();
        let mask = "[=test.Title]-[C].[E]";
        assert_eq!(wdx_fields(mask).unwrap(), ["test.Title"]);
        let plan = Plan::new_with_fields(std::slice::from_ref(&path), mask, None, |_, _| {
            Some("Metadaten [C]".into())
        })
        .unwrap();
        assert_eq!(
            plan.entries()[0].1,
            directory.path().join("Metadaten [C]-1.txt")
        );
        assert!(Plan::new_with_fields(&[path], mask, None, |_, _| None).is_err());
    }

    #[test]
    fn rename_cycle_can_be_reversed() {
        let root = tempfile::tempdir().unwrap();
        let a = root.path().join("a");
        let b = root.path().join("b");
        fs::write(&a, b"first").unwrap();
        fs::write(&b, b"second").unwrap();
        let plan =
            Plan::from_entries(vec![(a.clone(), b.clone()), (b.clone(), a.clone())]).unwrap();
        plan.apply(|from, to| fs::rename(from, to)).unwrap();
        assert_eq!(fs::read(&a).unwrap(), b"second");
        plan.inverse()
            .unwrap()
            .apply(|from, to| fs::rename(from, to))
            .unwrap();
        assert_eq!(fs::read(&a).unwrap(), b"first");
    }

    #[test]
    fn changed_source_is_not_renamed_after_preview() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("first.txt");
        let target = root.path().join("second.txt");
        fs::write(&source, b"before").unwrap();
        let plan = Plan::single(&source, OsStr::new("second.txt")).unwrap();
        fs::write(&source, b"changed since preview").unwrap();
        assert!(plan.apply(|from, to| fs::rename(from, to)).is_err());
        assert!(!target.exists());
        assert_eq!(fs::read(&source).unwrap(), b"changed since preview");
    }
}
