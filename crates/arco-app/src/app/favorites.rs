use super::{App, Navigation};
use crate::commands::{Dialog, Pending};
use crate::display_path;
use arco_core::profile::DirectoryFavorite;
use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};

pub(crate) enum Choice {
    Open(DirectoryFavorite),
    Group(Vec<String>),
    Edit,
}

pub(crate) enum Setup {
    Browse(Vec<Choice>),
    Manage(Vec<DirectoryFavorite>),
    Change(Vec<DirectoryFavorite>, usize),
    Edit(Vec<DirectoryFavorite>, Option<usize>),
}

impl App {
    fn favorites(&self) -> Vec<DirectoryFavorite> {
        let mut entries = self.profile.directory_favorites.clone();
        entries.extend(
            self.profile
                .favorites
                .iter()
                .filter(|path| path.is_absolute())
                .map(|path| DirectoryFavorite {
                    title: String::new(),
                    groups: Vec::new(),
                    path: path.clone(),
                    other: None,
                }),
        );
        entries
    }

    pub(crate) fn favorite_action(&mut self, command: &str) -> bool {
        match command {
            "favorites" => self.browse_favorites(Vec::new()),
            "favorites-manage" => self.manage_favorites(),
            "add-favorite" => {
                if !self.current().is_local() {
                    self.status("Wähle einen lokalen Ordner für die Favoriten.");
                } else {
                    self.edit_favorite(self.favorites(), None);
                }
            }
            _ => return false,
        }
        true
    }

    fn browse_favorites(&mut self, group: Vec<String>) {
        let entries = self.favorites();
        let mut labels = Vec::new();
        let mut choices = Vec::new();
        if !group.is_empty() {
            labels.push("Eine Ebene nach oben".to_owned());
            choices.push(Choice::Group(group[..group.len() - 1].to_vec()));
        }
        let mut subgroups = BTreeSet::new();
        for entry in entries {
            if !entry.groups.starts_with(&group) {
                continue;
            }
            if let Some(child) = entry.groups.get(group.len()) {
                if subgroups.insert(child.clone()) {
                    let mut next = group.clone();
                    next.push(child.clone());
                    labels.push(format!("{child}  ›"));
                    choices.push(Choice::Group(next));
                }
            } else {
                labels.push(label(&entry));
                choices.push(Choice::Open(entry));
            }
        }
        labels.push("Favoriten verwalten …".into());
        choices.push(Choice::Edit);
        let options: Vec<_> = labels.iter().map(String::as_str).collect();
        let title = if group.is_empty() {
            "Ordnerfavoriten".into()
        } else {
            group.join(" › ")
        };
        self.dialog(
            "confirm",
            Pending::Favorites(Setup::Browse(choices)),
            Dialog {
                title: &title,
                options: &options,
                ..Default::default()
            },
        );
    }

    fn manage_favorites(&mut self) {
        let entries = self.favorites();
        let labels: Vec<_> = std::iter::once("Neuer Favorit".into())
            .chain(entries.iter().map(|entry| {
                let name = label(entry);
                if entry.groups.is_empty() {
                    name
                } else {
                    format!("{} › {name}", entry.groups.join(" › "))
                }
            }))
            .collect();
        let options: Vec<_> = labels.iter().map(String::as_str).collect();
        self.dialog(
            "confirm",
            Pending::Favorites(Setup::Manage(entries)),
            Dialog {
                title: "Ordnerfavoriten verwalten",
                options: &options,
                ..Default::default()
            },
        );
    }

    fn edit_favorite(&mut self, entries: Vec<DirectoryFavorite>, index: Option<usize>) {
        let value = if let Some(entry) = index.and_then(|index| entries.get(index)) {
            format!(
                "{}\n{}\n{}\n{}",
                entry.title,
                entry.groups.join(" / "),
                display_path(&entry.path),
                entry.other.as_deref().map(display_path).unwrap_or_default()
            )
        } else {
            let path = &self.current().path;
            format!(
                "{}\n\n{}\n",
                path.file_name().map_or_else(
                    || display_path(path),
                    |name| name.to_string_lossy().into_owned()
                ),
                display_path(path)
            )
        };
        self.dialog("multiline", Pending::Favorites(Setup::Edit(entries, index)), Dialog {
            title: "Ordnerfavorit bearbeiten",
            description: "Je Zeile Titel, Gruppen (mit / getrennt), absoluter Ordner und optionaler Ordner für die Gegenseite. Eine leere Gruppenzeile platziert den Favoriten auf der obersten Ebene.",
            value: &value, ..Default::default()
        });
    }

    pub(crate) fn submit_favorite(
        &mut self,
        setup: Setup,
        value: &str,
        option: usize,
    ) -> io::Result<()> {
        match setup {
            Setup::Browse(choices) => match choices.into_iter().nth(option) {
                Some(Choice::Group(group)) => self.browse_favorites(group),
                Some(Choice::Open(entry)) => {
                    self.load(self.active, entry.path, Navigation::Jump);
                    if let Some(other) = entry.other {
                        self.load(1 - self.active, other, Navigation::Jump);
                    }
                }
                Some(Choice::Edit) => self.manage_favorites(),
                None => (),
            },
            Setup::Manage(entries) if option == 0 => self.edit_favorite(entries, None),
            Setup::Manage(entries) => {
                if let Some(entry) = entries.get(option - 1) {
                    let title = label(entry);
                    self.dialog(
                        "confirm",
                        Pending::Favorites(Setup::Change(entries, option - 1)),
                        Dialog {
                            title: &title,
                            options: &["Bearbeiten", "Nach oben", "Nach unten", "Entfernen"],
                            ..Default::default()
                        },
                    );
                }
            }
            Setup::Change(entries, index) if option == 0 => {
                self.edit_favorite(entries, Some(index))
            }
            Setup::Change(mut entries, index) => {
                if index < entries.len() {
                    match option {
                        1 if index > 0 => entries.swap(index, index - 1),
                        2 if index + 1 < entries.len() => entries.swap(index, index + 1),
                        3 => {
                            entries.remove(index);
                        }
                        _ => (),
                    }
                    self.save_favorites(entries)?;
                }
                self.manage_favorites();
            }
            Setup::Edit(mut entries, index) => {
                let entry = parse_favorite(value)?;
                if let Some(index) = index {
                    *entries
                        .get_mut(index)
                        .ok_or_else(|| io::Error::other("Der Favorit fehlt."))? = entry;
                } else if !entries.contains(&entry) {
                    if entries.len() >= 4096 {
                        return Err(io::Error::other(
                            "Es können höchstens 4.096 Favoriten gespeichert werden.",
                        ));
                    }
                    entries.push(entry);
                }
                self.save_favorites(entries)?;
                self.status("Ordnerfavorit gespeichert.");
            }
        }
        Ok(())
    }

    pub(crate) fn import_favorites(&mut self, entries: Vec<DirectoryFavorite>) {
        for entry in entries {
            if !self.profile.directory_favorites.contains(&entry) {
                self.profile.directory_favorites.push(entry);
            }
        }
    }

    fn save_favorites(&mut self, entries: Vec<DirectoryFavorite>) -> io::Result<()> {
        if !self.save_profile {
            return Err(io::Error::other("Das Arco-Profil ist nicht verfügbar."));
        }
        let mut profile = self.profile_snapshot();
        profile.directory_favorites = entries;
        profile.favorites.clear();
        profile.save(&self.profile_path)?;
        self.profile = profile;
        Ok(())
    }
}

pub(crate) fn label(entry: &DirectoryFavorite) -> String {
    if entry.title.is_empty() {
        display_path(&entry.path)
    } else {
        entry.title.clone()
    }
}

fn parse_favorite(value: &str) -> io::Result<DirectoryFavorite> {
    let mut lines = value.lines().map(str::trim);
    let title = lines.next().unwrap_or_default();
    let groups = lines.next().unwrap_or_default();
    let path = PathBuf::from(lines.next().unwrap_or_default().trim_matches('"'));
    let other = lines
        .next()
        .filter(|line| !line.is_empty())
        .map(|line| PathBuf::from(line.trim_matches('"')));
    let groups: Vec<_> = groups
        .split('/')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(str::to_owned)
        .collect();
    if title.chars().any(char::is_control)
        || title.chars().count() > 120
        || groups.len() > 32
        || groups
            .iter()
            .any(|group| group.chars().count() > 120 || group.chars().any(char::is_control))
        || !path.is_absolute()
        || other.as_ref().is_some_and(|path| !path.is_absolute())
        || lines.any(|line| !line.is_empty())
    {
        return Err(io::Error::other(
            "Favoriten benötigen einen absoluten Ordnerpfad und höchstens 32 Gruppenebenen.",
        ));
    }
    Ok(DirectoryFavorite {
        title: title.into(),
        groups,
        path,
        other,
    })
}
pub(crate) fn tc_profile(path: &Path) -> io::Result<arco_core::profile::TcImport> {
    fn executable(directory: &Path) -> Option<PathBuf> {
        ["TOTALCMD64.EXE", "TOTALCMD.EXE"]
            .into_iter()
            .map(|name| directory.join(name))
            .find(|path| path.is_file())
    }

    let directory = path
        .parent()
        .filter(|directory| executable(directory).is_some())
        .map(Path::to_path_buf)
        .or_else(|| {
            std::env::var_os("COMMANDER_PATH")
                .map(PathBuf::from)
                .filter(|directory| directory.is_absolute())
        })
        .or_else(arco_windows::total_commander_install);
    let exe = directory.as_deref().and_then(executable);
    arco_core::profile::read_tc_ini(path, |name| {
        let special = match name.to_ascii_uppercase().as_str() {
            "COMMANDER_PATH" => directory.as_deref(),
            "COMMANDER_INI" => Some(path),
            "COMMANDER_INI_PATH" => path.parent(),
            "COMMANDER_EXE" => exe.as_deref(),
            "COMMANDER_DRIVE" => {
                let prefix = directory.as_deref()?.components().next()?;
                return match prefix {
                    std::path::Component::Prefix(prefix) => Some(prefix.as_os_str().to_owned()),
                    _ => None,
                };
            }
            _ if name.starts_with('$') => {
                return arco_windows::tc_favorite_folder(&name.to_ascii_uppercase())
                    .map(PathBuf::into_os_string);
            }
            _ => return std::env::var_os(name),
        };
        special.map(|value| value.as_os_str().to_owned())
    })
}
