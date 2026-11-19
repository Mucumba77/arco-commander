//! SSH File Transfer Protocol. The SSH session validates the pinned host
//! key before attempting authentication and is reused across file operations.

use super::{
    AddressFamily, Connection, Entry, LIST_LIMIT, ListFormat, Listing, Security, Session,
    SyncUpload, Url, check_cancel, check_segment, invalid_input,
};
use base64::Engine;
use base64::engine::general_purpose::STANDARD_NO_PAD;
use chrono::DateTime;
use ssh2::{ErrorCode, FileStat, FileType, HashType, OpenFlags, OpenType, RenameFlags, Sftp};
use std::fs::File;
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::TryLockError;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

pub(super) struct Options {
    pub fingerprint: [u8; 32],
    private_key: Option<PathBuf>,
}

impl Options {
    pub(super) fn uses_key(&self) -> bool {
        self.private_key.is_some()
    }
}

impl Connection {
    pub fn with_ssh(mut self, fingerprint: &str, private_key: Option<PathBuf>) -> io::Result<Self> {
        if self.security != Security::Sftp {
            return Err(invalid_input(
                "SSH-Optionen benötigen eine SFTP-Verbindung.",
            ));
        }
        let invalid = || invalid_input("Gib den SSH-Hostschlüssel als SHA256-Fingerabdruck ein.");
        let digest = fingerprint
            .trim()
            .strip_prefix("SHA256:")
            .ok_or_else(invalid)?;
        let fingerprint = STANDARD_NO_PAD
            .decode(digest.trim_end_matches('='))
            .ok()
            .and_then(|value| value.try_into().ok())
            .ok_or_else(invalid)?;
        if private_key.as_ref().is_some_and(|key| !key.is_absolute()) {
            return Err(invalid_input(
                "Gib einen absoluten Pfad zur privaten SSH-Schlüsseldatei ein.",
            ));
        }
        self.ssh = Some(Options {
            fingerprint,
            private_key,
        });
        Ok(self)
    }
}

pub(super) fn binary_only() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "SFTP unterstützt in diesem Stand Binärtransfers.",
    )
}

fn path(url: &Url) -> io::Result<PathBuf> {
    let mut path = String::new();
    for segment in url.path().split('/').filter(|segment| !segment.is_empty()) {
        let decoded = percent_encoding::percent_decode_str(segment)
            .decode_utf8()
            .map_err(io::Error::other)?;
        check_segment(&decoded)?;
        path.push('/');
        path.push_str(&decoded);
    }
    if path.is_empty() {
        path.push('/');
    }
    Ok(path.into())
}

fn ssh_error(error: ssh2::Error) -> io::Error {
    let kind = match error.code() {
        ErrorCode::SFTP(2 | 10) => io::ErrorKind::NotFound,
        ErrorCode::SFTP(3 | 12) | ErrorCode::Session(-18 | -19) => io::ErrorKind::PermissionDenied,
        ErrorCode::SFTP(11) => io::ErrorKind::AlreadyExists,
        ErrorCode::SFTP(8) => io::ErrorKind::Unsupported,
        ErrorCode::SFTP(6 | 7) | ErrorCode::Session(-7 | -13 | -26 | -43) => {
            io::ErrorKind::ConnectionAborted
        }
        ErrorCode::Session(-9 | -30) => io::ErrorKind::TimedOut,
        _ => io::ErrorKind::Other,
    };
    io::Error::new(kind, error)
}

impl Session {
    pub(super) fn sftp_fingerprint(&self) -> io::Result<String> {
        let options = self
            .connection
            .ssh
            .as_ref()
            .ok_or_else(|| invalid_input("SSH-Profil fehlt."))?;
        Ok(format!(
            "SHA256:{}",
            STANDARD_NO_PAD.encode(options.fingerprint)
        ))
    }

    fn open_sftp(&self, cancel: &AtomicBool) -> io::Result<Sftp> {
        let url = &self.connection.url;
        let host = match url.host() {
            Some(url::Host::Ipv6(address)) => address.to_string(),
            Some(url::Host::Ipv4(address)) => address.to_string(),
            Some(url::Host::Domain(host)) => {
                let host = percent_encoding::percent_decode_str(host)
                    .decode_utf8()
                    .map_err(io::Error::other)?;
                url::Host::parse(&host)
                    .map_err(io::Error::other)?
                    .to_string()
            }
            None => return Err(invalid_input("Die SFTP-Adresse enthält keinen Server.")),
        };
        let addresses = (host.as_str(), url.port().unwrap_or(22)).to_socket_addrs()?;
        let mut last = io::Error::new(
            io::ErrorKind::AddrNotAvailable,
            "Keine passende SFTP-Adresse.",
        );
        let mut socket = None;
        for address in addresses
            .filter(|address| {
                matches!(
                    (address, self.connection.options.family),
                    (_, AddressFamily::Automatic)
                        | (SocketAddr::V4(_), AddressFamily::Ipv4)
                        | (SocketAddr::V6(_), AddressFamily::Ipv6)
                )
            })
            .take(4)
        {
            check_cancel(cancel)?;
            match TcpStream::connect_timeout(&address, Duration::from_secs(5)) {
                Ok(stream) => {
                    socket = Some(stream);
                    break;
                }
                Err(error) => last = error,
            }
        }
        let socket = socket.ok_or(last)?;
        let options = self
            .connection
            .ssh
            .as_ref()
            .ok_or_else(|| invalid_input("SSH-Profil fehlt."))?;
        let mut session = ssh2::Session::new().map_err(ssh_error)?;
        session.set_tcp_stream(socket);
        session.set_timeout(15_000);
        session.handshake().map_err(ssh_error)?;
        let key = session.host_key_hash(HashType::Sha256).ok_or_else(|| {
            io::Error::other("Der SSH-Server hat keinen Hostschlüssel geliefert.")
        })?;
        if key != options.fingerprint {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "Der SSH-Hostschlüssel weicht vom Profil ab. Server: SHA256:{}",
                    STANDARD_NO_PAD.encode(key)
                ),
            ));
        }
        check_cancel(cancel)?;
        let user = &self.connection.user;
        match &options.private_key {
            Some(file) => {
                let passphrase = (!self.password.is_empty()).then_some(self.password.as_str());
                session.userauth_pubkey_file(user, None, file, passphrase)
            }
            None => session.userauth_password(user, self.password.as_str()),
        }
        .map_err(ssh_error)?;
        check_cancel(cancel)?;
        session.sftp().map_err(ssh_error)
    }

    pub(super) fn sftp_operation<T>(
        &self,
        cancel: &AtomicBool,
        operation: impl FnOnce(&Sftp) -> io::Result<T>,
    ) -> io::Result<T> {
        let mut connection = loop {
            check_cancel(cancel)?;
            match self.ssh.try_lock() {
                Ok(connection) => break connection,
                Err(TryLockError::WouldBlock) => std::thread::sleep(Duration::from_millis(25)),
                Err(TryLockError::Poisoned(_)) => {
                    return Err(io::Error::other("SFTP-Sitzung beendet."));
                }
            }
        };
        if connection.is_none() {
            *connection = Some(self.open_sftp(cancel)?);
        }
        let result = operation(connection.as_ref().expect("initialized SFTP session"));
        if result.as_ref().is_err_and(|error| {
            matches!(
                error.kind(),
                io::ErrorKind::ConnectionAborted
                    | io::ErrorKind::ConnectionReset
                    | io::ErrorKind::BrokenPipe
                    | io::ErrorKind::TimedOut
            )
        }) {
            *connection = None;
        }
        result
    }

    pub(super) fn list_sftp(
        &self,
        directory: &Url,
        strict: bool,
        cancel: &AtomicBool,
    ) -> io::Result<Listing> {
        self.validate(directory)?;
        let path = path(directory)?;
        self.sftp_operation(cancel, |sftp| {
            let mut handle = sftp.opendir(&path).map_err(ssh_error)?;
            let mut entries = Vec::new();
            let mut names = std::collections::BTreeSet::new();
            let mut bytes = 0usize;
            loop {
                check_cancel(cancel)?;
                let (name, metadata) = match handle.readdir() {
                    Ok(value) => value,
                    Err(error) if error.code() == ErrorCode::Session(-16) => break,
                    Err(error) => return Err(ssh_error(error)),
                };
                let name = name
                    .to_str()
                    .ok_or_else(|| io::Error::other("Der SFTP-Name benötigt Unicode."))?;
                if matches!(name, "." | "..") {
                    continue;
                }
                check_segment(name)?;
                bytes = bytes.saturating_add(name.len() + 64);
                if bytes > LIST_LIMIT {
                    return Err(io::Error::other(
                        "Das SFTP-Verzeichnis überschreitet das Listenlimit.",
                    ));
                }
                let directory = match metadata.file_type() {
                    FileType::Directory => Some(true),
                    FileType::RegularFile => Some(false),
                    _ => None,
                };
                let modified = metadata
                    .mtime
                    .and_then(|time| i64::try_from(time).ok())
                    .and_then(|time| DateTime::from_timestamp(time, 0))
                    .map(|time| time.format("%Y%m%d%H%M%S").to_string());
                if strict
                    && (!names.insert(name.to_lowercase())
                        || directory == Some(false)
                            && (metadata.size.is_none() || modified.is_none()))
                {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("Mehrdeutige oder unvollständige SFTP-Dateiangaben: „{name}“."),
                    ));
                }
                entries.push(Entry {
                    name: name.to_owned(),
                    directory,
                    size: metadata.size.filter(|_| directory == Some(false)),
                    modified,
                    unique: None,
                });
            }
            handle.close().map_err(ssh_error)?;
            Ok(Listing {
                entries,
                format: ListFormat::Sftp,
            })
        })
    }

    pub(super) fn receive_sftp_file(
        &self,
        url: &Url,
        mut output: &File,
        limit: Option<u64>,
        cancel: &AtomicBool,
        mut observe: impl FnMut() -> io::Result<()>,
    ) -> io::Result<()> {
        self.validate(url)?;
        let remote = path(url)?;
        self.sftp_operation(cancel, |sftp| {
            let expected = sftp.lstat(&remote).map_err(ssh_error)?;
            if expected.file_type() != FileType::RegularFile {
                return Err(invalid_input(
                    "SFTP kann hier nur reguläre Dateien herunterladen.",
                ));
            }
            let mut input = sftp.open(&remote).map_err(ssh_error)?;
            let limit = limit.into_iter().chain(expected.size).min();
            let mut received = 0u64;
            let mut buffer = [0; 64 * 1024];
            loop {
                check_cancel(cancel)?;
                let count = input.read(&mut buffer)?;
                if count == 0 {
                    break;
                }
                received += count as u64;
                if limit.is_some_and(|max| received > max) {
                    return Err(io::Error::other(
                        "Die SFTP-Datei überschreitet das Größenlimit.",
                    ));
                }
                output.write_all(&buffer[..count])?;
                observe()?;
            }
            let current = sftp.lstat(&remote).map_err(ssh_error)?;
            if expected.size.is_some_and(|size| size != received)
                || current.size != expected.size
                || current.mtime != expected.mtime
                || current.file_type() != FileType::RegularFile
            {
                return Err(io::Error::other("Die SFTP-Quelldatei hat sich geändert."));
            }
            input.close().map_err(ssh_error)?;
            observe()
        })
    }

    pub(super) fn send_sftp_file(
        &self,
        remote: &Url,
        source: &Path,
        cancel: &AtomicBool,
        created: &mut bool,
    ) -> io::Result<()> {
        self.validate(remote)?;
        let remote = path(remote)?;
        let mut source = File::open(source)?;
        self.sftp_operation(cancel, |sftp| {
            let mut target = sftp
                .open_mode(
                    &remote,
                    OpenFlags::WRITE | OpenFlags::EXCLUSIVE,
                    0o600,
                    OpenType::File,
                )
                .map_err(ssh_error)?;
            *created = true;
            let mut buffer = [0; 64 * 1024];
            loop {
                check_cancel(cancel)?;
                let count = source.read(&mut buffer)?;
                if count == 0 {
                    break;
                }
                target.write_all(&buffer[..count])?;
            }
            target.close().map_err(ssh_error)
        })
    }

    pub(super) fn publish_sftp_upload(
        &self,
        directory: &Url,
        source: &Path,
        name: &str,
        replace: bool,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        let listing = self.list_sync(directory, cancel)?;
        let previous = listing
            .entries
            .iter()
            .find(|entry| entry.name.eq_ignore_ascii_case(name));
        if previous
            .is_some_and(|entry| !replace || entry.directory != Some(false) || entry.name != name)
        {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "Das SFTP-Ziel ist bereits vorhanden.",
            ));
        }
        let initial = source.metadata()?;
        let modified_utc = initial.modified().ok().map(|time| {
            DateTime::<chrono::Utc>::from(time)
                .format("%Y%m%d%H%M%S")
                .to_string()
        });
        let upload = SyncUpload {
            source,
            name,
            expected: previous,
            size: initial.len(),
            modified_utc: modified_utc.as_deref(),
        };
        self.upload_sync(
            directory,
            upload,
            || {
                let current = source.metadata()?;
                if current.len() == initial.len()
                    && current.modified().ok() == initial.modified().ok()
                {
                    Ok(())
                } else {
                    Err(io::Error::other(
                        "Die lokale SFTP-Quelldatei hat sich geändert.",
                    ))
                }
            },
            cancel,
        )
        .map(|_| ())
    }

    pub(super) fn sftp_create(&self, url: &Url, cancel: &AtomicBool) -> io::Result<()> {
        let path = path(url)?;
        self.sftp_operation(cancel, |sftp| sftp.mkdir(&path, 0o755).map_err(ssh_error))
    }

    pub(super) fn sftp_remove(
        &self,
        url: &Url,
        directory: bool,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        let path = path(url)?;
        self.sftp_operation(cancel, |sftp| {
            if directory {
                sftp.rmdir(&path)
            } else {
                sftp.unlink(&path)
            }
            .map_err(ssh_error)
        })
    }

    pub(super) fn sftp_rename(&self, from: &Url, to: &Url, cancel: &AtomicBool) -> io::Result<()> {
        let from = path(from)?;
        let to = path(to)?;
        self.sftp_operation(cancel, |sftp| {
            match sftp.lstat(&to) {
                Err(error) if matches!(error.code(), ErrorCode::SFTP(2 | 10)) => (),
                Err(error) => return Err(ssh_error(error)),
                Ok(_) => {
                    return Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        "Das SFTP-Ziel existiert.",
                    ));
                }
            }
            sftp.rename(&from, &to, Some(RenameFlags::empty()))
                .map_err(ssh_error)
        })
    }

    pub(super) fn sftp_set_metadata(
        &self,
        url: &Url,
        permissions: Option<u16>,
        modified: Option<&str>,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        let path = path(url)?;
        let mtime = modified
            .map(|time| {
                let time = chrono::NaiveDateTime::parse_from_str(time, "%Y%m%d%H%M%S")
                    .map_err(io::Error::other)?;
                u64::try_from(time.and_utc().timestamp()).map_err(io::Error::other)
            })
            .transpose()?;
        self.sftp_operation(cancel, |sftp| {
            let current = sftp.lstat(&path).map_err(ssh_error)?;
            if !matches!(
                current.file_type(),
                FileType::Directory | FileType::RegularFile
            ) {
                return Err(invalid_input(
                    "SFTP-Dateiattribute benötigen eine Datei oder einen Ordner.",
                ));
            }
            let file_stat = FileStat {
                size: None,
                uid: None,
                gid: None,
                perm: permissions.map(u32::from),
                atime: mtime.map(|time| current.atime.unwrap_or(time)),
                mtime,
            };
            sftp.setstat(&path, file_stat).map_err(ssh_error)
        })
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires scripts/test-sftp-reference.py"]
    fn reference_sftp_workflow() {
        let address = std::env::var("ARCO_SFTP_TEST_URL").unwrap();
        let fingerprint = std::env::var("ARCO_SFTP_TEST_FINGERPRINT").unwrap();
        let key = std::env::var_os("ARCO_SFTP_TEST_KEY")
            .map(PathBuf::from)
            .unwrap();
        let cancel = AtomicBool::new(false);
        let connect = |user, fingerprint: &str, key, password| {
            Connection::new(&address, user, Security::Sftp)
                .unwrap()
                .with_ssh(fingerprint, key)
                .unwrap()
                .authenticate(password)
                .unwrap()
        };
        let wrong = format!("SHA256:{}", STANDARD_NO_PAD.encode([0; 32]));
        let rejected = connect("reject-before-login", &wrong, None, "fixture-only");
        assert_eq!(
            rejected
                .list(&rejected.initial_directory(), &cancel)
                .unwrap_err()
                .kind(),
            io::ErrorKind::PermissionDenied
        );

        let session = connect("arco", &fingerprint, None, "fixture-only");
        let root = session.initial_directory();
        assert_eq!(
            session.list(&root, &cancel).unwrap().format,
            ListFormat::Sftp
        );
        assert!(
            session
                .connection_details(&root, &cancel)
                .unwrap()
                .contains(&fingerprint)
        );
        session
            .create_sync_directory(&root, "Büro #1", &cancel)
            .unwrap();
        let folder = session
            .relative_directory(&root, Path::new("Büro #1"))
            .unwrap();
        let files = tempfile::tempdir().unwrap();
        let source = files.path().join("input");
        let output = files.path().join("output");
        let content: Vec<u8> = (0..270_000).map(|index| (index % 251) as u8).collect();
        std::fs::write(&source, &content).unwrap();
        session
            .upload(&folder, &source, "Grüße %.txt", false, &cancel)
            .unwrap();
        assert_eq!(
            session
                .upload(&folder, &source, "Grüße %.txt", false, &cancel)
                .unwrap_err()
                .kind(),
            io::ErrorKind::AlreadyExists
        );
        let entry = session
            .list_sync(&folder, &cancel)
            .unwrap()
            .entries
            .pop()
            .unwrap();
        session
            .set_unix_permissions(&folder, &entry, 0o644, &cancel)
            .unwrap();
        session
            .rename_sync_entry(&folder, &entry, "Ergebnis.txt", &cancel)
            .unwrap();
        session
            .download(&folder, "Ergebnis.txt", &output, &cancel)
            .unwrap();
        assert_eq!(std::fs::read(&output).unwrap(), content);
        let bounded = files.path().join("bounded");
        assert!(
            session
                .download_bounded(&folder, "Ergebnis.txt", &bounded, 17, &cancel)
                .is_err()
        );
        assert!(!bounded.exists());

        let with_key = connect("arco", &fingerprint, Some(key), "");
        assert_eq!(
            with_key.list_sync(&folder, &cancel).unwrap().entries[0].name,
            "Ergebnis.txt"
        );
        std::fs::write(&source, b"new version").unwrap();
        with_key
            .upload(&folder, &source, "Ergebnis.txt", true, &cancel)
            .unwrap();
        let output = files.path().join("updated");
        with_key
            .download(&folder, "Ergebnis.txt", &output, &cancel)
            .unwrap();
        assert_eq!(std::fs::read(output).unwrap(), b"new version");
        let entry = session
            .list_sync(&folder, &cancel)
            .unwrap()
            .entries
            .pop()
            .unwrap();
        session.delete_sync_entry(&folder, &entry, &cancel).unwrap();
        let directory = session
            .list_sync(&root, &cancel)
            .unwrap()
            .entries
            .pop()
            .unwrap();
        session
            .delete_sync_entry(&root, &directory, &cancel)
            .unwrap();
        assert!(session.list(&root, &cancel).unwrap().entries.is_empty());
    }
}
