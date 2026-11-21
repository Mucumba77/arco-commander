use super::{App, Dialog, Pending, WorkspaceDialog, workspace_name};
use crate::app::{JobDescription, Outcome};
use arco_core::profile::{SavedPanel, SavedTab, SavedWorkspace, VirtualTab};
use arco_core::winscp::{self, ImportedWorkspace, WorkspaceTab};
use std::collections::BTreeSet;
use std::io;
use std::sync::Arc;

impl App {
    pub(super) fn show_workspace_import_sources(&mut self) {
        self.dialog(
            "confirm",
            Pending::Workspace(WorkspaceDialog::ImportSource),
            Dialog {
                title: "WinSCP-Arbeitsbereiche importieren",
                options: &[
                    crate::locale::tr("WinSCP-INI-Datei …"),
                    crate::locale::tr("WinSCP-Registrierung"),
                ],
                confirm_label: "Weiter",
                ..Dialog::default()
            },
        );
    }

    pub(super) fn import_workspaces(&mut self, source: usize) -> io::Result<()> {
        let path = if source == 0 {
            let Some(path) = arco_windows::sfx::file(
                crate::locale::tr("WinSCP-INI-Datei wählen"),
                false,
                "*.ini",
            )?
            else {
                self.show_workspace_import_sources();
                return Ok(());
            };
            Some(path)
        } else {
            None
        };
        let title = "WinSCP-Arbeitsbereiche importieren";
        let label = path.as_ref().map_or_else(
            || crate::locale::tr("WinSCP-Registrierung").to_owned(),
            |path| path.display().to_string(),
        );
        self.start_operation_job(
            title,
            false,
            JobDescription::new(crate::OperationKind::Ftp, label, String::new()),
            move |job| {
                arco_core::files::check_cancel(&job.cancel)?;
                let workspaces = if let Some(path) = path {
                    winscp::read_workspaces(&path)?
                } else {
                    read_registry_workspaces()?
                };
                arco_core::files::check_cancel(&job.cancel)?;
                Ok(Outcome::ImportedWorkspaces(workspaces))
            },
        );
        Ok(())
    }

    pub(in crate::app) fn review_imported_workspaces(
        &mut self,
        workspaces: Vec<ImportedWorkspace>,
    ) {
        let options: Vec<_> = workspaces
            .iter()
            .map(|(name, tabs)| match tabs {
                Ok(tabs) => format!("{name} · {} Tabs", tabs.len()),
                Err(error) => format!("{name} · {error}"),
            })
            .collect();
        self.dialog(
            "confirm",
            Pending::Workspace(WorkspaceDialog::ImportReview(Arc::new(workspaces))),
            Dialog {
                title: "WinSCP-Arbeitsbereiche importieren",
                description: "Wähle einen Arbeitsbereich. Arco legt seine Tabs und Serverprofile unter freien Namen an.",
                options: &options.iter().map(String::as_str).collect::<Vec<_>>(),
                confirm_label: "Importieren",
                ..Dialog::default()
            },
        );
    }

    pub(super) fn save_imported_workspace(
        &mut self,
        name: &str,
        tabs: &[WorkspaceTab],
    ) -> io::Result<()> {
        let mut updated = self.profile_snapshot();
        let mut workspace_names = updated
            .workspaces
            .keys()
            .map(|name| name.to_lowercase())
            .collect();
        let name = unique_name(workspace_name(name)?, &mut workspace_names);
        let mut profile_names = updated
            .ftp_profiles
            .keys()
            .map(|name| name.to_lowercase())
            .collect();
        let mut panels = [SavedPanel::default(), SavedPanel::default()];
        let mut linked_navigation = false;
        let mut active_panel = 0;
        let fallback = self.panels[0].current().path.clone();
        for (index, tab) in tabs.iter().enumerate() {
            let (left, right, options) = match tab {
                WorkspaceTab::Local { left, right } => {
                    (left.clone(), right.clone(), SavedTab::default())
                }
                WorkspaceTab::Remote { label, profile } => {
                    let local = profile
                        .local_directory
                        .clone()
                        .unwrap_or_else(|| fallback.clone());
                    let base = format!("WinSCP/{name}/{label}");
                    let profile_name = unique_name(&base, &mut profile_names);
                    let target = VirtualTab::Ftp {
                        profile: profile_name.clone(),
                        url: Some(profile.address.clone()),
                    };
                    if index == 0 {
                        active_panel = 1;
                        linked_navigation = profile.synchronize_browsing;
                    }
                    let mut profile = (**profile).clone();
                    profile.folder =
                        crate::app::ftp_profiles::connection_folder(&format!("WinSCP/{name}"))
                            .unwrap_or_else(|_| "WinSCP".to_owned());
                    updated.ftp_profiles.insert(profile_name, profile);
                    (
                        local.clone(),
                        local,
                        SavedTab {
                            virtual_tab: Some(target),
                            ..Default::default()
                        },
                    )
                }
            };
            panels[0].tabs.push(left);
            panels[1].tabs.push(right);
            panels[1].tab_options.push(options);
        }
        let workspace = SavedWorkspace {
            panels,
            active_panel,
            stacked: false,
            split_ratio: 0.5,
            separate_tree_mode: 0,
            linked_navigation,
            auto_save: false,
        };
        workspace.validate()?;
        updated.workspaces.insert(name, workspace);
        self.commit_workspace_profile(updated)?;
        self.show_workspaces();
        self.status(crate::locale::tr("WinSCP-Arbeitsbereich importiert."));
        Ok(())
    }
}

fn unique_name(base: &str, used: &mut BTreeSet<String>) -> String {
    for number in 1.. {
        let suffix = if number == 1 {
            String::new()
        } else {
            format!(" ({number})")
        };
        let mut name: String = base
            .chars()
            .filter(|char| !char.is_control())
            .take(120 - suffix.len())
            .collect();
        name.truncate(name.trim_end().len());
        name.push_str(&suffix);
        if used.insert(name.to_lowercase()) {
            return name;
        }
    }
    unreachable!("unbounded name suffixes")
}

#[cfg(windows)]
fn read_registry_workspaces() -> io::Result<Vec<ImportedWorkspace>> {
    use arco_windows::session_registry::{self, Source};
    let records = session_registry::read(
        Source::WinScp,
        winscp::WINSCP_FIELDS,
        arco_core::network_import::SESSION_LIMIT,
    )?;
    winscp::registry_workspaces(records.profiles, &records.keys)
}

#[cfg(not(windows))]
fn read_registry_workspaces() -> io::Result<Vec<ImportedWorkspace>> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Diese Importquelle benötigt Windows.",
    ))
}
