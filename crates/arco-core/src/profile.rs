use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use tempfile::NamedTempFile;

pub struct TcImport {
    pub panels: [Vec<PathBuf>; 2],
    pub favorites: Vec<DirectoryFavorite>,
    pub shortcuts: BTreeMap<String, String>,
    pub skipped: usize,
}

pub fn read_tc_ini(
    path: &Path,
    resolve_variable: impl Fn(&str) -> Option<OsString>,
) -> io::Result<TcImport> {
    let data = read_tc_text(path)?;
    let mut import = TcImport {
        panels: Default::default(),
        favorites: Vec::new(),
        shortcuts: BTreeMap::new(),
        skipped: 0,
    };
    let mut hotlist: BTreeMap<usize, [&str; 3]> = BTreeMap::new();
    for (section, key, value) in ini_entries(&data) {
        if section == "dirmenu" {
            for (index, prefix) in ["menu", "cmd", "path"].into_iter().enumerate() {
                if let Some(number) = key
                    .strip_prefix(prefix)
                    .and_then(|suffix| suffix.parse().ok())
                {
                    hotlist.entry(number).or_default()[index] = value;
                }
            }
            continue;
        }
        let target = match section.as_str() {
            "shortcuts" if !key.is_empty() && !value.is_empty() => {
                import
                    .shortcuts
                    .insert(key.to_uppercase(), value.to_owned());
                continue;
            }
            "shortcuts" | "shortcutswin" => {
                import.skipped += 1;
                continue;
            }
            "left" if key == "path" => &mut import.panels[0],
            "right" if key == "path" => &mut import.panels[1],
            "lefttabs" if is_tab_key(&key) => &mut import.panels[0],
            "righttabs" if is_tab_key(&key) => &mut import.panels[1],
            _ => continue,
        };
        let folder = PathBuf::from(value.trim_matches('"'));
        if !folder.is_absolute() || value.contains(['%', '\0']) {
            import.skipped += 1;
        } else if !target.contains(&folder) {
            target.push(folder);
        }
    }
    let mut groups = Vec::new();
    let mut extra_groups = 0usize;
    for [title, command, opposite] in hotlist.into_values() {
        match title {
            "--" => {
                if extra_groups > 0 {
                    extra_groups -= 1;
                } else {
                    groups.pop();
                }
                continue;
            }
            "-" => continue,
            title if title.starts_with('-') && command.is_empty() => {
                if groups.len() == 32 {
                    extra_groups += 1;
                } else {
                    groups.push(favorite_title(&title[1..]));
                }
                continue;
            }
            _ => (),
        }
        if command.is_empty() {
            continue;
        }
        if extra_groups > 0 || import.favorites.len() == 4096 {
            import.skipped += 1;
            continue;
        }
        let folder = command
            .split_once(char::is_whitespace)
            .filter(|(verb, _)| verb.eq_ignore_ascii_case("cd"))
            .and_then(|(_, path)| tc_hotlist_path(path, &resolve_variable));
        let Some(path) = folder else {
            import.skipped += 1;
            continue;
        };
        let other = tc_hotlist_path(opposite, &resolve_variable);
        if !opposite.is_empty() && other.is_none() {
            import.skipped += 1;
        }
        let favorite = DirectoryFavorite {
            title: favorite_title(title),
            groups: groups.clone(),
            path,
            other,
        };
        if !import.favorites.contains(&favorite) {
            import.favorites.push(favorite);
        }
    }
    Ok(import)
}

fn favorite_title(value: &str) -> String {
    let mut characters = value.chars().peekable();
    let mut title = String::new();
    while let Some(character) = characters.next() {
        if character == '&' && characters.next_if_eq(&'&').is_none() {
            continue;
        }
        if !character.is_control() {
            title.push(character);
        }
    }
    title.trim().to_owned()
}

fn tc_hotlist_path(
    value: &str,
    resolve_variable: &impl Fn(&str) -> Option<OsString>,
) -> Option<PathBuf> {
    let mut remaining = value.trim().trim_matches('"');
    let mut expanded = OsString::new();
    while let Some((prefix, variable)) = remaining.split_once('%') {
        let (name, suffix) = variable.split_once('%')?;
        if name.is_empty() {
            return None;
        }
        expanded.push(prefix);
        expanded.push(resolve_variable(name)?);
        remaining = suffix;
    }
    expanded.push(remaining);
    let path = PathBuf::from(expanded);
    (path.is_absolute() && !path.as_os_str().as_encoded_bytes().contains(&0)).then_some(path)
}

pub fn read_tc_text(path: &Path) -> io::Result<String> {
    use std::io::Read;
    const LIMIT: usize = 16 * 1024 * 1024;
    let mut raw = Vec::new();
    fs::File::open(path)?
        .take(LIMIT as u64 + 1)
        .read_to_end(&mut raw)?;
    if raw.len() > LIMIT {
        return Err(io::Error::other("TC INI exceeds the import size limit"));
    }
    let data = match encoding_rs::Encoding::for_bom(&raw) {
        Some((encoding, bom)) => encoding.decode_without_bom_handling(&raw[bom..]).0,
        None => match std::str::from_utf8(&raw) {
            Ok(text) => std::borrow::Cow::Borrowed(text),
            Err(_) => {
                encoding_rs::WINDOWS_1252
                    .decode_without_bom_handling(&raw)
                    .0
            }
        },
    };
    Ok(data.into_owned())
}

pub(crate) fn ini_entries(data: &str) -> impl Iterator<Item = (String, String, &str)> {
    ini_raw_entries(data).map(|(section, key, value)| (section, key.to_ascii_lowercase(), value))
}

pub(crate) fn ini_raw_entries(data: &str) -> impl Iterator<Item = (String, &str, &str)> {
    let mut section = String::new();
    data.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with(';'))
        .filter_map(move |line| {
            if let Some(name) = line
                .strip_prefix('[')
                .and_then(|value| value.strip_suffix(']'))
            {
                section = name.to_ascii_lowercase();
                return None;
            }
            let (key, value) = line.split_once('=')?;
            Some((section.clone(), key.trim(), value.trim()))
        })
}
fn is_tab_key(key: &str) -> bool {
    key.strip_suffix("_path")
        .is_some_and(|index| index.parse::<usize>().is_ok())
}

pub struct TcTabs {
    pub panels: [SavedPanel; 2],
    pub skipped: usize,
    pub unhandled_options: usize,
}

pub fn write_tc_tabs(path: &Path, panels: &[SavedPanel; 2], source: usize) -> io::Result<()> {
    if !path.is_absolute()
        || !path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("tab"))
        || panels.iter().all(|panel| panel.tabs.is_empty())
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Für den Export werden Ordner-Tabs und ein absoluter .tab-Pfad benötigt.",
        ));
    }
    let source = source.min(1);
    let mut text = String::new();
    use std::fmt::Write as _;
    for (section, panel) in [
        ("activetabs", &panels[source]),
        ("inactivetabs", &panels[1 - source]),
    ] {
        let _ = write!(text, "[{section}]\r\n");
        for (index, folder) in panel.tabs.iter().enumerate() {
            let name = folder
                .to_str()
                .filter(|name| !name.chars().any(char::is_control))
                .filter(|_| folder.is_absolute())
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        "Ein Tab enthält einen ungültigen lokalen Ordnerpfad.",
                    )
                })?;
            let _ = write!(text, "{index}_path={name}\r\n");
            let lock = if let Some(options) = panel.tab_options.get(index) {
                if !options.title.is_empty() {
                    if options.title.chars().any(char::is_control) {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "Ein Tab-Titel enthält Steuerzeichen.",
                        ));
                    }
                    let _ = write!(text, "{index}_caption={}\r\n", options.title);
                }
                match options.lock {
                    TabLock::Unlocked => 0,
                    TabLock::Fixed => 1,
                    TabLock::ReturnToBase => 2,
                }
            } else {
                0
            };
            let _ = write!(text, "{index}_options=1|0|0|0|0|{lock}|0\r\n");
        }
        if !panel.tabs.is_empty() {
            let active = panel.active_tab.min(panel.tabs.len() - 1);
            let _ = write!(text, "activetab={active}\r\n");
        }
        text.push_str("\r\n");
    }
    let mut output = NamedTempFile::new_in(path.parent().expect("absolute .tab path"))?;
    output.write_all(&[0xff, 0xfe])?;
    let bytes: Vec<_> = text.encode_utf16().flat_map(u16::to_le_bytes).collect();
    output.write_all(&bytes)?;
    output.as_file().sync_all()?;
    output
        .persist_noclobber(path)
        .map_err(|error| error.error)?;
    Ok(())
}

#[derive(Default)]
struct TcTab {
    path: String,
    title: String,
    options: String,
    filter: bool,
}

pub fn read_tc_tabs(path: &Path, bases: [&Path; 2], source: usize) -> io::Result<TcTabs> {
    let text = read_tc_text(path)?;
    let mut sections: [BTreeMap<u32, TcTab>; 2] = Default::default();
    let mut current = [0; 2];
    let mut unhandled_options = 0;
    let source = source.min(1);
    for (section, key, value) in ini_entries(&text) {
        let side = match section.as_str() {
            "activetabs" => source,
            "inactivetabs" => 1 - source,
            _ => continue,
        };
        if key == "activetab" {
            current[side] = value.parse().unwrap_or_default();
            continue;
        }
        if key == "redirectsection" {
            unhandled_options += 1;
            continue;
        }
        let Some((index, field)) = key.split_once('_') else {
            continue;
        };
        let Ok(index) = index.parse::<u32>() else {
            continue;
        };
        let tab = sections[side].entry(index).or_default();
        match field {
            "path" => tab.path = value.into(),
            "caption" => tab.title = value.into(),
            "options" => tab.options = value.into(),
            "filter" => tab.filter = !value.is_empty(),
            _ => unhandled_options += 1,
        }
    }
    let mut skipped = 0;
    let panels = std::array::from_fn(|side| {
        let mut panel = SavedPanel::default();
        for (index, tab) in std::mem::take(&mut sections[side]) {
            let raw = tab.path.trim_matches('"');
            let path = if raw == "." {
                bases[side].to_path_buf()
            } else {
                PathBuf::from(raw)
            };
            if !path.is_absolute()
                || raw.starts_with(r"\\.\")
                || raw.starts_with(r"\\\")
                || raw.chars().any(char::is_control)
            {
                skipped += 1;
                continue;
            }
            let mut fields = tab.options.split('|');
            let lock = match fields.nth(5) {
                Some("1") => TabLock::Fixed,
                Some("2") => TabLock::ReturnToBase,
                Some("0" | "") | None => TabLock::Unlocked,
                Some(_) => {
                    unhandled_options += 1;
                    TabLock::Unlocked
                }
            };
            if tab.filter
                || tab
                    .options
                    .split('|')
                    .enumerate()
                    .any(|(index, value)| index != 5 && !matches!(value, "" | "0"))
            {
                unhandled_options += 1;
            }
            if index <= current[side] {
                panel.active_tab = panel.tabs.len();
            }
            panel.tabs.push(path);
            panel.tab_options.push(SavedTab {
                title: tab
                    .title
                    .chars()
                    .filter(|character| !character.is_control())
                    .take(120)
                    .collect(),
                lock,
                ..SavedTab::default()
            });
        }
        panel
    });
    if panels.iter().all(|panel| panel.tabs.is_empty()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Die Datei enthält keine importierbaren Ordner-Tabs.",
        ));
    }
    Ok(TcTabs {
        panels,
        skipped,
        unhandled_options,
    })
}

#[derive(Default)]
pub struct TcFtpSite {
    pub host: String,
    pub directory: String,
    pub local_directory: Option<PathBuf>,
    pub user: String,
    pub anonymous: bool,
    pub passive: Option<bool>,
    pub tls: bool,
    pub listing: FtpListing,
    pub unsupported: bool,
}

pub fn read_tc_ftp_ini(path: &Path) -> io::Result<Vec<(String, TcFtpSite)>> {
    let text = read_tc_text(path)?;
    let mut connections = BTreeMap::new();
    let mut sections = BTreeMap::<String, TcFtpSite>::new();
    let mut default_passive = false;
    let mut external_connections = false;
    for (section, key, value) in ini_entries(&text) {
        match section.as_str() {
            "connections" => {
                if let Ok(number) = key.parse::<u32>() {
                    connections.insert(number, value.to_owned());
                } else if key == "redirectsection" {
                    external_connections = true;
                }
            }
            "default" if key == "pasvmode" => default_passive = value == "1",
            _ => {
                match key.as_str() {
                    "host" | "directory" | "localdir" | "username" | "anonymous" | "pasvmode"
                    | "ssl" | "mlsd" | "firewall" | "hosttype" | "clientcert" | "initcommands"
                    | "acct" | "redirectsection" => (),
                    _ => continue,
                }
                let site = sections.entry(section).or_default();
                match key.as_str() {
                    "host" => site.host = value.to_owned(),
                    "directory" => site.directory = value.to_owned(),
                    "localdir" if !value.is_empty() => site.local_directory = Some(value.into()),
                    "username" => site.user = value.to_owned(),
                    "anonymous" => site.anonymous = value == "1",
                    "pasvmode" => site.passive = Some(value == "1"),
                    "mlsd" => {
                        site.listing = match value {
                            "0" => FtpListing::List,
                            "1" => FtpListing::Mlsd,
                            _ => FtpListing::Automatic,
                        };
                    }
                    "ssl" => {
                        site.tls = value == "1";
                        site.unsupported |= !matches!(value, "" | "0" | "1");
                    }
                    "firewall" => site.unsupported |= !matches!(value, "" | "0"),
                    "hosttype" | "clientcert" | "initcommands" | "acct" | "redirectsection" => {
                        site.unsupported |= !value.is_empty();
                    }
                    _ => (),
                }
            }
        }
    }
    if external_connections {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Die FTP-Verbindungsliste verweist auf eine andere INI-Datei.",
        ));
    }
    Ok(connections
        .into_values()
        .map(|name| {
            let mut site = sections
                .remove(&name.to_ascii_lowercase())
                .unwrap_or_default();
            site.passive.get_or_insert(default_passive);
            (name, site)
        })
        .collect())
}

#[cfg(test)]
mod tests {
    #[test]
    fn expands_hotlist_variables_on_both_sides_and_skips_unresolved_paths() {
        let fixture = tempfile::tempdir().unwrap();
        let profile = fixture.path().join("wincmd.ini");
        let install = fixture.path().join("Portabel");
        let documents = fixture.path().join("Dokumente");
        std::fs::write(
            &profile,
            r#"[DirMenu]
menu1=-Projekte
menu2=Berichte
cmd2=cd "%commander_path%/Berichte %SUFFIX%"
path2=%$PERSONAL%/Vergleich
menu3=--
menu4=Unbekannt
cmd4=cd %UNKNOWN%/Datei
menu5=Unvollständig
cmd5=cd %COMMANDER_PATH
"#,
        )
        .unwrap();
        let result =
            super::read_tc_ini(&profile, |name| match name.to_ascii_uppercase().as_str() {
                "COMMANDER_PATH" => Some(install.as_os_str().to_owned()),
                "$PERSONAL" => Some(documents.as_os_str().to_owned()),
                "SUFFIX" => Some("Ö".into()),
                _ => None,
            })
            .unwrap();
        assert_eq!(result.skipped, 2);
        assert_eq!(result.favorites.len(), 1);
        assert_eq!(result.favorites[0].groups, ["Projekte"]);
        assert_eq!(result.favorites[0].path, install.join("Berichte Ö"));
        assert_eq!(result.favorites[0].other, Some(documents.join("Vergleich")));
    }

    #[test]
    fn imports_nested_tc_favorites_and_optional_opposite_folder() {
        let fixture = tempfile::tempdir().unwrap();
        let profile = fixture.path().join("wincmd.ini");
        let source = fixture.path().join("Berichte ä mit Leerzeichen");
        let other = fixture.path().join("Vergleich");
        std::fs::write(&profile, format!(
            "[DirMenu]\ncmd3=cd \"{}\"\npath3={}\nmenu1=-Pro&jekte\nmenu2=-Analyse && Daten\nmenu3=&Berichte\nmenu4=--\nmenu5=--\nmenu6=Programm\ncmd6=notepad.exe\n",
            source.display(), other.display()
        )).unwrap();
        let result = super::read_tc_ini(&profile, |_| None).unwrap();
        assert_eq!(result.skipped, 1);
        assert_eq!(
            result.favorites,
            [super::DirectoryFavorite {
                title: "Berichte".into(),
                groups: vec!["Projekte".into(), "Analyse & Daten".into()],
                path: source,
                other: Some(other),
            }]
        );
    }

    #[test]
    fn sync_profiles_keep_existing_options_and_round_trip_fixed_roots() {
        let mut saved: super::SavedSyncProfile =
            serde_json::from_str(r#"{"mode":"Mirror","masks":"*.txt","recursive":false}"#).unwrap();
        assert!(saved.roots.is_none());
        assert_eq!(saved.options.mode, crate::sync::Mode::Mirror);
        assert!(!saved.options.recursive);
        saved.roots = Some([
            std::path::PathBuf::from("links"),
            std::path::PathBuf::from("rechts"),
        ]);
        let restored: super::SavedSyncProfile =
            serde_json::from_str(&serde_json::to_string(&saved).unwrap()).unwrap();
        assert_eq!(restored.options.masks, "*.txt");
        assert_eq!(restored.roots, saved.roots);
    }

    use super::*;

    #[test]
    fn wdx_search_profiles_round_trip_and_are_optional_in_existing_profiles() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("arco.json");
        let mut profile = Profile::default();
        profile.save(&path).unwrap();
        assert!(
            !fs::read_to_string(&path)
                .unwrap()
                .contains("wdx_search_profiles")
        );
        assert!(Profile::load(&path).unwrap().wdx_search_profiles.is_empty());
        let search = WdxSearchProfile {
            masks: "*.txt;*.md".into(),
            all: false,
            conditions: vec![
                WdxSearchField {
                    plugin: PathBuf::from("meta.wdx64"),
                    selection: crate::plugin::WdxField {
                        field: "Size".into(),
                        unit: Some("kbytes".into()),
                    },
                    expression: "> 100".into(),
                },
                WdxSearchField {
                    plugin: PathBuf::from("tags.wdx64"),
                    selection: crate::plugin::WdxField {
                        field: "Title".into(),
                        unit: None,
                    },
                    expression: "~Grüße".into(),
                },
            ],
        };
        profile
            .wdx_search_profiles
            .insert("Übersicht".into(), search.clone());
        profile.save(&path).unwrap();
        assert_eq!(
            Profile::load(&path).unwrap().wdx_search_profiles["Übersicht"],
            search
        );
    }

    #[cfg(windows)]
    #[test]
    fn tc_tab_export_round_trips_without_replacing_an_existing_file() {
        let fixture = tempfile::tempdir().unwrap();
        let file = fixture.path().join("Test.tab");
        let panels = [
            SavedPanel {
                tabs: vec![PathBuf::from(r"C:\Test")],
                ..Default::default()
            },
            SavedPanel {
                tabs: vec![
                    PathBuf::from(r"C:\Archiv"),
                    PathBuf::from(r"\\server\share\Übersicht"),
                ],
                active_tab: 1,
                tab_options: vec![
                    SavedTab::default(),
                    SavedTab {
                        title: "Übersicht".into(),
                        lock: TabLock::ReturnToBase,
                        ..SavedTab::default()
                    },
                ],
            },
        ];
        write_tc_tabs(&file, &panels, 1).unwrap();
        let bytes = fs::read(&file).unwrap();
        assert_eq!(&bytes[..2], &[0xff, 0xfe]);
        assert_eq!(
            write_tc_tabs(&file, &panels, 0).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(fs::read(&file).unwrap(), bytes);
        let loaded = read_tc_tabs(&file, [Path::new(r"C:\"), Path::new(r"C:\")], 1).unwrap();
        assert_eq!(loaded.panels[0].tabs, panels[0].tabs);
        assert_eq!(loaded.panels[1].tabs, panels[1].tabs);
        assert_eq!(loaded.panels[1].active_tab, 1);
        assert_eq!(loaded.panels[1].tab_options, panels[1].tab_options);
    }

    #[cfg(windows)]
    #[test]
    fn tc_tabs_preserve_both_roles_captions_and_lock_modes() {
        let fixture = tempfile::tempdir().unwrap();
        let file = fixture.path().join("Projekt.tab");
        let text = "[activeTabs]\r\n0_path=C:\\Workspace\r\n0_caption=Übersicht\r\n0_options=0|0|0|0|0|1|0\r\n5_path=ftp://invalid/\r\n8_path=.\r\n8_options=0|0|0|0|0|2|0\r\nactivetab=8\r\n[inactiveTabs]\r\n3_path=\\\\server\\share\\ordner\r\n3_filter=*.txt\r\n";
        let bytes: Vec<_> = [0xfeff]
            .into_iter()
            .chain(text.encode_utf16())
            .flat_map(u16::to_le_bytes)
            .collect();
        fs::write(&file, bytes).unwrap();
        let tabs = read_tc_tabs(&file, [Path::new(r"C:\left"), Path::new(r"C:\right")], 1).unwrap();
        assert_eq!(
            tabs.panels[1].tabs,
            [PathBuf::from(r"C:\Workspace"), PathBuf::from(r"C:\right")]
        );
        assert_eq!(tabs.panels[1].active_tab, 1);
        assert_eq!(tabs.panels[1].tab_options[0].title, "Übersicht");
        assert_eq!(tabs.panels[1].tab_options[0].lock, TabLock::Fixed);
        assert_eq!(tabs.panels[1].tab_options[1].lock, TabLock::ReturnToBase);
        assert_eq!(
            tabs.panels[0].tabs,
            [PathBuf::from(r"\\server\share\ordner")]
        );
        assert_eq!(tabs.skipped, 1);
        assert_eq!(tabs.unhandled_options, 1);
    }

    #[test]
    fn tc_import_keeps_the_last_case_insensitive_binding_and_counts_win_shortcuts() {
        let file = NamedTempFile::new().unwrap();
        fs::write(file.path(), "[Shortcuts]\nA+F1=cm_RereadSource\na+f1=cm_LeftOpenDrives\n[ShortcutsWin]\nF1=cm_SearchFor\n").unwrap();
        let profile = read_tc_ini(file.path(), |_| None).unwrap();
        assert_eq!(
            profile.shortcuts,
            BTreeMap::from([("A+F1".into(), "cm_LeftOpenDrives".into())])
        );
        assert_eq!(profile.skipped, 1);
    }

    #[test]
    fn tab_view_defaults_to_details_and_round_trips_in_saved_tabs() {
        let old: SavedTab = serde_json::from_str(r#"{"title":"Bilder","lock":"fixed"}"#).unwrap();
        assert_eq!(old.view, TabView::Details);
        let tab = SavedTab {
            view: TabView::Thumbnails,
            ..old
        };
        let saved: SavedTab = serde_json::from_str(&serde_json::to_string(&tab).unwrap()).unwrap();
        assert_eq!(saved, tab);
    }
}

const PROFILE_VERSION: u32 = 1;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedPanel {
    pub tabs: Vec<PathBuf>,
    pub active_tab: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tab_options: Vec<SavedTab>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedTab {
    pub title: String,
    pub lock: TabLock,
    pub view: TabView,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column_set: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub sort: Vec<crate::files::SortOrder>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shell: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TabView {
    #[default]
    Details,
    Short,
    Thumbnails,
    Comments,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TabLock {
    #[default]
    Unlocked,
    Fixed,
    ReturnToBase,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct LayoutSettings {
    pub toolbar: bool,
    pub panel_toolbar: bool,
    pub drive_buttons: bool,
    pub drive_list: bool,
    pub folder_tabs: bool,
    pub current_path: bool,
    pub breadcrumbs: bool,
    pub history_buttons: bool,
    pub column_headers: bool,
    pub panel_status: bool,
    pub command_line: bool,
    pub function_keys: bool,
    pub numpad_folders: bool,
    pub quick_find_start: QuickFindStart,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum QuickFindStart {
    #[default]
    Shortcut,
    Typing,
    Alt,
    CtrlAlt,
}

impl Default for LayoutSettings {
    fn default() -> Self {
        Self {
            toolbar: true,
            panel_toolbar: false,
            drive_buttons: false,
            drive_list: true,
            folder_tabs: true,
            current_path: true,
            breadcrumbs: true,
            history_buttons: true,
            column_headers: true,
            panel_status: true,
            command_line: true,
            function_keys: true,
            numpad_folders: false,
            quick_find_start: QuickFindStart::Shortcut,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Profile {
    version: u32,
    pub panels: [SavedPanel; 2],
    pub favorites: Vec<PathBuf>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub directory_favorites: Vec<DirectoryFavorite>,
    pub active_panel: usize,
    pub show_hidden: bool,
    pub ignore_enabled: bool,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub ignore_rules: String,
    pub light_mode: bool,
    pub system_theme: bool,
    pub stacked: bool,
    pub split_ratio: f32,
    pub separate_tree_mode: u8,
    pub layout: LayoutSettings,
    #[serde(skip_serializing_if = "empty_buttonbars")]
    pub buttonbars: [Vec<ButtonbarButton>; 2],
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub starter: Vec<ButtonbarButton>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub buttonbar_templates: BTreeMap<String, ButtonbarTemplate>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub tab_groups: BTreeMap<String, [SavedPanel; 2]>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub plugins: Vec<crate::plugin::Registration>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub wdx_columns: Vec<crate::plugin::WdxColumn>,
    pub views: crate::view_settings::Settings,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub associations: Vec<FileAssociation>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub rename_templates: BTreeMap<String, RenameTemplate>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub wdx_search_profiles: BTreeMap<String, WdxSearchProfile>,
    pub search_last: crate::advanced_search::Options,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub search_profiles: BTreeMap<String, crate::advanced_search::Options>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub everything_cli: Option<PathBuf>,
    pub transfer_rate_kib: u32,
    pub transfer_ntfs: crate::files::NtfsCopyOptions,
    pub transfer_descriptions: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transfer_log: Option<PathBuf>,
    pub list_font: ListFont,
    pub list_font_size: u8,
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub shortcuts: std::collections::BTreeMap<String, Option<String>>,
    pub sync_options: crate::sync::Options,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub sync_profiles: BTreeMap<String, SavedSyncProfile>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub ftp_profiles: BTreeMap<String, FtpProfile>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub packer_profiles: BTreeMap<String, crate::packer_profile::PackerProfile>,
    pub editor: Option<PathBuf>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SavedSyncProfile {
    #[serde(flatten)]
    pub options: crate::sync::Options,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roots: Option<[PathBuf; 2]>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FileAssociation {
    pub name: String,
    pub masks: String,
    pub program: PathBuf,
    pub arguments: Vec<String>,
    pub default_open: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RenameTemplate {
    pub mask: String,
    pub replacement: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DirectoryFavorite {
    pub title: String,
    pub groups: Vec<String>,
    pub path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub other: Option<PathBuf>,
}

impl Default for Profile {
    fn default() -> Self {
        Self {
            version: PROFILE_VERSION,
            panels: Default::default(),
            favorites: Vec::new(),
            directory_favorites: Vec::new(),
            active_panel: 0,
            show_hidden: false,
            ignore_enabled: false,
            ignore_rules: String::new(),
            light_mode: false,
            system_theme: false,
            stacked: false,
            split_ratio: 0.5,
            separate_tree_mode: 0,
            layout: LayoutSettings::default(),
            buttonbars: Default::default(),
            starter: Vec::new(),
            buttonbar_templates: BTreeMap::new(),
            tab_groups: BTreeMap::new(),
            plugins: Vec::new(),
            wdx_columns: Vec::new(),
            views: Default::default(),
            associations: Vec::new(),
            rename_templates: BTreeMap::new(),
            wdx_search_profiles: BTreeMap::new(),
            search_last: Default::default(),
            search_profiles: BTreeMap::new(),
            everything_cli: None,
            transfer_rate_kib: 0,
            transfer_ntfs: Default::default(),
            transfer_descriptions: true,
            transfer_log: None,
            list_font: ListFont::default(),
            list_font_size: 14,
            shortcuts: Default::default(),
            sync_options: Default::default(),
            sync_profiles: BTreeMap::new(),
            ftp_profiles: BTreeMap::new(),
            packer_profiles: BTreeMap::new(),
            editor: None,
        }
    }
}

fn empty_buttonbars(bars: &[Vec<ButtonbarButton>; 2]) -> bool {
    bars.iter().all(Vec::is_empty)
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ButtonbarButton {
    pub label: String,
    pub icon: String,
    pub action: ButtonbarAction,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ButtonbarTemplate {
    pub bars: [Vec<ButtonbarButton>; 2],
    pub starter: Vec<ButtonbarButton>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum ButtonbarAction {
    Internal {
        command: String,
    },
    Program {
        path: PathBuf,
        arguments: Vec<String>,
    },
}

#[derive(Default)]
pub struct TcButtonbarEntry {
    pub command: String,
    pub parameters: String,
    pub working_directory: String,
    pub icon: String,
    pub label: String,
}

pub fn parse_tc_buttonbar(text: &str) -> io::Result<Vec<TcButtonbarEntry>> {
    let entries: BTreeMap<_, _> = ini_entries(text)
        .filter_map(|(section, key, value)| (section == "buttonbar").then_some((key, value)))
        .collect();
    let count = entries
        .get("buttoncount")
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|count| *count <= 4096)
        .ok_or_else(|| io::Error::other("Ungültiger Buttoncount in der TC-Buttonbar."))?;
    Ok((1..=count)
        .map(|index| {
            let value = |key: &str| {
                entries
                    .get(&format!("{key}{index}"))
                    .copied()
                    .unwrap_or_default()
                    .to_owned()
            };
            TcButtonbarEntry {
                command: value("cmd"),
                parameters: value("param"),
                working_directory: value("path"),
                icon: value("button"),
                label: value("menu"),
            }
        })
        .collect())
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WdxSearchProfile {
    pub masks: String,
    pub all: bool,
    pub conditions: Vec<WdxSearchField>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WdxSearchField {
    pub plugin: PathBuf,
    #[serde(flatten)]
    pub selection: crate::plugin::WdxField,
    pub expression: String,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ListFont {
    #[default]
    SegoeUi,
    Arial,
    Verdana,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FtpSecurity {
    #[default]
    ExplicitTls,
    ImplicitTls,
    Plain,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FtpListing {
    #[default]
    Automatic,
    Mlsd,
    List,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FtpProfile {
    pub address: String,
    pub user: String,
    pub security: FtpSecurity,
    pub proxy: String,
    pub active: bool,
    pub listing: FtpListing,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_directory: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credential: Option<String>,
}

impl Profile {
    pub fn load(path: &Path) -> io::Result<Self> {
        let content = match fs::read(path) {
            Ok(content) => content,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(error) => return Err(error),
        };
        let profile: Self = serde_json::from_slice(&content).map_err(io::Error::other)?;
        if profile.version != PROFILE_VERSION {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "unsupported profile version",
            ));
        }
        Ok(profile)
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        fs::create_dir_all(parent)?;
        let mut staging = NamedTempFile::new_in(parent)?;
        serde_json::to_writer_pretty(&mut staging, self).map_err(io::Error::other)?;
        staging.write_all(b"\n")?;
        staging.as_file().sync_all()?;
        staging.persist(path).map_err(|error| error.error)?;
        Ok(())
    }
}
