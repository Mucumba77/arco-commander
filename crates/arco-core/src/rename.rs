use crate::files;
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
            for (literal, placeholder) in &parts {
                output.push_str(literal);
                let Some(placeholder) = *placeholder else {
                    continue;
                };
                match placeholder {
                    "N" => output.push_str(stem),
                    "E" => output.push_str(extension),
                    "F" => output.push_str(file),
                    "C" => output.push_str(&(index + 1).to_string()),
                    key if key.starts_with('=') => {
                        let value = field(path, &key[1..]).ok_or_else(|| {
                            io::Error::other(format!(
                                "WDX-Feld [{key}] fehlt für {}",
                                path.display()
                            ))
                        })?;
                        output.push_str(&value);
                    }
                    _ => {
                        return Err(io::Error::other(format!(
                            "unknown placeholder [{placeholder}]"
                        )));
                    }
                }
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

fn mask_parts(mut mask: &str) -> io::Result<Vec<(&str, Option<&str>)>> {
    let mut result = Vec::new();
    while let Some((literal, rest)) = mask.split_once('[') {
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
