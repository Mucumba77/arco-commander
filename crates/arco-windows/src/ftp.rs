//! FTP/FTPS transport using Windows' Schannel-backed curl installation.

use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tempfile::NamedTempFile;
pub use url::Url;
use zeroize::Zeroizing;

#[cfg(windows)]
mod credentials;
mod fxp;
mod listing;
mod resume;
mod sync;
pub use sync::SyncUpload;

const LIST_LIMIT: usize = 8 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Security {
    ExplicitTls,
    ImplicitTls,
    Plain,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ListingMode {
    #[default]
    Automatic,
    Mlsd,
    List,
}

pub struct Connection {
    url: Url,
    user: String,
    security: Security,
    proxy: Option<Url>,
    active: bool,
    listing_mode: ListingMode,
}

impl Connection {
    pub fn new(address: &str, user: &str, security: Security) -> io::Result<Self> {
        let wanted_scheme = if security == Security::ImplicitTls {
            "ftps"
        } else {
            "ftp"
        };
        let address = address.trim();
        if address.starts_with(['/', '\\']) || address.chars().any(char::is_control) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Ungültige FTP-Adresse.",
            ));
        }
        let mut url = if address.contains("://") {
            Url::parse(address)
        } else {
            Url::parse(&format!("{wanted_scheme}://{address}"))
        }
        .map_err(io::Error::other)?;
        if !matches!(url.scheme(), "ftp" | "ftps")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "expected an FTP URL without embedded credentials, query, or fragment",
            ));
        }
        if url.scheme() != wanted_scheme {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("use {wanted_scheme}:// for this mode"),
            ));
        }
        if !url.path().ends_with('/') {
            url.path_segments_mut()
                .map_err(|_| io::Error::other("invalid FTP directory"))?
                .push("");
        }
        let user = if user.is_empty() { "anonymous" } else { user };
        if user.contains(':') || user.chars().any(char::is_control) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid FTP user name",
            ));
        }
        Ok(Self {
            url,
            user: user.into(),
            security,
            proxy: None,
            active: false,
            listing_mode: ListingMode::Automatic,
        })
    }

    pub fn with_proxy(mut self, address: &str) -> io::Result<Self> {
        if address.trim().is_empty() {
            return Ok(self);
        }
        if self.active {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Aktives FTP erfordert eine Direktverbindung.",
            ));
        }
        let proxy = Url::parse(address.trim()).map_err(io::Error::other)?;
        if !matches!(
            proxy.scheme(),
            "http" | "https" | "socks4" | "socks4a" | "socks5" | "socks5h"
        ) || proxy.host_str().is_none()
            || !matches!(proxy.path(), "" | "/")
            || proxy.query().is_some()
            || proxy.fragment().is_some()
            || !proxy.username().is_empty()
            || proxy.password().is_some()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "erwartet eine HTTP-, HTTPS- oder SOCKS-Proxy-Adresse ohne Zugangsdaten",
            ));
        }
        self.proxy = Some(proxy);
        Ok(self)
    }

    pub fn with_active_mode(mut self, active: bool) -> io::Result<Self> {
        if active && self.proxy.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Aktives FTP erfordert eine Direktverbindung.",
            ));
        }
        self.active = active;
        Ok(self)
    }

    pub fn with_listing_mode(mut self, listing_mode: ListingMode) -> Self {
        self.listing_mode = listing_mode;
        self
    }

    pub fn authenticate(self, password: &str) -> io::Result<Session> {
        check_password(password)?;
        Ok(Session {
            connection: self,
            password: Zeroizing::new(password.to_string()),
        })
    }
}

fn check_password(password: &str) -> io::Result<()> {
    if password.chars().any(char::is_control) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid character in FTP password",
        ));
    }
    Ok(())
}

pub struct Session {
    connection: Connection,
    password: Zeroizing<String>,
}

pub struct Listing {
    pub entries: Vec<Entry>,
    pub format: ListFormat,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ListFormat {
    Mlsd,
    List,
    Names,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Entry {
    pub name: String,
    pub directory: Option<bool>,
    pub size: Option<u64>,
    pub modified: Option<String>,
    pub unique: Option<String>,
}

impl Session {
    pub fn initial_directory(&self) -> Url {
        self.connection.url.clone()
    }

    pub fn directory(&self, path: &str) -> io::Result<Url> {
        let mut url = self.connection.url.clone();
        let mut segments = url
            .path_segments_mut()
            .map_err(|_| io::Error::other("invalid FTP directory"))?;
        segments.clear();
        for segment in path
            .trim_matches('/')
            .split('/')
            .filter(|segment| !segment.is_empty())
        {
            check_segment(segment)?;
            segments.push(segment);
        }
        segments.push("");
        drop(segments);
        Ok(url)
    }

    pub fn list(&self, directory: &Url, cancel: &AtomicBool) -> io::Result<Listing> {
        self.validate(directory)?;
        let mode = self.connection.listing_mode;
        if mode != ListingMode::List {
            match run(
                self.command(directory)?,
                &["--request", "MLSD"],
                None,
                cancel,
            ) {
                Ok(bytes) => return parse_mlsd(&bytes),
                Err(error)
                    if mode == ListingMode::Automatic
                        && error.kind() == io::ErrorKind::Unsupported => {}
                Err(error) => return Err(error),
            }
        }
        match run(self.command(directory)?, &[], None, cancel) {
            Ok(bytes) => {
                if let Some(entries) = listing::parse_list(&bytes) {
                    return Ok(Listing {
                        entries,
                        format: ListFormat::List,
                    });
                }
            }
            Err(error)
                if mode == ListingMode::Automatic && error.kind() == io::ErrorKind::Unsupported => {
            }
            Err(error) => return Err(error),
        }
        if mode == ListingMode::List {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Unbekanntes FTP-LIST-Format.",
            ));
        }
        let bytes = run(self.command(directory)?, &["--list-only"], None, cancel)?;
        let text = String::from_utf8(bytes).map_err(io::Error::other)?;
        let entries = text
            .lines()
            .filter_map(|name| {
                check_segment(name).ok().map(|_| Entry {
                    name: name.into(),
                    directory: None,
                    size: None,
                    modified: None,
                    unique: None,
                })
            })
            .collect();
        Ok(Listing {
            entries,
            format: ListFormat::Names,
        })
    }

    pub fn download(
        &self,
        directory: &Url,
        remote: &str,
        destination: &Path,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        self.download_file(directory, remote, destination, None, cancel)
    }

    pub fn download_bounded(
        &self,
        directory: &Url,
        remote: &str,
        destination: &Path,
        limit: u64,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        self.download_file(directory, remote, destination, Some(limit), cancel)
    }

    fn download_file(
        &self,
        directory: &Url,
        remote: &str,
        destination: &Path,
        limit: Option<u64>,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        let url = self.file(directory, remote)?;
        let parent = destination
            .parent()
            .ok_or_else(|| io::Error::other("missing download directory"))?;
        if destination.try_exists()? {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                destination.display().to_string(),
            ));
        }
        let temporary = NamedTempFile::new_in(parent)?;
        let limit_argument = limit.map(|bytes| bytes.max(1).to_string());
        let arguments: &[&str] = if let Some(limit) = &limit_argument {
            &["--max-filesize", limit]
        } else {
            &[]
        };
        run(
            self.command(&url)?,
            arguments,
            Some(temporary.reopen()?),
            cancel,
        )?;
        if let Some(limit) = limit
            && temporary.as_file().metadata()?.len() > limit
        {
            return Err(io::Error::other(
                "Die FTP-Datei überschreitet das Vorschaugrößenlimit.",
            ));
        }
        temporary.as_file().sync_all()?;
        check_cancel(cancel)?;
        temporary
            .persist_noclobber(destination)
            .map_err(|error| error.error)?;
        Ok(())
    }

    pub fn upload(
        &self,
        directory: &Url,
        source: &Path,
        remote: &str,
        replace: bool,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        let url = self.file(directory, remote)?;
        if !replace
            && self
                .list(directory, cancel)?
                .entries
                .iter()
                .any(|entry| entry.name.eq_ignore_ascii_case(remote))
        {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "remote entry already exists",
            ));
        }
        let source = source
            .to_str()
            .ok_or_else(|| io::Error::other("curl requires a Unicode file path"))?;
        run(
            self.command(&url)?,
            &["--upload-file", source],
            None,
            cancel,
        )?;
        Ok(())
    }

    fn validate(&self, url: &Url) -> io::Result<()> {
        if url.scheme() != self.connection.url.scheme()
            || url.host_str() != self.connection.url.host_str()
            || url.port() != self.connection.url.port()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "FTP host differs from the active connection",
            ));
        }
        Ok(())
    }

    fn file(&self, directory: &Url, name: &str) -> io::Result<Url> {
        self.validate(directory)?;
        check_segment(name)?;
        let mut url = directory.clone();
        url.path_segments_mut()
            .map_err(|_| io::Error::other("invalid FTP directory"))?
            .pop_if_empty()
            .push(name);
        Ok(url)
    }

    fn command(&self, url: &Url) -> io::Result<Request> {
        self.validate(url)?;
        let root = std::env::var_os("SystemRoot")
            .ok_or_else(|| io::Error::other("SystemRoot is missing"))?;
        let mut command = Command::new(PathBuf::from(root).join("System32").join("curl.exe"));
        command.args([
            "--disable",
            "--config",
            "-",
            "--silent",
            "--show-error",
            "--globoff",
            "--proto",
            "=ftp,ftps",
            "--connect-timeout",
            "15",
            "--speed-time",
            "30",
            "--speed-limit",
            "1",
            "--ftp-skip-pasv-ip",
        ]);
        if let Some(proxy) = &self.connection.proxy {
            command.args(["--noproxy", "", "--proxy", proxy.as_str()]);
            if matches!(proxy.scheme(), "http" | "https") {
                command.arg("--proxytunnel");
            }
        } else {
            command.args(["--noproxy", "*"]);
        }
        if self.connection.active {
            command.args(["--ftp-port", "-"]);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        if self.connection.security != Security::Plain {
            command.arg("--ssl-reqd");
        }
        let credential = Zeroizing::new(format!(
            "{}:{}",
            self.connection.user,
            self.password.as_str()
        ));
        let config = Zeroizing::new(format!(
            "url = {}\nuser = {}\n",
            config_value(url.as_str()),
            config_value(&credential)
        ));
        Ok(Request { command, config })
    }
}

fn check_segment(segment: &str) -> io::Result<()> {
    if segment.is_empty()
        || matches!(segment, "." | "..")
        || segment.contains(['/', '\\'])
        || segment.chars().any(char::is_control)
    {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "expected a single FTP name",
        ))
    } else {
        Ok(())
    }
}

fn config_value(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

struct Request {
    command: Command,
    config: Zeroizing<String>,
}

fn run(
    mut request: Request,
    arguments: &[&str],
    output: Option<File>,
    cancel: &AtomicBool,
) -> io::Result<Vec<u8>> {
    check_cancel(cancel)?;
    let mut child = request
        .command
        .args(arguments)
        .stdin(Stdio::piped())
        .stdout(output.map_or_else(Stdio::piped, Stdio::from))
        .stderr(Stdio::piped())
        .spawn()?;
    let input = child
        .stdin
        .take()
        .ok_or_else(|| io::Error::other("curl stdin missing"));
    if let Err(error) = input.and_then(|mut stdin| stdin.write_all(request.config.as_bytes())) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(error);
    }
    let output = child
        .stdout
        .take()
        .map(|stdout| std::thread::spawn(move || read_limited(stdout, LIST_LIMIT)));
    let error = child
        .stderr
        .take()
        .map(|stderr| std::thread::spawn(move || read_limited(stderr, 16 * 1024)));
    let result = loop {
        if let Err(error) = check_cancel(cancel) {
            let _ = child.kill();
            let _ = child.wait();
            break Err(error);
        }
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) => std::thread::sleep(Duration::from_millis(40)),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                break Err(error);
            }
        }
    };
    let output = output
        .map(|reader| {
            reader
                .join()
                .map_err(|_| io::Error::other("FTP output reader failed"))
        })
        .transpose()?;
    let error = error
        .map(|reader| {
            reader
                .join()
                .map_err(|_| io::Error::other("FTP error reader failed"))
        })
        .transpose()?;
    let status = result?;
    if !status.success() {
        let kind = if matches!(status.code(), Some(19 | 21)) {
            io::ErrorKind::Unsupported
        } else {
            io::ErrorKind::Other
        };
        let error = error.transpose()?.unwrap_or_default();
        let error = String::from_utf8_lossy(&error);
        return Err(io::Error::new(
            kind,
            format!("FTP {}: {}", status.code().unwrap_or(-1), error.trim()),
        ));
    }
    Ok(output.transpose()?.unwrap_or_default())
}

fn read_limited(reader: impl Read, limit: usize) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader.take(limit as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(io::Error::other(
            "FTP listing or diagnostic exceeds the size limit",
        ));
    }
    Ok(bytes)
}

fn check_cancel(cancel: &AtomicBool) -> io::Result<()> {
    if cancel.load(Ordering::Relaxed) {
        Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"))
    } else {
        Ok(())
    }
}

fn parse_mlsd(bytes: &[u8]) -> io::Result<Listing> {
    parse_mlsd_with_policy(bytes, false)
}

fn parse_mlsd_with_policy(bytes: &[u8], strict: bool) -> io::Result<Listing> {
    let text = std::str::from_utf8(bytes).map_err(io::Error::other)?;
    let mut entries = Vec::new();
    for line in text.lines().filter(|line| !line.is_empty()) {
        let Some((facts, name)) = line.split_once(' ') else {
            return Err(io::Error::other("invalid MLSD listing"));
        };
        if strict && !facts.ends_with(';') {
            return Err(io::Error::other("Ungültige MLSD-Fakten."));
        }
        let mut directory = None;
        let mut size = None;
        let mut modified = None;
        let mut unique = None;
        let mut skip = false;
        let mut seen = strict.then(std::collections::BTreeSet::new);
        for fact in facts.split_terminator(';') {
            let Some((key, value)) = fact.split_once('=') else {
                if strict {
                    return Err(io::Error::other("Ungültiger MLSD-Eintrag."));
                }
                continue;
            };
            if seen
                .as_mut()
                .is_some_and(|keys| !keys.insert(key.to_ascii_lowercase()))
            {
                return Err(io::Error::other("Doppelte MLSD-Fakten."));
            }
            if key.eq_ignore_ascii_case("type") {
                match value.to_ascii_lowercase().as_str() {
                    "cdir" | "pdir" => skip = true,
                    "dir" => directory = Some(true),
                    "file" => directory = Some(false),
                    _ => (),
                }
            }
            if key.eq_ignore_ascii_case("size") {
                size = value.parse().ok();
            }
            if key.eq_ignore_ascii_case("modify")
                && value.len() >= 14
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || byte == b'.')
            {
                modified = Some(value.to_owned());
            }
            if key.eq_ignore_ascii_case("unique") && !value.is_empty() {
                unique = Some(value.to_owned());
            }
        }
        if skip {
            continue;
        }
        if strict {
            check_segment(name)?;
            if directory.is_none()
                || (directory == Some(false) && (size.is_none() || modified.is_none()))
            {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    format!(
                        "MLSD liefert für „{name}“ keinen unterstützten Typ, keine Größe oder kein Änderungsdatum."
                    ),
                ));
            }
        }
        if check_segment(name).is_ok() {
            entries.push(Entry {
                name: name.into(),
                directory,
                size,
                modified,
                unique,
            });
        }
    }
    Ok(Listing {
        entries,
        format: ListFormat::Mlsd,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sync_listing_rejects_ambiguous_metadata_and_incomplete_file_facts() {
        for listing in [
            "type=file;size=4; x.txt\r\n",
            "type=file;TYPE=dir;size=4;modify=20261101000000; x.txt\r\n",
            "type=file;size=4;modify=20261101000000; ../x.txt\r\n",
            "type=OS.unix=slink; x.txt\r\n",
        ] {
            assert!(parse_mlsd_with_policy(listing.as_bytes(), true).is_err());
        }
        let listing = parse_mlsd_with_policy(
            b"type=cdir; .\r\ntype=file;size=4;modify=20261101000000;unique=123; a b.txt\r\n",
            true,
        )
        .unwrap();
        assert_eq!(listing.entries.len(), 1);
        assert_eq!(listing.entries[0].name, "a b.txt");
        assert_eq!(listing.entries[0].unique.as_deref(), Some("123"));
    }

    #[test]
    fn mlsd_preserves_spaces_and_skips_virtual_parents() {
        let listing =
            parse_mlsd(b"type=cdir; .\r\nType=file;Size=12;  hello.txt\r\nType=dir; folder\r\n")
                .unwrap();
        assert_eq!(listing.entries.len(), 2);
        assert_eq!(listing.entries[0].name, " hello.txt");
        assert_eq!(listing.entries[0].size, Some(12));
        assert_eq!(listing.entries[1].directory, Some(true));
    }

    #[test]
    #[cfg(windows)]
    #[ignore = "requires a disposable writable local FTP server"]
    fn local_ftp_round_trip() {
        let address = std::env::var("ARCO_TEST_FTP").unwrap();
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source.txt");
        let target = directory.path().join("target.txt");
        std::fs::write(&source, b"Arco FTP round trip").unwrap();
        let cancel = AtomicBool::new(false);
        let server = address.strip_prefix("ftp://").unwrap();
        let active_session = Connection::new(server, "arco", Security::Plain)
            .unwrap()
            .with_active_mode(true)
            .unwrap()
            .authenticate("test-only")
            .unwrap();
        let session = Connection::new(&address, "arco", Security::Plain)
            .unwrap()
            .authenticate("test-only")
            .unwrap();
        let folder = session.initial_directory();
        active_session
            .upload(
                &folder,
                &source,
                "datei mit Leerzeichen.txt",
                false,
                &cancel,
            )
            .unwrap();
        assert!(
            session
                .list(&folder, &cancel)
                .unwrap()
                .entries
                .iter()
                .any(|entry| entry.name == "datei mit Leerzeichen.txt")
        );
        assert!(
            session
                .upload(
                    &folder,
                    &source,
                    "datei mit Leerzeichen.txt",
                    false,
                    &cancel
                )
                .is_err()
        );
        session
            .download(&folder, "datei mit Leerzeichen.txt", &target, &cancel)
            .unwrap();
        assert_eq!(
            std::fs::read(&target).unwrap(),
            std::fs::read(&source).unwrap()
        );
        let entry = session
            .list_sync(&folder, &cancel)
            .unwrap()
            .entries
            .into_iter()
            .find(|entry| entry.name == "datei mit Leerzeichen.txt")
            .unwrap();
        session
            .set_unix_permissions(&folder, &entry, 0o644, &cancel)
            .unwrap();
    }

    #[test]
    #[cfg(windows)]
    #[ignore = "requires a disposable local FTP server which rejects MLSD"]
    fn legacy_ftp_automatically_falls_back_to_list() {
        let address = std::env::var("ARCO_TEST_LEGACY_FTP").unwrap();
        let session = Connection::new(&address, "arco", Security::Plain)
            .unwrap()
            .authenticate("test-only")
            .unwrap();
        let listing = session
            .list(&session.initial_directory(), &AtomicBool::new(false))
            .unwrap();
        assert_eq!(listing.format, ListFormat::List);
        assert!(
            listing
                .entries
                .iter()
                .any(|entry| entry.name == "legacy folder" && entry.directory == Some(true))
        );
    }
}
