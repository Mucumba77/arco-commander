use crate::network_sync::sync_names::SyncNames;
use crate::network_sync::{FtpEndpoint, FtpPlan, MAX_ENTRIES, ftp_ignores, remote_parent};
use arco_core::{
    files,
    ignore::IgnoreList,
    sync::{Comparison, FileMasks, LocalRoot, Mode, Options, Row, Scope},
    tools::{self, Hash},
};
use arco_windows::ftp;
use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant, SystemTime};

pub struct WatchOptions<'a> {
    pub profile_name: Option<&'a str>,
    pub ignore: Option<&'a IgnoreList>,
    pub interval: Duration,
    pub duration: Option<Duration>,
    pub delete: bool,
    pub initial_upload: bool,
    pub masks: &'a str,
}

pub fn parse_interval(value: &str) -> io::Result<Duration> {
    value
        .trim()
        .parse::<u64>()
        .ok()
        .filter(|seconds| (2..=300).contains(seconds))
        .map(Duration::from_secs)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "Gib ein Prüfintervall zwischen 2 und 300 Sekunden ein.",
            )
        })
}

type Stamps = BTreeMap<PathBuf, (u64, Option<SystemTime>)>;
type PublishedFiles = BTreeMap<PathBuf, PublishedFile>;

struct PublishedFile {
    path: PathBuf,
    entry: ftp::Entry,
    digest: String,
}

pub fn watch(
    root: LocalRoot,
    endpoint: FtpEndpoint,
    watch: WatchOptions<'_>,
    cancel: &AtomicBool,
    mut published: impl FnMut(usize, usize) -> io::Result<()>,
) -> io::Result<()> {
    if endpoint.0.in_recycle_directory(&endpoint.1) {
        return Err(io::Error::other(
            "Der Server-Papierkorb kann nicht das Ziel einer Ordnerüberwachung sein.",
        ));
    }
    let remote_ignore = ftp_ignores(watch.ignore, &endpoint.1, watch.profile_name);
    let masks = FileMasks::new(endpoint.0.transfer_masks())?;
    let extra_masks = FileMasks::new(watch.masks)?;
    let ignore = watch.ignore;
    let delete = watch.delete;
    let rules = endpoint.0.transfer_settings().names;
    let names = (!rules.is_default()).then(|| SyncNames::new(rules));
    let ignored = |path: &Path, directory: bool| {
        endpoint.0.recycle_relative_path(&endpoint.1, path)
            || remote_ignore(path, directory)
            || (!directory && (!masks.includes(path) || !extra_masks.includes(path)))
            || ignore.is_some_and(|rules| rules.local(&root.path().join(path), directory))
    };
    let options = Options {
        mode: Mode::LeftToRight,
        by_content: true,
        ignore_date: true,
        selected_only: true,
        ..Options::default()
    };
    let selection = Scope::new(false, &[])?;
    let snapshot = || -> io::Result<Stamps> {
        root.verify()?;
        let tree = root.scan_ignoring(&options, &selection, ignored, cancel)?;
        root.verify()?;
        let stamps: Stamps = tree
            .entries()
            .filter_map(|(path, info)| {
                info.size
                    .map(|size| (path.to_path_buf(), (size, info.modified)))
            })
            .collect();
        if stamps.len() > MAX_ENTRIES {
            return Err(io::Error::other(
                "Der überwachte Ordner enthält zu viele Dateien.",
            ));
        }
        Ok(stamps)
    };
    let mut sent = snapshot()?;
    let mut previous = sent.clone();
    let mut tracked = PublishedFiles::new();
    let mut initial = 0;
    if (delete || watch.initial_upload) && !sent.is_empty() {
        let selected = sent.keys().cloned().collect::<Vec<_>>();
        let plan = FtpPlan::compare(
            root.path(),
            endpoint.clone(),
            &options,
            &selected,
            &ignored,
            cancel,
            |_| {},
        )?;
        if delete {
            track_equal_files(&plan, &sent, &mut tracked, cancel)?;
        }
        if watch.initial_upload {
            if !matches_snapshot(&plan, &selected, &sent)? {
                return Err(io::Error::other(
                    "Der lokale Ordner hat sich während des Startabgleichs geändert.",
                ));
            }
            initial = apply_uploads(&plan, delete, &mut tracked, cancel)?;
        }
    }
    published(0, 0)?;
    if initial != 0 {
        published(initial, 0)?;
    }
    let deadline = watch.duration.map(|duration| Instant::now() + duration);
    loop {
        let until = Instant::now() + watch.interval;
        let until = deadline.map_or(until, |deadline| deadline.min(until));
        while std::time::Instant::now() < until {
            files::check_cancel(cancel)?;
            std::thread::sleep(
                until
                    .saturating_duration_since(std::time::Instant::now())
                    .min(Duration::from_millis(100)),
            );
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Ok(());
        }
        let current = snapshot()?;
        sent.retain(|path, _| current.contains_key(path));
        let changed = stable_changes(&sent, &previous, &current);
        let mut uploaded = 0;
        if !changed.is_empty() {
            root.verify()?;
            let plan = FtpPlan::compare(
                root.path(),
                endpoint.clone(),
                &options,
                &changed,
                &ignored,
                cancel,
                |_| {},
            )?;
            root.verify()?;
            if !matches_snapshot(&plan, &changed, &current)? {
                previous = current;
                continue;
            }
            if delete {
                track_equal_files(&plan, &current, &mut tracked, cancel)?;
            }
            uploaded = apply_uploads(&plan, delete, &mut tracked, cancel)?;
            for path in changed {
                let stamp = current[&path];
                sent.insert(path, stamp);
            }
        }
        let deleted = if delete {
            delete_missing_files(
                &root,
                &endpoint,
                names.as_ref(),
                &previous,
                &current,
                &mut tracked,
                cancel,
            )?
        } else {
            0
        };
        if uploaded + deleted > 0 {
            published(uploaded, deleted)?;
        }
        previous = current;
    }
}

fn matches_snapshot(plan: &FtpPlan, selected: &[PathBuf], expected: &Stamps) -> io::Result<bool> {
    let mut prepared = BTreeMap::new();
    for row in &plan.rows {
        if matches!(
            row.comparison,
            Comparison::TypeConflict | Comparison::Protected
        ) {
            return Err(io::Error::other(format!(
                "Der Server enthält einen Konflikt zu {}.",
                row.relative.display()
            )));
        }
        if let Some(info) = row.info(0)
            && let Some(size) = info.size
        {
            prepared.insert(
                plan.original_copy_path(0, row.path(0)),
                (size, info.modified),
            );
        }
    }
    Ok(selected
        .iter()
        .all(|path| prepared.get(path.as_path()) == expected.get(path)))
}

fn apply_uploads(
    plan: &FtpPlan,
    track: bool,
    tracked: &mut PublishedFiles,
    cancel: &AtomicBool,
) -> io::Result<usize> {
    let report = plan.apply_with_uploads(
        cancel,
        |_| {},
        |row, entry| {
            if track {
                track_file(plan, row, entry, tracked, cancel)?;
            }
            Ok(())
        },
    );
    if report.cancelled {
        return Err(io::ErrorKind::Interrupted.into());
    }
    if let Some(error) = report.errors.first() {
        return Err(io::Error::other(error.clone()));
    }
    Ok(report.completed)
}

fn track_equal_files(
    plan: &FtpPlan,
    snapshot: &Stamps,
    tracked: &mut PublishedFiles,
    cancel: &AtomicBool,
) -> io::Result<()> {
    for row in &plan.rows {
        let Some(info) = row.info(0) else { continue };
        let Some(size) = info.size else { continue };
        if row.comparison == Comparison::Equal
            && snapshot.get(plan.original_copy_path(0, row.path(0))) == Some(&(size, info.modified))
            && let Some(entry) = plan.expected.get(plan.original_copy_path(1, row.path(1)))
        {
            track_file(plan, row, entry.clone(), tracked, cancel)?;
        }
    }
    Ok(())
}

fn track_file(
    plan: &FtpPlan,
    row: &Row,
    entry: ftp::Entry,
    tracked: &mut PublishedFiles,
    cancel: &AtomicBool,
) -> io::Result<()> {
    let local = plan.original_copy_path(0, row.path(0));
    let path = plan.local.checked_path(local)?;
    let expected = row.metadata(0).expect("local file snapshot");
    files::ensure_unchanged(&path, expected)?;
    let digest = tools::checksum(&path, Hash::Blake3, cancel)?;
    files::ensure_unchanged(&path, expected)?;
    tracked.insert(
        local.to_owned(),
        PublishedFile {
            path: plan.original_copy_path(1, row.path(1)).to_owned(),
            entry,
            digest,
        },
    );
    Ok(())
}

fn delete_missing_files(
    root: &LocalRoot,
    endpoint: &FtpEndpoint,
    names: Option<&SyncNames>,
    previous: &Stamps,
    current: &Stamps,
    tracked: &mut PublishedFiles,
    cancel: &AtomicBool,
) -> io::Result<usize> {
    let project_snapshot = |stamps: &Stamps| {
        stamps
            .keys()
            .map(|path| names.expect("mapped watch").project(path))
            .collect::<io::Result<std::collections::BTreeSet<_>>>()
    };
    let live = if names.is_some() {
        Some([project_snapshot(previous)?, project_snapshot(current)?])
    } else {
        None
    };
    let removed: Vec<_> = tracked
        .keys()
        .filter(|path| !previous.contains_key(*path) && !current.contains_key(*path))
        .cloned()
        .collect();
    let mut deleted = 0;
    for path in removed {
        files::check_cancel(cancel)?;
        let logical = names.map(|names| names.project(&path)).transpose()?;
        if let (Some(logical), Some(live)) = (&logical, &live)
            && live.iter().any(|snapshot| snapshot.contains(logical))
        {
            // A replacement with an equivalent name must never cause a server-side deletion.
            if live.iter().all(|snapshot| snapshot.contains(logical)) {
                tracked.remove(&path);
            }
            continue;
        }
        let verify_local = || {
            if let (Some(names), Some(logical)) = (names, &logical) {
                names.verify_local_absent(root, logical, cancel)
            } else {
                root.verify_absent(&path)
            }
        };
        verify_local()?;
        let file = &tracked[&path];
        let (parent, entry) = if let Some(names) = names {
            (
                names.remote_parent(endpoint, &file.path)?,
                names.find_remote(endpoint, &file.path, cancel, false)?,
            )
        } else {
            (
                remote_parent(endpoint, &file.path)?,
                endpoint.0.sync_entry_at(&endpoint.1, &file.path, cancel)?,
            )
        };
        if entry.is_some() {
            let temporary = tempfile::NamedTempFile::new()?;
            endpoint
                .0
                .fetch_sync(&parent, &file.entry, temporary.as_file(), cancel)?;
            if tools::checksum(temporary.path(), Hash::Blake3, cancel)? != file.digest {
                return Err(io::Error::other(format!(
                    "Die Serverdatei „{}“ wurde geändert. Löschabgleich angehalten.",
                    file.path.display()
                )));
            }
            verify_local()?;
            endpoint
                .0
                .discard_sync_entry(&parent, &file.entry, cancel)
                .map_err(|error| {
                    io::Error::new(
                        error.kind(),
                        format!("Löschabgleich für „{}“: {error}", path.display()),
                    )
                })?;
            deleted += 1;
        }
        tracked.remove(&path);
    }
    Ok(deleted)
}

fn stable_changes(sent: &Stamps, previous: &Stamps, current: &Stamps) -> Vec<PathBuf> {
    current
        .iter()
        .filter(|(path, stamp)| {
            sent.get(*path) != Some(*stamp) && previous.get(*path) == Some(*stamp)
        })
        .map(|(path, _)| path.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn monitoring_waits_for_stable_changes_and_ignores_removal() {
        let path = PathBuf::from("example.txt");
        let removed = PathBuf::from("removed.txt");
        let sent = Stamps::from([(path.clone(), (4, None)), (removed, (5, None))]);
        let current = Stamps::from([(path.clone(), (6, None))]);
        assert!(stable_changes(&sent, &sent, &current).is_empty());
        assert_eq!(stable_changes(&sent, &current, &current), [path]);
    }
}
