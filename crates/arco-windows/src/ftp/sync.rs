use super::{
    Entry, Listing, Session, Url, check_cancel, check_segment, parse_mlsd_with_policy, run,
};
use std::fs::File;
use std::io;
use std::path::{Component, Path};
use std::sync::atomic::AtomicBool;

pub struct SyncUpload<'a> {
    pub source: &'a Path,
    pub name: &'a str,
    pub expected: Option<&'a Entry>,
    pub size: u64,
    pub modified_utc: Option<&'a str>,
}

#[derive(Clone, Copy)]
pub struct SyncChecksum {
    command: &'static str,
    algorithm: &'static str,
    hex_len: usize,
}

impl Session {
    pub fn directory_url(&self, address: &str) -> io::Result<Url> {
        let mut directory = Url::parse(address).map_err(io::Error::other)?;
        self.validate(&directory)?;
        if !directory.path().ends_with('/') {
            directory
                .path_segments_mut()
                .map_err(|_| io::Error::other("Ungültiger FTP-Ordner."))?
                .push("");
        }
        Ok(directory)
    }

    pub fn directory_name(directory: &Url) -> Option<String> {
        let name = directory
            .path_segments()?
            .rfind(|segment| !segment.is_empty())?;
        percent_encoding::percent_decode_str(name)
            .decode_utf8()
            .ok()
            .map(|value| value.into_owned())
    }

    pub fn directory_path(directory: &Url) -> String {
        percent_encoding::percent_decode_str(directory.path())
            .decode_utf8_lossy()
            .into_owned()
    }

    pub fn parent_directory(&self, directory: &Url) -> io::Result<Url> {
        self.validate(directory)?;
        let mut parent = directory.clone();
        parent
            .path_segments_mut()
            .map_err(|_| io::Error::other("Ungültiger FTP-Ordner."))?
            .pop_if_empty()
            .pop()
            .push("");
        Ok(parent)
    }

    pub fn relative_directory(&self, base: &Url, relative: &Path) -> io::Result<Url> {
        self.validate(base)?;
        let mut url = base.clone();
        let mut segments = url
            .path_segments_mut()
            .map_err(|_| io::Error::other("Ungültiger FTP-Ordner."))?;
        segments.pop_if_empty();
        for part in relative.components() {
            let Component::Normal(name) = part else {
                return Err(io::Error::other(
                    "FTP-Unterordner müssen relativ zum gewählten Ordner sein.",
                ));
            };
            let name = name
                .to_str()
                .ok_or_else(|| io::Error::other("FTP-Pfade benötigen Unicode."))?;
            check_segment(name)?;
            segments.push(name);
        }
        segments.push("");
        drop(segments);
        Ok(url)
    }

    pub fn list_sync(&self, directory: &Url, cancel: &AtomicBool) -> io::Result<Listing> {
        let result = run(
            self.command(directory)?,
            &["--request", "MLSD", "--max-time", "60"],
            None,
            cancel,
        );
        let bytes = result.map_err(|error| {
            if error.kind() == io::ErrorKind::Unsupported {
                io::Error::new(
                    error.kind(),
                    format!("Der FTP-Abgleich benötigt MLSD. {error}"),
                )
            } else {
                error
            }
        })?;
        parse_mlsd_with_policy(&bytes, true)
    }

    pub fn fetch_sync(
        &self,
        directory: &Url,
        expected: &Entry,
        output: &File,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        let size = expected
            .size
            .filter(|_| expected.directory == Some(false))
            .ok_or_else(|| {
                io::Error::other("Für den Download fehlt die erwartete FTP-Dateigröße.")
            })?;
        self.verify_sync_entry(directory, expected, cancel)?;
        let url = self.file(directory, &expected.name)?;
        let limit = size.max(1).to_string();
        run(
            self.command(&url)?,
            &["--max-filesize", &limit],
            Some(output.try_clone()?),
            cancel,
        )?;
        if output.metadata()?.len() != size {
            return Err(io::Error::other(
                "Die heruntergeladene Größe weicht vom FTP-Vergleich ab.",
            ));
        }
        self.verify_sync_entry(directory, expected, cancel)
    }

    pub fn verify_sync_entry(
        &self,
        directory: &Url,
        expected: &Entry,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        expect_entry(
            &self.list_sync(directory, cancel)?,
            &expected.name,
            Some(expected),
        )
    }

    pub fn sync_checksum_method(
        &self,
        directory: &Url,
        cancel: &AtomicBool,
    ) -> io::Result<Option<SyncChecksum>> {
        Ok(self
            .checksum_replies(directory, &["FEAT"], cancel)?
            .as_deref()
            .and_then(checksum_method))
    }

    pub fn sync_checksum(
        &self,
        directory: &Url,
        expected: &Entry,
        method: SyncChecksum,
        cancel: &AtomicBool,
    ) -> io::Result<Option<(&'static str, String)>> {
        if check_quote_name(&expected.name).is_err() || expected.directory != Some(false) {
            return Ok(None);
        }
        self.verify_sync_entry(directory, expected, cancel)?;
        let command = format!("+{} {}", method.command, expected.name);
        let opts = format!("+OPTS HASH {}", method.algorithm);
        let mut commands = vec!["+TYPE I"];
        if method.command == "HASH" {
            commands.push(&opts);
        }
        commands.push(&command);
        let Some(replies) = self.checksum_replies(directory, &commands, cancel)? else {
            return Ok(None);
        };
        let Some(digest) = checksum_reply(&replies, method, &expected.name) else {
            return Ok(None);
        };
        self.verify_sync_entry(directory, expected, cancel)?;
        Ok(Some((method.algorithm, digest.to_ascii_lowercase())))
    }

    fn checksum_replies(
        &self,
        directory: &Url,
        commands: &[&str],
        cancel: &AtomicBool,
    ) -> io::Result<Option<Vec<u8>>> {
        let mut arguments = vec!["--head", "--dump-header", "-", "--max-time", "20"];
        for command in commands {
            arguments.extend(["--quote", command]);
        }
        match run(self.command(directory)?, &arguments, None, cancel) {
            Ok(output) => Ok(Some(output)),
            Err(error) if error.kind() != io::ErrorKind::Interrupted => Ok(None),
            Err(error) => Err(error),
        }
    }

    pub fn create_sync_directory(
        &self,
        parent: &Url,
        name: &str,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        check_quote_name(name)?;
        expect_entry(&self.list_sync(parent, cancel)?, name, None)?;
        self.quote_in_directory(parent, &[&format!("+MKD {name}")], cancel)?;
        let listing = self.list_sync(parent, cancel)?;
        if !find_entry(&listing, name)?
            .is_some_and(|entry| entry.name == name && entry.directory == Some(true))
        {
            return Err(io::Error::other(
                "Der Server hat den angelegten Ordner nicht bestätigt.",
            ));
        }
        Ok(())
    }

    pub fn rename_sync_entry(
        &self,
        parent: &Url,
        before: &Entry,
        name: &str,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        check_quote_name(name)?;
        if before.name == name {
            return Ok(());
        }
        let listing = self.list_sync(parent, cancel)?;
        expect_entry(&listing, &before.name, Some(before))?;
        if before.name.to_lowercase() != name.to_lowercase() {
            expect_entry(&listing, name, None)?;
        }
        self.rename_in_directory(parent, &before.name, name, cancel)?;
        self.verify_published(parent, before, name, cancel)
            .map(|_| ())
    }

    pub fn delete_sync_entry(
        &self,
        parent: &Url,
        before: &Entry,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        check_quote_name(&before.name)?;
        let listing = self.list_sync(parent, cancel)?;
        let verb = if before.directory == Some(true) {
            let unchanged = find_entry(&listing, &before.name)?.is_some_and(|entry| {
                entry.name == before.name
                    && entry.directory == Some(true)
                    && entry.unique == before.unique
            });
            if !unchanged {
                return Err(io::Error::other(format!(
                    "Der FTP-Ordner „{}“ hat sich geändert.",
                    before.name
                )));
            }
            "RMD"
        } else {
            expect_entry(&listing, &before.name, Some(before))?;
            "DELE"
        };
        self.quote_in_directory(parent, &[&format!("+{verb} {}", before.name)], cancel)
    }

    pub fn set_unix_permissions(
        &self,
        parent: &Url,
        before: &Entry,
        permissions: u16,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        if permissions > 0o7777 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Ungültige FTP-Dateirechte.",
            ));
        }
        check_quote_name(&before.name)?;
        self.verify_sync_entry(parent, before, cancel)?;
        self.quote_in_directory(
            parent,
            &[&format!("+SITE CHMOD {permissions:03o} {}", before.name)],
            cancel,
        )
    }

    pub fn upload_sync(
        &self,
        parent: &Url,
        upload: SyncUpload<'_>,
        verify_source: impl FnOnce() -> io::Result<()>,
        cancel: &AtomicBool,
    ) -> io::Result<Entry> {
        check_quote_name(upload.name)?;
        if upload.modified_utc.is_some_and(|value| {
            value.len() != 14 || !value.bytes().all(|byte| byte.is_ascii_digit())
        }) {
            return Err(io::Error::other("Ungültiger UTC-Zeitstempel für FTP."));
        }
        let reservation = tempfile::Builder::new()
            .prefix("arco-upload-")
            .suffix(".part")
            .tempfile()?;
        let temporary = reservation
            .path()
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| io::Error::other("Ungültiger temporärer FTP-Dateiname."))?;
        let listing = self.list_sync(parent, cancel)?;
        expect_entry(&listing, upload.name, upload.expected)?;
        expect_entry(&listing, temporary, None)?;
        let result = (|| {
            self.upload(parent, upload.source, temporary, true, cancel)?;
            verify_source()?;
            if let Some(time) = upload.modified_utc {
                match self.quote_in_directory(
                    parent,
                    &[&format!("+MFMT {time} {temporary}")],
                    cancel,
                ) {
                    Err(error) if error.kind() == io::ErrorKind::Unsupported => (),
                    result => result?,
                }
            }
            let listing = self.list_sync(parent, cancel)?;
            expect_entry(&listing, upload.name, upload.expected)?;
            let staged = find_entry(&listing, temporary)?
                .filter(|entry| {
                    entry.name == temporary
                        && entry.directory == Some(false)
                        && entry.size == Some(upload.size)
                })
                .ok_or_else(|| {
                    io::Error::other(
                        "Der Server hat die Größe der temporären Datei nicht bestätigt.",
                    )
                })?;
            let publication = self.rename_in_directory(parent, temporary, upload.name, cancel);
            match (publication, upload.expected) {
                (Err(error), Some(previous)) if error.kind() == io::ErrorKind::Unsupported => {
                    self.replace_with_backup(parent, staged, previous, cancel)
                }
                (Ok(()), _) => self.verify_published(parent, staged, upload.name, cancel),
                (Err(error), _) => Err(error),
            }
        })();
        result.map_err(|error| {
            let cleanup = AtomicBool::new(false);
            let removed = self.list_sync(parent, &cleanup).and_then(|listing| {
                match find_entry(&listing, temporary)? {
                    None => Ok(()),
                    Some(entry) if entry.name == temporary && entry.directory == Some(false) => {
                        self.quote_in_directory(parent, &[&format!("+DELE {temporary}")], &cleanup)
                    }
                    _ => Err(io::Error::other(
                        "Die temporäre Datei ist nicht mehr eindeutig.",
                    )),
                }
            });
            if removed.is_err() {
                return io::Error::other(format!(
                    "{error}. Prüfe den Server auf die temporäre Datei „{temporary}“."
                ));
            }
            error
        })
    }

    fn replace_with_backup(
        &self,
        parent: &Url,
        staged: &Entry,
        previous: &Entry,
        cancel: &AtomicBool,
    ) -> io::Result<Entry> {
        let mut backup = previous.clone();
        backup.name = format!("{}.backup", staged.name);
        let listing = self.list_sync(parent, cancel)?;
        expect_entry(&listing, &previous.name, Some(previous))?;
        expect_entry(&listing, &backup.name, None)?;
        expect_entry(&listing, &staged.name, Some(staged))?;
        check_cancel(cancel)?;
        let moved = self.rename_in_directory(parent, &previous.name, &backup.name, cancel);
        let result = moved.and_then(|()| {
            let listing = self.list_sync(parent, cancel)?;
            expect_entry(&listing, &backup.name, Some(&backup))?;
            expect_entry(&listing, &previous.name, None)?;
            expect_entry(&listing, &staged.name, Some(staged))?;
            self.rename_in_directory(parent, &staged.name, &previous.name, cancel)?;
            self.verify_published(parent, staged, &previous.name, cancel)
        });
        let cleanup = AtomicBool::new(false);
        let published = result.map_err(|error| {
            if self
                .restore_backup(parent, previous, &backup, &cleanup)
                .is_err()
            {
                return io::Error::other(format!(
                    "{error}. Die Wiederherstellung von „{}“ konnte nicht bestätigt werden.",
                    backup.name
                ));
            }
            error
        })?;
        let removed = self
            .verify_sync_entry(parent, &backup, &cleanup)
            .and_then(|()| {
                self.quote_in_directory(parent, &[&format!("+DELE {}", backup.name)], &cleanup)
            });
        if let Err(error) = removed {
            return Err(io::Error::other(format!(
                "„{}“ wurde aktualisiert. Die alte Version „{}“ konnte nicht entfernt werden. {error}",
                previous.name, backup.name
            )));
        }
        Ok(published)
    }

    fn restore_backup(
        &self,
        parent: &Url,
        previous: &Entry,
        backup: &Entry,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        let listing = self.list_sync(parent, cancel)?;
        let current = find_entry(&listing, &previous.name)?;
        let saved = find_entry(&listing, &backup.name)?;
        match (current, saved) {
            (Some(current), None) if current == previous => Ok(()),
            (None, Some(saved)) if saved == backup => {
                self.rename_in_directory(parent, &backup.name, &previous.name, cancel)
            }
            _ => Err(io::Error::other(
                "Der Zielordner hat sich während der Wiederherstellung geändert.",
            )),
        }
    }

    fn rename_in_directory(
        &self,
        parent: &Url,
        from: &str,
        to: &str,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        check_quote_name(from)?;
        check_quote_name(to)?;
        self.quote_in_directory(
            parent,
            &[&format!("+RNFR {from}"), &format!("+RNTO {to}")],
            cancel,
        )
    }

    fn verify_published(
        &self,
        parent: &Url,
        staged: &Entry,
        name: &str,
        cancel: &AtomicBool,
    ) -> io::Result<Entry> {
        let mut published = staged.clone();
        published.name = name.to_owned();
        self.verify_sync_entry(parent, &published, cancel)
            .map_err(|error| {
                io::Error::other(format!(
                    "Die FTP-Veröffentlichung von „{name}“ konnte nicht bestätigt werden. {error}"
                ))
            })?;
        Ok(published)
    }

    fn quote_in_directory(
        &self,
        directory: &Url,
        commands: &[&str],
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        let mut arguments = vec!["--head", "--max-time", "20"];
        for command in commands {
            arguments.extend(["--quote", command]);
        }
        run(self.command(directory)?, &arguments, None, cancel).map(|_| ())
    }
}

fn checksum_method(replies: &[u8]) -> Option<SyncChecksum> {
    let replies = String::from_utf8_lossy(replies);
    let mut lines = replies.lines();
    lines.by_ref().find(|line| line.starts_with("211-"))?;
    let features: Vec<_> = lines
        .take_while(|line| !line.starts_with("211 "))
        .filter_map(|line| line.strip_prefix(' ').map(str::trim_start))
        .filter_map(|line| {
            let mut fields = line.splitn(2, char::is_whitespace);
            Some((fields.next()?, fields.next().unwrap_or("").trim()))
        })
        .collect();
    let hash = features
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case("HASH"))
        .flat_map(|(_, algorithms)| algorithms.split(';'));
    let advertised: Vec<_> = hash.map(|name| name.trim().trim_end_matches('*')).collect();
    for (algorithm, hex_len) in [
        ("SHA-512", 128),
        ("SHA-256", 64),
        ("SHA-1", 40),
        ("MD5", 32),
    ] {
        if advertised
            .iter()
            .any(|name| name.eq_ignore_ascii_case(algorithm))
        {
            return Some(SyncChecksum {
                command: "HASH",
                algorithm,
                hex_len,
            });
        }
    }
    for (command, algorithm, hex_len) in [
        ("XSHA1", "SHA-1", 40),
        ("XMD5", "MD5", 32),
        ("MD5", "MD5", 32),
        ("XCRC", "CRC32", 8),
    ] {
        if features
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case(command))
        {
            return Some(SyncChecksum {
                command,
                algorithm,
                hex_len,
            });
        }
    }
    None
}

fn checksum_reply(replies: &[u8], method: SyncChecksum, name: &str) -> Option<String> {
    let replies = String::from_utf8_lossy(replies);
    let mut multiline = None;
    let mut digest = None;
    for line in replies.lines() {
        let Some((code, payload)) = line.split_at_checked(3) else {
            continue;
        };
        if let Some(start) = multiline {
            if code == start && payload.starts_with(' ') {
                multiline = None;
            }
            continue;
        }
        if !code.bytes().all(|byte| byte.is_ascii_digit()) {
            continue;
        }
        if payload.starts_with('-') {
            multiline = Some(code);
            continue;
        }
        let Some(payload) = payload.strip_prefix(' ') else {
            continue;
        };
        let parsed = if method.command == "HASH" {
            if code != "213" {
                continue;
            }
            let mut fields = payload.splitn(3, ' ');
            let algorithm = fields.next()?;
            let hash = fields.next()?;
            let path = fields.next()?;
            (algorithm.eq_ignore_ascii_case(method.algorithm) && checksum_name(path, name))
                .then_some(hash)
        } else {
            if !matches!(code, "213" | "250" | "251") {
                continue;
            }
            let (first, rest) = payload.split_once(' ').unwrap_or((payload, ""));
            if rest.is_empty() || checksum_name(rest, name) {
                Some(first)
            } else {
                payload
                    .rsplit_once(' ')
                    .and_then(|(path, hash)| checksum_name(path, name).then_some(hash))
            }
        };
        if let Some(hash) = parsed.filter(|hash| {
            hash.len() == method.hex_len && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
        }) {
            if digest.replace(hash).is_some() {
                return None;
            }
        }
    }
    digest.map(str::to_owned)
}

fn checksum_name(path: &str, expected: &str) -> bool {
    path == expected
        || path
            .strip_prefix('"')
            .and_then(|path| path.strip_suffix('"'))
            == Some(expected)
}

fn check_quote_name(name: &str) -> io::Result<()> {
    check_segment(name)?;
    if name.trim() != name {
        return Err(io::Error::other(
            "FTP-Schreibbefehle benötigen Namen ohne führende oder abschließende Leerzeichen.",
        ));
    }
    Ok(())
}

fn find_entry<'a>(listing: &'a Listing, name: &str) -> io::Result<Option<&'a Entry>> {
    let name = name.to_lowercase();
    let mut matches = listing
        .entries
        .iter()
        .filter(|entry| entry.name.to_lowercase() == name);
    let first = matches.next();
    if matches.next().is_some() {
        return Err(io::Error::other(
            "Mehrdeutige Groß- und Kleinschreibung im FTP-Ordner.",
        ));
    }
    Ok(first)
}

fn expect_entry(listing: &Listing, name: &str, expected: Option<&Entry>) -> io::Result<()> {
    if find_entry(listing, name)? == expected {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "Der FTP-Eintrag „{name}“ hat sich seit dem Vergleich geändert."
        )))
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use crate::ftp::{Connection, Security};
    use std::fs;

    #[test]
    fn ftp_checksum_negotiation_validates_algorithm_digest_and_filename() {
        let method = checksum_method(
            b"220 Ready\r\n211-Features:\r\n XCRC\r\n HASH SHA-256;SHA-1*\r\n211 End\r\n",
        )
        .unwrap();
        assert_eq!(method.command, "HASH");
        assert_eq!(method.algorithm, "SHA-256");
        let hash = "a1".repeat(32);
        let name = "test file.txt";
        let reply = format!(
            "220-Welcome\r\n213 SHA-256 {hash} {name}\r\n220 Ready\r\n250 OK\r\n213 SHA-256 {hash} {name}\r\n"
        );
        assert_eq!(
            checksum_reply(reply.as_bytes(), method, name),
            Some(hash.clone())
        );
        assert!(checksum_reply(reply.as_bytes(), method, "other.txt").is_none());
        assert!(
            checksum_reply(
                format!("213 SHA-1 {hash} {name}\r\n").as_bytes(),
                method,
                name
            )
            .is_none()
        );
        let legacy =
            checksum_method(b"211-Features:\r\n XCRC filename;start;end\r\n211 End\r\n").unwrap();
        assert_eq!(legacy.command, "XCRC");
        assert_eq!(
            checksum_reply(b"250 OK\r\n250 A10F2245\r\n", legacy, name).as_deref(),
            Some("A10F2245")
        );
        assert!(checksum_reply(b"250 A10F224G\r\n", legacy, name).is_none());
        assert!(checksum_method(b"500 Unknown command\r\n").is_none());
    }

    #[test]
    #[ignore = "requires a disposable writable local FTP server"]
    fn synchronized_upload_is_published_in_the_selected_directory_and_checks_snapshots() {
        let address = std::env::var("ARCO_TEST_FTP").unwrap();
        let session = Connection::new(&address, "arco", Security::Plain)
            .unwrap()
            .authenticate("test-only")
            .unwrap();
        let cancel = AtomicBool::new(false);
        let fixture = tempfile::tempdir().unwrap();
        let local = fixture.path().join("source.txt");
        fs::write(&local, b"first").unwrap();
        let root = session.initial_directory();
        session
            .create_sync_directory(&root, "sync test", &cancel)
            .unwrap();
        let folder = session
            .relative_directory(&root, Path::new("sync test"))
            .unwrap();
        assert_eq!(session.parent_directory(&folder).unwrap(), root);
        assert_eq!(session.parent_directory(&root).unwrap(), root);
        let name = "archive # probe.txt";
        let upload = |expected, size, verify: fn() -> io::Result<()>| {
            session.upload_sync(
                &folder,
                SyncUpload {
                    source: &local,
                    name,
                    expected,
                    size,
                    modified_utc: Some("20231114000000"),
                },
                verify,
                &cancel,
            )
        };
        upload(None, 5, || Ok(())).unwrap();
        let listing = session.list_sync(&folder, &cancel).unwrap();
        let entry = find_entry(&listing, name).unwrap().unwrap();
        assert_eq!(entry.modified.as_deref(), Some("20231114000000"));
        let downloaded = tempfile::tempfile().unwrap();
        session
            .fetch_sync(&folder, entry, &downloaded, &cancel)
            .unwrap();
        assert_eq!(downloaded.metadata().unwrap().len(), 5);

        fs::write(&local, b"second version").unwrap();
        assert!(upload(Some(entry), 14, || Err(io::Error::other("changed source"))).is_err());
        let listing = session.list_sync(&folder, &cancel).unwrap();
        assert_eq!(listing.entries.as_slice(), std::slice::from_ref(entry));
        let mut backup = entry.clone();
        backup.name = "rollback backup".into();
        session
            .rename_in_directory(&folder, &entry.name, &backup.name, &cancel)
            .unwrap();
        session
            .restore_backup(&folder, entry, &backup, &cancel)
            .unwrap();
        upload(Some(entry), 14, || Ok(())).unwrap();
        assert!(
            session
                .fetch_sync(&folder, entry, &tempfile::tempfile().unwrap(), &cancel)
                .is_err()
        );
        let listing = session.list_sync(&folder, &cancel).unwrap();
        assert_eq!(listing.entries.len(), 1);
        assert_eq!(listing.entries[0].size, Some(14));
    }
}
