use super::{App, Outcome};
use crate::commands::{Dialog, Pending};
use crate::display_path;
use arco_core::{
    files,
    profile::{self, RenameTemplate},
    rename,
};
use regex::Regex;
use std::{fs, io, path::PathBuf};

pub(crate) enum Setup {
    Choose(Vec<String>),
    Manage(String),
    Template(Option<String>),
    Names(NameList),
    ExternalNames(NameList, tempfile::TempDir),
}

pub(crate) struct NameList {
    paths: Vec<PathBuf>,
    metadata: Vec<fs::Metadata>,
}

impl NameList {
    fn read(paths: Vec<PathBuf>) -> io::Result<Self> {
        if paths
            .iter()
            .any(|path| path.file_name().and_then(std::ffi::OsStr::to_str).is_none())
        {
            return Err(io::Error::other(
                "Die Namensliste benötigt Unicode-Dateinamen.",
            ));
        }
        let metadata = paths
            .iter()
            .map(|path| files::regular_metadata(path))
            .collect::<io::Result<_>>()?;
        Ok(Self { paths, metadata })
    }

    fn text(&self) -> String {
        self.paths
            .iter()
            .map(|path| path.file_name().unwrap_or_default().to_string_lossy())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn plan(&self, text: &str) -> io::Result<rename::Plan> {
        for (path, metadata) in self.paths.iter().zip(&self.metadata) {
            files::ensure_unchanged(path, metadata)?;
        }
        rename::Plan::from_names(&self.paths, &text.lines().collect::<Vec<_>>())
    }
}

impl App {
    pub(crate) fn rename_setup_action(&mut self, command: &str) -> bool {
        match command {
            "rename-templates" => self.choose_rename_template(),
            "rename-names" | "rename-external" => {
                if !self.current().is_local() || self.panels[self.active].loading {
                    self.status("Wähle Einträge in einem lokalen Dateifenster.");
                    return true;
                }
                let paths = self.current().paths();
                if paths.is_empty() {
                    self.status("Wähle Dateien oder Ordner.");
                    return true;
                }
                let external = command == "rename-external";
                let editor = self.profile.editor.clone();
                self.start_job("Bereite die Namensliste vor …", false, move |job| {
                    let list = NameList::read(paths)?;
                    files::check_cancel(&job.cancel)?;
                    let directory = if external {
                        let directory = tempfile::tempdir()?;
                        let file = directory.path().join("namen.txt");
                        fs::write(
                            &file,
                            format!("\u{feff}{}\r\n", list.text().replace('\n', "\r\n")),
                        )?;
                        arco_windows::edit(&file, editor.as_deref())?;
                        Some(directory)
                    } else {
                        None
                    };
                    Ok(Outcome::RenameNameList(list, directory))
                });
            }
            _ => return false,
        }
        true
    }

    fn choose_rename_template(&mut self) {
        let names: Vec<_> = self.profile.rename_templates.keys().cloned().collect();
        let options: Vec<_> = std::iter::once("Neue Vorlage")
            .chain(names.iter().map(String::as_str))
            .collect();
        self.dialog(
            "confirm",
            Pending::RenameSetup(Setup::Choose(names.clone())),
            Dialog {
                title: "Vorlagen für Mehrfachumbenennung",
                options: &options,
                ..Default::default()
            },
        );
    }

    fn edit_rename_template(&mut self, name: Option<String>) {
        let template = name
            .as_ref()
            .and_then(|name| self.profile.rename_templates.get(name));
        let value = format!(
            "{}\n{}\n{}",
            name.as_deref().unwrap_or("Meine Vorlage"),
            template.map_or("[F]", |item| item.mask.as_str()),
            template.map_or("", |item| item.replacement.as_str())
        );
        self.dialog("multiline", Pending::RenameSetup(Setup::Template(name)), Dialog {
            title: "Umbenennvorlage bearbeiten",
            description: "Je Zeile Vorlagenname, Namensmaske und optional die Ersetzung RegEx=>Ersatz.",
            value: &value, ..Default::default()
        });
    }

    pub(crate) fn open_rename_template(&mut self, name: &str) -> io::Result<()> {
        if !self.current().is_local() || self.panels[self.active].loading {
            return Err(io::Error::other(
                "Wähle lokale Einträge für die Umbenennung.",
            ));
        }
        let template = self
            .profile
            .rename_templates
            .get(name)
            .cloned()
            .ok_or_else(|| io::Error::other("Diese Umbenennvorlage fehlt."))?;
        self.rename_dialog_with_template(true, Some(template));
        Ok(())
    }

    pub(super) fn show_rename_names(
        &mut self,
        list: NameList,
        directory: Option<tempfile::TempDir>,
    ) {
        if let Some(directory) = directory {
            let path = display_path(&directory.path().join("namen.txt"));
            let description = format!(
                "{path}\nSpeichere die bearbeitete Liste im Editor und lade sie anschließend für die Vorschau."
            );
            self.dialog(
                "confirm",
                Pending::RenameSetup(Setup::ExternalNames(list, directory)),
                Dialog {
                    title: "Externe Namensliste laden",
                    description: &description,
                    ..Default::default()
                },
            );
        } else {
            let names = list.text();
            self.dialog("multiline", Pending::RenameSetup(Setup::Names(list)), Dialog {
                title: "Dateinamen bearbeiten",
                description: "Eine Zeile pro Eintrag. Behalte die Reihenfolge bei und bestätige anschließend die Vorschau.",
                value: &names, ..Default::default()
            });
        }
    }

    pub(crate) fn submit_rename_setup(
        &mut self,
        setup: Setup,
        value: &str,
        option: usize,
    ) -> io::Result<()> {
        match setup {
            Setup::Choose(_) if option == 0 => self.edit_rename_template(None),
            Setup::Choose(names) => {
                if let Some(name) = names.get(option - 1) {
                    self.dialog(
                        "confirm",
                        Pending::RenameSetup(Setup::Manage(name.clone())),
                        Dialog {
                            title: name,
                            options: &[
                                "Mit Vorlage umbenennen",
                                "Vorlage bearbeiten",
                                "Vorlage löschen",
                            ],
                            ..Default::default()
                        },
                    );
                }
            }
            Setup::Manage(name) => match option {
                0 => self.open_rename_template(&name)?,
                1 => self.edit_rename_template(Some(name)),
                2 => {
                    let mut draft = self.profile_snapshot();
                    draft.rename_templates.remove(&name);
                    self.save_rename_templates(draft)?;
                    self.choose_rename_template();
                }
                _ => (),
            },
            Setup::Template(original) => {
                let mut lines = value.lines();
                let name = lines.next().unwrap_or_default().trim();
                let mask = lines.next().unwrap_or_default();
                let replacement = lines.next().unwrap_or_default();
                if name.is_empty()
                    || name.chars().any(char::is_control)
                    || name.chars().count() > 80
                    || mask.is_empty()
                    || mask.len() > 4096
                    || replacement.len() > 4096
                    || lines.any(|line| !line.is_empty())
                {
                    return Err(io::Error::other(
                        "Ungültiger Vorlagenname oder ungültige Umbenennmaske.",
                    ));
                }
                rename::uses_clipboard(mask)?;
                if !replacement.is_empty() {
                    let (find, _) = replacement
                        .split_once("=>")
                        .ok_or_else(|| io::Error::other("Ersetzung benötigt Muster=>Ersatz."))?;
                    Regex::new(find).map_err(io::Error::other)?;
                }
                let mut draft = self.profile_snapshot();
                if original.as_deref() != Some(name) && draft.rename_templates.contains_key(name) {
                    return Err(io::Error::other("Diese Vorlage existiert bereits."));
                }
                if let Some(original) = original {
                    draft.rename_templates.remove(&original);
                }
                draft.rename_templates.insert(
                    name.into(),
                    RenameTemplate {
                        mask: mask.into(),
                        replacement: replacement.into(),
                    },
                );
                self.save_rename_templates(draft)?;
                self.choose_rename_template();
            }
            Setup::Names(list) => self.rename_review(list.plan(value)?),
            Setup::ExternalNames(list, directory) => {
                let file = directory.path().join("namen.txt");
                let text = profile::read_tc_text(&file)?;
                self.rename_review(list.plan(&text)?);
            }
        }
        Ok(())
    }

    fn save_rename_templates(&mut self, draft: profile::Profile) -> io::Result<()> {
        if !self.save_profile {
            return Err(io::Error::other("Das Arco-Profil ist nicht verfügbar."));
        }
        draft.save(&self.profile_path)?;
        self.profile = draft;
        Ok(())
    }
}
