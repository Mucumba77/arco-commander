use super::{App, Navigation, Outcome, strings};
use crate::FtpProfileData;
use crate::commands::{Dialog, Pending};
use crate::panel::{FtpLocation, Location};
use arco_core::profile::{self, FtpListing, FtpProfile, FtpSecurity, TcFtpSite};
use arco_windows::ftp::{self, Connection, Url};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;

fn connection(profile: &FtpProfile) -> io::Result<Connection> {
    let mode = match profile.security {
        FtpSecurity::ExplicitTls => ftp::Security::ExplicitTls,
        FtpSecurity::ImplicitTls => ftp::Security::ImplicitTls,
        FtpSecurity::Plain => ftp::Security::Plain,
    };
    let listing = match profile.listing {
        FtpListing::Automatic => ftp::ListingMode::Automatic,
        FtpListing::Mlsd => ftp::ListingMode::Mlsd,
        FtpListing::List => ftp::ListingMode::List,
    };
    Connection::new(&profile.address, &profile.user, mode)?
        .with_proxy(&profile.proxy)?
        .with_listing_mode(listing)
        .with_active_mode(profile.active)
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
        local_directory: site.local_directory,
        ..FtpProfile::default()
    };
    connection(&profile).map_err(|_| invalid("Ungültige FTP-Verbindungsdaten"))?;
    Ok(profile)
}

impl App {
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
                    },
                    active: profile.active,
                    listing: match profile.listing {
                        FtpListing::Automatic => 0,
                        FtpListing::Mlsd => 1,
                        FtpListing::List => 2,
                    },
                    remember: profile.credential.is_some(),
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
            ("connect", Some((name, profile))) => self.connect_ftp_profile(name, profile),
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
            security: match data.security {
                1 => FtpSecurity::ImplicitTls,
                2 => FtpSecurity::Plain,
                _ => FtpSecurity::ExplicitTls,
            },
            active: data.active,
            listing: match data.listing {
                1 => FtpListing::Mlsd,
                2 => FtpListing::List,
                _ => FtpListing::Automatic,
            },
            local_directory: (!local.is_empty()).then(|| PathBuf::from(local)),
            credential: None,
        };
        if profile
            .local_directory
            .as_ref()
            .is_some_and(|path| !path.is_absolute())
        {
            return Err(invalid(
                "Gib einen absoluten Pfad zum lokalen Startordner ein.",
            ));
        }
        let new_connection = connection(&profile)?;
        if !self.save_profile {
            return Err(io::Error::other(
                "Das Arco-Profil konnte beim Start nicht geladen werden.",
            ));
        }
        let previous = selected
            .as_ref()
            .and_then(|(_, previous)| previous.credential.as_ref().map(|id| (previous, id)));
        let mut staged = None;
        if data.remember {
            if password.is_empty() {
                let Some((old, id)) = previous else {
                    return Err(invalid(
                        "Gib ein Passwort ein oder deaktiviere das Speichern.",
                    ));
                };
                if !new_connection.same_credential_scope(&connection(old)?) {
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
        let cleanup = self.remove_unused_password(previous);
        self.ui().set_ftp_profile_notice(match cleanup {
            Ok(()) => "Profil gespeichert.".into(),
            Err(error) => format!(
                "Profil gespeichert. Bisheriges Passwort konnte nicht entfernt werden: {error}"
            )
            .into(),
        });
        Ok(())
    }

    fn connect_ftp_profile(&mut self, name: String, profile: FtpProfile) -> io::Result<()> {
        let connection = connection(&profile)?;
        let password = profile
            .credential
            .as_deref()
            .map(|id| connection.read_password(id))
            .transpose()?
            .flatten();
        if profile.security != FtpSecurity::Plain
            && let Some(password) = password
        {
            self.close_dialog();
            return self.mount_ftp(
                self.active,
                connection,
                &password,
                profile.local_directory,
                Some(name),
            );
        }
        self.dialog("input", Pending::FtpProfilePassword(self.active, connection, profile.local_directory, name), Dialog {
            title: "FTP-Anmeldung",
            description: if profile.security == FtpSecurity::Plain {
                "Klartext-FTP überträgt Anmeldung und Dateien unverschlüsselt."
            } else if profile.credential.is_some() {
                "Das gespeicherte Passwort ist auf diesem Windows-Konto nicht verfügbar."
            } else {
                "Gib das Passwort für diese Verbindung ein. Bei anonymen Servern kann es leer bleiben."
            },
            placeholder: "Passwort",
            value: password.as_deref().map_or("", String::as_str),
            secret: true,
            ..Dialog::default()
        });
        Ok(())
    }

    pub(crate) fn mount_ftp(
        &mut self,
        side: usize,
        connection: Connection,
        password: &str,
        local: Option<PathBuf>,
        profile: Option<String>,
    ) -> io::Result<()> {
        let session = Arc::new(connection.authenticate(password)?);
        let directory = session.initial_directory();
        let navigation = local.map_or(Navigation::Jump, |local| Navigation::FtpProfile {
            local,
            opposite_request: self.panels[1 - side].request,
        });
        self.load_location(
            side,
            Location::Ftp(FtpLocation {
                session,
                directory,
                profile,
            }),
            navigation,
        );
        Ok(())
    }

    pub(crate) fn delete_ftp_profile(&mut self, name: &str) -> io::Result<()> {
        let mut profiles = self.profile.ftp_profiles.clone();
        let previous = profiles.remove(name);
        self.commit_ftp_profiles(profiles)?;
        self.open_ftp_profiles(None);
        let credential = previous
            .as_ref()
            .and_then(|profile| profile.credential.as_ref().map(|id| (profile, id)));
        self.ui()
            .set_ftp_profile_notice(match self.remove_unused_password(credential) {
                Ok(()) => "Profil gelöscht.".into(),
                Err(error) => format!(
                    "Profil gelöscht. Windows-Passwort konnte nicht entfernt werden: {error}"
                )
                .into(),
            });
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
            connection(old)?.delete_password(id)?;
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
        self.start_job("Lese FTP-Verbindungsprofile …", false, move |_| {
            Ok(Outcome::ImportedFtp(profile::read_tc_ftp_ini(&path)?))
        });
        Ok(())
    }

    pub(super) fn review_ftp_import(&mut self, sites: Vec<(String, TcFtpSite)>) {
        let mut names: std::collections::BTreeSet<_> = self
            .profile
            .ftp_profiles
            .keys()
            .map(|name| name.to_lowercase())
            .collect();
        let mut profiles = BTreeMap::new();
        let mut report = String::new();
        for (name, site) in sites {
            let imported = validate_name(&name).and_then(|_| profile_from_tc(site));
            match imported {
                Ok(profile) if names.insert(name.to_lowercase()) => {
                    let mode = match profile.security {
                        FtpSecurity::ExplicitTls => "Explizites FTPS",
                        FtpSecurity::ImplicitTls => "Implizites FTPS",
                        FtpSecurity::Plain => "Klartext-FTP",
                    };
                    let _ = writeln!(report, "Übernehmen: {name} · {mode} · {}", profile.address);
                    profiles.insert(name, profile);
                }
                Ok(_) => {
                    let _ = writeln!(report, "Überspringen: {name} · Name bereits vorhanden");
                }
                Err(error) => {
                    let label: String = name
                        .chars()
                        .filter(|ch| !ch.is_control())
                        .take(120)
                        .collect();
                    let _ = writeln!(report, "Überspringen: {label} · {error}");
                }
            }
        }
        if profiles.is_empty() {
            self.text("Keine importierbaren FTP-Profile", &report);
        } else {
            self.dialog("review", Pending::ApplyFtpImport(profiles), Dialog {
                title: "FTP-Profilimport prüfen",
                description: "Passwörter werden beim ersten Verbinden abgefragt. Vorhandene Profile bleiben erhalten.",
                value: &report,
                ..Dialog::default()
            });
        }
    }

    pub(crate) fn apply_ftp_import(
        &mut self,
        imported: BTreeMap<String, FtpProfile>,
    ) -> io::Result<()> {
        let mut profiles = self.profile.ftp_profiles.clone();
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
    fn import_preserves_tls_port_and_path_without_passwords() {
        let mut ini = tempfile::NamedTempFile::new().unwrap();
        let content = "\
[connections]
1=München
2=Andere
[München]
host=ftpis://example.invalid:992
directory=/Fotos 2026/Äste
pasvmode=1
mlsd=0
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
