use crate::profile::{FtpAddressFamily, FtpProfile, FtpSecurity, read_tc_text};
use base64::{Engine, engine::general_purpose::STANDARD_NO_PAD};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use url::Url;
use zeroize::Zeroizing;

pub type ImportedSite = (String, io::Result<FtpProfile>);

pub fn read_sites(path: &Path) -> io::Result<Vec<ImportedSite>> {
    parse_sites(&Zeroizing::new(read_tc_text(path)?))
}

fn parse_sites(input: &str) -> io::Result<Vec<ImportedSite>> {
    let mut sites = Vec::new();
    let mut host_keys = BTreeMap::new();
    let mut section = None;
    let mut in_host_keys = false;
    for line in input.lines().map(str::trim) {
        if let Some(name) = line
            .strip_prefix('[')
            .and_then(|line| line.strip_suffix(']'))
        {
            section = None;
            in_host_keys = name.eq_ignore_ascii_case("SshHostKeys");
            if let Some((prefix, name)) = name.split_once('\\')
                && prefix.eq_ignore_ascii_case("Sessions")
            {
                let name = unmunge(name);
                if !name.eq_ignore_ascii_case("Default Settings") {
                    if sites.len() >= 2000 {
                        return Err(invalid("Die WinSCP-Datei enthält mehr als 2000 Profile."));
                    }
                    sites.push((name, BTreeMap::new()));
                    section = Some(sites.len() - 1);
                }
            }
        } else if in_host_keys && let Some((key, value)) = line.split_once('=') {
            let key = unmunge(key.trim()).to_ascii_lowercase();
            let value = value.trim();
            host_keys
                .entry(key)
                .and_modify(|previous: &mut Option<&str>| {
                    if *previous != Some(value) {
                        *previous = None;
                    }
                })
                .or_insert(Some(value));
        } else if let Some(index) = section
            && let Some((key, value)) = line.split_once('=')
        {
            let key = key.trim().to_ascii_lowercase();
            if matches!(
                key.as_str(),
                "hostname"
                    | "portnumber"
                    | "username"
                    | "fsprotocol"
                    | "ftps"
                    | "remotedirectory"
                    | "localdirectory"
                    | "publickeyfile"
                    | "sshhostkey"
                    | "addressfamily"
                    | "timeout"
                    | "ftppasvmode"
                    | "proxymethod"
                    | "proxyhost"
                    | "proxyport"
                    | "proxyusername"
                    | "tunnel"
                    | "s3defaultregion"
                    | "tlsclientcertificatefile"
                    | "workspace"
                    | "authgssapikex"
            ) {
                sites[index].1.insert(key, unmunge(value.trim()));
            }
        }
    }
    if sites.is_empty() {
        return Err(invalid(
            "Die Datei enthält keine gespeicherten WinSCP-Sitzungen.",
        ));
    }
    Ok(sites
        .into_iter()
        .map(|(name, fields)| (name, site(&fields, &host_keys)))
        .collect())
}

pub(super) fn site(
    fields: &BTreeMap<String, String>,
    host_keys: &BTreeMap<String, Option<&str>>,
) -> io::Result<FtpProfile> {
    let value = |key| fields.get(key).map(String::as_str).unwrap_or_default();
    if flag(value("workspace")) || flag(value("tunnel")) {
        return Err(invalid(
            "Dieser WinSCP-Eintrag benötigt einen Arbeitsbereich oder SSH-Tunnel.",
        ));
    }
    if !value("tlsclientcertificatefile").is_empty() {
        return Err(invalid(
            "Das TLS-Clientzertifikat muss in Arco separat eingerichtet werden.",
        ));
    }
    let tls = value("ftps");
    let (security, scheme, default_port) = match value("fsprotocol") {
        "0" => (FtpSecurity::Scp, "scp", 22),
        "" | "1" | "2" => (FtpSecurity::Sftp, "sftp", 22),
        "5" => match tls {
            "" | "0" => (FtpSecurity::Plain, "ftp", 21),
            "1" => (FtpSecurity::ImplicitTls, "ftps", 990),
            "3" => (FtpSecurity::ExplicitTls, "ftp", 21),
            _ => {
                return Err(invalid(
                    "Diese FTPS-Variante wird beim Import nicht unterstützt.",
                ));
            }
        },
        "6" | "7" => {
            let security = if value("fsprotocol") == "6" {
                FtpSecurity::WebDav
            } else {
                FtpSecurity::S3
            };
            if tls == "0" {
                (security, "http", 80)
            } else {
                (security, "https", 443)
            }
        }
        _ => {
            return Err(invalid(
                "Dieses WinSCP-Dateiprotokoll wird nicht unterstützt.",
            ));
        }
    };
    let kerberos_kex = security.is_ssh() && flag(value("authgssapikex"));
    let mut address = host_url(scheme, value("hostname"))?;
    let port = number(value("portnumber"), default_port)?;
    address
        .set_port(Some(port))
        .map_err(|_| invalid("Ungültiger Server-Port."))?;
    let remote = value("remotedirectory");
    if !remote.is_empty() && !remote.starts_with('/') {
        return Err(invalid(
            "Ein relativer WinSCP-Startordner braucht eine manuelle Zuordnung.",
        ));
    }
    {
        let mut segments = address
            .path_segments_mut()
            .map_err(|_| invalid("Ungültiger Serverpfad."))?;
        for part in remote.split('/').filter(|part| !part.is_empty()) {
            if matches!(part, "." | "..") || part.chars().any(char::is_control) {
                return Err(invalid(
                    "Der WinSCP-Startordner enthält einen ungültigen Pfadteil.",
                ));
            }
            segments.push(part);
        }
    }
    let proxy = match value("proxymethod") {
        "" | "0" => String::new(),
        "2" | "3" => {
            let scheme = if value("proxymethod") == "2" {
                "socks5h"
            } else {
                "http"
            };
            let mut proxy = host_url(scheme, value("proxyhost"))?;
            proxy
                .set_port(Some(number(value("proxyport"), 8080)?))
                .map_err(|_| invalid("Ungültiger Proxy-Port."))?;
            proxy.to_string()
        }
        _ => {
            return Err(invalid(
                "Dieser Proxy-Typ oder seine Anmeldung benötigt eine manuelle Einrichtung.",
            ));
        }
    };
    let address_family = match value("addressfamily") {
        "1" => FtpAddressFamily::Ipv4,
        "2" => FtpAddressFamily::Ipv6,
        _ => FtpAddressFamily::Automatic,
    };
    let key_file = PathBuf::from(value("publickeyfile"));
    if kerberos_kex && !key_file.as_os_str().is_empty() {
        return Err(invalid(
            "GSSAPI-Schlüsselaustausch in Kombination mit einer privaten SSH-Schlüsseldatei braucht eine manuelle Zuordnung.",
        ));
    }
    if security.is_ssh() && !key_file.as_os_str().is_empty() && !key_file.is_absolute() {
        return Err(invalid(
            "Die WinSCP-Schlüsseldatei braucht einen absoluten Pfad.",
        ));
    }
    let local_directory = PathBuf::from(value("localdirectory"));
    let user = value("username");
    if user.contains(':') || user.chars().any(char::is_control) {
        return Err(invalid("Ungültiger Benutzername im WinSCP-Profil."));
    }
    let timeout = value("timeout")
        .parse::<u64>()
        .ok()
        .filter(|seconds| (1..=300).contains(seconds));
    let pin = if !security.is_ssh() {
        String::new()
    } else if !value("sshhostkey").is_empty() {
        fingerprint(value("sshhostkey"))
    } else {
        cached_fingerprint(host_keys, address.host_str().unwrap_or_default(), port)
            .unwrap_or_default()
    };
    Ok(FtpProfile {
        address: address.to_string(),
        user: user.to_owned(),
        security,
        proxy,
        proxy_user: if matches!(value("proxymethod"), "2" | "3") {
            value("proxyusername").to_owned()
        } else {
            String::new()
        },
        active: matches!(
            security,
            FtpSecurity::Plain | FtpSecurity::ExplicitTls | FtpSecurity::ImplicitTls
        ) && value("ftppasvmode") == "0",
        address_family,
        ssh_fingerprint: pin,
        ssh_kerberos: kerberos_kex.then(String::new),
        ssh_kerberos_kex: kerberos_kex,
        ssh_private_key: (security.is_ssh() && key_file.is_absolute()).then_some(key_file),
        ssh_timeout: if security.is_ssh() { timeout } else { None },
        s3_region: if security == FtpSecurity::S3 {
            value("s3defaultregion").to_owned()
        } else {
            String::new()
        },
        local_directory: local_directory.is_absolute().then_some(local_directory),
        ..Default::default()
    })
}

fn cached_fingerprint(
    cache: &BTreeMap<String, Option<&str>>,
    host: &str,
    port: u16,
) -> Option<String> {
    const KEY_TYPES: &[&str] = &[
        "ssh-ed25519",
        "ecdsa-sha2-nistp256",
        "ecdsa-sha2-nistp384",
        "ecdsa-sha2-nistp521",
        "rsa2",
    ];
    let host = host.to_ascii_lowercase();
    KEY_TYPES.iter().find_map(|kind| {
        let address = format!("{kind}@{port}:{host}");
        let data = cache.get(&address).copied().flatten()?;
        let key = cached_key(kind, data)?;
        Some(format!(
            "SHA256:{}",
            STANDARD_NO_PAD.encode(Sha256::digest(key))
        ))
    })
}

fn cached_key(kind: &str, data: &str) -> Option<Vec<u8>> {
    let mut fields = data.split(',');
    let mut blob = Vec::new();
    match kind {
        "ssh-ed25519" => {
            let x = hexadecimal(fields.next()?, 32)?;
            let y = hexadecimal(fields.next()?, 32)?;
            let mut point = [0; 32];
            for (output, value) in point.iter_mut().zip(y.into_iter().rev()) {
                *output = value;
            }
            if point[31] & 0x80 != 0 {
                return None;
            }
            point[31] |= (x.last()? & 1) << 7;
            ssh_string(&mut blob, kind.as_bytes());
            ssh_string(&mut blob, &point);
        }
        "rsa2" => {
            ssh_string(&mut blob, b"ssh-rsa");
            for limit in [8, 1024] {
                let mut number = hexadecimal(fields.next()?, limit)?;
                if number.first()? & 0x80 != 0 {
                    number.insert(0, 0);
                }
                ssh_string(&mut blob, &number);
            }
        }
        kind => {
            let curve = kind.strip_prefix("ecdsa-sha2-")?;
            let width = match curve {
                "nistp256" => 32,
                "nistp384" => 48,
                "nistp521" => 66,
                _ => return None,
            };
            if fields.next()? != curve {
                return None;
            }
            let mut point = vec![0; 1 + 2 * width];
            point[0] = 4;
            for part in point[1..].chunks_exact_mut(width) {
                let coordinate = hexadecimal(fields.next()?, width)?;
                part[width - coordinate.len()..].copy_from_slice(&coordinate);
            }
            ssh_string(&mut blob, kind.as_bytes());
            ssh_string(&mut blob, curve.as_bytes());
            ssh_string(&mut blob, &point);
        }
    }
    fields.next().is_none().then_some(blob)
}

fn hexadecimal(value: &str, limit: usize) -> Option<Vec<u8>> {
    let digits = value.strip_prefix("0x")?.trim_start_matches('0');
    if digits.is_empty()
        || digits.len() > 2 * limit
        || !digits.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return None;
    }
    let offset = digits.len() % 2;
    let mut bytes = Vec::with_capacity(digits.len().div_ceil(2));
    if offset != 0 {
        bytes.push(u8::from_str_radix(&digits[..1], 16).ok()?);
    }
    for start in (offset..digits.len()).step_by(2) {
        bytes.push(u8::from_str_radix(&digits[start..start + 2], 16).ok()?);
    }
    Some(bytes)
}

fn ssh_string(blob: &mut Vec<u8>, bytes: &[u8]) {
    blob.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    blob.extend_from_slice(bytes);
}

fn fingerprint(value: &str) -> String {
    let mut candidates = value.split_ascii_whitespace().filter_map(|item| {
        let digest = item.strip_prefix("SHA256:")?.trim_end_matches('=');
        (STANDARD_NO_PAD.decode(digest).ok()?.len() == 32).then_some(digest)
    });
    match (candidates.next(), candidates.next()) {
        (Some(digest), None) if !value.contains([';', '*']) => format!("SHA256:{digest}"),
        _ => String::new(),
    }
}

fn host_url(scheme: &str, host: &str) -> io::Result<Url> {
    let host = host.trim();
    let authority = if let Ok(ip) = host.parse::<std::net::Ipv6Addr>() {
        format!("[{ip}]")
    } else if host.starts_with('[')
        && host.ends_with(']')
        && host[1..host.len() - 1]
            .parse::<std::net::Ipv6Addr>()
            .is_ok()
    {
        host.to_owned()
    } else if !host.is_empty()
        && !host.contains(['@', '/', '\\', '?', '#', ':', '%'])
        && !host.chars().any(|ch| ch.is_control() || ch.is_whitespace())
    {
        host.to_owned()
    } else {
        return Err(invalid("Ungültiger WinSCP-Servername."));
    };
    Url::parse(&format!("{scheme}://{authority}/")).map_err(io::Error::other)
}

fn number(raw: &str, default: u16) -> io::Result<u16> {
    if raw.is_empty() {
        return Ok(default);
    }
    raw.parse::<u16>()
        .ok()
        .filter(|port| *port != 0)
        .ok_or_else(|| invalid("Ungültiger WinSCP-Port."))
}

fn flag(value: &str) -> bool {
    value == "1" || value.eq_ignore_ascii_case("true")
}

pub(super) fn unmunge(value: &str) -> String {
    let bytes: Vec<u8> = percent_encoding::percent_decode_str(value).collect();
    if let Ok(text) = std::str::from_utf8(&bytes) {
        text.trim_start_matches('\u{feff}').to_owned()
    } else {
        encoding_rs::WINDOWS_1252
            .decode_without_bom_handling(&bytes)
            .0
            .into_owned()
    }
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imports_profiles_without_passwords_and_keeps_ssh_pins_explicit() {
        let input = r#"
[Sessions\Default%20Settings]
HostName=ignored.test
[Sessions\Team%2FGr%C3%BC%C3%9Fe]
HostName=example.test
FSProtocol=2
PortNumber=2222
UserName=arco
Password=private
RemoteDirectory=/a b/100%
[Sessions\Archiv]
HostName=s3.test
FSProtocol=7
Ftps=3
S3DefaultRegion=eu-central-1
[Sessions\Tunnel]
HostName=other.test
Tunnel=1
"#;
        let sites = parse_sites(input).unwrap();
        assert_eq!(sites.len(), 3);
        let (name, profile) = &sites[0];
        let profile = profile.as_ref().unwrap();
        assert_eq!(name, "Team/Grüße");
        assert_eq!(profile.address, "sftp://example.test:2222/a%20b/100%25");
        assert_eq!(profile.security, FtpSecurity::Sftp);
        assert!(profile.credential.is_none() && profile.ssh_fingerprint.is_empty());
        assert_eq!(sites[1].1.as_ref().unwrap().s3_region, "eu-central-1");
        assert!(sites[2].1.is_err());
    }

    #[test]
    fn reads_munged_ini_fields_and_pins_cached_host_key_to_its_port() {
        let input = r#"
[Sessions\%EF%BB%BFTeam%2FGr%C3%BC%C3%9Fe]
HostName=EXAMPLE.TEST
FSProtocol=2
PortNumber=2222
UserName=gr%FCn
RemoteDirectory=/Team%20Docs/100%25
[Sessions\Other%20Port]
HostName=example.test
PortNumber=22
[SshHostKeys]
ssh-ed25519%402222%3Aexample.test=0x02,0x1a5107f7681a02af2523a6daf372e10e3a0764c9d3fe4bd5b70ab18201985ad7
"#;
        let sites = parse_sites(input).unwrap();
        let profile = sites[0].1.as_ref().unwrap();
        assert_eq!(profile.user, "grün");
        assert_eq!(
            profile.address,
            "sftp://EXAMPLE.TEST:2222/Team%20Docs/100%25"
        );
        assert_eq!(
            profile.ssh_fingerprint,
            "SHA256:bbXpuKG6zhzdmnxq256TlqzFBzRl2f6OOg722cYNbU8"
        );
        assert!(sites[1].1.as_ref().unwrap().ssh_fingerprint.is_empty());
    }

    #[test]
    fn formats_rsa_mpints_and_ecdsa_points_in_ssh_wire_format() {
        let rsa = cached_key("rsa2", "0x10001,0x80").unwrap();
        assert_eq!(
            rsa,
            b"\0\0\0\x07ssh-rsa\0\0\0\x03\x01\0\x01\0\0\0\x02\0\x80"
        );
        let kind = "ecdsa-sha2-nistp256";
        let curve = "nistp256";
        let ecdsa = cached_key(kind, "nistp256,0x1,0x2").unwrap();
        let point = &ecdsa[12 + kind.len() + curve.len()..];
        assert_eq!(point.len(), 65);
        assert_eq!([point[0], point[32], point[64]], [4, 1, 2]);
        assert!(cached_key(kind, "nistp384,0x1,0x2").is_none());
    }
}
