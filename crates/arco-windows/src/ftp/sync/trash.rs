use super::{
    Entry, Security, Session, Url, check_cancel, check_quote_name, expect_entry, find_entry,
};
use std::io;
use std::path::Path;
use std::sync::atomic::AtomicBool;

const BIN: &str = ".arco-trash";

impl Session {
    pub(super) fn replace_recycling_previous(
        &self,
        parent: &Url,
        staged: &Entry,
        previous: &Entry,
        cancel: &AtomicBool,
    ) -> io::Result<Entry> {
        if previous.directory != Some(false) {
            return Err(io::Error::other(
                "Nur reguläre Serverdateien können beim Überschreiben gesichert werden.",
            ));
        }
        self.verify_sync_entry(parent, staged, cancel)?;
        let slot = self.recycle_entry(parent, previous, cancel)?;
        let publication = (|| {
            let current = self.list_sync(parent, cancel)?;
            expect_entry(&current, &previous.name, None)?;
            expect_entry(&current, &staged.name, Some(staged))?;
            self.rename_in_directory(parent, &staged.name, &previous.name, cancel)?;
            self.verify_published(parent, staged, &previous.name, cancel)
        })();
        match publication {
            Ok(entry) => Ok(entry),
            Err(error) => {
                let cleanup = AtomicBool::new(false);
                let stage_was_consumed = self
                    .list_sync(parent, &cleanup)
                    .and_then(|listing| expect_entry(&listing, &staged.name, None))
                    .is_ok();
                if stage_was_consumed
                    && let Ok(entry) =
                        self.verify_published(parent, staged, &previous.name, &cleanup)
                {
                    return Ok(entry);
                }
                let restored = (|| {
                    let bin = self.recycle_directory()?;
                    let saved = self.relative_directory(&bin, Path::new(&slot))?;
                    self.verify_sync_entry(&saved, previous, &cleanup)?;
                    self.restore_recycled_entry(&slot, parent, &cleanup)?;
                    Ok::<_, io::Error>(())
                })();
                if restored.is_ok() {
                    Err(error)
                } else {
                    Err(io::Error::new(
                        error.kind(),
                        format!(
                            "{error}. Die alte Datei bleibt im Server-Papierkorb unter {slot}."
                        ),
                    ))
                }
            }
        }
    }

    pub fn recycle_remote_enabled(&self) -> bool {
        self.connection.recycle_remote
    }

    pub fn recycle_directory(&self) -> io::Result<Url> {
        if self.security() == Security::S3 {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Auf diesem Server ist kein Arco-Papierkorb verfügbar.",
            ));
        }
        self.relative_directory(&self.initial_directory(), Path::new(BIN))
    }

    pub fn recycle_relative_path(&self, root: &Url, relative: &Path) -> bool {
        !self.in_recycle_directory(root)
            && self
                .relative_directory(root, relative)
                .is_ok_and(|path| self.in_recycle_directory(&path))
    }

    pub fn recycle_restore_destination(&self, value: &str) -> io::Result<Url> {
        let value = value.trim();
        if value.contains("://") {
            self.directory_url(value)
        } else if value.starts_with('/') {
            self.directory(value)
        } else if matches!(value, "" | ".") {
            Ok(self.initial_directory())
        } else {
            self.relative_directory(&self.initial_directory(), Path::new(value))
        }
    }

    pub fn in_recycle_directory(&self, directory: &Url) -> bool {
        self.recycle_directory()
            .is_ok_and(|bin| within(directory, &bin))
    }

    pub fn will_recycle(&self, parent: &Url) -> bool {
        self.recycle_remote_enabled() && !self.in_recycle_directory(parent)
    }

    pub fn protects_recycle_directory(
        &self,
        parent: &Url,
        entry: &Entry,
        cancel: &AtomicBool,
    ) -> io::Result<bool> {
        if entry.directory != Some(true) || self.security() == Security::S3 {
            return Ok(false);
        }
        let bin = self.recycle_directory()?;
        let source = self.entry_url(parent, entry)?;
        if !within(&bin, &source) {
            return Ok(false);
        }
        if within(&source, &bin) {
            return Ok(true);
        }
        Ok(find_entry(&self.list_sync(&self.initial_directory(), cancel)?, BIN)?.is_some())
    }

    pub fn ensure_recycle_directory(&self, cancel: &AtomicBool) -> io::Result<Url> {
        let directory = self.recycle_directory()?;
        let start = self.initial_directory();
        let listing = self.list_sync(&start, cancel)?;
        match find_entry(&listing, BIN)? {
            Some(entry) if entry.directory == Some(true) && entry.name == BIN => (),
            Some(_) => {
                return Err(io::Error::other(
                    "Der Name des Server-Papierkorbs ist belegt.",
                ));
            }
            None => self.create_sync_directory(&start, BIN, cancel)?,
        }
        Ok(directory)
    }

    pub fn discard_sync_entry(
        &self,
        parent: &Url,
        entry: &Entry,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        if self.protects_recycle_directory(parent, entry, cancel)? {
            return Err(io::Error::other(
                "Öffne den Server-Papierkorb, um seine Inhalte zu entfernen.",
            ));
        }
        if self.will_recycle(parent) {
            self.recycle_entry(parent, entry, cancel).map(|_| ())
        } else {
            self.delete_sync_entry(parent, entry, cancel)
        }
    }

    pub fn recycle_entry(
        &self,
        parent: &Url,
        entry: &Entry,
        cancel: &AtomicBool,
    ) -> io::Result<String> {
        let bin = self.recycle_directory()?;
        let source = self.entry_url(parent, entry)?;
        if within(&source, &bin) || within(&bin, &source) {
            return Err(io::Error::other(
                "Dieser Eintrag liegt im Server-Papierkorb oder enthält ihn.",
            ));
        }
        self.verify_sync_entry(parent, entry, cancel)?;
        self.ensure_recycle_directory(cancel)?;
        let reservation = tempfile::Builder::new()
            .prefix("")
            .rand_bytes(12)
            .tempfile()?;
        let suffix = reservation
            .path()
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| {
                io::Error::other("Der Papierkorbeintrag konnte nicht benannt werden.")
            })?;
        let slot = format!("{}-{suffix}", chrono::Utc::now().format("%Y%m%d-%H%M%SZ"));
        self.create_sync_directory(&bin, &slot, cancel)?;
        let destination = self.relative_directory(&bin, Path::new(&slot))?;
        if let Err(error) = self.move_checked(parent, entry, &destination, cancel) {
            // Keep a slot that may already contain the only copy of the entry.
            return Err(io::Error::new(
                error.kind(),
                format!("Papierkorbeintrag {slot}: {error}"),
            ));
        }
        Ok(slot)
    }

    pub fn restore_recycled_entry(
        &self,
        slot: &str,
        destination: &Url,
        cancel: &AtomicBool,
    ) -> io::Result<Entry> {
        check_quote_name(slot)?;
        if !is_slot(slot) || self.in_recycle_directory(destination) {
            return Err(io::Error::other(
                "Wähle einen Papierkorbeintrag und einen Zielordner außerhalb des Papierkorbs.",
            ));
        }
        let bin = self.recycle_directory()?;
        let slot_entry = self
            .sync_entry_at(&bin, Path::new(slot), cancel)?
            .filter(|entry| entry.name == slot && entry.directory == Some(true))
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::NotFound, "Der Papierkorbeintrag fehlt.")
            })?;
        let parent = self.relative_directory(&bin, Path::new(slot))?;
        let listing = self.list_sync(&parent, cancel)?;
        let [entry] = listing.entries.as_slice() else {
            return Err(io::Error::other(
                "Der Papierkorbeintrag enthält keine eindeutig wiederherstellbare Datei oder keinen eindeutigen Ordner.",
            ));
        };
        self.move_checked(&parent, entry, destination, cancel)?;
        // Once published, cleanup of an empty container must not undo the restore.
        let _ = self.delete_sync_entry(&bin, &slot_entry, cancel);
        Ok(entry.clone())
    }

    fn move_checked(
        &self,
        parent: &Url,
        entry: &Entry,
        destination: &Url,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        check_quote_name(&entry.name)?;
        if entry.directory.is_none() && !entry.symlink {
            return Err(io::Error::other(
                "Der Servertyp des zu verschiebenden Eintrags ist unbekannt.",
            ));
        }
        self.verify_sync_entry(parent, entry, cancel)?;
        expect_entry(&self.list_sync(destination, cancel)?, &entry.name, None)?;
        check_cancel(cancel)?;
        let source = self.entry_url(parent, entry)?;
        let target = if entry.directory == Some(true) {
            self.relative_directory(destination, Path::new(&entry.name))?
        } else {
            self.file(destination, &entry.name)?
        };
        match self.security() {
            Security::Sftp => self.sftp_rename(
                &self.file(parent, &entry.name)?,
                &self.file(destination, &entry.name)?,
                cancel,
            )?,
            Security::Scp => self.scp_rename(
                &self.file(parent, &entry.name)?,
                &self.file(destination, &entry.name)?,
                cancel,
            )?,
            Security::WebDav => self.webdav_move(&source, &target, Some(entry), None, cancel)?,
            security if security.is_ftp() => {
                let target = ftp_target(parent, destination, &entry.name)?;
                self.quote_in_directory(
                    parent,
                    &[&format!("+RNFR {}", entry.name), &format!("+RNTO {target}")],
                    cancel,
                )?;
            }
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "Dieses Protokoll bietet keinen verschiebbaren Papierkorb.",
                ));
            }
        }
        let listing = self.list_sync(destination, cancel)?;
        if !find_entry(&listing, &entry.name)?.is_some_and(|target| {
            target.name == entry.name
                && target.directory == entry.directory
                && target.symlink == entry.symlink
                && target.size == entry.size
        }) {
            return Err(io::Error::other(
                "Der Server hat den verschobenen Eintrag nicht bestätigt.",
            ));
        }
        expect_entry(&self.list_sync(parent, cancel)?, &entry.name, None)
    }
}

fn directory_path(url: &Url) -> String {
    Session::directory_path(url)
        .trim_end_matches('/')
        .to_lowercase()
}

fn within(child: &Url, parent: &Url) -> bool {
    if !same_server(child, parent) {
        return false;
    }
    let (child, parent) = (directory_path(child), directory_path(parent));
    child
        .strip_prefix(&parent)
        .is_some_and(|suffix| suffix.is_empty() || suffix.starts_with('/'))
}

fn same_server(first: &Url, second: &Url) -> bool {
    first.scheme() == second.scheme()
        && first.host_str() == second.host_str()
        && first.port_or_known_default() == second.port_or_known_default()
}

fn ftp_target(parent: &Url, destination: &Url, name: &str) -> io::Result<String> {
    let parent = Session::directory_path(parent);
    let destination = Session::directory_path(destination);
    if parent.starts_with("//") != destination.starts_with("//") {
        return Err(io::Error::other(
            "Der Papierkorb benötigt dieselbe FTP-Pfadwurzel.",
        ));
    }
    let parent: Vec<_> = parent.split('/').filter(|part| !part.is_empty()).collect();
    let destination: Vec<_> = destination
        .split('/')
        .filter(|part| !part.is_empty())
        .collect();
    for part in parent.iter().chain(&destination) {
        check_quote_name(part)?;
    }
    let shared = parent
        .iter()
        .zip(&destination)
        .take_while(|(a, b)| a == b)
        .count();
    let mut target = "../".repeat(parent.len() - shared);
    for part in &destination[shared..] {
        target.push_str(part);
        target.push('/');
    }
    target.push_str(name);
    Ok(target)
}

fn is_slot(value: &str) -> bool {
    let Some((stamp, suffix)) = value.rsplit_once('-') else {
        return false;
    };
    stamp.len() == 16
        && chrono::NaiveDateTime::parse_from_str(stamp, "%Y%m%d-%H%M%SZ").is_ok()
        && suffix.len() == 12
        && suffix.bytes().all(|byte| byte.is_ascii_alphanumeric())
}
