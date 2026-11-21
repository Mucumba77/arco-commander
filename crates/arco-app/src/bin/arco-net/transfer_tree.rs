use super::{Transfer, invalid};
use arco_app::network_sync::{FtpEndpoint, FtpPlan, MAX_ENTRIES, ftp_ignores};
use arco_core::{
    files,
    ignore::IgnoreList,
    profile::Profile,
    sync::{Action, Comparison, FileMasks, LocalRoot, Row},
};
use arco_windows::ftp::{PreferredTransferMode, Session, Url};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, atomic::AtomicBool};

impl<'a> Transfer<'a> {
    pub(super) fn sources(&self) -> impl Iterator<Item = &'a str> + '_ {
        std::iter::once(self.source).chain(self.more.iter().copied())
    }

    pub(super) fn download_tree(
        &self,
        profile: &Profile,
        session: &Arc<Session>,
        directory: &Url,
        local: &Path,
        profile_name: Option<&str>,
        cancel: &AtomicBool,
    ) -> io::Result<Value> {
        self.validate_tree_options()?;
        let target = LocalRoot::open(&local.join(self.target.unwrap_or(".")))?;
        let mut sources = Sources::default();
        let mut listings = BTreeMap::new();
        for input in self.sources() {
            files::check_cancel(cancel)?;
            let (parent, pattern) =
                session.download_location(directory, input.trim_end_matches('/'))?;
            let listing = match listings.entry(parent.clone()) {
                std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(session.list_sync(&parent, cancel)?)
                }
            };
            let (select, wildcard) = pattern_match(&pattern)?;
            let mut matches = 0;
            for entry in &listing.entries {
                if select(&entry.name) && !entry.symlink {
                    if entry.directory.is_none() {
                        return Err(invalid(
                            "Der Server konnte den Typ einer Quelle nicht eindeutig bestimmen.",
                        ));
                    }
                    let relative = Path::new(&entry.name);
                    if session.recycle_relative_path(&parent, relative) {
                        continue;
                    }
                    sources.add(parent.clone(), &entry.name)?;
                    matches += 1;
                }
            }
            self.require_match(wildcard, input, matches)?;
        }
        let mut plans = Vec::new();
        for (parent, selected) in sources.by_parent() {
            let active = self.effective_session(profile, session, &parent, Some(target.path()))?;
            plans.push(self.prepare_tree(
                profile,
                profile_name,
                target.path(),
                (active, parent),
                &selected,
                false,
                cancel,
            )?);
        }
        run_plans(plans, false, cancel)
    }

    pub(super) fn upload_tree(
        &self,
        profile: &Profile,
        session: &Arc<Session>,
        directory: &Url,
        local: &Path,
        profile_name: Option<&str>,
        cancel: &AtomicBool,
    ) -> io::Result<Value> {
        self.validate_tree_options()?;
        let remote = match self.target {
            Some(path) if path.starts_with('/') => session.directory(path)?,
            Some(path) if path.trim_end_matches(['/', '\\']) != "." => session
                .relative_directory(directory, Path::new(path.trim_end_matches(['/', '\\'])))?,
            _ => directory.clone(),
        };
        let mut sources = Sources::default();
        for input in self.sources() {
            files::check_cancel(cancel)?;
            let path = local.join(input);
            let pattern = path
                .file_name()
                .and_then(OsStr::to_str)
                .ok_or_else(|| invalid("Der Name der lokalen Quelle fehlt."))?;
            let parent = path
                .parent()
                .ok_or_else(|| invalid("Der Quellordner fehlt."))?
                .canonicalize()?;
            let (select, wildcard) = pattern_match(pattern)?;
            let mut matches = 0;
            if !wildcard {
                let name = files::leaf_name(OsStr::new(pattern))?;
                checked_kind(&fs::symlink_metadata(parent.join(name))?)?;
                sources.add(parent, pattern)?;
                matches = 1;
            } else {
                for entry in parent.read_dir()? {
                    files::check_cancel(cancel)?;
                    let entry = entry?;
                    let name = entry.file_name();
                    if let Some(name) = name.to_str()
                        && select(name)
                        && checked_kind(&entry.metadata()?).is_ok()
                    {
                        sources.add(parent.clone(), name)?;
                        matches += 1;
                    }
                }
            }
            self.require_match(wildcard, input, matches)?;
        }
        let mut plans = Vec::new();
        for (parent, selected) in sources.by_parent() {
            let active = self.effective_session(profile, session, &remote, Some(&parent))?;
            plans.push(self.prepare_tree(
                profile,
                profile_name,
                &parent,
                (active, remote.clone()),
                &selected,
                true,
                cancel,
            )?);
        }
        run_plans(plans, true, cancel)
    }

    fn validate_tree_options(&self) -> io::Result<()> {
        if self.latest
            || self.resume
            || self
                .mode
                .is_some_and(|mode| mode != PreferredTransferMode::Binary)
        {
            return Err(invalid(
                "Ordnertransfers verwenden Binärmodus. -latest und -resume sind für einzelne Dateien und flache Dateilisten verfügbar.",
            ));
        }
        Ok(())
    }

    fn require_match(&self, wildcard: bool, input: &str, count: usize) -> io::Result<()> {
        if count == 0 && (!wildcard || self.fail_on_no_match) {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("Keine übertragbare Quelle für „{input}“."),
            ));
        }
        Ok(())
    }

    fn prepare_tree(
        &self,
        profile: &Profile,
        profile_name: Option<&str>,
        local: &Path,
        endpoint: FtpEndpoint,
        selected: &[PathBuf],
        upload: bool,
        cancel: &AtomicBool,
    ) -> io::Result<FtpPlan> {
        let ignore = profile
            .ignore_enabled
            .then(|| IgnoreList::parse(&profile.ignore_rules))
            .transpose()?;
        let remote = ftp_ignores(ignore.as_ref(), &endpoint.1, profile_name);
        let extra = FileMasks::new(self.masks)?;
        let ignored = |path: &Path, directory| {
            remote(path, directory)
                || (!directory && !extra.includes(path))
                || ignore
                    .as_ref()
                    .is_some_and(|ignore| ignore.local(&local.join(path), directory))
        };
        let plan = FtpPlan::for_copy(
            local,
            endpoint.clone(),
            selected,
            upload,
            &ignored,
            cancel,
            |_| {},
        )?;
        for row in &plan.rows {
            if conflicts(row) {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!(
                        "Doppeltes oder vorhandenes Transferziel „{}“.",
                        row.relative.display()
                    ),
                ));
            }
        }
        Ok(plan)
    }
}

fn conflicts(row: &Row) -> bool {
    row.comparison == Comparison::TypeConflict
        || matches!((row.info(0), row.info(1)), (Some(left), Some(right)) if !left.is_dir() || !right.is_dir())
}

fn checked_kind(metadata: &fs::Metadata) -> io::Result<()> {
    if files::is_link(metadata) || !(metadata.is_file() || metadata.is_dir()) {
        return Err(invalid(
            "Ordnertransfers verwenden reguläre Dateien und Ordner.",
        ));
    }
    Ok(())
}

fn run_plans(plans: Vec<FtpPlan>, upload: bool, cancel: &AtomicBool) -> io::Result<Value> {
    let mut completed = 0;
    let mut paths = Vec::new();
    let action = if upload {
        Action::CopyRight
    } else {
        Action::CopyLeft
    };
    let target = usize::from(upload);
    let mut reserved = BTreeSet::new();
    for row in plans
        .iter()
        .flat_map(|plan| &plan.rows)
        .filter(|row| row.action == action)
    {
        let key = row.path(target).to_string_lossy().to_lowercase();
        if !reserved.insert(key) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("Doppeltes Transferziel „{}“.", row.path(target).display()),
            ));
        }
    }
    for plan in plans {
        let report = plan.apply(cancel, |_| {});
        completed += report.completed;
        if report.cancelled {
            return Err(io::ErrorKind::Interrupted.into());
        }
        if let Some(error) = report.errors.first() {
            return Err(io::Error::other(format!(
                "Ordnertransfer nach {completed} abgeschlossenen Einträgen: {error}"
            )));
        }
        for row in plan.rows.iter().filter(|row| row.action == action) {
            paths.push(if upload {
                plan.reported_remote_copy_path(row.path(1))?
            } else {
                plan.local.path().join(row.path(0))
            });
        }
    }
    Ok(if upload {
        json!({"uploaded": paths, "completed": completed})
    } else {
        json!({"downloaded": paths, "completed": completed})
    })
}

struct Sources<T> {
    selected: BTreeMap<String, (T, String)>,
}

impl<T> Default for Sources<T> {
    fn default() -> Self {
        Self {
            selected: BTreeMap::new(),
        }
    }
}

impl<T: Ord + Clone> Sources<T> {
    fn add(&mut self, parent: T, name: &str) -> io::Result<()> {
        arco_core::transfer_names::remote_leaf(name)?;
        let key = name.to_lowercase();
        if let Some((previous, spelling)) = self.selected.get(&key) {
            if previous != &parent || spelling != name {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!("Mehrere Quellen haben das Ziel „{name}“."),
                ));
            }
        } else {
            if self.selected.len() >= MAX_ENTRIES {
                return Err(invalid("Die Transferliste enthält zu viele Einträge."));
            }
            self.selected.insert(key, (parent, name.to_owned()));
        }
        Ok(())
    }

    fn by_parent(self) -> BTreeMap<T, Vec<PathBuf>> {
        let mut groups = BTreeMap::<T, Vec<_>>::new();
        for (parent, name) in self.selected.into_values() {
            groups.entry(parent).or_default().push(name.into());
        }
        groups
    }
}

fn pattern_match(literal: &str) -> io::Result<(impl Fn(&str) -> bool + '_, bool)> {
    let glob = literal
        .contains(['*', '?'])
        .then(|| files::compile_masks(literal))
        .transpose()?;
    let wildcard = glob.is_some();
    Ok((
        move |value: &str| {
            glob.as_ref()
                .map_or(value == literal, |glob| glob.is_match(value))
        },
        wildcard,
    ))
}
