use super::{App, Outcome};
use crate::commands::{Dialog, Pending};
use crate::{PluginRowData, display_path};
use arco_core::files::Kind;
use arco_core::plugin::{self, Interface, Package, Registration, TcRegistrations};
use slint::{ModelRc, VecModel};
use std::fmt::Write as _;
use std::io;
use std::path::{Path, PathBuf};

impl App {
    pub(crate) fn show_plugin_manager(&mut self, selection: Option<usize>) {
        let entries = self
            .profile
            .plugins
            .iter()
            .map(|item| PluginRowData {
                name: item.name.as_str().into(),
                kind: item.interface.label().into(),
                architecture: item.architecture.label().into(),
                enabled: item.enabled,
                masks: item.masks.as_str().into(),
                path: display_path(&item.path).into(),
                detection: item.tc_detection.as_deref().unwrap_or_default().into(),
            })
            .collect::<Vec<_>>();
        let ui = self.ui();
        let selected = selection
            .filter(|&index| index < entries.len())
            .map_or_else(
                || if entries.is_empty() { -1 } else { 0 },
                |index| index as i32,
            );
        ui.set_plugin_entries(ModelRc::new(VecModel::from(entries)));
        ui.set_plugin_selection(selected);
        ui.set_dialog_kind("plugins".into());
        self.pending = None;
    }

    pub(super) fn operate_plugin(&mut self, action: &str, index: i32) {
        if self.ui().get_dialog_kind() != "plugins" || self.job.is_some() {
            return;
        }
        let result = match action {
            "add" | "package" | "import" => {
                self.plugin_input(action);
                Ok(())
            }
            _ => usize::try_from(index)
                .ok()
                .filter(|&index| index < self.profile.plugins.len())
                .map_or(Ok(()), |index| self.plugin_action(action, index)),
        };
        if let Err(error) = result {
            self.text("Plugin-Verwaltung", &error.to_string());
        }
    }

    fn plugin_input(&mut self, action: &str) {
        let selected = self
            .active_plugin_file()
            .map(|path| display_path(&path))
            .unwrap_or_default();
        let (pending, title, placeholder, second_placeholder, description, selected) = match action
        {
            "add" => (
                Pending::PluginAdd,
                "Plugin-DLL registrieren",
                "Absoluter Pfad zur Plugin-DLL",
                "Anzeigename (optional)",
                "Die Registrierung ist zunächst deaktiviert.",
                selected_if(&selected, |path| Interface::for_path(path).is_some()),
            ),
            "package" => (
                Pending::PluginPackage,
                "Plugin-Paket installieren",
                "Absoluter Pfad zum ZIP-Paket",
                "",
                "Arco liest die pluginst.inf und zeigt die Paketdateien vor der Installation an.",
                selected_if(&selected, |path| extension(path, "zip")),
            ),
            _ => (
                Pending::PluginImport,
                "Total-Commander-Plugins importieren",
                "Absoluter Pfad zur wincmd.ini",
                "Total-Commander-Programmordner (optional)",
                "Für %COMMANDER_PATH% gib den Total-Commander-Programmordner an. Die Registrierungen werden deaktiviert übernommen.",
                selected_if(&selected, |path| extension(path, "ini")),
            ),
        };
        self.dialog(
            "input",
            pending,
            Dialog {
                title,
                description,
                value: selected,
                placeholder,
                second_placeholder,
                ..Dialog::default()
            },
        );
    }

    fn plugin_action(&mut self, action: &str, index: usize) -> io::Result<()> {
        let entry = self.profile.plugins[index].clone();
        match action {
            "toggle" => {
                let mut plugins = self.profile.plugins.clone();
                plugins[index].enabled = !entry.enabled;
                self.commit_plugins(plugins)?;
                self.show_plugin_manager(Some(index));
            }
            "up" | "down" => {
                let direction = if action == "up" { -1 } else { 1 };
                if let Some(next) = index.checked_add_signed(direction).filter(|&next| next < self.profile.plugins.len()) {
                    let mut plugins = self.profile.plugins.clone();
                    plugins.swap(index, next);
                    self.commit_plugins(plugins)?;
                    self.show_plugin_manager(Some(next));
                }
            }
            "edit" => self.dialog("input", Pending::PluginEdit(index), Dialog {
                title: "Plugin-Registrierung bearbeiten",
                description: "Dateimasken werden durch Semikolon getrennt. Eine leere Maske lässt das Plugin zur manuellen Auswahl zu.",
                value: &entry.name,
                second: &entry.masks,
                placeholder: "Anzeigename",
                second_placeholder: "Dateimasken, z. B. *.txt;*.log",
                ..Dialog::default()
            }),
            "remove" => {
                let details = if entry.package.is_some() {
                    "Wenn keine andere Registrierung das Paket verwendet, entfernt Arco auch dessen Paketdateien."
                } else { "Die externe DLL bleibt auf dem Datenträger." };
                self.dialog("confirm", Pending::PluginRemove(index), Dialog {
                    title: "Plugin entfernen",
                    description: &format!("„{}“ aus der Plugin-Verwaltung entfernen? {details}", entry.name),
                    ..Dialog::default()
                });
            }
            "probe" => {
                self.close_dialog();
                self.start_job("Prüfe Plugin im Host …", false, move |job| {
                    let rows = plugin::probe(entry.interface, &entry.path, &job.cancel)
                        .map_err(|error| plugin_error(&entry.name, error))?;
                    Ok(Outcome::Text(format!("{} · {} · {}", entry.name, entry.interface.label(), entry.architecture.label()), describe(rows)))
                });
            }
            "files" if entry.enabled => self.show_plugin_file_actions(index),
            "column" if entry.enabled && entry.interface == Interface::Wdx => self.query_wdx_fields(entry),
            "run" if entry.enabled => {
                let file = if entry.interface == Interface::Wfx {
                    PathBuf::new()
                } else {
                    self.active_plugin_file().ok_or_else(|| io::Error::other("Wähle zuerst eine lokale Datei im aktiven Dateifenster."))?
                };
                if !entry.matches(entry.interface, &file) {
                    return Err(io::Error::other("Die ausgewählte Datei passt nicht zu den Dateimasken des Plugins."));
                }
                self.close_dialog();
                self.use_registered(index, file);
            }
            _ => (),
        }
        Ok(())
    }

    pub(crate) fn register_plugin(&mut self, path: &str, name: &str) -> io::Result<()> {
        let entry = Registration::read(&user_path(path), name, "")?;
        let mut plugins = self.profile.plugins.clone();
        if plugins.iter().any(|existing| existing.same_file(&entry)) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "Diese DLL ist bereits registriert.",
            ));
        }
        plugins.push(entry);
        self.commit_plugins(plugins)?;
        self.show_plugin_manager(self.profile.plugins.len().checked_sub(1));
        Ok(())
    }

    pub(crate) fn edit_plugin(&mut self, index: usize, name: &str, masks: &str) -> io::Result<()> {
        let mut plugins = self.profile.plugins.clone();
        if let Some(entry) = plugins.get_mut(index) {
            entry.update(name, masks)?;
            self.commit_plugins(plugins)?;
            self.show_plugin_manager(Some(index));
        }
        Ok(())
    }

    pub(crate) fn remove_plugin(&mut self, index: usize) -> io::Result<()> {
        let mut plugins = self.profile.plugins.clone();
        if index >= plugins.len() {
            return Ok(());
        }
        let removed = plugins.remove(index);
        let last_package = removed.package.as_ref().filter(|package| {
            !plugins.iter().any(|entry| {
                entry.path.starts_with(package) || entry.package.as_ref() == Some(package)
            })
        });
        self.commit_plugins(plugins)?;
        if let Some(package) = last_package {
            match plugin::remove_package(package, &self.plugin_root()) {
                Ok(()) => (),
                Err(error) if error.kind() == io::ErrorKind::NotFound => (),
                Err(error) => {
                    return Err(io::Error::other(format!(
                        "Registrierung entfernt. Paketdateien unter {} konnten nicht entfernt werden: {error}",
                        display_path(package)
                    )));
                }
            }
        }
        self.show_plugin_manager(Some(index.saturating_sub(1)));
        Ok(())
    }

    fn commit_plugins(&mut self, plugins: Vec<Registration>) -> io::Result<()> {
        if !self.save_profile {
            return Err(io::Error::other(
                "Das Arco-Profil konnte beim Start nicht geladen werden.",
            ));
        }
        let mut profile = self.profile_snapshot();
        profile.plugins = plugins;
        profile.save(&self.profile_path)?;
        self.profile = profile;
        self.invalidate_wdx_columns();
        self.invalidate_thumbnails();
        self.stop_quick_plugin();
        self.quick_requested = None;
        Ok(())
    }

    fn plugin_root(&self) -> PathBuf {
        self.profile_path.with_file_name("plugins")
    }

    pub(crate) fn read_plugin_package(&mut self, path: &str) -> io::Result<()> {
        let path = user_path(path);
        if !path.is_absolute() || !extension(&path, "zip") {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Gib den absoluten Pfad zu einem ZIP-Paket ein.",
            ));
        }
        let root = self.plugin_root();
        self.start_job("Prüfe ZIP-Plugin-Paket …", false, move |job| {
            Package::read(&path, &root, &job.cancel)
                .map(|package| Outcome::PluginPackage(Box::new(package)))
        });
        Ok(())
    }

    pub(super) fn review_plugin_package(&mut self, package: Package) {
        let mut body = String::new();
        if !package.version.is_empty() {
            let _ = writeln!(body, "Paketversion: {}\n", package.version);
        }
        for entry in &package.registrations {
            let _ = writeln!(
                body,
                "{} · {} · {}\n  {}",
                entry.name,
                entry.interface.label(),
                entry.architecture.label(),
                display_path(&entry.path)
            );
        }
        self.dialog("review", Pending::ApplyPluginPackage(Box::new(package)), Dialog {
            title: "Plugin-Paket prüfen",
            description: "Die DLLs werden deaktiviert installiert. Beim späteren Laden läuft Plugin-Code mit deinen Windows-Rechten.",
            value: &body,
            ..Dialog::default()
        });
    }

    pub(crate) fn install_plugin_package(&mut self, package: Package) -> io::Result<()> {
        let mut plugins = self.profile.plugins.clone();
        let first = plugins.len();
        plugins.extend(package.registrations.iter().cloned());
        self.commit_plugins(plugins)?;
        package.keep();
        self.show_plugin_manager(Some(first));
        Ok(())
    }

    pub(crate) fn read_plugin_import(&mut self, path: &str, root: &str) -> io::Result<()> {
        let path = user_path(path);
        let root = (!root.trim().is_empty()).then(|| user_path(root));
        if !path.is_absolute()
            || !extension(&path, "ini")
            || root.as_ref().is_some_and(|root| !root.is_absolute())
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Gib absolute Pfade zur wincmd.ini und gegebenenfalls zum Programmordner ein.",
            ));
        }
        self.start_job("Lese Total-Commander-Plugins …", false, move |_| {
            plugin::read_tc_plugins(&path, root.as_deref()).map(Outcome::PluginImport)
        });
        Ok(())
    }

    pub(super) fn review_plugin_import(&mut self, import: TcRegistrations) {
        let mut body = String::new();
        let mut candidates = Vec::new();
        let _ = writeln!(
            body,
            "{} Registrierungen mit fehlenden oder nicht unterstützten Angaben übersprungen.\n",
            import.skipped
        );
        for entry in import.plugins {
            let duplicate = self
                .profile
                .plugins
                .iter()
                .any(|installed| installed.same_file(&entry));
            let _ = writeln!(
                body,
                "{}: {} · {} · {}\n  {}",
                if duplicate {
                    "Bereits registriert"
                } else {
                    "Übernehmen"
                },
                entry.name,
                entry.interface.label(),
                entry.architecture.label(),
                display_path(&entry.path)
            );
            if entry.tc_detection.is_some() {
                let _ = writeln!(body, "  TC-Erkennungsregel als Referenz übernommen.");
            }
            if !duplicate {
                candidates.push(entry);
            }
        }
        if candidates.is_empty() {
            self.text("Keine neuen Plugins gefunden", &body);
        } else {
            self.dialog("review", Pending::ApplyPluginImport(candidates), Dialog {
                title: "Plugin-Import prüfen",
                description: "Die Plugins werden deaktiviert registriert. Einfache TC-Erweiterungsregeln werden zu Dateimasken. Weitere Regeln bleiben als Hinweis sichtbar.",
                value: &body,
                ..Dialog::default()
            });
        }
    }

    pub(crate) fn apply_plugin_import(&mut self, import: Vec<Registration>) -> io::Result<()> {
        let mut plugins = self.profile.plugins.clone();
        let first = plugins.len();
        for entry in import {
            if !plugins.iter().any(|existing| existing.same_file(&entry)) {
                plugins.push(entry);
            }
        }
        self.commit_plugins(plugins)?;
        self.show_plugin_manager(Some(first));
        Ok(())
    }

    pub(crate) fn offer_registered_plugins(&mut self, interface: Interface, file: &Path) -> bool {
        let choices: Vec<_> = self
            .profile
            .plugins
            .iter()
            .enumerate()
            .filter(|(_, plugin)| plugin.matches(interface, file))
            .map(|(index, _)| index)
            .collect();
        if choices.is_empty() {
            return false;
        }
        let labels: Vec<_> = choices
            .iter()
            .map(|&index| {
                let entry = &self.profile.plugins[index];
                format!("{} · {}", entry.name, entry.architecture.label())
            })
            .collect();
        let options: Vec<_> = labels
            .iter()
            .map(String::as_str)
            .chain(["Andere Plugin-DLL wählen …"])
            .collect();
        self.dialog(
            "confirm",
            Pending::RegisteredPlugin(interface, file.to_owned(), choices),
            Dialog {
                title: interface.label(),
                description: "Aktivierte passende Plugins in der eingestellten Reihenfolge.",
                options: &options,
                ..Dialog::default()
            },
        );
        true
    }

    pub(crate) fn choose_registered_plugin(
        &mut self,
        interface: Interface,
        file: PathBuf,
        choices: &[usize],
        selection: usize,
    ) {
        if let Some(&index) = choices.get(selection) {
            self.use_registered(index, file);
        } else {
            self.manual_plugin_dialog(interface, file);
        }
    }

    fn use_registered(&mut self, index: usize, file: PathBuf) {
        let Some(entry) = self
            .profile
            .plugins
            .get(index)
            .filter(|entry| entry.enabled)
            .cloned()
        else {
            return;
        };
        if entry.interface == Interface::Wfx {
            self.dialog(
                "input",
                Pending::RegisteredWfx(index),
                Dialog {
                    title: &entry.name,
                    description: "Gib den Pfad innerhalb des Plugin-Dateisystems an.",
                    value: "\\",
                    placeholder: "Pfad im Plugin-Dateisystem",
                    ..Dialog::default()
                },
            );
        } else {
            self.run_plugin(entry.interface, entry.path, file, entry.name);
        }
    }

    pub(crate) fn run_registered_wfx(&mut self, index: usize, value: &str) {
        let Some(entry) = self
            .profile
            .plugins
            .get(index)
            .filter(|entry| entry.enabled && entry.interface == Interface::Wfx)
            .cloned()
        else {
            return;
        };
        self.mount_wfx(entry, value.trim());
    }

    pub(crate) fn preview_registered(&mut self, file: &Path) -> bool {
        let Some(entry) = self
            .profile
            .plugins
            .iter()
            .find(|entry| !entry.masks.is_empty() && entry.matches(Interface::Wlx, file))
            .cloned()
        else {
            return false;
        };
        self.run_plugin(entry.interface, entry.path, file.to_owned(), entry.name);
        true
    }

    pub(crate) fn run_plugin(
        &mut self,
        interface: Interface,
        path: PathBuf,
        file: PathBuf,
        name: String,
    ) {
        self.start_job("Plugin-Host wird gestartet …", false, move |job| {
            if interface == Interface::Wlx {
                let host = plugin::view(&path, &file, &job.cancel)
                    .map_err(|error| plugin_error(&name, error))?;
                Ok(Outcome::PluginPreview(name, host))
            } else {
                let rows = if interface == Interface::Wfx {
                    plugin::list_wfx(&path, &file, &job.cancel, &mut |prompt| {
                        job.prompt(&name, prompt)
                    })
                } else {
                    plugin::inspect(interface, &path, &file, &job.cancel)
                }
                .map_err(|error| plugin_error(&name, error))?;
                Ok(Outcome::Text(
                    format!("{} · {name}", interface.label()),
                    describe(rows),
                ))
            }
        });
    }

    pub(super) fn active_plugin_file(&self) -> Option<PathBuf> {
        if self.panels[self.active].loading || !self.current().is_local() {
            return None;
        }
        self.current()
            .display_row(self.current().cursor)
            .filter(|entry| entry.kind == Kind::File)
            .map(|entry| entry.path.clone())
    }
}

fn selected_if(selected: &str, matches: impl FnOnce(&Path) -> bool) -> &str {
    if matches(Path::new(selected)) {
        selected
    } else {
        ""
    }
}

fn extension(path: &Path, expected: &str) -> bool {
    path.extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case(expected))
}

pub(super) fn user_path(path: &str) -> PathBuf {
    path.trim().trim_matches('"').into()
}

pub(super) fn plugin_error(name: &str, error: io::Error) -> io::Error {
    io::Error::new(error.kind(), format!("Plugin „{name}“: {error}"))
}

fn describe(rows: Vec<plugin::Row>) -> String {
    let mut body = String::new();
    for row in rows {
        let _ = writeln!(body, "{}    {}", row.name, row.value);
    }
    body
}
