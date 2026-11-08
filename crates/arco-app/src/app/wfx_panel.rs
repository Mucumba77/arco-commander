use super::{App, JobContext, LoadedDirectory, Navigation, Outcome, StagedPreview};
use crate::commands::{Dialog, Pending};
use crate::panel::{Location, WfxLocation};
use crate::{human_size, preview};
use arco_core::archive_view::MAX_PREVIEW;
use arco_core::files::{self, Directory, Entry, Kind};
use arco_core::plugin::{self, Operation, Registration, WfxOperation};
use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub(super) fn read_panel(location: &WfxLocation, job: &JobContext) -> io::Result<LoadedDirectory> {
    let plugin = &location.plugin;
    let rows = plugin::list_wfx(
        &plugin.path,
        Path::new(&location.directory),
        &job.cancel,
        &mut |prompt| job.prompt(&plugin.name, prompt),
    )?;
    let mut directory = Directory {
        entries: Vec::new(),
        errors: Vec::new(),
    };
    let mut names = BTreeSet::new();
    for row in rows {
        if files::leaf_name(OsStr::new(&row.name)).is_err()
            || !names.insert(row.name.to_lowercase())
        {
            directory.errors.push(format!(
                "Ungültiger oder mehrdeutiger WFX-Name: {}",
                row.name
            ));
            continue;
        }
        let folder = row.value == "Ordner";
        let size = row
            .value
            .strip_suffix(" B")
            .and_then(|size| size.parse().ok());
        directory.entries.push(Entry::from_remote(
            PathBuf::from(row.name),
            if folder { Kind::Directory } else { Kind::File },
            size,
            None,
        ));
    }
    Ok(LoadedDirectory {
        directory,
        space: None,
        archive: None,
        ftp: None,
    })
}

fn navigate(location: &WfxLocation, value: &str) -> io::Result<WfxLocation> {
    let prefix = format!("{}::", location.plugin.name);
    let value = value
        .strip_prefix(&prefix)
        .unwrap_or(value)
        .replace('/', "\\");
    let mut result = if value.starts_with('\\') {
        location.at("\\")
    } else {
        location.clone()
    };
    for name in value.split('\\').filter(|name| !name.is_empty()) {
        match name {
            "." => (),
            ".." => {
                if let Some(parent) = result.parent() {
                    result = parent;
                }
            }
            name => {
                files::leaf_name(OsStr::new(name))?;
                result.directory = result.child(name);
            }
        }
    }
    Ok(result)
}

impl App {
    pub(super) fn wfx_enabled(&self, plugin: &Registration) -> bool {
        self.profile
            .plugins
            .iter()
            .any(|item| item.enabled && item.same_file(plugin))
    }

    pub(super) fn mount_wfx(&mut self, entry: Registration, value: &str) {
        let location = WfxLocation {
            plugin: Arc::new(entry),
            directory: "\\".into(),
        };
        self.navigate_wfx(location, value);
    }

    pub(super) fn navigate_wfx(&mut self, location: WfxLocation, value: &str) {
        match navigate(&location, value) {
            Ok(location) => {
                self.load_location(self.active, Location::Wfx(location), Navigation::Jump)
            }
            Err(error) => self.status(error.to_string()),
        }
    }

    pub(super) fn open_wfx_row(&mut self, row: usize) {
        let tab = self.current();
        let Some((location, entry)) = tab.wfx.as_ref().zip(tab.display_row(row)) else {
            return;
        };
        if entry.kind == Kind::Directory {
            let directory = location.child(&entry.name.to_string_lossy());
            self.load_location(
                self.active,
                Location::Wfx(location.at(&directory)),
                Navigation::Jump,
            );
        } else {
            self.view_wfx_entry(row);
        }
    }

    fn view_wfx_entry(&mut self, row: usize) {
        let tab = self.current();
        let Some((location, entry)) = tab.wfx.as_ref().zip(tab.display_row(row)) else {
            return;
        };
        if !self.wfx_enabled(&location.plugin) {
            self.status("Dieses WFX-Plugin ist deaktiviert.");
            return;
        }
        if entry.kind != Kind::File || entry.size.is_none_or(|size| size > MAX_PREVIEW) {
            self.status("Wähle eine WFX-Datei mit bekannter Größe bis 64 MiB.");
            return;
        }
        let remote = location.child(&entry.name.to_string_lossy());
        let plugin = location.plugin.clone();
        let name = entry.name.clone();
        self.start_job("Lade WFX-Vorschau …", false, move |job| {
            let storage = Arc::new(tempfile::tempdir()?);
            let path = storage.path().join(files::leaf_name(&name)?);
            plugin::operate_interactive(
                &plugin.path,
                WfxOperation::Download {
                    remote: remote.clone(),
                    local: path.clone(),
                },
                &job.cancel,
                |message| job.progress(message),
                &mut |prompt| job.prompt(&plugin.name, prompt),
            )?;
            if std::fs::metadata(&path)?.len() > MAX_PREVIEW {
                return Err(io::Error::other(
                    "Die WFX-Datei überschreitet das Vorschaulimit.",
                ));
            }
            let content = preview::read(&path)?;
            Ok(Outcome::StagedPreview(
                StagedPreview {
                    storage,
                    path,
                    title: remote,
                },
                content,
            ))
        });
    }

    pub(crate) fn wfx_command(&mut self, command: &str) -> bool {
        if matches!(command, "copy" | "move")
            && self
                .panels
                .iter()
                .any(|panel| panel.current().wfx.is_some())
        {
            self.wfx_transfer(command == "move");
            return true;
        }
        let Some(location) = self.current().wfx.clone() else {
            return false;
        };
        if self.panels[self.active].loading
            && matches!(
                command,
                "view" | "rename" | "mkdir" | "delete" | "delete-permanent" | "properties"
            )
        {
            self.status("Warte, bis das WFX-Dateifenster geladen ist.");
            return true;
        }
        match command {
            "wfx-local" => self.load(self.active, self.current().path.clone(), Navigation::Jump),
            "root" => {
                if !self.return_to_tab_base() {
                    self.load_location(
                        self.active,
                        Location::Wfx(location.at("\\")),
                        Navigation::Jump,
                    );
                }
            }
            "view" => self.view_wfx_entry(self.current().cursor),
            "properties" => {
                if let Some(entry) = self.current().display_row(self.current().cursor) {
                    let path = location.child(&entry.name.to_string_lossy());
                    let details = format!(
                        "{}::{}\n{}",
                        location.plugin.name,
                        path,
                        entry
                            .size
                            .map_or_else(|| "Ordner oder Größe unbekannt".into(), human_size)
                    );
                    self.text("WFX-Eigenschaften", &details);
                }
            }
            "mkdir" => self.dialog(
                "input",
                Pending::WfxPanelEdit(location, None),
                Dialog {
                    title: "WFX-Ordner erstellen",
                    placeholder: "Ordnername",
                    ..Dialog::default()
                },
            ),
            "rename" => {
                if let Some(entry) = self.current().display_row(self.current().cursor) {
                    let name = entry.name.to_string_lossy().into_owned();
                    let source = location.child(&name);
                    self.dialog(
                        "input",
                        Pending::WfxPanelEdit(location, Some(source)),
                        Dialog {
                            title: "WFX-Eintrag umbenennen",
                            value: &name,
                            placeholder: "Neuer Name",
                            ..Dialog::default()
                        },
                    );
                }
            }
            "delete" | "delete-permanent" => self.wfx_delete(location),
            "search" | "quick-view" | "sync" | "zip-extract" | "zip-test" => {
                self.status("Für diesen Befehl wird ein lokaler Ordner benötigt.");
            }
            _ => return false,
        }
        true
    }

    pub(crate) fn edit_wfx_panel(
        &mut self,
        location: WfxLocation,
        source: Option<String>,
        value: &str,
    ) {
        let value = value.trim();
        if files::leaf_name(OsStr::new(value)).is_err() {
            self.status("Gib einen gültigen Namen ohne Verzeichnisteile ein.");
            return;
        }
        let target = location.child(value);
        let operation = source.map_or_else(
            || WfxOperation::Mkdir {
                path: target.clone(),
            },
            |from| WfxOperation::Rename {
                from,
                to: target.clone(),
            },
        );
        self.run_wfx_batch(location, vec![(value.into(), Operation::Wfx(operation))]);
    }

    fn wfx_transfer(&mut self, moving: bool) {
        if self.panels.iter().any(|panel| panel.loading) {
            self.status("Warte, bis beide Dateifenster geladen sind.");
            return;
        }
        let source = self.current();
        let target = self.panels[1 - self.active].current();
        let mut tasks = Vec::new();
        let location = match (&source.wfx, &target.wfx) {
            (Some(location), None) if target.is_local() => {
                for entry in selected_transfer_entries(source) {
                    let name = entry.name.to_string_lossy().into_owned();
                    let remote = location.child(&name);
                    let local = target.path.join(&entry.name);
                    let operation = if moving {
                        WfxOperation::DownloadMove {
                            remote,
                            local,
                            tree: entry.kind == Kind::Directory,
                        }
                    } else if entry.kind == Kind::Directory {
                        WfxOperation::DownloadTree { remote, local }
                    } else {
                        WfxOperation::Download { remote, local }
                    };
                    tasks.push((name, Operation::Wfx(operation)));
                }
                location.clone()
            }
            (None, Some(location)) if source.is_local() => {
                for entry in selected_transfer_entries(source) {
                    let name = entry.name.to_string_lossy().into_owned();
                    let remote = location.child(&name);
                    let local = entry.path.clone();
                    let operation = if moving {
                        WfxOperation::UploadMove {
                            local,
                            remote,
                            tree: entry.kind == Kind::Directory,
                        }
                    } else if entry.kind == Kind::Directory {
                        WfxOperation::UploadTree { local, remote }
                    } else {
                        WfxOperation::Upload { local, remote }
                    };
                    tasks.push((name, Operation::Wfx(operation)));
                }
                location.clone()
            }
            (Some(from), Some(to)) if from.plugin.same_file(&to.plugin) => {
                if from.directory == to.directory {
                    self.status("Wähle einen anderen Zielordner im WFX-Dateisystem.");
                    return;
                }
                for entry in selected_transfer_entries(source) {
                    let name = entry.name.to_string_lossy().into_owned();
                    let from = from.child(&name);
                    let to = to.child(&name);
                    let operation = match (moving, entry.kind) {
                        (true, Kind::Directory) => WfxOperation::MoveTree { from, to },
                        (false, Kind::Directory) => WfxOperation::CopyTree { from, to },
                        (true, _) => WfxOperation::Rename { from, to },
                        (false, _) => WfxOperation::Copy { from, to },
                    };
                    tasks.push((name, Operation::Wfx(operation)));
                }
                from.clone()
            }
            _ => {
                self.status(if moving {
                    "Verschieben benötigt einen lokalen Ordner oder zwei Ordner desselben WFX-Plugins."
                } else {
                    "Kopieren benötigt einen lokalen Ordner oder zwei Ordner desselben WFX-Plugins."
                });
                return;
            }
        };
        self.confirm_wfx_batch(
            if moving {
                "WFX-Auswahl verschieben"
            } else {
                "WFX-Auswahl kopieren"
            },
            if moving {
                "Dateien und vollständige Ordner verschieben? Nach der Übertragung wird die Quelle dauerhaft entfernt."
            } else {
                "Dateien und vollständige Ordner übertragen? Vorhandene Ziele werden als Konflikt gemeldet."
            },
            location,
            tasks,
        );
    }

    fn wfx_delete(&mut self, location: WfxLocation) {
        let tab = self.current();
        let paths: BTreeSet<_> = tab.selected_paths().into_iter().collect();
        let tasks = tab
            .display_rows()
            .filter(|entry| paths.contains(&entry.path))
            .map(|entry| {
                let name = entry.name.to_string_lossy().into_owned();
                let path = location.child(&name);
                let operation = if entry.kind == Kind::Directory {
                    WfxOperation::DeleteTree { path }
                } else {
                    WfxOperation::Delete { path }
                };
                (name, Operation::Wfx(operation))
            })
            .collect();
        self.confirm_wfx_batch(
            "WFX-Einträge löschen",
            "Die markierten Dateien und Ordner mitsamt ihrem Inhalt dauerhaft löschen?",
            location,
            tasks,
        );
    }

    fn confirm_wfx_batch(
        &mut self,
        title: &str,
        description: &str,
        location: WfxLocation,
        tasks: Vec<(String, Operation)>,
    ) {
        if tasks.is_empty() {
            self.status("Wähle passende WFX-Dateien oder lokale Dateien.");
            return;
        }
        let contents = tasks
            .iter()
            .map(|(label, _)| label.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        self.dialog(
            "review",
            Pending::WfxPanelBatch(location, tasks),
            Dialog {
                title,
                description,
                value: &contents,
                ..Dialog::default()
            },
        );
    }

    pub(crate) fn run_wfx_batch(&mut self, location: WfxLocation, tasks: Vec<(String, Operation)>) {
        if !self.wfx_enabled(&location.plugin) {
            self.status("Dieses WFX-Plugin ist deaktiviert.");
            return;
        }
        self.run_plugin_batch((*location.plugin).clone(), PathBuf::new(), tasks);
    }

    pub(super) fn wfx_context_dialog(&mut self) {
        self.dialog(
            "confirm",
            Pending::WfxContext,
            Dialog {
                title: "WFX-Eintrag",
                options: &[
                    "Öffnen",
                    "Vorschau",
                    "Kopieren",
                    "Verschieben",
                    "Umbenennen",
                    "Neuer Ordner",
                    "Dauerhaft löschen",
                    "Eigenschaften",
                ],
                ..Dialog::default()
            },
        );
    }
}

fn selected_transfer_entries(tab: &crate::panel::Tab) -> impl Iterator<Item = &Entry> {
    let paths: BTreeSet<_> = tab.selected_paths().into_iter().collect();
    tab.display_rows().filter(move |entry| {
        matches!(entry.kind, Kind::File | Kind::Directory) && paths.contains(&entry.path)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use arco_core::plugin::{Architecture, Interface};

    #[test]
    fn wfx_navigation_keeps_the_plugin_root_and_relative_parent() {
        let root = WfxLocation {
            plugin: Arc::new(Registration {
                name: "Test".into(),
                path: PathBuf::new(),
                interface: Interface::Wfx,
                architecture: Architecture::X64,
                enabled: true,
                masks: String::new(),
                tc_detection: None,
                package: None,
            }),
            directory: "\\".into(),
        };
        let nested = navigate(&root, r"\Server\Projekt").unwrap();
        assert_eq!(
            navigate(&nested, r"..\Anderes").unwrap().directory,
            r"\Server\Anderes"
        );
        assert_eq!(
            navigate(&nested, r"Test::\Server").unwrap().directory,
            r"\Server"
        );
        assert_eq!(navigate(&root, r"..\..").unwrap(), root);
        assert!(navigate(&root, r"\*.txt").is_err());
    }
}
