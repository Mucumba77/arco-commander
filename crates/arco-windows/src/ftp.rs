//! FTP/FTPS transport using Windows' Schannel-backed curl installation.

use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tempfile::NamedTempFile;
pub use url::Url;
use zeroize::Zeroizing;

mod checksums;
#[cfg(windows)]
mod credentials;
mod data;
#[cfg(windows)]
mod discovery;
#[cfg(windows)]
pub use discovery::{HttpProxy, detect_http_proxy};
mod forward;
mod fxp;
mod gateway;
mod http;
mod listing;
mod local_proxy;
mod native;
mod options;
mod otp;
mod proxy_template;
mod rate;
pub use rate::TransferLimit;
mod connections;
pub use connections::{ConnectionSlots, TransferConnections};
#[cfg(all(test, windows))]
mod reference;
mod resume;
mod s3;
mod s3_credentials;
mod sftp;
mod sync;
mod webdav;
mod webdav_locks;
pub use checksums::{ChecksumAlgorithm, RemoteChecksum};
pub use options::{
    AddressFamily, ConnectionOptions, PreferredTransferMode, TransferMode, TransferSettings,
};
#[cfg(windows)]
pub fn ssh_console_bridge() -> io::Result<()> {
    sftp::console_bridge()
}
pub use fxp::FxpCopy;
pub use s3::{S3Mfa, S3Tags, S3Version, S3Versioning, S3VersioningConfig};
pub use sftp::{CommandOutput, MAX_SSH_JUMPS, SshAgent, SshChallenge};
pub use sftp::{RemoteLinkKind, ResolvedLink};
pub use sync::SyncUpload;
pub use sync::UnixMetadata;
pub use webdav_locks::WebDavLockInfo;

const LIST_LIMIT: usize = 8 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Security {
    ExplicitTls,
    ImplicitTls,
    Plain,
    Sftp,
    Scp,
    WebDav,
    S3,
}

impl Security {
    pub fn is_ssh(self) -> bool {
        matches!(self, Self::Sftp | Self::Scp)
    }

    pub fn is_ftp(self) -> bool {
        matches!(self, Self::Plain | Self::ExplicitTls | Self::ImplicitTls)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Plain => "FTP",
            Self::ExplicitTls | Self::ImplicitTls => "FTPS",
            Self::Sftp => "SFTP",
            Self::Scp => "SCP",
            Self::WebDav => "WebDAV",
            Self::S3 => "S3",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ListingMode {
    #[default]
    Automatic,
    Mlsd,
    List,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DownloadPhase {
    Receiving,
    Verifying,
    Reconnecting,
}

pub struct DownloadProgress {
    pub received: u64,
    pub total: Option<u64>,
    pub phase: DownloadPhase,
}

pub struct Connection {
    url: Url,
    user: String,
    security: Security,
    recycle_remote: bool,
    recycle_overwrites: bool,
    proxy: Option<Url>,
    telnet_proxy: Option<proxy_template::Command>,
    local_proxy: Option<local_proxy::Command>,
    ssh_proxy_dns_locally: bool,
    proxy_user: String,
    ftp_gateway: arco_core::profile::FtpGateway,
    proxy_password: Zeroizing<String>,
    active: bool,
    listing_mode: ListingMode,
    options: options::Options,
    transfer: TransferSettings,
    ssh: Option<sftp::Options>,
    webdav_liberal_escaping: bool,
    webdav_windows_auth: bool,
    webdav_cross_domain_redirects: bool,
    webdav_unencrypted_redirects: bool,
    s3_region: String,
    s3_aws_profile: Option<String>,
    s3_role_arn: Option<String>,
    s3_session_token: Option<Zeroizing<String>>,
    s3_token_required: bool,
    s3_virtual_host: bool,
    s3_requester_pays: bool,
}

impl Connection {
    pub fn with_remote_recycle_bin(mut self, enabled: bool) -> io::Result<Self> {
        if enabled && self.security == Security::S3 {
            return Err(invalid_input(
                "S3 verwendet die Objektversionierung. Der Server-Papierkorb benötigt einen verschiebbaren Serverordner.",
            ));
        }
        self.recycle_remote = enabled;
        Ok(self)
    }

    pub fn with_recycled_overwrites(mut self, enabled: bool) -> io::Result<Self> {
        if enabled && self.security != Security::Sftp {
            return Err(invalid_input(
                "Das Sichern überschriebener Serverdateien benötigt SFTP.",
            ));
        }
        self.recycle_overwrites = enabled;
        Ok(self)
    }
    pub fn new(address: &str, user: &str, security: Security) -> io::Result<Self> {
        let wanted_scheme = match security {
            Security::ImplicitTls => "ftps",
            Security::Sftp => "sftp",
            Security::Scp => "scp",
            Security::WebDav | Security::S3 => "https",
            _ => "ftp",
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
        if url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Die Serveradresse darf keine Zugangsdaten, Abfrage oder Fragment enthalten.",
            ));
        }
        let http_endpoint = matches!(security, Security::WebDav | Security::S3);
        if url.scheme() != wanted_scheme && !(http_endpoint && url.scheme() == "http") {
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
        if security.is_ssh() && user.is_empty() {
            return Err(invalid_input("Gib einen SSH-Benutzernamen ein."));
        }
        let user = if security.is_ftp() && user.is_empty() {
            "anonymous"
        } else {
            user
        };
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
            recycle_remote: false,
            recycle_overwrites: false,
            proxy: None,
            telnet_proxy: None,
            local_proxy: None,
            ssh_proxy_dns_locally: false,
            proxy_user: String::new(),
            ftp_gateway: Default::default(),
            proxy_password: Zeroizing::default(),
            active: false,
            listing_mode: ListingMode::Automatic,
            options: Default::default(),
            transfer: TransferSettings::default(),
            ssh: None,
            webdav_liberal_escaping: false,
            webdav_windows_auth: false,
            webdav_cross_domain_redirects: false,
            webdav_unencrypted_redirects: false,
            s3_region: "us-east-1".into(),
            s3_aws_profile: None,
            s3_role_arn: None,
            s3_session_token: None,
            s3_token_required: false,
            s3_virtual_host: false,
            s3_requester_pays: false,
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
            "http"
                | "https"
                | "socks4"
                | "socks4a"
                | "socks5"
                | "socks5h"
                | "ftp"
                | "telnet"
                | "local"
        ) || proxy.host_str().is_none()
            || !matches!(proxy.path(), "" | "/")
            || proxy.query().is_some()
            || proxy.fragment().is_some()
            || !proxy.username().is_empty()
            || proxy.password().is_some()
            || (proxy.scheme() == "local"
                && (proxy.host_str() != Some("command") || proxy.port().is_some()))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Ungültige Proxy-Adresse oder Zugangsdaten in der Adresse.",
            ));
        }
        if self.proxy.as_ref() != Some(&proxy) {
            self.proxy_user.clear();
            self.proxy_password = Zeroizing::default();
            self.local_proxy = None;
            self.telnet_proxy = (proxy.scheme() == "telnet")
                .then(|| proxy_template::Command::telnet(""))
                .transpose()?;
        }
        self.proxy = Some(proxy);
        self.validate_options()?;
        Ok(self)
    }

    pub fn with_telnet_proxy_command(mut self, command: &str) -> io::Result<Self> {
        if let Some(configured) = &mut self.telnet_proxy {
            *configured = proxy_template::Command::telnet(command)?;
        } else if !command.is_empty() {
            return Err(invalid_input(
                "Ein Telnet-Proxy-Kommando benötigt eine telnet://-Adresse.",
            ));
        }
        Ok(self)
    }

    pub fn with_local_proxy_command(mut self, command: &str) -> io::Result<Self> {
        if self
            .proxy
            .as_ref()
            .is_some_and(|proxy| proxy.scheme() == "local")
        {
            self.local_proxy = Some(local_proxy::Command::parse(command)?);
        } else if !command.is_empty() {
            return Err(invalid_input("Aktiviere zuerst den lokalen SSH-Proxy."));
        }
        Ok(self)
    }

    pub fn with_ssh_proxy_dns_locally(mut self, local: bool) -> io::Result<Self> {
        if local && (!self.security.is_ssh() || self.proxy.is_none()) {
            return Err(invalid_input(
                "Diese DNS-Option benötigt eine SSH-Proxy-Verbindung.",
            ));
        }
        self.ssh_proxy_dns_locally = local;
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
        self.validate_options()?;
        Ok(self)
    }

    pub fn with_proxy_user(mut self, user: &str) -> io::Result<Self> {
        if !user.is_empty() {
            let proxy = self
                .proxy
                .as_ref()
                .ok_or_else(|| invalid_input("Trage zuerst eine Proxy-Adresse ein."))?;
            let supports_login = matches!(
                proxy.scheme(),
                "http" | "https" | "socks4" | "socks4a" | "socks5" | "socks5h" | "telnet" | "local"
            ) || (proxy.scheme() == "ftp"
                && self.ftp_gateway.supports_proxy_login());
            if !supports_login
                || (user.contains(':') && !matches!(proxy.scheme(), "local" | "telnet"))
                || user.chars().any(char::is_control)
                || (matches!(proxy.scheme(), "socks5" | "socks5h") && user.len() > 255)
            {
                return Err(invalid_input(
                    "Ungültiger Benutzername oder inkompatibler Proxy für die Anmeldung.",
                ));
            }
        }
        self.proxy_user = user.into();
        self.proxy_password = Zeroizing::default();
        Ok(self)
    }

    pub fn with_ftp_gateway(mut self, gateway: arco_core::profile::FtpGateway) -> io::Result<Self> {
        self.ftp_gateway = gateway;
        self.validate_options()?;
        Ok(self)
    }

    pub fn with_proxy_password(mut self, password: &str) -> io::Result<Self> {
        self.check_proxy_password(password)?;
        self.proxy_password = Zeroizing::new(password.to_owned());
        Ok(self)
    }

    fn check_proxy_password(&self, password: &str) -> io::Result<()> {
        check_password(password)?;
        if self.proxy_uses_user_id_only() {
            return Err(invalid_input("Der gewählte Proxy benötigt kein Passwort."));
        }
        if !self.proxy_has_credential_identity()
            || password.is_empty()
            || self.proxy.as_ref().is_some_and(|url| {
                matches!(url.scheme(), "socks5" | "socks5h") && password.len() > 255
            })
        {
            return Err(invalid_input(
                "Die Proxy-Anmeldung benötigt einen Benutzernamen und ein gültiges Passwort.",
            ));
        }
        Ok(())
    }

    pub fn proxy_uses_user_id_only(&self) -> bool {
        self.proxy
            .as_ref()
            .is_some_and(|url| matches!(url.scheme(), "socks4" | "socks4a"))
            || self
                .proxy_command_login()
                .is_some_and(|(_, password)| !password)
    }

    fn proxy_has_credential_identity(&self) -> bool {
        !self.proxy_user.is_empty()
            || self
                .proxy_command_login()
                .is_some_and(|(user, password)| password && !user)
    }

    fn proxy_command_login(&self) -> Option<(bool, bool)> {
        self.local_proxy
            .as_ref()
            .map(|command| (command.user, command.password))
            .or_else(|| {
                self.telnet_proxy
                    .as_ref()
                    .map(|command| (command.user, command.password))
            })
    }

    fn proxy_credentials(&self) -> Option<(&str, &str)> {
        (!self.proxy_user.is_empty()).then_some((&self.proxy_user, &self.proxy_password))
    }

    fn append_proxy_config(&self, config: &mut Zeroizing<String>) {
        if let Some((user, password)) = self.proxy_credentials() {
            let credentials = Zeroizing::new(format!("{user}:{password}"));
            let quoted = Zeroizing::new(config_value(&credentials));
            config.push_str("proxy-user = ");
            config.push_str(&quoted);
            config.push('\n');
        }
    }

    pub fn with_listing_mode(mut self, listing_mode: ListingMode) -> Self {
        self.listing_mode = listing_mode;
        self
    }

    pub fn with_options(mut self, options: ConnectionOptions) -> io::Result<Self> {
        self.options = options::Options::new(options)?;
        self.validate_options()?;
        Ok(self)
    }

    fn validate_options(&self) -> io::Result<()> {
        if self.options.http_forward
            && (self.security != Security::Plain
                || self.active
                || self.listing_mode != ListingMode::Automatic
                || self.options.mode_z
                || self.options.otp
                || self.options.show_hidden
                || self.options.list_pattern.is_some()
                || !self
                    .proxy
                    .as_ref()
                    .is_some_and(|proxy| matches!(proxy.scheme(), "http" | "https")))
        {
            return Err(invalid_input(
                "HTTP-Forward benötigt passives Klartext-FTP mit HTTP(S)-Proxy und automatischer Verzeichnisliste.",
            ));
        }
        if self.security.is_ssh()
            && (self.active
                || self.listing_mode != ListingMode::Automatic
                || self.options.ca_bundle.is_some()
                || self.options.client_certificate.is_some()
                || self.options.mode_z
                || self.options.otp
                || self.options.list_pattern.is_some()
                || self.options.show_hidden)
        {
            return Err(invalid_input(
                "SFTP verwendet eigene SSH-Verbindungsoptionen.",
            ));
        }
        if self.security.is_ssh()
            && self.proxy.as_ref().is_some_and(|url| {
                !matches!(
                    url.scheme(),
                    "http" | "socks4" | "socks4a" | "socks5" | "socks5h" | "telnet" | "local"
                )
            })
        {
            return Err(invalid_input(
                "SFTP und SCP unterstützen HTTP-CONNECT, SOCKS4, SOCKS5 sowie Telnet- und lokale Proxys.",
            ));
        }
        if self.security == Security::Plain
            && (self.options.client_certificate.is_some()
                || self.options.ca_bundle.is_some()
                    && !(self.options.http_forward
                        && self
                            .proxy
                            .as_ref()
                            .is_some_and(|proxy| proxy.scheme() == "https")))
        {
            return Err(invalid_input("Zertifikate benötigen eine FTPS-Verbindung."));
        }
        if self.security != Security::Plain
            && self.proxy.as_ref().is_some_and(|url| url.scheme() == "ftp")
        {
            return Err(invalid_input("Das FTP-Gateway benötigt Klartext-FTP."));
        }
        if !self.security.is_ssh()
            && self
                .proxy
                .as_ref()
                .is_some_and(|proxy| matches!(proxy.scheme(), "telnet" | "local"))
        {
            return Err(invalid_input(
                "Ein SSH-Proxy-Kommando benötigt SFTP oder SCP.",
            ));
        }
        if self.options.otp
            && (self.security != Security::Plain || self.active || self.proxy.is_some())
        {
            return Err(invalid_input(
                "OTP benötigt eine passive Klartext-Direktverbindung.",
            ));
        }
        if self.ftp_gateway != arco_core::profile::FtpGateway::default()
            && self
                .proxy
                .as_ref()
                .is_none_or(|proxy| proxy.scheme() != "ftp")
        {
            return Err(invalid_input(
                "Das FTP-Gateway benötigt eine Proxy-Adresse mit ftp://.",
            ));
        }
        Ok(())
    }

    pub fn authenticate(mut self, password: &str) -> io::Result<Session> {
        check_password(password)?;
        self.validate_options()?;
        if self
            .proxy
            .as_ref()
            .is_some_and(|proxy| proxy.scheme() == "local")
            && self.local_proxy.is_none()
        {
            return Err(invalid_input("Gib ein lokales SSH-Proxy-Kommando ein."));
        }
        let command_login = self.proxy_command_login();
        if let Some((user, password)) = command_login
            && (user && self.proxy_user.is_empty() || password && self.proxy_password.is_empty())
        {
            return Err(invalid_input(
                "Ergänze die Zugangsdaten für den SSH-Proxy im Netzwerkprofil.",
            ));
        }
        if (command_login.is_none()
            && !self.proxy_user.is_empty()
            && self.proxy_password.is_empty()
            && !self.proxy_uses_user_id_only())
            || (self.ftp_gateway.requires_proxy_login() && self.proxy_user.is_empty())
        {
            return Err(invalid_input(
                "Hinterlege das Proxy-Passwort in den Netzwerkprofilen.",
            ));
        }
        if self.security.is_ssh() && self.ssh.is_none() {
            return Err(invalid_input(
                "Für SFTP ist ein SSH-Hostschlüssel erforderlich.",
            ));
        }
        let preset = self.transfer.preset.take();
        if self.s3_token_required && self.s3_session_token.is_none() {
            return Err(invalid_input(
                "Für dieses S3-Profil fehlt das Sitzungstoken.",
            ));
        }
        let s3_token = self.s3_session_token.take().map(Arc::new);
        let mut session = Session {
            connection: Arc::new(self),
            password: Arc::new(Zeroizing::new(password.to_string())),
            otp_lock: Default::default(),
            ssh: Default::default(),
            scp: Default::default(),
            s3_token,
            aws_credentials: Default::default(),
            webdav_locks: Default::default(),
            webdav_aliases: Default::default(),
            transfer_override: None,
            transfer_limit: None,
            parallel_downloads: None,
            file_workers: 1,
            configured_transfer_preset: Arc::new(Mutex::new(preset)),
        };
        if session.security() == Security::S3 {
            session.load_s3_credentials()?;
        }
        Ok(session)
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

#[derive(Clone)]
pub struct Session {
    connection: Arc<Connection>,
    password: Arc<Zeroizing<String>>,
    otp_lock: Arc<Mutex<()>>,
    ssh: Arc<Mutex<Option<sftp::SftpConnection>>>,
    scp: Arc<Mutex<Option<sftp::SshConnection>>>,
    s3_token: Option<Arc<Zeroizing<String>>>,
    aws_credentials: Arc<Mutex<Option<s3_credentials::CachedCredentials>>>,
    webdav_locks: Arc<Mutex<std::collections::BTreeMap<Url, webdav_locks::WriteLock>>>,
    webdav_aliases: Arc<Mutex<std::collections::BTreeMap<Url, Url>>>,
    transfer_override: Option<TransferSettings>,
    transfer_limit: Option<TransferLimit>,
    parallel_downloads: Option<connections::ParallelDownloads>,
    file_workers: usize,
    configured_transfer_preset: Arc<Mutex<Option<String>>>,
}

#[derive(Debug)]
pub struct Listing {
    pub entries: Vec<Entry>,
    pub format: ListFormat,
}

impl Listing {
    pub fn regular_file(&self, name: &str) -> io::Result<&Entry> {
        let mut matching = self.entries.iter().filter(|entry| entry.name == name);
        let entry = matching.next().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "Die in der URL genannte Datei fehlt auf dem Server.",
            )
        })?;
        if matching.next().is_some() || entry.directory != Some(false) {
            return Err(invalid_input(
                "Die URL verweist nicht eindeutig auf eine reguläre Serverdatei.",
            ));
        }
        Ok(entry)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ListFormat {
    Mlsd,
    List,
    Names,
    HttpForward,
    Sftp,
    Scp,
    WebDav,
    S3,
}

impl ListFormat {
    pub fn structured(self) -> bool {
        matches!(
            self,
            Self::Mlsd | Self::Sftp | Self::Scp | Self::WebDav | Self::S3
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Entry {
    pub name: String,
    pub directory: Option<bool>,
    pub symlink: bool,
    pub size: Option<u64>,
    pub modified: Option<String>,
    pub unique: Option<String>,
}

impl Session {
    fn uses_native_ftp(&self) -> bool {
        self.connection.options.otp || self.connection.ftp_gateway != Default::default()
    }

    pub fn session_link_identity(&self) -> (&str, String) {
        (
            self.connection.user.as_str(),
            self.sftp_fingerprint().unwrap_or_default(),
        )
    }

    pub fn security(&self) -> Security {
        self.connection.security
    }

    pub fn is_sftp(&self) -> bool {
        self.security() == Security::Sftp
    }

    pub fn is_http_forward_ftp(&self) -> bool {
        self.connection.options.http_forward
    }

    pub fn initial_directory(&self) -> Url {
        self.connection.url.clone()
    }

    pub fn username(&self) -> &str {
        &self.connection.user
    }

    pub fn restore_directory(&self, address: Option<&str>) -> io::Result<Url> {
        let Some(address) = address else {
            return Ok(self.initial_directory());
        };
        let mut url = Url::parse(address).map_err(io::Error::other)?;
        self.validate(&url)?;
        if !url.path().ends_with('/') {
            url.path_segments_mut()
                .map_err(|_| io::Error::other("Ungültiger FTP-Pfad."))?
                .push("");
        }
        Ok(url)
    }

    pub fn directory(&self, path: &str) -> io::Result<Url> {
        let mut url = self.connection.url.clone();
        let mut segments = url
            .path_segments_mut()
            .map_err(|_| io::Error::other("invalid FTP directory"))?;
        if self.security() == Security::WebDav {
            segments.pop_if_empty();
        } else {
            segments.clear();
        }
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
        if self.is_http_forward_ftp() {
            return self.list_forward(directory, cancel);
        }
        match self.security() {
            Security::Sftp => return self.list_sftp(directory, false, cancel),
            Security::Scp => return self.list_scp(directory, cancel),
            Security::WebDav => return self.list_webdav(directory, cancel),
            Security::S3 => return self.list_s3(directory, cancel),
            _ => (),
        }
        let mode = self.connection.listing_mode;
        let settings = &self.connection.options;
        if mode != ListingMode::List
            && settings.list_pattern.is_none()
            && (mode != ListingMode::Automatic || !settings.show_hidden)
        {
            match run(
                self.command(directory)?,
                &["--request", "MLSD"],
                None,
                cancel,
            ) {
                Ok(bytes) => return self.parse_listing(&bytes, false),
                Err(error)
                    if mode == ListingMode::Automatic
                        && error.kind() == io::ErrorKind::Unsupported => {}
                Err(error) => return Err(error),
            }
        }
        match self.raw_listing_bytes(directory, cancel) {
            Ok(bytes) => {
                let decoded = self.connection.options.decode(&bytes)?;
                let entries = match &self.connection.options.list_pattern {
                    Some(pattern) => Some(listing::parse_custom(&decoded, pattern)?),
                    None => listing::parse_list(decoded.as_bytes()),
                };
                if let Some(entries) = entries {
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
        if mode == ListingMode::List || settings.list_pattern.is_some() || settings.show_hidden {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Unbekanntes FTP-LIST-Format.",
            ));
        }
        let bytes = run(self.command(directory)?, &["--list-only"], None, cancel)?;
        let text = self.connection.options.decode(&bytes)?;
        let entries = text
            .lines()
            .filter_map(|name| {
                check_segment(name).ok().map(|_| Entry {
                    name: name.into(),
                    directory: None,
                    symlink: false,
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

    fn parse_listing(&self, bytes: &[u8], strict: bool) -> io::Result<Listing> {
        parse_mlsd_with_policy(self.connection.options.decode(bytes)?.as_bytes(), strict)
    }

    pub fn download(
        &self,
        directory: &Url,
        remote: &str,
        destination: &Path,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        self.download_file(
            directory,
            remote,
            destination,
            TransferMode::Binary,
            None,
            cancel,
            |_| {},
        )
    }

    pub fn download_bounded(
        &self,
        directory: &Url,
        remote: &str,
        destination: &Path,
        limit: u64,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        self.download_file(
            directory,
            remote,
            destination,
            TransferMode::Binary,
            Some(limit),
            cancel,
            |_| {},
        )
    }

    pub fn download_bounded_with_progress(
        &self,
        directory: &Url,
        remote: &str,
        destination: &Path,
        limit: u64,
        cancel: &AtomicBool,
        progress: impl FnMut(DownloadProgress),
    ) -> io::Result<()> {
        self.download_file(
            directory,
            remote,
            destination,
            TransferMode::Binary,
            Some(limit),
            cancel,
            progress,
        )
    }

    pub fn download_with_progress(
        &self,
        directory: &Url,
        remote: &str,
        destination: &Path,
        cancel: &AtomicBool,
        progress: impl FnMut(DownloadProgress),
    ) -> io::Result<()> {
        self.download_file(
            directory,
            remote,
            destination,
            TransferMode::Binary,
            None,
            cancel,
            progress,
        )
    }

    pub fn download_preferred(
        &self,
        directory: &Url,
        remote: &str,
        destination: &Path,
        cancel: &AtomicBool,
        progress: impl FnMut(DownloadProgress),
    ) -> io::Result<()> {
        self.download_file(
            directory,
            remote,
            destination,
            self.preferred_transfer_mode(remote),
            None,
            cancel,
            progress,
        )
    }

    pub fn download_text(
        &self,
        directory: &Url,
        remote: &str,
        destination: &Path,
        cancel: &AtomicBool,
        progress: impl FnMut(DownloadProgress),
    ) -> io::Result<()> {
        self.download_file(
            directory,
            remote,
            destination,
            TransferMode::Text,
            None,
            cancel,
            progress,
        )
    }

    fn download_file(
        &self,
        directory: &Url,
        remote: &str,
        destination: &Path,
        mode: TransferMode,
        limit: Option<u64>,
        cancel: &AtomicBool,
        mut progress: impl FnMut(DownloadProgress),
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
        let expected = if limit.is_none() && self.preserve_transfer_time(false) {
            Some(
                self.list_sync(directory, cancel)?
                    .regular_file(remote)?
                    .clone(),
            )
        } else {
            None
        };
        let mut total = None;
        self.receive_file(&url, temporary.as_file(), mode, limit, cancel, |update| {
            let update = if let Some(update) = update {
                total = update.total;
                update
            } else {
                DownloadProgress {
                    received: temporary.as_file().metadata()?.len(),
                    total,
                    phase: DownloadPhase::Receiving,
                }
            };
            progress(update);
            Ok(())
        })?;
        progress(DownloadProgress {
            received: temporary.as_file().metadata()?.len(),
            total,
            phase: DownloadPhase::Verifying,
        });
        if let Some(limit) = limit
            && temporary.as_file().metadata()?.len() > limit
        {
            return Err(io::Error::other(
                "Die FTP-Datei überschreitet das Vorschaugrößenlimit.",
            ));
        }
        if let Some(expected) = expected {
            self.preserve_download_time(directory, &expected, temporary.as_file(), cancel)?;
        }
        temporary.as_file().sync_all()?;
        check_cancel(cancel)?;
        temporary
            .persist_noclobber(destination)
            .map_err(|error| error.error)?;
        Ok(())
    }

    fn preserve_download_time(
        &self,
        directory: &Url,
        expected: &Entry,
        output: &File,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        self.verify_sync_entry(directory, expected, cancel)?;
        let stamp = expected
            .modified
            .as_deref()
            .ok_or_else(|| invalid_input("Der Server liefert keine Änderungszeit."))?;
        let modified = chrono::NaiveDateTime::parse_from_str(stamp, "%Y%m%d%H%M%S%.f")
            .map_err(io::Error::other)?
            .and_utc();
        output.set_modified(modified.into())
    }

    pub fn upload(
        &self,
        directory: &Url,
        source: &Path,
        remote: &str,
        replace: bool,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        self.upload_with_mode(
            directory,
            source,
            remote,
            replace,
            TransferMode::Binary,
            cancel,
        )
    }

    pub fn upload_with_mode(
        &self,
        directory: &Url,
        source: &Path,
        remote: &str,
        replace: bool,
        mode: TransferMode,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        let url = self.file(directory, remote)?;
        if !self.security().is_ftp() {
            if mode == TransferMode::Text {
                return Err(sftp::binary_only());
            }
            return self.publish_remote_upload(directory, source, remote, replace, cancel);
        }
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
        let modified = if self.preserve_transfer_time(false) {
            let time = chrono::DateTime::<chrono::Utc>::from(source.metadata()?.modified()?);
            Some(time.format("%Y%m%d%H%M%S").to_string())
        } else {
            None
        };
        self.send_file(&url, source, mode, cancel)?;
        if let Some(stamp) = modified {
            self.set_ftp_upload_modified(directory, remote, &stamp, cancel)?;
        }
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
            || (self.security() == Security::WebDav
                && !url.path().starts_with(self.connection.url.path()))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "FTP host differs from the active connection",
            ));
        }
        Ok(())
    }

    pub fn entry_url(&self, directory: &Url, entry: &Entry) -> io::Result<Url> {
        let mut address = self.file(directory, &entry.name)?;
        if entry.directory == Some(true) {
            address
                .path_segments_mut()
                .map_err(|_| io::Error::other("invalid server directory"))?
                .push("");
        }
        Ok(address)
    }

    pub fn remote_path(&self, address: &Url) -> io::Result<String> {
        self.validate(address)?;
        percent_encoding::percent_decode_str(address.path())
            .decode_utf8()
            .map(|path| path.into_owned())
            .map_err(io::Error::other)
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

    fn command(&self, url: &Url) -> io::Result<Request<'_>> {
        self.validate(url)?;
        if self.is_http_forward_ftp() {
            return Err(forward::read_only());
        }
        if !self.security().is_ftp() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Dieser Befehl benötigt eine FTP- oder FTPS-Verbindung.",
            ));
        }
        let root = std::env::var_os("SystemRoot")
            .ok_or_else(|| io::Error::other("SystemRoot is missing"))?;
        let mut command = Command::new(PathBuf::from(root).join("System32").join("curl.exe"));
        if self.uses_native_ftp() {
            return Ok(Request {
                command,
                config: Zeroizing::default(),
                encoding: self.connection.options.encoding,
                native_ftp: Some(NativeRequest {
                    session: self,
                    url: url.clone(),
                    transfer: false,
                }),
                rate: None,
                protocol: "FTP",
            });
        }
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
        let settings = &self.connection.options;
        let mut wire_url = settings.encode_path(url)?;
        let mut user = self.connection.user.clone();
        let mut gateway_login = false;
        if let Some(gateway) = self
            .connection
            .proxy
            .as_ref()
            .filter(|url| url.scheme() == "ftp")
        {
            user.push('@');
            user.push_str(
                url.host_str()
                    .ok_or_else(|| invalid_input("FTP-Server fehlt."))?,
            );
            if let Some(port) = url.port() {
                use std::fmt::Write as _;
                write!(user, ":{port}").expect("format port");
            }
            gateway_login = true;
            wire_url
                .set_host(gateway.host_str())
                .map_err(io::Error::other)?;
            wire_url
                .set_port(gateway.port())
                .map_err(|()| invalid_input("Ungültiger Gateway-Port."))?;
            command.args(["--noproxy", "*"]);
        } else if let Some(proxy) = &self.connection.proxy {
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
        match settings.family {
            AddressFamily::Automatic => (),
            AddressFamily::Ipv4 => {
                command.arg("--ipv4");
            }
            AddressFamily::Ipv6 => {
                command.arg("--ipv6");
            }
        }
        if let Some(bundle) = &settings.ca_bundle {
            command.arg("--cacert").arg(bundle);
        }
        if let Some(thumbprint) = &settings.client_certificate {
            command.args(["--cert", &format!("CurrentUser\\MY\\{thumbprint}")]);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        if self.connection.security != Security::Plain {
            command.arg("--ssl-reqd");
        }
        let mut config = if gateway_login {
            // curl's user option splits at the first colon. A gateway USER
            // can contain a destination port, so use encoded URL userinfo
            // only in the private curl configuration pipe.
            use percent_encoding::{NON_ALPHANUMERIC, percent_encode};
            let (scheme, address) = wire_url.as_str().split_once("://").expect("FTP URL");
            Zeroizing::new(format!(
                "url = \"{scheme}://{}:{}@{address}\"\n",
                percent_encode(user.as_bytes(), NON_ALPHANUMERIC),
                percent_encode(self.password.as_bytes(), NON_ALPHANUMERIC)
            ))
        } else {
            let credential = Zeroizing::new(format!("{}:{}", user, self.password.as_str()));
            Zeroizing::new(format!(
                "url = {}\nuser = {}\n",
                config_value(wire_url.as_str()),
                config_value(&credential)
            ))
        };
        self.connection.append_proxy_config(&mut config);
        Ok(Request {
            command,
            config: Zeroizing::new(config.as_bytes().to_vec()),
            encoding: settings.encoding,
            native_ftp: None,
            rate: None,
            protocol: "FTP",
        })
    }

    fn transfer_command(&self, url: &Url) -> io::Result<Request<'_>> {
        let mut request = self.command(url)?;
        if let Some(native) = &mut request.native_ftp {
            native.transfer = true;
        } else {
            request.rate = Some(self.transfer_rate());
        }
        Ok(request)
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

struct Request<'a> {
    command: Command,
    config: Zeroizing<Vec<u8>>,
    encoding: &'static encoding_rs::Encoding,
    native_ftp: Option<NativeRequest<'a>>,
    rate: Option<rate::Rate>,
    protocol: &'static str,
}

struct NativeRequest<'a> {
    session: &'a Session,
    url: Url,
    transfer: bool,
}

fn run(
    request: Request<'_>,
    arguments: &[&str],
    output: Option<File>,
    cancel: &AtomicBool,
) -> io::Result<Vec<u8>> {
    run_observed(request, arguments, output, cancel, || Ok(()))
}

fn run_observed(
    mut request: Request<'_>,
    arguments: &[&str],
    output: Option<File>,
    cancel: &AtomicBool,
    mut observe: impl FnMut() -> io::Result<()>,
) -> io::Result<Vec<u8>> {
    check_cancel(cancel)?;
    if let Some(native) = request.native_ftp.take() {
        return native::execute(native, arguments, output, cancel, observe);
    }
    let mut arguments = arguments.iter();
    while let Some(argument) = arguments.next() {
        if *argument == "--quote" {
            let value = arguments
                .next()
                .ok_or_else(|| invalid_input("FTP-Befehl fehlt."))?;
            let quoted = config_value(value);
            let (encoded, _, errors) = request.encoding.encode(&quoted);
            if errors {
                return Err(invalid_input(
                    "Der FTP-Befehl ist im gewählten Zeichensatz nicht darstellbar.",
                ));
            }
            request.config.extend_from_slice(b"quote = ");
            request.config.extend_from_slice(&encoded);
            request.config.push(b'\n');
        } else {
            request.command.arg(argument);
        }
    }
    let streaming = output.is_some() && request.rate.as_ref().is_some_and(rate::Rate::adjustable);
    let (stdout, streamed_file) = if streaming {
        (Stdio::piped(), output)
    } else {
        if let Some(rate) = request.rate.as_ref().and_then(rate::Rate::bytes_per_second) {
            request.command.arg("--limit-rate").arg(rate.to_string());
        }
        (output.map_or_else(Stdio::piped, Stdio::from), None)
    };
    let mut child = request
        .command
        .stdin(Stdio::piped())
        .stdout(stdout)
        .stderr(Stdio::piped())
        .spawn()?;
    let input = child
        .stdin
        .take()
        .ok_or_else(|| io::Error::other("curl stdin missing"));
    if let Err(error) = input.and_then(|mut stdin| stdin.write_all(&request.config)) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(error);
    }
    let stop_reading = AtomicBool::new(false);
    let (status, output, error) = std::thread::scope(|scope| {
        let rate = request.rate.take();
        let stop = &stop_reading;
        let output = child.stdout.take().map(|stdout| {
            scope.spawn(move || match (streamed_file, rate) {
                (Some(file), Some(rate)) => rate::copy_output(stdout, file, rate, stop),
                _ => read_limited(stdout, LIST_LIMIT),
            })
        });
        let error = child
            .stderr
            .take()
            .map(|stderr| scope.spawn(move || read_limited(stderr, 16 * 1024)));
        let result: io::Result<_> = (|| {
            loop {
                check_cancel(cancel)?;
                observe()?;
                if let Some(status) = child.try_wait()?
                    && output.as_ref().is_none_or(|reader| reader.is_finished())
                {
                    return Ok(status);
                }
                std::thread::sleep(Duration::from_millis(40));
            }
        })();
        if result.is_err() {
            stop.store(true, std::sync::atomic::Ordering::Relaxed);
            let _ = child.kill();
            let _ = child.wait();
        }
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
        Ok::<_, io::Error>((result?, output, error))
    })?;
    if streaming && let Some(Err(error)) = &output {
        return Err(io::Error::new(error.kind(), error.to_string()));
    }
    if !status.success() {
        if matches!(status.code(), Some(51 | 58 | 60 | 77 | 90 | 91)) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "{}-Zertifikatsprüfung fehlgeschlagen (curl {}). Prüfe Servername, Vertrauenskette und Zertifikatsdatei.",
                    request.protocol,
                    status.code().unwrap()
                ),
            ));
        }
        let kind = match status.code() {
            Some(7) => io::ErrorKind::ConnectionRefused,
            Some(12 | 28) => io::ErrorKind::TimedOut,
            Some(18 | 52) => io::ErrorKind::UnexpectedEof,
            Some(19 | 21) => io::ErrorKind::Unsupported,
            Some(55 | 56) => io::ErrorKind::ConnectionReset,
            _ => io::ErrorKind::Other,
        };
        let error = error.transpose()?.unwrap_or_default();
        let error = String::from_utf8_lossy(&error);
        return Err(io::Error::new(
            kind,
            format!(
                "{} {}: {}",
                request.protocol,
                status.code().unwrap_or(-1),
                error.trim()
            ),
        ));
    }
    observe()?;
    check_cancel(cancel)?;
    Ok(output.transpose()?.unwrap_or_default())
}

fn invalid_input(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
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

#[cfg(test)]
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
                symlink: false,
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
    #[test]
    #[ignore = "requires Windows curl and opens a loopback socket"]
    fn reference_curl_authenticated_http_proxy() {
        use base64::Engine;
        use std::io::{BufRead as _, BufReader};
        use std::net::TcpListener;
        use std::time::Duration;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let proxy = format!("http://{}", listener.local_addr().unwrap());
        let peer = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + Duration::from_secs(15);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error)
                        if error.kind() == std::io::ErrorKind::WouldBlock
                            && std::time::Instant::now() < deadline =>
                    {
                        std::thread::sleep(Duration::from_millis(20));
                    }
                    Err(error) => panic!("{error}"),
                }
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut reader = BufReader::new(&mut stream);
            let mut request = String::new();
            reader.read_line(&mut request).unwrap();
            assert!(request.starts_with("PROPFIND http://proxy-fixture.invalid/dav/ HTTP/"));
            let auth =
                base64::engine::general_purpose::STANDARD.encode(br#"proxy-user:gate:"\only"#);
            let mut verified = false;
            loop {
                request.clear();
                reader.read_line(&mut request).unwrap();
                if matches!(request.as_str(), "" | "\r\n") {
                    break;
                }
                verified |= request
                    .strip_prefix("Proxy-Authorization: Basic ")
                    .is_some_and(|token| token.trim() == auth);
            }
            assert!(verified);
            let xml = r#"<D:multistatus xmlns:D="DAV:"><D:response><D:href>/dav/</D:href><D:propstat><D:prop><D:resourcetype><D:collection/></D:resourcetype></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response></D:multistatus>"#;
            write!(stream, "HTTP/1.1 207 Multi-Status\r\nContent-Type: application/xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{xml}", xml.len()).unwrap();
        });
        let session = super::Connection::new(
            "http://proxy-fixture.invalid/dav/",
            "",
            super::Security::WebDav,
        )
        .unwrap()
        .with_proxy(&proxy)
        .unwrap()
        .with_proxy_user("proxy-user")
        .unwrap()
        .with_proxy_password(r#"gate:"\only"#)
        .unwrap()
        .authenticate("")
        .unwrap();
        let result = session.list(
            &session.initial_directory(),
            &std::sync::atomic::AtomicBool::new(false),
        );
        peer.join().unwrap();
        assert!(result.unwrap().entries.is_empty());
    }

    #[test]
    fn proxy_password_stays_in_curls_private_config() {
        let session =
            super::Connection::new("ftp://example.test/", "login", super::Security::ExplicitTls)
                .unwrap()
                .with_proxy("http://proxy.test:8080")
                .unwrap()
                .with_proxy_user("proxy-account")
                .unwrap()
                .with_proxy_password(r#"secret:"\part"#)
                .unwrap()
                .authenticate("server-password")
                .unwrap();
        let request = session.command(&session.initial_directory()).unwrap();
        let config = std::str::from_utf8(&request.config).unwrap();
        assert!(config.contains(r#"proxy-user = "proxy-account:secret:\"\\part""#));
        assert!(
            request
                .command
                .get_args()
                .all(|arg| !arg.to_string_lossy().contains("secret"))
        );
    }

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
        let mut updates = Vec::new();
        session
            .download_with_progress(
                &folder,
                "datei mit Leerzeichen.txt",
                &target,
                &cancel,
                |p| {
                    updates.push((p.phase, p.received));
                },
            )
            .unwrap();
        assert!(updates.iter().any(|p| p.0 == DownloadPhase::Receiving));
        assert_eq!(updates.last(), Some(&(DownloadPhase::Verifying, 19)));
        let aborted = directory.path().join("cancelled.txt");
        let abort = AtomicBool::new(false);
        let error = session
            .download_with_progress(
                &folder,
                "datei mit Leerzeichen.txt",
                &aborted,
                &abort,
                |_| {
                    abort.store(true, Ordering::Relaxed);
                },
            )
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        assert!(!aborted.exists());
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 2);
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
