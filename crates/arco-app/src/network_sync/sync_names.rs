use super::copy_names::{decode_remote_name, encode_remote_name};
use super::{FtpEndpoint, MAX_ENTRIES, copy_names};
use arco_core::files;
use arco_core::sync::{LocalRoot, Row, Tree};
use arco_core::transfer_names::NameRules;
use arco_windows::ftp;
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

pub(crate) struct SyncNames {
    rules: NameRules,
    paths: [BTreeMap<PathBuf, PathBuf>; 2],
}

impl SyncNames {
    pub(crate) fn new(rules: NameRules) -> Self {
        Self {
            rules,
            paths: Default::default(),
        }
    }

    pub(crate) fn project(&self, path: &Path) -> io::Result<PathBuf> {
        path.iter().map(|part| self.project_stored(part)).collect()
    }

    pub(super) fn project_remote_leaf(&self, raw: &OsStr) -> io::Result<String> {
        Ok(self.rules.download(unicode(raw)?)?.to_lowercase())
    }

    fn project_stored(&self, stored: &OsStr) -> io::Result<String> {
        let name = unicode(stored)?;
        if self.rules.windows {
            return self.project_remote_leaf(OsStr::new(&decode_remote_name(name)?));
        }
        self.project_remote_leaf(stored)
    }

    pub(super) fn remap(&mut self, side: usize, tree: &mut Tree) -> io::Result<()> {
        self.paths[side] = tree.remap_paths(|path| self.project(path))?;
        Ok(())
    }

    pub(super) fn physical<'a>(&'a self, side: usize, name: &'a Path) -> &'a Path {
        self.paths[side].get(name).map_or(name, PathBuf::as_path)
    }

    pub(super) fn plan_targets(&mut self, rows: &[Row], cancel: &AtomicBool) -> io::Result<()> {
        for row in rows {
            files::check_cancel(cancel)?;
            if row.info(0).is_some() == row.info(1).is_some() {
                continue;
            }
            let target_side = usize::from(row.info(1).is_none());
            let source = self.physical(1 - target_side, &row.relative);
            let leaf = unicode(source.file_name().expect("relative file name"))?;
            let leaf = if target_side == 0 {
                self.rules.download(&self.raw_remote(leaf)?)?
            } else {
                let remote = self.rules.upload(leaf)?;
                if self.rules.windows {
                    encode_remote_name(&remote)?
                } else {
                    remote
                }
            };
            let parent = row.relative.parent().expect("relative parent");
            let physical = self.physical(target_side, parent).join(leaf);
            if self.project(&physical)? != row.relative {
                return Err(io::Error::other(format!(
                    "Der Zielname „{}“ lässt sich nicht eindeutig abgleichen.",
                    physical.display()
                )));
            }
            self.paths[target_side].insert(row.relative.clone(), physical);
        }
        Ok(())
    }

    fn raw_remote(&self, name: &str) -> io::Result<String> {
        if self.rules.windows {
            decode_remote_name(name)
        } else {
            Ok(name.to_owned())
        }
    }

    pub(crate) fn remote_parent(
        &self,
        endpoint: &FtpEndpoint,
        path: &Path,
    ) -> io::Result<ftp::Url> {
        copy_names::remote_parent(endpoint, path, self.rules.windows)
    }

    pub(crate) fn verify_local_absent(
        &self,
        root: &LocalRoot,
        relative: &Path,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        let mut parent = PathBuf::new();
        let mut parts = relative.iter().peekable();
        while let Some(key) = parts.next() {
            root.verify()?;
            let dir = if parent.as_os_str().is_empty() {
                root.path().to_path_buf()
            } else {
                root.checked_path(&parent)?
            };
            let mut matched = None;
            for (index, entry) in fs::read_dir(dir)?.enumerate() {
                check_listing_limit(index, cancel)?;
                let entry = entry?;
                let name = entry.file_name();
                if self.project_stored(&name)?.as_str() != key {
                    continue;
                }
                if parts.peek().is_none() || matched.is_some() {
                    return Err(local_conflict());
                }
                let metadata = fs::symlink_metadata(entry.path())?;
                if !metadata.is_dir() || files::is_link(&metadata) {
                    return Err(local_conflict());
                }
                matched = Some(name);
            }
            root.verify()?;
            let Some(name) = matched else { return Ok(()) };
            parent.push(name);
        }
        Err(local_conflict())
    }

    pub(crate) fn find_remote(
        &self,
        endpoint: &FtpEndpoint,
        relative: &Path,
        cancel: &AtomicBool,
        by_comparison_name: bool,
    ) -> io::Result<Option<ftp::Entry>> {
        let mut directory = endpoint.1.clone();
        let mut parts = relative.iter().peekable();
        while let Some(key) = parts.next() {
            let key = unicode(key)?;
            let exact = (!by_comparison_name)
                .then(|| self.raw_remote(key))
                .transpose()?;
            let mut matched = None;
            for (index, entry) in endpoint
                .0
                .list_sync(&directory, cancel)?
                .entries
                .into_iter()
                .enumerate()
            {
                check_listing_limit(index, cancel)?;
                let relevant = match &exact {
                    Some(name) => entry.name == *name,
                    None => self.project_remote_leaf(OsStr::new(&entry.name))? == key,
                };
                if relevant && matched.replace(entry).is_some() {
                    return Err(remote_conflict());
                }
            }
            let Some(entry) = matched else {
                return Ok(None);
            };
            if parts.peek().is_none() {
                return Ok(Some(entry));
            }
            if entry.directory != Some(true) {
                return Err(remote_conflict());
            }
            directory = endpoint.0.child_directory(&directory, &entry.name)?;
        }
        Err(remote_conflict())
    }

    pub(super) fn verify_remote_absent(
        &self,
        endpoint: &FtpEndpoint,
        relative: &Path,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        if self
            .find_remote(endpoint, relative, cancel, true)?
            .is_some()
        {
            return Err(remote_conflict());
        }
        Ok(())
    }
}

fn unicode(name: &OsStr) -> io::Result<&str> {
    name.to_str()
        .ok_or_else(|| io::Error::other("Dateinamen benötigen Unicode."))
}

fn local_conflict() -> io::Error {
    io::Error::other("Lokal ist inzwischen ein gleichwertiger Name vorhanden.")
}

fn remote_conflict() -> io::Error {
    io::Error::other("Auf dem Server ist inzwischen ein gleichwertiger Name vorhanden.")
}

fn check_listing_limit(index: usize, cancel: &AtomicBool) -> io::Result<()> {
    files::check_cancel(cancel)?;
    if index >= MAX_ENTRIES {
        return Err(io::Error::other(
            "Der Abgleich kann diesen Ordner wegen seiner Eintragszahl nicht sicher prüfen.",
        ));
    }
    Ok(())
}
