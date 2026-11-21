//! SSH File Transfer Protocol. The SSH session validates the pinned host
//! key before attempting authentication and is reused across file operations.

use super::{
    Connection, Entry, LIST_LIMIT, ListFormat, Listing, Session, SyncUpload, Url, check_cancel,
    check_segment, invalid_input,
};
use arco_core::ssh_host_keys::HostKeyPins;
use chrono::DateTime;
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, OnceLock, TryLockError};
use std::time::Duration;
use tokio::runtime::Runtime;
use zeroize::Zeroizing;

#[cfg(windows)]
mod kerberos;
mod links;
mod parallel;
pub use links::{RemoteLinkKind, ResolvedLink};
mod scp;
pub use scp::CommandOutput;
#[cfg(windows)]
mod terminal;
#[cfg(windows)]
pub(crate) use terminal::console_bridge;
mod transport;
mod upload_resume;
mod wire;
pub(super) use transport::SftpConnection;
pub(super) use transport::SshConnection;
use wire::{Attrs, Wire};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SshAgent {
    OpenSsh,
    Pageant,
}

pub const MAX_SSH_JUMPS: usize = 8;

pub struct SshChallenge {
    pub endpoint: String,
    pub instructions: String,
    pub prompt: String,
    pub echo: bool,
    response: tokio::sync::oneshot::Sender<Zeroizing<String>>,
}

impl SshChallenge {
    pub fn expired(&self) -> bool {
        self.response.is_closed()
    }

    pub fn answer(self, value: &str) -> io::Result<()> {
        if value.len() > 4096 {
            return Err(invalid_input("Die SSH-Antwort ist zu lang."));
        }
        self.response
            .send(Zeroizing::new(value.to_owned()))
            .map_err(|_| io::ErrorKind::Interrupted.into())
    }
}

type ChallengeHandler = dyn Fn(SshChallenge) -> io::Result<()> + Send + Sync;

pub(super) struct Options {
    pub fingerprints: HostKeyPins,
    pub(super) kerberos_kex: bool,
    private_key: Option<PathBuf>,
    agent: Option<SshAgent>,
    pub(super) kerberos_spn: Option<String>,
    kerberos_delegate: bool,
    interactive: bool,
    challenge: Option<Arc<ChallengeHandler>>,
    timeout: u64,
    keepalive: Option<u64>,
    jumps: Vec<(Connection, Zeroizing<String>)>,
}

fn runtime() -> io::Result<&'static Runtime> {
    static RUNTIME: OnceLock<io::Result<Runtime>> = OnceLock::new();
    RUNTIME
        .get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .thread_name("arco-ssh")
                .enable_all()
                .build()
        })
        .as_ref()
        .map_err(|error| io::Error::new(error.kind(), error.to_string()))
}

impl Options {
    pub(super) fn uses_key(&self) -> bool {
        self.private_key.is_some()
    }
}

impl Connection {
    pub fn with_ssh(self, fingerprint: &str, private_key: Option<PathBuf>) -> io::Result<Self> {
        self.with_ssh_options(fingerprint, private_key, false)
    }

    /// Require Windows Kerberos during SSH key exchange. If no SSH host key
    /// is pinned, the operator must explicitly supply the trusted principal.
    pub fn with_ssh_kerberos_kex(self, fingerprint: &str, principal: &str) -> io::Result<Self> {
        if !cfg!(windows) {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Kerberos-Schlüsselaustausch benötigt Windows.",
            ));
        }
        if fingerprint.trim().is_empty() && principal.trim().is_empty() {
            return Err(invalid_input(
                "Gib für den Kerberos-Schlüsselaustausch ohne SSH-Fingerabdruck den vertrauenswürdigen Dienstprinzipal ausdrücklich an.",
            ));
        }
        self.with_ssh_options(fingerprint, None, true)?
            .with_ssh_kerberos(principal)
    }

    fn with_ssh_options(
        mut self,
        fingerprint: &str,
        private_key: Option<PathBuf>,
        kerberos_kex: bool,
    ) -> io::Result<Self> {
        if !self.security.is_ssh() {
            return Err(invalid_input(
                "SSH-Optionen benötigen eine SFTP-Verbindung.",
            ));
        }
        let fingerprints = if fingerprint.trim().is_empty() && kerberos_kex {
            HostKeyPins::default()
        } else {
            HostKeyPins::parse(fingerprint)?
        };
        if private_key.as_ref().is_some_and(|key| !key.is_absolute()) {
            return Err(invalid_input(
                "Gib einen absoluten Pfad zur privaten SSH-Schlüsseldatei ein.",
            ));
        }
        self.ssh = Some(Options {
            fingerprints,
            kerberos_kex,
            private_key,
            agent: None,
            kerberos_spn: None,
            kerberos_delegate: false,
            interactive: false,
            challenge: None,
            timeout: 15,
            keepalive: None,
            jumps: Vec::new(),
        });
        Ok(self)
    }

    pub fn with_ssh_agent(mut self, mode: SshAgent) -> io::Result<Self> {
        let options = self
            .ssh
            .as_mut()
            .ok_or_else(|| invalid_input("SSH-Profil fehlt."))?;
        if options.private_key.is_some() || options.kerberos_spn.is_some() {
            return Err(invalid_input(
                "Wähle den SSH-Agenten oder eine private Schlüsseldatei.",
            ));
        }
        options.agent = Some(mode);
        Ok(self)
    }

    pub fn with_ssh_kerberos(mut self, principal: &str) -> io::Result<Self> {
        let options = self
            .ssh
            .as_mut()
            .ok_or_else(|| invalid_input("SSH-Profil fehlt."))?;
        if options.private_key.is_some() || options.agent.is_some() {
            return Err(invalid_input(
                "Wähle für Kerberos eine eigene SSH-Anmeldemethode.",
            ));
        }
        let principal = principal.trim();
        let spn = if principal.is_empty() {
            let Some(url::Host::Domain(host)) = self.url.host() else {
                return Err(invalid_input(
                    "Gib für Kerberos einen DNS-Namen oder Dienstprinzipal ein.",
                ));
            };
            let host = host.trim_end_matches('.');
            if host.is_empty() || host.parse::<std::net::IpAddr>().is_ok() {
                return Err(invalid_input(
                    "Gib für Kerberos einen DNS-Namen oder Dienstprinzipal ein.",
                ));
            }
            format!("host/{host}")
        } else {
            principal.to_owned()
        };
        if spn.len() > 255
            || !spn
                .split_once('/')
                .is_some_and(|(service, host)| !service.is_empty() && !host.is_empty())
            || !spn
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._-/@".contains(&byte))
        {
            return Err(invalid_input(
                "Ungültiger Kerberos-Dienstprinzipal. Verwende host/server.example oder host/server.example@REALM.",
            ));
        }
        options.kerberos_spn = Some(spn);
        Ok(self)
    }

    pub fn with_ssh_kerberos_delegation(mut self, enabled: bool) -> io::Result<Self> {
        if enabled {
            let options = self
                .ssh
                .as_mut()
                .filter(|options| options.kerberos_spn.is_some())
                .ok_or_else(|| {
                    invalid_input(
                        "Ticketdelegation benötigt Windows-Kerberos als SSH-Anmeldemethode.",
                    )
                })?;
            options.kerberos_delegate = true;
        } else if let Some(options) = &mut self.ssh {
            options.kerberos_delegate = false;
        }
        Ok(self)
    }

    pub fn with_ssh_interactive(mut self, enabled: bool) -> io::Result<Self> {
        self.ssh
            .as_mut()
            .ok_or_else(|| invalid_input("SSH-Profil fehlt."))?
            .interactive = enabled;
        Ok(self)
    }

    pub fn on_ssh_challenge(
        mut self,
        handler: impl Fn(SshChallenge) -> io::Result<()> + Send + Sync + 'static,
    ) -> Self {
        if let Some(options) = &mut self.ssh {
            options.challenge = Some(Arc::new(handler));
        }
        self
    }

    pub fn with_ssh_timing(
        mut self,
        timeout: Option<u64>,
        keepalive: Option<u64>,
    ) -> io::Result<Self> {
        let options = self
            .ssh
            .as_mut()
            .ok_or_else(|| invalid_input("SSH-Profil fehlt."))?;
        if timeout.is_some_and(|value| !(1..=300).contains(&value))
            || keepalive.is_some_and(|value| !(1..=600).contains(&value))
        {
            return Err(invalid_input(
                "SSH-Zeitlimit: 1–300 Sekunden. Keepalive: 1–600 Sekunden.",
            ));
        }
        options.timeout = timeout.unwrap_or(15);
        options.keepalive = keepalive;
        Ok(self)
    }

    pub fn with_ssh_jumps(
        mut self,
        jumps: Vec<(Connection, Zeroizing<String>)>,
    ) -> io::Result<Self> {
        if jumps.len() > MAX_SSH_JUMPS {
            return Err(invalid_input(
                "Die SSH-Sprungserverkette darf höchstens acht Server enthalten.",
            ));
        }
        if (!jumps.is_empty() && self.proxy.is_some())
            || jumps.iter().skip(1).any(|(jump, _)| jump.proxy.is_some())
        {
            return Err(invalid_input(
                "Trage den Proxy im Profil des ersten SSH-Sprungservers ein.",
            ));
        }
        let options = self
            .ssh
            .as_mut()
            .ok_or_else(|| invalid_input("SSH-Profil fehlt."))?;
        for (jump, secret) in &jumps {
            if jump.ssh.as_ref().is_none_or(|ssh| !ssh.jumps.is_empty()) {
                return Err(invalid_input(
                    "Jeder SSH-Sprungserver benötigt ein eigenes SSH-Profil.",
                ));
            }
            super::check_password(secret)?;
        }
        options.jumps = jumps;
        Ok(self)
    }
}

pub(super) fn binary_only() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "SFTP unterstützt in diesem Stand Binärtransfers.",
    )
}

fn path(url: &Url) -> io::Result<String> {
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
    Ok(path)
}

impl Session {
    pub fn set_sftp_modified(
        &self,
        directory: &Url,
        entry: &Entry,
        stamp: &str,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        if !self.is_sftp() {
            return Err(invalid_input(
                "Das Setzen der Server-Änderungszeit benötigt SFTP.",
            ));
        }
        self.set_ssh_modified(directory, entry, stamp, cancel)
    }

    pub fn set_ssh_modified(
        &self,
        directory: &Url,
        entry: &Entry,
        stamp: &str,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        let url = self.file(directory, &entry.name)?;
        self.verify_sync_entry(directory, entry, cancel)?;
        match self.security() {
            super::Security::Sftp => self.sftp_set_metadata(&url, None, Some(stamp), cancel)?,
            super::Security::Scp => self.scp_metadata(&url, None, Some(stamp), cancel)?,
            _ => return Err(invalid_input("Diese Funktion benötigt SFTP oder SCP.")),
        }
        let mut expected = entry.clone();
        expected.modified = Some(stamp.to_owned());
        self.verify_sync_entry(directory, &expected, cancel)
            .map_err(|error| {
                io::Error::new(
                    error.kind(),
                    format!("Der Server hat die neue Änderungszeit nicht bestätigt. {error}"),
                )
            })
    }

    pub fn sftp_sha256(
        &self,
        directory: &Url,
        entry: &Entry,
        cancel: &AtomicBool,
        progress: impl FnMut(super::DownloadProgress),
    ) -> io::Result<String> {
        self.sftp_checksum(
            directory,
            entry,
            super::ChecksumAlgorithm::Sha256,
            cancel,
            progress,
        )
    }

    pub(super) fn sftp_checksum(
        &self,
        directory: &Url,
        entry: &Entry,
        algorithm: super::ChecksumAlgorithm,
        cancel: &AtomicBool,
        mut progress: impl FnMut(super::DownloadProgress),
    ) -> io::Result<String> {
        use sha2::{Digest, Sha224, Sha256, Sha384, Sha512, digest::DynDigest};
        if !self.is_sftp() || entry.directory != Some(false) {
            return Err(invalid_input(
                "Für die Prüfsumme wird eine reguläre SFTP-Datei benötigt.",
            ));
        }
        let url = self.file(directory, &entry.name)?;
        self.verify_sync_entry(directory, entry, cancel)?;
        let remote = path(&url)?;
        if let Some(digest) =
            self.sftp_operation(cancel, |wire| wire.checksum(&remote, algorithm))?
        {
            self.verify_sync_entry(directory, entry, cancel)?;
            return Ok(checksum_hex(&digest));
        }
        let observe = |received| {
            progress(super::DownloadProgress {
                received,
                total: entry.size,
                phase: super::DownloadPhase::Receiving,
            });
            Ok(())
        };
        let mut digest: Box<dyn DynDigest> = match algorithm {
            super::ChecksumAlgorithm::Sha512 => Box::new(Sha512::new()),
            super::ChecksumAlgorithm::Sha384 => Box::new(Sha384::new()),
            super::ChecksumAlgorithm::Sha256 => Box::new(Sha256::new()),
            super::ChecksumAlgorithm::Sha224 => Box::new(Sha224::new()),
            super::ChecksumAlgorithm::Sha1 => Box::new(sha1::Sha1::new()),
            super::ChecksumAlgorithm::Md5 => Box::new(md5::Md5::new()),
            super::ChecksumAlgorithm::Crc32 => {
                let mut digest = flate2::CrcWriter::new(io::sink());
                self.receive_sftp_file(&url, &mut digest, 0, entry.size, cancel, observe)?;
                self.verify_sync_entry(directory, entry, cancel)?;
                return Ok(format!("{:08x}", digest.crc().sum()));
            }
        };
        struct DigestWriter<'a>(&'a mut dyn DynDigest);
        impl std::io::Write for DigestWriter<'_> {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.0.update(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        self.receive_sftp_file(
            &url,
            &mut DigestWriter(digest.as_mut()),
            0,
            entry.size,
            cancel,
            observe,
        )?;
        self.verify_sync_entry(directory, entry, cancel)?;
        Ok(checksum_hex(&digest.finalize()))
    }

    pub(super) fn sftp_fingerprint(&self) -> io::Result<String> {
        let options = self
            .connection
            .ssh
            .as_ref()
            .ok_or_else(|| invalid_input("SSH-Profil fehlt."))?;
        if !options.fingerprints.is_empty() {
            return Ok(options.fingerprints.to_string());
        }
        options
            .kerberos_spn
            .as_deref()
            .filter(|_| options.kerberos_kex)
            .map(|principal| format!("kerberos:{principal}"))
            .ok_or_else(|| invalid_input("SSH-Serveridentität fehlt."))
    }

    fn sftp_operation<T>(
        &self,
        cancel: &AtomicBool,
        operation: impl FnOnce(&mut Wire) -> io::Result<T>,
    ) -> io::Result<T> {
        self.with_sftp_connection(cancel, |connection| operation(&mut connection.wire))
    }

    fn with_sftp_connection<T>(
        &self,
        cancel: &AtomicBool,
        operation: impl FnOnce(&mut SftpConnection) -> io::Result<T>,
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
        if connection.as_ref().is_none_or(SftpConnection::is_closed) {
            *connection = Some(self.open_sftp(cancel)?);
        }
        let connection_state = connection.as_mut().expect("initialized SFTP session");
        let result = operation(connection_state);
        if !connection_state.wire.usable {
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
            let handle = sftp.open_dir(&path)?;
            let result = (|| {
                let mut entries = Vec::new();
                let mut names = std::collections::BTreeSet::new();
                let mut bytes = 0usize;
                loop {
                    check_cancel(cancel)?;
                    let Some(batch) = sftp.read_dir(&handle)? else {
                        break;
                    };
                    for (name, metadata) in batch {
                        if matches!(name.as_str(), "." | "..") {
                            continue;
                        }
                        check_segment(&name)?;
                        bytes = bytes.saturating_add(name.len() + 64);
                        if bytes > LIST_LIMIT {
                            return Err(io::Error::other(
                                "Das SFTP-Verzeichnis überschreitet das Listenlimit.",
                            ));
                        }
                        let directory = metadata.directory();
                        let modified = metadata
                            .modified
                            .and_then(|time| DateTime::from_timestamp(i64::from(time), 0))
                            .map(|time| time.format("%Y%m%d%H%M%S").to_string());
                        if strict
                            && (!names.insert(name.to_lowercase())
                                || directory == Some(false)
                                    && (metadata.size.is_none() || modified.is_none()))
                        {
                            return Err(io::Error::new(
                                io::ErrorKind::InvalidData,
                                format!(
                                    "Mehrdeutige oder unvollständige SFTP-Dateiangaben: „{name}“."
                                ),
                            ));
                        }
                        entries.push(Entry {
                            name,
                            directory,
                            symlink: metadata.is_symlink(),
                            size: metadata.size.filter(|_| directory == Some(false)),
                            modified,
                            unique: None,
                        });
                    }
                }
                Ok(Listing {
                    entries,
                    format: ListFormat::Sftp,
                })
            })();
            let closed = sftp.close(&handle);
            result.and_then(|listing| closed.map(|()| listing))
        })
    }

    pub(super) fn connected_sftp_fingerprint(&self, cancel: &AtomicBool) -> io::Result<String> {
        self.with_sftp_connection(cancel, |connection| match connection.peer_fingerprint() {
            Some(fingerprint) => Ok(fingerprint.to_owned()),
            None => self.sftp_fingerprint(),
        })
    }

    pub(super) fn read_sftp_range(
        &self,
        url: &Url,
        range: std::ops::Range<u64>,
        cancel: &AtomicBool,
    ) -> io::Result<Vec<u8>> {
        self.validate(url)?;
        let remote = path(url)?;
        self.sftp_operation(cancel, |sftp| {
            let handle = sftp.open(&remote, false)?;
            let result = (|| {
                let mut offset = range.start;
                let mut data = Vec::new();
                while offset < range.end {
                    check_cancel(cancel)?;
                    let count = (range.end - offset).min(32 * 1024) as u32;
                    let chunk = sftp.read(&handle, offset, count)?.ok_or_else(|| {
                        io::Error::new(io::ErrorKind::UnexpectedEof, "SFTP-Teildatei ist zu kurz.")
                    })?;
                    offset += chunk.len() as u64;
                    data.extend_from_slice(&chunk);
                }
                Ok(data)
            })();
            let closed = sftp.close(&handle);
            result.and_then(|data| closed.map(|()| data))
        })
    }

    pub(super) fn receive_sftp_file(
        &self,
        url: &Url,
        mut output: impl Write,
        offset: u64,
        limit: Option<u64>,
        cancel: &AtomicBool,
        mut observe: impl FnMut(u64) -> io::Result<()>,
    ) -> io::Result<()> {
        self.validate(url)?;
        let remote = path(url)?;
        self.sftp_operation(cancel, |sftp| {
            let expected = sftp.lstat(&remote)?;
            if expected.directory() != Some(false) {
                return Err(invalid_input(
                    "SFTP kann hier nur reguläre Dateien herunterladen.",
                ));
            }
            let handle = sftp.open(&remote, false)?;
            let result = (|| {
                let limit = limit.into_iter().chain(expected.size).min();
                let mut received = offset;
                let mut rate = self.transfer_rate();
                loop {
                    check_cancel(cancel)?;
                    let Some(data) = sftp.read(&handle, received, rate.chunk(32 * 1024) as u32)?
                    else {
                        break;
                    };
                    received += data.len() as u64;
                    if limit.is_some_and(|max| received > max) {
                        return Err(io::Error::other(
                            "Die SFTP-Datei überschreitet das Größenlimit.",
                        ));
                    }
                    rate.wait(received - offset, cancel)?;
                    output.write_all(&data)?;
                    observe(received)?;
                }
                let current = sftp.lstat(&remote)?;
                if expected.size.is_some_and(|size| size != received)
                    || current.size != expected.size
                    || current.modified != expected.modified
                    || current.directory() != Some(false)
                {
                    return Err(io::Error::other("Die SFTP-Quelldatei hat sich geändert."));
                }
                observe(received)
            })();
            let closed = sftp.close(&handle);
            result.and(closed)
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
            let handle = sftp.open(&remote, true)?;
            *created = true;
            let result = (|| {
                let mut buffer = [0; 32 * 1024];
                let mut offset = 0;
                let mut rate = self.transfer_rate();
                let limit = rate.chunk(buffer.len());
                loop {
                    check_cancel(cancel)?;
                    let count = source.read(&mut buffer[..limit])?;
                    if count == 0 {
                        break;
                    }
                    rate.wait(offset + count as u64, cancel)?;
                    sftp.write(&handle, offset, &buffer[..count])?;
                    offset += count as u64;
                }
                Ok(())
            })();
            let closed = sftp.close(&handle);
            result.and(closed)
        })
    }

    pub(super) fn publish_remote_upload(
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
        let modified_utc = initial
            .modified()
            .ok()
            .filter(|_| self.preserve_transfer_time(true))
            .map(|time| {
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
        self.sftp_operation(cancel, |sftp| sftp.mkdir(&path))
    }

    pub(super) fn sftp_remove(
        &self,
        url: &Url,
        directory: bool,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        let path = path(url)?;
        self.sftp_operation(cancel, |sftp| sftp.remove(&path, directory))
    }

    pub(super) fn sftp_rename(&self, from: &Url, to: &Url, cancel: &AtomicBool) -> io::Result<()> {
        let from = path(from)?;
        let to = path(to)?;
        self.sftp_operation(cancel, |sftp| {
            match sftp.lstat(&to) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => (),
                Err(error) => return Err(error),
                Ok(_) => {
                    return Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        "Das SFTP-Ziel existiert.",
                    ));
                }
            }
            sftp.rename(&from, &to)
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
                u32::try_from(time.and_utc().timestamp()).map_err(io::Error::other)
            })
            .transpose()?;
        self.sftp_operation(cancel, |sftp| {
            let current = sftp.lstat(&path)?;
            if current.directory().is_none() {
                return Err(invalid_input(
                    "SFTP-Dateiattribute benötigen eine Datei oder einen Ordner.",
                ));
            }
            sftp.set_attrs(
                &path,
                Attrs {
                    permissions: permissions.map(u32::from),
                    access: mtime.map(|time| current.access.unwrap_or(time)),
                    modified: mtime,
                    ..Default::default()
                },
            )
        })
    }

    pub(super) fn sftp_unix_metadata(
        &self,
        url: &Url,
        cancel: &AtomicBool,
    ) -> io::Result<super::UnixMetadata> {
        let path = path(url)?;
        let attrs = self.sftp_operation(cancel, |sftp| sftp.lstat(&path))?;
        let (owner, group) = attrs.ownership.unzip();
        Ok(super::UnixMetadata {
            mode: attrs.permissions.map(|mode| (mode & 0o7777) as u16),
            owner,
            group,
        })
    }

    pub(super) fn sftp_set_unix_metadata(
        &self,
        url: &Url,
        before: &Entry,
        edit: super::UnixMetadata,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        let path = path(url)?;
        self.sftp_operation(cancel, |sftp| {
            let current = sftp.lstat(&path)?;
            let kind = current.directory().ok_or_else(|| {
                invalid_input(
                    "Server-Rechte lassen sich nur für reguläre Dateien und Ordner ändern.",
                )
            })?;
            let stamp = current
                .modified
                .and_then(|secs| DateTime::from_timestamp(i64::from(secs), 0))
                .map(|date| date.format("%Y%m%d%H%M%S").to_string());
            if before.directory != Some(kind)
                || (!kind && before.size != current.size)
                || before.modified != stamp
            {
                return Err(io::Error::other(
                    "Der Servereintrag hat sich seit dem Anzeigen geändert.",
                ));
            }
            let ownership = if edit.owner.is_some() || edit.group.is_some() {
                let (uid, gid) = current.ownership.unzip();
                let wanted = edit.owner.or(uid).zip(edit.group.or(gid)).ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::Unsupported,
                        "Der SFTP-Server meldet weder UID noch GID für diesen Eintrag.",
                    )
                })?;
                if Some(wanted) != current.ownership {
                    sftp.set_attrs(
                        &path,
                        Attrs {
                            ownership: Some(wanted),
                            ..Default::default()
                        },
                    )?;
                }
                Some(wanted)
            } else {
                None
            };
            if let Some(mode) = edit.mode {
                sftp.set_attrs(
                    &path,
                    Attrs {
                        permissions: Some(u32::from(mode)),
                        ..Default::default()
                    },
                )?;
            }
            let after = sftp.lstat(&path)?;
            if after.directory() != Some(kind)
                || ownership.is_some_and(|wanted| after.ownership != Some(wanted))
                || edit.mode.is_some_and(|mode| {
                    after.permissions.map(|bits| bits & 0o7777) != Some(u32::from(mode))
                })
            {
                return Err(io::Error::other(
                    "Der Server hat die neuen Rechte oder Besitzdaten nicht bestätigt.",
                ));
            }
            Ok(())
        })
    }
}

fn checksum_hex(digest: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use crate::ftp::Security;
    use base64::{Engine, engine::general_purpose::STANDARD_NO_PAD};

    #[test]
    #[ignore = "requires scripts/test-sftp-reference.py"]
    fn reference_sftp_interactive() {
        let address = std::env::var("ARCO_SFTP_TEST_URL").unwrap();
        let fingerprint = std::env::var("ARCO_SFTP_TEST_FINGERPRINT").unwrap();
        let key = PathBuf::from(std::env::var_os("ARCO_SFTP_TEST_ED_KEY").unwrap());
        let cancel = AtomicBool::new(false);
        for (user, key) in [("interactive", None), ("mfa", Some(key))] {
            let prompts = Arc::new(std::sync::Mutex::new(Vec::new()));
            let record = prompts.clone();
            let session = Connection::new(&address, user, Security::Sftp)
                .unwrap()
                .with_ssh(&fingerprint, key)
                .unwrap()
                .with_ssh_interactive(true)
                .unwrap()
                .on_ssh_challenge(move |challenge| {
                    record.lock().unwrap().push(challenge.prompt.clone());
                    assert!(
                        challenge
                            .endpoint
                            .starts_with(&format!("{user}@127.0.0.1:"))
                    );
                    let result = match (challenge.prompt.as_str(), challenge.echo) {
                        ("Password", false) => "fixture-password",
                        ("Account", true) => "test account",
                        ("One-time code", false) => "123456",
                        _ => panic!("unexpected challenge"),
                    };
                    challenge.answer(result)
                })
                .authenticate("")
                .unwrap();
            assert_eq!(
                session
                    .list(&session.initial_directory(), &cancel)
                    .unwrap()
                    .format,
                ListFormat::Sftp
            );
            assert_eq!(
                *prompts.lock().unwrap(),
                ["Password", "Account", "One-time code"]
            );
        }
        let aborted = Connection::new(&address, "interactive", Security::Sftp)
            .unwrap()
            .with_ssh(&fingerprint, None)
            .unwrap()
            .with_ssh_interactive(true)
            .unwrap()
            .on_ssh_challenge(|_| Ok(()))
            .authenticate("")
            .unwrap();
        assert_eq!(
            aborted
                .list(&aborted.initial_directory(), &cancel)
                .unwrap_err()
                .kind(),
            io::ErrorKind::Interrupted
        );
    }

    #[test]
    #[ignore = "requires scripts/test-sftp-reference.py"]
    fn reference_sftp_modern_authentication() {
        fn connection(label: &str, user: &str, key: bool) -> Connection {
            let variable = |field| format!("ARCO_SFTP_TEST_{label}_{field}");
            let key = key.then(|| PathBuf::from(std::env::var_os(variable("KEY")).unwrap()));
            Connection::new(
                &std::env::var(variable("URL")).unwrap(),
                user,
                Security::Sftp,
            )
            .unwrap()
            .with_ssh(&std::env::var(variable("FINGERPRINT")).unwrap(), key)
            .unwrap()
        }

        let route = || {
            vec![
                (
                    connection("ED", "jump", true)
                        .with_proxy(&std::env::var("ARCO_SFTP_TEST_HTTP").unwrap())
                        .unwrap(),
                    Zeroizing::default(),
                ),
                (
                    connection("EC", "jump", true),
                    Zeroizing::new("key-passphrase".into()),
                ),
            ]
        };
        let cancel = AtomicBool::new(false);
        let fingerprint = std::env::var("ARCO_SFTP_TEST_FINGERPRINT").unwrap();
        let address = std::env::var("ARCO_SFTP_TEST_URL").unwrap();
        let target = |user, fingerprint| {
            Connection::new(&address, user, Security::Sftp)
                .unwrap()
                .with_ssh(fingerprint, None)
                .unwrap()
        };
        let session = target("arco", &fingerprint)
            .with_ssh_jumps(route())
            .unwrap()
            .with_ssh_timing(Some(10), Some(2))
            .unwrap()
            .authenticate("fixture-only")
            .unwrap();
        assert_eq!(
            session
                .list(&session.initial_directory(), &cancel)
                .unwrap()
                .format,
            ListFormat::Sftp
        );

        let wrong = format!("SHA256:{}", STANDARD_NO_PAD.encode([0; 32]));
        let rejected = target("bad-target-pin", &wrong)
            .with_ssh_jumps(route())
            .unwrap()
            .authenticate("bad-target-pin")
            .unwrap();
        assert_eq!(
            rejected
                .list(&rejected.initial_directory(), &cancel)
                .unwrap_err()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
        let rejected = target("arco", &fingerprint)
            .with_ssh_jumps(vec![(
                connection("ED", "bad-jump-pin", false)
                    .with_ssh(&wrong, None)
                    .unwrap(),
                Zeroizing::new("bad-jump-pin".into()),
            )])
            .unwrap()
            .authenticate("fixture-only")
            .unwrap();
        assert_eq!(
            rejected
                .list(&rejected.initial_directory(), &cancel)
                .unwrap_err()
                .kind(),
            io::ErrorKind::PermissionDenied
        );

        // Use a private pipe for a disposable OpenSSH-compatible agent.
        let agent_pipe = std::env::var_os("SSH_AUTH_SOCK").unwrap();
        let host = runtime().unwrap();
        let agent_task = host.block_on(async {
            use futures_util::StreamExt;
            use russh::keys::agent::{client::AgentClient, server};
            use tokio::net::windows::named_pipe::ServerOptions;

            let first = ServerOptions::new()
                .first_pipe_instance(true)
                .create(&agent_pipe)
                .unwrap();
            let second = ServerOptions::new().create(&agent_pipe).unwrap();
            let listener = futures_util::stream::iter([first, second]).then(|pipe| async {
                pipe.connect().await?;
                Ok(pipe)
            });
            let task = tokio::spawn(server::serve(Box::pin(listener), ()));
            let key = std::env::var_os("ARCO_SFTP_TEST_ED_KEY").unwrap();
            let key = russh::keys::load_secret_key(Path::new(&key), None).unwrap();
            AgentClient::connect_named_pipe(&agent_pipe)
                .await
                .unwrap()
                .add_identity(&key, &[])
                .await
                .unwrap();
            task
        });
        let agent_address = address.replace("127.0.0.1", "ssh.invalid");
        let agent_session = Connection::new(&agent_address, "arco", Security::Sftp)
            .unwrap()
            .with_ssh(&fingerprint, None)
            .unwrap()
            .with_proxy(&std::env::var("ARCO_SFTP_TEST_SOCKS").unwrap())
            .unwrap()
            .with_ssh_agent(SshAgent::OpenSsh)
            .unwrap()
            .authenticate("")
            .unwrap();
        assert_eq!(
            agent_session
                .list(&agent_session.initial_directory(), &cancel)
                .unwrap()
                .format,
            ListFormat::Sftp
        );
        host.block_on(agent_task).unwrap().unwrap();

        let stalled = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let stalled_session = Connection::new(
            &format!("sftp://{}/", stalled.local_addr().unwrap()),
            "arco",
            Security::Sftp,
        )
        .unwrap()
        .with_ssh(&fingerprint, None)
        .unwrap()
        .with_ssh_timing(Some(1), None)
        .unwrap()
        .authenticate("fixture-only")
        .unwrap();
        assert_eq!(
            stalled_session
                .list(&stalled_session.initial_directory(), &cancel)
                .unwrap_err()
                .kind(),
            io::ErrorKind::TimedOut
        );
    }

    #[test]
    #[ignore = "requires scripts/test-sftp-reference.py"]
    fn reference_sftp_proxy_authentication() {
        let direct = std::env::var("ARCO_SFTP_TEST_URL").unwrap();
        let fingerprint = std::env::var("ARCO_SFTP_TEST_FINGERPRINT").unwrap();
        let cancel = AtomicBool::new(false);
        let named = direct.replace("127.0.0.1", "ssh.invalid");
        for (kind, address) in [
            ("HTTP", &direct),
            ("HTTP_BASIC", &direct),
            ("HTTP_MD5", &named),
            ("HTTP_SHA_256", &named),
            ("HTTP_SHA_512_256_SESS", &named),
            ("SOCKS", &named),
            ("SOCKS_CHAP", &named),
        ] {
            let proxy = std::env::var(format!("ARCO_SFTP_TEST_{kind}_AUTH")).unwrap();
            let connect = |password| {
                Connection::new(address, "arco", Security::Sftp)
                    .unwrap()
                    .with_ssh(&fingerprint, None)
                    .unwrap()
                    .with_proxy(&proxy)
                    .unwrap()
                    .with_proxy_user("proxy-fixture")
                    .unwrap()
                    .with_proxy_password(password)
                    .unwrap()
                    .authenticate("fixture-only")
                    .unwrap()
            };
            let session = connect("gate:\"\\only");
            assert_eq!(
                session
                    .list(&session.initial_directory(), &cancel)
                    .unwrap_or_else(|error| panic!("{kind}: {error}"))
                    .format,
                ListFormat::Sftp
            );
            let rejected = connect("wrong");
            assert_eq!(
                rejected
                    .list(&rejected.initial_directory(), &cancel)
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::PermissionDenied
            );
        }
    }

    #[test]
    #[ignore = "requires scripts/test-sftp-reference.py"]
    fn reference_sftp_socks4_user_id_and_dns_modes() {
        let direct = std::env::var("ARCO_SFTP_TEST_URL").unwrap();
        let fingerprint = std::env::var("ARCO_SFTP_TEST_FINGERPRINT").unwrap();
        let cancel = AtomicBool::new(false);
        for (kind, address) in [
            ("SOCKS4", direct.replace("127.0.0.1", "localhost")),
            ("SOCKS4A", direct.replace("127.0.0.1", "ssh.invalid")),
        ] {
            let proxy = std::env::var(format!("ARCO_SFTP_TEST_{kind}")).unwrap();
            let connect = |user| {
                Connection::new(&address, "arco", Security::Sftp)
                    .unwrap()
                    .with_ssh(&fingerprint, None)
                    .unwrap()
                    .with_proxy(&proxy)
                    .unwrap()
                    .with_proxy_user(user)
                    .unwrap()
                    .authenticate("fixture-only")
                    .unwrap()
            };
            let session = connect("proxy-fixture");
            assert_eq!(
                session
                    .list(&session.initial_directory(), &cancel)
                    .unwrap()
                    .format,
                ListFormat::Sftp
            );
            let rejected = connect("wrong");
            assert_eq!(
                rejected
                    .list(&rejected.initial_directory(), &cancel)
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::PermissionDenied
            );
        }
    }

    #[test]
    #[ignore = "requires scripts/test-sftp-reference.py"]
    fn reference_sftp_telnet_proxy_command() {
        let address = std::env::var("ARCO_SFTP_TEST_URL")
            .unwrap()
            .replace("127.0.0.1", "ssh.invalid");
        let fingerprint = std::env::var("ARCO_SFTP_TEST_FINGERPRINT").unwrap();
        let cancel = AtomicBool::new(false);
        let connect = |pin: &str, env| {
            Connection::new(&address, "arco", Security::Sftp)
                .unwrap()
                .with_ssh(pin, None)
                .unwrap()
                .with_proxy(&std::env::var(env).unwrap())
                .unwrap()
        };
        let default = connect(&fingerprint, "ARCO_SFTP_TEST_TELNET_DEFAULT")
            .authenticate("fixture-only")
            .unwrap();
        assert_eq!(
            default
                .list(&default.initial_directory(), &cancel)
                .unwrap()
                .format,
            ListFormat::Sftp
        );
        let authorize = |pin: &str, password| {
            connect(pin, "ARCO_SFTP_TEST_TELNET")
                .with_telnet_proxy_command(r"%user\n%pass\nconnect %host %port\r\n")
                .unwrap()
                .with_proxy_user("proxy-fixture")
                .unwrap()
                .with_proxy_password(password)
                .unwrap()
                .authenticate("fixture-only")
                .unwrap()
        };
        let session = authorize(&fingerprint, r#"gate:"\only"#);
        assert_eq!(
            session
                .list(&session.initial_directory(), &cancel)
                .unwrap()
                .format,
            ListFormat::Sftp
        );
        let rejected = authorize(&fingerprint, "wrong");
        assert!(
            rejected
                .list(&rejected.initial_directory(), &cancel)
                .is_err()
        );
        let wrong_pin = format!("SHA256:{}", STANDARD_NO_PAD.encode([0; 32]));
        let rejected = authorize(&wrong_pin, r#"gate:"\only"#);
        assert_eq!(
            rejected
                .list(&rejected.initial_directory(), &cancel)
                .unwrap_err()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
    }

    #[test]
    #[ignore = "requires scripts/test-sftp-reference.py"]
    #[cfg(windows)]
    fn reference_sftp_local_proxy_process() {
        let direct = std::env::var("ARCO_SFTP_TEST_URL").unwrap();
        let address = direct.replace("127.0.0.1", "ssh.invalid");
        let pin = std::env::var("ARCO_SFTP_TEST_FINGERPRINT").unwrap();
        let cancel = AtomicBool::new(false);
        let command = std::env::var("ARCO_SFTP_TEST_LOCAL_COMMAND").unwrap();
        let connect = |address: &str, security, pin: &str, command: &str| {
            Connection::new(address, "arco", security)
                .unwrap()
                .with_ssh(pin, None)
                .unwrap()
                .with_proxy("local://command")
                .unwrap()
                .with_local_proxy_command(command)
                .unwrap()
        };
        let authenticate = |connection: Connection| {
            connection
                .with_proxy_user(r#"gate: "Benutzer""#)
                .unwrap()
                .with_proxy_password(r#"paß"%host\only"#)
                .unwrap()
                .authenticate("fixture-only")
                .unwrap()
        };
        let session = authenticate(connect(&address, Security::Sftp, &pin, &command));
        let root = session.initial_directory();
        assert_eq!(
            session.list(&root, &cancel).unwrap().format,
            ListFormat::Sftp
        );
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let dest = temp.path().join("out");
        std::fs::write(&source, b"binary\x00data").unwrap();
        session
            .upload(&root, &source, "local-proxy.bin", false, &cancel)
            .unwrap();
        session
            .download(&root, "local-proxy.bin", &dest, &cancel)
            .unwrap();
        assert_eq!(std::fs::read(dest).unwrap(), b"binary\x00data");
        let listing = session.list_sync(&root, &cancel).unwrap();
        let uploaded = listing.regular_file("local-proxy.bin").unwrap();
        session.delete_sync_entry(&root, uploaded, &cancel).unwrap();

        let local_address = direct.replace("127.0.0.1", "localhost");
        let local_command = std::env::var("ARCO_SFTP_TEST_LOCAL_RESOLVED").unwrap();
        let resolved = authenticate(
            connect(&local_address, Security::Sftp, &pin, &local_command)
                .with_ssh_proxy_dns_locally(true)
                .unwrap()
                .with_options(crate::ftp::ConnectionOptions {
                    family: crate::ftp::AddressFamily::Ipv4,
                    ..Default::default()
                })
                .unwrap(),
        );
        assert_eq!(
            resolved
                .list(&resolved.initial_directory(), &cancel)
                .unwrap()
                .format,
            ListFormat::Sftp
        );

        let wrong_pin = format!("SHA256:{}", STANDARD_NO_PAD.encode([0; 32]));
        let rejected = authenticate(connect(&address, Security::Sftp, &wrong_pin, &command));
        assert_eq!(
            rejected.list(&root, &cancel).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );

        let scp = std::env::var("ARCO_SCP_TEST_URL")
            .unwrap()
            .replace("sftp://127.0.0.1", "scp://ssh.invalid");
        let scp_pin = std::env::var("ARCO_SCP_TEST_FINGERPRINT").unwrap();
        let scp = authenticate(connect(&scp, Security::Scp, &scp_pin, &command));
        let root = scp.initial_directory();
        assert_eq!(scp.list(&root, &cancel).unwrap().format, ListFormat::Scp);
        let target = temp.path().join("scp");
        scp.download(&root, "proxy-reference.txt", &target, &cancel)
            .unwrap();
        assert_eq!(
            std::fs::read(target).unwrap(),
            b"SCP through an authenticated proxy\n"
        );

        let stall = std::env::var("ARCO_SFTP_TEST_LOCAL_STALL").unwrap();
        let stalled = connect(&address, Security::Sftp, &pin, &stall)
            .with_ssh_timing(Some(1), None)
            .unwrap()
            .authenticate("fixture-only")
            .unwrap();
        assert_eq!(
            stalled
                .list(&stalled.initial_directory(), &cancel)
                .unwrap_err()
                .kind(),
            io::ErrorKind::TimedOut
        );
    }

    #[test]
    #[ignore = "requires scripts/test-sftp-reference.py"]
    fn reference_sftp_parallel_transfers_use_independent_connections() {
        use std::sync::{Arc, atomic::Ordering, mpsc};
        use std::time::Duration;

        let session = Connection::new(
            &std::env::var("ARCO_SFTP_TEST_URL").unwrap(),
            "arco",
            Security::Sftp,
        )
        .unwrap()
        .with_ssh(&std::env::var("ARCO_SFTP_TEST_FINGERPRINT").unwrap(), None)
        .unwrap()
        .authenticate("fixture-only")
        .unwrap();
        let directory = session.initial_directory();
        let local = tempfile::tempdir().unwrap();
        let contents = vec![0x5a; 256 * 1024];
        let source = local.path().join("input");
        std::fs::write(&source, &contents).unwrap();
        let name = "parallel-reference.bin";
        session
            .upload(&directory, &source, name, false, &AtomicBool::new(false))
            .unwrap();
        let outputs = [local.path().join("first"), local.path().join("second")];
        let stops = [
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
        ];
        let (arrived, progress) = mpsc::channel();
        std::thread::scope(|scope| {
            let jobs = std::array::from_fn::<_, 2, _>(|index| {
                let task_session = session.with_transfer_connection();
                let stop = stops[index].clone();
                let arrived = arrived.clone();
                let output = &outputs[index];
                let directory = &directory;
                let (release, wait) = mpsc::channel();
                let worker = scope.spawn(move || {
                    let mut first = true;
                    task_session.download_with_progress(directory, name, output, &stop, |sample| {
                        if sample.received > 0 && std::mem::take(&mut first) {
                            arrived.send(index).unwrap();
                            if wait.recv_timeout(Duration::from_secs(15)).is_err() {
                                stop.store(true, Ordering::Relaxed);
                            }
                        }
                    })
                });
                (release, worker)
            });
            let mut entered = [
                progress.recv_timeout(Duration::from_secs(12)).unwrap(),
                progress.recv_timeout(Duration::from_secs(12)).unwrap(),
            ];
            entered.sort();
            assert_eq!(entered, [0, 1]);
            stops[0].store(true, Ordering::Relaxed);
            let [(release_first, first), (release_second, second)] = jobs;
            release_first.send(()).unwrap();
            release_second.send(()).unwrap();
            assert_eq!(
                first.join().unwrap().unwrap_err().kind(),
                io::ErrorKind::Interrupted
            );
            second.join().unwrap().unwrap();
        });
        assert!(!outputs[0].exists());
        assert_eq!(std::fs::read(&outputs[1]).unwrap(), contents);
        let cancel = AtomicBool::new(false);
        let listing = session.list_sync(&directory, &cancel).unwrap();
        session
            .delete_sync_entry(&directory, listing.regular_file(name).unwrap(), &cancel)
            .unwrap();
    }

    #[test]
    #[ignore = "requires scripts/test-sftp-reference.py"]
    fn reference_sftp_changes_the_limit_while_downloading() {
        let fingerprint = std::env::var("ARCO_SFTP_TEST_FINGERPRINT").unwrap();
        let session = Connection::new(
            &std::env::var("ARCO_SFTP_TEST_URL").unwrap(),
            "arco",
            Security::Sftp,
        )
        .unwrap()
        .with_ssh(&fingerprint, None)
        .unwrap()
        .authenticate("fixture-only")
        .unwrap();
        crate::ftp::rate::tests::check_live_download(&session, &session.initial_directory());
    }

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
        let other_wrong = format!("SHA256:{}", STANDARD_NO_PAD.encode([1; 32]));
        let rejected = connect(
            "reject-before-login",
            &format!("{wrong};{other_wrong}"),
            None,
            "fixture-only",
        );
        let error = rejected
            .list(&rejected.initial_directory(), &cancel)
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert!(
            error.to_string().contains("weicht vom Profil ab"),
            "{error}"
        );

        let session = connect(
            "arco",
            &format!("{wrong};{fingerprint}"),
            None,
            "fixture-only",
        );
        let root = session.initial_directory();
        assert_eq!(
            session.list(&root, &cancel).unwrap().format,
            ListFormat::Sftp
        );
        let details = session.connection_details(&root, &cancel).unwrap();
        assert!(details.contains(&fingerprint));
        assert!(!details.contains(&wrong));
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
        let address = session.entry_url(&folder, &entry).unwrap();
        assert_eq!(
            address.path(),
            "/B%C3%BCro%20%231/Gr%C3%BC%C3%9Fe%20%25.txt"
        );
        assert_eq!(
            session.remote_path(&address).unwrap(),
            "/Büro #1/Grüße %.txt"
        );
        assert!(address.username().is_empty());
        assert!(address.password().is_none());
        use sha2::Digest;
        assert_eq!(
            session
                .sftp_sha256(&folder, &entry, &cancel, |_| {})
                .unwrap(),
            format!("{:x}", sha2::Sha256::digest(&content))
        );
        let method = session
            .prepare_checksum(&folder, Some(crate::ftp::ChecksumAlgorithm::Sha1), &cancel)
            .unwrap();
        let mut streamed = false;
        let (algorithm, digest) = session
            .remote_checksum(&folder, &entry, method, &cancel, |_| streamed = true)
            .unwrap();
        assert_eq!(algorithm, "SHA-1");
        assert_eq!(digest, format!("{:x}", sha1::Sha1::digest(&content)));
        assert!(!streamed);
        let stamp = "20260101123456";
        session
            .set_sftp_modified(&folder, &entry, stamp, &cancel)
            .unwrap();
        assert!(session.verify_sync_entry(&folder, &entry, &cancel).is_err());
        let mut entry = entry;
        entry.modified = Some(stamp.to_owned());
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
