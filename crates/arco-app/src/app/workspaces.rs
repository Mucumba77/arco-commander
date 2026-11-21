use super::{App, Navigation, buttonbars::join_windows_arguments};
use crate::commands::{Dialog, Pending};
use crate::panel::Panel;
use arco_core::profile::{Profile, SavedWorkspace, VirtualTab};
use std::io;
use std::sync::Arc;

mod workspace_import;
mod workspace_jumps;

#[derive(Clone)]
pub(crate) enum WorkspaceDialog {
    List(Vec<String>),
    Name,
    Manage(String),
    Open(String),
    Replace(String),
    Remove(String),
    ImportSource,
    ImportReview(Arc<Vec<arco_core::winscp::ImportedWorkspace>>),
}

impl App {
    pub(crate) fn show_workspaces(&mut self) {
        let names: Vec<_> = self.profile.workspaces.keys().cloned().collect();
        let options: Vec<_> = [
            crate::locale::tr("Aktuellen Arbeitsbereich speichern …"),
            crate::locale::tr("WinSCP-Arbeitsbereiche importieren …"),
            crate::locale::tr(if self.profile.workspace_jump_list {
                "Windows-Sprungliste deaktivieren"
            } else {
                "Windows-Sprungliste aktivieren"
            }),
        ]
        .into_iter()
        .chain(names.iter().map(String::as_str))
        .collect();
        self.dialog("confirm", Pending::Workspace(WorkspaceDialog::List(names.clone())), Dialog {
            title: "Arbeitsbereiche",
            description: "Arbeitsbereiche merken sich die Tabs und Anordnung beider Dateifenster.",
            options: &options,
            confirm_label: "Weiter",
            ..Dialog::default()
        });
    }

    pub(crate) fn ask_workspace_name(&mut self) {
        self.dialog(
            "input",
            Pending::Workspace(WorkspaceDialog::Name),
            Dialog {
                title: "Arbeitsbereich speichern",
                placeholder: "Name des Arbeitsbereichs",
                confirm_label: "Speichern",
                ..Dialog::default()
            },
        );
    }

    pub(crate) fn workspace_dialog(
        &mut self,
        dialog: WorkspaceDialog,
        value: &str,
        option: usize,
    ) -> io::Result<()> {
        match dialog {
            WorkspaceDialog::List(names) => {
                if option == 0 {
                    self.ask_workspace_name();
                } else if option == 1 {
                    self.show_workspace_import_sources();
                } else if option == 2 {
                    self.toggle_workspace_jump_list()?;
                } else if let Some(name) = names.get(option - 3) {
                    self.dialog(
                        "confirm",
                        Pending::Workspace(WorkspaceDialog::Manage(name.clone())),
                        Dialog {
                            title: name,
                            options: &[
                                crate::locale::tr("Arbeitsbereich öffnen …"),
                                crate::locale::tr("Mit aktuellem Stand überschreiben …"),
                                crate::locale::tr("Arbeitsbereich-Verknüpfung speichern …"),
                                crate::locale::tr("Arbeitsbereich entfernen …"),
                                crate::locale::tr(
                                    if self
                                        .profile
                                        .workspaces
                                        .get(name)
                                        .is_some_and(|workspace| workspace.auto_save)
                                    {
                                        "Automatisch speichern deaktivieren"
                                    } else {
                                        "Automatisch speichern und wieder öffnen"
                                    },
                                ),
                            ],
                            confirm_label: "Weiter",
                            ..Dialog::default()
                        },
                    );
                }
            }
            WorkspaceDialog::Name => {
                let name = workspace_name(value)?;
                if let Some(existing) = self
                    .profile
                    .workspaces
                    .keys()
                    .find(|key| key.to_lowercase() == name.to_lowercase())
                {
                    self.confirm_workspace(WorkspaceDialog::Replace(existing.clone()));
                } else {
                    self.store_workspace(name)?;
                }
            }
            WorkspaceDialog::Manage(name) => match option {
                0 => self.confirm_workspace(WorkspaceDialog::Open(name)),
                1 => self.confirm_workspace(WorkspaceDialog::Replace(name)),
                2 => self.save_workspace_shortcut(&name)?,
                3 => self.confirm_workspace(WorkspaceDialog::Remove(name)),
                4 => {
                    let mut updated = self.profile_snapshot();
                    let workspace = updated
                        .workspaces
                        .get_mut(&name)
                        .ok_or_else(missing_workspace)?;
                    workspace.auto_save = !workspace.auto_save;
                    self.commit_workspace_profile(updated)?;
                    self.status(crate::locale::tr("Arbeitsbereich-Einstellung gespeichert."));
                }
                _ => (),
            },
            WorkspaceDialog::Open(name) => self.open_workspace(&name)?,
            WorkspaceDialog::Replace(name) => self.store_workspace(&name)?,
            WorkspaceDialog::Remove(name) => {
                let mut updated = self.profile_snapshot();
                updated.workspaces.remove(&name);
                updated.recent_workspaces.retain(|entry| entry != &name);
                if updated.active_workspace.as_deref() == Some(&name) {
                    updated.active_workspace = None;
                }
                self.commit_workspace_profile(updated)?;
                self.status(crate::locale::tr("Arbeitsbereich entfernt."));
                self.update_workspace_jump_list();
                self.show_workspaces();
            }
            WorkspaceDialog::ImportSource => self.import_workspaces(option)?,
            WorkspaceDialog::ImportReview(import) => {
                if let Some((name, tabs)) = import.get(option) {
                    match tabs {
                        Ok(tabs) => self.save_imported_workspace(name, tabs)?,
                        Err(error) => return Err(io::Error::other(error.to_string())),
                    }
                }
            }
        }
        Ok(())
    }

    fn confirm_workspace(&mut self, action: WorkspaceDialog) {
        let (name, title, label) = match &action {
            WorkspaceDialog::Open(name) => (
                name,
                "Offene Tabs durch den Arbeitsbereich ersetzen?",
                "Öffnen",
            ),
            WorkspaceDialog::Replace(name) => (
                name,
                "Gespeicherten Arbeitsbereich überschreiben?",
                "Überschreiben",
            ),
            WorkspaceDialog::Remove(name) => {
                (name, "Gespeicherten Arbeitsbereich entfernen?", "Entfernen")
            }
            _ => return,
        };
        self.dialog(
            "confirm",
            Pending::Workspace(action.clone()),
            Dialog {
                title,
                description: name,
                confirm_label: label,
                ..Dialog::default()
            },
        );
    }

    fn store_workspace(&mut self, name: &str) -> io::Result<()> {
        let mut updated = self.profile_snapshot();
        let workspace = self.current_workspace(&updated, name)?;
        updated.workspaces.insert(name.to_owned(), workspace);
        updated.active_workspace = Some(name.to_owned());
        workspace_jumps::remember(&mut updated, name);
        self.commit_workspace_profile(updated)?;
        self.status(crate::locale::tr("Arbeitsbereich gespeichert."));
        self.update_workspace_jump_list();
        Ok(())
    }

    pub(super) fn save_active_workspace(&mut self) -> io::Result<()> {
        let Some(name) = self.profile.active_workspace.clone().filter(|name| {
            self.profile
                .workspaces
                .get(name)
                .is_some_and(|workspace| workspace.auto_save)
        }) else {
            return Ok(());
        };
        let mut updated = self.profile_snapshot();
        // Preserve the saved version when a tab is loading or needs a named profile.
        if let Ok(workspace) = self.current_workspace(&updated, &name) {
            updated.workspaces.insert(name, workspace);
            self.commit_workspace_profile(updated)?;
        }
        Ok(())
    }

    fn current_workspace(&self, updated: &Profile, name: &str) -> io::Result<SavedWorkspace> {
        if self.panels.iter().any(|panel| panel.loading) {
            return Err(io::Error::other(
                "Warte, bis beide Dateifenster geladen sind.",
            ));
        }
        for tab in self.panels.iter().flat_map(|panel| &panel.tabs) {
            if let Some(ftp) = &tab.ftp
                && !ftp
                    .location
                    .profile
                    .as_ref()
                    .is_some_and(|name| self.profile.ftp_profiles.contains_key(name))
            {
                return Err(io::Error::other(
                    "Speichere die geöffneten Netzwerkverbindungen als Profile, bevor du den Arbeitsbereich speicherst.",
                ));
            }
        }
        let workspace = SavedWorkspace {
            panels: updated.panels.clone(),
            active_panel: updated.active_panel,
            stacked: updated.stacked,
            split_ratio: updated.split_ratio.clamp(0.37, 0.63),
            separate_tree_mode: updated.separate_tree_mode,
            linked_navigation: self.workspace_link_enabled(),
            auto_save: updated
                .workspaces
                .get(name)
                .is_some_and(|workspace| workspace.auto_save),
        };
        workspace.validate()?;
        Ok(workspace)
    }

    fn commit_workspace_profile(&mut self, updated: Profile) -> io::Result<()> {
        if !self.save_profile {
            return Err(io::Error::other(
                "Das Arco-Profil konnte beim Start nicht geladen werden.",
            ));
        }
        updated.save(&self.profile_path)?;
        self.profile = updated;
        Ok(())
    }

    pub(crate) fn open_workspace(&mut self, name: &str) -> io::Result<()> {
        if self.job.is_some() {
            return Err(io::Error::other(
                "Warte, bis der laufende Auftrag abgeschlossen ist.",
            ));
        }
        let workspace = self
            .profile
            .workspaces
            .get(name)
            .ok_or_else(missing_workspace)?
            .clone();
        workspace.validate()?;
        if self.profile.active_workspace.as_deref() != Some(name) {
            self.save_active_workspace()?;
        }
        self.profile.active_workspace = Some(name.to_owned());
        let [left, right] = workspace.panels;
        let panels = [
            Panel::from_saved(left, &self.panels[0].current().path),
            Panel::from_saved(right, &self.panels[1].current().path),
        ];
        let reusable = panels
            .each_ref()
            .map(|panel| self.deferred_ftp_location(panel.current().deferred.as_ref()?));
        self.panels = panels;
        self.set_pending_navigation_link(workspace.linked_navigation);
        self.profile.stacked = workspace.stacked;
        self.ui().set_stacked(workspace.stacked);
        self.ui().set_split_ratio(workspace.split_ratio);
        self.ui().set_full_panel(-1);
        self.set_active(workspace.active_panel);
        self.set_separate_trees(workspace.separate_tree_mode);
        for (side, request) in reusable.into_iter().enumerate() {
            if let Some(location) = request {
                self.invalidate_tree(side);
                self.load_location(side, location, Navigation::Refresh);
            } else {
                self.refresh(side);
            }
        }
        self.ui().invoke_focus_panels();
        workspace_jumps::remember(&mut self.profile, name);
        self.save();
        let needs_login = self.panels.iter().any(|panel| {
            !panel.loading && matches!(panel.current().deferred, Some(VirtualTab::Ftp { .. }))
        });
        self.status(crate::locale::tr(if needs_login {
            "Arbeitsbereich geöffnet. Server-Tabs lassen sich mit Eingabe verbinden."
        } else {
            "Arbeitsbereich geöffnet."
        }));
        self.update_workspace_jump_list();
        Ok(())
    }

    fn save_workspace_shortcut(&self, name: &str) -> io::Result<()> {
        let Some(mut target) = arco_windows::sfx::file(
            crate::locale::tr("Arbeitsbereich-Verknüpfung speichern"),
            true,
            "Arco-Arbeitsbereich.lnk",
        )?
        else {
            return Ok(());
        };
        if target.extension().is_none() {
            target.set_extension("lnk");
        } else if !target
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case(std::ffi::OsStr::new("lnk")))
        {
            return Err(io::Error::other("Verwende die Dateiendung .lnk."));
        }
        let args = workspace_arguments(&self.profile_path, name)?;
        arco_windows::create_shortcut_with_arguments(&std::env::current_exe()?, &target, &args)?;
        self.status(crate::locale::tr("Arbeitsbereich-Verknüpfung gespeichert."));
        Ok(())
    }
}

fn missing_workspace() -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, "Der Arbeitsbereich fehlt.")
}

fn workspace_arguments(path: &std::path::Path, name: &str) -> io::Result<String> {
    let profile = std::path::absolute(path)?;
    let profile = profile
        .to_str()
        .ok_or_else(|| io::Error::other("Der Pfad zur Arco-Profildatei ist ungültig."))?;
    Ok(join_windows_arguments(
        ["--profile", profile, "--workspace", name].into_iter(),
    ))
}

fn workspace_name(input: &str) -> io::Result<&str> {
    let name = input.trim();
    if name.is_empty() || name.chars().count() > 120 || input.chars().any(char::is_control) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Gib einen Arbeitsbereichsnamen mit 1 bis 120 Zeichen ein.",
        ));
    }
    Ok(name)
}
