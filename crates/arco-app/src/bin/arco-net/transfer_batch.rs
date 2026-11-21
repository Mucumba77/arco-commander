use super::{Transfer, invalid};
use arco_app::network_sync::{MAX_ENTRIES, modified};
use arco_core::{files, profile::Profile, sync::FileMasks};
use arco_windows::ftp::{Session, Url};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs::Metadata;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, atomic::AtomicBool};

impl Transfer<'_> {
    pub(super) fn is_batch(&self, upload: bool, local: &Path) -> bool {
        !self.more.is_empty()
            || wildcard(self.source)
            || self.target.is_some_and(|target| {
                target.ends_with(['/', '\\']) || (!upload && local.join(target).is_dir())
            })
    }

    pub(super) fn download_batch(
        &self,
        profile: &Profile,
        session: &Arc<Session>,
        directory: &Url,
        local: &Path,
        profile_name: Option<&str>,
        cancel: &AtomicBool,
    ) -> io::Result<Value> {
        let destination = local.join(self.target.unwrap_or(".")).canonicalize()?;
        if !destination.is_dir() {
            return Err(invalid(
                "Für mehrere Downloads ist ein vorhandener Zielordner erforderlich.",
            ));
        }
        let masks = FileMasks::new(self.masks)?;
        let mut listings = BTreeMap::new();
        let mut transfers = BTreeMap::new();
        for source in std::iter::once(self.source).chain(self.more.iter().copied()) {
            files::check_cancel(cancel)?;
            let (parent, pattern) = session.download_location(directory, source)?;
            let (active, profile_mask, listing) = match listings.entry(parent.clone()) {
                std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
                std::collections::btree_map::Entry::Vacant(entry) => {
                    let active =
                        self.effective_session(profile, session, &parent, Some(&destination))?;
                    let profile_mask = FileMasks::new(active.transfer_masks())?;
                    entry.insert((active, profile_mask, session.list_sync(&parent, cancel)?))
                }
            };
            let glob = wildcard(&pattern)
                .then(|| files::compile_masks(&pattern))
                .transpose()?;
            if glob.is_none() {
                if listing
                    .entries
                    .iter()
                    .any(|entry| entry.name == pattern && entry.directory == Some(true))
                {
                    return self.download_tree(
                        profile,
                        session,
                        directory,
                        local,
                        profile_name,
                        cancel,
                    );
                }
                listing.regular_file(&pattern)?;
            }
            let mut found = false;
            for entry in &listing.entries {
                let name = Path::new(&entry.name);
                if entry.directory != Some(false)
                    || !glob
                        .as_ref()
                        .map_or(entry.name == pattern, |glob| glob.is_match(name))
                    || !masks.includes(name)
                    || !profile_mask.includes(name)
                {
                    continue;
                }
                let target =
                    destination.join(active.transfer_settings().names.download(&entry.name)?);
                if transfers.len() >= MAX_ENTRIES {
                    return Err(invalid("Die Transferliste enthält zu viele Dateien."));
                }
                transfers
                    .entry((parent.clone(), entry.name.clone()))
                    .or_insert_with(|| (active.clone(), entry.clone(), target));
                found = true;
            }
            self.check_match(found, source)?;
        }
        let mut transfers: Vec<_> = transfers.into_iter().collect();
        if self.latest && !transfers.is_empty() {
            let index = latest_index(transfers.iter().map(|(_, (_, entry, _))| {
                modified(entry)?
                    .ok_or_else(|| invalid("Für -latest fehlt eine Server-Änderungszeit."))
            }))?;
            transfers = vec![transfers.swap_remove(index)];
        }
        let mut names = BTreeSet::new();
        for (_, (_, _, target)) in &transfers {
            let name = target
                .file_name()
                .and_then(OsStr::to_str)
                .expect("validated target name");
            reserve(&mut names, name, target.try_exists()?)?;
        }
        let mut downloaded = Vec::new();
        for ((parent, name), (active, entry, target)) in transfers {
            let result = active
                .verify_sync_entry(&parent, &entry, cancel)
                .and_then(|()| self.download_file(&active, &parent, &name, &target, cancel));
            result.map_err(|error| transfer_error(&name, downloaded.len(), error))?;
            downloaded.push(target);
        }
        Ok(json!({ "downloaded": downloaded, "completed": downloaded.len() }))
    }

    pub(super) fn upload_batch(
        &self,
        profile: &Profile,
        session: &Arc<Session>,
        directory: &Url,
        local: &Path,
        cancel: &AtomicBool,
    ) -> io::Result<Value> {
        let destination = match self.target {
            Some(target) if target.starts_with('/') => session.directory(target)?,
            Some(target) if target.trim_end_matches(['/', '\\']) == "." => directory.clone(),
            Some(target) => session
                .relative_directory(directory, Path::new(target.trim_end_matches(['/', '\\'])))?,
            None => directory.clone(),
        };
        let remote = session.list_sync(&destination, cancel)?;
        let mut occupied = BTreeMap::<_, Vec<_>>::new();
        for entry in &remote.entries {
            occupied
                .entry(entry.name.to_lowercase())
                .or_default()
                .push(entry);
        }
        let masks = FileMasks::new(self.masks)?;
        let mut transfers = BTreeMap::new();
        for source in std::iter::once(self.source).chain(self.more.iter().copied()) {
            files::check_cancel(cancel)?;
            let requested = local.join(source);
            let name = requested
                .file_name()
                .and_then(OsStr::to_str)
                .ok_or_else(|| invalid("Für den Upload fehlt ein gültiger Dateiname."))?;
            let parent = requested
                .parent()
                .ok_or_else(|| invalid("Der Quellordner fehlt."))?;
            let parent = parent.canonicalize()?;
            let active = self.effective_session(profile, session, &destination, Some(&parent))?;
            let profile_mask = FileMasks::new(active.transfer_masks())?;
            let includes = |name: &Path| masks.includes(name) && profile_mask.includes(name);
            let mut found = false;
            if wildcard(name) {
                let glob = files::compile_masks(name)?;
                for entry in parent.read_dir()? {
                    files::check_cancel(cancel)?;
                    let entry = entry?;
                    let leaf = entry.file_name();
                    let name = Path::new(&leaf);
                    if entry.file_type()?.is_file() && glob.is_match(name) && includes(name) {
                        stage_upload(&mut transfers, entry.path(), &active)?;
                        found = true;
                    }
                }
            } else {
                let path = parent.join(files::leaf_name(OsStr::new(name))?);
                files::regular_metadata(&path)?;
                if includes(Path::new(name)) {
                    stage_upload(&mut transfers, path, &active)?;
                    found = true;
                }
            }
            self.check_match(found, source)?;
        }
        let mut transfers: Vec<_> = transfers.into_iter().collect();
        if self.latest && !transfers.is_empty() {
            let index = latest_index(transfers.iter().map(|(_, (meta, _, _))| meta.modified()))?;
            transfers = vec![transfers.swap_remove(index)];
        }
        let mut names = BTreeSet::new();
        for (_, (_, _, name)) in &transfers {
            let occupied = occupied.get(&name.to_lowercase()).is_some_and(|entries| {
                !(self.resume && session.security().is_ftp()
                    && matches!(entries.as_slice(), [entry] if entry.name == *name && entry.directory == Some(false)))
            });
            reserve(&mut names, name, occupied)?;
        }
        let mut uploaded = Vec::new();
        for (source, (before, active, name)) in transfers {
            let result = files::ensure_unchanged(&source, &before)
                .and_then(|()| self.upload_file(&active, &destination, &source, &name, cancel));
            result.map_err(|error| transfer_error(&name, uploaded.len(), error))?;
            uploaded.push(name);
        }
        Ok(json!({ "uploaded": uploaded, "completed": uploaded.len() }))
    }

    fn check_match(&self, found: bool, source: &str) -> io::Result<()> {
        if !found && self.fail_on_no_match {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("Keine übertragbare Datei für „{source}“."),
            ));
        }
        Ok(())
    }
}

fn wildcard(name: &str) -> bool {
    name.contains(['*', '?'])
}

fn reserve(names: &mut BTreeSet<String>, name: &str, occupied: bool) -> io::Result<()> {
    if occupied || !names.insert(name.to_lowercase()) {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("Doppeltes oder vorhandenes Transferziel „{name}“."),
        ));
    }
    Ok(())
}

fn stage_upload(
    uploads: &mut BTreeMap<PathBuf, (Metadata, Arc<Session>, String)>,
    path: PathBuf,
    active: &Arc<Session>,
) -> io::Result<()> {
    if uploads.len() >= MAX_ENTRIES {
        return Err(invalid("Die Transferliste enthält zu viele Dateien."));
    }
    let leaf = path
        .file_name()
        .ok_or_else(|| invalid("Quellname fehlt."))?;
    let name = leaf
        .to_str()
        .ok_or_else(|| invalid("Dateinamen benötigen Unicode."))?;
    let name = active.transfer_settings().names.upload(name)?;
    let meta = files::regular_metadata(&path)?;
    uploads.entry(path).or_insert((meta, active.clone(), name));
    Ok(())
}

fn latest_index<T: Ord>(times: impl Iterator<Item = io::Result<T>>) -> io::Result<usize> {
    let mut latest = None;
    for (index, time) in times.enumerate() {
        let time = time?;
        if latest.as_ref().is_none_or(|(_, previous)| &time > previous) {
            latest = Some((index, time));
        }
    }
    latest
        .map(|(index, _)| index)
        .ok_or_else(|| invalid("Keine Datei für -latest."))
}

fn transfer_error(name: &str, completed: usize, error: io::Error) -> io::Error {
    io::Error::new(
        error.kind(),
        format!("„{name}“ nach {completed} abgeschlossenen Transfers: {error}"),
    )
}
