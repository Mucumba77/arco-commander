use super::{App, Navigation, Outcome};
use crate::commands::{Dialog, Pending};
use crate::display_path;
use crate::panel::{FtpLocation, FtpTab, Location, Panel, Tab, WfxLocation};
use arco_core::plugin::Interface;
use arco_core::profile::{self, Profile, SavedTab, TabLock, TcTabs, VirtualTab};
use arco_windows::ftp::Listing;
use std::fmt::Write as _;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;

pub(super) struct ClosedTab {
    side: usize,
    location: Location,
    fallback: PathBuf,
    options: SavedTab,
    base: Option<Location>,
    deferred: Option<VirtualTab>,
}

impl App {
    pub(super) fn remember_closed_tab(&mut self, side: usize, tab: Tab) {
        self.closed_tabs.push(ClosedTab {
            side,
            location: tab.location(),
            fallback: tab.path,
            options: tab.options,
            base: tab.lock_base,
            deferred: tab.deferred,
        });
        if self.closed_tabs.len() > 20 {
            self.closed_tabs.remove(0);
        }
    }

    pub(super) fn forget_closed_ftp_tabs(&mut self, session: &Arc<arco_windows::ftp::Session>) {
        let refers = |location: &Location| matches!(location, Location::Ftp(ftp) if Arc::ptr_eq(session, &ftp.session));
        self.closed_tabs
            .retain(|tab| !refers(&tab.location) && !tab.base.as_ref().is_some_and(refers));
    }

    pub(crate) fn show_closed_tabs(&mut self) {
        if self.closed_tabs.is_empty() {
            self.status("In dieser Sitzung wurden noch keine Tabs geschlossen.");
            return;
        }
        let titles: Vec<_> = self
            .closed_tabs
            .iter()
            .rev()
            .map(|tab| {
                let side = if tab.side == 0 { "Links" } else { "Rechts" };
                let path = tab.location.display();
                if tab.options.title.is_empty() {
                    format!("{side} · {path}")
                } else {
                    format!("{side} · {} · {path}", tab.options.title)
                }
            })
            .collect();
        let options: Vec<_> = titles.iter().map(String::as_str).collect();
        self.dialog(
            "confirm",
            Pending::ReopenTab,
            Dialog {
                title: "Kürzlich geschlossene Tabs",
                description: "Der Tab öffnet sich auf seiner ursprünglichen Seite.",
                options: &options,
                ..Dialog::default()
            },
        );
    }

    pub(crate) fn reopen_tab(&mut self, option: usize) {
        let Some(index) = option
            .checked_add(1)
            .and_then(|offset| self.closed_tabs.len().checked_sub(offset))
        else {
            return;
        };
        let closed = self.closed_tabs.remove(index);
        let mut tab = Tab::new(closed.fallback);
        tab.options = closed.options;
        tab.lock_base = closed.base;
        tab.deferred = closed.deferred;
        let panel = &mut self.panels[closed.side];
        panel.tabs.push(tab);
        panel.tab = panel.tabs.len() - 1;
        self.set_active(closed.side);
        if !self.refresh_deferred_tab(closed.side) {
            self.load_location(closed.side, closed.location, Navigation::Refresh);
        }
    }

    pub(crate) fn open_selected_folders(&mut self, side: usize) {
        if self.panels[self.active].loading || !self.current().is_local() {
            self.status("Wähle lokale Ordner in einem geladenen Dateifenster.");
            return;
        }
        let folders = self.current().selected_directories();
        if folders.is_empty() {
            self.status("Wähle mindestens einen lokalen Ordner.");
            return;
        }
        let count = folders.len();
        self.panels[side]
            .tabs
            .extend(folders.into_iter().map(Tab::new));
        self.publish(side);
        self.status(format!("{count} Ordner als Hintergrund-Tabs geöffnet."));
    }

    pub(crate) fn show_tab_groups(&mut self) {
        let names: Vec<_> = self.profile.tab_groups.keys().cloned().collect();
        let options: Vec<_> = [
            "Neue Gruppe speichern …",
            "Total-Commander-Datei importieren …",
            "Offene Tabs als .tab exportieren …",
        ]
        .into_iter()
        .chain(names.iter().map(String::as_str))
        .collect();
        self.dialog(
            "confirm",
            Pending::TabGroups(names.clone()),
            Dialog {
                title: "Tab-Gruppen",
                description: "Gruppen speichern die Ordner-Tabs beider Dateifenster.",
                options: &options,
                ..Dialog::default()
            },
        );
    }

    pub(crate) fn choose_tab_group(&mut self, names: &[String], option: usize) {
        if option == 0 {
            self.dialog(
                "input",
                Pending::TabGroupSave,
                Dialog {
                    title: "Neue Tab-Gruppe speichern",
                    placeholder: "Gruppenname",
                    ..Dialog::default()
                },
            );
        } else if option == 1 {
            self.tab_import_dialog();
        } else if option == 2 {
            self.tab_export_dialog(None);
        } else if let Some(name) = names.get(option - 3) {
            self.show_tab_group_options(name);
        }
    }

    fn show_tab_group_options(&mut self, name: &str) {
        self.dialog(
            "confirm",
            Pending::TabGroupAction(name.to_owned()),
            Dialog {
                title: name,
                options: &[
                    "Tabs anhängen",
                    "Offene Tabs ersetzen …",
                    "Gruppe löschen …",
                    "Als .tab exportieren …",
                ],
                ..Dialog::default()
            },
        );
    }

    pub(crate) fn save_tab_group(&mut self, name: &str) -> io::Result<()> {
        let name = name.trim();
        self.validate_group_name(name)?;
        let mut profile = self.profile_snapshot();
        profile
            .tab_groups
            .insert(name.to_owned(), profile.panels.clone());
        self.commit_tab_group(profile)?;
        self.status(format!("Tab-Gruppe gespeichert · {name}"));
        Ok(())
    }

    fn validate_group_name(&self, name: &str) -> io::Result<()> {
        if name.is_empty() || name.chars().count() > 120 || name.chars().any(char::is_control) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Gib einen Gruppennamen mit 1 bis 120 Zeichen ein.",
            ));
        }
        if self
            .profile
            .tab_groups
            .keys()
            .any(|key| key.to_lowercase() == name.to_lowercase())
        {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "Eine Gruppe mit diesem Namen ist bereits gespeichert.",
            ));
        }
        Ok(())
    }

    pub(crate) fn tab_import_dialog(&mut self) {
        let selected = self
            .current()
            .current()
            .filter(|entry| {
                entry.path.extension().is_some_and(|ext| {
                    ext.eq_ignore_ascii_case("tab") || ext.eq_ignore_ascii_case("ini")
                })
            })
            .map(|entry| display_path(&entry.path))
            .unwrap_or_default();
        self.dialog("input", Pending::TabImport, Dialog {
            title: "Total-Commander-Tabs importieren",
            description: "Bei .tab-Dateien erhält die gewählte Seite die aktiven Tabs. Bei einer .ini-Datei bleiben links und rechts erhalten. Vor dem Speichern erscheint eine Vorschau.",
            value: &selected,
            placeholder: "Absoluter Pfad zur .tab- oder .ini-Datei",
            second_placeholder: "Gruppenname (optional)",
            ..Dialog::default()
        });
    }

    pub(crate) fn read_tab_import(&mut self, path: &str, name: &str) -> io::Result<()> {
        let path = PathBuf::from(path.trim().trim_matches('"'));
        if !path.is_absolute()
            || !path.extension().is_some_and(|ext| {
                ext.eq_ignore_ascii_case("tab") || ext.eq_ignore_ascii_case("ini")
            })
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Gib den absoluten Pfad zu einer .tab- oder .ini-Datei ein.",
            ));
        }
        let name = if name.trim().is_empty() {
            path.file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .trim()
                .to_owned()
        } else {
            name.trim().to_owned()
        };
        self.validate_group_name(&name)?;
        let bases = self
            .panels
            .each_ref()
            .map(|panel| panel.current().path.clone());
        let source = self.active;
        let layouts = self.profile.views.tc_columns.clone();
        self.start_job("Lese gespeicherte Ordner-Tabs …", false, move |_| {
            let tabs = profile::read_tc_tabs_with_columns(
                &path,
                [&bases[0], &bases[1]],
                source,
                super::favorites::tc_path_variables(&path),
                &layouts,
            )?;
            Ok(Outcome::ImportedTabs(name, Box::new(tabs)))
        });
        Ok(())
    }

    pub(crate) fn tab_export_dialog(&mut self, group: Option<String>) {
        let initial = self.current().path.join("Ordner-Tabs.tab");
        let initial = display_path(&initial);
        self.dialog("input", Pending::TabExport(group), Dialog {
            title: "Ordner-Tabs als .tab exportieren",
            description: "Total Commander ordnet die aktive Seite der Datei seiner beim Import aktiven Seite zu. Wähle einen neuen Dateinamen.",
            value: &initial,
            placeholder: "Absoluter Zielpfad",
            options: &["Beide Seiten", "Nur linke Seite", "Nur rechte Seite"],
            ..Dialog::default()
        });
    }

    pub(crate) fn write_tab_export(
        &mut self,
        group: Option<&str>,
        value: &str,
        option: usize,
    ) -> io::Result<()> {
        let mut panels = if let Some(group) = group {
            self.profile
                .tab_groups
                .get(group)
                .cloned()
                .ok_or_else(|| io::Error::other("Tab-Gruppe fehlt."))?
        } else {
            self.panels.each_ref().map(Panel::saved)
        };
        let source = match option {
            1 => {
                panels[1] = Default::default();
                0
            }
            2 => {
                panels[0] = Default::default();
                1
            }
            _ => self.active,
        };
        let path = PathBuf::from(value.trim().trim_matches('"'));
        self.start_tab_export(path, panels, source);
        Ok(())
    }

    fn start_tab_export(&mut self, path: PathBuf, panels: [profile::SavedPanel; 2], source: usize) {
        let layouts = self.profile.views.tc_columns.clone();
        let columns = self.profile.views.columns.clone();
        self.start_job("Exportiere Ordner-Tabs …", false, move |_| {
            profile::write_tc_tabs_with_views(&path, &panels, source, &layouts, &columns)?;
            Ok(Outcome::Notice(format!(
                "Tab-Datei gespeichert · {}",
                display_path(&path)
            )))
        });
    }

    pub(crate) fn tc_tab_command(&mut self, verb: &str, file: &str) -> bool {
        let verb = verb.to_ascii_uppercase();
        let path = PathBuf::from(file.trim().trim_matches('"'));
        let load_side = match verb.as_str() {
            "OPENTABS" | "APPENDTABS" => Some(self.active),
            "OPENTABSL" | "APPENDTABSL" => Some(0),
            "OPENTABSR" | "APPENDTABSR" => Some(1),
            _ => None,
        };
        if let Some(source) = load_side {
            if !path
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("tab"))
            {
                self.status("Gib eine .tab-Datei an.");
                return true;
            }
            let path = self.current().path.join(path);
            let bases = self
                .panels
                .each_ref()
                .map(|panel| panel.current().path.clone());
            let replace = verb.starts_with("OPEN");
            let layouts = self.profile.views.tc_columns.clone();
            self.start_job("Lese gespeicherte Ordner-Tabs …", false, move |_| {
                let tabs = profile::read_tc_tabs_with_columns(
                    &path,
                    [&bases[0], &bases[1]],
                    source,
                    super::favorites::tc_path_variables(&path),
                    &layouts,
                )?;
                Ok(Outcome::TabFile(Box::new(tabs), replace))
            });
            return true;
        }
        let (source, both) = match verb.as_str() {
            "SAVETABS" => (self.active, false),
            "SAVETABSL" => (0, false),
            "SAVETABSR" => (1, false),
            "SAVETABS2" => (self.active, true),
            "SAVETABS2L" => (0, true),
            "SAVETABS2R" => (1, true),
            _ => return false,
        };
        let mut panels = self.panels.each_ref().map(Panel::saved);
        if !both {
            panels[1 - source] = Default::default();
        }
        self.start_tab_export(self.current().path.join(path), panels, source);
        true
    }

    pub(super) fn apply_tab_file(&mut self, mut import: TcTabs, replace: bool) {
        let embedded = import.embedded_column_count();
        if let Err(error) = import.install_embedded_columns(&mut self.profile.views.columns) {
            self.text("Tab-Datei konnte nicht geöffnet werden", &error.to_string());
            return;
        }
        self.apply_saved_tabs(import.panels, replace);
        self.status(format!(
            "Tab-Datei geöffnet · {embedded} eingebettete Spaltensätze · {} übersprungen · {} weitere Optionen",
            import.skipped, import.unhandled_options,
        ));
        self.save();
    }

    pub(super) fn review_tab_import(&mut self, name: String, import: TcTabs) {
        let description = format!(
            "Speichert die Gruppe „{name}“. {} eingebettete Spaltensätze, {} Tabs übersprungen, {} Einträge mit weiteren oder unbekannten Optionen.",
            import.embedded_column_count(),
            import.skipped,
            import.unhandled_options,
        );
        let mut body = String::new();
        for (side, panel) in ["Links", "Rechts"].into_iter().zip(&import.panels) {
            let _ = writeln!(body, "{side}");
            for (index, (path, options)) in panel.tabs.iter().zip(&panel.tab_options).enumerate() {
                let active = if index == panel.active_tab {
                    " [aktiv]"
                } else {
                    ""
                };
                let lock = match options.lock {
                    TabLock::Unlocked => "",
                    TabLock::Fixed => " [gesperrt]",
                    TabLock::ReturnToBase => " [Rückkehr zum Stammordner]",
                };
                let address = options
                    .virtual_tab
                    .as_ref()
                    .and_then(VirtualTab::tc_path)
                    .or_else(|| options.shell.clone())
                    .unwrap_or_else(|| display_path(path));
                let _ = writeln!(body, "  {}{}{} {}", options.title, active, lock, address);
            }
            body.push('\n');
        }
        self.dialog(
            "review",
            Pending::ApplyTabImport(name, Box::new(import)),
            Dialog {
                title: "Tab-Import prüfen",
                description: &description,
                value: &body,
                ..Dialog::default()
            },
        );
    }

    pub(crate) fn save_imported_tabs(&mut self, name: &str, mut import: TcTabs) -> io::Result<()> {
        self.validate_group_name(name)?;
        let mut profile = self.profile_snapshot();
        import.install_embedded_columns(&mut profile.views.columns)?;
        profile.tab_groups.insert(name.to_owned(), import.panels);
        self.commit_tab_group(profile)?;
        self.status(format!("Tab-Gruppe importiert · {name}"));
        self.show_tab_group_options(name);
        Ok(())
    }

    pub(crate) fn tab_group_action(&mut self, name: &str, option: usize) -> io::Result<()> {
        match option {
            0 => self.load_tab_group(name, false)?,
            1 => self.dialog(
                "confirm",
                Pending::TabGroupReplace(name.to_owned()),
                Dialog {
                    title: "Offene Tabs ersetzen",
                    description: &format!(
                        "Die offenen Tabs auf den in „{name}“ gespeicherten Seiten ersetzen?"
                    ),
                    ..Dialog::default()
                },
            ),
            2 => self.dialog(
                "confirm",
                Pending::TabGroupDelete(name.to_owned()),
                Dialog {
                    title: "Tab-Gruppe löschen",
                    description: &format!("Die Gruppe „{name}“ entfernen?"),
                    ..Dialog::default()
                },
            ),
            3 => self.tab_export_dialog(Some(name.to_owned())),
            _ => (),
        }
        Ok(())
    }

    pub(crate) fn load_tab_group(&mut self, name: &str, replace: bool) -> io::Result<()> {
        let group = self
            .profile
            .tab_groups
            .get(name)
            .ok_or_else(|| io::Error::other("Tab-Gruppe fehlt."))?;
        if group
            .iter()
            .any(|panel| panel.tabs.iter().any(|path| !path.is_absolute()))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Die Tab-Gruppe enthält ungültige lokale Pfade.",
            ));
        }
        self.apply_saved_tabs(group.clone(), replace);
        self.status(format!("Tab-Gruppe geladen · {name}"));
        self.save();
        Ok(())
    }

    fn apply_saved_tabs(&mut self, panels: [profile::SavedPanel; 2], replace: bool) {
        for (side, saved) in panels.into_iter().enumerate() {
            if saved.tabs.is_empty() {
                continue;
            }
            let new_panel = Panel::from_saved(saved, &self.panels[side].current().path);
            if replace {
                self.panels[side] = new_panel;
                self.refresh(side);
            } else {
                self.panels[side].tabs.extend(new_panel.tabs);
                self.publish(side);
            }
        }
    }

    pub(super) fn refresh_deferred_tab(&mut self, side: usize) -> bool {
        let Some(deferred) = self.panels[side].current().deferred.clone() else {
            return false;
        };
        let location = match deferred {
            VirtualTab::Archive { file, folder } => Some(Location::Archive { file, folder }),
            VirtualTab::Wfx { plugin, directory } => {
                let mut matches = self.profile.plugins.iter().filter(|entry| {
                    entry.enabled
                        && entry.interface == Interface::Wfx
                        && entry.name.eq_ignore_ascii_case(&plugin)
                });
                match (matches.next(), matches.next()) {
                    (Some(entry), None) => Some(Location::Wfx(WfxLocation {
                        plugin: Arc::new(entry.clone()),
                        directory,
                    })),
                    _ => {
                        if side == self.active {
                            self.status(format!("WFX-Tab · Das aktive Plugin „{plugin}“ fehlt oder ist nicht eindeutig."));
                        }
                        None
                    }
                }
            }
            VirtualTab::Ftp { profile, url } => {
                let mut sessions = self
                    .panels
                    .iter()
                    .flat_map(|panel| &panel.tabs)
                    .filter_map(|tab| tab.ftp.as_ref())
                    .filter(|ftp| ftp.location.profile.as_deref() == Some(profile.as_str()));
                let location = sessions.find_map(|ftp| {
                    let session = &ftp.location.session;
                    let directory = session.restore_directory(url.as_deref()).ok()?;
                    Some(Location::Ftp(FtpLocation {
                        session: session.clone(),
                        directory,
                        profile: Some(profile.clone()),
                    }))
                });
                if location.is_none() && side == self.active {
                    self.status(format!(
                        "FTP-Tab · Mit Enter das Profil „{profile}“ verbinden."
                    ));
                }
                location
            }
        };
        if let Some(location) = location {
            self.load_location(side, location, Navigation::Refresh);
        } else {
            self.waiting_loads[side] = None;
            if let Some(cancel) = &self.loader_cancel[side] {
                cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            }
            self.panels[side].loading = false;
            self.panels[side].request = 0;
            self.publish(side);
        }
        true
    }

    pub(super) fn open_deferred_tab(&mut self) -> bool {
        if let Some(VirtualTab::Ftp { profile, .. }) = self.current().deferred.clone() {
            if let Err(error) = self.connect_deferred_ftp(&profile) {
                self.status(error.to_string());
            }
            true
        } else {
            self.refresh_deferred_tab(self.active)
        }
    }

    pub(crate) fn delete_tab_group(&mut self, name: &str) -> io::Result<()> {
        let mut profile = self.profile_snapshot();
        profile.tab_groups.remove(name);
        self.commit_tab_group(profile)?;
        self.status(format!("Tab-Gruppe entfernt · {name}"));
        Ok(())
    }

    fn commit_tab_group(&mut self, profile: Profile) -> io::Result<()> {
        if !self.save_profile {
            return Err(io::Error::other(
                "Das Arco-Profil konnte beim Start nicht geladen werden.",
            ));
        }
        profile.save(&self.profile_path)?;
        self.profile = profile;
        Ok(())
    }

    pub(crate) fn show_tab_menu(&mut self) {
        let description = match self.current().options.lock {
            TabLock::Unlocked => "Ordnerwechsel in diesem Tab sind erlaubt.",
            TabLock::Fixed => {
                "Der Ordner dieses Tabs ist fest. Ordnerwechsel öffnen einen neuen Tab."
            }
            TabLock::ReturnToBase => "Beim Aktivieren kehrt der Tab in seinen Stammordner zurück.",
        };
        self.dialog(
            "confirm",
            Pending::TabMenu,
            Dialog {
                title: "Aktiven Ordner-Tab verwalten",
                description,
                options: &[
                    "Umbenennen …",
                    "Im Vordergrund duplizieren",
                    "Im Hintergrund duplizieren",
                    "Ins andere Dateifenster kopieren",
                    "Ins andere Dateifenster verschieben",
                    "In der Leiste nach links",
                    "In der Leiste nach rechts",
                    "Ordner fest sperren",
                    "Sperren, Ordnerwechsel erlauben",
                    "Sperre aufheben",
                    "Zum gespeicherten Stammordner",
                    "Tab schließen",
                ],
                ..Dialog::default()
            },
        );
    }

    pub(crate) fn show_tab_list(&mut self) {
        let labels: Vec<_> = self.panels[self.active]
            .tabs
            .iter()
            .enumerate()
            .map(|(index, tab)| {
                let location = tab.location().display();
                if tab.options.title.is_empty() {
                    format!("{} · {location}", index + 1)
                } else {
                    format!("{} · {} · {location}", index + 1, tab.options.title)
                }
            })
            .collect();
        let options: Vec<_> = labels.iter().map(String::as_str).collect();
        self.dialog(
            "confirm",
            Pending::TabList,
            Dialog {
                title: "Ordner-Tabs",
                description: "Wähle einen Tab im aktiven Dateifenster.",
                options: &options,
                ..Dialog::default()
            },
        );
    }

    pub(crate) fn tab_menu_action(&mut self, option: usize) {
        let side = self.active;
        match option {
            0 => {
                let title = self.current().options.title.clone();
                self.dialog(
                    "input",
                    Pending::TabRename,
                    Dialog {
                        title: "Ordner-Tab umbenennen",
                        description: "Ein leeres Feld verwendet den Ordnernamen.",
                        value: &title,
                        placeholder: "Tab-Titel",
                        ..Dialog::default()
                    },
                );
            }
            1 => self.duplicate_tab(side, true),
            2 => self.duplicate_tab(side, false),
            3 => self.duplicate_tab(1 - side, true),
            4 => self.move_tab_to_other_side(),
            5 => self.reorder_tab(-1),
            6 => self.reorder_tab(1),
            7 => self.set_tab_lock(TabLock::Fixed),
            8 => self.set_tab_lock(TabLock::ReturnToBase),
            9 => self.set_tab_lock(TabLock::Unlocked),
            10 => {
                self.return_to_tab_base();
            }
            11 => self.request_close_tab(),
            _ => (),
        }
    }

    pub(crate) fn rename_tab(&mut self, title: &str) -> io::Result<()> {
        let title = title.trim();
        if title.chars().count() > 120 || title.chars().any(char::is_control) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Tab-Titel dürfen höchstens 120 Zeichen enthalten.",
            ));
        }
        self.current_mut().options.title = title.into();
        self.publish(self.active);
        self.save();
        Ok(())
    }

    pub(crate) fn toggle_tab_lock(&mut self, mode: TabLock) {
        self.set_tab_lock(if self.current().options.lock == mode {
            TabLock::Unlocked
        } else {
            mode
        });
    }

    fn set_tab_lock(&mut self, mode: TabLock) {
        if self.panels[self.active].loading {
            self.status("Warte, bis der Ordner geladen ist.");
            return;
        }
        if self.current().options.lock != mode {
            self.current_mut().set_lock(mode);
            self.publish(self.active);
            self.save();
        }
    }

    pub(crate) fn return_to_tab_base(&mut self) -> bool {
        let Some(base) = self.current().lock_base.clone() else {
            return false;
        };
        if self.current().location() != base || self.panels[self.active].loading {
            self.load_location(self.active, base, Navigation::Jump);
        }
        true
    }

    fn duplicate_tab(&mut self, side: usize, activate: bool) {
        let source = self.current();
        let mut tab = Tab::new(source.path.clone());
        tab.archive = source.archive.clone();
        tab.ftp = source.ftp.as_ref().map(|ftp| FtpTab {
            location: ftp.location.clone(),
            listing: Listing {
                entries: Vec::new(),
                format: ftp.listing.format,
            },
        });
        tab.wfx = source.wfx.clone();
        tab.shell = source.shell.clone();
        tab.deferred = source.deferred.clone();
        tab.options = source.options.clone();
        tab.options.lock = TabLock::Unlocked;
        let panel = &mut self.panels[side];
        panel.tabs.push(tab);
        if activate {
            panel.tab = panel.tabs.len() - 1;
            self.refresh(side);
        } else {
            self.publish(side);
        }
    }

    fn reorder_tab(&mut self, delta: isize) {
        let panel = &mut self.panels[self.active];
        if let Some(next) = panel
            .tab
            .checked_add_signed(delta)
            .filter(|&next| next < panel.tabs.len())
        {
            panel.tabs.swap(panel.tab, next);
            panel.tab = next;
            self.publish(self.active);
            self.save();
        }
    }

    fn move_tab_to_other_side(&mut self) {
        if self.panels.iter().any(|panel| panel.loading) {
            self.status("Warte, bis beide Dateifenster geladen sind.");
            return;
        }
        let side = self.active;
        let source = &mut self.panels[side];
        let moved = source.tabs.remove(source.tab);
        if source.tabs.is_empty() {
            source.tabs.push(Tab::new(moved.path.clone()));
        }
        source.tab = source.tab.min(source.tabs.len() - 1);
        source.request = 0;
        let destination = &mut self.panels[1 - side];
        destination.tabs.push(moved);
        destination.tab = destination.tabs.len() - 1;
        destination.request = 0;
        self.publish(side);
        if !self.return_to_tab_base() && !self.panels[side].current().loaded {
            self.refresh(side);
        }
        self.set_active(1 - side);
        self.publish(1 - side);
        self.save();
    }

    pub(crate) fn request_close_tab(&mut self) {
        if self.panels[self.active].tabs.len() <= 1 {
            return;
        }
        if self.current().options.lock == TabLock::Unlocked {
            self.close_tab();
        } else {
            self.dialog(
                "confirm",
                Pending::TabClose,
                Dialog {
                    title: "Gesperrten Tab schließen",
                    description: "Diesen gesperrten Ordner-Tab schließen?",
                    ..Dialog::default()
                },
            );
        }
    }
}
