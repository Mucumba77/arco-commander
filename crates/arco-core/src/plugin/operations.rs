use super::{Interface, Operation, PromptHandler, Row, inspect, inspect_host, inspect_interactive};
use crate::{archive_view, archives, files, tools};
use serde::{Deserialize, Serialize};
use std::ffi::OsStr;
use std::fs::{self, File, Metadata};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

pub const WFX_TREE_DEPTH: usize = 64;
pub const WFX_TREE_ENTRIES: usize = 100_000;

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub enum WcxOperation {
    Test,
    Extract { member: String, target: PathBuf },
    Pack { source: PathBuf },
    Delete { member: String },
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub enum WfxOperation {
    Overwrite {
        operation: Box<WfxOperation>,
        expected: WfxVersion,
    },
    Download {
        remote: String,
        local: PathBuf,
    },
    Upload {
        local: PathBuf,
        remote: String,
    },
    DownloadTree {
        remote: String,
        local: PathBuf,
    },
    UploadTree {
        local: PathBuf,
        remote: String,
    },
    DownloadMove {
        remote: String,
        local: PathBuf,
        tree: bool,
    },
    UploadMove {
        local: PathBuf,
        remote: String,
        tree: bool,
    },
    RemoveDownloaded {
        remote: String,
        local: PathBuf,
        tree: bool,
    },
    CopyTree {
        from: String,
        to: String,
    },
    MoveTree {
        from: String,
        to: String,
    },
    Copy {
        from: String,
        to: String,
    },
    Rename {
        from: String,
        to: String,
    },
    Mkdir {
        path: String,
    },
    Delete {
        path: String,
    },
    RemoveDir {
        path: String,
    },
    DeleteTree {
        path: String,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct WfxVersion {
    pub size: u64,
    pub modified: u64,
    pub attributes: u32,
}

impl WfxOperation {
    pub fn file_destination(&self) -> Option<&str> {
        match self {
            Self::Upload { remote, .. }
            | Self::UploadMove {
                remote,
                tree: false,
                ..
            } => Some(remote),
            Self::Copy { to, .. } | Self::Rename { to, .. } => Some(to),
            _ => None,
        }
    }
}

pub fn operate(
    plugin: &Path,
    archive: &Path,
    operation: Operation,
    cancel: &AtomicBool,
    progress: impl FnMut(String),
) -> io::Result<Vec<Row>> {
    match operation {
        Operation::Wcx(operation) => wcx(plugin, archive, operation, cancel, progress),
        Operation::Wfx(operation) => wfx(plugin, operation, cancel, progress, None),
        _ => Err(invalid("Die Anfrage enthält keine Plugin-Dateioperation.")),
    }
}

pub fn operate_interactive(
    plugin: &Path,
    operation: WfxOperation,
    cancel: &AtomicBool,
    progress: impl FnMut(String),
    prompt: &mut PromptHandler<'_>,
) -> io::Result<Vec<Row>> {
    wfx(plugin, operation, cancel, progress, Some(prompt))
}

fn wcx(
    plugin: &Path,
    archive: &Path,
    operation: WcxOperation,
    cancel: &AtomicBool,
    progress: impl FnMut(String),
) -> io::Result<Vec<Row>> {
    check_absolute(archive)?;
    match operation {
        WcxOperation::Extract { member, target } => {
            let member = checked_member(&member)?;
            let target = destination(&target)?;
            let metadata = regular(archive)?;
            let catalog = inspect(Interface::Wcx, plugin, archive, cancel)?;
            let entry = selected_member(&catalog, &member)?;
            let size = member_size(entry)?;
            let member = checked_member(&entry.name)?;
            files::ensure_unchanged(archive, &metadata)?;
            let workspace = temp_near(&target)?;
            let staged = workspace.path().join(leaf(&target)?);
            let rows = inspect_host(
                Interface::Wcx,
                plugin,
                archive,
                Operation::Wcx(WcxOperation::Extract {
                    member,
                    target: staged.clone(),
                }),
                cancel,
                progress,
            )?;
            files::ensure_unchanged(archive, &metadata)?;
            if regular(&staged)?.len() != size {
                return Err(io::Error::other(
                    "Die entpackte Größe weicht vom Archiveintrag ab.",
                ));
            }
            publish(staged, &target, None, cancel)?;
            Ok(rows)
        }
        WcxOperation::Test => {
            let metadata = regular(archive)?;
            let rows = inspect_host(
                Interface::Wcx,
                plugin,
                archive,
                Operation::Wcx(WcxOperation::Test),
                cancel,
                progress,
            )?;
            files::ensure_unchanged(archive, &metadata)?;
            Ok(rows)
        }
        operation => edit_archive(plugin, archive, operation, cancel, progress),
    }
}

fn edit_archive(
    plugin: &Path,
    archive: &Path,
    operation: WcxOperation,
    cancel: &AtomicBool,
    progress: impl FnMut(String),
) -> io::Result<Vec<Row>> {
    let archive = resolved_target(archive)?;
    let original = match regular(&archive) {
        Ok(metadata) => Some(metadata),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    let workspace = temp_near(&archive)?;
    let staged = workspace.path().join(leaf(&archive)?);
    if let Some(metadata) = &original {
        snapshot(&archive, &staged, metadata, cancel)?;
    }
    let mut expected = if original.is_some() {
        inspect(Interface::Wcx, plugin, &staged, cancel)?
    } else {
        Vec::new()
    };
    let input = tempfile::tempdir_in(workspace.path())?;
    let operation = match operation {
        WcxOperation::Pack { source } => {
            check_absolute(&source)?;
            let metadata = regular(&source)?;
            let name = leaf(&source)?;
            let member = name
                .to_str()
                .ok_or_else(|| invalid("WCX-Dateinamen benötigen Unicode."))?
                .to_owned();
            if expected.iter().any(|row| matches_member(row, &member)) {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "Der Dateiname ist bereits im Archiv vorhanden.",
                ));
            }
            let source_copy = input.path().join(name);
            snapshot(&source, &source_copy, &metadata, cancel)?;
            expected.push(Row {
                name: member,
                value: format!("{} B", metadata.len()),
                ..Default::default()
            });
            WcxOperation::Pack {
                source: source_copy,
            }
        }
        WcxOperation::Delete { member } => {
            let member = checked_member(&member)?;
            let entry = selected_member(&expected, &member)?;
            member_size(entry)?;
            let raw = entry.name.clone();
            checked_member(&raw)?;
            expected.retain(|row| row.name != raw);
            WcxOperation::Delete { member: raw }
        }
        _ => return Err(invalid("Ungültiger WCX-Schreibauftrag.")),
    };
    let rows = inspect_host(
        Interface::Wcx,
        plugin,
        &staged,
        Operation::Wcx(operation),
        cancel,
        progress,
    )?;
    regular(&staged)?;
    let actual = inspect(Interface::Wcx, plugin, &staged, cancel)?;
    if signature(expected) != signature(actual) {
        return Err(io::Error::other(
            "Die Plugin-Ausgabe enthält nicht den erwarteten Archivzustand.",
        ));
    }
    publish(staged, &archive, original.as_ref(), cancel)?;
    Ok(rows)
}

fn matches_member(row: &Row, member: &str) -> bool {
    row.name.replace('/', "\\").eq_ignore_ascii_case(member)
}

fn selected_member<'a>(entries: &'a [Row], member: &str) -> io::Result<&'a Row> {
    let mut matches = entries.iter().filter(|row| matches_member(row, member));
    let row = matches
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "Der Archiveintrag fehlt."))?;
    if matches.next().is_some() {
        return Err(invalid("Der Archiveintrag ist mehrdeutig."));
    }
    Ok(row)
}

fn member_size(row: &Row) -> io::Result<u64> {
    row.value
        .strip_suffix(" B")
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| invalid("Wähle eine einzelne Datei im Archiv."))
}

fn signature(rows: Vec<Row>) -> Vec<(String, String)> {
    let mut signature: Vec<_> = rows
        .into_iter()
        .map(|row| (row.name.replace('/', "\\"), row.value))
        .collect();
    signature.sort_unstable();
    signature
}

fn wfx(
    plugin: &Path,
    operation: WfxOperation,
    cancel: &AtomicBool,
    progress: impl FnMut(String),
    prompt: Option<&mut PromptHandler<'_>>,
) -> io::Result<Vec<Row>> {
    if matches!(
        operation,
        WfxOperation::DownloadTree { .. }
            | WfxOperation::DownloadMove { .. }
            | WfxOperation::UploadMove { .. }
    ) {
        return Err(invalid(
            "Ein WFX-Ordnerdownload benötigt den Mehrfachauftrag.",
        ));
    }
    let staged = stage_wfx(operation, cancel, false)?;
    let rows = inspect_interactive(
        Interface::Wfx,
        plugin,
        Path::new(""),
        Operation::Wfx(staged.operation.clone()),
        cancel,
        progress,
        prompt,
    )
    .map_err(|error| {
        if staged.output.is_some() {
            error
        } else {
            io::Error::other(format!("{error}. Prüfe den Zustand im Plugin-Dateisystem."))
        }
    })?;
    staged.finish(cancel, None, None)?;
    Ok(rows)
}

struct StagedWfx {
    operation: WfxOperation,
    output: Option<DownloadOutput>,
    local_move: Option<LocalMove>,
    _workspace: Option<tempfile::TempDir>,
}

type RenameDirectory = fn(&Path, &Path) -> io::Result<()>;
pub type DownloadConflict<'a> =
    dyn Fn(&str, files::FileConflict<'_>) -> io::Result<files::ConflictDecision> + 'a;

enum Finish {
    Completed,
    Skipped,
    Remove(WfxOperation),
}

struct DownloadOutput {
    staged: PathBuf,
    target: PathBuf,
    remote: String,
    tree: bool,
    moving: bool,
}

struct LocalMove {
    source: PathBuf,
    staged: PathBuf,
    entries: Vec<(PathBuf, Metadata)>,
}

impl StagedWfx {
    fn finish(
        self,
        cancel: &AtomicBool,
        rename_directory: Option<RenameDirectory>,
        resolve: Option<&DownloadConflict<'_>>,
    ) -> io::Result<Finish> {
        if let Some(LocalMove {
            source,
            staged,
            entries,
        }) = self.local_move
        {
            remove_local_snapshot(&source, &staged, &entries, cancel).map_err(|error| {
                io::Error::other(format!(
                    "WFX-Ziel übertragen. Lokale Quelle nicht vollständig entfernt: {error}"
                ))
            })?;
        }
        if let Some(DownloadOutput {
            staged,
            mut target,
            remote,
            tree,
            moving,
        }) = self.output
        {
            files::check_cancel(cancel)?;
            let source = files::regular_metadata(&staged)?;
            let mut previous = None;
            if let Some(resolve) = resolve {
                match fs::symlink_metadata(&target) {
                    Ok(existing)
                        if tree && !moving && existing.is_dir() && !files::is_link(&existing) =>
                    {
                        let on_conflict = |conflict: files::FileConflict<'_>| {
                            let member = conflict
                                .source
                                .strip_prefix(&staged)
                                .map_err(io::Error::other)?;
                            resolve(&format!("{remote}\\{}", member.display()), conflict)
                        };
                        let report = files::transfer(
                            std::slice::from_ref(&staged),
                            target.parent().expect("resolved target"),
                            files::Transfer::Move,
                            files::TransferPolicy {
                                resolve_conflict: Some(&on_conflict),
                                ..files::Conflict::Fail.into()
                            },
                            cancel,
                            rename_directory,
                            |_| {},
                        )?;
                        if report.cancelled {
                            return Err(io::ErrorKind::Interrupted.into());
                        }
                        if !report.errors.is_empty() {
                            return Err(io::Error::other(report.errors.join("\n")));
                        }
                        return Ok(if report.completed == 0 {
                            Finish::Skipped
                        } else {
                            Finish::Completed
                        });
                    }
                    Ok(existing) => {
                        let decision = resolve(
                            &remote,
                            files::FileConflict {
                                source: &staged,
                                target: &target,
                                source_metadata: &source,
                                target_metadata: &existing,
                            },
                        )?;
                        files::check_cancel(cancel)?;
                        if let files::ConflictDecision::RenameTo(name) = decision {
                            target.set_file_name(files::leaf_name(&name)?);
                        } else if let files::ConflictDecision::Apply(rule) = decision {
                            match rule {
                                files::Conflict::Skip => return Ok(Finish::Skipped),
                                files::Conflict::Rename => {
                                    target = files::unused_copy_name(&target, cancel)?
                                }
                                files::Conflict::Replace
                                | files::Conflict::Newer
                                | files::Conflict::Larger
                                    if source.is_file()
                                        && existing.is_file()
                                        && !files::is_link(&existing) =>
                                {
                                    if (rule == files::Conflict::Newer
                                        && source.modified()? <= existing.modified()?)
                                        || (rule == files::Conflict::Larger
                                            && source.len() <= existing.len())
                                    {
                                        return Ok(Finish::Skipped);
                                    }
                                    previous = Some(existing);
                                }
                                _ => return Err(io::ErrorKind::AlreadyExists.into()),
                            }
                        }
                    }
                    Err(error) if error.kind() == io::ErrorKind::NotFound => (),
                    Err(error) => return Err(error),
                }
            }
            if tree {
                if !source.is_dir() {
                    return Err(invalid("Das Plugin hat keinen regulären Ordner geliefert."));
                }
                files::check_cancel(cancel)?;
                rename_directory.ok_or(io::ErrorKind::Unsupported)?(&staged, &target)?;
            } else {
                publish(staged, &target, previous.as_ref(), cancel)?;
            }
            if moving {
                return Ok(Finish::Remove(WfxOperation::RemoveDownloaded {
                    remote,
                    local: target,
                    tree,
                }));
            }
        }
        Ok(Finish::Completed)
    }
}

fn stage_wfx(
    operation: WfxOperation,
    cancel: &AtomicBool,
    conflicts: bool,
) -> io::Result<StagedWfx> {
    if let WfxOperation::Overwrite {
        operation,
        expected,
    } = operation
    {
        if operation.file_destination().is_none()
            || (!conflicts && matches!(*operation, WfxOperation::UploadMove { .. }))
        {
            return Err(invalid(
                "Nur einzelne WFX-Dateien können überschrieben werden.",
            ));
        }
        let mut staged = stage_wfx(*operation, cancel, conflicts)?;
        staged.operation = WfxOperation::Overwrite {
            operation: Box::new(staged.operation),
            expected,
        };
        return Ok(staged);
    }
    files::check_cancel(cancel)?;
    let operation = normalize_wfx(operation)?;
    let tree = matches!(
        operation,
        WfxOperation::DownloadTree { .. }
            | WfxOperation::UploadTree { .. }
            | WfxOperation::DownloadMove { tree: true, .. }
            | WfxOperation::UploadMove { tree: true, .. }
    );
    let moving = matches!(
        operation,
        WfxOperation::DownloadMove { .. } | WfxOperation::UploadMove { .. }
    );
    match operation {
        WfxOperation::Download { remote, local }
        | WfxOperation::DownloadTree { remote, local }
        | WfxOperation::DownloadMove { remote, local, .. } => {
            let local = if conflicts {
                resolved_target(&local)?
            } else {
                destination(&local)?
            };
            let workspace = temp_near(&local)?;
            let staged = workspace.path().join(leaf(&local)?);
            let operation = if tree {
                WfxOperation::DownloadTree {
                    remote: remote.clone(),
                    local: staged.clone(),
                }
            } else {
                WfxOperation::Download {
                    remote: remote.clone(),
                    local: staged.clone(),
                }
            };
            Ok(StagedWfx {
                operation,
                output: Some(DownloadOutput {
                    staged,
                    target: local,
                    remote,
                    tree,
                    moving,
                }),
                local_move: None,
                _workspace: Some(workspace),
            })
        }
        WfxOperation::Upload { local, remote }
        | WfxOperation::UploadTree { local, remote }
        | WfxOperation::UploadMove { local, remote, .. } => {
            check_absolute(&local)?;
            let workspace = tempfile::tempdir()?;
            let staged = workspace.path().join(leaf(&local)?);
            let entries = if tree {
                snapshot_tree(&local, &staged, cancel)?
            } else {
                let metadata = regular(&local)?;
                snapshot(&local, &staged, &metadata, cancel)?;
                vec![(PathBuf::new(), metadata)]
            };
            let local_move = moving.then(|| LocalMove {
                source: local,
                staged: staged.clone(),
                entries,
            });
            let operation = if moving {
                WfxOperation::UploadMove {
                    local: staged,
                    remote,
                    tree,
                }
            } else if tree {
                WfxOperation::UploadTree {
                    local: staged,
                    remote,
                }
            } else {
                WfxOperation::Upload {
                    local: staged,
                    remote,
                }
            };
            Ok(StagedWfx {
                operation,
                output: None,
                local_move,
                _workspace: Some(workspace),
            })
        }
        operation => Ok(StagedWfx {
            operation,
            output: None,
            local_move: None,
            _workspace: None,
        }),
    }
}

pub fn wfx_batch(
    plugin: &Path,
    items: Vec<(String, WfxOperation)>,
    cancel: &AtomicBool,
    mut progress: impl FnMut(String),
    prompt: &mut PromptHandler<'_>,
    rename_directory: RenameDirectory,
    resolve: Option<&DownloadConflict<'_>>,
) -> files::TransferReport {
    let mut report = files::TransferReport::default();
    const CHUNK: usize = 48;
    for (group, items_group) in items.chunks(CHUNK).enumerate() {
        if files::check_cancel(cancel).is_err() {
            report.cancelled = true;
            report.skipped += items.len() - group * CHUNK;
            break;
        }
        let mut staged = Vec::new();
        for (label, operation) in items_group {
            match stage_wfx(operation.clone(), cancel, resolve.is_some()) {
                Ok(operation) => staged.push((label, operation)),
                Err(error) => report.errors.push(format!("{label}: {error}")),
            }
        }
        if staged.is_empty() {
            continue;
        }
        let wire = staged
            .iter()
            .map(|(_, staged)| staged.operation.clone())
            .collect();
        let rows = match wfx_group(plugin, wire, cancel, &mut progress, prompt) {
            Ok(rows) => rows,
            Err(error) => {
                report.errors.push(format!(
                    "{error}. Status von {} WFX-Einträgen prüfen.",
                    staged.len()
                ));
                report.cancelled |= files::check_cancel(cancel).is_err();
                report.skipped += items.len() - (group * CHUNK + items_group.len());
                break;
            }
        };
        let mut cleanup = Vec::new();
        for (row, (label, staged)) in rows.into_iter().zip(staged) {
            if !row.value.is_empty() {
                report.errors.push(format!("{label}: {}", row.value));
            } else {
                match staged.finish(cancel, Some(rename_directory), resolve) {
                    Ok(Finish::Remove(operation)) => cleanup.push((label, operation)),
                    Ok(Finish::Completed) => report.completed += 1,
                    Ok(Finish::Skipped) => report.skipped += 1,
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                        report.cancelled = true;
                        break;
                    }
                    Err(error) => report.errors.push(format!("{label}: {error}")),
                }
            }
        }
        if !cleanup.is_empty() {
            let (labels, operations): (Vec<_>, Vec<_>) = cleanup.into_iter().unzip();
            match wfx_group(plugin, operations, cancel, &mut progress, prompt) {
                Ok(rows) => {
                    for (row, label) in rows.into_iter().zip(labels) {
                        if row.value.is_empty() {
                            report.completed += 1;
                        } else {
                            report.errors.push(format!("{label}: Lokales Ziel geschrieben. WFX-Quelle nicht vollständig entfernt: {}", row.value));
                        }
                    }
                }
                Err(error) => {
                    report.errors.push(format!(
                        "Lokale Ziele geschrieben. Zustand der WFX-Quellen prüfen ({}): {error}",
                        labels.into_iter().cloned().collect::<Vec<_>>().join(", "),
                    ));
                }
            }
        }
    }
    report.cancelled |= files::check_cancel(cancel).is_err();
    report
}

fn wfx_group(
    plugin: &Path,
    operations: Vec<WfxOperation>,
    cancel: &AtomicBool,
    progress: impl FnMut(String),
    prompt: &mut PromptHandler<'_>,
) -> io::Result<Vec<Row>> {
    let count = operations.len();
    let rows = inspect_interactive(
        Interface::Wfx,
        plugin,
        Path::new(""),
        Operation::WfxBatch(operations),
        cancel,
        progress,
        Some(prompt),
    )?;
    if rows.len() != count
        || rows
            .iter()
            .enumerate()
            .any(|(i, row)| row.name.parse::<usize>().ok() != Some(i))
    {
        return Err(io::Error::other("Ungültiger Ergebnisbericht"));
    }
    Ok(rows)
}

fn normalize_wfx(operation: WfxOperation) -> io::Result<WfxOperation> {
    Ok(match operation {
        WfxOperation::Overwrite { .. } => {
            return Err(invalid("Ungültiger verschachtelter WFX-Auftrag."));
        }
        WfxOperation::Download { remote, local } => WfxOperation::Download {
            remote: checked_remote(&remote)?,
            local,
        },
        WfxOperation::Upload { local, remote } => WfxOperation::Upload {
            local,
            remote: checked_remote(&remote)?,
        },
        WfxOperation::DownloadTree { remote, local } => WfxOperation::DownloadTree {
            remote: checked_remote(&remote)?,
            local,
        },
        WfxOperation::UploadTree { local, remote } => WfxOperation::UploadTree {
            local,
            remote: checked_remote(&remote)?,
        },
        WfxOperation::DownloadMove {
            remote,
            local,
            tree,
        } => WfxOperation::DownloadMove {
            remote: checked_delete_path(&remote, tree)?,
            local,
            tree,
        },
        WfxOperation::UploadMove {
            local,
            remote,
            tree,
        } => WfxOperation::UploadMove {
            local,
            remote: checked_delete_path(&remote, tree)?,
            tree,
        },
        WfxOperation::RemoveDownloaded { .. } => {
            return Err(invalid(
                "WFX-Quelllöschung benötigt ein zuvor veröffentlichtes lokales Ziel.",
            ));
        }
        WfxOperation::Copy { from, to } => {
            let (from, to) = remote_pair(&from, &to, false)?;
            WfxOperation::Copy { from, to }
        }
        WfxOperation::Rename { from, to } => {
            let (from, to) = remote_pair(&from, &to, false)?;
            WfxOperation::Rename { from, to }
        }
        WfxOperation::CopyTree { from, to } => {
            let (from, to) = remote_pair(&from, &to, true)?;
            WfxOperation::CopyTree { from, to }
        }
        WfxOperation::MoveTree { from, to } => {
            let (from, to) = remote_pair(&from, &to, true)?;
            WfxOperation::MoveTree { from, to }
        }
        WfxOperation::Mkdir { path } => WfxOperation::Mkdir {
            path: checked_remote(&path)?,
        },
        WfxOperation::Delete { path } => WfxOperation::Delete {
            path: checked_remote(&path)?,
        },
        WfxOperation::RemoveDir { path } => WfxOperation::RemoveDir {
            path: checked_remote(&path)?,
        },
        WfxOperation::DeleteTree { path } => WfxOperation::DeleteTree {
            path: checked_delete_path(&path, true)?,
        },
    })
}

fn checked_delete_path(path: &str, tree: bool) -> io::Result<String> {
    let path = checked_remote(path)?;
    if tree && !path[1..].contains('\\') {
        return Err(invalid(
            "Ordner direkt im WFX-Stamm können Verbindungen sein. Öffne die Verbindung und wähle einen Unterordner.",
        ));
    }
    Ok(path)
}

fn remote_pair(from: &str, to: &str, tree: bool) -> io::Result<(String, String)> {
    let from = checked_remote(from)?;
    let to = checked_remote(to)?;
    if from == to {
        return Err(invalid("Quell- und Zielpfad sind gleich."));
    }
    if tree {
        let source = from.to_lowercase();
        let target = to.to_lowercase();
        if target == source
            || target
                .strip_prefix(&source)
                .is_some_and(|tail| tail.starts_with('\\'))
        {
            return Err(invalid("Der Zielordner liegt im ausgewählten Quellordner."));
        }
    }
    Ok((from, to))
}

fn checked_member(value: &str) -> io::Result<String> {
    let value = value.replace('\\', "/");
    archives::checked_archive_path(&value)?;
    Ok(value.replace('/', "\\"))
}

fn checked_remote(value: &str) -> io::Result<String> {
    let value = value.replace('/', "\\");
    let suffix = value
        .strip_prefix('\\')
        .ok_or_else(|| invalid("Ein WFX-Pfad beginnt mit einem Rückstrich."))?;
    if suffix.is_empty()
        || value.encode_utf16().count() > 1023
        || suffix.split('\\').any(|part| {
            part.is_empty()
                || matches!(part, "." | "..")
                || part.contains(['*', '?'])
                || part.chars().any(char::is_control)
        })
    {
        return Err(invalid("Ungültiger Pfad im Plugin-Dateisystem."));
    }
    Ok(value)
}

fn check_absolute(path: &Path) -> io::Result<()> {
    if !path.is_absolute() {
        return Err(invalid("Ein lokaler Pfad muss absolut sein."));
    }
    Ok(())
}

fn leaf(path: &Path) -> io::Result<&OsStr> {
    files::leaf_name(
        path.file_name()
            .ok_or_else(|| invalid("Ein Dateiname fehlt."))?,
    )
}

fn resolved_target(path: &Path) -> io::Result<PathBuf> {
    check_absolute(path)?;
    let parent = path
        .parent()
        .ok_or_else(|| invalid("Ein Zielordner fehlt."))?;
    let parent = fs::canonicalize(parent)?;
    if !parent.is_dir() {
        return Err(invalid("Der Zielordner fehlt."));
    }
    Ok(parent.join(leaf(path)?))
}

fn destination(path: &Path) -> io::Result<PathBuf> {
    let path = resolved_target(path)?;
    match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(path),
        Err(error) => Err(error),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "Die lokale Zieldatei existiert bereits.",
        )),
    }
}

fn temp_near(path: &Path) -> io::Result<tempfile::TempDir> {
    tempfile::Builder::new()
        .prefix("arco-plugin-")
        .tempdir_in(path.parent().expect("resolved target"))
}

fn regular(path: &Path) -> io::Result<Metadata> {
    let metadata = files::regular_metadata(path)?;
    if !metadata.is_file() {
        return Err(invalid("Eine reguläre Datei wird benötigt."));
    }
    Ok(metadata)
}

fn snapshot(
    source: &Path,
    target: &Path,
    metadata: &Metadata,
    cancel: &AtomicBool,
) -> io::Result<()> {
    let mut output = File::create_new(target)?;
    let copied = tools::copy_cancelled(File::open(source)?, &mut output, cancel)?;
    if copied != metadata.len() {
        return Err(io::Error::other(
            "Die Quelldatei wurde beim Lesen geändert.",
        ));
    }
    files::ensure_unchanged(source, metadata)?;
    archive_view::preserve_timestamp(&output, metadata.modified().ok())?;
    output.sync_all()
}

fn snapshot_tree(
    source: &Path,
    target: &Path,
    cancel: &AtomicBool,
) -> io::Result<Vec<(PathBuf, Metadata)>> {
    if !files::regular_metadata(source)?.is_dir() {
        return Err(invalid("Ein regulärer Ordner wird benötigt."));
    }
    let mut entries = Vec::new();
    for (index, entry) in walkdir::WalkDir::new(source)
        .follow_links(false)
        .into_iter()
        .enumerate()
    {
        files::check_cancel(cancel)?;
        let entry = entry.map_err(io::Error::other)?;
        if index >= WFX_TREE_ENTRIES || entry.depth() > WFX_TREE_DEPTH {
            return Err(invalid("WFX-Ordner überschreitet das Transferlimit."));
        }
        let path = entry.path();
        let relative = path.strip_prefix(source).map_err(io::Error::other)?;
        if entry.depth() != 0 {
            leaf(path)?;
        }
        let target = target.join(relative);
        let metadata = files::regular_metadata(path)?;
        if metadata.is_dir() {
            fs::create_dir(target)?;
        } else {
            snapshot(path, &target, &metadata, cancel)?;
        }
        entries.push((relative.to_path_buf(), metadata));
    }
    Ok(entries)
}

fn remove_local_snapshot(
    source: &Path,
    staged: &Path,
    entries: &[(PathBuf, Metadata)],
    cancel: &AtomicBool,
) -> io::Result<()> {
    let path_at = |root: &Path, relative: &Path| {
        if relative.as_os_str().is_empty() {
            root.to_path_buf()
        } else {
            root.join(relative)
        }
    };
    let expected: std::collections::HashSet<_> = entries
        .iter()
        .map(|(relative, _)| relative.as_path())
        .collect();
    for (relative, metadata) in entries {
        let path = path_at(source, relative);
        files::check_cancel(cancel)?;
        files::ensure_unchanged(&path, metadata)?;
        if metadata.is_dir() {
            for child in fs::read_dir(&path)? {
                files::check_cancel(cancel)?;
                let child = child?.path();
                let relative = child.strip_prefix(source).map_err(io::Error::other)?;
                if !expected.contains(relative) {
                    return Err(io::Error::other(format!(
                        "Neuer Eintrag im Quellordner: {}",
                        child.display()
                    )));
                }
            }
        }
        if metadata.is_file()
            && !tools::contents_match(&path, &path_at(staged, relative), || {
                cancel.load(std::sync::atomic::Ordering::Relaxed)
            })?
        {
            return Err(io::Error::other(format!(
                "Quelldatei geändert: {}",
                path.display()
            )));
        }
    }
    for (relative, metadata) in entries.iter().rev() {
        let path = path_at(source, relative);
        files::check_cancel(cancel)?;
        if metadata.is_dir() {
            let current = files::regular_metadata(&path)?;
            if !current.is_dir() || !files::same_identity_hint(&current, metadata) {
                return Err(io::Error::other(format!(
                    "Quellordner geändert: {}",
                    path.display()
                )));
            }
            fs::remove_dir(&path)?;
        } else {
            files::ensure_unchanged(&path, metadata)?;
            fs::remove_file(&path)?;
        }
    }
    Ok(())
}

fn publish(
    staged: PathBuf,
    target: &Path,
    before: Option<&Metadata>,
    cancel: &AtomicBool,
) -> io::Result<()> {
    regular(&staged)?;
    File::options().write(true).open(&staged)?.sync_all()?;
    files::check_cancel(cancel)?;
    let staged = tempfile::TempPath::try_from_path(staged)?;
    if let Some(before) = before {
        files::ensure_unchanged(target, before)?;
        staged.persist(target)
    } else {
        staged.persist_noclobber(target)
    }
    .map_err(|error| error.error)
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn wfx_download_conflicts_preserve_target_on_skip_and_use_renamed_path_for_cleanup() {
        let fixture = tempfile::tempdir().unwrap();
        let target = fixture.path().join("sample.txt");
        let cancel = AtomicBool::new(false);
        fs::write(&target, b"existing").unwrap();
        let stage = || {
            let output = stage_wfx(
                WfxOperation::DownloadMove {
                    remote: r"\files\sample.txt".into(),
                    local: target.clone(),
                    tree: false,
                },
                &cancel,
                true,
            )
            .unwrap();
            fs::write(&output.output.as_ref().unwrap().staged, b"downloaded").unwrap();
            output
        };
        let skip = |_: &str, _: files::FileConflict<'_>| Ok(files::Conflict::Skip.into());
        assert!(matches!(
            stage().finish(&cancel, None, Some(&skip)).unwrap(),
            Finish::Skipped
        ));
        assert_eq!(fs::read(&target).unwrap(), b"existing");
        let rename = |remote: &str, _: files::FileConflict<'_>| {
            assert_eq!(remote, r"\files\sample.txt");
            Ok(files::Conflict::Rename.into())
        };
        let Finish::Remove(WfxOperation::RemoveDownloaded { local, .. }) =
            stage().finish(&cancel, None, Some(&rename)).unwrap()
        else {
            panic!("expected verified removal to be scheduled");
        };
        assert_eq!(local.file_name().unwrap(), "sample (2).txt");
        assert_eq!(fs::read(local).unwrap(), b"downloaded");
        assert_eq!(fs::read(&target).unwrap(), b"existing");
    }

    #[test]
    fn remote_paths_reject_root_wildcards_and_parent_components() {
        assert_eq!(
            checked_remote("/project/Über.txt").unwrap(),
            r"\project\Über.txt"
        );
        for path in [
            "\\",
            r"\\server",
            r"\project\..",
            r"\project\*.txt",
            "file.txt",
        ] {
            assert!(checked_remote(path).is_err(), "{path}");
        }
        for member in [r"..\file", r"a\..\file", "C:/file", "a/*.txt"] {
            assert!(checked_member(member).is_err(), "{member}");
        }
    }

    #[test]
    fn remote_tree_rejects_its_own_descendants_and_accepts_siblings() {
        for destination in [
            r"\Server\Photos",
            r"\server\photos",
            r"\Server\Photos\Backup",
        ] {
            assert!(remote_pair(r"\Server\Photos", destination, true).is_err());
        }
        assert!(remote_pair(r"\Server\Photos", r"\Server\Photos-old", true).is_ok());
    }

    #[test]
    fn local_move_keeps_the_entire_source_when_snapshot_content_changed() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let staged = temp.path().join("staged");
        fs::create_dir_all(source.join("nested")).unwrap();
        fs::write(source.join("first.txt"), b"first").unwrap();
        let changed = source.join("nested").join("second.txt");
        fs::write(&changed, b"before").unwrap();
        let cancel = AtomicBool::new(false);
        let entries = snapshot_tree(&source, &staged, &cancel).unwrap();
        let modified =
            filetime::FileTime::from_last_modification_time(&fs::metadata(&changed).unwrap());
        fs::write(&changed, b"edited").unwrap();
        filetime::set_file_mtime(&changed, modified).unwrap();
        assert!(remove_local_snapshot(&source, &staged, &entries, &cancel).is_err());
        assert_eq!(fs::read(source.join("first.txt")).unwrap(), b"first");
        assert_eq!(fs::read(changed).unwrap(), b"edited");
        assert_eq!(
            fs::read(staged.join("nested").join("second.txt")).unwrap(),
            b"before"
        );
    }

    #[test]
    fn plugin_publication_respects_existing_target_and_cancellation() {
        let dir = tempfile::tempdir().unwrap();
        let staged = dir.path().join("staged");
        let target = dir.path().join("target");
        let cancel = AtomicBool::new(false);
        fs::write(&target, b"existing").unwrap();
        fs::write(&staged, b"modified").unwrap();
        assert_eq!(
            publish(staged.clone(), &target, None, &cancel)
                .unwrap_err()
                .kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(fs::read(&target).unwrap(), b"existing");
        fs::write(&staged, b"cancelled").unwrap();
        cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        assert_eq!(
            publish(staged, &target, None, &cancel).unwrap_err().kind(),
            io::ErrorKind::Interrupted
        );
        assert_eq!(fs::read(&target).unwrap(), b"existing");
    }
}
