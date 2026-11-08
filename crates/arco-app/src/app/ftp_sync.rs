use super::{
    FtpEndpoint, JobContext,
    transfer_queue::{
        ConflictSide, VIRTUAL_CONFLICT_PREVIEW_LIMIT, ask_conflict, conflict_preview,
    },
};
use arco_core::files::{self, Conflict, ConflictDecision, TransferReport};
use arco_core::ignore::{IgnoreList, Source};
use arco_core::sync::{self, Action, EntryInfo, LocalRoot, Mode, Options, Row, Scope, Tree};
use arco_core::tools::{self, Hash};
use arco_windows::ftp::{self, SyncUpload};
use chrono::{DateTime, NaiveDateTime, Utc};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs;
use std::io::{self, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::SystemTime;
use tempfile::NamedTempFile;

const MAX_DEPTH: usize = 64;
const MAX_ENTRIES: usize = 200_000;
type IgnorePath<'a> = dyn Fn(&Path, bool) -> bool + 'a;

pub(super) fn ftp_ignores<'a>(
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

pub(super) struct FtpPlan {
    pub rows: Vec<Row>,
    local: LocalRoot,
    endpoint: FtpEndpoint,
    expected: BTreeMap<PathBuf, ftp::Entry>,
    choices: CopyChoices,
}

pub(super) struct FtpRelayPlan {
    pub rows: Vec<Row>,
    endpoints: [FtpEndpoint; 2],
    entries: [BTreeMap<PathBuf, ftp::Entry>; 2],
    choices: CopyChoices,
}

#[derive(Default)]
struct CopyChoices {
    alternate: BTreeMap<PathBuf, PathBuf>,
    append: BTreeSet<PathBuf>,
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
        let excluded = RefCell::new(BTreeSet::new());
        let skip = |path: &Path, directory| {
            let skip = ignored(path, directory);
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
        let stamp = modified(entry)?.map(ftp_timestamp);
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

fn append_to_ftp_snapshot(
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

fn skip_existing_copies(rows: &mut Vec<Row>, source: usize) {
    rows.retain(|row| row.info(source).is_some());
    for row in rows {
        if row.info(1 - source).is_some() {
            row.action = Action::Skip;
        }
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

impl FtpPlan {
    pub(super) fn compare(
        local: &Path,
        endpoint: FtpEndpoint,
        options: &Options,
        selected: &[PathBuf],
        ignored: &IgnorePath<'_>,
        cancel: &AtomicBool,
        progress: impl FnMut(&Path),
    ) -> io::Result<Self> {
        Self::prepare(
            local, endpoint, options, selected, false, ignored, cancel, progress,
        )
    }

    pub(super) fn for_copy(
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
            local, endpoint, &options, selected, true, ignored, cancel, progress,
        )?;
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
        ignored: &IgnorePath<'_>,
        cancel: &AtomicBool,
        mut progress: impl FnMut(&Path),
    ) -> io::Result<Self> {
        let local = LocalRoot::open(local)?;
        let scope = Scope::new(options.selected_only, selected)?;
        let excluded = RefCell::new(BTreeSet::new());
        let skip = |path: &Path, directory| {
            let skip = ignored(path, directory);
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
        let mut rows = sync::compare_trees(
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
        Ok(Self {
            rows,
            local,
            endpoint,
            expected,
            choices: CopyChoices::default(),
        })
    }

    pub(super) fn resolve_conflicts(&mut self, job: &JobContext) -> io::Result<usize> {
        let sides = [
            CopySide::Local(&self.local),
            CopySide::Remote(&self.endpoint, &self.expected),
        ];
        resolve_copy_conflicts(&mut self.rows, sides, &mut self.choices, job)
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
            let modified = before.modified().ok().map(ftp_timestamp);
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
                .map(|_| ());
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
        if let Some(time) = modified(expected)? {
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
            let directory = entry.directory.expect("strict FTP listing");
            if !scope.visits(&path) || ignored(&path, directory) {
                continue;
            }
            if expected.len() >= MAX_ENTRIES {
                return Err(io::Error::other(
                    "Der FTP-Ordnerbaum enthält zu viele Einträge.",
                ));
            }
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
