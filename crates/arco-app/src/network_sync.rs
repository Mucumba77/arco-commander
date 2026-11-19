use arco_core::files::{self, TransferReport};
use arco_core::ignore::{IgnoreList, Source};
use arco_core::sync::{
    self, Action, Comparison, EntryInfo, LocalRoot, Mode, Options, Row, Scope, Tree,
};
use arco_core::tools::{self, Hash};
use arco_windows::ftp::{self, SyncUpload};
use chrono::{DateTime, NaiveDateTime, Utc};
use std::cell::RefCell;
use std::cmp::Ordering as TimeOrder;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs;
use std::io::{self, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::SystemTime;
use tempfile::NamedTempFile;

pub type FtpEndpoint = (Arc<ftp::Session>, ftp::Url);

pub const MAX_DEPTH: usize = 64;
pub const MAX_ENTRIES: usize = 200_000;
pub type IgnorePath<'a> = dyn Fn(&Path, bool) -> bool + 'a;

pub fn check_transfer_mask(session: &ftp::Session, name: &str) -> io::Result<()> {
    if sync::FileMasks::new(session.transfer_masks())?.includes(Path::new(name)) {
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("Die Datei „{name}“ ist durch die Transfermaske des Profils ausgeschlossen."),
    ))
}

pub fn ftp_ignores<'a>(
    ignore: Option<&'a IgnoreList>,
    url: &'a ftp::Url,
    profile: Option<&'a str>,
) -> impl Fn(&Path, bool) -> bool + 'a {
    let root = ftp::Session::directory_path(url);
    move |relative, directory| {
        ignore.is_some_and(|ignore| {
            let path = format!(
                "{}/{}",
                root.trim_end_matches('/'),
                relative.to_string_lossy().replace('\\', "/")
            );
            ignore.matches(
                Source::Ftp {
                    server: url.host_str().unwrap_or_default(),
                    profile,
                },
                &path,
                directory,
            )
        })
    }
}

#[derive(Clone, Copy)]
pub struct ScriptComparison {
    pub time: bool,
    pub size: bool,
    pub checksum: bool,
    pub mirror: bool,
    pub preserve_time: Option<bool>,
}

impl ScriptComparison {
    fn compare(
        self,
        left: EntryInfo,
        right: EntryInfo,
        mode: Mode,
        same_content: impl FnOnce() -> io::Result<bool>,
    ) -> io::Result<(Comparison, Action)> {
        let ordering = if self.time {
            let (left, right) = left.modified.zip(right.modified).ok_or_else(|| {
                io::Error::other("Der Zeitvergleich benötigt Änderungszeiten auf beiden Seiten.")
            })?;
            Some(sync::compare_time(left, right))
        } else {
            None
        };
        let sizes_equal = left.size == right.size;
        let equal = ordering.is_none_or(TimeOrder::is_eq)
            && (!(self.size || self.checksum) || sizes_equal)
            && (self.time || self.size || self.checksum)
            && (!self.checksum || same_content()?);
        let action = match mode {
            _ if equal => return Ok((Comparison::Equal, Action::Skip)),
            Mode::BothWays => match ordering {
                Some(TimeOrder::Greater) => Action::CopyRight,
                Some(TimeOrder::Less) => Action::CopyLeft,
                _ => Action::Skip,
            },
            Mode::RightToLeft if self.mirror || ordering != Some(TimeOrder::Greater) => {
                Action::CopyLeft
            }
            Mode::LeftToRight | Mode::Mirror
                if self.mirror || ordering != Some(TimeOrder::Less) =>
            {
                Action::CopyRight
            }
            _ => Action::Skip,
        };
        Ok((Comparison::Different, action))
    }
}

pub struct FtpPlan {
    pub rows: Vec<Row>,
    pub local: LocalRoot,
    pub endpoint: FtpEndpoint,
    pub expected: BTreeMap<PathBuf, ftp::Entry>,
    pub choices: CopyChoices,
    protected: BTreeSet<String>,
    preserve_time: bool,
    timestamps_only: bool,
}

#[derive(Default)]
pub struct CopyChoices {
    pub alternate: BTreeMap<PathBuf, PathBuf>,
    pub append: BTreeSet<PathBuf>,
}

pub fn append_to_ftp_snapshot(
    session: &ftp::Session,
    directory: &ftp::Url,
    previous: Option<&ftp::Entry>,
    source: &Path,
    metadata: &fs::Metadata,
    cancel: &AtomicBool,
) -> io::Result<NamedTempFile> {
    let previous =
        previous.ok_or_else(|| io::Error::other("Die FTP-Zieldatei zum Anhängen fehlt."))?;
    let mut combined = NamedTempFile::new()?;
    session.fetch_sync(directory, previous, combined.as_file(), cancel)?;
    combined.seek(SeekFrom::End(0))?;
    files::copy_snapshot_to(source, metadata, &mut combined, cancel)?;
    Ok(combined)
}

pub fn skip_existing_copies(rows: &mut Vec<Row>, source: usize) {
    rows.retain(|row| row.info(source).is_some());
    for row in rows {
        if row.info(1 - source).is_some() {
            row.action = Action::Skip;
        }
    }
}

impl FtpPlan {
    pub fn timestamps_only(&self) -> bool {
        self.timestamps_only
    }

    pub fn compare(
        local: &Path,
        endpoint: FtpEndpoint,
        options: &Options,
        selected: &[PathBuf],
        ignored: &IgnorePath<'_>,
        cancel: &AtomicBool,
        progress: impl FnMut(&Path),
    ) -> io::Result<Self> {
        Self::prepare(
            local, endpoint, options, selected, false, None, ignored, cancel, progress,
        )
    }

    pub fn for_copy(
        local: &Path,
        endpoint: FtpEndpoint,
        selected: &[PathBuf],
        upload: bool,
        ignored: &IgnorePath<'_>,
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
        let mut plan = Self::prepare(
            local, endpoint, &options, selected, true, None, ignored, cancel, progress,
        )?;
        let source = usize::from(!upload);
        skip_existing_copies(&mut plan.rows, source);
        Ok(plan)
    }

    pub fn for_script(
        local: &Path,
        endpoint: FtpEndpoint,
        options: &Options,
        comparison: ScriptComparison,
        ignored: &IgnorePath<'_>,
        cancel: &AtomicBool,
    ) -> io::Result<Self> {
        Self::prepare(
            local,
            endpoint,
            options,
            &[],
            false,
            Some(comparison),
            ignored,
            cancel,
            |_| {},
        )
    }

    fn prepare(
        local: &Path,
        endpoint: FtpEndpoint,
        options: &Options,
        selected: &[PathBuf],
        copy_only: bool,
        script: Option<ScriptComparison>,
        ignored: &IgnorePath<'_>,
        cancel: &AtomicBool,
        mut progress: impl FnMut(&Path),
    ) -> io::Result<Self> {
        let local = LocalRoot::open(local)?;
        if options.timestamps_only
            && options.mode != Mode::RightToLeft
            && !(endpoint.0.security().is_ssh() || endpoint.0.security().is_ftp())
        {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "WebDAV und S3 können hier nur als Quelle für den Zeitstempelabgleich dienen.",
            ));
        }
        let scope = Scope::new(options.selected_only, selected)?;
        let masks = sync::FileMasks::new(endpoint.0.transfer_masks())?;
        let excluded = RefCell::new(BTreeSet::new());
        let skip = |path: &Path, directory| {
            let skip = endpoint.0.recycle_relative_path(&endpoint.1, path)
                || ignored(path, directory)
                || (!directory && !masks.includes(path));
            if skip {
                excluded.borrow_mut().insert(path.to_path_buf());
            }
            skip
        };
        let mut local_entries = local.scan_ignoring(options, &scope, skip, cancel)?;
        let (mut remote_entries, expected) = scan(
            &endpoint,
            &scope,
            options.recursive,
            &skip,
            cancel,
            &mut progress,
        )?;
        let excluded = excluded.into_inner();
        local_entries.discard_subtrees(&excluded);
        remote_entries.discard_subtrees(&excluded);
        let mut checksum_method = None;
        let mut rows = sync::compare_trees_by(
            [&local_entries, &remote_entries],
            options,
            &scope,
            cancel,
            |left, right| {
                let mut same_content = |left: &sync::Entry, right: &sync::Entry| {
                    if copy_only {
                        return Ok(false);
                    }
                    progress(right.path());
                    let file = local.checked_path(left.path())?;
                    let meta = left.metadata().expect("local entry");
                    files::ensure_unchanged(&file, meta)?;
                    let remote = &expected[right.path()];
                    let parent = remote_parent(&endpoint, right.path())?;
                    let method = match checksum_method {
                        Some(method) => method,
                        None => {
                            let method = endpoint.0.sync_checksum_method(&endpoint.1, cancel)?;
                            checksum_method = Some(method);
                            method
                        }
                    };
                    if let Some(method) = method
                        && let Some((algorithm, digest)) =
                            endpoint.0.sync_checksum(&parent, remote, method, cancel)?
                    {
                        let algorithm = Hash::parse(algorithm).expect("supported FTP checksum");
                        let equal = tools::checksum(&file, algorithm, cancel)? == digest;
                        files::ensure_unchanged(&file, meta)?;
                        return Ok(equal);
                    }
                    let temporary = NamedTempFile::new()?;
                    endpoint
                        .0
                        .fetch_sync(&parent, remote, temporary.as_file(), cancel)?;
                    let equal = tools::checksum(&file, Hash::Blake3, cancel)?
                        == tools::checksum(temporary.path(), Hash::Blake3, cancel)?;
                    files::ensure_unchanged(&file, meta)?;
                    Ok(equal)
                };
                if let Some(script) = script {
                    script.compare(left.info(), right.info(), options.mode, || {
                        same_content(left, right)
                    })
                } else {
                    sync::compare_file(left, right, options, &mut same_content)
                }
            },
        )?;
        let protected: BTreeSet<_> = excluded
            .iter()
            .flat_map(|path| path.ancestors().skip(1))
            .map(|path| path.to_string_lossy().to_lowercase())
            .collect();
        for row in &mut rows {
            if protected.contains(&row.relative.to_string_lossy().to_lowercase()) {
                row.prevent_delete();
            }
        }
        let preserve_time = script
            .and_then(|script| script.preserve_time)
            .unwrap_or_else(|| endpoint.0.preserve_transfer_time(true));
        Ok(Self {
            rows,
            local,
            endpoint,
            expected,
            choices: CopyChoices::default(),
            protected,
            preserve_time,
            timestamps_only: options.timestamps_only,
        })
    }

    pub fn apply(&self, cancel: &AtomicBool, progress: impl FnMut(&Path)) -> TransferReport {
        self.apply_with_uploads(cancel, progress, |_, _| Ok(()))
    }

    pub fn apply_with_uploads(
        &self,
        cancel: &AtomicBool,
        mut progress: impl FnMut(&Path),
        mut uploaded: impl FnMut(&Row, ftp::Entry) -> io::Result<()>,
    ) -> TransferReport {
        let mut report = TransferReport::default();
        let copies = self.rows.iter().filter(|row| {
            matches!(row.action, Action::CopyLeft | Action::CopyRight) && row.can(row.action)
        });
        run_rows(copies, &mut report, cancel, &mut progress, |row| {
            if self.timestamps_only {
                self.apply_timestamp(row, cancel)
            } else {
                self.copy(row, cancel, &mut uploaded)
            }
        });
        if self.timestamps_only {
            return report;
        }
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

    pub fn local_obsolete<'a>(&'a self, has_masks: bool) -> impl Iterator<Item = &'a Row> {
        self.rows.iter().rev().filter(move |row| {
            row.comparison == Comparison::OnlyLeft
                && !self
                    .protected
                    .contains(&row.relative.to_string_lossy().to_lowercase())
                && (!has_masks || row.info(0).is_some_and(|info| !info.is_dir()))
        })
    }

    pub fn apply_local_deletions(
        &self,
        has_masks: bool,
        report: &mut TransferReport,
        cancel: &AtomicBool,
    ) {
        let obsolete = self.local_obsolete(has_masks);
        if report.cancelled || !report.errors.is_empty() {
            report.skipped += obsolete.count();
            return;
        }
        run_rows(obsolete, report, cancel, &mut |_| {}, |row| {
            self.endpoint
                .0
                .verify_sync_absent(&self.endpoint.1, row.path(1), cancel)?;
            self.local
                .delete_checked(row.path(0), row.metadata(0).expect("local delete target"))
        });
    }

    fn delete(&self, row: &Row, cancel: &AtomicBool) -> io::Result<()> {
        self.local.verify_absent(row.path(0))?;
        let parent = remote_parent(&self.endpoint, row.path(1))?;
        let entry = &self.expected[row.path(1)];
        self.endpoint.0.discard_sync_entry(&parent, entry, cancel)
    }

    fn apply_timestamp(&self, row: &Row, cancel: &AtomicBool) -> io::Result<()> {
        let session = &self.endpoint.0;
        let parent = remote_parent(&self.endpoint, row.path(1))?;
        let entry = &self.expected[row.path(1)];
        let local = row.metadata(0).expect("local timestamp snapshot");
        if row.action == Action::CopyLeft {
            session.verify_sync_entry(&parent, entry, cancel)?;
            let time =
                modified(entry)?.ok_or_else(|| io::Error::other("Server-Zeitstempel fehlt."))?;
            return self.local.set_modified_checked(row.path(0), local, time);
        }
        let path = self.local.checked_path(row.path(0))?;
        files::ensure_unchanged(&path, local)?;
        session.set_sync_modified(&parent, entry, &ftp_timestamp(local.modified()?), cancel)
    }

    fn copy(
        &self,
        row: &Row,
        cancel: &AtomicBool,
        uploaded: &mut impl FnMut(&Row, ftp::Entry) -> io::Result<()>,
    ) -> io::Result<()> {
        let alternate = self.choices.alternate.get(&row.relative);
        let append = self.choices.append.contains(&row.relative);
        let local_relative = if row.action == Action::CopyLeft {
            alternate.map_or_else(|| row.path(0), PathBuf::as_path)
        } else {
            row.path(0)
        };
        let local = self.local.checked_path(local_relative)?;
        let remote = row.path(1);
        let parent = remote_parent(&self.endpoint, remote)?;
        let session = &self.endpoint.0;
        let expected = self.expected.get(remote);
        if row.action == Action::CopyRight {
            let before = row.metadata(0).expect("local copy source");
            files::ensure_unchanged(&local, before)?;
            let name = alternate
                .map_or(remote, PathBuf::as_path)
                .file_name()
                .and_then(OsStr::to_str)
                .ok_or_else(|| io::Error::other("FTP-Dateinamen benötigen Unicode."))?;
            if before.is_dir() {
                return session.create_sync_directory(&parent, name, cancel);
            }
            let modified = before
                .modified()
                .ok()
                .filter(|_| self.preserve_time)
                .map(ftp_timestamp);
            let previous = if alternate.is_some() { None } else { expected };
            let joined = if append {
                Some(append_to_ftp_snapshot(
                    session, &parent, previous, &local, before, cancel,
                )?)
            } else {
                None
            };
            let (upload, size) = if let Some(joined) = &joined {
                (joined.path(), joined.as_file().metadata()?.len())
            } else {
                (local.as_path(), before.len())
            };
            return session
                .upload_sync(
                    &parent,
                    SyncUpload {
                        source: upload,
                        name,
                        expected: previous,
                        size,
                        modified_utc: modified.as_deref(),
                    },
                    || {
                        self.local.checked_path(row.path(0))?;
                        files::ensure_unchanged(&local, before)
                    },
                    cancel,
                )
                .and_then(|entry| uploaded(row, entry));
        }

        let expected = expected.expect("remote copy source");
        if expected.directory == Some(true) {
            session.verify_sync_entry(&parent, expected, cancel)?;
            self.local.checked_path(row.path(0))?;
            return fs::create_dir(local);
        }
        let previous = if alternate.is_some() {
            None
        } else {
            row.metadata(0)
        };
        if let Some(previous) = previous {
            files::ensure_unchanged(&local, previous)?;
        }
        let temporary = NamedTempFile::new_in(local.parent().expect("local copy parent"))?;
        session.fetch_sync(&parent, expected, temporary.as_file(), cancel)?;
        if self.preserve_time
            && let Some(time) = modified(expected)?
        {
            temporary.as_file().set_modified(time)?;
        }
        self.local.checked_path(local_relative)?;
        if append {
            let previous = previous
                .ok_or_else(|| io::Error::other("Die lokale Zieldatei zum Anhängen fehlt."))?;
            let temporary = temporary.into_temp_path();
            let metadata = files::regular_metadata(&temporary)?;
            return files::append_file_checked(&temporary, &local, &metadata, previous, cancel);
        }
        files::publish_checked(temporary, &local, previous, cancel)?;
        Ok(())
    }
}

pub fn ftp_timestamp(time: SystemTime) -> String {
    DateTime::<Utc>::from(time)
        .format("%Y%m%d%H%M%S")
        .to_string()
}

pub fn run_rows<'a>(
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

pub fn remote_parent(endpoint: &FtpEndpoint, path: &Path) -> io::Result<ftp::Url> {
    endpoint
        .0
        .relative_directory(&endpoint.1, path.parent().unwrap_or(Path::new("")))
}

pub fn scan(
    endpoint: &FtpEndpoint,
    scope: &Scope,
    recursive: bool,
    ignored: &IgnorePath<'_>,
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
            if !scope.visits(&path) || ignored(&path, entry.directory == Some(true)) {
                continue;
            }
            let directory = entry.directory.ok_or_else(|| {
                io::Error::other(format!(
                    "Der Eintrag „{}“ kann im Serverabgleich nicht als Datei oder Ordner geprüft werden.",
                    path.display()
                ))
            })?;
            if expected.len() >= MAX_ENTRIES {
                return Err(io::Error::other(
                    "Der FTP-Ordnerbaum enthält zu viele Einträge.",
                ));
            }
            let info = EntryInfo {
                size: if directory {
                    None
                } else {
                    Some(entry.size.ok_or_else(|| {
                        io::Error::other(format!(
                            "Für die Serverdatei „{}“ fehlt die Größe.",
                            path.display()
                        ))
                    })?)
                },
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

pub fn modified(entry: &ftp::Entry) -> io::Result<Option<SystemTime>> {
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
