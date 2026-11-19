use super::{
    FtpEndpoint, JobContext,
    transfer_queue::{
        ConflictSide, VIRTUAL_CONFLICT_PREVIEW_LIMIT, ask_conflict, conflict_preview,
    },
};
pub(crate) use arco_app::network_sync::modified;
use arco_app::network_sync::{
    CopyChoices, IgnorePath, append_to_ftp_snapshot, ftp_timestamp, remote_parent, run_rows,
    skip_existing_copies,
};
pub(super) use arco_app::network_sync::{FtpPlan, MAX_DEPTH, MAX_ENTRIES, ftp_ignores, scan};
use arco_core::files::{self, Conflict, ConflictDecision, TransferReport};
use arco_core::sync::{self, Action, EntryInfo, LocalRoot, Mode, Options, Row, Scope};
use arco_windows::ftp::{self, SyncUpload};
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
}

impl FtpRelayPlan {
    pub(super) fn for_copy(
        endpoints: [FtpEndpoint; 2],
        selected: &[PathBuf],
        ignored: &IgnorePath<'_>,
        cancel: &AtomicBool,
    ) -> io::Result<Self> {
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
        let scope = Scope::new(true, selected)?;
        let [source_masks, target_masks] = [
            sync::FileMasks::new(endpoints[0].0.transfer_masks())?,
            sync::FileMasks::new(endpoints[1].0.transfer_masks())?,
        ];
        let excluded = RefCell::new(BTreeSet::new());
        let skip = |path: &Path, directory| {
            let skip = ignored(path, directory)
                || (!directory && (!source_masks.includes(path) || !target_masks.includes(path)));
            if skip {
                excluded.borrow_mut().insert(path.to_path_buf());
            }
            skip
        };
        let [(mut left, source), (mut right, target)] = [
            scan(&endpoints[0], &scope, true, &skip, cancel, &mut |_| {})?,
            scan(&endpoints[1], &scope, true, &skip, cancel, &mut |_| {})?,
        ];
        let excluded = excluded.into_inner();
        left.discard_subtrees(&excluded);
        right.discard_subtrees(&excluded);
        let mut rows =
            sync::compare_trees([&left, &right], &options, &scope, cancel, |_, _| Ok(false))?;
        skip_existing_copies(&mut rows, 0);
        Ok(Self {
            rows,
            endpoints,
            entries: [source, target],
            choices: CopyChoices::default(),
        })
    }

    pub(super) fn resolve_conflicts(&mut self, job: &JobContext) -> io::Result<usize> {
        let sides = [
            CopySide::Remote(&self.endpoints[0], &self.entries[0]),
            CopySide::Remote(&self.endpoints[1], &self.entries[1]),
        ];
        resolve_copy_conflicts(&mut self.rows, sides, &mut self.choices, job)
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
            |row| self.copy(row, cancel),
        );
        report
    }

    fn copy(&self, row: &Row, cancel: &AtomicBool) -> io::Result<()> {
        let [source, target] = &self.endpoints;
        let source_parent = remote_parent(source, row.path(0))?;
        let alternate = self.choices.alternate.get(&row.relative);
        let destination = alternate.map_or_else(|| row.path(1), PathBuf::as_path);
        let target_parent = remote_parent(target, destination)?;
        let entry = &self.entries[0][row.path(0)];
        let name = destination
            .file_name()
            .and_then(OsStr::to_str)
            .ok_or_else(|| io::Error::other("FTP-Dateinamen benötigen Unicode."))?;
        if entry.directory == Some(true) {
            source.0.verify_sync_entry(&source_parent, entry, cancel)?;
            return target.0.create_sync_directory(&target_parent, name, cancel);
        }
        let temporary = NamedTempFile::new()?;
        source
            .0
            .fetch_sync(&source_parent, entry, temporary.as_file(), cancel)?;
        let temporary = temporary.into_temp_path();
        let metadata = files::regular_metadata(&temporary)?;
        let previous = if alternate.is_some() {
            None
        } else {
            self.entries[1].get(row.path(1))
        };
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
        let stamp = modified(entry)?
            .filter(|_| target.0.preserve_transfer_time(true))
            .map(ftp_timestamp);
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
enum CopySide<'a> {
    Local(&'a LocalRoot),
    Remote(&'a FtpEndpoint, &'a BTreeMap<PathBuf, ftp::Entry>),
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
            Self::Local(root) => {
                let metadata = row.metadata(index).expect("local FTP copy side");
                Ok(ConflictSide::local(
                    &root.checked_path(path)?,
                    metadata,
                    None,
                ))
            }
            Self::Remote(endpoint, entries) => {
                let mut url = endpoint.1.clone();
                let _ = url.set_username("");
                let _ = url.set_password(None);
                url.set_query(None);
                url.set_fragment(None);
                let mut side = ConflictSide::virtual_entry(
                    format!("{}/{}", url.as_str().trim_end_matches('/'), path.display()),
                    info.size,
                    info.modified,
                );
                if info
                    .size
                    .is_some_and(|size| size <= VIRTUAL_CONFLICT_PREVIEW_LIMIT)
                {
                    let load = || {
                        let expected = entries
                            .get(path)
                            .ok_or_else(|| io::Error::other("Der FTP-Dateieintrag fehlt."))?;
                        let parent = remote_parent(endpoint, path)?;
                        let file = NamedTempFile::new()?;
                        endpoint
                            .0
                            .fetch_sync(&parent, expected, file.as_file(), &job.cancel)?;
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
            Self::Local(root) => {
                let path = root.checked_path(path)?;
                fs::read_dir(path.parent().expect("checked path"))?
                    .map(|entry| {
                        entry.map(|entry| entry.file_name().to_string_lossy().to_lowercase())
                    })
                    .collect()
            }
            Self::Remote(endpoint, _) => {
                let parent = remote_parent(endpoint, path)?;
                Ok(endpoint
                    .0
                    .list_sync(&parent, cancel)?
                    .entries
                    .into_iter()
                    .map(|entry| entry.name.to_lowercase())
                    .collect())
            }
        }
    }
}

fn resolve_copy_conflicts(
    rows: &mut [Row],
    sides: [CopySide<'_>; 2],
    choices: &mut CopyChoices,
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
            let (decision, all) = ask_conflict(
                job,
                sides[source].conflict_side(row, source, from, job)?,
                sides[target].conflict_side(row, target, to, job)?,
                vec![
                    (Conflict::Skip, "Überspringen"),
                    (Conflict::Replace, "Überschreiben"),
                    (Conflict::Newer, "Nur mit neuerer Datei überschreiben"),
                    (Conflict::Larger, "Nur mit größerer Datei überschreiben"),
                    (Conflict::Rename, "Unter freiem Namen speichern"),
                    (Conflict::Append, "An Zieldatei anhängen"),
                ],
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
        let sides = [
            CopySide::Local(&self.local),
            CopySide::Remote(&self.endpoint, &self.expected),
        ];
        resolve_copy_conflicts(&mut self.rows, sides, &mut self.choices, job)
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use arco_core::ignore::IgnoreList;
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
