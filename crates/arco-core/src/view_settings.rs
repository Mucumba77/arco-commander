use crate::files::{Entry, Kind, SortKey, SortOrder};
use crate::plugin::{WdxColumn, WfxField};
use crate::profile::TabView;
use crate::sync::FileMasks;
use globset::{GlobBuilder, GlobMatcher};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    io,
    path::{Path, PathBuf},
};

mod tc;
mod tc_columns;
mod tc_drives;
pub use tc::{TcViews, parse_wfx_default_view, read_tc_views};
pub use tc_columns::TcColumnExpression;
pub use tc_drives::{request_tc_drive_type, tc_drive_changed_since, tc_drive_revision};

pub const MAX_COLUMNS: usize = 29;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Field {
    Extension,
    Size,
    Modified,
    Attributes,
    Comment,
    TcExpression(TcColumnExpression),
    Wdx(WdxColumn),
    Wfx(WfxField),
}

impl Field {
    pub fn key(&self) -> &'static str {
        match self {
            Self::Extension => "extension",
            Self::Size => "size",
            Self::Modified => "time",
            Self::Attributes => "attributes",
            Self::Comment => "comment",
            Self::TcExpression(expression) => expression.sort_key().map_or("", SortKey::column),
            Self::Wdx(_) => "wdx",
            Self::Wfx(_) => "wfx",
        }
    }

    pub fn label(&self) -> String {
        match self {
            Self::Extension => "Erweiterung",
            Self::Size => "Größe",
            Self::Modified => "Änderungsdatum",
            Self::Attributes => "Attribute",
            Self::Comment => "Kommentar",
            Self::TcExpression(_) => "TC-Ausdruck",
            Self::Wdx(column) => return column.selection.label(),
            Self::Wfx(field) => return field.selection.label(),
        }
        .into()
    }

    pub fn needs_comment(&self) -> bool {
        match self {
            Self::Comment => true,
            Self::TcExpression(expression) => expression.uses_comment(),
            _ => false,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Column {
    pub field: Field,
    pub title: String,
    pub width: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub right_aligned: Option<bool>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Mode {
    pub view: TabView,
    pub inherit_view: bool,
    pub columns: Option<String>,
    pub sort: Vec<SortOrder>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub colors: Option<ModeColors>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ModeColors {
    pub tab: Option<u32>,
    pub light: [Option<u32>; 2],
    pub dark: [Option<u32>; 2],
}

impl ModeColors {
    pub(crate) fn from_tc(
        fields: impl IntoIterator<Item = impl AsRef<str>>,
    ) -> (Option<Self>, bool) {
        let mut fields = fields.into_iter();
        let mut partial = false;
        let mut color = || match fields
            .next()
            .as_ref()
            .map(AsRef::as_ref)
            .unwrap_or_default()
        {
            "" | "-1" => None,
            field => match field.parse::<u32>() {
                Ok(bgr) if bgr <= 0xffffff => Some(swap_red_blue(bgr)),
                _ => {
                    partial = true;
                    None
                }
            },
        };
        let colors = Self {
            tab: color(),
            light: [color(), color()],
            dark: [color(), color()],
        };
        partial |= fields.any(|field| !matches!(field.as_ref(), "" | "-1"));
        ((colors != Self::default()).then_some(colors), partial)
    }

    pub(crate) fn to_tc(self) -> String {
        [self.tab]
            .into_iter()
            .chain(self.light)
            .chain(self.dark)
            .map(|color| color.map_or_else(|| "-1".into(), |rgb| swap_red_blue(rgb).to_string()))
            .collect::<Vec<_>>()
            .join(",")
    }
}

fn swap_red_blue(value: u32) -> u32 {
    ((value & 0xff) << 16) | (value & 0x00ff00) | ((value >> 16) & 0xff)
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tc_expression: Option<String>,
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
        if let Some(expression) = &self.tc_expression {
            return tc::parse_expression(expression).map(|_| ());
        }
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
        if let Some(expression) = &self.tc_expression {
            return Ok(tc::parse_expression(expression)?.matches(scope, path, entries));
        }
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
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub wfx_columns: BTreeMap<PathBuf, WfxColumnSets>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub tc_columns: BTreeMap<u16, TcColumnLayout>,
    pub modes: BTreeMap<String, Mode>,
    pub rules: Vec<Rule>,
    pub automatic: bool,
    pub colors: Vec<FileColor>,
    pub selection: Option<ColorPair>,
    pub cursor: Option<ColorPair>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct WfxColumnSets {
    pub sets: BTreeMap<String, Vec<Column>>,
    pub active: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plugin_default: Option<bool>,
}

impl WfxColumnSets {
    pub fn active_columns(&self) -> Option<&[Column]> {
        self.sets.get(self.active.as_ref()?).map(Vec::as_slice)
    }

    pub fn uses_plugin_default(&self) -> bool {
        self.active.is_none() && self.plugin_default.unwrap_or(self.sets.is_empty())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TcColumnLayout {
    pub name: String,
    pub sort_keys: Vec<Option<SortKey>>,
}

impl TcColumnLayout {
    pub(crate) fn custom_sort(&self, index: usize) -> Option<SortKey> {
        self.sort_keys.get(index).copied().flatten()
    }

    pub(crate) fn sort_index(&self, key: SortKey) -> Option<usize> {
        self.sort_keys.iter().position(|entry| *entry == Some(key))
    }
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
    pub fn uses_tc_drive_types(&self) -> bool {
        self.automatic
            && self.rules.iter().any(|rule| {
                self.modes.contains_key(&rule.mode)
                    && rule.tc_expression.as_ref().is_some_and(|expression| {
                        expression
                            .split("||")
                            .flat_map(|group| group.split("|&"))
                            .any(|term| {
                                term.trim().strip_prefix('L').is_some_and(|types| {
                                    !types.is_empty()
                                        && types.bytes().all(|kind| (b'1'..=b'6').contains(&kind))
                                })
                            })
                    })
            })
    }

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
            .any(|column| column.field.needs_comment())
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
