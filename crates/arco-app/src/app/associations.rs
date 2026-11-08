use super::{App, Outcome};
use crate::commands::{Dialog, Pending};
use crate::display_path;
use arco_core::files::{self, Kind};
use arco_core::profile::FileAssociation;
use arco_core::sync::FileMasks;
use std::ffi::{OsStr, OsString};
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

pub(crate) enum Setup {
    Choose,
    Manage(usize),
    Edit(Option<usize>),
    Verbs(PathBuf, Vec<FileAssociation>),
    Shortcut { source: PathBuf, directory: PathBuf },
}

impl App {
    pub(crate) fn open_internal_association(&self, path: &Path) -> Option<io::Result<()>> {
        self.profile
            .associations
            .iter()
            .find(|entry| entry.default_open && matches_path(entry, path))
            .map(|entry| run(entry, path))
    }

    pub(crate) fn association_action(&mut self, command: &str) -> bool {
        if command == "associations" {
            self.choose_association();
            return true;
        }
        if !matches!(
            command,
            "internal-verbs"
                | "system-open"
                | "open-with"
                | "run-admin"
                | "run-other-user"
                | "create-shortcut"
        ) {
            return false;
        }
        if !self.current().is_local() || self.panels[self.active].loading {
            self.status("Wähle einen Eintrag in einem lokalen Dateifenster.");
            return true;
        }
        let Some(entry) = self.current().current() else {
            self.status("Wähle einen Eintrag.");
            return true;
        };
        let path = entry.path.clone();
        if command == "create-shortcut" {
            let other = self.panels[1 - self.active].current();
            let directory = if other.is_local() {
                other.path.clone()
            } else {
                self.current().path.clone()
            };
            let destination = display_path(&directory);
            let label = format!(
                "{}.lnk",
                path.file_name().unwrap_or_default().to_string_lossy()
            );
            self.dialog(
                "input",
                Pending::Associations(Setup::Shortcut {
                    source: path,
                    directory,
                }),
                Dialog {
                    title: "Windows-Verknüpfung erstellen",
                    description: &destination,
                    value: &label,
                    placeholder: "Name der Verknüpfung",
                    ..Default::default()
                },
            );
            return true;
        }
        if entry.kind == Kind::Directory && command != "system-open" {
            self.status("Wähle eine Datei.");
            return true;
        }
        match command {
            "internal-verbs" => self.choose_internal_verb(path),
            "system-open" => self.report(arco_windows::open(&path)),
            "open-with" => self.report(arco_windows::execute_verb(&path, "openas")),
            _ => {
                if !path.extension().is_some_and(|extension| {
                    ["exe", "lnk"]
                        .iter()
                        .any(|value| extension.eq_ignore_ascii_case(value))
                }) {
                    self.status("Diese Aktion benötigt eine EXE-Datei oder Windows-Verknüpfung.");
                } else {
                    let verb = if command == "run-admin" {
                        "runas"
                    } else {
                        "runasuser"
                    };
                    self.report(arco_windows::execute_verb(&path, verb));
                }
            }
        }
        true
    }

    fn choose_internal_verb(&mut self, path: PathBuf) {
        let matches: Vec<_> = self
            .profile
            .associations
            .iter()
            .filter(|entry| matches_path(entry, &path))
            .cloned()
            .collect();
        if matches.is_empty() {
            self.status("Für diese Datei ist keine interne Aktion eingerichtet.");
            return;
        }
        let options: Vec<_> = matches.iter().map(|entry| entry.name.clone()).collect();
        let labels: Vec<_> = options.iter().map(String::as_str).collect();
        let description = display_path(&path);
        self.dialog(
            "confirm",
            Pending::Associations(Setup::Verbs(path, matches)),
            Dialog {
                title: "Interne Dateiaktionen",
                description: &description,
                options: &labels,
                ..Default::default()
            },
        );
    }

    fn choose_association(&mut self) {
        let labels: Vec<_> = std::iter::once("Neue Zuordnung")
            .chain(
                self.profile
                    .associations
                    .iter()
                    .map(|entry| entry.name.as_str()),
            )
            .collect();
        let labels: Vec<_> = labels.into_iter().map(str::to_owned).collect();
        let options: Vec<_> = labels.iter().map(String::as_str).collect();
        self.dialog("confirm", Pending::Associations(Setup::Choose), Dialog {
            title: "Interne Dateizuordnungen",
            description: "Beim Öffnen gilt die erste passende Standardaktion. Eigene Verben erscheinen unter Dateien → Interne Dateiaktionen.",
            options: &options, ..Default::default()
        });
    }

    fn edit_association(&mut self, index: Option<usize>) {
        let value = index
            .and_then(|index| self.profile.associations.get(index))
            .map_or_else(
                || {
                    "Datei anzeigen\n*.txt\nC:\\Windows\\System32\\notepad.exe\nnein\n{datei}"
                        .into()
                },
                format_association,
            );
        self.dialog("multiline", Pending::Associations(Setup::Edit(index)), Dialog {
            title: "Interne Dateizuordnung bearbeiten",
            description: "Je Zeile Name, Masken, absoluter EXE-Pfad und ja oder nein für die Standardaktion. Danach ein Argument pro Zeile. Platzhalter als eigenes Argument: {datei}, {ordner}, {name}.",
            value: &value, ..Default::default()
        });
    }

    pub(crate) fn submit_association(
        &mut self,
        setup: Setup,
        value: &str,
        option: usize,
    ) -> io::Result<()> {
        match setup {
            Setup::Choose if option == 0 => self.edit_association(None),
            Setup::Choose => {
                if let Some(entry) = self.profile.associations.get(option - 1) {
                    let title = entry.name.clone();
                    self.dialog(
                        "confirm",
                        Pending::Associations(Setup::Manage(option - 1)),
                        Dialog {
                            title: &title,
                            options: &["Bearbeiten", "Nach oben", "Nach unten", "Entfernen"],
                            ..Default::default()
                        },
                    );
                }
            }
            Setup::Manage(index) if option == 0 => self.edit_association(Some(index)),
            Setup::Manage(index) => {
                let mut entries = self.profile.associations.clone();
                if index < entries.len() {
                    match option {
                        1 if index > 0 => entries.swap(index, index - 1),
                        2 if index + 1 < entries.len() => entries.swap(index, index + 1),
                        3 => {
                            entries.remove(index);
                        }
                        _ => (),
                    }
                    self.save_associations(entries)?;
                }
                self.choose_association();
            }
            Setup::Edit(index) => {
                let association = parse_association(value)?;
                let mut entries = self.profile.associations.clone();
                if let Some(index) = index {
                    let entry = entries
                        .get_mut(index)
                        .ok_or_else(|| io::Error::other("Die Zuordnung fehlt."))?;
                    *entry = association;
                } else if entries.len() < 256 {
                    entries.push(association);
                } else {
                    return Err(io::Error::other(
                        "Es können höchstens 256 Zuordnungen gespeichert werden.",
                    ));
                }
                self.save_associations(entries)?;
                self.choose_association();
            }
            Setup::Verbs(path, verbs) => {
                if let Some(entry) = verbs.get(option) {
                    run(entry, &path)?;
                }
            }
            Setup::Shortcut { source, directory } => {
                let name = files::leaf_name(OsStr::new(value.trim()))?;
                let mut destination = directory.join(name);
                if !destination
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("lnk"))
                {
                    let mut leaf = name.to_os_string();
                    leaf.push(".lnk");
                    destination.set_file_name(files::leaf_name(&leaf)?);
                }
                self.start_job("Erstelle Verknüpfung …", true, move |job| {
                    files::check_cancel(&job.cancel)?;
                    arco_windows::create_shortcut(&source, &destination)?;
                    Ok(Outcome::Notice(format!(
                        "Verknüpfung erstellt · {}",
                        display_path(&destination)
                    )))
                });
            }
        }
        Ok(())
    }

    fn save_associations(&mut self, entries: Vec<FileAssociation>) -> io::Result<()> {
        if !self.save_profile {
            return Err(io::Error::other("Das Arco-Profil ist nicht verfügbar."));
        }
        let mut draft = self.profile_snapshot();
        draft.associations = entries;
        draft.save(&self.profile_path)?;
        self.profile = draft;
        Ok(())
    }
}

fn format_association(entry: &FileAssociation) -> String {
    let mut value = format!(
        "{}\n{}\n{}\n{}",
        entry.name,
        entry.masks,
        display_path(&entry.program),
        if entry.default_open { "ja" } else { "nein" }
    );
    for arg in &entry.arguments {
        value.push('\n');
        value.push_str(arg);
    }
    value
}

fn parse_association(value: &str) -> io::Result<FileAssociation> {
    if value.len() > 8192 {
        return Err(io::Error::other("Die Zuordnung enthält zu viel Text."));
    }
    let mut lines = value.lines();
    let name = lines.next().unwrap_or_default().trim();
    let masks = lines.next().unwrap_or_default().trim();
    let program = PathBuf::from(lines.next().unwrap_or_default().trim().trim_matches('"'));
    let default_open = match lines
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "ja" => true,
        "nein" => false,
        _ => {
            return Err(io::Error::other(
                "Die Standardaktion wird mit ja oder nein eingestellt.",
            ));
        }
    };
    if name.is_empty()
        || name.chars().count() > 80
        || name.chars().any(char::is_control)
        || masks.is_empty()
        || masks.chars().any(char::is_control)
    {
        return Err(io::Error::other(
            "Gib einen Namen und mindestens eine Dateimaske ein.",
        ));
    }
    FileMasks::new(masks)?;
    let arguments: Vec<_> = lines.map(str::to_owned).collect();
    if !valid_program(&program)
        || arguments
            .iter()
            .any(|argument| argument.chars().any(char::is_control))
    {
        return Err(io::Error::other(
            "Die Zuordnung benötigt einen absoluten EXE-Pfad und Argumente ohne Steuerzeichen.",
        ));
    }
    Ok(FileAssociation {
        name: name.into(),
        masks: masks.into(),
        program,
        arguments,
        default_open,
    })
}

fn valid_program(path: &Path) -> bool {
    path.is_absolute()
        && path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("exe"))
        && !path
            .as_os_str()
            .to_string_lossy()
            .chars()
            .any(char::is_control)
}

fn matches_path(entry: &FileAssociation, path: &Path) -> bool {
    FileMasks::new(&entry.masks).is_ok_and(|masks| masks.includes(path))
}

fn arguments(entry: &FileAssociation, path: &Path) -> Vec<OsString> {
    let mut includes_file = false;
    let mut args: Vec<_> = entry
        .arguments
        .iter()
        .map(|value| match value.as_str() {
            "{datei}" => {
                includes_file = true;
                path.as_os_str().to_os_string()
            }
            "{name}" => {
                includes_file = true;
                path.file_name().unwrap_or_default().to_os_string()
            }
            "{ordner}" => path
                .parent()
                .map_or_else(OsString::new, |parent| parent.as_os_str().to_os_string()),
            value => value.into(),
        })
        .collect();
    if !includes_file {
        args.push(path.as_os_str().to_os_string());
    }
    args
}

fn run(entry: &FileAssociation, path: &Path) -> io::Result<()> {
    if !valid_program(&entry.program) {
        return Err(io::Error::other(
            "Die Zuordnung enthält keinen gültigen EXE-Pfad.",
        ));
    }
    Command::new(&entry.program)
        .args(arguments(entry, path))
        .spawn()
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_and_path_arguments_do_not_depend_on_shell_quoting() {
        let path = std::env::temp_dir()
            .join("Ordner mit Leerzeichen")
            .join("Daten ä.txt");
        let executable = std::env::temp_dir().join("app.exe");
        let rule = parse_association(&format!(
            "Edit\n*.txt | ignoriert*\n{}\nja\n--file\n{{datei}}\n--format=one two",
            executable.display()
        ))
        .unwrap();
        assert!(rule.default_open);
        assert!(matches_path(&rule, &path));
        assert!(!matches_path(&rule, &path.with_file_name("ignoriert.txt")));
        assert_eq!(
            arguments(&rule, &path),
            [
                OsString::from("--file"),
                path.into_os_string(),
                OsString::from("--format=one two")
            ]
        );
    }
}
