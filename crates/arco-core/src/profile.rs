use crate::files::{SortKey, SortOrder};
use crate::view_settings::{Column, TcColumnLayout};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use tempfile::NamedTempFile;

mod tab_views;
use tab_views::PortableViews;

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
            "shortcutswin" if !key.is_empty() && !value.is_empty() => {
                import
                    .shortcuts
                    .insert(format!("WIN+{}", key.to_uppercase()), value.to_owned());
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
    let path = PathBuf::from(expand_tc_path(value, resolve_variable)?);
    path.is_absolute().then_some(path)
}

pub fn expand_tc_path(
    value: &str,
    resolve_variable: &impl Fn(&str) -> Option<OsString>,
) -> Option<OsString> {
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
    (!expanded.as_encoded_bytes().contains(&0)).then_some(expanded)
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
    portable_views: PortableViews,
}

impl TcTabs {
    pub fn embedded_column_count(&self) -> usize {
        self.portable_views.set_count()
    }

    pub fn install_embedded_columns(
        &mut self,
        columns: &mut BTreeMap<String, Vec<Column>>,
    ) -> io::Result<()> {
        std::mem::take(&mut self.portable_views).install(&mut self.panels, columns)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum VirtualTab {
    Archive {
        file: PathBuf,
        folder: PathBuf,
    },
    Wfx {
        plugin: String,
        directory: String,
    },
    Ftp {
        profile: String,
        url: Option<String>,
    },
}

impl VirtualTab {
    pub fn tc_path(&self) -> Option<String> {
        match self {
            Self::Archive { file, folder }
                if file.is_absolute() && valid_archive_folder(folder) =>
            {
                let path = if folder.as_os_str().is_empty() {
                    file.clone()
                } else {
                    file.join(folder)
                };
                path.into_os_string()
                    .into_string()
                    .ok()
                    .filter(|value| clean_tab_text(value))
            }
            Self::Wfx { plugin, directory }
                if clean_tab_text(plugin)
                    && !plugin.is_empty()
                    && !plugin.contains(['\\', '/'])
                    && valid_wfx_directory(directory) =>
            {
                Some(format!(r"\\\{plugin}{directory}"))
            }
            Self::Ftp { profile, url } if clean_tab_text(profile) && !profile.trim().is_empty() => {
                match url {
                    Some(url) => clean_ftp_url(url).map(|url| format!("{url}\t{profile}")),
                    None => Some(format!("FTPOPEN:{profile}")),
                }
            }
            _ => None,
        }
    }
}

fn clean_tab_text(value: &str) -> bool {
    !value.chars().any(char::is_control)
}

pub fn shell_tab_address(value: &str) -> bool {
    clean_tab_text(value)
        && (value
            .strip_prefix("::{")
            .is_some_and(|rest| rest.contains('}'))
            || value
                .get(..6)
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("shell:"))
                && value.len() > 6)
}

fn valid_archive_folder(path: &Path) -> bool {
    path.components().all(|part| match part {
        std::path::Component::Normal(name) => crate::files::leaf_name(name).is_ok(),
        _ => false,
    })
}

fn valid_wfx_directory(directory: &str) -> bool {
    directory.strip_prefix('\\').is_some_and(|tail| {
        tail.is_empty()
            || tail
                .split('\\')
                .all(|name| crate::files::leaf_name(name.as_ref()).is_ok())
    })
}

fn clean_ftp_url(value: &str) -> Option<String> {
    if !clean_tab_text(value) {
        return None;
    }
    let mut url = url::Url::parse(value).ok()?;
    if !matches!(url.scheme(), "ftp" | "ftps") || url.query().is_some() || url.fragment().is_some()
    {
        return None;
    }
    url.host_str()?;
    url.set_password(None).ok()?;
    url.set_username("").ok()?;
    Some(url.into())
}

fn virtual_tc_tab(raw: &str) -> Option<VirtualTab> {
    if let Some(name) = raw
        .get(..8)
        .filter(|prefix| prefix.eq_ignore_ascii_case("FTPOPEN:"))
    {
        let profile = raw[name.len()..].trim();
        return (clean_tab_text(profile) && !profile.is_empty()).then(|| VirtualTab::Ftp {
            profile: profile.into(),
            url: None,
        });
    }
    if let Some((address, profile)) = raw.split_once('\t') {
        let profile = profile.trim();
        return (clean_tab_text(profile) && !profile.is_empty())
            .then_some(())
            .and_then(|()| {
                Some(VirtualTab::Ftp {
                    profile: profile.into(),
                    url: Some(clean_ftp_url(address)?),
                })
            });
    }
    if let Some(network) = raw.strip_prefix(r"\\\") {
        let (plugin, tail) = network.split_once('\\').unwrap_or((network, ""));
        let directory = format!("\\{}", tail.trim_end_matches('\\'));
        return (clean_tab_text(plugin)
            && !plugin.is_empty()
            && !plugin.contains('/')
            && valid_wfx_directory(&directory))
        .then(|| VirtualTab::Wfx {
            plugin: plugin.into(),
            directory,
        });
    }
    let path = Path::new(raw);
    if !clean_tab_text(raw) || !path.is_absolute() || raw.starts_with(r"\\.\") {
        return None;
    }
    let file = path.ancestors().find(|prefix| {
        crate::archive_view::is_supported(prefix)
            && std::fs::symlink_metadata(prefix)
                .is_ok_and(|meta| meta.is_file() && !crate::files::is_link(&meta))
    })?;
    let folder = path.strip_prefix(file).ok()?;
    valid_archive_folder(folder).then(|| VirtualTab::Archive {
        file: file.to_path_buf(),
        folder: folder.to_path_buf(),
    })
}

pub fn write_tc_tabs(path: &Path, panels: &[SavedPanel; 2], source: usize) -> io::Result<()> {
    write_tc_tabs_with_columns(path, panels, source, &BTreeMap::new())
}

pub fn write_tc_tabs_with_columns(
    path: &Path,
    panels: &[SavedPanel; 2],
    source: usize,
    layouts: &BTreeMap<u16, TcColumnLayout>,
) -> io::Result<()> {
    write_tc_tabs_with_views(path, panels, source, layouts, &BTreeMap::new())
}

pub fn write_tc_tabs_with_views(
    path: &Path,
    panels: &[SavedPanel; 2],
    source: usize,
    layouts: &BTreeMap<u16, TcColumnLayout>,
    columns: &BTreeMap<String, Vec<Column>>,
) -> io::Result<()> {
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
    let mut views = PortableViews::default();
    use std::fmt::Write as _;
    for (relative, (section, panel)) in [
        ("activetabs", &panels[source]),
        ("inactivetabs", &panels[1 - source]),
    ]
    .into_iter()
    .enumerate()
    {
        let _ = write!(text, "[{section}]\r\n");
        for (index, folder) in panel.tabs.iter().enumerate() {
            let options = panel.tab_options.get(index);
            if let Some(tab) = options {
                views.record(relative, index, tab, columns);
            }
            let virtual_name = options
                .and_then(|options| options.virtual_tab.as_ref())
                .map(|tab| {
                    tab.tc_path().ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::InvalidData,
                            "Ein Tab enthält einen ungültigen virtuellen Pfad.",
                        )
                    })
                })
                .transpose()?;
            let shell_name = options
                .and_then(|options| options.shell.as_deref())
                .filter(|name| {
                    shell_tab_address(name)
                        || (clean_tab_text(name) && Path::new(name).is_absolute())
                });
            let name = virtual_name
                .as_deref()
                .or(shell_name)
                .or_else(|| {
                    folder
                        .to_str()
                        .filter(|name| clean_tab_text(name) && folder.is_absolute())
                })
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        "Ein Tab enthält einen ungültigen lokalen Ordnerpfad.",
                    )
                })?;
            let _ = write!(text, "{index}_path={name}\r\n");
            let lock = if let Some(options) = options {
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
            let layout = options
                .filter(|tab| tab.view == TabView::Details)
                .and_then(|tab| tab.column_set.as_deref())
                .and_then(|name| layouts.iter().find(|(_, layout)| layout.name == name))
                .and_then(|(id, layout)| Some((id.checked_add(5)?, layout)));
            let view = layout.map_or_else(
                || u16::from(options.map_or(TabView::Details, |tab| tab.view).tc_id()),
                |(id, _)| id,
            );
            let primary = options
                .and_then(|tab| tab.sort.first())
                .copied()
                .unwrap_or_default();
            let custom_index = |key| {
                layout
                    .and_then(|(_, layout)| layout.sort_index(key))
                    .and_then(|index| i32::try_from(index).ok())
            };
            let custom = if matches!(primary.key, SortKey::Name | SortKey::Extension) {
                0
            } else {
                custom_index(primary.key)
                    .and_then(|index| index.checked_add(100))
                    .unwrap_or_default()
            };
            let sort = tc_standard_sort_code(primary.key).unwrap_or_default();
            let reversed = u8::from(
                primary.descending && (custom != 0 || tc_standard_sort_code(primary.key).is_some()),
            );
            let reversed = u8::from(reversed);
            let extra = options
                .into_iter()
                .flat_map(|tab| tab.sort.iter().skip(1))
                .filter_map(|order| {
                    let column = match (layout, order.key) {
                        (_, SortKey::Name) => 1,
                        (_, SortKey::Extension) => 2,
                        (Some(_), key) => custom_index(key)?.checked_add(3)?,
                        (None, SortKey::Size) => 3,
                        (None, SortKey::Modified) => 4,
                        _ => return None,
                    };
                    Some(if order.descending { -column } else { column }.to_string())
                })
                .collect::<Vec<_>>()
                .join(",");
            let colors = options
                .and_then(|tab| tab.view_colors)
                .map(|colors| colors.to_tc());
            let extra = match colors {
                Some(colors) => format!("|{extra}|{colors}"),
                None if extra.is_empty() => extra,
                None => format!("|{extra}"),
            };
            let _ = write!(
                text,
                "{index}_options={view}|{sort}|{reversed}|0|0|{lock}|{custom}{extra}\r\n"
            );
            if let Some(filter) = options
                .map(|tab| tab.file_filter.as_str())
                .filter(|filter| !filter.is_empty())
            {
                validate_tc_tab_filter(filter)?;
                let _ = write!(text, "{index}_filter={filter}\r\n");
            }
        }
        if !panel.tabs.is_empty() {
            let active = panel.active_tab.min(panel.tabs.len() - 1);
            let _ = write!(text, "activetab={active}\r\n");
        }
        text.push_str("\r\n");
    }
    views.write_to(&mut text)?;
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
    filter: String,
}

pub fn validate_tc_tab_filter(filter: &str) -> io::Result<()> {
    if filter.len() > 4096 || filter.chars().any(char::is_control) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Ungültiger Dateitypfilter.",
        ));
    }
    crate::sync::FileMasks::from_tc(filter).map(|_| ())
}

pub fn read_tc_tabs(path: &Path, bases: [&Path; 2], source: usize) -> io::Result<TcTabs> {
    read_tc_tabs_with_variables(path, bases, source, |name| std::env::var_os(name))
}

pub fn read_tc_tabs_with_variables(
    path: &Path,
    bases: [&Path; 2],
    source: usize,
    resolve_variable: impl Fn(&str) -> Option<OsString>,
) -> io::Result<TcTabs> {
    read_tc_tabs_with_columns(path, bases, source, resolve_variable, &BTreeMap::new())
}

pub fn read_tc_tabs_with_columns(
    path: &Path,
    bases: [&Path; 2],
    source: usize,
    resolve_variable: impl Fn(&str) -> Option<OsString>,
    layouts: &BTreeMap<u16, TcColumnLayout>,
) -> io::Result<TcTabs> {
    let text = read_tc_text(path)?;
    let mut sections: [BTreeMap<u32, TcTab>; 2] = Default::default();
    let mut current = [0; 2];
    let mut unhandled_options = 0;
    let source = source.min(1);
    let is_ini = path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("ini"));
    let mut portable_views = if is_ini {
        PortableViews::default()
    } else {
        match PortableViews::read_from(&text, source) {
            Ok(views) => views,
            Err(_) => {
                unhandled_options += 1;
                PortableViews::default()
            }
        }
    };
    let mut portable_positions = [BTreeMap::new(), BTreeMap::new()];
    let section_names = if is_ini {
        [("lefttabs", 0), ("righttabs", 1)]
    } else {
        [("activetabs", source), ("inactivetabs", 1 - source)]
    };
    let alternate =
        ini_value(&text, "configuration", "alternateuserini").filter(|value| !value.is_empty());
    let mut redirect_error = None;
    for (section, side) in section_names {
        let reference = match ini_value(&text, section, "redirectsection") {
            Some("0" | "") => None,
            Some("1") => {
                if alternate.is_none() {
                    unhandled_options += 1;
                }
                alternate
            }
            Some(reference) => Some(reference),
            None if is_ini => alternate,
            None => None,
        };
        let mut redirected = None;
        if let Some(reference) = reference {
            match read_tab_redirect(path, reference, &resolve_variable) {
                Ok(data) if ini_entries(&data).any(|(name, _, _)| name == section) => {
                    redirected = Some(data);
                }
                Ok(_) => {
                    unhandled_options += 1;
                    redirect_error = Some(io::Error::other(format!(
                        "Abschnitt [{section}] fehlt in „{reference}“."
                    )));
                }
                Err(error) => {
                    unhandled_options += 1;
                    redirect_error = Some(io::Error::new(
                        error.kind(),
                        format!("Ausgelagerte Tabs in „{reference}“: {error}"),
                    ));
                }
            }
        }
        let tab_text = redirected.as_deref().unwrap_or(&text);
        for (_, key, value) in ini_entries(tab_text).filter(|(name, _, _)| name == section) {
            if key == "activetab" {
                current[side] = value.parse().unwrap_or_default();
                continue;
            }
            if key == "redirectsection" {
                unhandled_options += usize::from(redirected.is_some());
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
                "filter" => tab.filter = value.into(),
                _ => unhandled_options += 1,
            }
        }
        if is_ini {
            let pane = if side == 0 { "left" } else { "right" };
            if let Some(tab) = active_tc_tab(&text, pane, tab_text, section) {
                // The INI stores the active tab separately. The numbered rows
                // after it are shifted by one, unlike a standalone .tab file.
                let index = current[side];
                let following = sections[side].split_off(&index);
                sections[side].insert(index, tab);
                for (index, tab) in following {
                    if let Some(index) = index.checked_add(1) {
                        sections[side].insert(index, tab);
                    } else {
                        unhandled_options += 1;
                    }
                }
            }
        }
    }
    let mut skipped = 0;
    let panels = std::array::from_fn(|side| {
        let mut panel = SavedPanel::default();
        for (index, tab) in std::mem::take(&mut sections[side]) {
            let raw = tab.path.trim_matches('"');
            let shell = shell_tab_address(raw).then(|| raw.to_owned());
            let virtual_tab = shell.is_none().then(|| virtual_tc_tab(raw)).flatten();
            let path = if raw == "." || shell.is_some() || virtual_tab.is_some() {
                bases[side].to_path_buf()
            } else {
                PathBuf::from(raw)
            };
            if !path.is_absolute()
                || (virtual_tab.is_none()
                    && (raw.starts_with(r"\\.\")
                        || raw.starts_with(r"\\\")
                        || !clean_tab_text(raw)))
            {
                skipped += 1;
                continue;
            }
            let mut fields = tab.options.split('|');
            let embedded_view = !matches!(virtual_tab, Some(VirtualTab::Wfx { .. }))
                && portable_views.contains(side, index);
            let mut view_warnings = 0;
            let view_id = fields.next().filter(|id| !id.is_empty()).unwrap_or("1");
            let layout = view_id
                .parse::<u16>()
                .ok()
                .and_then(|id| layouts.get(&id.checked_sub(5)?));
            let view = if layout.is_some() {
                TabView::Details
            } else {
                match TabView::from_tc_id(view_id) {
                    Some(view) => view,
                    None => {
                        view_warnings += 1;
                        TabView::Details
                    }
                }
            };
            let mut key = match fields
                .next()
                .filter(|value| !value.is_empty())
                .unwrap_or("0")
                .parse()
                .ok()
                .and_then(tc_standard_sort)
            {
                Some(key) => key,
                None => {
                    view_warnings += 1;
                    SortKey::Name
                }
            };
            let descending = match fields.next() {
                Some("1") => true,
                Some("0" | "") | None => false,
                Some(_) => {
                    view_warnings += 1;
                    false
                }
            };
            let lock = match fields.nth(2) {
                Some("1") => TabLock::Fixed,
                Some("2") => TabLock::ReturnToBase,
                Some("0" | "") | None => TabLock::Unlocked,
                Some(_) => {
                    unhandled_options += 1;
                    TabLock::Unlocked
                }
            };
            let custom = fields.next().unwrap_or_default();
            if !matches!(custom, "" | "0") {
                match custom
                    .parse::<usize>()
                    .ok()
                    .and_then(|index| layout?.custom_sort(index.checked_sub(100)?))
                {
                    Some(custom) => key = custom,
                    None => view_warnings += 1,
                }
            }
            let mut sort = vec![SortOrder { key, descending }];
            for value in fields
                .next()
                .unwrap_or_default()
                .split(',')
                .filter(|value| !matches!(*value, "" | "0"))
            {
                let Ok(column) = value.trim().parse::<i32>() else {
                    view_warnings += 1;
                    continue;
                };
                let index = column.unsigned_abs();
                let key = match (layout, index) {
                    (_, 1) => Some(SortKey::Name),
                    (_, 2) => Some(SortKey::Extension),
                    (Some(layout), 3..) => layout.custom_sort(index as usize - 3),
                    (None, 3) => Some(SortKey::Size),
                    (None, 4) => Some(SortKey::Modified),
                    _ => None,
                };
                let Some(key) = key else {
                    view_warnings += 1;
                    continue;
                };
                if !sort.iter().any(|order| order.key == key) {
                    sort.push(SortOrder {
                        key,
                        descending: column < 0,
                    });
                }
            }
            if sort.len() == 1 && sort[0] == SortOrder::default() {
                sort.clear();
            }
            if embedded_view {
                portable_positions[side].insert(index, panel.tabs.len());
            } else {
                unhandled_options += view_warnings;
            }
            let file_filter = if validate_tc_tab_filter(&tab.filter).is_ok() {
                tab.filter
            } else {
                unhandled_options += 1;
                String::new()
            };
            let (view_colors, invalid_colors) = crate::view_settings::ModeColors::from_tc(
                fields.next().unwrap_or_default().split(','),
            );
            if invalid_colors
                || tab
                    .options
                    .split('|')
                    .enumerate()
                    .any(|(index, value)| match index {
                        9 | 10 => !matches!(value, "" | "0" | "-1" | "10001"),
                        3 | 4 | 11.. => !matches!(value, "" | "0"),
                        _ => false,
                    })
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
                view,
                column_set: layout.map(|layout| layout.name.clone()),
                view_colors,
                sort,
                file_filter,
                shell,
                virtual_tab,
                ..SavedTab::default()
            });
        }
        panel
    });
    if panels.iter().all(|panel| panel.tabs.is_empty()) {
        if let Some(error) = redirect_error {
            return Err(error);
        }
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Die Datei enthält keine importierbaren Ordner-Tabs.",
        ));
    }
    portable_views.retain_tabs(portable_positions);
    Ok(TcTabs {
        panels,
        skipped,
        unhandled_options,
        portable_views,
    })
}

fn active_tc_tab(data: &str, pane: &str, tabs: &str, section: &str) -> Option<TcTab> {
    let setting = |key| ini_value(data, pane, key).unwrap_or_default();
    let tab_setting = |key| ini_value(tabs, section, key).unwrap_or_default();
    let path = setting("path");
    if path.is_empty() {
        return None;
    }
    let view = match setting("showalldetails") {
        "0" => Some(0),
        "" | "1" => match setting("specialview") {
            "" | "0" => Some(1),
            other => other
                .parse::<u16>()
                .ok()
                .and_then(|value| value.checked_add(3)),
        },
        _ => None,
    };
    let view = view.map_or_else(|| "?".into(), |view| view.to_string());
    let colors = [
        tab_setting("activetabcolor"),
        setting("activepanelcolor"),
        setting("activepanelcolor2"),
        setting("activepanelcolordark"),
        setting("activepanelcolordark2"),
    ]
    .join(",");
    Some(TcTab {
        path: path.into(),
        title: tab_setting("activecaption").into(),
        options: format!(
            "{view}|{}|{}|0|0|{}|{}|{}|{colors}|{}|{}",
            setting("sortorder"),
            setting("negative sortorder"),
            tab_setting("activelocked"),
            setting("customsortorder"),
            setting("addsortorders"),
            setting("viewmode"),
            setting("autoviewmode"),
        ),
        filter: match setting("show") {
            "2" => "*.exe;*.com;*.bat;*.cmd".into(),
            "3" => setting("userspec").into(),
            _ => String::new(),
        },
    })
}

pub(crate) fn tc_standard_sort(code: i32) -> Option<SortKey> {
    Some(match code {
        0 => SortKey::Name,
        1 => SortKey::Extension,
        2 => SortKey::Size,
        3 => SortKey::Modified,
        4 => SortKey::Unsorted,
        _ => return None,
    })
}

fn tc_standard_sort_code(key: SortKey) -> Option<i32> {
    match key {
        SortKey::Name => Some(0),
        SortKey::Extension => Some(1),
        SortKey::Size => Some(2),
        SortKey::Modified => Some(3),
        SortKey::Unsorted => Some(4),
        _ => None,
    }
}

fn ini_value<'a>(data: &'a str, section: &str, key: &str) -> Option<&'a str> {
    ini_entries(data)
        .filter(|(name, field, _)| name == section && field == key)
        .map(|(_, _, value)| value)
        .last()
}

fn read_tab_redirect(
    source: &Path,
    reference: &str,
    resolve_variable: &impl Fn(&str) -> Option<OsString>,
) -> io::Result<String> {
    let file = tc_related_file(source, reference, &["ini", "tab"], resolve_variable)?;
    if fs::canonicalize(&file)? == fs::canonicalize(source)? {
        return Err(io::Error::other(
            "Ein Tab-Abschnitt verweist auf seine eigene Datei.",
        ));
    }
    read_tc_text(&file)
}

pub fn tc_related_file(
    source: &Path,
    reference: &str,
    extensions: &[&str],
    resolve_variable: &impl Fn(&str) -> Option<OsString>,
) -> io::Result<PathBuf> {
    let parent = source.parent().ok_or(io::ErrorKind::InvalidInput)?;
    let raw = reference.trim().trim_matches('"');
    let expanded = expand_tc_path(raw, resolve_variable).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "Eine Pfadvariable konnte nicht aufgelöst werden.",
        )
    })?;
    let file = parent.join(expanded);
    if !clean_tab_text(raw)
        || !file.is_absolute()
        || !file.extension().is_some_and(|ext| {
            extensions
                .iter()
                .any(|allowed| ext.eq_ignore_ascii_case(allowed))
        })
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Die referenzierte Datei hat einen ungültigen Pfad oder Dateityp.",
        ));
    }
    let file = std::path::absolute(file)?;
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use std::path::{Component, Prefix};
        use windows_sys::Win32::Storage::FileSystem::GetDriveTypeW;
        let Some(Component::Prefix(prefix)) = file.components().next() else {
            return Err(io::ErrorKind::InvalidInput.into());
        };
        if !matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_)) {
            return Err(io::Error::other(
                "Die referenzierte Datei benötigt ein lokales Laufwerk.",
            ));
        }
        let root: Vec<_> = prefix
            .as_os_str()
            .encode_wide()
            .chain([b'\\' as u16, 0])
            .collect();
        // The drive root is NUL-terminated and lives through the call.
        #[allow(unsafe_code)]
        let drive_type = unsafe { GetDriveTypeW(root.as_ptr()) };
        if drive_type == 4 {
            return Err(io::Error::other(
                "Die referenzierte Datei benötigt ein lokales Laufwerk.",
            ));
        }
        let parents: Vec<_> = file.ancestors().skip(1).collect();
        for parent in parents.into_iter().rev() {
            if crate::files::is_link(&fs::symlink_metadata(parent)?) {
                return Err(io::Error::other(
                    "Der referenzierte Pfad darf nicht über Verzeichnislinks führen.",
                ));
            }
        }
    }
    let metadata = fs::symlink_metadata(&file)?;
    if !metadata.is_file() || crate::files::is_link(&metadata) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Die referenzierte Datei muss eine reguläre Datei sein.",
        ));
    }
    Ok(file)
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
                        view: TabView::Thumbnails,
                        view_colors: Some(crate::view_settings::ModeColors {
                            tab: Some(0xff0000),
                            light: [Some(0xfefefe), None],
                            dark: [None, Some(0x000080)],
                        }),
                        sort: vec![SortOrder {
                            key: SortKey::Modified,
                            descending: true,
                        }],
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
        assert_eq!(loaded.unhandled_options, 0);
        assert_eq!(loaded.panels[0].tabs, panels[0].tabs);
        assert_eq!(loaded.panels[1].tabs, panels[1].tabs);
        assert_eq!(loaded.panels[1].active_tab, 1);
        assert_eq!(loaded.panels[1].tab_options, panels[1].tab_options);
    }

    #[cfg(windows)]
    #[test]
    fn arco_tabs_restore_native_columns_without_a_tc_profile() {
        use crate::view_settings::{Field, TcColumnExpression};

        let fixture = tempfile::tempdir().unwrap();
        let export = fixture.path().join("Portable.tab");
        let definition = vec![Column {
            field: Field::TcExpression(TcColumnExpression::parse("[=tc.size.kbytes] KB").unwrap()),
            title: "Größe".into(),
            width: 110,
            right_aligned: Some(true),
        }];
        let sort = vec![SortOrder {
            key: SortKey::Size,
            descending: true,
        }];
        let panels = [
            SavedPanel::default(),
            SavedPanel {
                tabs: vec![PathBuf::from(r"C:\Projekte")],
                tab_options: vec![SavedTab {
                    column_set: Some("Meine Größen".into()),
                    sort: sort.clone(),
                    ..SavedTab::default()
                }],
                ..SavedPanel::default()
            },
        ];
        let definitions = BTreeMap::from([("Meine Größen".into(), definition.clone())]);
        write_tc_tabs_with_views(&export, &panels, 1, &BTreeMap::new(), &definitions).unwrap();
        let text = read_tc_text(&export).unwrap();
        assert!(text.contains("0_options=1|2|1|0|0|0|0"));
        assert!(text.contains("[ArcoTabViews]"));

        let mut profile_columns = BTreeMap::from([(
            "Meine Größen".into(),
            vec![Column {
                field: Field::Comment,
                title: "Kommentar".into(),
                width: 100,
                right_aligned: None,
            }],
        )]);
        let bases = [Path::new(r"C:\"), Path::new(r"C:\")];
        for _ in 0..2 {
            let mut imported = read_tc_tabs(&export, bases, 0).unwrap();
            assert_eq!(imported.unhandled_options, 0);
            assert_eq!(imported.embedded_column_count(), 1);
            imported
                .install_embedded_columns(&mut profile_columns)
                .unwrap();
            let tab = &imported.panels[0].tab_options[0];
            assert_eq!(tab.column_set.as_deref(), Some("Meine Größen (Arco 1)"));
            assert_eq!(tab.sort, sort);
            assert_eq!(profile_columns["Meine Größen (Arco 1)"], definition);
            assert_eq!(profile_columns.len(), 2);
        }
    }

    #[cfg(windows)]
    #[test]
    fn arco_tab_import_rejects_untrusted_plugin_columns_and_keeps_standard_tabs() {
        let fixture = tempfile::tempdir().unwrap();
        let source = fixture.path().join("External.tab");
        let untrusted = serde_json::json!({
            "sets": {"External": [{
                "field": {"wdx": {"plugin": "C:/fake.wdx64", "selection": {"field": "Field"}}},
                "title": "External", "width": 90
            }]},
            "tabs": [{"0": {"columns": "External", "sort": []}}, {}]
        });
        fs::write(&source, format!("[activetabs]\n0_path=C:\\Docs\n0_options=0|3|0|0|0|2|0\n[ArcoTabViews]\nV1={untrusted}\n")).unwrap();
        let mut imported =
            read_tc_tabs(&source, [Path::new(r"C:\"), Path::new(r"C:\")], 0).unwrap();
        assert_eq!(imported.unhandled_options, 1);
        assert_eq!(imported.embedded_column_count(), 0);
        let mut columns = BTreeMap::new();
        imported.install_embedded_columns(&mut columns).unwrap();
        let tab = &imported.panels[0].tab_options[0];
        assert_eq!(tab.view, TabView::Short);
        assert_eq!(tab.lock, TabLock::ReturnToBase);
        assert!(columns.is_empty());
    }

    #[cfg(windows)]
    #[test]
    fn tc_tab_round_trip_keeps_custom_columns_and_original_sort_indices() {
        let fixture = tempfile::tempdir().unwrap();
        let source = fixture.path().join("Original.tab");
        fs::write(
            &source,
            "[activetabs]\n0_path=C:\\Medien\n0_options=6|0|1|0|0|0|102|3,1\nactivetab=0\n",
        )
        .unwrap();
        let layouts = BTreeMap::from([(
            1,
            TcColumnLayout {
                name: "Medien".into(),
                sort_keys: vec![Some(SortKey::Comment), None, Some(SortKey::Wdx(0))],
            },
        )]);
        let bases = [Path::new(r"C:\"), Path::new(r"C:\")];
        let loaded = read_tc_tabs_with_columns(&source, bases, 0, |_| None, &layouts).unwrap();
        assert_eq!(loaded.unhandled_options, 0);
        let tab = &loaded.panels[0].tab_options[0];
        assert_eq!(tab.column_set.as_deref(), Some("Medien"));
        assert_eq!(
            tab.sort,
            [
                SortOrder {
                    key: SortKey::Wdx(0),
                    descending: true
                },
                SortOrder {
                    key: SortKey::Comment,
                    descending: false
                },
                SortOrder {
                    key: SortKey::Name,
                    descending: false
                },
            ]
        );
        let export = fixture.path().join("Export.tab");
        write_tc_tabs_with_columns(&export, &loaded.panels, 0, &layouts).unwrap();
        assert!(
            read_tc_text(&export)
                .unwrap()
                .contains("0_options=6|0|1|0|0|0|102|3,1")
        );
        let restored = read_tc_tabs_with_columns(&export, bases, 0, |_| None, &layouts).unwrap();
        assert_eq!(restored.panels[0].tab_options, loaded.panels[0].tab_options);
    }

    #[cfg(windows)]
    #[test]
    fn tc_ini_inserts_the_active_pane_between_its_saved_tabs() {
        let fixture = tempfile::tempdir().unwrap();
        let file = fixture.path().join("wincmd.ini");
        fs::write(&file, concat!(
            "[left]\npath=C:\\Images\nShowAllDetails=1\nSpecialView=2\n",
            "SortOrder=3\nNegative Sortorder=1\nshow=3\nuserspec=*.jpg\nActivePanelColorDark=128\n",
            "[lefttabs]\n0_path=C:\\First\n1_path=C:\\Last\n",
            "activetab=1\nactivecaption=Fotos\nactivelocked=2\nactivetabcolor=255\n",
        )).unwrap();
        let imported = read_tc_tabs(&file, [Path::new(r"C:\"), Path::new(r"C:\")], 0).unwrap();
        let left = &imported.panels[0];
        assert_eq!(
            left.tabs,
            [r"C:\First", r"C:\Images", r"C:\Last"].map(PathBuf::from)
        );
        assert_eq!(left.active_tab, 1);
        let tab = &left.tab_options[1];
        assert_eq!(tab.title, "Fotos");
        assert_eq!(tab.lock, TabLock::ReturnToBase);
        assert_eq!(tab.view, TabView::Thumbnails);
        assert_eq!(tab.file_filter, "*.jpg");
        assert_eq!(
            tab.sort,
            [SortOrder {
                key: SortKey::Modified,
                descending: true
            }]
        );
        let colors = tab.view_colors.unwrap();
        assert_eq!(colors.tab, Some(0xff0000));
        assert_eq!(colors.dark[0], Some(0x800000));
        assert_eq!(imported.unhandled_options, 0);
    }

    #[cfg(windows)]
    #[test]
    fn tc_tabs_read_external_sections_without_mixing_inline_or_nested_redirects() {
        let fixture = tempfile::tempdir().unwrap();
        let base = fixture.path();
        let source = base.join("wincmd.ini");
        let alternate = base.join("shared.ini");
        let extra = base.join("Tabs links.tab");
        fs::write(&source, "[Configuration]\nAlternateUserIni=shared.ini\n[LeftTabs]\n0_path=C:\\Old\nRedirectSection=\"%COMMANDER_INI_PATH%\\Tabs links.tab\"\n[RightTabs]\nRedirectSection=0\n0_path=C:\\Right\n").unwrap();
        fs::write(
            &alternate,
            "[lefttabs]\n0_path=C:\\SharedLeft\n[righttabs]\n0_path=C:\\SharedRight\n",
        )
        .unwrap();
        fs::write(&extra, "[lefttabs]\nredirectsection=missing.ini\n5_path=C:\\Left\n5_caption=Übernommen\n5_options=0|2|1|0|0|1|0\nactivetab=5\n").unwrap();
        let imported = read_tc_tabs_with_variables(&source, [base, base], 1, |name| {
            (name == "COMMANDER_INI_PATH").then(|| base.as_os_str().to_owned())
        })
        .unwrap();
        assert_eq!(imported.panels[0].tabs, [PathBuf::from(r"C:\Left")]);
        assert_eq!(imported.panels[1].tabs, [PathBuf::from(r"C:\Right")]);
        assert_eq!(imported.panels[0].tab_options[0].title, "Übernommen");
        assert_eq!(imported.panels[0].tab_options[0].lock, TabLock::Fixed);
        assert_eq!(imported.unhandled_options, 1);

        fs::write(
            &source,
            "[configuration]\nAlternateUserIni=shared.ini\n[lefttabs]\nRedirectSection=1\n",
        )
        .unwrap();
        let imported = read_tc_tabs(&source, [base, base], 0).unwrap();
        assert_eq!(imported.panels[0].tabs, [PathBuf::from(r"C:\SharedLeft")]);
        assert_eq!(imported.panels[1].tabs, [PathBuf::from(r"C:\SharedRight")]);
        assert_eq!(imported.unhandled_options, 0);

        let tab = base.join("project.tab");
        fs::write(&tab, "[activetabs]\nRedirectSection=project.tab\n").unwrap();
        assert!(
            read_tc_tabs(&tab, [base, base], 0)
                .err()
                .unwrap()
                .to_string()
                .contains("eigene Datei")
        );
        fs::write(
            &tab,
            "[activetabs]\nRedirectSection=missing.tab\n[inactivetabs]\n0_path=C:\\Valid\n",
        )
        .unwrap();
        let imported = read_tc_tabs(&tab, [base, base], 0).unwrap();
        assert_eq!(imported.panels[1].tabs, [PathBuf::from(r"C:\Valid")]);
        assert_eq!(imported.unhandled_options, 1);
    }

    #[cfg(windows)]
    #[test]
    fn tc_tabs_round_trip_virtual_locations_without_credentials() {
        let fixture = tempfile::tempdir().unwrap();
        let base = fixture.path();
        let archive = base.join("Dokumente.zip");
        fs::write(&archive, b"").unwrap();
        let file = base.join("Virtuell.tab");
        let shell = "::{20D04FE0-3AEA-1069-A2D8-08002B30309D}";
        fs::write(&file, format!(
            "[activetabs]\n0_path={}\\Berichte\n1_path=\\\\\\SFTP\\Projekte\n2_path=ftp://alice:secret@example.org/folder/\tTestserver\nactivetab=2\n[inactivetabs]\n0_path={shell}\n1_path=FTPOPEN:Testserver\n",
            archive.display(),
        )).unwrap();
        let imported = read_tc_tabs(&file, [base, base], 0).unwrap();
        assert_eq!(imported.skipped, 0);
        assert_eq!(imported.panels[0].active_tab, 2);
        let options = &imported.panels[0].tab_options;
        assert_eq!(
            options[0].virtual_tab,
            Some(VirtualTab::Archive {
                file: archive,
                folder: "Berichte".into(),
            })
        );
        assert_eq!(
            options[1].virtual_tab,
            Some(VirtualTab::Wfx {
                plugin: "SFTP".into(),
                directory: r"\Projekte".into(),
            })
        );
        assert_eq!(
            options[2].virtual_tab,
            Some(VirtualTab::Ftp {
                profile: "Testserver".into(),
                url: Some("ftp://example.org/folder/".into()),
            })
        );
        assert_eq!(
            imported.panels[1].tab_options[0].shell.as_deref(),
            Some(shell)
        );
        assert_eq!(
            imported.panels[1].tab_options[1].virtual_tab,
            Some(VirtualTab::Ftp {
                profile: "Testserver".into(),
                url: None,
            })
        );
        let exported = base.join("Export.tab");
        write_tc_tabs(&exported, &imported.panels, 0).unwrap();
        assert!(!read_tc_text(&exported).unwrap().contains("alice:secret"));
        let restored = read_tc_tabs(&exported, [base, base], 0).unwrap();
        assert_eq!(restored.unhandled_options, 0);
        for side in 0..2 {
            assert_eq!(
                restored.panels[side].tab_options,
                imported.panels[side].tab_options
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn tc_tabs_preserve_both_roles_captions_and_lock_modes() {
        let fixture = tempfile::tempdir().unwrap();
        let file = fixture.path().join("Projekt.tab");
        let text = "[activeTabs]\r\n0_path=C:\\Workspace\r\n0_caption=Übersicht\r\n0_options=0|0|0|0|0|1|0|2,-4\r\n5_path=ftp://invalid/\r\n8_path=.\r\n8_options=0|0|0|0|0|2|0\r\nactivetab=8\r\n[inactiveTabs]\r\n3_path=\\\\server\\share\\ordner\r\n3_filter=*.txt *.md | alt*\r\n";
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
        assert_eq!(tabs.unhandled_options, 0);
        let tab = &tabs.panels[0].tab_options[0];
        assert_eq!(tab.file_filter, "*.txt *.md | alt*");
        let filter = crate::sync::FileMasks::from_tc(&tab.file_filter).unwrap();
        assert!(filter.includes(Path::new("neu.md")));
        assert!(!filter.includes(Path::new("alt.txt")));
        assert_eq!(
            tabs.panels[1].tab_options[0].sort,
            [
                SortOrder::default(),
                SortOrder {
                    key: SortKey::Extension,
                    descending: false
                },
                SortOrder {
                    key: SortKey::Modified,
                    descending: true
                },
            ]
        );
        let exported = fixture.path().join("Rundlauf.tab");
        write_tc_tabs(&exported, &tabs.panels, 1).unwrap();
        let restored = read_tc_tabs(
            &exported,
            [Path::new(r"C:\left"), Path::new(r"C:\right")],
            1,
        )
        .unwrap();
        assert_eq!(restored.panels[0].tab_options, tabs.panels[0].tab_options);
        assert_eq!(restored.panels[1].tab_options, tabs.panels[1].tab_options);
    }

    #[test]
    fn tc_import_keeps_last_case_insensitive_bindings_and_windows_chords_separate() {
        let file = NamedTempFile::new().unwrap();
        fs::write(file.path(), "[Shortcuts]\nA+F1=cm_RereadSource\na+f1=cm_LeftOpenDrives\n[ShortcutsWin]\nF1=cm_SearchFor\n").unwrap();
        let profile = read_tc_ini(file.path(), |_| None).unwrap();
        assert_eq!(
            profile.shortcuts,
            BTreeMap::from([
                ("A+F1".into(), "cm_LeftOpenDrives".into()),
                ("WIN+F1".into(), "cm_SearchFor".into()),
            ])
        );
        assert_eq!(profile.skipped, 0);
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
    pub view_colors: Option<crate::view_settings::ModeColors>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column_set: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub sort: Vec<crate::files::SortOrder>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub file_filter: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shell: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub virtual_tab: Option<VirtualTab>,
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

impl TabView {
    pub(crate) fn from_tc_id(id: &str) -> Option<Self> {
        Some(match id {
            "0" => Self::Short,
            "1" => Self::Details,
            "4" => Self::Comments,
            "5" => Self::Thumbnails,
            _ => return None,
        })
    }

    fn tc_id(self) -> u8 {
        match self {
            Self::Short => 0,
            Self::Details => 1,
            Self::Comments => 4,
            Self::Thumbnails => 5,
        }
    }
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

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Language {
    #[default]
    German,
    English,
    French,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Profile {
    #[serde(skip_serializing_if = "WindowPlacement::is_empty")]
    pub window_placement: WindowPlacement,
    version: u32,
    pub panels: [SavedPanel; 2],
    pub favorites: Vec<PathBuf>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub directory_favorites: Vec<DirectoryFavorite>,
    pub active_panel: usize,
    pub language: Language,
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

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct WindowPlacement {
    pub display: String,
    pub displays: BTreeMap<String, DisplayPlacement>,
}

impl WindowPlacement {
    fn is_empty(&self) -> bool {
        self.displays.is_empty()
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct DisplayPlacement {
    pub offset: [i32; 2],
    pub size: [u32; 2],
    pub maximized: bool,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extension: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search: Option<crate::rename::SearchOptions>,
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
            window_placement: WindowPlacement::default(),
            version: PROFILE_VERSION,
            panels: Default::default(),
            favorites: Vec::new(),
            directory_favorites: Vec::new(),
            active_panel: 0,
            language: Language::default(),
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        working_directory: Option<PathBuf>,
        #[serde(default)]
        window: ProgramWindow,
        #[serde(default)]
        tc_arguments: bool,
        #[serde(default)]
        prompt_arguments: bool,
    },
    Menu {
        buttons: Vec<ButtonbarButton>,
    },
    SwitchBar {
        bar: Option<usize>,
        buttons: Option<Vec<ButtonbarButton>>,
    },
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProgramWindow {
    #[default]
    Normal,
    Minimized,
    Maximized,
}

#[derive(Default)]
pub struct TcButtonbarEntry {
    pub command: String,
    pub parameters: String,
    pub working_directory: String,
    pub icon: String,
    pub label: String,
    pub iconic: String,
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
                iconic: value("iconic"),
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
