use crate::profile::{FtpProfile, FtpSecurity};
use crate::ssh_host_keys::HostKeyPins;
use percent_encoding::{NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};
use std::{collections::BTreeMap, io};
use url::Url;

pub struct SessionLink {
    pub profile: Option<String>,
    pub connection: FtpProfile,
    pub file: Option<String>,
}

pub fn resolve(value: &str, profiles: &BTreeMap<String, FtpProfile>) -> io::Result<SessionLink> {
    let value = value.trim();
    if value.len() > 8192 || value.chars().any(char::is_control) {
        return Err(invalid("Ungültige oder zu lange Sitzungs-URL."));
    }
    let (scheme, rest) = value
        .split_once("://")
        .ok_or_else(|| invalid("Gib eine vollständige Sitzungs-URL ein."))?;
    if scheme.eq_ignore_ascii_case("arco") {
        return resolve_profile(value, profiles);
    }
    let scheme = scheme.to_ascii_lowercase();
    let (scheme, security) = match scheme.strip_prefix("winscp-").unwrap_or(&scheme) {
        "ftp" => ("ftp", FtpSecurity::Plain),
        "ftpes" => ("ftp", FtpSecurity::ExplicitTls),
        "ftps" => ("ftps", FtpSecurity::ImplicitTls),
        "sftp" => ("sftp", FtpSecurity::Sftp),
        "scp" => ("scp", FtpSecurity::Scp),
        "http" | "dav" => ("http", FtpSecurity::WebDav),
        "https" | "davs" | "webdav" => ("https", FtpSecurity::WebDav),
        "s3" => ("https", FtpSecurity::S3),
        _ => {
            return Err(invalid(
                "Dieses Protokoll wird für Sitzungs-URLs nicht unterstützt.",
            ));
        }
    };
    let (authority, suffix) = rest.split_at(rest.find(['/', '?', '#']).unwrap_or(rest.len()));
    check_path(suffix.split(['?', '#']).next().unwrap_or_default())?;
    let (host, user, inline) = match authority.rsplit_once('@') {
        Some((identity, host)) => {
            let (user, extra) = identity.split_once(';').unwrap_or((identity, ""));
            if user.contains([':', '@']) {
                return Err(invalid("Entferne das Passwort aus der Sitzungs-URL."));
            }
            let inline = if extra.is_empty() {
                String::new()
            } else {
                decode(
                    extra
                        .strip_prefix("fingerprint=")
                        .filter(|raw| !raw.contains(';'))
                        .ok_or_else(|| {
                            invalid("Erweiterte Einstellungen gehören ins Netzwerkprofil.")
                        })?,
                )?
            };
            (host, decode(user)?, inline)
        }
        None => (authority, String::new(), String::new()),
    };
    if host.is_empty()
        || host.contains(['\\', '%'])
        || user.contains(':')
        || user.chars().any(char::is_control)
    {
        return Err(invalid(
            "Server oder Benutzername in der Sitzungs-URL sind ungültig.",
        ));
    }
    let mut address = Url::parse(&format!("{scheme}://{host}{suffix}"))
        .map_err(|_| invalid("Ungültige Adresse in der Sitzungs-URL."))?;
    if address.host().is_none() || address.fragment().is_some() || address.port() == Some(0) {
        return Err(invalid(
            "Ungültige Adresse oder Fragment in der Sitzungs-URL.",
        ));
    }
    let supplied = query(&address, "fingerprint")?;
    address.set_query(None);
    if !inline.is_empty() && supplied.is_some() {
        return Err(invalid("Die Sitzungs-URL enthält zwei SSH-Fingerabdrücke."));
    }
    let pin = supplied.unwrap_or(inline);
    let mut trusted = None;
    if security.is_ssh() {
        if user.is_empty() {
            return Err(invalid("Die SSH-Sitzungs-URL braucht einen Benutzernamen."));
        }
        if !pin.is_empty() {
            trusted = Some(HostKeyPins::parse(&pin)?);
        }
        for known in profiles
            .values()
            .filter(|profile| profile.security.is_ssh() && !profile.ssh_fingerprint.is_empty())
        {
            let Ok(endpoint) = profile_address(known) else {
                continue;
            };
            if endpoint
                .host_str()
                .zip(address.host_str())
                .is_some_and(|(a, b)| {
                    a.trim_end_matches('.')
                        .eq_ignore_ascii_case(b.trim_end_matches('.'))
                })
                && endpoint.port().unwrap_or(22) == address.port().unwrap_or(22)
            {
                let saved = HostKeyPins::parse(&known.ssh_fingerprint)?;
                if let Some(trusted) = trusted.as_mut() {
                    if pin.is_empty() {
                        trusted.retain_shared(&saved);
                    }
                    if trusted.is_empty() || !trusted.is_subset(&saved) {
                        return Err(invalid(
                            "Die Sitzung hat keinen gemeinsamen vertrauenswürdigen SSH-Hostschlüssel oder die URL nennt einen unbekannten Schlüssel.",
                        ));
                    }
                } else {
                    trusted = Some(saved);
                }
            }
        }
    } else if !pin.is_empty() {
        return Err(invalid(
            "SSH-Fingerabdrücke gehören zu SFTP- oder SCP-URLs.",
        ));
    }
    let file = file_target(&mut address)?;
    Ok(SessionLink {
        profile: None,
        connection: FtpProfile {
            address: address.into(),
            user,
            security,
            ssh_fingerprint: trusted.map(|pins| pins.to_string()).unwrap_or_default(),
            ..FtpProfile::default()
        },
        file,
    })
}

fn resolve_profile(
    value: &str,
    profiles: &BTreeMap<String, FtpProfile>,
) -> io::Result<SessionLink> {
    let url = Url::parse(value).map_err(|_| invalid("Ungültige Arco-Profil-URL."))?;
    if url.host_str() != Some("session")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || url.port().is_some()
    {
        return Err(invalid("Ungültige Arco-Profil-URL."));
    }
    let encoded = url.path().strip_prefix('/').unwrap_or_default();
    if encoded.is_empty() || encoded.contains('/') {
        return Err(invalid("In der Arco-URL fehlt ein eindeutiger Profilname."));
    }
    let name = decode(encoded.strip_prefix("profile=").unwrap_or(encoded))?;
    let mut connection = profiles.get(&name).cloned().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "Das in der URL genannte Arco-Profil fehlt.",
        )
    })?;
    let mut address = profile_address(&connection)?;
    let mut file = None;
    if let Some(path) = query(&url, "path")? {
        check_path(&path)?;
        if !path.starts_with('/') {
            return Err(invalid(
                "Die Profil-URL erwartet einen absoluten Serverpfad.",
            ));
        }
        if matches!(connection.security, FtpSecurity::WebDav | FtpSecurity::S3) {
            let root = address.path().trim_end_matches('/');
            if !root.is_empty()
                && !path
                    .strip_prefix(root)
                    .is_some_and(|relative| relative.starts_with('/'))
            {
                return Err(invalid(
                    "Der Serverpfad liegt außerhalb des Netzwerkprofils.",
                ));
            }
        }
        address.set_path(&path);
        file = file_target(&mut address)?;
    }
    connection.address = address.into();
    Ok(SessionLink {
        profile: Some(name),
        connection,
        file,
    })
}

fn profile_address(profile: &FtpProfile) -> io::Result<Url> {
    let scheme = match profile.security {
        FtpSecurity::Plain | FtpSecurity::ExplicitTls => "ftp",
        FtpSecurity::ImplicitTls => "ftps",
        FtpSecurity::Sftp => "sftp",
        FtpSecurity::Scp => "scp",
        FtpSecurity::WebDav | FtpSecurity::S3 => "https",
    };
    let raw = profile.address.trim();
    let error = || invalid("Die Adresse im Netzwerkprofil ist ungültig.");
    if raw.is_empty() || raw.starts_with(['/', '\\']) || raw.chars().any(char::is_control) {
        return Err(error());
    }
    let parsed = if raw.contains("://") {
        Url::parse(raw)
    } else {
        Url::parse(&format!("{scheme}://{raw}"))
    }
    .map_err(|_| error())?;
    if parsed.host().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.fragment().is_some()
        || parsed.query().is_some()
        || parsed.port() == Some(0)
        || !(parsed.scheme() == scheme || (scheme == "https" && parsed.scheme() == "http"))
    {
        return Err(error());
    }
    Ok(parsed)
}

pub fn profile_link(name: &str, target: &Url) -> String {
    let mut link = Url::parse("arco://session/").expect("constant profile URL");
    // The prefix prevents names such as "." or ".." from becoming URL dot segments.
    link.set_path(&format!(
        "/profile={}",
        utf8_percent_encode(name, NON_ALPHANUMERIC)
    ));
    link.query_pairs_mut().append_pair("path", target.path());
    link.into()
}

pub fn direct_link(
    directory: &Url,
    security: FtpSecurity,
    user: &str,
    pin: &str,
) -> io::Result<String> {
    let mut link = directory.clone();
    link.set_username(&utf8_percent_encode(user, NON_ALPHANUMERIC).to_string())
        .map_err(|_| invalid("Der Benutzername ist in dieser URL ungültig."))?;
    if !pin.is_empty() {
        link.query_pairs_mut().append_pair("fingerprint", pin);
    }
    let (scheme, rest) = link
        .as_str()
        .split_once("://")
        .ok_or_else(|| invalid("Ungültige Sitzungs-URL."))?;
    let scheme = match security {
        FtpSecurity::ExplicitTls => "ftpes",
        FtpSecurity::S3 if scheme == "https" => "s3",
        FtpSecurity::S3 => {
            return Err(invalid(
                "Für diesen S3-Endpunkt wird ein gespeichertes Profil benötigt.",
            ));
        }
        _ => scheme,
    };
    Ok(format!("{scheme}://{rest}"))
}

pub fn normalize_pin(pin: &str) -> io::Result<String> {
    if pin.contains(';') {
        return Err(invalid("Erwartet wird genau ein SSH-Hostschlüssel."));
    }
    HostKeyPins::parse(pin).map(|pins| pins.to_string())
}

fn check_path(path: &str) -> io::Result<()> {
    for segment in path.split('/') {
        let value = decode(segment)?;
        if matches!(value.as_str(), "." | "..")
            || value.contains(['/', '\\'])
            || value.chars().any(char::is_control)
        {
            return Err(invalid(
                "Die Sitzungs-URL enthält einen ungültigen Serverpfad.",
            ));
        }
    }
    Ok(())
}

fn file_target(address: &mut Url) -> io::Result<Option<String>> {
    let path = address.path();
    let Some((parent, encoded)) = path.rsplit_once('/') else {
        return Ok(None);
    };
    if encoded.is_empty() {
        return Ok(None);
    }
    let file = decode(encoded)?;
    let parent = format!("{parent}/");
    address.set_path(&parent);
    Ok(Some(file))
}

fn decode(value: &str) -> io::Result<String> {
    if value.as_bytes().iter().enumerate().any(|(index, &byte)| {
        byte == b'%'
            && !value
                .as_bytes()
                .get(index + 1..index + 3)
                .is_some_and(|digits| digits.iter().all(u8::is_ascii_hexdigit))
    }) {
        return Err(invalid("Ungültige Prozentkodierung in der Sitzungs-URL."));
    }
    percent_decode_str(value)
        .decode_utf8()
        .map(|text| text.into_owned())
        .map_err(|_| invalid("Ungültige UTF-8-Kodierung in der Sitzungs-URL."))
}

fn query(url: &Url, parameter: &str) -> io::Result<Option<String>> {
    let mut value = None;
    for (key, item) in url.query_pairs() {
        if key != parameter || value.is_some() || item.is_empty() {
            return Err(invalid(
                "Die Sitzungs-URL enthält unbekannte oder mehrdeutige Parameter.",
            ));
        }
        value = Some(item.into_owned());
    }
    Ok(value)
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine, engine::general_purpose::STANDARD_NO_PAD};

    #[test]
    fn profile_links_use_existing_credentials_and_preserve_encoded_paths() {
        let profiles = BTreeMap::from([(
            "Team/Übersicht".into(),
            FtpProfile {
                address: "example.test/dav".into(),
                security: FtpSecurity::WebDav,
                credential: Some("vault-reference".into()),
                ..Default::default()
            },
        )]);
        let directory = Url::parse("https://example.test/dav/Plan%20%25/").unwrap();
        let link = profile_link("Team/Übersicht", &directory);
        assert!(!link.contains("vault-reference"));
        let resolved = resolve(&link, &profiles).unwrap();
        assert_eq!(resolved.connection.address, directory.as_str());
        assert_eq!(
            resolved.connection.credential.as_deref(),
            Some("vault-reference")
        );
        assert!(
            resolve(
                "arco://session/Team%2F%C3%9Cbersicht?path=%2Felsewhere%2F",
                &profiles
            )
            .is_err()
        );
    }

    #[test]
    fn direct_and_winscp_links_cannot_replace_saved_host_keys() {
        let digest = STANDARD_NO_PAD.encode([32u8; 32]);
        let fingerprint = format!("SHA256:{digest}");
        let address = Url::parse("sftp://example.test:2222/path/").unwrap();
        let link = direct_link(
            &address,
            FtpSecurity::Sftp,
            "team+user@example.test",
            &fingerprint,
        )
        .unwrap();
        let resolved = resolve(&link, &BTreeMap::new()).unwrap();
        assert_eq!(resolved.connection.user, "team+user@example.test");
        assert_eq!(resolved.connection.ssh_fingerprint, fingerprint);
        let winscp =
            format!("winscp-sftp://team;fingerprint=ssh-ed25519-{digest}@example.test:2222/path/");
        assert_eq!(
            resolve(&winscp, &BTreeMap::new())
                .unwrap()
                .connection
                .ssh_fingerprint,
            fingerprint
        );
        let conflicting = FtpProfile {
            address: "example.test:2222/path/".into(),
            security: FtpSecurity::Scp,
            ssh_fingerprint: format!("SHA256:{}", STANDARD_NO_PAD.encode([42u8; 32])),
            ..Default::default()
        };
        assert!(resolve(&link, &BTreeMap::from([("saved".into(), conflicting)])).is_err());
        for invalid in [
            "sftp://u:p@example.test/",
            "sftp://u@example.test/a%2fb/",
            "sftp://u@example.test/../secret",
            "sftp://u@example.test/app/%2e%2e/secret",
            "https://example.test/dav\\secret",
            "sftp://u@example.test/?password=secret",
        ] {
            assert!(resolve(invalid, &BTreeMap::new()).is_err());
        }
    }

    #[test]
    fn direct_links_roundtrip_alternatives_and_respect_all_saved_pins() {
        let a = format!("SHA256:{}", STANDARD_NO_PAD.encode([1; 32]));
        let b = format!("SHA256:{}", STANDARD_NO_PAD.encode([2; 32]));
        let c = format!("SHA256:{}", STANDARD_NO_PAD.encode([3; 32]));
        let address = Url::parse("sftp://example.test:2222/path/").unwrap();
        let link = |pins: &str| direct_link(&address, FtpSecurity::Sftp, "arco", pins).unwrap();
        let profile = |pins: String| FtpProfile {
            address: address.to_string(),
            security: FtpSecurity::Sftp,
            ssh_fingerprint: pins,
            ..Default::default()
        };
        let both = link(&format!("{b};{a}"));
        assert!(both.contains("%3B"));
        assert_eq!(
            resolve(&both, &BTreeMap::new())
                .unwrap()
                .connection
                .ssh_fingerprint,
            format!("{a};{b}")
        );
        let profiles = BTreeMap::from([
            ("one".into(), profile(format!("{a};{b}"))),
            ("two".into(), profile(format!("{b};{c}"))),
        ]);
        for valid in [link(""), link(&b)] {
            assert_eq!(
                resolve(&valid, &profiles)
                    .unwrap()
                    .connection
                    .ssh_fingerprint,
                b
            );
        }
        assert!(resolve(&both, &profiles).is_err());
        assert!(resolve(&link(&c), &profiles).is_err());
    }

    #[test]
    fn file_urls_roundtrip_encoded_names_and_stay_inside_webdav_root() {
        let target =
            Url::parse("https://example.test/dav/Plan%20%25/Gr%C3%BC%C3%9Fe%20%231.txt").unwrap();
        let profiles = BTreeMap::from([(
            "..".into(),
            FtpProfile {
                address: "https://example.test/dav/".into(),
                security: FtpSecurity::WebDav,
                credential: Some("vault-reference".into()),
                ..Default::default()
            },
        )]);
        let saved = resolve(&profile_link("..", &target), &profiles).unwrap();
        let direct = resolve(
            &direct_link(&target, FtpSecurity::WebDav, "user", "").unwrap(),
            &profiles,
        )
        .unwrap();
        for link in [saved, direct] {
            assert_eq!(
                link.connection.address,
                "https://example.test/dav/Plan%20%25/"
            );
            assert_eq!(link.file.as_deref(), Some("Grüße #1.txt"));
        }
        let outside = Url::parse("https://example.test/dav-private/secret.txt").unwrap();
        assert!(resolve(&profile_link("..", &outside), &profiles).is_err());
    }
}
