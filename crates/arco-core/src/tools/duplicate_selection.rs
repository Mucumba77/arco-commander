use crate::advanced_search::{Options, Query};
use crate::files::{self, Entry};
use crate::sync::FileMasks;
use globset::{GlobBuilder, GlobMatcher};
use regex::RegexBuilder;
use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DuplicateSelection {
    pub names: String,
    pub exclude: String,
    pub folders: Vec<PathBuf>,
    pub include_subfolders: bool,
    /// 0: all, 1: oldest, 2: newest, 3: older than newest, 4: newer than oldest.
    pub date: u8,
    /// 0: all, 1: smallest, 2: largest, 3: smaller than largest, 4: larger than smallest.
    pub size: u8,
    pub keep_one: bool,
    /// Ordered tie-breakers: 0 first, 1 last, 2 oldest, 3 newest, 4 smallest, 5 largest.
    pub keep: [u8; 3],
}

enum NameFilter {
    All,
    Masks(FileMasks),
    Regex(regex::Regex),
    Profile(Box<Query>, Option<String>),
}

impl NameFilter {
    fn parse(value: &str, profiles: &HashMap<String, Options>) -> io::Result<Self> {
        let value = value.trim();
        if value.is_empty() {
            return Ok(Self::All);
        }
        if let Some(regex) = value.strip_prefix('<') {
            return RegexBuilder::new(regex)
                .case_insensitive(true)
                .build()
                .map(Self::Regex)
                .map_err(io::Error::other);
        }
        if let Some(name) = value.strip_prefix('>') {
            let options = profiles
                .get(name.trim())
                .ok_or_else(|| io::Error::other("Der gespeicherte Suchfilter fehlt."))?;
            if options.everything().is_some() {
                return Err(io::Error::other(
                    "Dieser Suchfilter benötigt eine vorgelagerte Index-/WDX-Abfrage.",
                ));
            }
            return options.compile().map(|query| {
                Self::Profile(
                    Box::new(query),
                    (!options.wdx_profile.is_empty()).then(|| name.trim().to_owned()),
                )
            });
        }
        FileMasks::from_tc(value).map(Self::Masks)
    }
    fn matches(
        &self,
        entry: &Entry,
        cancel: &AtomicBool,
        profile: &mut impl FnMut(&str, &Path) -> io::Result<bool>,
    ) -> io::Result<bool> {
        match self {
            Self::All => Ok(true),
            Self::Masks(masks) => Ok(masks.includes(&entry.path)),
            Self::Regex(regex) => Ok(regex.is_match(&entry.name.to_string_lossy())),
            Self::Profile(query, name) => {
                if !query.matches(entry, cancel)? {
                    return Ok(false);
                }
                name.as_ref()
                    .map_or(Ok(true), |name| profile(name, &entry.path))
            }
        }
    }
}

fn normalized(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "/")
        .trim_end_matches('/')
        .to_lowercase()
}

pub fn duplicate_folder_matches(
    path: &Path,
    pattern: &str,
    direct_parent: bool,
) -> io::Result<bool> {
    let pattern = pattern.trim().replace('\\', "/");
    if pattern.is_empty() {
        return Ok(true);
    }
    let text = if direct_parent {
        path.file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned()
    } else {
        path.to_string_lossy().replace('\\', "/")
    };
    if !pattern.contains(['*', '?', '[', '{']) {
        return Ok(text.to_lowercase().contains(&pattern.to_lowercase()));
    }
    let matcher: GlobMatcher = GlobBuilder::new(&pattern)
        .case_insensitive(true)
        .literal_separator(false)
        .backslash_escape(false)
        .build()
        .map_err(io::Error::other)?
        .compile_matcher();
    Ok(matcher.is_match(text))
}

pub fn select_duplicates(
    entries: &[Entry],
    groups: &[usize],
    selection: &DuplicateSelection,
    profiles: &HashMap<String, Options>,
    cancel: &AtomicBool,
) -> io::Result<HashSet<PathBuf>> {
    select_duplicates_with_profiles(entries, groups, selection, profiles, cancel, |_, _| {
        Err(io::Error::other(
            "Dieser Suchfilter benötigt eine WDX-Abfrage.",
        ))
    })
}

pub fn select_duplicates_with_profiles(
    entries: &[Entry],
    groups: &[usize],
    selection: &DuplicateSelection,
    profiles: &HashMap<String, Options>,
    cancel: &AtomicBool,
    mut profile: impl FnMut(&str, &Path) -> io::Result<bool>,
) -> io::Result<HashSet<PathBuf>> {
    if selection.date > 4 || selection.size > 4 || selection.keep.iter().any(|choice| *choice > 5) {
        return Err(io::Error::other("Ungültige Duplikat-Auswahlregel."));
    }
    if groups
        .iter()
        .try_fold(0usize, |total, count| total.checked_add(*count))
        != Some(entries.len())
    {
        return Err(io::Error::other(
            "Die Duplikatgruppen stimmen nicht mehr mit der Ergebnisliste überein.",
        ));
    }
    let names = NameFilter::parse(&selection.names, profiles)?;
    let exclude = (!selection.exclude.trim().is_empty())
        .then(|| NameFilter::parse(&selection.exclude, profiles))
        .transpose()?;
    let folders = selection
        .folders
        .iter()
        .map(|path| normalized(path))
        .collect::<HashSet<_>>();
    let folder_matches = |entry: &Entry| {
        entry.path.parent().is_some_and(|parent| {
            if !selection.include_subfolders {
                return folders.contains(&normalized(parent));
            }
            parent
                .ancestors()
                .any(|parent| folders.contains(&normalized(parent)))
        })
    };
    let mut result = HashSet::new();
    let mut begin = 0;
    for &count in groups {
        let group = &entries[begin..begin + count];
        begin += count;
        let dates = group.iter().filter_map(|entry| entry.modified);
        let date = if matches!(selection.date, 1 | 4) {
            dates.min()
        } else {
            dates.max()
        };
        let sizes = group.iter().filter_map(|entry| entry.size);
        let size = if matches!(selection.size, 1 | 4) {
            sizes.min()
        } else {
            sizes.max()
        };
        let compare = |order: std::cmp::Ordering, choice: u8| match choice {
            1 | 2 => order.is_eq(),
            3 => order.is_lt(),
            4 => order.is_gt(),
            _ => true,
        };
        for entry in group {
            files::check_cancel(cancel)?;
            if !folder_matches(entry) || !names.matches(entry, cancel, &mut profile)? {
                continue;
            }
            if let Some(filter) = &exclude
                && filter.matches(entry, cancel, &mut profile)?
            {
                continue;
            }
            if selection.date != 0
                && !entry
                    .modified
                    .zip(date)
                    .is_some_and(|(value, edge)| compare(value.cmp(&edge), selection.date))
            {
                continue;
            }
            if selection.size != 0
                && !entry
                    .size
                    .zip(size)
                    .is_some_and(|(value, edge)| compare(value.cmp(&edge), selection.size))
            {
                continue;
            }
            result.insert(entry.path.clone());
        }
        if selection.keep_one
            && !group.is_empty()
            && group.iter().all(|entry| result.contains(&entry.path))
        {
            let mut candidates = group.iter().enumerate().collect::<Vec<_>>();
            for priority in selection.keep {
                #[derive(Clone, Copy, Eq, PartialEq, Ord, PartialOrd)]
                enum Rank {
                    Position(usize),
                    Time(std::time::SystemTime),
                    Size(u64),
                }
                let rank = |(index, entry): &(usize, &Entry)| -> Option<Rank> {
                    match priority {
                        0 | 1 => Some(Rank::Position(*index)),
                        2 | 3 => entry.modified.map(Rank::Time),
                        4 | 5 => entry.size.map(Rank::Size),
                        _ => None,
                    }
                };
                let mut values = candidates.iter().filter_map(rank);
                let first = values.next();
                if let Some(best) = first.map(|first| {
                    values.fold(first, |left, right| {
                        if matches!(priority, 1 | 3 | 5) {
                            left.max(right)
                        } else {
                            left.min(right)
                        }
                    })
                }) {
                    candidates.retain(|candidate| rank(candidate) == Some(best));
                }
                if candidates.len() <= 1 {
                    break;
                }
            }
            if let Some((_, keep)) = candidates.first() {
                result.remove(&keep.path);
            }
        }
    }
    files::check_cancel(cancel)?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::files::Kind;
    use std::time::{Duration, UNIX_EPOCH};
    fn entry(path: &str, bytes: u64, seconds: u64) -> Entry {
        Entry::from_remote(
            PathBuf::from(path),
            Kind::File,
            Some(bytes),
            Some(UNIX_EPOCH + Duration::from_secs(seconds)),
        )
    }
    #[test]
    fn masks_folders_dates_and_keep_priorities_preserve_each_group() {
        let entries = [
            entry("backup/a.txt", 10, 1),
            entry("backup/sub/a.txt", 10, 2),
            entry("original/a.txt", 10, 3),
            entry("backup/b.tmp", 20, 4),
            entry("backup/sub/b.txt", 20, 4),
        ];
        let mut options = DuplicateSelection {
            names: "* | *.tmp".into(),
            folders: vec!["backup".into()],
            include_subfolders: true,
            keep_one: true,
            keep: [3, 4, 0],
            ..Default::default()
        };
        let select = |options: &DuplicateSelection| {
            select_duplicates(
                &entries,
                &[3, 2],
                options,
                &HashMap::new(),
                &AtomicBool::new(false),
            )
            .unwrap()
        };
        let marked = select(&options);
        assert!(
            marked.contains(Path::new("backup/a.txt"))
                && marked.contains(Path::new("backup/sub/a.txt"))
        );
        assert!(
            marked.contains(Path::new("backup/sub/b.txt")),
            "an already excluded peer must count as the unselected copy"
        );
        options.names.clear();
        options.folders.push("original".into());
        let marked = select(&options);
        assert!(!marked.contains(Path::new("original/a.txt")));
        assert_eq!(marked.len(), 3);
        options.keep_one = false;
        options.date = 3;
        let marked = select(&options);
        assert_eq!(marked.len(), 2);
        options.include_subfolders = false;
        assert_eq!(select(&options).len(), 1);
    }
    #[test]
    fn saved_filters_regex_and_invalid_groups_are_checked_before_selection() {
        let entries = [entry("root/a.txt", 1, 1), entry("root/b.bin", 9, 1)];
        let mut profiles = HashMap::new();
        profiles.insert(
            "Small".into(),
            Options {
                maximum_bytes: "2".into(),
                ..Default::default()
            },
        );
        let options = DuplicateSelection {
            names: ">Small".into(),
            exclude: "<^b".into(),
            folders: vec!["root".into()],
            ..Default::default()
        };
        assert_eq!(
            select_duplicates(&entries, &[2], &options, &profiles, &AtomicBool::new(false))
                .unwrap()
                .len(),
            1
        );
        assert!(
            select_duplicates(&entries, &[3], &options, &profiles, &AtomicBool::new(false))
                .is_err()
        );
        assert!(
            select_duplicates(&entries, &[2], &options, &profiles, &AtomicBool::new(true)).is_err()
        );
        assert!(duplicate_folder_matches(Path::new("backup/archive"), "backup", false).unwrap());
        assert!(!duplicate_folder_matches(Path::new("backup/archive"), "backup", true).unwrap());
    }
}
