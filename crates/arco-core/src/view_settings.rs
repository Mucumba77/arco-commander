use crate::files::{Entry, Kind, SortOrder};
use crate::plugin::WdxColumn;
use crate::profile::TabView;
use crate::sync::FileMasks;
use globset::{GlobBuilder, GlobMatcher};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, io, path::Path};

pub const MAX_COLUMNS: usize = 29;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Field {
    Extension,
    Size,
    Modified,
    Attributes,
    Comment,
    Wdx(WdxColumn),
}

impl Field {
    pub fn key(&self) -> &'static str {
        match self {
            Self::Extension => "extension",
            Self::Size => "size",
            Self::Modified => "time",
            Self::Attributes => "attributes",
            Self::Comment => "comment",
            Self::Wdx(_) => "wdx",
        }
    }

    pub fn label(&self) -> String {
        match self {
            Self::Extension => "Erweiterung",
            Self::Size => "Größe",
            Self::Modified => "Änderungsdatum",
            Self::Attributes => "Attribute",
            Self::Comment => "Kommentar",
            Self::Wdx(column) => return column.selection.label(),
        }
        .into()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Column {
    pub field: Field,
    pub title: String,
    pub width: u16,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Mode {
    pub view: TabView,
    pub columns: Option<String>,
    pub sort: Vec<SortOrder>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Scope {
    #[default]
    Any,
    Local,
    Drive,
    Unc,
    Archive,
    Ftp,
    Wfx,
    Shell,
    Search,
    Branch,
}

impl Scope {
    pub fn key(self) -> &'static str {
        match self {
            Self::Any => "alle",
            Self::Local => "lokal",
            Self::Drive => "laufwerk",
            Self::Unc => "unc",
            Self::Archive => "archiv",
            Self::Ftp => "ftp",
            Self::Wfx => "wfx",
            Self::Shell => "shell",
            Self::Search => "suche",
            Self::Branch => "zweig",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        [
            Self::Any,
            Self::Local,
            Self::Drive,
            Self::Unc,
            Self::Archive,
            Self::Ftp,
            Self::Wfx,
            Self::Shell,
            Self::Search,
            Self::Branch,
        ]
        .into_iter()
        .find(|scope| scope.key().eq_ignore_ascii_case(value))
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Rule {
    pub mode: String,
    pub scope: Scope,
    pub path: String,
    pub contains: String,
    pub excludes: String,
    pub any: bool,
    pub minimum_percent: Option<u8>,
}

impl Rule {
    fn path_mask(&self) -> io::Result<Option<GlobMatcher>> {
        let path = self.path.trim().replace('\\', "/");
        (!path.is_empty())
            .then(|| {
                GlobBuilder::new(&path)
                    .case_insensitive(cfg!(windows))
                    .literal_separator(true)
                    .backslash_escape(false)
                    .build()
                    .map(|pattern| pattern.compile_matcher())
                    .map_err(io::Error::other)
            })
            .transpose()
    }

    pub fn validate(&self) -> io::Result<()> {
        self.path_mask()?;
        FileMasks::new(&self.contains)?;
        FileMasks::new(&self.excludes)?;
        if self
            .minimum_percent
            .is_some_and(|percent| !(1..=100).contains(&percent) || self.contains.trim().is_empty())
        {
            return Err(io::Error::other(
                "Der Dateianteil benötigt eine Dateimaske und einen Wert von 1 bis 100 Prozent.",
            ));
        }
        Ok(())
    }

    fn matches(&self, scope: Scope, path: &str, entries: &[Entry]) -> io::Result<bool> {
        if self.scope != Scope::Any
            && self.scope != scope
            && !(self.scope == Scope::Local && scope == Scope::Unc)
            && !(self.scope == Scope::Drive
                && scope == Scope::Local
                && Path::new(path).has_root()
                && path.as_bytes().get(1) == Some(&b':'))
        {
            return Ok(false);
        }
        let path_match = self
            .path_mask()?
            .map(|mask| mask.is_match(path.replace('\\', "/")));
        let file_match = |value: &str, percent: Option<u8>| -> io::Result<bool> {
            let masks = FileMasks::new(value)?;
            let mut names = entries
                .iter()
                .filter(|entry| entry.kind == Kind::File)
                .map(|entry| Path::new(&entry.name));
            let Some(percent) = percent else {
                return Ok(names.any(|name| masks.includes(name)));
            };
            let (count, matches) = names.fold((0u128, 0u128), |(count, matches), name| {
                (count + 1, matches + u128::from(masks.includes(name)))
            });
            Ok(count > 0 && matches * 100 >= count * u128::from(percent))
        };
        let contains = (!self.contains.trim().is_empty())
            .then(|| file_match(&self.contains, self.minimum_percent))
            .transpose()?;
        let excludes = (!self.excludes.trim().is_empty())
            .then(|| file_match(&self.excludes, None).map(|matches| !matches))
            .transpose()?;
        let conditions = [path_match, contains, excludes];
        Ok(if self.any && conditions.iter().any(Option::is_some) {
            conditions.into_iter().flatten().any(|matches| matches)
        } else {
            conditions.into_iter().flatten().all(|matches| matches)
        })
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub columns: BTreeMap<String, Vec<Column>>,
    pub modes: BTreeMap<String, Mode>,
    pub rules: Vec<Rule>,
    pub automatic: bool,
    pub colors: Vec<FileColor>,
    pub selection: Option<ColorPair>,
    pub cursor: Option<ColorPair>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ColorPair {
    pub light: u32,
    pub dark: u32,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FileColor {
    pub masks: String,
    pub color: ColorPair,
}

pub struct CompiledColor {
    masks: FileMasks,
    pub color: ColorPair,
}

impl CompiledColor {
    pub fn matches(&self, name: &std::ffi::OsStr) -> bool {
        self.masks.includes(Path::new(name))
    }
}

impl Settings {
    pub fn compile_colors(&self) -> Vec<CompiledColor> {
        self.colors
            .iter()
            .take(128)
            .filter_map(|rule| {
                let masks = FileMasks::new(&rule.masks).ok()?;
                Some(CompiledColor {
                    masks,
                    color: rule.color,
                })
            })
            .collect()
    }
    pub fn automatic_mode(&self, scope: Scope, path: &str, entries: &[Entry]) -> Option<&Mode> {
        if !self.automatic {
            return None;
        }
        self.rules.iter().find_map(|rule| {
            let mode = self.modes.get(&rule.mode)?;
            rule.matches(scope, path, entries).ok()?.then_some(mode)
        })
    }

    pub fn needs_comments(&self) -> bool {
        self.columns
            .values()
            .flatten()
            .any(|column| column.field == Field::Comment)
            || (self.automatic
                && self
                    .modes
                    .values()
                    .any(|mode| mode.view == TabView::Comments))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn uses_the_first_matching_rule_and_requires_all_of_its_conditions() {
        let photos = Mode {
            view: TabView::Thumbnails,
            ..Mode::default()
        };
        let details = Mode::default();
        let settings = Settings {
            columns: BTreeMap::new(),
            modes: BTreeMap::from([
                ("Fotos".into(), photos.clone()),
                ("Sonstige".into(), details.clone()),
            ]),
            rules: vec![
                Rule {
                    mode: "Fotos".into(),
                    scope: Scope::Local,
                    path: "C:/Medien/**".into(),
                    contains: "*.jpg".into(),
                    excludes: "*.raw".into(),
                    ..Rule::default()
                },
                Rule {
                    mode: "Sonstige".into(),
                    ..Rule::default()
                },
            ],
            automatic: true,
            ..Settings::default()
        };
        let file = |name: &str| Entry::from_remote(PathBuf::from(name), Kind::File, None, None);
        assert_eq!(
            settings.automatic_mode(Scope::Local, "C:/Medien/Urlaub", &[file("Meer.jpg")]),
            Some(&photos)
        );
        assert_eq!(
            settings.automatic_mode(
                Scope::Local,
                "C:/Medien/Urlaub",
                &[file("Meer.jpg"), file("Meer.raw")]
            ),
            Some(&details)
        );
        assert_eq!(
            settings.automatic_mode(Scope::Archive, "C:/Medien/Urlaub", &[file("Meer.jpg")]),
            Some(&details)
        );
        assert_eq!(
            settings.automatic_mode(Scope::Local, "C:/Andere", &[file("Meer.jpg")]),
            Some(&details)
        );
    }

    #[test]
    fn view_rule_threshold_excludes_directories_and_can_combine_conditions_with_or() {
        let entry = |name: &str, kind| Entry::from_remote(PathBuf::from(name), kind, None, None);
        let files = [
            entry("cover.jpg", Kind::File),
            entry("notes.txt", Kind::File),
            entry("folder.jpg", Kind::Directory),
        ];
        let mut rule = Rule {
            path: "C:/Fotos/**".into(),
            contains: "*.jpg".into(),
            minimum_percent: Some(50),
            ..Rule::default()
        };
        assert!(rule.matches(Scope::Local, "C:/Fotos/2026", &files).unwrap());
        assert!(!rule.matches(Scope::Local, "C:/Andere", &files).unwrap());
        rule.any = true;
        assert!(rule.matches(Scope::Local, "C:/Andere", &files).unwrap());
        rule.minimum_percent = Some(51);
        assert!(!rule.matches(Scope::Local, "C:/Andere", &files).unwrap());
        rule.minimum_percent = Some(0);
        assert!(rule.validate().is_err());
    }
}
