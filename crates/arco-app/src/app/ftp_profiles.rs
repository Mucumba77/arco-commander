use super::{App, Navigation, Outcome, strings};
use crate::FtpProfileData;
use crate::commands::{Dialog, Pending};
use crate::network_profile::{connection, connection_settings, jump_profiles, saved_secret};
use crate::panel::{FtpLocation, Location};
use arco_core::network_import::{self, KnownHosts};
use arco_core::profile::{
    self, FtpAddressFamily, FtpListing, FtpProfile, FtpSecurity, SshAgent, TcFtpSite,
};
use arco_core::session_url::{self, SessionLink};
use arco_windows::ftp::{Connection, Url};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use zeroize::Zeroizing;

#[derive(Clone, Copy)]
pub(crate) enum NetworkImport {
    WinScpIni,
    WinScpRegistry,
    Putty,
    Kitty,
    FileZilla,
    OpenSsh,
    KnownHosts,
    Ghisler,
}

impl NetworkImport {
    const OPTIONS: &[(Self, &str)] = &[
        (Self::WinScpIni, "WinSCP-INI auswählen"),
        (Self::WinScpRegistry, "WinSCP aus der Windows-Registrierung"),
        (Self::Putty, "PuTTY aus der Windows-Registrierung"),
        (Self::Kitty, "KiTTY aus der Windows-Registrierung"),
        (Self::FileZilla, "FileZilla-XML auswählen"),
        (Self::OpenSsh, "OpenSSH config auswählen"),
        (Self::KnownHosts, "OpenSSH known_hosts auswählen"),
        (Self::Ghisler, "Total Commander SFTP-INI auswählen"),
    ];
}

pub(crate) struct KnownHostProposal {
    name: String,
    address: String,
    previous: String,
    candidate: io::Result<Option<String>>,
}

struct JumpLogin {
    name: String,
    connection: Connection,
    passphrase: bool,
    secret: Option<Zeroizing<String>>,
}

pub(crate) struct SshProfileLogin {
    side: usize,
    name: String,
    profile: FtpProfile,
    connection: Connection,
    jumps: Vec<JumpLogin>,
    mode: FtpConnectMode,
}

pub enum FtpConnectMode {
    Panel,
    LinkedPanel(Option<String>),
    DownloadLink { file: String, target: PathBuf },
    Scp,
}

enum MountDestination {
    Profile,
    Link(Option<String>),
}

fn jump_names(profiles: &BTreeMap<String, FtpProfile>) -> impl Iterator<Item = &str> {
    profiles
        .iter()
        .filter(|(_, profile)| profile.security.is_ssh())
        .map(|(name, _)| name.as_str())
}

fn ssh_number(value: &str) -> io::Result<Option<u64>> {
    let value = value.trim();
    if value.is_empty() {
        Ok(None)
    } else {
        value
            .parse()
            .map(Some)
            .map_err(|_| invalid("Gib für SSH-Zeitangaben ganze Sekunden ein."))
    }
}

fn validate_name(name: &str) -> io::Result<()> {
    if name.is_empty() || name.chars().count() > 120 || name.chars().any(char::is_control) {
        return Err(invalid("Gib einen Profilnamen mit 1 bis 120 Zeichen ein."));
    }
    Ok(())
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn profile_from_tc(site: TcFtpSite) -> io::Result<FtpProfile> {
    if site.unsupported {
        return Err(invalid("Spezielle Server-, Firewall- oder Anmeldeoptionen"));
    }
    let (address, security) = if let Some((scheme, host)) = site.host.split_once("://") {
        let (scheme, security) = match scheme.to_ascii_lowercase().as_str() {
            "ftpes" | "ftps" => ("ftp", FtpSecurity::ExplicitTls),
            "ftpis" => ("ftps", FtpSecurity::ImplicitTls),
            "ftp" if site.tls => ("ftp", FtpSecurity::ExplicitTls),
            "ftp" => ("ftp", FtpSecurity::Plain),
            _ => return Err(invalid("Kein unterstütztes FTP-Protokoll")),
        };
        (format!("{scheme}://{host}"), security)
    } else {
        if site.host.is_empty() || site.host.starts_with(['\\', '/']) {
            return Err(invalid("Kein FTP-Hostname"));
        }
        let security = if site.tls {
            FtpSecurity::ExplicitTls
        } else {
            FtpSecurity::Plain
        };
        (format!("ftp://{}", site.host), security)
    };
    let mut url = Url::parse(&address).map_err(|_| invalid("Ungültiger FTP-Hostname"))?;
    if !site.directory.is_empty() {
        let mut path = url
            .path_segments_mut()
            .map_err(|_| invalid("Ungültiger Startordner"))?;
        path.clear();
        for part in site
            .directory
            .trim_matches('/')
            .split('/')
            .filter(|part| !part.is_empty())
        {
            if matches!(part, "." | "..") || part.chars().any(char::is_control) {
                return Err(invalid("Ungültiger FTP-Startordner"));
            }
            path.push(part);
        }
        path.push("");
    }
    if site
        .local_directory
        .as_ref()
        .is_some_and(|path| !path.is_absolute())
    {
        return Err(invalid(
            "Der lokale Startordner benötigt einen absoluten Pfad",
        ));
    }
    let profile = FtpProfile {
        address: url.into(),
        user: if site.anonymous {
            String::new()
        } else {
            site.user
        },
        security,
        active: !site.passive.unwrap_or(true),
        listing: site.listing,
        charset: site.charset,
        show_hidden: site.show_hidden,
        mode_z: site.mode_z,
        local_directory: site.local_directory,
        ..FtpProfile::default()
    };
    connection(&profile).map_err(|_| invalid("Ungültige FTP-Verbindungsdaten"))?;
    Ok(profile)
}

impl App {
    pub(crate) fn session_protocol_dialog(&mut self) {
        self.dialog(
            "confirm",
            Pending::SessionProtocol,
            Dialog {
                title: "Arco-Links in Windows",
                description: "Arco-Profil-Links mit arco:// über dieses Windows-Konto öffnen.",
                options: &[
                    crate::locale::tr("Registrieren"),
                    crate::locale::tr("Registrierung entfernen"),
                ],
                confirm_label: "Anwenden",
                ..Dialog::default()
            },
        );
    }

    pub(crate) fn update_session_protocol(&self, remove: bool) -> io::Result<()> {
        if remove {
            arco_windows::session_protocol::remove()?;
            self.status(crate::locale::tr("Arco-Link-Registrierung entfernt"));
        } else {
            let profile = std::path::absolute(&self.profile_path)?;
            arco_windows::session_protocol::install(&std::env::current_exe()?, &profile)?;
            self.status(crate::locale::tr("Arco-Links in Windows registriert"));
        }
        Ok(())
    }

    pub(crate) fn open_session_url(&mut self) {
        self.dialog("input", Pending::SessionUrl, Dialog {
            title: "Sitzungs-URL öffnen",
            description: "Füge die URL einer Datei, eines Ordners oder eines gespeicherten Arco-Profils ein.",
            placeholder: "Sitzungs-URL",
            confirm_label: "Prüfen",
            ..Dialog::default()
        });
    }

    pub(crate) fn prepare_session_url(&mut self, value: &str) -> io::Result<()> {
        let link = session_url::resolve(value, &self.profile.ftp_profiles)?;
        if link.connection.security.is_ssh() && link.connection.ssh_fingerprint.is_empty() {
            let address = link.connection.address.clone();
            self.dialog(
                "input",
                Pending::SessionUrlFingerprint(Box::new(link)),
                Dialog {
                    title: "SSH-Hostschlüssel prüfen",
                    description: &address,
                    placeholder: "Verifizierter SHA256-Fingerabdruck",
                    confirm_label: "Weiter",
                    ..Dialog::default()
                },
            );
        } else {
            self.confirm_session_url(link);
        }
        Ok(())
    }

    pub(crate) fn confirm_session_url(&mut self, link: SessionLink) {
        let mut details = link.connection.address.clone();
        let file = link.file.is_some();
        if let Some(name) = &link.file {
            let _ = write!(details, "\n{name}");
        }
        if !link.connection.ssh_fingerprint.is_empty() {
            let _ = write!(details, "\n{}", link.connection.ssh_fingerprint);
        }
        let file_actions = [
            crate::locale::tr("Datei im Dateifenster zeigen"),
            crate::locale::tr("Datei herunterladen"),
        ];
        self.dialog(
            "confirm",
            Pending::ConnectSessionUrl(Box::new(link)),
            Dialog {
                title: "Sitzungs-URL prüfen",
                description: &details,
                options: if file { &file_actions } else { &[] },
                confirm_label: if file { "Weiter" } else { "Verbinden" },
                ..Dialog::default()
            },
        );
    }

    pub(crate) fn connect_session_url(
        &mut self,
        link: SessionLink,
        download: bool,
    ) -> io::Result<()> {
        let mode = if download && let Some(file) = link.file.as_deref() {
            let initial = if arco_core::files::leaf_name(file.as_ref()).is_ok() {
                file
            } else {
                ""
            };
            let Some(target) =
                arco_windows::sfx::file(crate::locale::tr("Datei herunterladen"), true, initial)?
            else {
                return Ok(());
            };
            if !target.is_absolute() {
                return Err(invalid("Wähle einen absoluten Zielpfad für den Download."));
            }
            FtpConnectMode::DownloadLink {
                file: file.to_owned(),
                target,
            }
        } else {
            FtpConnectMode::LinkedPanel(link.file)
        };
        self.connect_ftp_profile(link.profile.unwrap_or_default(), link.connection, mode)
    }

    pub(crate) fn copy_session_url(&self, prefer_profile: bool) -> io::Result<()> {
        arco_windows::copy_text(&self.current_session_url(prefer_profile, false)?)
    }

    pub(crate) fn copy_file_url(&self, prefer_profile: bool) -> io::Result<()> {
        arco_windows::copy_text(&self.current_session_url(prefer_profile, true)?)
    }

    pub(crate) fn save_session_shortcut(&self, file: bool) -> io::Result<()> {
        let url = self.current_session_url(true, file)?;
        let title = if file {
            "Dateiverknüpfung speichern"
        } else {
            "Sitzungsverknüpfung speichern"
        };
        let Some(mut target) = arco_windows::sfx::file(
            crate::locale::tr(title),
            true,
            if file {
                "Arco-Datei.lnk"
            } else {
                "Arco-Sitzung.lnk"
            },
        )?
        else {
            return Ok(());
        };
        if target.extension().is_none() {
            target.set_extension("lnk");
        }
        let profile = std::path::absolute(&self.profile_path)?;
        let profile = profile
            .to_str()
            .filter(|path| !path.contains('"'))
            .ok_or_else(|| invalid("Der Pfad zur Arco-Profildatei ist ungültig."))?;
        if url.contains('"') {
            return Err(invalid(
                "Die Sitzungs-URL enthält ein ungültiges Anführungszeichen.",
            ));
        }
        let arguments = format!(r#"--profile "{profile}" --session-url "{url}""#);
        arco_windows::create_shortcut_with_arguments(
            &std::env::current_exe()?,
            &target,
            &arguments,
        )?;
        self.status(crate::locale::tr("Sitzungsverknüpfung gespeichert"));
        Ok(())
    }

    fn current_session_url(&self, prefer_profile: bool, file: bool) -> io::Result<String> {
        let tab = self.current();
        let remote = tab
            .ftp
            .as_ref()
            .ok_or_else(|| invalid("Öffne zuerst einen Serverordner."))?;
        let location = &remote.location;
        let target = if file {
            let paths = tab.selected_paths();
            let [path] = paths.as_slice() else {
                return Err(invalid("Wähle genau eine Serverdatei."));
            };
            let entry = remote
                .entry(path)
                .filter(|entry| entry.directory == Some(false))
                .ok_or_else(|| invalid("Wähle eine reguläre Serverdatei."))?;
            location.session.entry_url(&location.directory, entry)?
        } else {
            location.directory.clone()
        };
        let url = if prefer_profile
            && let Some(name) = location
                .profile
                .as_deref()
                .filter(|name| self.profile.ftp_profiles.contains_key(*name))
        {
            session_url::profile_link(name, &target)
        } else {
            let (user, pin) = location.session.session_link_identity();
            let security = match location.session.security() {
                arco_windows::ftp::Security::ExplicitTls => FtpSecurity::ExplicitTls,
                arco_windows::ftp::Security::ImplicitTls => FtpSecurity::ImplicitTls,
                arco_windows::ftp::Security::Plain => FtpSecurity::Plain,
                arco_windows::ftp::Security::Sftp => FtpSecurity::Sftp,
                arco_windows::ftp::Security::Scp => FtpSecurity::Scp,
                arco_windows::ftp::Security::WebDav => FtpSecurity::WebDav,
                arco_windows::ftp::Security::S3 => FtpSecurity::S3,
            };
            session_url::direct_link(&target, security, user, &pin)?
        };
        Ok(url)
    }

    pub(super) fn open_next_ssh_challenge(&mut self) {
        if matches!(&self.pending, Some(Pending::SshChallenge(challenge)) if challenge.expired()) {
            self.close_dialog();
        }
        let ui = self.ui();
        if !ui.get_dialog_kind().is_empty()
            || ui.get_settings_open()
            || ui.get_drive_side() >= 0
            || self.context_menu_open
        {
            return;
        }
        self.ssh_challenges.retain(|challenge| !challenge.expired());
        if let Some(challenge) = self.ssh_challenges.pop_front() {
            let description = format!(
                "{}\n{}\n{}",
                challenge.endpoint,
                challenge.instructions.trim(),
                challenge.prompt
            );
            let secret = !challenge.echo;
            self.dialog(
                "input",
                Pending::SshChallenge(challenge),
                Dialog {
                    title: "SSH-Anmeldung",
                    description: &description,
                    placeholder: "Antwort",
                    confirm_label: "Weiter",
                    secret,
                    ..Default::default()
                },
            );
        }
    }

    fn ssh_challenges(&self, connection: Connection) -> Connection {
        let sender = self.sender.clone();
        connection.on_ssh_challenge(move |challenge| {
            sender
                .send(super::Event::SshChallenge(challenge))
                .map_err(|_| io::ErrorKind::Interrupted.into())
        })
    }

    pub(crate) fn open_ftp_profiles(&mut self, selection: Option<&str>) {
        self.pending = None;
        let ui = self.ui();
        let index = selection
            .and_then(|name| {
                self.profile
                    .ftp_profiles
                    .keys()
                    .position(|item| item == name)
            })
            .map_or(i32::from(!self.profile.ftp_profiles.is_empty()), |index| {
                index as i32 + 1
            });
        ui.set_ftp_profile_names(strings(
            std::iter::once("Neues Profil")
                .chain(self.profile.ftp_profiles.keys().map(String::as_str)),
        ));
        ui.set_ftp_jump_profiles(strings(
            std::iter::once(crate::locale::tr("Direkte SSH-Verbindung"))
                .chain(jump_names(&self.profile.ftp_profiles)),
        ));
        ui.set_ftp_transfer_names(strings(
            std::iter::once(crate::locale::tr("Automatische Auswahl")).chain(
                self.profile
                    .network_transfer_presets
                    .iter()
                    .map(|preset| preset.name.as_str()),
            ),
        ));
        self.update_ftp_profile(index);
        ui.set_dialog_kind("ftp-profiles".into());
    }

    pub(super) fn select_ftp_profile(&mut self, index: i32) {
        if self.ui().get_dialog_kind() == "ftp-profiles" {
            self.update_ftp_profile(index);
        }
    }

    fn update_ftp_profile(&self, index: i32) {
        let ui = self.ui();
        let index = index.clamp(0, self.profile.ftp_profiles.len() as i32);
        let draft =
            self.ftp_profile_at(index)
                .map_or_else(FtpProfileData::default, |(name, profile)| FtpProfileData {
                    name: name.into(),
                    address: profile.address.as_str().into(),
                    user: profile.user.as_str().into(),
                    proxy: profile.proxy.as_str().into(),
                    local_directory: profile
                        .local_directory
                        .as_ref()
                        .map_or_else(Default::default, |path| crate::display_path(path).into()),
                    security: match profile.security {
                        FtpSecurity::ExplicitTls => 0,
                        FtpSecurity::ImplicitTls => 1,
                        FtpSecurity::Plain => 2,
                        FtpSecurity::Sftp => 3,
                        FtpSecurity::Scp => 4,
                        FtpSecurity::WebDav => 5,
                        FtpSecurity::S3 => 6,
                    },
                    active: profile.active,
                    recycle_remote: profile.recycle_remote,
                    recycle_overwrites: profile.recycle_overwrites,
                    transfer_mode: match profile.transfer.mode {
                        arco_core::profile::FtpTransferMode::Binary => 0,
                        arco_core::profile::FtpTransferMode::Text => 1,
                        arco_core::profile::FtpTransferMode::Automatic => 2,
                    },
                    transfer_speed: profile
                        .transfer
                        .speed_kib
                        .map(|rate| rate.to_string())
                        .unwrap_or_default()
                        .into(),
                    transfer_time: match profile.transfer.preserve_time {
                        None => 0,
                        Some(true) => 1,
                        Some(false) => 2,
                    },
                    transfer_mask: profile.transfer.masks.as_str().into(),
                    transfer_preset: self
                        .profile
                        .network_transfer_presets
                        .iter()
                        .position(|preset| {
                            profile.transfer_preset.as_deref() == Some(preset.name.as_str())
                        })
                        .map_or(0, |index| index as i32 + 1),
                    listing: match profile.listing {
                        FtpListing::Automatic => 0,
                        FtpListing::Mlsd => 1,
                        FtpListing::List => 2,
                    },
                    remember: profile.credential.is_some(),
                    address_family: match profile.address_family {
                        FtpAddressFamily::Automatic => 0,
                        FtpAddressFamily::Ipv4 => 1,
                        FtpAddressFamily::Ipv6 => 2,
                    },
                    charset: profile.charset.as_str().into(),
                    list_pattern: profile.list_pattern.as_str().into(),
                    show_hidden: profile.show_hidden,
                    ca_bundle: profile
                        .ca_bundle
                        .as_ref()
                        .map_or_else(Default::default, |path| crate::display_path(path).into()),
                    client_certificate: profile
                        .client_certificate
                        .as_deref()
                        .unwrap_or_default()
                        .into(),
                    mode_z: profile.mode_z,
                    otp: profile.otp,
                    ssh_fingerprint: profile.ssh_fingerprint.as_str().into(),
                    ssh_private_key: profile
                        .ssh_private_key
                        .as_ref()
                        .map_or_else(Default::default, |path| crate::display_path(path).into()),
                    ssh_agent: match profile.ssh_agent {
                        None => {
                            if profile.ssh_kerberos.is_some() {
                                3
                            } else {
                                0
                            }
                        }
                        Some(SshAgent::OpenSsh) => 1,
                        Some(SshAgent::Pageant) => 2,
                    },
                    ssh_kerberos_principal: profile
                        .ssh_kerberos
                        .as_deref()
                        .unwrap_or_default()
                        .into(),
                    ssh_kerberos_delegate: profile.ssh_kerberos_delegate,
                    ssh_kerberos_kex: profile.ssh_kerberos_kex,
                    ssh_interactive: profile.ssh_interactive,
                    ssh_jump: profile
                        .ssh_jump
                        .as_deref()
                        .and_then(|jump| {
                            jump_names(&self.profile.ftp_profiles).position(|name| name == jump)
                        })
                        .map_or(0, |index| index as i32 + 1),
                    ssh_timeout: profile
                        .ssh_timeout
                        .map(|time| time.to_string())
                        .unwrap_or_default()
                        .into(),
                    ssh_keepalive: profile
                        .ssh_keepalive
                        .map(|time| time.to_string())
                        .unwrap_or_default()
                        .into(),
                    s3_region: profile.s3_region.as_str().into(),
                    s3_aws_enabled: profile.s3_aws_profile.is_some(),
                    s3_aws_profile: profile.s3_aws_profile.as_deref().unwrap_or_default().into(),
                });
        ui.set_ftp_profile_index(index);
        ui.set_ftp_profile_data(draft);
        ui.set_ftp_profile_notice("".into());
        ui.set_ftp_profile_revision(ui.get_ftp_profile_revision().wrapping_add(1));
    }

    fn ftp_profile_at(&self, index: i32) -> Option<(&str, &FtpProfile)> {
        let index = usize::try_from(index).ok()?.checked_sub(1)?;
        self.profile
            .ftp_profiles
            .iter()
            .nth(index)
            .map(|(name, profile)| (name.as_str(), profile))
    }

    pub(super) fn operate_ftp_profile(
        &mut self,
        operation: &str,
        data: FtpProfileData,
        password: &str,
    ) {
        let ui = self.ui();
        if ui.get_dialog_kind() != "ftp-profiles" {
            return;
        }
        let selected = self
            .ftp_profile_at(ui.get_ftp_profile_index())
            .map(|(name, profile)| (name.to_owned(), profile.clone()));
        let result = match (operation, selected) {
            ("save", selected) => self.save_ftp_profile(selected, data, password),
            ("save-connect", selected) => {
                if self.job.is_some() {
                    Err(invalid("Ein Auftrag läuft bereits."))
                } else {
                    let name = data.name.trim().to_owned();
                    self.save_ftp_profile(selected, data, password)
                        .and_then(|()| self.connect_deferred_ftp(&name))
                }
            }
            ("connect", Some((name, profile))) => {
                self.connect_ftp_profile(name, profile, FtpConnectMode::Panel)
            }
            ("proxy-login", Some((name, profile))) if !profile.proxy.is_empty() => {
                self.dialog("input", Pending::FtpProxyLogin(name), Dialog {
                    title: "Proxy-Anmeldung",
                    description: "Hinterlege die Zugangsdaten für den Proxy dieses Profils im Windows-Anmeldeinformationsspeicher.",
                    value: &profile.proxy_user,
                    placeholder: "Proxy-Benutzername",
                    second_placeholder: "Proxy-Passwort",
                    second_secret: true,
                    options: &[crate::locale::tr("Speichern"), crate::locale::tr("Proxy-Anmeldung entfernen")],
                    confirm_label: "Anwenden",
                    ..Dialog::default()
                });
                Ok(())
            }
            ("scp", Some((name, profile))) if profile.security.is_ssh() => {
                self.connect_ftp_profile(name, profile, FtpConnectMode::Scp)
            }
            ("delete", Some((name, _))) => {
                let description =
                    format!("Das Profil „{name}“ und sein gespeichertes Passwort entfernen?");
                self.dialog(
                    "confirm",
                    Pending::FtpProfileDelete(name),
                    Dialog {
                        title: "FTP-Profil löschen",
                        description: &description,
                        ..Dialog::default()
                    },
                );
                Ok(())
            }
            ("import", _) => {
                self.dialog("input", Pending::FtpProfileImport, Dialog {
                    title: "Total-Commander-FTP-Profile importieren",
                    description: "Zuerst werden die erkannten Verbindungen in einer Vorschau angezeigt.",
                    placeholder: "Pfad zur wcx_ftp.ini",
                    ..Dialog::default()
                });
                Ok(())
            }
            _ => Ok(()),
        };
        if let Err(error) = result {
            ui.set_ftp_profile_notice(error.to_string().into());
        }
    }

    fn save_ftp_profile(
        &mut self,
        selected: Option<(String, FtpProfile)>,
        data: FtpProfileData,
        password: &str,
    ) -> io::Result<()> {
        let name = data.name.trim();
        validate_name(name)?;
        let mut profiles = self.profile.ftp_profiles.clone();
        if let Some((old, _)) = &selected {
            profiles.remove(old);
        }
        if profiles
            .keys()
            .any(|key| key.to_lowercase() == name.to_lowercase())
        {
            return Err(invalid("Dieser Profilname ist bereits vergeben."));
        }
        let local = data.local_directory.trim();
        let mut profile = FtpProfile {
            address: data.address.trim().into(),
            user: data.user.trim().into(),
            proxy: data.proxy.trim().into(),
            proxy_user: String::new(),
            proxy_credential: None,
            security: match data.security {
                1 => FtpSecurity::ImplicitTls,
                2 => FtpSecurity::Plain,
                3 => FtpSecurity::Sftp,
                4 => FtpSecurity::Scp,
                5 => FtpSecurity::WebDav,
                6 => FtpSecurity::S3,
                _ => FtpSecurity::ExplicitTls,
            },
            active: data.active,
            recycle_remote: data.recycle_remote && data.security != 6,
            recycle_overwrites: data.recycle_overwrites && data.security == 3,
            transfer: arco_core::profile::FtpTransfer {
                mode: match data.transfer_mode {
                    1 => arco_core::profile::FtpTransferMode::Text,
                    2 => arco_core::profile::FtpTransferMode::Automatic,
                    _ => arco_core::profile::FtpTransferMode::Binary,
                },
                speed_kib: if data.transfer_speed.trim().is_empty() {
                    None
                } else {
                    Some(
                        data.transfer_speed
                            .trim()
                            .parse()
                            .map_err(|_| invalid("Gib das Durchsatzlimit in ganzen KiB/s ein."))?,
                    )
                },
                preserve_time: match data.transfer_time {
                    1 => Some(true),
                    2 => Some(false),
                    _ => None,
                },
                masks: data.transfer_mask.trim().to_owned(),
            },
            transfer_preset: if data.transfer_preset > 0 {
                Some(
                    self.profile
                        .network_transfer_presets
                        .get(data.transfer_preset as usize - 1)
                        .ok_or_else(|| invalid("Die ausgewählte Transfervorgabe fehlt."))?
                        .name
                        .clone(),
                )
            } else {
                None
            },
            listing: match data.listing {
                1 => FtpListing::Mlsd,
                2 => FtpListing::List,
                _ => FtpListing::Automatic,
            },
            address_family: match data.address_family {
                1 => FtpAddressFamily::Ipv4,
                2 => FtpAddressFamily::Ipv6,
                _ => FtpAddressFamily::Automatic,
            },
            charset: data.charset.trim().into(),
            list_pattern: data.list_pattern.trim().into(),
            show_hidden: data.show_hidden,
            ca_bundle: (!data.ca_bundle.trim().is_empty())
                .then(|| PathBuf::from(data.ca_bundle.trim())),
            client_certificate: (!data.client_certificate.trim().is_empty())
                .then(|| data.client_certificate.trim().to_owned()),
            mode_z: data.mode_z,
            otp: data.otp,
            ssh_fingerprint: data.ssh_fingerprint.trim().into(),
            ssh_private_key: (!data.ssh_private_key.trim().is_empty())
                .then(|| PathBuf::from(data.ssh_private_key.trim())),
            ssh_agent: match data.ssh_agent {
                1 => Some(SshAgent::OpenSsh),
                2 => Some(SshAgent::Pageant),
                _ => None,
            },
            ssh_interactive: data.ssh_interactive,
            ssh_kerberos: (data.ssh_agent == 3)
                .then(|| data.ssh_kerberos_principal.trim().to_owned()),
            ssh_kerberos_delegate: data.ssh_agent == 3 && data.ssh_kerberos_delegate,
            ssh_kerberos_kex: data.ssh_agent == 3 && data.ssh_kerberos_kex,
            ssh_jump: if data.ssh_jump > 0 {
                Some(
                    jump_names(&self.profile.ftp_profiles)
                        .nth(data.ssh_jump as usize - 1)
                        .ok_or_else(|| invalid("Das ausgewählte SSH-Sprungprofil fehlt."))?
                        .to_owned(),
                )
            } else {
                None
            },
            ssh_timeout: ssh_number(&data.ssh_timeout)?,
            ssh_keepalive: ssh_number(&data.ssh_keepalive)?,
            s3_region: data.s3_region.trim().into(),
            s3_aws_profile: data
                .s3_aws_enabled
                .then(|| data.s3_aws_profile.trim().to_owned()),
            local_directory: (!local.is_empty()).then(|| PathBuf::from(local)),
            credential: None,
        };
        if let Some((_, old)) = &selected
            && old.proxy == profile.proxy
        {
            profile.proxy_user.clone_from(&old.proxy_user);
            profile.proxy_credential.clone_from(&old.proxy_credential);
        }
        if profile
            .local_directory
            .as_ref()
            .is_some_and(|path| !path.is_absolute())
        {
            return Err(invalid(
                "Gib einen absoluten Pfad zum lokalen Startordner ein.",
            ));
        }
        let new_connection = connection_settings(&profile)?;
        if let Some((old, _)) = &selected
            && old != name
        {
            for value in profiles.values_mut().chain(std::iter::once(&mut profile)) {
                if value.ssh_jump.as_deref() == Some(old) {
                    value.ssh_jump = Some(name.to_owned());
                }
            }
        }
        profiles.insert(name.to_owned(), profile.clone());
        for (name, profile) in &profiles {
            jump_profiles(name, profile, &profiles)?;
        }
        if !self.save_profile {
            return Err(io::Error::other(
                "Das Arco-Profil konnte beim Start nicht geladen werden.",
            ));
        }
        let previous = selected
            .as_ref()
            .and_then(|(_, previous)| previous.credential.as_ref().map(|id| (previous, id)));
        let mut staged = None;
        if data.remember
            && profile.ssh_agent.is_none()
            && profile.ssh_kerberos.is_none()
            && !(profile.security == FtpSecurity::S3 && profile.user.is_empty())
        {
            if password.is_empty() {
                let Some((old, id)) = previous else {
                    return Err(invalid(
                        "Gib ein Passwort ein oder deaktiviere das Speichern.",
                    ));
                };
                if !new_connection.same_credential_scope(&connection_settings(old)?) {
                    return Err(invalid(
                        "Gib für die geänderte Verbindung das Passwort erneut ein.",
                    ));
                }
                profile.credential = Some(id.clone());
            } else {
                let id = new_connection.store_password(password)?;
                profile.credential = Some(id.clone());
                staged = Some(id);
            }
        }
        profiles.insert(name.to_owned(), profile);
        if let Err(error) = self.commit_ftp_profiles(profiles) {
            if let Some(id) = staged
                && let Err(cleanup) = new_connection.delete_password(&id)
            {
                return Err(io::Error::other(format!(
                    "{error}. Windows-Passwort konnte nicht entfernt werden: {cleanup}"
                )));
            }
            return Err(error);
        }
        self.open_ftp_profiles(Some(name));
        let proxy_cleanup =
            self.remove_unused_proxy_password(selected.as_ref().map(|(_, old)| old));
        let cleanup = self.remove_unused_password(previous).and(proxy_cleanup);
        self.ui().set_ftp_profile_notice(match cleanup {
            Ok(()) => "Profil gespeichert.".into(),
            Err(error) => format!(
                "Profil gespeichert. Bisheriges Passwort konnte nicht entfernt werden: {error}"
            )
            .into(),
        });
        Ok(())
    }

    pub(crate) fn save_proxy_login(
        &mut self,
        name: &str,
        user: &str,
        password: &str,
        remove: bool,
    ) -> io::Result<()> {
        let mut profiles = self.profile.ftp_profiles.clone();
        let profile = profiles
            .get_mut(name)
            .filter(|p| !p.proxy.is_empty())
            .ok_or_else(|| invalid("Das Netzwerkprofil oder seine Proxy-Adresse fehlt."))?;
        let previous = profile.clone();
        let mut staged = None;
        if remove {
            profile.proxy_user.clear();
            profile.proxy_credential = None;
        } else {
            profile.proxy_user = user.trim().into();
            if profile.proxy_user != previous.proxy_user
                || profile.proxy_credential.is_none()
                || !password.is_empty()
            {
                let settings = connection_settings(profile)?;
                let id = settings.store_proxy_password(password)?;
                profile.proxy_credential = Some(id.clone());
                staged = Some((settings, id));
            }
        }
        if let Err(error) = self.commit_ftp_profiles(profiles) {
            if let Some((settings, id)) = staged
                && let Err(cleanup) = settings.delete_proxy_password(&id)
            {
                return Err(io::Error::other(format!(
                    "{error}. Proxy-Passwort konnte nicht entfernt werden: {cleanup}"
                )));
            }
            return Err(error);
        }
        self.open_ftp_profiles(Some(name));
        if let Err(error) = self.remove_unused_proxy_password(Some(&previous)) {
            self.ui().set_ftp_profile_notice(error.to_string().into());
        }
        Ok(())
    }

    fn connect_ftp_profile(
        &mut self,
        name: String,
        profile: FtpProfile,
        mode: FtpConnectMode,
    ) -> io::Result<()> {
        if self.job.is_some() || self.panels[self.active].loading {
            return Err(invalid(
                "Warte, bis der laufende Auftrag und das Dateifenster fertig sind.",
            ));
        }
        let jumps = jump_profiles(&name, &profile, &self.profile.ftp_profiles)?
            .into_iter()
            .map(|(name, profile)| {
                let connection = connection(profile)?;
                let secret = saved_secret(profile, &connection)?;
                Ok(JumpLogin {
                    name: name.to_owned(),
                    connection,
                    secret,
                    passphrase: profile.ssh_private_key.is_some(),
                })
            })
            .collect::<io::Result<Vec<_>>>()?;
        self.continue_ssh_profile(
            SshProfileLogin {
                side: self.active,
                name,
                connection: connection(&profile)?,
                profile,
                jumps,
                mode,
            },
            None,
        )
    }

    pub(crate) fn continue_ssh_profile(
        &mut self,
        mut login: SshProfileLogin,
        secret: Option<&str>,
    ) -> io::Result<()> {
        if let Some(secret) = secret {
            let jump = login
                .jumps
                .iter_mut()
                .find(|hop| hop.secret.is_none())
                .ok_or_else(|| invalid("Die SSH-Anmeldung ist bereits vollständig."))?;
            jump.secret = Some(Zeroizing::new(secret.to_owned()));
        }
        if let Some(jump) = login.jumps.iter().find(|hop| hop.secret.is_none()) {
            let description = jump.name.clone();
            let placeholder = if jump.passphrase {
                "Schlüsselpassphrase (optional)"
            } else {
                "Passwort"
            };
            self.dialog(
                "input",
                Pending::SshJumpPassword(Box::new(login)),
                Dialog {
                    title: "SSH-Sprungserver anmelden",
                    description: &description,
                    placeholder,
                    secret: true,
                    confirm_label: "Weiter",
                    ..Default::default()
                },
            );
            return Ok(());
        }
        let SshProfileLogin {
            side,
            name,
            profile,
            connection,
            jumps,
            mode,
        } = login;
        let connection = if jumps.is_empty() {
            connection
        } else {
            connection.with_ssh_jumps(
                jumps
                    .into_iter()
                    .map(|hop| {
                        (
                            hop.connection,
                            hop.secret.expect("all jump credentials collected"),
                        )
                    })
                    .collect(),
            )?
        };
        let password = saved_secret(&profile, &connection)?;
        if profile.security != FtpSecurity::Plain
            && let Some(password) = password
        {
            self.close_dialog();
            return self.connect_ftp_secret(
                side,
                connection,
                &password,
                profile.local_directory,
                name,
                mode,
            );
        }
        self.dialog("input", Pending::FtpProfilePassword(side, connection, profile.local_directory, name, mode), Dialog {
            title: match profile.security {
                FtpSecurity::Sftp | FtpSecurity::Scp => "SSH-Anmeldung",
                FtpSecurity::WebDav => "WebDAV-Anmeldung",
                FtpSecurity::S3 => "S3-Anmeldung",
                _ => "FTP-Anmeldung",
            },
            description: if profile.ssh_private_key.is_some() {
                "Gib die Passphrase der SSH-Schlüsseldatei ein. Ist sie unverschlüsselt, lass das Feld leer."
            } else if profile.security.is_ssh() {
                "Gib das Passwort für diese SSH-Verbindung ein."
            } else if profile.security == FtpSecurity::S3 {
                "Gib den geheimen S3-Zugriffsschlüssel ein."
            } else if profile.otp {
                "Gib die OTP-Passphrase ein. Der Server erhält nur den berechneten Einmalcode."
            } else if profile.security == FtpSecurity::Plain {
                "Klartext-FTP überträgt Anmeldung und Dateien unverschlüsselt."
            } else if profile.credential.is_some() {
                "Das gespeicherte Passwort ist auf diesem Windows-Konto nicht verfügbar."
            } else {
                "Gib das Passwort für diese Verbindung ein. Bei anonymen Servern kann es leer bleiben."
            },
            placeholder: if profile.security == FtpSecurity::S3 { "Geheimer Zugriffsschlüssel" } else { "Passwort" },
            value: password.as_deref().map_or("", String::as_str),
            secret: true,
            ..Dialog::default()
        });
        Ok(())
    }

    pub(super) fn connect_deferred_ftp(&mut self, name: &str) -> io::Result<()> {
        let profile = self
            .profile
            .ftp_profiles
            .get(name)
            .cloned()
            .ok_or_else(|| {
                io::Error::other(format!(
                    "FTP-Profil „{name}“ fehlt. Lege es an oder importiere es zuerst."
                ))
            })?;
        self.connect_ftp_profile(name.to_owned(), profile, FtpConnectMode::Panel)
    }

    pub(crate) fn connect_ftp_secret(
        &mut self,
        side: usize,
        connection: Connection,
        password: &str,
        local: Option<PathBuf>,
        profile: String,
        mode: FtpConnectMode,
    ) -> io::Result<()> {
        match mode {
            FtpConnectMode::Panel => {
                self.mount_ftp(side, connection, password, local, Some(profile))
            }
            FtpConnectMode::LinkedPanel(file) => self.mount_ftp_inner(
                side,
                connection,
                password,
                local,
                (!profile.is_empty()).then_some(profile),
                MountDestination::Link(file),
            ),
            FtpConnectMode::DownloadLink { file, target } => {
                let session = Arc::new(self.ssh_challenges(connection).authenticate(password)?);
                let directory = session.initial_directory();
                let session = arco_app::network_transfer::resolve(
                    &self.profile.network_transfer_presets,
                    &session,
                    &directory,
                    target.parent(),
                    None,
                )?;
                arco_app::network_sync::check_transfer_mask(&session, &file)?;
                let source = format!("{directory}{file}");
                let description = super::JobDescription::new(
                    crate::OperationKind::Ftp,
                    source.clone(),
                    crate::display_path(&target),
                );
                self.enqueue_virtual_operation(
                    "Datei herunterladen",
                    true,
                    description,
                    move |job| {
                        let list = session.list_sync(&directory, &job.cancel)?;
                        let entry = list.regular_file(&file)?;
                        session.download_preferred(
                            &directory,
                            &file,
                            &target,
                            &job.cancel,
                            job.ftp_download_progress(source, entry.size),
                        )?;
                        Ok(Outcome::Report(
                            format!("Download · {}", crate::display_path(&target)),
                            arco_core::files::TransferReport {
                                completed: 1,
                                ..Default::default()
                            },
                        ))
                    },
                );
                Ok(())
            }
            FtpConnectMode::Scp => {
                let session = Arc::new(self.ssh_challenges(connection).authenticate(password)?);
                let folder = local.unwrap_or_else(|| self.panels[side].current().path.clone());
                self.dialog(
                    "input",
                    Pending::ScpTransfer(session, folder),
                    Dialog {
                        title: "SCP-Dateitransfer",
                        description: "Überträgt eine Datei über das ausgewählte SSH-Profil.",
                        placeholder: "Serverpfad",
                        second_placeholder: "Lokaler Dateiname oder absoluter Pfad",
                        options: &[
                            crate::locale::tr("Vom Server herunterladen"),
                            crate::locale::tr("Auf den Server hochladen"),
                        ],
                        confirm_label: "Übertragen",
                        ..Dialog::default()
                    },
                );
                Ok(())
            }
        }
    }

    pub(crate) fn mount_ftp(
        &mut self,
        side: usize,
        connection: Connection,
        password: &str,
        local: Option<PathBuf>,
        profile: Option<String>,
    ) -> io::Result<()> {
        self.mount_ftp_inner(
            side,
            connection,
            password,
            local,
            profile,
            MountDestination::Profile,
        )
    }

    fn mount_ftp_inner(
        &mut self,
        side: usize,
        connection: Connection,
        password: &str,
        local: Option<PathBuf>,
        profile: Option<String>,
        destination: MountDestination,
    ) -> io::Result<()> {
        if self.job.is_some() || self.panels[side].loading {
            return Err(invalid(
                "Warte, bis der laufende Auftrag und das Dateifenster fertig sind.",
            ));
        }
        let session = Arc::new(self.ssh_challenges(connection).authenticate(password)?);
        let job_title = format!("{}-Verbindung herstellen", session.security().label());
        let deferred = self.panels[side].current().deferred.as_ref();
        let (directory, navigation) = match deferred {
            Some(arco_core::profile::VirtualTab::Ftp {
                profile: saved,
                url,
            }) if matches!(destination, MountDestination::Profile)
                && profile.as_deref() == Some(saved.as_str()) =>
            {
                (
                    session.restore_directory(url.as_deref())?,
                    Navigation::Refresh,
                )
            }
            _ => (
                session.initial_directory(),
                local.map_or(Navigation::Jump, |local| Navigation::FtpProfile {
                    local,
                    opposite_request: self.panels[1 - side].request,
                }),
            ),
        };
        let location = Location::Ftp(FtpLocation {
            session,
            directory,
            profile,
        });
        let request = self.panels[side].request;
        let description = super::JobDescription::new(
            crate::OperationKind::Ftp,
            location.display(),
            String::new(),
        );
        self.start_operation_job(&job_title, false, description, move |job| {
            job.phase("Anmelden und Verzeichnis laden …");
            let Location::Ftp(ftp) = &location else {
                unreachable!()
            };
            let directory = super::ftp_panel::read_panel(ftp, &job.cancel)?;
            let navigation = if let MountDestination::Link(Some(file)) = destination {
                directory
                    .ftp
                    .as_ref()
                    .expect("FTP listing")
                    .listing
                    .regular_file(&file)?;
                let path = PathBuf::from(file);
                if !directory
                    .directory
                    .entries
                    .iter()
                    .any(|entry| entry.path == path)
                {
                    return Err(invalid(
                        "Der Dateiname kann im Windows-Dateifenster nicht dargestellt werden.",
                    ));
                }
                Navigation::Focus(path)
            } else {
                navigation
            };
            arco_core::files::check_cancel(&job.cancel)?;
            Ok(Outcome::FtpMounted {
                side,
                request,
                location,
                navigation,
                directory: Box::new(directory),
            })
        });
        Ok(())
    }

    pub(crate) fn delete_ftp_profile(&mut self, name: &str) -> io::Result<()> {
        let mut profiles = self.profile.ftp_profiles.clone();
        if profiles
            .values()
            .any(|profile| profile.ssh_jump.as_deref() == Some(name))
        {
            return Err(invalid(
                "Das Profil wird als SSH-Sprungserver verwendet. Passe zuerst die abhängigen Profile an.",
            ));
        }
        let previous = profiles.remove(name);
        self.commit_ftp_profiles(profiles)?;
        self.open_ftp_profiles(None);
        let credential = previous
            .as_ref()
            .and_then(|profile| profile.credential.as_ref().map(|id| (profile, id)));
        let proxy_cleanup = self.remove_unused_proxy_password(previous.as_ref());
        self.ui().set_ftp_profile_notice(
            match self.remove_unused_password(credential).and(proxy_cleanup) {
                Ok(()) => "Profil gelöscht.".into(),
                Err(error) => format!(
                    "Profil gelöscht. Windows-Passwort konnte nicht entfernt werden: {error}"
                )
                .into(),
            },
        );
        Ok(())
    }

    fn remove_unused_password(&self, previous: Option<(&FtpProfile, &String)>) -> io::Result<()> {
        if let Some((old, id)) = previous
            && !self
                .profile
                .ftp_profiles
                .values()
                .any(|profile| profile.credential.as_ref() == Some(id))
        {
            connection_settings(old)?.delete_password(id)?;
        }
        Ok(())
    }

    fn remove_unused_proxy_password(&self, previous: Option<&FtpProfile>) -> io::Result<()> {
        if let Some(old) = previous
            && let Some(id) = &old.proxy_credential
            && !self
                .profile
                .ftp_profiles
                .values()
                .any(|profile| profile.proxy_credential.as_ref() == Some(id))
        {
            connection_settings(old)?.delete_proxy_password(id)?;
        }
        Ok(())
    }

    fn commit_ftp_profiles(&mut self, profiles: BTreeMap<String, FtpProfile>) -> io::Result<()> {
        if !self.save_profile {
            return Err(io::Error::other(
                "Das Arco-Profil konnte beim Start nicht geladen werden.",
            ));
        }
        let mut updated = self.profile_snapshot();
        updated.ftp_profiles = profiles;
        updated.save(&self.profile_path)?;
        self.profile = updated;
        Ok(())
    }

    pub(crate) fn import_ftp_profiles(&mut self, path: &str) -> io::Result<()> {
        let path = PathBuf::from(path.trim_matches('"'));
        if !path.is_absolute() {
            return Err(invalid("Gib den absoluten Pfad zur wcx_ftp.ini ein."));
        }
        let description = super::JobDescription::new(
            crate::OperationKind::Ftp,
            crate::display_path(&path),
            String::new(),
        );
        self.start_operation_job("FTP-Profile lesen", false, description, move |job| {
            job.phase("FTP-Verbindungsprofile auswerten …");
            arco_core::files::check_cancel(&job.cancel)?;
            let profiles = profile::read_tc_ftp_ini(&path)?;
            arco_core::files::check_cancel(&job.cancel)?;
            Ok(Outcome::ImportedFtp(profiles))
        });
        Ok(())
    }

    pub(super) fn review_ftp_import(&mut self, sites: Vec<(String, TcFtpSite)>) {
        self.review_network_import(
            sites
                .into_iter()
                .map(|(name, site)| (name, profile_from_tc(site)))
                .collect(),
            "FTP-Profilimport prüfen",
        );
    }

    pub(crate) fn open_network_import(&mut self) {
        let options = NetworkImport::OPTIONS
            .iter()
            .map(|(_, label)| crate::locale::tr(label))
            .collect::<Vec<_>>();
        self.dialog("confirm", Pending::NetworkImportSource, Dialog {
            title: "Verbindungsprofile importieren",
            description: "Wähle die Quelle für Profile oder SSH-Hostschlüssel. Arco zeigt die Änderungen vor dem Speichern.",
            options: &options,
            options_label: "Importquelle",
            confirm_label: "Weiter",
            ..Dialog::default()
        });
    }

    pub(crate) fn choose_network_import(&mut self, option: usize) -> io::Result<()> {
        let &(source, title) = NetworkImport::OPTIONS
            .get(option)
            .ok_or_else(|| invalid("Wähle eine Importquelle."))?;
        if matches!(
            source,
            NetworkImport::WinScpRegistry | NetworkImport::Putty | NetworkImport::Kitty
        ) {
            return self.import_network_registry(source, title);
        }
        #[cfg(windows)]
        match arco_windows::sfx::file(crate::locale::tr(title), false, "") {
            Ok(Some(path)) => return self.import_network_file(source, path),
            Ok(None) => return Ok(()),
            Err(_) => (),
        }
        self.dialog(
            "input",
            Pending::NetworkImportPath(source),
            Dialog {
                title,
                placeholder: "Pfad zur Importdatei",
                confirm_label: "Vorschau",
                ..Dialog::default()
            },
        );
        Ok(())
    }

    fn import_network_registry(
        &mut self,
        source: NetworkImport,
        label: &'static str,
    ) -> io::Result<()> {
        #[cfg(windows)]
        {
            use arco_windows::session_registry::{self, Source};
            let (source, fields, win_scp) = match source {
                NetworkImport::WinScpRegistry => {
                    (Source::WinScp, network_import::WINSCP_FIELDS, true)
                }
                NetworkImport::Putty => (Source::Putty, network_import::PUTTY_FIELDS, false),
                NetworkImport::Kitty => (Source::Kitty, network_import::PUTTY_FIELDS, false),
                _ => return Err(invalid("Ungültige Registry-Importquelle.")),
            };
            let description = super::JobDescription::new(
                crate::OperationKind::Ftp,
                label.to_owned(),
                String::new(),
            );
            self.start_operation_job(
                "Verbindungsprofile importieren",
                false,
                description,
                move |job| {
                    arco_core::files::check_cancel(&job.cancel)?;
                    let records =
                        session_registry::read(source, fields, network_import::SESSION_LIMIT)?;
                    let sites =
                        network_import::registry_sites(records.profiles, &records.keys, win_scp)?;
                    arco_core::files::check_cancel(&job.cancel)?;
                    Ok(Outcome::ImportedNetwork(sites))
                },
            );
            Ok(())
        }
        #[cfg(not(windows))]
        {
            let _ = (source, label);
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Diese Importquelle benötigt Windows.",
            ))
        }
    }

    pub(crate) fn import_network_file(
        &mut self,
        source: NetworkImport,
        path: PathBuf,
    ) -> io::Result<()> {
        if !path.is_absolute() {
            return Err(invalid("Gib einen absoluten Pfad zur Importdatei ein."));
        }
        let description = super::JobDescription::new(
            crate::OperationKind::Ftp,
            crate::display_path(&path),
            String::new(),
        );
        let ssh_profiles = if matches!(source, NetworkImport::KnownHosts) {
            self.profile
                .ftp_profiles
                .iter()
                .filter(|(_, profile)| profile.security.is_ssh())
                .map(|(name, profile)| {
                    (
                        name.clone(),
                        profile.address.clone(),
                        profile.ssh_fingerprint.clone(),
                    )
                })
                .collect()
        } else {
            Vec::new()
        };
        self.start_operation_job(
            "Verbindungsprofile importieren",
            false,
            description,
            move |job| {
                arco_core::files::check_cancel(&job.cancel)?;
                let result = match source {
                    NetworkImport::WinScpIni => {
                        Outcome::ImportedNetwork(arco_core::winscp::read_sites(&path)?)
                    }
                    NetworkImport::FileZilla => {
                        Outcome::ImportedNetwork(network_import::read_filezilla(&path)?)
                    }
                    NetworkImport::OpenSsh => {
                        Outcome::ImportedNetwork(network_import::read_openssh(&path)?)
                    }
                    NetworkImport::KnownHosts => {
                        let keys = KnownHosts::read(&path)?;
                        let mut proposals = Vec::new();
                        for (name, address, previous) in ssh_profiles {
                            arco_core::files::check_cancel(&job.cancel)?;
                            let candidate = if previous.is_empty() {
                                keys.fingerprint(&address)
                            } else {
                                Ok(None)
                            };
                            proposals.push(KnownHostProposal {
                                name,
                                address,
                                previous,
                                candidate,
                            });
                        }
                        Outcome::ImportedKnownHosts(proposals)
                    }
                    NetworkImport::Ghisler => {
                        Outcome::ImportedNetwork(network_import::read_ghisler_sftp(&path)?)
                    }
                    _ => return Err(invalid("Ungültige Datei-Importquelle.")),
                };
                arco_core::files::check_cancel(&job.cancel)?;
                Ok(result)
            },
        );
        Ok(())
    }

    pub(super) fn review_imported_network(&mut self, sites: Vec<arco_core::winscp::ImportedSite>) {
        self.review_network_import(sites, "Verbindungsprofile prüfen");
    }

    pub(super) fn review_known_hosts(&mut self, proposals: Vec<KnownHostProposal>) {
        let mut pins = BTreeMap::new();
        let mut rows = Vec::new();
        for KnownHostProposal {
            name,
            address,
            previous,
            candidate,
        } in proposals
        {
            let (status, detail, kind) = if !previous.is_empty() {
                ("Bereits vorhanden", previous, "skip")
            } else {
                match candidate {
                    Ok(Some(pin)) => {
                        pins.insert(name.clone(), (address, pin.clone()));
                        ("Übernehmen", pin, "ftp")
                    }
                    Ok(None) => ("Kein passender Hostschlüssel", address, "skip"),
                    Err(error) => ("Überspringen", error.to_string(), "error"),
                }
            };
            rows.push(crate::ReviewRow {
                primary: name.as_str().into(),
                secondary: crate::locale::tr(status).into(),
                detail: detail.into(),
                kind: kind.into(),
            });
        }
        self.management_review(
            crate::OperationKind::Ftp,
            rows,
            ["Profil", "SSH-Hostschlüssel"],
            "Importieren",
        );
        self.ui().set_review_can_confirm(!pins.is_empty());
        self.dialog(
            "management-review",
            Pending::ApplyKnownHosts(pins),
            Dialog {
                title: "SSH-Hostschlüssel prüfen",
                description: "Arco ergänzt fehlende Fingerabdrücke in gespeicherten SSH-Profilen.",
                ..Dialog::default()
            },
        );
    }

    pub(crate) fn apply_known_hosts(
        &mut self,
        pins: BTreeMap<String, (String, String)>,
    ) -> io::Result<()> {
        let mut profiles = self.profile.ftp_profiles.clone();
        for (name, (address, pin)) in pins {
            let profile = profiles
                .get_mut(&name)
                .filter(|profile| {
                    profile.security.is_ssh()
                        && profile.address == address
                        && profile.ssh_fingerprint.is_empty()
                })
                .ok_or_else(|| {
                    invalid("Ein SSH-Profil wurde inzwischen geändert. Öffne die Vorschau erneut.")
                })?;
            profile.ssh_fingerprint = pin;
        }
        self.commit_ftp_profiles(profiles)?;
        self.status(crate::locale::tr("SSH-Hostschlüssel übernommen"));
        Ok(())
    }

    fn review_network_import(&mut self, sites: Vec<arco_core::winscp::ImportedSite>, title: &str) {
        let mut names: std::collections::BTreeSet<_> = self
            .profile
            .ftp_profiles
            .keys()
            .map(|name| name.to_lowercase())
            .collect();
        let mut profiles = BTreeMap::new();
        let mut report = String::new();
        let mut rows = Vec::new();
        for (name, imported) in sites {
            let imported = validate_name(&name).and(imported);
            match imported {
                Ok(profile) if names.insert(name.to_lowercase()) => {
                    let mode = match profile.security {
                        FtpSecurity::ExplicitTls => "Explizites FTPS",
                        FtpSecurity::ImplicitTls => "Implizites FTPS",
                        FtpSecurity::Plain => "Klartext-FTP",
                        FtpSecurity::Sftp => "SFTP (SSH)",
                        FtpSecurity::Scp => "SCP (SSH)",
                        FtpSecurity::WebDav => "WebDAV",
                        FtpSecurity::S3 => "S3",
                    };
                    let mut detail = profile.address.clone();
                    if profile.ssh_kerberos_kex {
                        detail.push_str(" · ");
                        detail.push_str(crate::locale::tr("GSSAPI-Schlüsselaustausch verlangen"));
                    }
                    if profile.ssh_kerberos_kex && profile.ssh_fingerprint.is_empty() {
                        detail.push_str(" · ");
                        detail.push_str(crate::locale::tr(
                            "SSH-Fingerabdruck oder Kerberos-Dienstprinzipal ergänzen",
                        ));
                    } else if profile.security.is_ssh() && profile.ssh_fingerprint.is_empty() {
                        detail.push_str(" · ");
                        detail.push_str(crate::locale::tr("SSH-Hostschlüssel ergänzen"));
                    }
                    let _ = writeln!(report, "Übernehmen: {name} · {mode} · {detail}");
                    rows.push(crate::ReviewRow {
                        primary: name.as_str().into(),
                        secondary: crate::locale::tr(mode).into(),
                        detail: detail.into(),
                        kind: "ftp".into(),
                    });
                    profiles.insert(name, profile);
                }
                Ok(_) => {
                    let _ = writeln!(report, "Überspringen: {name} · Name bereits vorhanden");
                    rows.push(crate::ReviewRow {
                        primary: name.as_str().into(),
                        secondary: crate::locale::tr("Bereits vorhanden").into(),
                        kind: "skip".into(),
                        ..Default::default()
                    });
                }
                Err(error) => {
                    let label: String = name
                        .chars()
                        .filter(|ch| !ch.is_control())
                        .take(120)
                        .collect();
                    let _ = writeln!(report, "Überspringen: {label} · {error}");
                    rows.push(crate::ReviewRow {
                        primary: label.into(),
                        detail: error.to_string().into(),
                        secondary: crate::locale::tr("Überspringen").into(),
                        kind: "error".into(),
                    });
                }
            }
        }
        self.management_review(
            crate::OperationKind::Ftp,
            rows,
            ["Profil", "Übernahme / Modus"],
            "Importieren",
        );
        self.ui().set_review_can_confirm(!profiles.is_empty());
        self.dialog("management-review", Pending::ApplyFtpImport(profiles), Dialog {
            title,
            description: "Passwörter werden beim ersten Verbinden abgefragt. Vorhandene Profile bleiben erhalten.",
            value: &report, ..Dialog::default()
        });
    }

    pub(crate) fn apply_ftp_import(
        &mut self,
        imported: BTreeMap<String, FtpProfile>,
    ) -> io::Result<()> {
        let mut profiles = self.profile.ftp_profiles.clone();
        if imported.keys().any(|name| {
            profiles
                .keys()
                .any(|existing| existing.to_lowercase() == name.to_lowercase())
        }) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "Mindestens ein Importname ist inzwischen vergeben.",
            ));
        }
        let first = imported.keys().next().cloned();
        let count = imported.len();
        profiles.extend(imported);
        self.commit_ftp_profiles(profiles)?;
        self.open_ftp_profiles(first.as_deref());
        self.ui()
            .set_ftp_profile_notice(format!("{count} FTP-Profile übernommen.").into());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn jump_profiles_resolve_in_connection_order_and_reject_invalid_routes() {
        let profile = |jump: Option<&str>| FtpProfile {
            security: FtpSecurity::Sftp,
            ssh_jump: jump.map(str::to_owned),
            ..Default::default()
        };
        let mut profiles = BTreeMap::from([
            ("target".into(), profile(Some("inner"))),
            ("inner".into(), profile(Some("outer"))),
            ("outer".into(), profile(None)),
        ]);
        let route = |profiles: &BTreeMap<String, FtpProfile>| {
            jump_profiles("target", &profiles["target"], profiles).map(|hops| {
                hops.iter()
                    .map(|(name, _)| (*name).to_owned())
                    .collect::<Vec<_>>()
            })
        };
        assert_eq!(route(&profiles).unwrap(), ["outer", "inner"]);
        profiles.get_mut("target").unwrap().proxy = "http://proxy.invalid".into();
        assert_eq!(
            route(&profiles).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        profiles.get_mut("target").unwrap().proxy.clear();
        profiles.get_mut("outer").unwrap().ssh_jump = Some("target".into());
        assert_eq!(
            route(&profiles).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        profiles.remove("outer");
        assert_eq!(
            route(&profiles).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
    }

    #[test]
    fn import_preserves_tls_port_and_path_without_passwords() {
        let mut ini = tempfile::NamedTempFile::new().unwrap();
        let content = "\
[connections]
1=München
2=Andere
[General]
FtpShowHidden=1
ModeZ=2
[München]
host=ftpis://example.invalid:992
directory=/Fotos 2026/Äste
pasvmode=1
mlsd=0
encoding=1252
username=arco
password=test-only-secret
[Andere]
host=ftp.example.invalid
firewall=1
";
        ini.write_all(&[0xff, 0xfe]).unwrap();
        for character in content.encode_utf16() {
            ini.write_all(&character.to_le_bytes()).unwrap();
        }
        ini.flush().unwrap();
        let mut sites = profile::read_tc_ftp_ini(ini.path()).unwrap().into_iter();
        let (name, site) = sites.next().unwrap();
        assert_eq!(name, "München");
        let profile = profile_from_tc(site).unwrap();
        assert_eq!(
            profile.address,
            "ftps://example.invalid:992/Fotos%202026/%C3%84ste/"
        );
        assert_eq!(profile.security, FtpSecurity::ImplicitTls);
        assert_eq!(profile.listing, FtpListing::List);
        assert_eq!(profile.charset, "windows-1252");
        assert!(profile.show_hidden);
        assert!(profile.mode_z);
        assert_eq!(profile.user, "arco");
        assert!(!profile.active);
        assert!(profile.credential.is_none());
        assert!(profile_from_tc(sites.next().unwrap().1).is_err());
        let mut saved = arco_core::profile::Profile::default();
        saved.ftp_profiles.insert(name, profile);
        let target = ini.path().with_extension("json");
        saved.save(&target).unwrap();
        let bytes = std::fs::read_to_string(&target).unwrap();
        let restored = arco_core::profile::Profile::load(&target).unwrap();
        std::fs::remove_file(target).unwrap();
        assert!(!bytes.contains("test-only-secret"));
        assert_eq!(saved.ftp_profiles, restored.ftp_profiles);
    }
}
