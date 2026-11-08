use super::FtpEndpoint;
use arco_core::files::{self, TransferReport};
use arco_core::sync::{self, Action, EntryInfo, LocalRoot, Mode, Options, Row, Scope, Tree};
use arco_core::tools::{self, Hash};
use arco_windows::ftp::{self, SyncUpload};
use chrono::{DateTime, NaiveDateTime, Utc};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::SystemTime;
use tempfile::NamedTempFile;

const MAX_DEPTH: usize = 64;
const MAX_ENTRIES: usize = 200_000;

pub(super) struct FtpPlan {
    pub rows: Vec<Row>,
    local: LocalRoot,
    endpoint: FtpEndpoint,
    expected: BTreeMap<PathBuf, ftp::Entry>,
}

pub(super) struct FtpRelayPlan {
    pub rows: Vec<Row>,
    endpoints: [FtpEndpoint; 2],
    entries: [BTreeMap<PathBuf, ftp::Entry>; 2],
}

impl FtpRelayPlan {
    pub(super) fn for_copy(
        endpoints: [FtpEndpoint; 2],
        selected: &[PathBuf],
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
        let [(left, source), (right, target)] = [
            scan(&endpoints[0], &scope, true, cancel, &mut |_| {})?,
            scan(&endpoints[1], &scope, true, cancel, &mut |_| {})?,
        ];
        let mut rows =
            sync::compare_trees([&left, &right], &options, &scope, cancel, |_, _| Ok(false))?;
        skip_existing_copies(&mut rows, 0);
        Ok(Self {
            rows,
            endpoints,
            entries: [source, target],
        })
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
        let target_parent = remote_parent(target, row.path(1))?;
        let entry = &self.entries[0][row.path(0)];
        let name = row
            .path(1)
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
        let stamp = modified(entry)?.map(ftp_timestamp);
        target
            .0
            .upload_sync(
                &target_parent,
                SyncUpload {
                    source: temporary.path(),
                    name,
                    expected: self.entries[1].get(row.path(1)),
                    size: entry.size.expect("strict FTP file size"),
                    modified_utc: stamp.as_deref(),
                },
                || source.0.verify_sync_entry(&source_parent, entry, cancel),
                cancel,
            )
            .map(|_| ())
    }
}

fn skip_existing_copies(rows: &mut Vec<Row>, source: usize) {
    rows.retain(|row| row.info(source).is_some());
    for row in rows {
        if row.info(1 - source).is_some() {
            row.action = Action::Skip;
        }
    }
}

impl FtpPlan {
    pub(super) fn compare(
        local: &Path,
        endpoint: FtpEndpoint,
        options: &Options,
        selected: &[PathBuf],
        cancel: &AtomicBool,
        progress: impl FnMut(&Path),
    ) -> io::Result<Self> {
        Self::prepare(local, endpoint, options, selected, false, cancel, progress)
    }

    pub(super) fn for_copy(
        local: &Path,
        endpoint: FtpEndpoint,
        selected: &[PathBuf],
        upload: bool,
        cancel: &AtomicBool,
        progress: impl FnMut(&Path),
    ) -> io::Result<Self> {
        let options = Options {
            mode: if upload {
                Mode::LeftToRight
            } else {
                Mode::RightToLeft
            },
            selected_only: true,
            by_content: true,
            ignore_date: true,
            ..Options::default()
        };
        let mut plan = Self::prepare(local, endpoint, &options, selected, true, cancel, progress)?;
        let source = usize::from(!upload);
        skip_existing_copies(&mut plan.rows, source);
        Ok(plan)
    }

    fn prepare(
        local: &Path,
        endpoint: FtpEndpoint,
        options: &Options,
        selected: &[PathBuf],
        copy_only: bool,
        cancel: &AtomicBool,
        mut progress: impl FnMut(&Path),
    ) -> io::Result<Self> {
        let local = LocalRoot::open(local)?;
        let scope = Scope::new(options.selected_only, selected)?;
        let local_entries = local.scan(options, &scope, cancel)?;
        let (remote_entries, expected) =
            scan(&endpoint, &scope, options.recursive, cancel, &mut progress)?;
        let rows = sync::compare_trees(
            [&local_entries, &remote_entries],
            options,
            &scope,
            cancel,
            |left, right| {
                if copy_only {
                    return Ok(false);
                }
                progress(right.path());
                let file = local.checked_path(left.path())?;
                let meta = left.metadata().expect("local entry");
                files::ensure_unchanged(&file, meta)?;
                let remote = &expected[right.path()];
                let temporary = NamedTempFile::new()?;
                let parent = remote_parent(&endpoint, right.path())?;
                endpoint
                    .0
                    .fetch_sync(&parent, remote, temporary.as_file(), cancel)?;
                let equal = tools::checksum(&file, Hash::Blake3, cancel)?
                    == tools::checksum(temporary.path(), Hash::Blake3, cancel)?;
                files::ensure_unchanged(&file, meta)?;
                Ok(equal)
            },
        )?;
        Ok(Self {
            rows,
            local,
            endpoint,
            expected,
        })
    }

    pub(super) fn apply(
        &self,
        cancel: &AtomicBool,
        mut progress: impl FnMut(&Path),
    ) -> TransferReport {
        let mut report = TransferReport::default();
        let copies = self.rows.iter().filter(|row| {
            matches!(row.action, Action::CopyLeft | Action::CopyRight) && row.can(row.action)
        });
        run_rows(copies, &mut report, cancel, &mut progress, |row| {
            self.copy(row, cancel)
        });
        let deletes = self
            .rows
            .iter()
            .rev()
            .filter(|row| row.action == Action::DeleteRight && row.can(row.action));
        if report.errors.is_empty() && !report.cancelled {
            run_rows(deletes, &mut report, cancel, &mut progress, |row| {
                self.delete(row, cancel)
            });
        } else {
            report.skipped += deletes.count();
        }
        report
    }

    fn delete(&self, row: &Row, cancel: &AtomicBool) -> io::Result<()> {
        self.local.verify_absent(row.path(0))?;
        let parent = remote_parent(&self.endpoint, row.path(1))?;
        let entry = &self.expected[row.path(1)];
        self.endpoint.0.delete_sync_entry(&parent, entry, cancel)
    }

    fn copy(&self, row: &Row, cancel: &AtomicBool) -> io::Result<()> {
        let local = self.local.checked_path(row.path(0))?;
        let remote = row.path(1);
        let parent = remote_parent(&self.endpoint, remote)?;
        let session = &self.endpoint.0;
        let expected = self.expected.get(remote);
        if row.action == Action::CopyRight {
            let before = row.metadata(0).expect("local copy source");
            files::ensure_unchanged(&local, before)?;
            let name = remote
                .file_name()
                .and_then(OsStr::to_str)
                .ok_or_else(|| io::Error::other("FTP-Dateinamen benötigen Unicode."))?;
            if before.is_dir() {
                return session.create_sync_directory(&parent, name, cancel);
            }
            let modified = before.modified().ok().map(ftp_timestamp);
            return session
                .upload_sync(
                    &parent,
                    SyncUpload {
                        source: &local,
                        name,
                        expected,
                        size: before.len(),
                        modified_utc: modified.as_deref(),
                    },
                    || {
                        self.local.checked_path(row.path(0))?;
                        files::ensure_unchanged(&local, before)
                    },
                    cancel,
                )
                .map(|_| ());
        }

        let expected = expected.expect("remote copy source");
        if expected.directory == Some(true) {
            session.verify_sync_entry(&parent, expected, cancel)?;
            self.local.checked_path(row.path(0))?;
            return fs::create_dir(local);
        }
        let previous = row.metadata(0);
        if let Some(previous) = previous {
            files::ensure_unchanged(&local, previous)?;
        }
        let temporary = NamedTempFile::new_in(local.parent().expect("local copy parent"))?;
        session.fetch_sync(&parent, expected, temporary.as_file(), cancel)?;
        if let Some(time) = modified(expected)? {
            temporary.as_file().set_modified(time)?;
        }
        self.local.checked_path(row.path(0))?;
        files::publish_checked(temporary, &local, previous, cancel)?;
        Ok(())
    }
}

fn ftp_timestamp(time: SystemTime) -> String {
    DateTime::<Utc>::from(time)
        .format("%Y%m%d%H%M%S")
        .to_string()
}

fn run_rows<'a>(
    rows: impl Iterator<Item = &'a Row>,
    report: &mut TransferReport,
    cancel: &AtomicBool,
    progress: &mut impl FnMut(&Path),
    mut apply: impl FnMut(&Row) -> io::Result<()>,
) {
    for row in rows {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        progress(&row.relative);
        match apply(row) {
            Ok(()) => report.completed += 1,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                report.cancelled = true;
                break;
            }
            Err(error) => report
                .errors
                .push(format!("{}: {error}", row.relative.display())),
        }
    }
    report.cancelled |= cancel.load(Ordering::Relaxed);
}

fn remote_parent(endpoint: &FtpEndpoint, path: &Path) -> io::Result<ftp::Url> {
    endpoint
        .0
        .relative_directory(&endpoint.1, path.parent().unwrap_or(Path::new("")))
}

pub(super) fn scan(
    endpoint: &FtpEndpoint,
    scope: &Scope,
    recursive: bool,
    cancel: &AtomicBool,
    progress: &mut impl FnMut(&Path),
) -> io::Result<(Tree, BTreeMap<PathBuf, ftp::Entry>)> {
    let mut tree = Tree::default();
    let mut expected = BTreeMap::new();
    let mut unique = BTreeSet::new();
    let mut queue = vec![(PathBuf::new(), 0)];
    while let Some((relative, depth)) = queue.pop() {
        if depth >= MAX_DEPTH {
            return Err(io::Error::other(
                "Der FTP-Ordnerbaum ist zu tief für einen sicheren Vergleich.",
            ));
        }
        progress(&relative);
        let url = endpoint.0.relative_directory(&endpoint.1, &relative)?;
        for entry in endpoint.0.list_sync(&url, cancel)?.entries {
            if cancel.load(Ordering::Relaxed) {
                return Err(io::ErrorKind::Interrupted.into());
            }
            let name = files::leaf_name(OsStr::new(&entry.name))?;
            let path = relative.join(name);
            if !scope.visits(&path) {
                continue;
            }
            if expected.len() >= MAX_ENTRIES {
                return Err(io::Error::other(
                    "Der FTP-Ordnerbaum enthält zu viele Einträge.",
                ));
            }
            let directory = entry.directory.expect("strict FTP listing");
            let info = EntryInfo {
                size: (!directory).then(|| entry.size.expect("strict FTP file size")),
                modified: modified(&entry)?,
            };
            tree.insert_remote(path.clone(), info)?;
            if directory && recursive {
                if let Some(id) = &entry.unique
                    && !unique.insert(id.clone())
                {
                    return Err(io::Error::other(format!(
                        "FTP-Ordner mehrfach erreicht: {}",
                        path.display()
                    )));
                }
                queue.push((path.clone(), depth + 1));
            }
            expected.insert(path, entry);
        }
    }
    Ok((tree, expected))
}

pub(crate) fn modified(entry: &ftp::Entry) -> io::Result<Option<SystemTime>> {
    entry
        .modified
        .as_deref()
        .map(|time| {
            NaiveDateTime::parse_from_str(time, "%Y%m%d%H%M%S%.f")
                .map(|time| SystemTime::from(time.and_utc()))
                .map_err(|_| {
                    io::Error::other(format!(
                        "Ungültiges MLSD-Änderungsdatum für „{}“.",
                        entry.name
                    ))
                })
        })
        .transpose()
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
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
        let first =
            FtpPlan::compare(&local, endpoint.clone(), &options, &[], &cancel, |_| {}).unwrap();
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
        let second =
            FtpPlan::compare(&local, endpoint.clone(), &options, &[], &cancel, |_| {}).unwrap();
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
        let plan = FtpRelayPlan::for_copy(endpoints.clone(), &selected, &cancel).unwrap();
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
        let mut plan = FtpRelayPlan::for_copy(endpoints, &selected, &cancel).unwrap();
        plan.rows
            .iter_mut()
            .find(|row| row.relative == Path::new("exists.txt"))
            .unwrap()
            .toggle(Action::CopyRight);
        let report = plan.apply(&cancel, |_| {});
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(report.completed, 1);
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
        let cancel = AtomicBool::new(false);
        let fixture = tempfile::tempdir().unwrap();
        let local = fixture.path().join("local");
        fs::create_dir_all(local.join("outgoing")).unwrap();
        fs::write(local.join("outgoing").join("ä # export.txt"), b"from local").unwrap();
        fs::write(local.join("both.txt"), b"AAAA").unwrap();
        fs::write(local.join("skip.bin"), b"excluded").unwrap();

        let root = session.initial_directory();
        session
            .create_sync_directory(&root, "round trip", &cancel)
            .unwrap();
        let remote = session
            .relative_directory(&root, Path::new("round trip"))
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
        let verification =
            FtpPlan::compare(&local, (session, remote), &options, &[], &cancel, |_| {}).unwrap();
        assert!(
            verification
                .rows
                .iter()
                .all(|row| row.comparison == sync::Comparison::Equal)
        );
    }
}
