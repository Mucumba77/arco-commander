use super::plugins::{plugin_error, user_path};
use super::{App, Outcome};
use crate::commands::{Dialog, Pending};
use crate::display_path;
use arco_core::files;
use arco_core::plugin::{self, Interface, Operation, Registration, WcxOperation, WfxOperation};
use std::ffi::OsStr;
use std::io;
use std::path::PathBuf;

#[derive(Clone, Copy)]
pub(crate) enum PluginFileAction {
    Test,
    Extract,
    Pack,
    DeleteMember,
    PackSelection,
    ExtractMany,
    DeleteMany,
    Download,
    Upload,
    Rename,
    Mkdir,
    Delete,
    RemoveDir,
    DeleteTree,
}

const WCX: &[(PluginFileAction, &str)] = &[
    (PluginFileAction::Test, "Archiv prüfen"),
    (PluginFileAction::Extract, "Datei entpacken"),
    (PluginFileAction::Pack, "Datei packen"),
    (PluginFileAction::DeleteMember, "Datei aus Archiv löschen"),
    (
        PluginFileAction::PackSelection,
        "Markierte lokale Dateien packen",
    ),
    (
        PluginFileAction::ExtractMany,
        "Mehrere Archiveinträge entpacken",
    ),
    (
        PluginFileAction::DeleteMany,
        "Mehrere Archiveinträge löschen",
    ),
];

const WFX: &[(PluginFileAction, &str)] = &[
    (PluginFileAction::Download, "Datei herunterladen"),
    (PluginFileAction::Upload, "Datei hochladen"),
    (PluginFileAction::Rename, "Umbenennen oder verschieben"),
    (PluginFileAction::Mkdir, "Ordner anlegen"),
    (PluginFileAction::Delete, "Datei löschen"),
    (PluginFileAction::RemoveDir, "Leeren Ordner löschen"),
    (PluginFileAction::DeleteTree, "Ordner samt Inhalt löschen"),
];

fn actions(interface: Interface) -> &'static [(PluginFileAction, &'static str)] {
    match interface {
        Interface::Wcx => WCX,
        Interface::Wfx => WFX,
        _ => &[],
    }
}

impl App {
    pub(super) fn show_plugin_file_actions(&mut self, index: usize) {
        let Some(entry) = self
            .profile
            .plugins
            .get(index)
            .filter(|entry| entry.enabled)
        else {
            return;
        };
        let title = format!("{} · {}", entry.name, entry.interface.label());
        let options: Vec<_> = actions(entry.interface)
            .iter()
            .map(|(_, label)| *label)
            .collect();
        if !options.is_empty() {
            self.dialog("confirm", Pending::PluginFiles(index), Dialog {
                title: &title,
                description: "Die verfügbaren Operationen hängen von den Exporten des Plugins ab.",
                options: &options,
                ..Dialog::default()
            });
        }
    }

    pub(crate) fn choose_plugin_file_action(
        &mut self,
        index: usize,
        option: usize,
    ) -> io::Result<()> {
        let Some(entry) = self
            .profile
            .plugins
            .get(index)
            .filter(|entry| entry.enabled)
        else {
            return Ok(());
        };
        let Some(&(action, title)) = actions(entry.interface).get(option) else {
            return Ok(());
        };
        if matches!(action, PluginFileAction::PackSelection) {
            return self.ask_plugin_pack_selection(index);
        }
        let selected = self.active_plugin_file();
        let mut archive = PathBuf::new();
        if matches!(
            action,
            PluginFileAction::Test
                | PluginFileAction::Extract
                | PluginFileAction::DeleteMember
                | PluginFileAction::ExtractMany
                | PluginFileAction::DeleteMany
        ) {
            archive = selected.ok_or_else(|| {
                invalid("Wähle zuerst das lokale Archiv im aktiven Dateifenster.")
            })?;
        }
        let selected_name = self
            .active_plugin_file()
            .map(|file| display_path(&file))
            .unwrap_or_default();
        if matches!(action, PluginFileAction::Test) {
            let details = format!(
                "Prüfe „{}“ mit dem gewählten WCX-Plugin.",
                display_path(&archive)
            );
            self.dialog(
                "confirm",
                Pending::ApplyPluginFile(
                    index,
                    archive,
                    Box::new(Operation::Wcx(WcxOperation::Test)),
                ),
                Dialog {
                    title,
                    description: &details,
                    ..Dialog::default()
                },
            );
            return Ok(());
        }
        let details = if archive.as_os_str().is_empty() {
            if entry.interface == Interface::Wfx {
                "WFX-Pfade beginnen mit einem Rückstrich und beziehen sich auf das Plugin-Dateisystem.".into()
            } else {
                "Du kannst ein neues Archiv anlegen oder eine Datei zu einem vorhandenen Archiv hinzufügen.".into()
            }
        } else {
            format!("Archiv · {}", display_path(&archive))
        };
        let (value, second, placeholder, second_placeholder) = match action {
            PluginFileAction::Extract => (
                "",
                "",
                "Dateipfad innerhalb des Archivs",
                "Lokale Zieldatei (optional)",
            ),
            PluginFileAction::Pack => (
                "",
                selected_name.as_str(),
                "Absoluter Archivpfad",
                "Absolute Quelldatei",
            ),
            PluginFileAction::DeleteMember => ("", "", "Dateipfad innerhalb des Archivs", ""),
            PluginFileAction::ExtractMany => (
                "",
                "",
                "Archiveinträge, getrennt durch |",
                "Lokaler Zielordner (optional)",
            ),
            PluginFileAction::DeleteMany => ("", "", "Archiveinträge, getrennt durch |", ""),
            PluginFileAction::Download => {
                ("", "", "Plugin-Quelldatei", "Lokale Zieldatei (optional)")
            }
            PluginFileAction::Upload => (
                selected_name.as_str(),
                "",
                "Absolute Quelldatei",
                "Plugin-Zieldatei",
            ),
            PluginFileAction::Rename => ("", "", "Plugin-Quellpfad", "Plugin-Zielpfad"),
            PluginFileAction::Mkdir
            | PluginFileAction::RemoveDir
            | PluginFileAction::DeleteTree => ("", "", "Plugin-Ordnerpfad", ""),
            PluginFileAction::Delete => ("", "", "Plugin-Dateipfad", ""),
            PluginFileAction::Test | PluginFileAction::PackSelection => unreachable!(),
        };
        self.dialog(
            "input",
            Pending::PluginFileInput(index, action, archive),
            Dialog {
                title,
                description: &details,
                value,
                second,
                placeholder,
                second_placeholder,
                ..Dialog::default()
            },
        );
        Ok(())
    }

    pub(crate) fn prepare_plugin_file_action(
        &mut self,
        index: usize,
        action: PluginFileAction,
        mut archive: PathBuf,
        value: &str,
        second: &str,
    ) -> io::Result<()> {
        let value = value.trim();
        let second = second.trim();
        if value.is_empty() {
            return Err(invalid("Gib einen Pfad ein."));
        }
        if matches!(
            action,
            PluginFileAction::ExtractMany | PluginFileAction::DeleteMany
        ) {
            return self.prepare_wcx_members(index, archive, action, value, second);
        }
        let operation = match action {
            PluginFileAction::Extract => Operation::Wcx(WcxOperation::Extract {
                member: value.into(),
                target: self.plugin_local_target(value, second)?,
            }),
            PluginFileAction::Pack => {
                archive = user_path(value);
                Operation::Wcx(WcxOperation::Pack {
                    source: user_path(second),
                })
            }
            PluginFileAction::DeleteMember => Operation::Wcx(WcxOperation::Delete {
                member: value.into(),
            }),
            PluginFileAction::Download => Operation::Wfx(WfxOperation::Download {
                remote: value.into(),
                local: self.plugin_local_target(value, second)?,
            }),
            PluginFileAction::Upload => Operation::Wfx(WfxOperation::Upload {
                local: user_path(value),
                remote: second.into(),
            }),
            PluginFileAction::Rename => Operation::Wfx(WfxOperation::Rename {
                from: value.into(),
                to: second.into(),
            }),
            PluginFileAction::Mkdir => Operation::Wfx(WfxOperation::Mkdir { path: value.into() }),
            PluginFileAction::Delete => Operation::Wfx(WfxOperation::Delete { path: value.into() }),
            PluginFileAction::RemoveDir => {
                Operation::Wfx(WfxOperation::RemoveDir { path: value.into() })
            }
            PluginFileAction::DeleteTree => {
                Operation::Wfx(WfxOperation::DeleteTree { path: value.into() })
            }
            _ => return Err(invalid("Ungültiger Plugin-Auftrag.")),
        };
        if matches!(
            action,
            PluginFileAction::DeleteMember
                | PluginFileAction::Delete
                | PluginFileAction::RemoveDir
                | PluginFileAction::DeleteTree
        ) {
            let description = if matches!(action, PluginFileAction::DeleteTree) {
                format!("„{value}“ samt Inhalt dauerhaft löschen?")
            } else {
                format!("„{value}“ dauerhaft löschen?")
            };
            self.dialog(
                "confirm",
                Pending::ApplyPluginFile(index, archive, Box::new(operation)),
                Dialog {
                    title: "Plugin-Eintrag löschen",
                    description: &description,
                    ..Dialog::default()
                },
            );
            return Ok(());
        }
        self.run_plugin_file_action(index, archive, operation)
    }

    fn ask_plugin_pack_selection(&mut self, index: usize) -> io::Result<()> {
        let tab = self.current();
        if self.panels[self.active].loading || !tab.is_local() {
            return Err(invalid(
                "Wähle lokale Dateien in einem geladenen Dateifenster.",
            ));
        }
        let selected: std::collections::BTreeSet<_> = tab.selected_paths().into_iter().collect();
        let files: Vec<_> = tab
            .display_rows()
            .filter(|entry| entry.kind == files::Kind::File && selected.contains(&entry.path))
            .map(|entry| entry.path.clone())
            .collect();
        if files.is_empty() {
            return Err(invalid("Wähle mindestens eine lokale Datei."));
        }
        self.dialog("input", Pending::PluginPackSelection(index, files), Dialog {
            title: "Markierte Dateien mit WCX packen", placeholder: "Absoluter Archivpfad",
            description: "Jede Datei wird einzeln im Archiv veröffentlicht. Bereits vorhandene Einträge werden als Konflikt gemeldet.",
            ..Dialog::default()
        });
        Ok(())
    }

    pub(crate) fn pack_plugin_selection(
        &mut self,
        index: usize,
        paths: Vec<PathBuf>,
        value: &str,
    ) -> io::Result<()> {
        let archive = user_path(value);
        if !archive.is_absolute() {
            return Err(invalid("Gib einen absoluten Archivpfad ein."));
        }
        let tasks = paths
            .into_iter()
            .map(|source| {
                let name = display_path(&source);
                (name, Operation::Wcx(WcxOperation::Pack { source }))
            })
            .collect();
        self.start_registered_batch(index, archive, tasks)
    }

    fn prepare_wcx_members(
        &mut self,
        index: usize,
        archive: PathBuf,
        action: PluginFileAction,
        value: &str,
        target: &str,
    ) -> io::Result<()> {
        let mut tasks = Vec::new();
        let mut seen = std::collections::BTreeSet::new();
        for member in value.split('|').map(str::trim) {
            if member.is_empty() || !seen.insert(member.replace('/', "\\").to_lowercase()) {
                return Err(invalid(
                    "Jeder Archiveintrag muss genau einmal angegeben werden.",
                ));
            }
            let operation = if matches!(action, PluginFileAction::DeleteMany) {
                WcxOperation::Delete {
                    member: member.into(),
                }
            } else {
                let output = if target.is_empty() {
                    self.plugin_local_target(member, "")?
                } else {
                    let leaf = member.rsplit(['/', '\\']).next().unwrap_or_default();
                    user_path(target).join(files::leaf_name(OsStr::new(leaf))?)
                };
                WcxOperation::Extract {
                    member: member.into(),
                    target: output,
                }
            };
            tasks.push((member.to_owned(), Operation::Wcx(operation)));
        }
        if matches!(action, PluginFileAction::DeleteMany) {
            let contents = tasks
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            self.dialog(
                "review",
                Pending::ApplyPluginBatch(index, archive, tasks),
                Dialog {
                    title: "WCX-Archiveinträge löschen",
                    description: "Die aufgeführten Dateien dauerhaft aus dem Archiv entfernen?",
                    value: &contents,
                    ..Dialog::default()
                },
            );
            Ok(())
        } else {
            self.start_registered_batch(index, archive, tasks)
        }
    }

    pub(crate) fn start_registered_batch(
        &mut self,
        index: usize,
        archive: PathBuf,
        tasks: Vec<(String, Operation)>,
    ) -> io::Result<()> {
        let entry = self
            .profile
            .plugins
            .get(index)
            .filter(|entry| entry.enabled)
            .cloned()
            .ok_or_else(|| invalid("Das Plugin ist nicht mehr aktiviert."))?;
        self.run_plugin_batch(entry, archive, tasks);
        Ok(())
    }

    fn plugin_local_target(&self, source: &str, explicit: &str) -> io::Result<PathBuf> {
        if !explicit.is_empty() {
            return Ok(user_path(explicit));
        }
        let other = &self.panels[1 - self.active];
        if other.loading || !other.current().is_local() {
            return Err(invalid(
                "Gib die lokale Zieldatei an oder wähle einen geladenen lokalen Zielordner im anderen Fenster.",
            ));
        }
        let leaf = source.rsplit(['/', '\\']).next().unwrap_or_default();
        Ok(other
            .current()
            .path
            .join(files::leaf_name(OsStr::new(leaf))?))
    }

    pub(crate) fn run_plugin_file_action(
        &mut self,
        index: usize,
        archive: PathBuf,
        operation: Operation,
    ) -> io::Result<()> {
        let Some(entry) = self
            .profile
            .plugins
            .get(index)
            .filter(|entry| entry.enabled)
            .cloned()
        else {
            return Err(invalid("Das Plugin ist nicht mehr aktiviert."));
        };
        let refresh = !matches!(operation, Operation::Wcx(WcxOperation::Test))
            && matches!(
                operation,
                Operation::Wcx(_) | Operation::Wfx(WfxOperation::Download { .. })
            );
        self.start_job("Plugin-Dateioperation läuft …", refresh, move |job| {
            let progress = |message| job.progress(format!("{} · {message}", entry.name));
            let rows = if let Operation::Wfx(operation) = operation {
                plugin::operate_interactive(
                    &entry.path,
                    operation,
                    &job.cancel,
                    progress,
                    &mut |prompt| job.prompt(&entry.name, prompt),
                )
            } else {
                plugin::operate(&entry.path, &archive, operation, &job.cancel, progress)
            }
            .map_err(|error| plugin_error(&entry.name, error))?;
            let details = rows
                .iter()
                .map(|row| format!("{} · {}", row.name, row.value))
                .collect::<Vec<_>>()
                .join(", ");
            Ok(Outcome::Notice(if details.is_empty() {
                format!("Plugin-Dateioperation abgeschlossen · {}", entry.name)
            } else {
                format!("{} · {details}", entry.name)
            }))
        });
        Ok(())
    }

    pub(super) fn run_plugin_batch(
        &mut self,
        entry: Registration,
        archive: PathBuf,
        tasks: Vec<(String, Operation)>,
    ) {
        self.start_job("Plugin-Mehrfachauftrag …", true, move |job| {
            if entry.interface == Interface::Wfx {
                let operations = tasks
                    .into_iter()
                    .filter_map(|(name, operation)| {
                        if let Operation::Wfx(operation) = operation {
                            Some((name, operation))
                        } else {
                            None
                        }
                    })
                    .collect();
                let report = plugin::wfx_batch(
                    &entry.path,
                    operations,
                    &job.cancel,
                    |message| job.progress(format!("{} · {message}", entry.name)),
                    &mut |prompt| job.prompt(&entry.name, prompt),
                    arco_windows::rename_noreplace,
                );
                return Ok(Outcome::Report(entry.name, report));
            }
            let mut report = files::TransferReport::default();
            let total = tasks.len();
            for (index, (label, operation)) in tasks.into_iter().enumerate() {
                if job.cancel.load(std::sync::atomic::Ordering::Relaxed) {
                    report.cancelled = true;
                    report.skipped += total - index;
                    break;
                }
                job.progress(format!(
                    "{} · {}/{} · {label}",
                    entry.name,
                    index + 1,
                    total
                ));
                let progress = |message| job.progress(format!("{label} · {message}"));
                let result =
                    plugin::operate(&entry.path, &archive, operation, &job.cancel, progress);
                match result {
                    Ok(_) => report.completed += 1,
                    Err(error) => report.errors.push(format!("{label}: {error}")),
                }
                report.cancelled |= job.cancel.load(std::sync::atomic::Ordering::Relaxed);
            }
            Ok(Outcome::Report(entry.name, report))
        });
    }
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
