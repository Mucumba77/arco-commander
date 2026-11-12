use super::{App, LoadedDirectory, Navigation, Outcome};
use crate::commands::{Dialog, Pending};
use crate::panel::{Location, ShellLocation, Tab};
use arco_core::files::{self, Directory, Entry, Kind};
use arco_windows::{ShellAction, ShellTransferOptions};
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

pub(super) fn read_panel(
    location: &mut ShellLocation,
    hidden: bool,
    cancel: &AtomicBool,
) -> io::Result<LoadedDirectory> {
    let listing = arco_windows::shell_listing(location.address(), hidden, cancel)?;
    let mut directory = Directory {
        entries: Vec::with_capacity(listing.entries.len()),
        errors: Vec::new(),
    };
    let mut shell = BTreeMap::new();
    for item in listing.entries {
        files::check_cancel(cancel)?;
        let key = PathBuf::from(&item.parsing_name);
        if shell.contains_key(&key) {
            continue;
        }
        let kind = if item.directory {
            Kind::Directory
        } else {
            Kind::File
        };
        let metadata = item
            .path
            .as_deref()
            .and_then(|path| files::regular_metadata(path).ok());
        let size = metadata
            .as_ref()
            .filter(|metadata| metadata.is_file())
            .map(|metadata| metadata.len());
        let modified = metadata.and_then(|metadata| metadata.modified().ok());
        directory.entries.push(
            Entry::from_remote(key.clone(), kind, size, modified)
                .with_display_name(item.name.as_str().into()),
        );
        shell.insert(key, item);
    }
    *location = ShellLocation(listing.ancestors);
    Ok(LoadedDirectory {
        directory,
        space: None,
        archive: None,
        ftp: None,
        shell,
    })
}

impl App {
    pub(crate) fn shell_command(&mut self, command: &str) -> bool {
        self.dispatch_shell_action(command, false)
    }

    pub(crate) fn open_shell_elevation(&mut self) {
        if shell_location(self.current()).is_none() {
            self.status("Wähle einen lokalen Ordner oder einen Windows-Shell-Ordner.");
            return;
        }
        self.dialog("confirm", Pending::ShellElevation, Dialog {
            title: "Dateiauftrag mit Windows-Rechteabfrage",
            description: "Windows kann für diesen einzelnen Auftrag bei Bedarf die Benutzerkontensteuerung anzeigen.",
            options: &["Kopieren", "Verschieben", "Umbenennen", "Ordner erstellen", "Papierkorb", "Endgültig löschen"],
            ..Default::default()
        });
    }

    pub(crate) fn choose_shell_elevation(&mut self, option: usize) -> io::Result<()> {
        let command = [
            "copy",
            "move",
            "rename",
            "mkdir",
            "delete",
            "delete-permanent",
        ]
        .get(option)
        .ok_or_else(|| io::Error::other("Wähle einen Dateiauftrag."))?;
        self.dispatch_shell_action(command, true);
        Ok(())
    }

    fn dispatch_shell_action(&mut self, command: &str, elevated: bool) -> bool {
        let tab = self.current();
        let other = self.panels[1 - self.active].current();
        let transfer = matches!(command, "copy" | "move");
        let local_action = matches!(command, "delete" | "delete-permanent" | "rename" | "mkdir");
        if !(elevated
            || transfer && (tab.shell.is_some() || other.shell.is_some())
            || local_action && tab.shell.is_some())
        {
            return false;
        }
        if self.panels[self.active].loading || transfer && self.panels[1 - self.active].loading {
            self.status("Warte, bis die beteiligten Dateifenster geladen sind.");
            return true;
        }
        let Some(source) = shell_location(tab) else {
            self.status("Windows-Shell-Aufträge benötigen einen lokalen Ordner oder einen Windows-Shell-Ordner.");
            return true;
        };
        if command == "mkdir" {
            self.shell_name_dialog(source, true, elevated, "");
            return true;
        }
        if command == "rename" {
            if let Some(entry) = tab.current() {
                let (path, name) = if let Some(item) = tab.shell_entries.get(&entry.path) {
                    let path = PathBuf::from(&item.parsing_name);
                    let full_name = item.path.as_deref().and_then(Path::file_name).or_else(|| {
                        // Windows can hide a known extension in the display label.
                        (path.extension().is_some()
                            && path.file_stem() == Some(OsStr::new(&item.name)))
                        .then(|| path.file_name())
                        .flatten()
                    });
                    let name = full_name.map_or_else(
                        || item.name.clone(),
                        |name| name.to_string_lossy().into_owned(),
                    );
                    (path, name)
                } else {
                    (
                        entry.path.clone(),
                        entry.name.to_string_lossy().into_owned(),
                    )
                };
                self.shell_name_dialog(path, false, elevated, &name);
            } else {
                self.status("Wähle einen Windows-Shell-Eintrag.");
            }
            return true;
        }
        let paths = if tab.shell.is_some() {
            tab.paths()
                .iter()
                .filter_map(|path| tab.shell_entries.get(path))
                .map(|item| PathBuf::from(&item.parsing_name))
                .collect()
        } else {
            tab.paths()
        };
        if paths.is_empty() {
            self.status("Wähle Dateien oder Ordner.");
            return true;
        }
        let (action, destination) = match command {
            "copy" | "move" => {
                let Some(target) = shell_location(other) else {
                    self.status("Wähle einen lokalen oder Windows-Shell-Zielordner.");
                    return true;
                };
                let description = format!("Zielordner: {}", other.location().display());
                let action = if command == "copy" { ShellAction::Copy(target) } else { ShellAction::Move(target) };
                (action, description)
            }
            "delete" => (ShellAction::Recycle, "Windows verschiebt die Auswahl in den Papierkorb, wenn der Anbieter dies unterstützt.".into()),
            "delete-permanent" => (ShellAction::Delete, "Die ausgewählten Einträge werden endgültig gelöscht.".into()),
            _ => unreachable!("shell command checked above"),
        };
        let label = shell_label(&action);
        let description = format!("{} Einträge\n{destination}", paths.len());
        self.dialog(
            "confirm",
            Pending::ShellTransfer(paths, action, elevated),
            Dialog {
                title: label,
                description: &description,
                ..Default::default()
            },
        );
        true
    }

    fn shell_name_dialog(&mut self, path: PathBuf, folder: bool, elevated: bool, initial: &str) {
        self.dialog(
            "input",
            Pending::ShellName(path, folder, elevated),
            Dialog {
                title: if folder {
                    "Neuer Windows-Shell-Ordner"
                } else {
                    "Windows-Shell-Eintrag umbenennen"
                },
                value: initial,
                placeholder: "Name",
                ..Default::default()
            },
        );
    }

    pub(crate) fn submit_shell_name(
        &mut self,
        path: PathBuf,
        folder: bool,
        elevated: bool,
        value: &str,
    ) -> io::Result<()> {
        let name = files::leaf_name(OsStr::new(value))?.to_owned();
        let action = if folder {
            ShellAction::NewFolder(name)
        } else {
            ShellAction::Rename(name)
        };
        self.queue_shell_transfer(vec![path], action, elevated);
        Ok(())
    }

    pub(crate) fn queue_shell_transfer(
        &mut self,
        paths: Vec<PathBuf>,
        action: ShellAction,
        elevated: bool,
    ) {
        let label = shell_label(&action);
        let options = ShellTransferOptions {
            owner: self.native_window_handle(),
            allow_elevation: elevated,
        };
        let (kind, target) = match &action {
            ShellAction::Copy(target) => (crate::OperationKind::Copy, crate::display_path(target)),
            ShellAction::Move(target) => (crate::OperationKind::Move, crate::display_path(target)),
            ShellAction::Recycle | ShellAction::Delete => {
                (crate::OperationKind::Delete, String::new())
            }
            _ => (crate::OperationKind::Task, String::new()),
        };
        let mut presentation = super::JobDescription::files(kind, &paths, Path::new(""));
        presentation.target = target;
        self.enqueue_virtual_operation(label, true, presentation, move |job| {
            let report = arco_windows::shell_transfer(
                &paths,
                &action,
                options,
                job.cancel.clone(),
                |path| {
                    job.progress(format!("Windows-Shell · {}", crate::display_path(path)));
                },
            )?;
            Ok(Outcome::Report(
                label.into(),
                files::TransferReport {
                    completed: report.completed,
                    skipped: report.skipped,
                    cancelled: report.cancelled,
                    errors: report.errors,
                    ..Default::default()
                },
            ))
        });
    }

    pub(crate) fn open_shell_panel(&mut self, side: usize, address: &str) {
        if cfg!(windows) {
            self.load_location(
                side,
                Location::Shell(ShellLocation::from_address(address)),
                Navigation::Jump,
            );
        } else {
            self.status("Windows-Shell-Ordner sind unter Windows verfügbar.");
        }
    }

    pub(super) fn open_shell_panel_row(&mut self, row: usize) {
        let tab = self.current();
        let Some(item) = tab
            .display_row(row)
            .and_then(|entry| tab.shell_entries.get(&entry.path))
            .cloned()
        else {
            return;
        };
        if let Some(path) = item.path.filter(|path| path.is_absolute()) {
            self.open_path(&path);
        } else if item.directory {
            self.open_shell_panel(self.active, &item.parsing_name);
        } else {
            self.start_job("Öffne Shell-Eintrag …", false, move |_| {
                arco_windows::open_shell_item(&item.parsing_name)?;
                Ok(Outcome::Notice("Shell-Eintrag geöffnet.".into()))
            });
        }
    }

    pub(crate) fn current_shell_file(&self) -> Option<&Path> {
        let tab = self.current();
        let entry = tab.display_row(tab.cursor)?;
        let item = tab.shell_entries.get(&entry.path)?;
        (!item.directory)
            .then(|| item.path.as_deref())
            .flatten()
            .filter(|path| path.is_absolute())
    }
}

fn shell_location(tab: &Tab) -> Option<PathBuf> {
    tab.shell
        .as_ref()
        .map(|shell| PathBuf::from(shell.address()))
        .or_else(|| tab.is_local().then(|| tab.path.clone()))
}

fn shell_label(action: &ShellAction) -> &'static str {
    match action {
        ShellAction::Copy(_) => "Mit Windows-Shell kopieren",
        ShellAction::Move(_) => "Mit Windows-Shell verschieben",
        ShellAction::Recycle => "In den Windows-Papierkorb verschieben",
        ShellAction::Delete => "Endgültig aus der Windows-Shell löschen",
        ShellAction::Rename(_) => "Windows-Shell-Eintrag umbenennen",
        ShellAction::NewFolder(_) => "Windows-Shell-Ordner erstellen",
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn shell_listing_exposes_computer_and_its_filesystem_drives() {
        let cancel = AtomicBool::new(false);
        let mut desktop = ShellLocation::default();
        let root = read_panel(&mut desktop, false, &cancel).unwrap();
        let computer = root.shell.values().find(|item| item.computer).unwrap();
        assert!(computer.directory);
        let mut location = ShellLocation::from_address(&computer.parsing_name);
        let drives = read_panel(&mut location, false, &cancel).unwrap();
        assert_eq!(
            Location::Shell(location).parent(),
            Some(Location::Shell(desktop))
        );
        assert!(drives.shell.values().any(|item| {
            item.directory && item.path.as_ref().is_some_and(|path| path.is_absolute())
        }));
    }

    #[test]
    fn shell_tabs_keep_the_local_fallback_and_shell_lock_on_restore() {
        use crate::panel::{Panel, Tab};
        use arco_core::profile::TabLock;
        let local = tempfile::tempdir().unwrap();
        let mut panel = Panel::from_saved(Default::default(), local.path());
        let shell = Location::Shell(ShellLocation::from_address(arco_windows::SHELL_COMPUTER));
        let mut tab = Tab::new(local.path().to_owned());
        tab.change_location(&shell, None);
        tab.set_lock(TabLock::ReturnToBase);
        panel.tabs = vec![tab];
        let restored = Panel::from_saved(panel.saved(), local.path());
        assert_eq!(restored.current().path, local.path());
        assert_eq!(restored.current().location(), shell);
        assert_eq!(restored.current().lock_base, Some(shell));
    }
}
