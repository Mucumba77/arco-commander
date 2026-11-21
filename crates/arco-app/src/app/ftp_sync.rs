use super::{
    FtpEndpoint, JobContext,
    transfer_queue::{
        ConflictSide, VIRTUAL_CONFLICT_PREVIEW_LIMIT, ask_conflict, conflict_preview,
    },
};
use arco_app::network_batch::{Batch, copy_groups};
pub(crate) use arco_app::network_sync::modified;
use arco_app::network_sync::{
    CopyChoices, IgnorePath, append_to_ftp_snapshot, copy_names, file_worker_endpoint,
    ftp_timestamp, run_rows, scan_with_aliases, skip_existing_copies,
};
pub(super) use arco_app::network_sync::{FtpPlan, MAX_DEPTH, MAX_ENTRIES, ftp_ignores};
use arco_core::files::{self, Conflict, ConflictDecision, TransferReport};
use arco_core::sync::{self, Action, EntryInfo, LocalRoot, Mode, Options, Row, Scope};
use arco_windows::ftp::{self, FxpCopy, SyncUpload};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use tempfile::NamedTempFile;

pub(super) struct FtpRelayPlan {
    pub rows: Vec<Row>,
    endpoints: [FtpEndpoint; 2],
    entries: [BTreeMap<PathBuf, ftp::Entry>; 2],
    choices: CopyChoices,
    source_paths: Option<BTreeMap<PathBuf, PathBuf>>,
    direct_fxp: bool,
}

impl FtpRelayPlan {
    pub(super) fn validate_direct(endpoints: &[FtpEndpoint; 2]) -> io::Result<()> {
        if !endpoints[0].0.can_fxp_to(&endpoints[1].0) {
            return Err(io::Error::other(
                "FXP benötigt zwei direkt verbundene Klartext-FTP-Server mit einer gemeinsamen IP-Version.",
            ));
        }
        if endpoints.iter().any(|(session, _)| {
            session
                .transfer_settings()
                .speed_kib
                .is_some_and(|rate| rate > 0)
        }) {
            return Err(io::Error::other(
                "Bei FXP bestimmen die Server den Durchsatz. Setze das Limit für diesen Auftrag auf 0 KiB/s.",
            ));
        }
        Ok(())
    }

    pub(super) fn use_direct_fxp(&mut self) {
        self.direct_fxp = true;
    }

    pub(super) fn is_direct_fxp(&self) -> bool {
        self.direct_fxp
    }

    pub(super) fn endpoints_mut(&mut self) -> &mut [FtpEndpoint; 2] {
        &mut self.endpoints
    }

    pub(super) fn for_copy(
        endpoints: [FtpEndpoint; 2],
        selected: &[PathBuf],
        ignored: &IgnorePath<'_>,
        cancel: &AtomicBool,
    ) -> io::Result<Self> {
        if endpoints[1].0.is_http_forward_ftp() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "FTP über HTTP-Forward kann nur als Quelle dienen.",
            ));
        }
        let [source, target] = endpoints.each_ref().map(|endpoint| &endpoint.1);
        if source.host_str() == target.host_str()
            && source.port_or_known_default() == target.port_or_known_default()
        {
            let source = source.path().to_lowercase();
            let target = target.path().to_lowercase();
            if source.starts_with(&target) || target.starts_with(&source) {
                return Err(io::Error::other(
                    "Die FTP-Quell- und Zielordner müssen getrennt sein.",
                ));
            }
        }
        let options = Options {
            mode: Mode::LeftToRight,
            selected_only: true,
            by_content: true,
            ignore_date: true,
            ..Options::default()
        };
        let rules = endpoints
            .each_ref()
            .map(|endpoint| endpoint.0.transfer_settings().names);
        let mapped = rules.iter().any(|rules| !rules.is_default());
        let destination = |path: &Path| -> io::Result<PathBuf> {
            if !mapped {
                return Ok(path.to_owned());
            }
            path.iter()
                .map(|name| {
                    let name = name
                        .to_str()
                        .ok_or_else(|| io::Error::other("FTP-Pfade benötigen Unicode."))?;
                    let name = copy_names::decode_remote_name(name)?;
                    let intermediate = rules[0].download(&name)?;
                    copy_names::encode_remote_name(&rules[1].upload(&intermediate)?)
                })
                .collect()
        };
        let selected = if mapped {
            selected
                .iter()
                .map(|name| {
                    let name = name
                        .to_str()
                        .ok_or_else(|| io::Error::other("FTP-Pfade benötigen Unicode."))?;
                    copy_names::encode_remote_name(name).map(PathBuf::from)
                })
                .collect::<io::Result<Vec<_>>>()?
        } else {
            selected.to_vec()
        };
        let targets = selected
            .iter()
            .map(|path| destination(path))
            .collect::<io::Result<Vec<_>>>()?;
        let source_scope = Scope::new(true, &selected)?;
        let target_scope = Scope::new(true, &targets)?;
        let [source_masks, target_masks] = [
            sync::FileMasks::new(endpoints[0].0.transfer_masks())?,
            sync::FileMasks::new(endpoints[1].0.transfer_masks())?,
        ];
        let excluded = RefCell::new(BTreeSet::new());
        let mapping_error = RefCell::new(None);
        let skip = |path: &Path, directory| {
            let mut skip = ignored(path, directory) || (!directory && !source_masks.includes(path));
            if !skip {
                match destination(path) {
                    Ok(target) => {
                        skip = (mapped && ignored(&target, directory))
                            || (!directory && !target_masks.includes(&target));
                    }
                    Err(error) => {
                        mapping_error.borrow_mut().get_or_insert(error);
                        return true;
                    }
                }
            }
            if skip {
                excluded.borrow_mut().insert(path.to_path_buf());
            }
            skip
        };
        let keep = |_: &Path, _: bool| false;
        let [(mut left, source), (mut right, target)] = [
            scan_with_aliases(
                &endpoints[0],
                &source_scope,
                true,
                &skip,
                mapped,
                cancel,
                &mut |_| {},
            )?,
            scan_with_aliases(
                &endpoints[1],
                &target_scope,
                true,
                if mapped { &keep } else { &skip },
                mapped,
                cancel,
                &mut |_| {},
            )?,
        ];
        if let Some(error) = mapping_error.into_inner() {
            return Err(error);
        }
        let excluded = excluded.into_inner();
        left.discard_subtrees(&excluded);
        let source_paths = if mapped {
            Some(left.remap_paths(destination)?)
        } else {
            right.discard_subtrees(&excluded);
            None
        };
        let mut links = BTreeSet::new();
        for (path, entry) in &source {
            if entry.symlink {
                links.insert(destination(path)?);
            }
        }
        links.extend(
            target
                .iter()
                .filter(|(_, entry)| entry.symlink)
                .map(|(path, _)| path.clone()),
        );
        left.discard_subtrees(&links);
        right.discard_subtrees(&links);
        let mut rows =
            sync::compare_trees([&left, &right], &options, &target_scope, cancel, |_, _| {
                Ok(false)
            })?;
        skip_existing_copies(&mut rows, 0);
        Ok(Self {
            rows,
            endpoints,
            entries: [source, target],
            choices: CopyChoices::default(),
            source_paths,
            direct_fxp: false,
        })
    }

    pub(super) fn resolve_conflicts(&mut self, job: &JobContext) -> io::Result<usize> {
        let mut rows = std::mem::take(&mut self.rows);
        let mut choices = std::mem::take(&mut self.choices);
        let sides = [
            CopySide::Remote(self.side(0)),
            CopySide::Remote(self.side(1)),
        ];
        let result = resolve_copy_conflicts(&mut rows, sides, &mut choices, !self.direct_fxp, job);
        self.rows = rows;
        self.choices = choices;
        result
    }

    fn side(&self, index: usize) -> RemoteCopySide<'_> {
        RemoteCopySide {
            endpoint: &self.endpoints[index],
            entries: &self.entries[index],
            original: if index == 0 {
                self.source_paths.as_ref()
            } else {
                None
            },
            escaped: self.source_paths.is_some(),
            preview_contents: !self.direct_fxp,
        }
    }

    pub(super) fn preview_paths(&self, row: &Row) -> Option<(PathBuf, PathBuf)> {
        self.source_paths.as_ref()?;
        Some((
            self.side(0).reported(row.path(0)).ok()?,
            self.side(1).reported(row.path(1)).ok()?,
        ))
    }

    pub(super) fn apply(
        &self,
        cancel: &AtomicBool,
        mut progress: impl FnMut(&Path),
    ) -> TransferReport {
        let mut report = TransferReport::default();
        run_rows(
            self.rows
                .iter()
                .filter(|row| row.action == Action::CopyRight && row.can(row.action)),
            &mut report,
            cancel,
            &mut progress,
            |row| self.copy(row, &self.endpoints, cancel),
        );
        report
    }

    pub(super) fn apply_background_copy(
        &self,
        batch: Batch<'_>,
        progress: impl FnMut(&Path),
    ) -> TransferReport {
        copy_groups(
            batch,
            self.rows
                .iter()
                .filter(|row| row.action == Action::CopyRight && row.can(row.action)),
            |row| {
                if row.info(0).is_none_or(EntryInfo::is_dir)
                    || self.choices.append.contains(&row.relative)
                    || self.choices.alternate.contains_key(&row.relative)
                {
                    return None;
                }
                Some(
                    self.side(1)
                        .reported(row.path(1))
                        .ok()?
                        .to_string_lossy()
                        .to_lowercase(),
                )
            },
            |index, width| {
                self.endpoints
                    .each_ref()
                    .map(|endpoint| file_worker_endpoint(endpoint, index, width))
            },
            |endpoints, row| self.copy(row, endpoints, batch.cancel),
            progress,
        )
    }

    fn copy(&self, row: &Row, endpoints: &[FtpEndpoint; 2], cancel: &AtomicBool) -> io::Result<()> {
        let [source, target] = endpoints;
        let from = self.side(0);
        let to = self.side(1);
        let source_path = from.physical(row.path(0));
        let source_parent = from.parent(source_path)?;
        let alternate = self.choices.alternate.get(&row.relative);
        let destination = alternate.map_or_else(|| row.path(1), PathBuf::as_path);
        let target_parent = to.parent(destination)?;
        let entry = &self.entries[0][source_path];
        let name = destination
            .file_name()
            .and_then(OsStr::to_str)
            .ok_or_else(|| io::Error::other("FTP-Dateinamen benötigen Unicode."))?;
        let decoded;
        let name = if alternate.is_none() && to.escaped {
            decoded = copy_names::decode_remote_name(name)?;
            decoded.as_str()
        } else {
            name
        };
        if entry.directory == Some(true) {
            source.0.verify_sync_entry(&source_parent, entry, cancel)?;
            return target.0.create_sync_directory(&target_parent, name, cancel);
        }
        let previous = if alternate.is_some() {
            None
        } else {
            self.entries[1].get(row.path(1))
        };
        let stamp = modified(entry)?
            .filter(|_| target.0.preserve_transfer_time(true))
            .map(ftp_timestamp);
        if self.direct_fxp {
            return source
                .0
                .fxp_copy_sync(
                    &source_parent,
                    &target.0,
                    &target_parent,
                    FxpCopy {
                        source: entry,
                        target_name: name,
                        expected_target: previous,
                        modified_utc: stamp.as_deref(),
                    },
                    cancel,
                )
                .map(|_| ());
        }
        let temporary = NamedTempFile::new()?;
        source
            .0
            .fetch_sync(&source_parent, entry, temporary.as_file(), cancel)?;
        let temporary = temporary.into_temp_path();
        let metadata = files::regular_metadata(&temporary)?;
        let joined = if self.choices.append.contains(&row.relative) {
            Some(append_to_ftp_snapshot(
                &target.0,
                &target_parent,
                previous,
                &temporary,
                &metadata,
                cancel,
            )?)
        } else {
            None
        };
        let (upload, size) = if let Some(joined) = &joined {
            (joined.path(), joined.as_file().metadata()?.len())
        } else {
            (temporary.as_ref(), metadata.len())
        };
        target
            .0
            .upload_sync(
                &target_parent,
                SyncUpload {
                    source: upload,
                    name,
                    expected: previous,
                    size,
                    modified_utc: stamp.as_deref(),
                },
                || source.0.verify_sync_entry(&source_parent, entry, cancel),
                cancel,
            )
            .map(|_| ())
    }
}

pub(super) fn enable_copy_conflicts(rows: &mut [Row], source: usize) {
    let action = [Action::CopyRight, Action::CopyLeft][source];
    for row in rows {
        if row.can(action) && (0..2).all(|side| row.info(side).is_some_and(|info| !info.is_dir())) {
            row.action = action;
        }
    }
}

#[derive(Clone, Copy)]
struct RemoteCopySide<'a> {
    endpoint: &'a FtpEndpoint,
    entries: &'a BTreeMap<PathBuf, ftp::Entry>,
    original: Option<&'a BTreeMap<PathBuf, PathBuf>>,
    escaped: bool,
    preview_contents: bool,
}

impl RemoteCopySide<'_> {
    fn physical<'a>(&'a self, path: &'a Path) -> &'a Path {
        self.original
            .and_then(|paths| paths.get(path))
            .map_or(path, PathBuf::as_path)
    }

    fn parent(&self, path: &Path) -> io::Result<ftp::Url> {
        copy_names::remote_parent(self.endpoint, path, self.escaped)
    }

    fn reported(&self, path: &Path) -> io::Result<PathBuf> {
        copy_names::reported_path(self.physical(path), self.escaped)
    }
}

#[derive(Clone, Copy)]
enum CopySide<'a> {
    Local(&'a LocalRoot, &'a FtpPlan),
    Remote(RemoteCopySide<'a>),
}

impl CopySide<'_> {
    fn conflict_side(
        self,
        row: &Row,
        index: usize,
        info: EntryInfo,
        job: &JobContext,
    ) -> io::Result<ConflictSide> {
        let path = row.path(index);
        match self {
            Self::Local(root, plan) => {
                let path = plan.original_copy_path(0, path);
                let metadata = row.metadata(index).expect("local FTP copy side");
                Ok(ConflictSide::local(
                    &root.checked_path(path)?,
                    metadata,
                    None,
                ))
            }
            Self::Remote(remote) => {
                let path = remote.physical(path);
                let display = copy_names::reported_path(path, remote.escaped)?;
                let mut url = remote.endpoint.1.clone();
                let _ = url.set_username("");
                let _ = url.set_password(None);
                url.set_query(None);
                url.set_fragment(None);
                let mut side = ConflictSide::virtual_entry(
                    format!(
                        "{}/{}",
                        url.as_str().trim_end_matches('/'),
                        display.display()
                    ),
                    info.size,
                    info.modified,
                );
                if !remote.preview_contents {
                    return Ok(side);
                }
                if info
                    .size
                    .is_some_and(|size| size <= VIRTUAL_CONFLICT_PREVIEW_LIMIT)
                {
                    let load = || {
                        let expected = remote
                            .entries
                            .get(path)
                            .ok_or_else(|| io::Error::other("Der FTP-Dateieintrag fehlt."))?;
                        let parent = remote.parent(path)?;
                        let file = NamedTempFile::new()?;
                        remote.endpoint.0.fetch_sync(
                            &parent,
                            expected,
                            file.as_file(),
                            &job.cancel,
                        )?;
                        Ok::<_, io::Error>(conflict_preview(
                            file.path(),
                            &file.as_file().metadata()?,
                        ))
                    };
                    side.preview = match load() {
                        Ok(preview) => preview,
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                            return Err(error);
                        }
                        Err(_) => {
                            "Die FTP-Datei konnte für die Vorschau nicht gelesen werden.".into()
                        }
                    };
                } else {
                    side.preview = "FTP-Vorschau für Dateien bis 64 KiB verfügbar.".into();
                }
                Ok(side)
            }
        }
    }

    fn occupied(self, path: &Path, cancel: &AtomicBool) -> io::Result<BTreeSet<String>> {
        match self {
            Self::Local(root, _) => {
                let path = root.checked_path(path)?;
                fs::read_dir(path.parent().expect("checked path"))?
                    .map(|entry| {
                        entry.map(|entry| entry.file_name().to_string_lossy().to_lowercase())
                    })
                    .collect()
            }
            Self::Remote(remote) => {
                let parent = remote.parent(path)?;
                let mut occupied = BTreeSet::new();
                for entry in remote.endpoint.0.list_sync(&parent, cancel)?.entries {
                    occupied.insert(entry.name.to_lowercase());
                    if remote.escaped {
                        occupied
                            .insert(copy_names::encode_remote_name(&entry.name)?.to_lowercase());
                    }
                }
                Ok(occupied)
            }
        }
    }
}

fn resolve_copy_conflicts(
    rows: &mut [Row],
    sides: [CopySide<'_>; 2],
    choices: &mut CopyChoices,
    allow_append: bool,
    job: &JobContext,
) -> io::Result<usize> {
    let mut skipped = 0;
    *choices = CopyChoices::default();
    let mut remaining = None;
    let mut reserved = BTreeMap::<(usize, String), BTreeSet<String>>::new();
    let mut listed = BTreeSet::new();
    let directory = |side, path: &Path| {
        (
            side,
            path.parent()
                .unwrap_or(Path::new(""))
                .to_string_lossy()
                .to_lowercase(),
        )
    };
    for row in rows.iter() {
        if let Some(target) = target_side(row.action) {
            let path = row.path(target);
            if let Some(name) = path.file_name() {
                reserved
                    .entry(directory(target, path))
                    .or_default()
                    .insert(name.to_string_lossy().to_lowercase());
            }
        }
    }
    for row in rows {
        files::check_cancel(&job.cancel)?;
        let Some(target) = target_side(row.action) else {
            continue;
        };
        let source = 1 - target;
        let [Some(from), Some(to)] = [row.info(source), row.info(target)] else {
            continue;
        };
        if from.is_dir() || to.is_dir() {
            continue;
        }
        let decision = if let Some(rule) = remaining {
            ConflictDecision::Apply(rule)
        } else {
            let mut rules = vec![
                (Conflict::Skip, "Überspringen"),
                (Conflict::Replace, "Überschreiben"),
                (Conflict::Newer, "Nur mit neuerer Datei überschreiben"),
                (Conflict::Larger, "Nur mit größerer Datei überschreiben"),
                (Conflict::Rename, "Unter freiem Namen speichern"),
            ];
            if allow_append {
                rules.push((Conflict::Append, "An Zieldatei anhängen"));
            }
            let (decision, all) = ask_conflict(
                job,
                sides[source].conflict_side(row, source, from, job)?,
                sides[target].conflict_side(row, target, to, job)?,
                rules,
            )?;
            if all && let ConflictDecision::Apply(rule) = decision {
                remaining = Some(rule);
            }
            decision
        };
        let proceed = match decision {
            ConflictDecision::Apply(Conflict::Replace) => true,
            ConflictDecision::Apply(Conflict::Newer) => {
                from.modified.zip(to.modified).is_some_and(|(a, b)| a > b)
            }
            ConflictDecision::Apply(Conflict::Larger) => from.size > to.size,
            ConflictDecision::Apply(Conflict::Append) => {
                choices.append.insert(row.relative.clone());
                true
            }
            ConflictDecision::Apply(Conflict::Rename) | ConflictDecision::RenameTo(_) => {
                let path = row.path(target);
                let key = directory(target, path);
                let names = reserved.entry(key.clone()).or_default();
                if listed.insert(key) {
                    names.extend(sides[target].occupied(path, &job.cancel)?);
                }
                let candidate = match decision {
                    ConflictDecision::RenameTo(name) => {
                        files::leaf_name(&name)?;
                        if !names.insert(name.to_string_lossy().to_lowercase()) {
                            return Err(io::Error::new(
                                io::ErrorKind::AlreadyExists,
                                "Der gewählte FTP-Zielname ist bereits vorhanden oder vorgemerkt.",
                            ));
                        }
                        path.with_file_name(name)
                    }
                    _ => {
                        let mut candidate = None;
                        for index in 2..=10_000 {
                            let name = files::numbered_copy_name(path, index)?;
                            if names.insert(
                                name.file_name()
                                    .expect("copy name")
                                    .to_string_lossy()
                                    .to_lowercase(),
                            ) {
                                candidate = Some(name);
                                break;
                            }
                        }
                        candidate
                            .ok_or_else(|| io::Error::other("Kein freier FTP-Zielname gefunden."))?
                    }
                };
                choices.alternate.insert(row.relative.clone(), candidate);
                true
            }
            _ => false,
        };
        if !proceed {
            row.action = Action::Skip;
            skipped += 1;
        }
    }
    Ok(skipped)
}

fn target_side(action: Action) -> Option<usize> {
    match action {
        Action::CopyRight => Some(1),
        Action::CopyLeft => Some(0),
        _ => None,
    }
}

pub(super) trait FtpPlanConflicts {
    fn resolve_conflicts(&mut self, job: &JobContext) -> io::Result<usize>;
}

impl FtpPlanConflicts for FtpPlan {
    fn resolve_conflicts(&mut self, job: &JobContext) -> io::Result<usize> {
        let mut rows = std::mem::take(&mut self.rows);
        let mut choices = std::mem::take(&mut self.choices);
        let sides = [
            CopySide::Local(&self.local, self),
            CopySide::Remote(RemoteCopySide {
                endpoint: &self.endpoint,
                entries: &self.expected,
                original: self.copy_source_paths(1),
                escaped: self.escaped_remote_copy_names(),
                preview_contents: true,
            }),
        ];
        let result = resolve_copy_conflicts(&mut rows, sides, &mut choices, true, job);
        self.rows = rows;
        self.choices = choices;
        result
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use arco_core::ignore::IgnoreList;

    #[test]
    #[ignore = "requires scripts/test-ftp-sync-reference.py with ARCO_TEST_FXP_COPY=1"]
    fn direct_fxp_copy_reviews_and_applies_a_directory() {
        use arco_windows::ftp::{Connection, Security, TransferSettings};
        use std::sync::Arc;
        use std::sync::atomic::AtomicBool;

        let connect = |variable| {
            let address = std::env::var(variable).unwrap();
            let session = Connection::new(&address, "arco", Security::Plain)
                .unwrap()
                .authenticate("test-only")
                .unwrap()
                .with_transfer_snapshot(TransferSettings {
                    masks: "*.txt".into(),
                    preserve_time: Some(true),
                    ..Default::default()
                });
            let directory = session.initial_directory();
            (Arc::new(session), directory)
        };
        let endpoints = [
            connect("ARCO_TEST_FXP_COPY_SOURCE"),
            connect("ARCO_TEST_FXP_COPY_TARGET"),
        ];
        let cancel = Arc::new(AtomicBool::new(false));
        FtpRelayPlan::validate_direct(&endpoints).unwrap();
        let mut plan = FtpRelayPlan::for_copy(
            endpoints,
            &[PathBuf::from("batch")],
            &|path, _| path.ends_with("ignored.txt"),
            &cancel,
        )
        .unwrap();
        assert!(!plan.is_direct_fxp());
        plan.use_direct_fxp();
        enable_copy_conflicts(&mut plan.rows, 0);
        let (sender, receiver) = std::sync::mpsc::channel();
        let job = JobContext {
            archives: Default::default(),
            cancel: cancel.clone(),
            sender,
        };
        std::thread::scope(|scope| {
            let worker = scope.spawn(|| plan.resolve_conflicts(&job));
            for (rule, custom_name) in [(5, "gewählt.txt"), (1, "")] {
                let super::super::Event::TransferConflict(ticket) = receiver
                    .recv_timeout(std::time::Duration::from_secs(15))
                    .unwrap()
                else {
                    panic!("expected a target conflict")
                };
                assert!(
                    ticket
                        .previews()
                        .iter()
                        .all(|preview| !preview.contains("payload"))
                );
                ticket.answer(rule, false, custom_name);
            }
            assert_eq!(worker.join().unwrap().unwrap(), 0);
        });
        let report = plan.apply(&cancel, |_| {});
        assert_eq!(report.errors.len(), 1, "{:?}", report.errors);
        assert!(
            report.errors[0].contains("race.txt") && report.errors[0].contains(".arco-fxp-"),
            "{:?}",
            report.errors
        );
        assert!(report.completed >= 5, "{:?}", report.errors);
    }
    use std::sync::Arc;

    fn test_server(variable: &str, name: &str, cancel: &AtomicBool) -> FtpEndpoint {
        let address = std::env::var(variable).unwrap();
        let session = Arc::new(
            ftp::Connection::new(&address, "arco", ftp::Security::Plain)
                .unwrap()
                .authenticate("test-only")
                .unwrap(),
        );
        let root = session.initial_directory();
        session.create_sync_directory(&root, name, cancel).unwrap();
        let directory = session.relative_directory(&root, Path::new(name)).unwrap();
        (session, directory)
    }

    #[test]
    #[ignore = "requires ARCO_TEST_HASH_FTP with HASH and RETR disabled except for fallback.txt"]
    fn ftp_content_comparison_uses_advertised_hash_without_retr() {
        let cancel = AtomicBool::new(false);
        let endpoint = test_server("ARCO_TEST_HASH_FTP", "hash compare", &cancel);
        let remote = endpoint
            .0
            .relative_directory(&endpoint.1, Path::new("sub folder"))
            .unwrap();
        endpoint
            .0
            .create_sync_directory(&endpoint.1, "sub folder", &cancel)
            .unwrap();
        let fixture = tempfile::tempdir().unwrap();
        let local = fixture.path().join("local");
        let folder = local.join("sub folder");
        fs::create_dir_all(&folder).unwrap();
        let seed = fixture.path().join("seed");
        fs::write(&seed, b"remote").unwrap();
        for (name, bytes) in [
            ("same file.txt", b"remote"),
            ("different.txt", b"local!"),
            ("fallback.txt", b"remote"),
        ] {
            fs::write(folder.join(name), bytes).unwrap();
            endpoint
                .0
                .upload(&remote, &seed, name, false, &cancel)
                .unwrap();
        }
        let expected = endpoint
            .0
            .list_sync(&remote, &cancel)
            .unwrap()
            .entries
            .into_iter()
            .find(|entry| entry.name == "same file.txt")
            .unwrap();
        let tmp = NamedTempFile::new().unwrap();
        assert!(
            endpoint
                .0
                .fetch_sync(&remote, &expected, tmp.as_file(), &cancel)
                .is_err()
        );
        let options = Options {
            recursive: true,
            by_content: true,
            ignore_date: true,
            ..Options::default()
        };
        let plan = FtpPlan::compare(
            &local,
            endpoint,
            &options,
            &[],
            &|_, _| false,
            &cancel,
            |_| {},
        )
        .unwrap();
        for (name, comparison) in [
            ("same file.txt", sync::Comparison::Equal),
            ("different.txt", sync::Comparison::Different),
            ("fallback.txt", sync::Comparison::Equal),
        ] {
            let row = plan
                .rows
                .iter()
                .find(|row| row.relative == Path::new("sub folder").join(name))
                .unwrap();
            assert_eq!(row.comparison, comparison);
        }
    }

    #[test]
    #[ignore = "requires a disposable writable local FTP server"]
    fn ftp_mirror_protects_directories_containing_ignored_remote_files() {
        let cancel = AtomicBool::new(false);
        let endpoint = test_server("ARCO_TEST_FTP", "ignore mirror", &cancel);
        let fixture = tempfile::tempdir().unwrap();
        let local = fixture.path().join("local");
        fs::create_dir(&local).unwrap();
        let seed = fixture.path().join("seed");
        fs::write(&seed, b"sample").unwrap();
        endpoint
            .0
            .create_sync_directory(&endpoint.1, "folder", &cancel)
            .unwrap();
        let folder = endpoint
            .0
            .relative_directory(&endpoint.1, Path::new("folder"))
            .unwrap();
        for name in ["keep.txt", "remove.txt"] {
            endpoint
                .0
                .upload(&folder, &seed, name, false, &cancel)
                .unwrap();
        }
        let rules = IgnoreList::parse("FTP:>Testprofil:keep.txt").unwrap();
        let ignored = ftp_ignores(Some(&rules), &endpoint.1, Some("Testprofil"));
        let options = Options {
            mode: Mode::Mirror,
            ..Options::default()
        };
        let plan = FtpPlan::compare(
            &local,
            endpoint.clone(),
            &options,
            &[],
            &ignored,
            &cancel,
            |_| {},
        )
        .unwrap();
        assert!(
            plan.rows
                .iter()
                .all(|row| !row.relative.ends_with("keep.txt"))
        );
        let parent = plan
            .rows
            .iter()
            .find(|row| row.relative == Path::new("folder"))
            .unwrap();
        assert!(!parent.can(Action::DeleteRight));
        let report = plan.apply(&cancel, |_| {});
        assert!(report.errors.is_empty(), "{report:?}");
        assert_eq!(report.completed, 1);
        let names = endpoint.0.list_sync(&folder, &cancel).unwrap().entries;
        assert_eq!(names.len(), 1);
        assert_eq!(names[0].name, "keep.txt");
    }

    #[test]
    #[ignore = "requires a disposable writable local FTP server"]
    fn ftp_mirror_checks_absence_and_keeps_deletions_out_of_failed_copy_runs() {
        let cancel = AtomicBool::new(false);
        let endpoint = test_server("ARCO_TEST_FTP", "mirror", &cancel);
        let fixture = tempfile::tempdir().unwrap();
        let local = fixture.path().join("local");
        fs::create_dir(&local).unwrap();
        let source = local.join("update.txt");
        let seed = fixture.path().join("seed");
        fs::write(&source, b"planned update").unwrap();
        fs::write(&seed, b"old data").unwrap();
        endpoint
            .0
            .create_sync_directory(&endpoint.1, "obsolete", &cancel)
            .unwrap();
        let obsolete = endpoint
            .0
            .relative_directory(&endpoint.1, Path::new("obsolete"))
            .unwrap();
        endpoint
            .0
            .upload(&obsolete, &seed, "nested.txt", false, &cancel)
            .unwrap();
        endpoint
            .0
            .upload(&endpoint.1, &seed, "late.txt", false, &cancel)
            .unwrap();
        let options = Options {
            mode: Mode::Mirror,
            ..Options::default()
        };
        let first = FtpPlan::compare(
            &local,
            endpoint.clone(),
            &options,
            &[],
            &|_, _| false,
            &cancel,
            |_| {},
        )
        .unwrap();
        fs::write(&source, b"changed after preview").unwrap();
        let report = first.apply(&cancel, |_| {});
        assert_eq!(report.completed, 0);
        assert_eq!(report.skipped, 3);
        assert_eq!(report.errors.len(), 1);
        assert_eq!(
            endpoint
                .0
                .list_sync(&endpoint.1, &cancel)
                .unwrap()
                .entries
                .len(),
            2
        );
        let second = FtpPlan::compare(
            &local,
            endpoint.clone(),
            &options,
            &[],
            &|_, _| false,
            &cancel,
            |_| {},
        )
        .unwrap();
        fs::write(local.join("late.txt"), b"left now exists").unwrap();
        let report = second.apply(&cancel, |_| {});
        assert_eq!(report.completed, 3);
        assert_eq!(report.errors.len(), 1);
        let names: BTreeSet<_> = endpoint
            .0
            .list_sync(&endpoint.1, &cancel)
            .unwrap()
            .entries
            .into_iter()
            .map(|entry| entry.name)
            .collect();
        assert_eq!(
            names,
            BTreeSet::from(["late.txt".into(), "update.txt".into()])
        );
    }

    #[test]
    #[ignore = "requires two disposable writable local FTP servers"]
    fn ftp_relay_copies_selected_tree_and_skips_existing_file_until_approved() {
        let cancel = AtomicBool::new(false);
        let source = test_server("ARCO_TEST_FTP", "relay source", &cancel);
        let target = test_server("ARCO_TEST_FXP_TARGET", "relay target", &cancel);
        let fixture = tempfile::tempdir().unwrap();
        let seed = fixture.path().join("seed");
        fs::write(&seed, b"source bytes").unwrap();
        source
            .0
            .create_sync_directory(&source.1, "folder", &cancel)
            .unwrap();
        let directory = source
            .0
            .relative_directory(&source.1, Path::new("folder"))
            .unwrap();
        source
            .0
            .upload(&directory, &seed, "ä # probe.txt", false, &cancel)
            .unwrap();
        source
            .0
            .upload(&source.1, &seed, "exists.txt", false, &cancel)
            .unwrap();
        fs::write(&seed, b"preserve").unwrap();
        target
            .0
            .upload(&target.1, &seed, "exists.txt", false, &cancel)
            .unwrap();
        let selected = [PathBuf::from("folder"), PathBuf::from("exists.txt")];
        let endpoints = [source, target.clone()];
        let plan =
            FtpRelayPlan::for_copy(endpoints.clone(), &selected, &|_, _| false, &cancel).unwrap();
        assert_eq!(
            plan.rows
                .iter()
                .find(|row| row.relative == Path::new("exists.txt"))
                .unwrap()
                .action,
            Action::Skip
        );
        let report = plan.apply(&cancel, |_| {});
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(report.completed, 2);
        let target_file = fixture.path().join("verify.txt");
        target
            .0
            .download(&target.1, "exists.txt", &target_file, &cancel)
            .unwrap();
        assert_eq!(fs::read(&target_file).unwrap(), b"preserve");
        fs::remove_file(&target_file).unwrap();
        let directory = target
            .0
            .relative_directory(&target.1, Path::new("folder"))
            .unwrap();
        target
            .0
            .download(&directory, "ä # probe.txt", &target_file, &cancel)
            .unwrap();
        assert_eq!(fs::read(&target_file).unwrap(), b"source bytes");
        let selected = [PathBuf::from("exists.txt")];
        let resolve = |plan: &mut FtpRelayPlan, choice, name: &str, skipped, before: &str| {
            enable_copy_conflicts(&mut plan.rows, 0);
            let (sender, receiver) = std::sync::mpsc::channel();
            let job = JobContext {
                archives: Default::default(),
                cancel: Arc::new(AtomicBool::new(false)),
                sender,
            };
            std::thread::scope(|scope| {
                let worker = scope.spawn(|| plan.resolve_conflicts(&job));
                let super::super::Event::TransferConflict(ticket) = receiver
                    .recv_timeout(std::time::Duration::from_secs(15))
                    .unwrap()
                else {
                    panic!("expected an FTP conflict");
                };
                assert_eq!(ticket.previews(), ["source bytes", before]);
                ticket.answer(choice, true, name);
                assert_eq!(worker.join().unwrap().unwrap(), skipped);
            });
        };
        let prepare = || {
            FtpRelayPlan::for_copy(endpoints.clone(), &selected, &|_, _| false, &cancel).unwrap()
        };
        let mut plan = prepare();
        resolve(&mut plan, 0, "", 1, "preserve");
        resolve(&mut plan, 6, "gewählt.txt", 0, "preserve");
        assert_eq!(plan.apply(&cancel, |_| {}).completed, 1);
        for (name, content) in [
            ("gewählt.txt", b"source bytes".as_slice()),
            ("exists.txt", b"preserve".as_slice()),
        ] {
            fs::remove_file(&target_file).unwrap();
            target
                .0
                .download(&target.1, name, &target_file, &cancel)
                .unwrap();
            assert_eq!(fs::read(&target_file).unwrap(), content);
        }
        let mut plan = prepare();
        resolve(&mut plan, 1, "", 0, "preserve");
        let report = plan.apply(&cancel, |_| {});
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(report.completed, 1);
        fs::remove_file(&target_file).unwrap();
        target
            .0
            .download(&target.1, "exists.txt", &target_file, &cancel)
            .unwrap();
        assert_eq!(fs::read(&target_file).unwrap(), b"source bytes");
        let mut plan = prepare();
        resolve(&mut plan, 5, "", 0, "source bytes");
        let report = plan.apply(&cancel, |_| {});
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(report.completed, 1);
        fs::remove_file(&target_file).unwrap();
        target
            .0
            .download(&target.1, "exists.txt", &target_file, &cancel)
            .unwrap();
        assert_eq!(fs::read(&target_file).unwrap(), b"source bytessource bytes");
    }

    #[test]
    #[ignore = "requires a disposable writable local FTP server"]
    fn ftp_plan_compares_content_and_synchronizes_both_directions() {
        let address = std::env::var("ARCO_TEST_FTP").unwrap();
        let session = Arc::new(
            ftp::Connection::new(&address, "arco", ftp::Security::Plain)
                .unwrap()
                .authenticate("test-only")
                .unwrap(),
        );
        compare_transfer_round_trip(&session, &session.initial_directory());
    }

    #[test]
    #[ignore = "requires scripts/test-sftp-reference.py"]
    fn ssh_search_and_bidirectional_sync() {
        for (prefix, security) in [
            ("ARCO_SFTP_TEST", ftp::Security::Sftp),
            ("ARCO_SCP_TEST", ftp::Security::Scp),
        ] {
            let mut address =
                ftp::Url::parse(&std::env::var(format!("{prefix}_URL")).unwrap()).unwrap();
            if security == ftp::Security::Scp {
                address.set_scheme("scp").unwrap();
            }
            let session = Arc::new(
                ftp::Connection::new(address.as_str(), "arco", security)
                    .unwrap()
                    .with_ssh(
                        &std::env::var(format!("{prefix}_FINGERPRINT")).unwrap(),
                        None,
                    )
                    .unwrap()
                    .authenticate("fixture-only")
                    .unwrap(),
            );
            compare_transfer_round_trip(&session, &session.initial_directory());
        }
    }

    #[test]
    #[ignore = "requires scripts/test-sftp-reference.py"]
    fn sftp_background_copies_transfer_independent_files_in_parallel() {
        use std::time::{Duration, Instant};
        let cancel = AtomicBool::new(false);
        let session = Arc::new(
            ftp::Connection::new(
                &std::env::var("ARCO_SFTP_TEST_URL").unwrap(),
                "arco",
                ftp::Security::Sftp,
            )
            .unwrap()
            .with_ssh(&std::env::var("ARCO_SFTP_TEST_FINGERPRINT").unwrap(), None)
            .unwrap()
            .with_transfer_settings(ftp::TransferSettings {
                speed_kib: Some(256),
                ..Default::default()
            })
            .unwrap()
            .authenticate("fixture-only")
            .unwrap(),
        );
        let pool = ftp::TransferConnections::default();
        pool.set_limit(2);
        let _job = pool.try_claim().unwrap();
        let batch = Batch {
            connections: Some(&pool),
            cancel: &cancel,
        };
        let local = tempfile::tempdir().unwrap();
        let upload_root = local.path().join("upload");
        let download_root = local.path().join("download");
        fs::create_dir_all(upload_root.join("data")).unwrap();
        fs::create_dir(&download_root).unwrap();
        let remote = PathBuf::from(std::env::var_os("ARCO_SFTP_TEST_ROOT").unwrap());
        let server_root = session.initial_directory();
        let root = |name| {
            session
                .create_sync_directory(&server_root, name, &cancel)
                .unwrap();
            (
                session.clone(),
                session.child_directory(&server_root, name).unwrap(),
            )
        };
        let upload_target = root("arco-parallel-upload");
        let download_source = root("arco-parallel-download");
        let relay_target = root("arco-parallel-relay");
        fs::create_dir(remote.join("arco-parallel-download/data")).unwrap();
        for (name, value) in [("a.bin", 0x19), ("b.bin", 0xa9)] {
            let content = vec![value; 256 * 1024];
            fs::write(upload_root.join("data").join(name), &content).unwrap();
            fs::write(
                remote.join("arco-parallel-download/data").join(name),
                &content,
            )
            .unwrap();
        }
        let selection = [PathBuf::from("data")];
        for (folder, endpoint, upload) in [
            (&upload_root, upload_target, true),
            (&download_root, download_source.clone(), false),
        ] {
            let plan = FtpPlan::for_copy(
                folder,
                endpoint,
                &selection,
                upload,
                &|_, _| false,
                &cancel,
                |_| {},
            )
            .unwrap();
            let started = Instant::now();
            let mut progress = Vec::new();
            let report = plan.apply_background_copy(batch, |path| progress.push(path.to_owned()));
            assert!(report.errors.is_empty(), "{report:?}");
            assert_eq!(report.completed, 3);
            assert_eq!(progress.first(), Some(&PathBuf::from("data")));
            assert!(
                started.elapsed() >= Duration::from_millis(1700),
                "the files exceeded their shared rate"
            );
        }
        let relay = FtpRelayPlan::for_copy(
            [download_source, relay_target],
            &selection,
            &|_, _| false,
            &cancel,
        )
        .unwrap();
        let report = relay.apply_background_copy(batch, |_| {});
        assert!(report.errors.is_empty(), "{report:?}");
        assert_eq!(report.completed, 3);
        assert!(pool.try_claim().is_some());
        for (name, value) in [("a.bin", 0x19), ("b.bin", 0xa9)] {
            let content = vec![value; 256 * 1024];
            assert_eq!(
                fs::read(download_root.join("data").join(name)).unwrap(),
                content
            );
            for directory in ["arco-parallel-upload/data", "arco-parallel-relay/data"] {
                assert_eq!(
                    fs::read(remote.join(directory).join(name)).unwrap(),
                    content
                );
            }
        }
    }

    #[test]
    #[ignore = "requires scripts/test-sftp-reference.py"]
    fn sftp_job_transfer_options_apply_speed_masks_and_names() {
        use arco_app::network_transfer::JobOptions;
        use arco_core::profile::FtpTransfer;
        use arco_core::transfer_names::{NameCase, NameRules};
        use arco_core::transfer_presets::TransferPreset;
        use std::sync::Arc;
        use std::time::{Duration, Instant};

        let session = Arc::new(
            ftp::Connection::new(
                &std::env::var("ARCO_SFTP_TEST_URL").unwrap(),
                "arco",
                ftp::Security::Sftp,
            )
            .unwrap()
            .with_ssh(&std::env::var("ARCO_SFTP_TEST_FINGERPRINT").unwrap(), None)
            .unwrap()
            .authenticate("fixture-only")
            .unwrap(),
        );
        let cancel = AtomicBool::new(false);
        let root = session.initial_directory();
        session
            .create_sync_directory(&root, "transfer-options", &cancel)
            .unwrap();
        let directory = session.child_directory(&root, "transfer-options").unwrap();
        let fixture = tempfile::tempdir().unwrap();
        let presets = [TransferPreset {
            name: "Reports".into(),
            settings: FtpTransfer {
                speed_kib: Some(1024),
                masks: "*.txt".into(),
                names: NameRules {
                    case: NameCase::Upper,
                    ..Default::default()
                },
                ..Default::default()
            },
            rule: None,
        }];
        let options = JobOptions::new(Some("Reports".into()), "32").unwrap();
        let endpoint = options
            .endpoint(
                &presets,
                &(session.clone(), directory.clone()),
                Some(fixture.path()),
            )
            .unwrap();
        let body = vec![b'Y'; 64 * 1024];
        fs::write(fixture.path().join("report.txt"), &body).unwrap();
        fs::write(fixture.path().join("ignore.bin"), b"outside the mask").unwrap();
        let selected = [PathBuf::from("report.txt"), PathBuf::from("ignore.bin")];
        let plan = FtpPlan::for_copy(
            fixture.path(),
            endpoint,
            &selected,
            true,
            &|_, _| false,
            &cancel,
            |_| {},
        )
        .unwrap();
        let started = Instant::now();
        let report = plan.apply(&cancel, |_| {});
        assert!(
            started.elapsed() >= Duration::from_millis(1700),
            "The job-specific rate was skipped: {:?}",
            started.elapsed()
        );
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(report.completed, 1);
        let listing = session.list_sync(&directory, &cancel).unwrap();
        assert_eq!(listing.entries.len(), 1);
        assert_eq!(listing.entries[0].name, "REPORT.TXT");
        assert_eq!(listing.entries[0].size, Some(body.len() as u64));
        assert_eq!(session.transfer_settings().speed_kib, None);
    }

    #[test]
    #[ignore = "requires scripts/test-sftp-reference.py"]
    fn sftp_mapped_sync_rechecks_mirror_deletions() {
        use arco_core::transfer_names::{NameCase, NameRules};

        let cancel = AtomicBool::new(false);
        let session = ftp::Connection::new(
            &std::env::var("ARCO_SFTP_TEST_URL").unwrap(),
            "arco",
            ftp::Security::Sftp,
        )
        .unwrap()
        .with_ssh(&std::env::var("ARCO_SFTP_TEST_FINGERPRINT").unwrap(), None)
        .unwrap()
        .authenticate("fixture-only")
        .unwrap();
        let mut transfer = session.transfer_settings().clone();
        transfer.names = NameRules {
            case: NameCase::Upper,
            windows: true,
        };
        let session = Arc::new(session.with_transfer_snapshot(transfer));
        let root = session.initial_directory();
        let remote_name = "mapped-sync-reference";
        session
            .create_sync_directory(&root, remote_name, &cancel)
            .unwrap();
        let remote = session.child_directory(&root, remote_name).unwrap();
        let endpoint = (session.clone(), remote.clone());
        let fixture = tempfile::tempdir().unwrap();
        let local = fixture.path().join("local");
        fs::create_dir(&local).unwrap();
        let seed = fixture.path().join("seed");
        fs::write(&seed, b"remote file").unwrap();
        let upload = |name| {
            session
                .upload_sync(
                    &remote,
                    SyncUpload {
                        source: &seed,
                        name,
                        expected: None,
                        size: 11,
                        modified_utc: None,
                    },
                    || Ok(()),
                    &cancel,
                )
                .unwrap();
        };
        upload("Straße.txt");
        let plan = FtpPlan::compare(
            &local,
            endpoint.clone(),
            &Options {
                mode: Mode::Mirror,
                ..Options::default()
            },
            &[],
            &|_, _| false,
            &cancel,
            |_| {},
        )
        .unwrap();
        assert_eq!(plan.rows[0].action, Action::DeleteRight);
        fs::write(local.join("STRASSE.txt"), b"late local file").unwrap();
        let report = plan.apply(&cancel, |_| {});
        assert_eq!(report.completed, 0);
        assert_eq!(report.errors.len(), 1);
        assert!(report.errors[0].contains("gleichwertiger Name"));
        assert_eq!(
            session.list_sync(&remote, &cancel).unwrap().entries[0].name,
            "Straße.txt"
        );

        let local_file = local.join("GROSSE.txt");
        fs::write(&local_file, b"local only").unwrap();
        let plan = FtpPlan::compare_selection(
            &local,
            endpoint,
            &Options {
                selected_only: true,
                mode: Mode::RightToLeft,
                ..Options::default()
            },
            [
                &[PathBuf::from("GROSSE.txt")],
                &[PathBuf::from("Straße.txt")],
            ],
            &|_, _| false,
            &cancel,
            |_| {},
        )
        .unwrap();
        upload("Große.txt");
        let mut report = TransferReport::default();
        plan.apply_local_deletions(false, &mut report, &cancel);
        assert_eq!(report.completed, 0);
        assert_eq!(report.errors.len(), 1);
        assert!(report.errors[0].contains("gleichwertiger Name"));
        assert!(local_file.is_file());

        let source_directory = session.child_directory(&remote, "Relay:").unwrap();
        session
            .create_sync_directory(&remote, "Relay:", &cancel)
            .unwrap();
        session
            .upload_sync(
                &source_directory,
                SyncUpload {
                    source: &seed,
                    name: "Q?.txt",
                    expected: None,
                    size: 11,
                    modified_utc: None,
                },
                || Ok(()),
                &cancel,
            )
            .unwrap();
        let mut transfer = session.transfer_settings().clone();
        transfer.names.case = NameCase::Lower;
        let target_session = Arc::new(session.with_transfer_snapshot(transfer));
        target_session
            .create_sync_directory(&root, "mapped-relay-target", &cancel)
            .unwrap();
        let target = target_session
            .child_directory(&root, "mapped-relay-target")
            .unwrap();
        let relay = FtpRelayPlan::for_copy(
            [
                (session.clone(), remote),
                (target_session.clone(), target.clone()),
            ],
            &[PathBuf::from("Relay:")],
            &|_, _| false,
            &cancel,
        )
        .unwrap();
        assert_eq!(relay.rows.len(), 2);
        let (from, to) = relay.preview_paths(&relay.rows[1]).unwrap();
        assert_eq!(from, Path::new("Relay:/Q?.txt"));
        assert_eq!(to, Path::new("relay:/q?.txt"));
        let report = relay.apply(&cancel, |_| {});
        assert!(report.errors.is_empty(), "{report:?}");
        assert_eq!(report.completed, 2);
        let destination = target_session.child_directory(&target, "relay:").unwrap();
        let entries = target_session
            .list_sync(&destination, &cancel)
            .unwrap()
            .entries;
        assert_eq!(entries[0].name, "q?.txt");
        let file = NamedTempFile::new().unwrap();
        target_session
            .fetch_sync(&destination, &entries[0], file.as_file(), &cancel)
            .unwrap();
        assert_eq!(fs::read(file.path()).unwrap(), b"remote file");
    }

    #[test]
    #[ignore = "requires scripts/test-native-http-reference.py"]
    fn webdav_and_s3_search_sync_and_relay() {
        let cancel = AtomicBool::new(false);
        let dav = Arc::new(
            ftp::Connection::new(
                &std::env::var("ARCO_TEST_WEBDAV").unwrap(),
                "arco",
                ftp::Security::WebDav,
            )
            .unwrap()
            .authenticate("fixture-only")
            .unwrap(),
        );
        let s3 = Arc::new(
            ftp::Connection::new(
                &std::env::var("ARCO_TEST_S3").unwrap(),
                "ARCOACCESS",
                ftp::Security::S3,
            )
            .unwrap()
            .with_s3_region("eu-central-1")
            .unwrap()
            .authenticate("fixture-only")
            .unwrap(),
        );
        let dav_root = dav.initial_directory();
        let bucket = "arco-panel-copy";
        let s3_root = s3.initial_directory();
        s3.create_sync_directory(&s3_root, bucket, &cancel).unwrap();
        let target = s3.relative_directory(&s3_root, Path::new(bucket)).unwrap();
        let local = tempfile::tempdir().unwrap();
        let source = local.path().join("example.data");
        let output = local.path().join("verified");
        let name = "Büro & Team.txt";
        let data = b"relayed through the file panels";
        fs::write(&source, data).unwrap();
        dav.upload(&dav_root, &source, name, false, &cancel)
            .unwrap();
        let plan = FtpRelayPlan::for_copy(
            [
                (dav.clone(), dav_root.clone()),
                (s3.clone(), target.clone()),
            ],
            &[PathBuf::from(name)],
            &|_, _| false,
            &cancel,
        )
        .unwrap();
        let report = plan.apply(&cancel, |_| {});
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(report.completed, 1);
        s3.download(&target, name, &output, &cancel).unwrap();
        assert_eq!(fs::read(output).unwrap(), data);
        compare_transfer_round_trip(&dav, &dav_root);
        compare_transfer_round_trip(&s3, &target);
    }

    fn compare_transfer_round_trip(session: &Arc<ftp::Session>, root: &ftp::Url) {
        let cancel = AtomicBool::new(false);
        let fixture = tempfile::tempdir().unwrap();
        let local = fixture.path().join("local");
        fs::create_dir_all(local.join("outgoing")).unwrap();
        fs::write(local.join("outgoing").join("ä # export.txt"), b"from local").unwrap();
        fs::write(local.join("both.txt"), b"AAAA").unwrap();
        fs::write(local.join("skip.bin"), b"excluded").unwrap();

        session
            .create_sync_directory(root, "round trip", &cancel)
            .unwrap();
        let remote = session
            .relative_directory(root, Path::new("round trip"))
            .unwrap();
        session
            .create_sync_directory(&remote, "incoming", &cancel)
            .unwrap();
        let incoming = session
            .relative_directory(&remote, Path::new("incoming"))
            .unwrap();
        let seed = fixture.path().join("seed.txt");
        fs::write(&seed, b"from server").unwrap();
        session
            .upload(&incoming, &seed, "probe.txt", false, &cancel)
            .unwrap();
        fs::write(&seed, b"BBBB").unwrap();
        session
            .upload(&remote, &seed, "both.txt", false, &cancel)
            .unwrap();

        let hits = super::super::ftp_panel::search_remote(
            &crate::panel::FtpLocation {
                session: session.clone(),
                directory: remote.clone(),
                profile: None,
            },
            |name| name == "probe.txt",
            None,
            &cancel,
            |_| {},
        )
        .unwrap();
        assert_eq!(hits.len(), 1);

        let options = Options {
            by_content: true,
            ignore_date: true,
            masks: "*.txt".into(),
            ..Options::default()
        };
        let mut plan = FtpPlan::compare(
            &local,
            (session.clone(), remote.clone()),
            &options,
            &[],
            &|_, _| false,
            &cancel,
            |_| {},
        )
        .unwrap();
        let conflict = plan
            .rows
            .iter_mut()
            .find(|row| row.relative == Path::new("both.txt"))
            .unwrap();
        assert_eq!(conflict.action, Action::Skip);
        conflict.toggle(Action::CopyRight);
        let report = plan.apply(&cancel, |_| {});
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(report.completed, 5);
        assert_eq!(
            fs::read(local.join("incoming").join("probe.txt")).unwrap(),
            b"from server"
        );
        let downloaded = fixture.path().join("verified.txt");
        session
            .download(&remote, "both.txt", &downloaded, &cancel)
            .unwrap();
        assert_eq!(fs::read(downloaded).unwrap(), b"AAAA");
        let outgoing = session
            .relative_directory(&remote, Path::new("outgoing"))
            .unwrap();
        assert!(
            session
                .list_sync(&outgoing, &cancel)
                .unwrap()
                .entries
                .iter()
                .any(|entry| entry.name == "ä # export.txt")
        );
        let verification = FtpPlan::compare(
            &local,
            (session.clone(), remote.clone()),
            &options,
            &[],
            &|_, _| false,
            &cancel,
            |_| {},
        )
        .unwrap();
        assert!(
            verification
                .rows
                .iter()
                .all(|row| row.comparison == sync::Comparison::Equal)
        );
        for (action, expected) in [
            (Action::CopyRight, b"AAAAlocal prefix".as_slice()),
            (Action::CopyLeft, b"local prefixAAAAlocal prefix".as_slice()),
        ] {
            fs::write(local.join("both.txt"), b"local prefix").unwrap();
            let mut plan = FtpPlan::compare(
                &local,
                (session.clone(), remote.clone()),
                &options,
                &[],
                &|_, _| false,
                &cancel,
                |_| {},
            )
            .unwrap();
            for row in &mut plan.rows {
                row.action = if row.relative == Path::new("both.txt") {
                    action
                } else {
                    Action::Skip
                };
            }
            plan.choices.append.insert(PathBuf::from("both.txt"));
            let report = plan.apply(&cancel, |_| {});
            assert!(report.errors.is_empty(), "{:?}", report.errors);
            assert_eq!(report.completed, 1);
            if action == Action::CopyRight {
                let saved = fixture.path().join("appended.txt");
                session
                    .download(&remote, "both.txt", &saved, &cancel)
                    .unwrap();
                assert_eq!(fs::read(saved).unwrap(), expected);
            } else {
                assert_eq!(fs::read(local.join("both.txt")).unwrap(), expected);
            }
        }
    }
}
