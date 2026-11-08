use super::plugins::{plugin_error, user_path};
use super::{
    App, JobContext, Outcome,
    transfer_queue::{
        ConflictSide, VIRTUAL_CONFLICT_PREVIEW_LIMIT, ask_conflict, conflict_preview,
    },
};
use crate::commands::{Dialog, Pending};
use crate::display_path;
use arco_core::files;
use arco_core::plugin::{self, Interface, Operation, Registration, WcxOperation, WfxOperation};
use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet, btree_map::Entry};
use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};

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
        let label = format!("{} · {} Plugin-Aufträge", entry.name, tasks.len());
        self.enqueue_virtual_transfer(&label, true, move |job| {
            if entry.interface == Interface::Wfx {
                let mut operations: Vec<_> = tasks
                    .into_iter()
                    .filter_map(|(name, operation)| {
                        if let Operation::Wfx(operation) = operation {
                            Some((name, operation))
                        } else {
                            None
                        }
                    })
                    .collect();
                let skipped = resolve_wfx_destinations(&entry, &mut operations, &job)?;
                let remaining = Cell::new(None);
                let resolve = |remote: &str, conflict: files::FileConflict<'_>| {
                    if let Some(rule) = remaining.get()
                        && (conflict.source_metadata.is_file()
                            && conflict.target_metadata.is_file()
                            && !files::is_link(conflict.target_metadata)
                            || matches!(rule, files::Conflict::Skip | files::Conflict::Rename))
                    {
                        return Ok(rule.into());
                    }
                    let label = format!("{} · {remote}", entry.name);
                    let (decision, all) = super::transfer_queue::request_conflict(
                        &job,
                        conflict,
                        files::Transfer::Move,
                        Some(label),
                    )?;
                    if all && let files::ConflictDecision::Apply(rule) = decision {
                        remaining.set(Some(rule));
                    }
                    Ok(decision)
                };
                let mut report = plugin::wfx_batch(
                    &entry.path,
                    operations,
                    &job.cancel,
                    |message| job.progress(format!("{} · {message}", entry.name)),
                    &mut |prompt| job.prompt(&entry.name, prompt),
                    arco_windows::rename_noreplace,
                    Some(&resolve),
                );
                report.skipped += skipped;
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

enum WfxSource<'a> {
    Local(&'a Path),
    Remote(&'a str, bool),
}

fn wfx_destination(operation: &mut WfxOperation) -> Option<(&mut String, WfxSource<'_>)> {
    Some(match operation {
        WfxOperation::Upload { local, remote }
        | WfxOperation::UploadTree { local, remote }
        | WfxOperation::UploadMove { local, remote, .. } => (remote, WfxSource::Local(local)),
        WfxOperation::Copy { from, to } => (to, WfxSource::Remote(from, false)),
        WfxOperation::CopyTree { from, to } | WfxOperation::MoveTree { from, to } => {
            (to, WfxSource::Remote(from, true))
        }
        WfxOperation::Rename { from, to } if !from.eq_ignore_ascii_case(to) => {
            (to, WfxSource::Remote(from, false))
        }
        _ => return None,
    })
}

fn wfx_remote_preview(
    registration: &Registration,
    remote: &str,
    size: Option<u64>,
    job: &JobContext,
) -> io::Result<String> {
    let Some(size) = size.filter(|size| *size <= VIRTUAL_CONFLICT_PREVIEW_LIMIT) else {
        return Ok("WFX-Vorschau für Dateien bekannter Größe bis 64 KiB verfügbar.".into());
    };
    let load = || {
        let storage = tempfile::tempdir()?;
        let name = remote.rsplit('\\').next().unwrap_or("");
        let path = storage.path().join(files::leaf_name(OsStr::new(name))?);
        plugin::operate_interactive(
            &registration.path,
            WfxOperation::Download {
                remote: remote.to_owned(),
                local: path.clone(),
            },
            &job.cancel,
            |message| job.progress(message),
            &mut |prompt| job.prompt(&registration.name, prompt),
        )?;
        let metadata = files::regular_metadata(&path)?;
        if metadata.len() != size {
            return Err(io::Error::other("Der WFX-Eintrag hat sich geändert."));
        }
        Ok::<_, io::Error>(conflict_preview(&path, &metadata))
    };
    match load() {
        Ok(preview) => Ok(preview),
        Err(error) if error.kind() == io::ErrorKind::Interrupted => Err(error),
        Err(_) => Ok("Die WFX-Datei konnte für die Vorschau nicht gelesen werden.".into()),
    }
}

fn wfx_file_size(value: &str) -> Option<u64> {
    value.strip_suffix(" B")?.parse().ok()
}

fn wfx_alternate_name(
    current: &str,
    decision: files::ConflictDecision,
    is_free: impl Fn(&str) -> bool,
    cancel: &std::sync::atomic::AtomicBool,
) -> io::Result<Option<String>> {
    match decision {
        files::ConflictDecision::Apply(files::Conflict::Rename) => {
            for index in 2..=10_000 {
                files::check_cancel(cancel)?;
                let name = files::numbered_copy_name(Path::new(current), index)?;
                let name = name.to_string_lossy();
                if is_free(&name) {
                    return Ok(Some(name.into_owned()));
                }
            }
            Err(invalid("Kein freier WFX-Zielname gefunden."))
        }
        files::ConflictDecision::RenameTo(name) => {
            let name = files::leaf_name(&name)?.to_string_lossy();
            if !is_free(&name) {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "Der gewählte WFX-Zielname ist bereits vorhanden oder vorgemerkt.",
                ));
            }
            Ok(Some(name.into_owned()))
        }
        _ => Ok(None),
    }
}

fn resolve_wfx_destinations(
    plugin: &Registration,
    operations: &mut Vec<(String, WfxOperation)>,
    job: &JobContext,
) -> io::Result<usize> {
    use files::{Conflict, ConflictDecision};
    let mut directories = BTreeMap::<String, BTreeMap<String, plugin::Row>>::new();
    let mut reserved = BTreeSet::new();
    for (_, operation) in operations.iter_mut() {
        if let Some((target, _)) = wfx_destination(operation) {
            reserved.insert(target.to_lowercase());
        }
    }
    let mut remaining = None;
    let mut kept = Vec::with_capacity(operations.len());
    let mut skipped = 0;
    for (label, mut operation) in operations.drain(..) {
        files::check_cancel(&job.cancel)?;
        let file_destination = operation.file_destination().is_some();
        let mut overwrite = None;
        let Some((target, source)) = wfx_destination(&mut operation) else {
            kept.push((label, operation));
            continue;
        };
        let (parent, name) = target
            .rsplit_once('\\')
            .ok_or_else(|| invalid("Ungültiges WFX-Ziel."))?;
        let parent = if parent.is_empty() { "\\" } else { parent };
        files::leaf_name(OsStr::new(name))?;
        let rows = match directories.entry(parent.to_lowercase()) {
            Entry::Occupied(cached) => cached.into_mut(),
            Entry::Vacant(slot) => {
                let rows = plugin::list_wfx(
                    &plugin.path,
                    Path::new(parent),
                    &job.cancel,
                    &mut |prompt| job.prompt(&plugin.name, prompt),
                )?;
                let mut entries = BTreeMap::new();
                for row in rows {
                    if entries.insert(row.name.to_lowercase(), row).is_some() {
                        return Err(invalid("Der WFX-Zielordner enthält mehrdeutige Einträge."));
                    }
                }
                slot.insert(entries)
            }
        };
        let mut name = name.to_owned();
        let source = match source {
            WfxSource::Local(path) => {
                let metadata = files::regular_metadata(path)?;
                (metadata.is_dir(), Some((path, metadata)), String::new())
            }
            WfxSource::Remote(path, directory) => (directory, None, path.to_owned()),
        };
        if let Some(existing) = rows.get(&name.to_lowercase()) {
            let can_replace = file_destination && !source.0 && existing.wfx_version.is_some();
            let existing_path = format!("{}\\{}", parent.trim_end_matches('\\'), existing.name);
            let decision = if let Some(rule) = remaining
                && (rule != Conflict::Replace || can_replace)
            {
                ConflictDecision::Apply(rule)
            } else {
                let source_side = if let Some((path, metadata)) = &source.1 {
                    ConflictSide::local(path, metadata, None)
                } else if source.0 {
                    ConflictSide {
                        path: format!("{} · {}", plugin.name, source.2),
                        info: "Ordner".into(),
                        preview: String::new(),
                    }
                } else {
                    let (parent, name) = source.2.rsplit_once('\\').unwrap_or(("", &source.2));
                    let parent = if parent.is_empty() { "\\" } else { parent };
                    let rows = plugin::list_wfx(
                        &plugin.path,
                        Path::new(parent),
                        &job.cancel,
                        &mut |prompt| job.prompt(&plugin.name, prompt),
                    );
                    let size = match rows {
                        Ok(rows) => rows
                            .into_iter()
                            .find(|row| row.name == name)
                            .and_then(|row| wfx_file_size(&row.value)),
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                            return Err(error);
                        }
                        Err(_) => None,
                    };
                    let mut side = ConflictSide::virtual_entry(
                        format!("{} · {}", plugin.name, source.2),
                        size,
                        None,
                    );
                    if size.is_none() {
                        side.info = "WFX-Datei · Größe unbekannt".into();
                    }
                    side.preview = wfx_remote_preview(plugin, &source.2, size, job)?;
                    side
                };
                let target_preview = if existing.value == "Ordner" {
                    String::new()
                } else {
                    wfx_remote_preview(plugin, &existing_path, wfx_file_size(&existing.value), job)?
                };
                let mut actions = vec![
                    (Conflict::Skip, "Überspringen"),
                    (Conflict::Rename, "Unter freiem Namen speichern"),
                ];
                if can_replace {
                    actions.push((Conflict::Replace, "Überschreiben"));
                }
                let (decision, all) = ask_conflict(
                    job,
                    source_side,
                    ConflictSide {
                        path: format!("{} · {existing_path}", plugin.name),
                        info: existing.value.clone(),
                        preview: target_preview,
                    },
                    actions,
                )?;
                if all && let ConflictDecision::Apply(rule) = decision {
                    remaining = Some(rule);
                }
                decision
            };
            if matches!(decision, ConflictDecision::Apply(Conflict::Replace)) && can_replace {
                overwrite = existing.wfx_version;
                name = existing.name.clone();
                *target = existing_path;
            } else {
                let parent = parent.trim_end_matches('\\');
                let is_free = |name: &str| {
                    !rows.contains_key(&name.to_lowercase())
                        && !reserved.contains(&format!("{parent}\\{name}").to_lowercase())
                };
                let Some(alternate) = wfx_alternate_name(&name, decision, is_free, &job.cancel)?
                else {
                    skipped += 1;
                    continue;
                };
                name = alternate;
                *target = format!("{parent}\\{name}");
                reserved.insert(target.to_lowercase());
            }
        }
        rows.insert(
            name.to_lowercase(),
            plugin::Row {
                name,
                value: if source.0 {
                    "Ordner".into()
                } else {
                    "Datei aus demselben Auftrag".into()
                },
                ..Default::default()
            },
        );
        if let Some(expected) = overwrite {
            operation = WfxOperation::Overwrite {
                operation: Box::new(operation),
                expected,
            };
        }
        kept.push((label, operation));
    }
    *operations = kept;
    Ok(skipped)
}

#[cfg(test)]
mod tests {
    use super::*;
    use arco_core::files::{Conflict, ConflictDecision};
    use std::sync::atomic::AtomicBool;

    #[test]
    fn wfx_rename_preserves_existing_and_reserved_names() {
        let cancel = AtomicBool::new(false);
        let first = files::numbered_copy_name(Path::new("Text.txt"), 2).unwrap();
        let next = files::numbered_copy_name(Path::new("Text.txt"), 3).unwrap();
        let is_free = |name: &str| Path::new(name) != first;
        let auto = wfx_alternate_name("Text.txt", Conflict::Rename.into(), is_free, &cancel)
            .unwrap()
            .unwrap();
        assert_eq!(Path::new(&auto), next);
        let occupied = wfx_alternate_name(
            "Text.txt",
            ConflictDecision::RenameTo(first.into_os_string()),
            |_| false,
            &cancel,
        )
        .unwrap_err();
        assert_eq!(occupied.kind(), io::ErrorKind::AlreadyExists);
        let invalid = wfx_alternate_name(
            "Text.txt",
            ConflictDecision::RenameTo("../Datei.txt".into()),
            |_| true,
            &cancel,
        );
        assert!(invalid.is_err());
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "requires the Ghisler sample WFX fixture in ARCO_TEST_WFX_SAMPLE"]
    fn wfx_overwrite_preserves_changed_targets_and_stages_file_transfers() {
        let plugin_path = PathBuf::from(std::env::var_os("ARCO_TEST_WFX_SAMPLE").unwrap());
        let fixture = tempfile::tempdir().unwrap();
        let cancel = AtomicBool::new(false);
        let remote = |path: &Path| format!("\\{}", display_path(path));
        let directory = remote(fixture.path());
        let source = fixture.path().join("source.txt");
        let target = fixture.path().join("target.txt");
        let remote_target = remote(&target);
        std::fs::write(&source, b"new source").unwrap();
        std::fs::write(&target, b"old").unwrap();
        let version = || {
            plugin::list_wfx(&plugin_path, Path::new(&directory), &cancel, &mut |_| {
                panic!("the sample plugin does not ask for input")
            })
            .unwrap()
            .into_iter()
            .find(|row| row.name == "target.txt")
            .unwrap()
            .wfx_version
            .unwrap()
        };
        let run = |operation, expected| {
            plugin::wfx_batch(
                &plugin_path,
                vec![(
                    "target.txt".into(),
                    WfxOperation::Overwrite {
                        operation: Box::new(operation),
                        expected,
                    },
                )],
                &cancel,
                |_| (),
                &mut |_| panic!("the sample plugin does not ask for input"),
                arco_windows::rename_noreplace,
                None,
            )
        };
        let upload = || WfxOperation::Upload {
            local: source.clone(),
            remote: remote_target.clone(),
        };
        let old = version();
        std::fs::write(&target, b"concurrent change").unwrap();
        let report = run(upload(), old);
        assert_eq!(report.errors.len(), 1);
        assert_eq!(std::fs::read(&target).unwrap(), b"concurrent change");
        let report = run(upload(), version());
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(std::fs::read(&target).unwrap(), b"new source");
        std::fs::write(&source, b"move contents").unwrap();
        let report = run(
            WfxOperation::Rename {
                from: remote(&source),
                to: remote_target,
            },
            version(),
        );
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(std::fs::read(&target).unwrap(), b"move contents");
        assert!(!source.exists());
        assert_eq!(std::fs::read_dir(fixture.path()).unwrap().count(), 1);
    }
}
