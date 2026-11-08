use crate::files;
use chrono::{DateTime, Local};
use regex::Regex;
use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct Plan {
    entries: Vec<(PathBuf, PathBuf)>,
    before: Vec<fs::Metadata>,
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
        mut field: impl FnMut(&Path, &str) -> Option<String>,
    ) -> io::Result<Self> {
        let parts = mask_parts(mask)?;
        let mut entries = Vec::new();
        for (index, path) in paths.iter().enumerate() {
            let stem = path
                .file_stem()
                .and_then(OsStr::to_str)
                .ok_or_else(|| io::Error::other("invalid name"))?;
            let extension = path.extension().and_then(OsStr::to_str).unwrap_or("");
            let file = path
                .file_name()
                .and_then(OsStr::to_str)
                .ok_or_else(|| io::Error::other("invalid name"))?;
            let mut output = String::new();
            let mut case = Case::Original;
            let mut timestamp = None;
            for (literal, placeholder) in &parts {
                case.push(&mut output, literal);
                let Some(placeholder) = *placeholder else {
                    continue;
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
                    "n" => {
                        case = Case::Original;
                        continue;
                    }
                    "N" => stem.into(),
                    "c" => paths.len().to_string(),
                    "E" => extension.into(),
                    "F" => file.into(),
                    "P" | "G" => ancestor_name(path, placeholder == "G").into_owned(),
                    "[" | "]" => placeholder.into(),
                    "X" => clipboard
                        .filter(|text| !text.is_empty())
                        .ok_or_else(|| {
                            io::Error::other("Die Zwischenablage enthält keinen Text für [X].")
                        })?
                        .into(),
                    "X+" => clipboard.unwrap_or_default().into(),
                    "Y" | "M" | "D" | "h" | "m" | "s" | "d" | "t" => {
                        let format = match placeholder {
                            "Y" => "%Y",
                            "M" => "%m",
                            "D" => "%d",
                            "h" => "%H",
                            "m" => "%M",
                            "s" => "%S",
                            "d" => "%Y-%m-%d",
                            _ => "%H.%M.%S",
                        };
                        if timestamp.is_none() {
                            timestamp = Some(DateTime::<Local>::from(
                                files::regular_metadata(path)?.modified()?,
                            ));
                        }
                        timestamp.as_ref().unwrap().format(format).to_string()
                    }
                    key if key.starts_with('C') => counter(&key[1..], index, paths.len())?,
                    key if key.starts_with(['N', 'E', 'P', 'G']) => {
                        let text = match &key[..1] {
                            "N" => stem.into(),
                            "E" => extension.into(),
                            "P" => ancestor_name(path, false),
                            _ => ancestor_name(path, true),
                        };
                        name_range(&text, &key[1..])?
                    }
                    key if key.starts_with(|ch: char| ch.is_ascii_digit() || ch == '-') => {
                        name_range(file, key)?
                    }
                    key if key.starts_with('=') => field(path, &key[1..]).ok_or_else(|| {
                        io::Error::other(format!("WDX-Feld [{key}] fehlt für {}", path.display()))
                    })?,
                    _ => {
                        return Err(io::Error::other(format!(
                            "unknown placeholder [{placeholder}]"
                        )));
                    }
                };
                case.push(&mut output, &value);
            }
            if let Some((pattern, replacement)) = replacement {
                output = pattern.replace_all(&output, replacement).into_owned();
            }
            let target = path.with_file_name(files::leaf_name(OsStr::new(&output))?);
            if path.as_os_str() != target.as_os_str() {
                entries.push((path.clone(), target));
            }
        }
        Self::from_entries(entries)
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
        let plan = Self { entries, before };
        plan.validate()?;
        Ok(plan)
    }

    pub fn entries(&self) -> &[(PathBuf, PathBuf)] {
        &self.entries
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

fn name_range(text: &str, range: &str) -> io::Result<String> {
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
    let start = start.clamp(0, length) as usize;
    let end = end.clamp(0, length) as usize;
    Ok(characters
        .get(start..end)
        .unwrap_or_default()
        .iter()
        .collect())
}

#[derive(Clone, Copy)]
enum Case {
    Original,
    Upper,
    Lower,
}

impl Case {
    fn push(self, output: &mut String, value: &str) {
        match self {
            Self::Original => output.push_str(value),
            Self::Upper => output.extend(value.chars().flat_map(char::to_uppercase)),
            Self::Lower => output.extend(value.chars().flat_map(char::to_lowercase)),
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
