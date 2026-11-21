use crate::profile::{FtpAddressFamily, FtpGateway, FtpProfile, FtpSecurity, read_tc_text};
use base64::{Engine, engine::general_purpose::STANDARD_NO_PAD};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use url::Url;
use zeroize::Zeroizing;

pub type ImportedSite = (String, io::Result<FtpProfile>);
type SessionFields = BTreeMap<String, String>;
type SessionRecords = Vec<(String, SessionFields)>;
type HostKeys<'a> = BTreeMap<String, Option<&'a str>>;

mod workspaces;
pub use workspaces::{ImportedWorkspace, WorkspaceTab, read_workspaces, registry_workspaces};

pub const WINSCP_FIELDS: &[&str] = &[
    "HostName",
    "PortNumber",
    "UserName",
    "FSProtocol",
    "Ftps",
    "RemoteDirectory",
    "LocalDirectory",
    "SynchronizeBrowsing",
    "PublicKeyFile",
    "SshHostKey",
    "AddressFamily",
    "Timeout",
    "FtpPasvMode",
    "ProxyMethod",
    "FtpProxyLogonType",
    "ProxyHost",
    "ProxyPort",
    "ProxyDNS",
    "ProxyTelnetCommand",
    "ProxyLocalCommand",
    "ProxyUsername",
    "Tunnel",
    "WebDavLiberalEscaping",
    "WebDavCrossDomainRedirects",
    "WebDavUnencryptedRedirects",
    "S3DefaultRegion",
    "S3UrlStyle",
    "S3RequesterPays",
    "S3RoleArn",
    "TlsClientCertificateFile",
    "Workspace",
    "IsWorkspace",
    "Link",
    "NameOverride",
    "OtherLocalDirectory",
    "AuthGSSAPIKEX",
];

pub fn read_sites(path: &Path) -> io::Result<Vec<ImportedSite>> {
    parse_sites(&Zeroizing::new(read_tc_text(path)?))
}

fn parse_sites(input: &str) -> io::Result<Vec<ImportedSite>> {
    let (sites, host_keys) = parse_records(input)?;
    Ok(sites
        .into_iter()
        .map(|(name, fields)| (name, site(&fields, &host_keys)))
        .collect())
}

fn parse_records(input: &str) -> io::Result<(SessionRecords, HostKeys<'_>)> {
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
            if WINSCP_FIELDS
                .iter()
                .any(|field| field.eq_ignore_ascii_case(&key))
            {
                sites[index].1.insert(key, unmunge(value.trim()));
            }
        }
    }
    if sites.is_empty() {
        return Err(invalid(
            "Die Datei enthält keine gespeicherten WinSCP-Sitzungen.",
        ));
    }
    Ok((sites, host_keys))
}

pub(super) fn site(
    fields: &BTreeMap<String, String>,
    host_keys: &BTreeMap<String, Option<&str>>,
) -> io::Result<FtpProfile> {
    let value = |key| fields.get(key).map(String::as_str).unwrap_or_default();
    if flag(value("isworkspace")) || flag(value("workspace")) || flag(value("tunnel")) {
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
    let ftp_gateway = match value("ftpproxylogontype") {
        "" | "0" => None,
        "1" => Some(FtpGateway::Site),
        "2" => Some(FtpGateway::ProxyThenUserAtHost),
        "3" => Some(FtpGateway::Open),
        "4" => Some(FtpGateway::ProxyThenUser),
        "5" => Some(FtpGateway::UserAtHost),
        "6" => Some(FtpGateway::ProxyAtHostThenUser),
        "7" => Some(FtpGateway::UserAtHostAndProxy),
        "8" => Some(FtpGateway::UserAtProxyAtHost),
        _ => return Err(invalid("Unbekannte WinSCP-FTP-Gateway-Anmeldung.")),
    };
    let proxy = if ftp_gateway.is_some() {
        if security != FtpSecurity::Plain {
            return Err(invalid("Die FTP-Gateway-Anmeldung benötigt Klartext-FTP."));
        }
        let host = value("proxyhost").trim();
        let mut proxy = match host_url("ftp", host) {
            Ok(proxy) => proxy,
            Err(_) => Url::parse(&format!("ftp://{host}")).map_err(io::Error::other)?,
        };
        if proxy.host_str().is_none()
            || !proxy.username().is_empty()
            || proxy.password().is_some()
            || proxy.query().is_some()
            || proxy.fragment().is_some()
            || proxy.path() != "/"
            || host.chars().any(|ch| ch.is_control() || ch.is_whitespace())
        {
            return Err(invalid("Ungültige WinSCP-FTP-Gateway-Adresse."));
        }
        if proxy.port().is_none() && !host.ends_with(":21") {
            proxy
                .set_port(Some(number(value("proxyport"), 21)?))
                .map_err(|_| invalid("Ungültiger Proxy-Port."))?;
        }
        proxy.to_string()
    } else {
        match value("proxymethod") {
            "" | "0" => String::new(),
            "1" | "2" | "3" => {
                let (scheme, default_port) = match (value("proxymethod"), value("proxydns")) {
                    ("1", "2") => ("socks4a", 1080),
                    ("1", _) => ("socks4", 1080),
                    ("2", "0") => ("socks5", 1080),
                    ("2", _) => ("socks5h", 1080),
                    _ => ("http", 8080),
                };
                let mut proxy = host_url(scheme, value("proxyhost"))?;
                proxy
                    .set_port(Some(number(value("proxyport"), default_port)?))
                    .map_err(|_| invalid("Ungültiger Proxy-Port."))?;
                proxy.to_string()
            }
            "4" | "Telnet" if matches!(security, FtpSecurity::Sftp | FtpSecurity::Scp) => {
                let mut proxy = host_url("telnet", value("proxyhost"))?;
                proxy
                    .set_port(Some(number(value("proxyport"), 23)?))
                    .map_err(|_| invalid("Ungültiger Telnet-Proxy-Port."))?;
                proxy.to_string()
            }
            "5" | "Cmd" if security.is_ssh() && !value("proxylocalcommand").trim().is_empty() => {
                "local://command".to_owned()
            }
            _ => {
                return Err(invalid(
                    "Dieser Proxy-Typ oder seine Anmeldung benötigt eine manuelle Einrichtung.",
                ));
            }
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
        crate::ssh_host_keys::HostKeyPins::parse(value("sshhostkey"))?.to_string()
    } else {
        cached_fingerprint(host_keys, address.host_str().unwrap_or_default(), port)
            .unwrap_or_default()
    };
    let active = proxy.is_empty()
        && matches!(
            security,
            FtpSecurity::Plain | FtpSecurity::ExplicitTls | FtpSecurity::ImplicitTls
        )
        && value("ftppasvmode") == "0";
    let ssh_proxy_dns_locally = security.is_ssh() && !proxy.is_empty() && value("proxydns") == "0";
    Ok(FtpProfile {
        address: address.to_string(),
        user: user.to_owned(),
        security,
        proxy,
        ssh_proxy_dns_locally,
        telnet_proxy_command: match value("proxymethod") {
            "4" | "Telnet" => value("proxytelnetcommand").to_owned(),
            _ => String::new(),
        },
        local_proxy_command: match value("proxymethod") {
            "5" | "Cmd" => value("proxylocalcommand").to_owned(),
            _ => String::new(),
        },
        ftp_gateway: ftp_gateway.unwrap_or_default(),
        proxy_user: if ftp_gateway.map_or_else(
            || {
                matches!(
                    value("proxymethod"),
                    "1" | "2" | "3" | "4" | "Telnet" | "5" | "Cmd"
                )
            },
            FtpGateway::supports_proxy_login,
        ) {
            value("proxyusername").to_owned()
        } else {
            String::new()
        },
        active,
        address_family,
        ssh_fingerprint: pin,
        ssh_kerberos: kerberos_kex.then(String::new),
        ssh_kerberos_kex: kerberos_kex,
        ssh_private_key: (security.is_ssh() && key_file.is_absolute()).then_some(key_file),
        ssh_timeout: if security.is_ssh() { timeout } else { None },
        webdav_liberal_escaping: security == FtpSecurity::WebDav
            && flag(value("webdavliberalescaping")),
        webdav_cross_domain_redirects: security == FtpSecurity::WebDav
            && flag(value("webdavcrossdomainredirects")),
        webdav_unencrypted_redirects: security == FtpSecurity::WebDav
            && flag(value("webdavunencryptedredirects")),
        s3_region: if security == FtpSecurity::S3 {
            value("s3defaultregion").to_owned()
        } else {
            String::new()
        },
        s3_virtual_host: security == FtpSecurity::S3
            && match value("s3urlstyle") {
                "" | "0" => true,
                "1" => false,
                _ => return Err(invalid("Die WinSCP-S3-Adressierungsart ist unbekannt.")),
            },
        s3_requester_pays: security == FtpSecurity::S3 && flag(value("s3requesterpays")),
        s3_role_arn: if security == FtpSecurity::S3 {
            value("s3rolearn").to_owned()
        } else {
            String::new()
        },
        local_directory: local_directory.is_absolute().then_some(local_directory),
        synchronize_browsing: flag(value("synchronizebrowsing")),
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
    let pins: Vec<_> = KEY_TYPES
        .iter()
        .filter_map(|kind| {
            let address = format!("{kind}@{port}:{host}");
            let data = cache.get(&address).copied().flatten()?;
            let key = cached_key(kind, data)?;
            Some(format!(
                "SHA256:{}",
                STANDARD_NO_PAD.encode(Sha256::digest(key))
            ))
        })
        .collect();
    crate::ssh_host_keys::HostKeyPins::parse(&pins.join(";"))
        .ok()
        .map(|pins| pins.to_string())
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
    fn imports_webdav_compatibility_per_site() {
        let sites = parse_sites(concat!(
            "[Sessions\\Legacy]\nHostName=legacy.test\nFSProtocol=6\nWebDavLiberalEscaping=1\n",
            "WebDavCrossDomainRedirects=1\nWebDavUnencryptedRedirects=1\n",
            "[Sessions\\Standard]\nHostName=standard.test\nFSProtocol=6\n",
        ))
        .unwrap();
        assert_eq!(sites[0].1.as_ref().unwrap().security, FtpSecurity::WebDav);
        assert!(sites[0].1.as_ref().unwrap().webdav_liberal_escaping);
        assert!(sites[0].1.as_ref().unwrap().webdav_cross_domain_redirects);
        assert!(sites[0].1.as_ref().unwrap().webdav_unencrypted_redirects);
        assert!(!sites[1].1.as_ref().unwrap().webdav_liberal_escaping);
        assert!(!sites[1].1.as_ref().unwrap().webdav_cross_domain_redirects);
        assert!(!sites[1].1.as_ref().unwrap().webdav_unencrypted_redirects);
    }

    #[test]
    fn imports_socks_user_id_and_dns_resolution_mode() {
        for (method, dns, scheme) in [
            (1, 1, "socks4"),
            (1, 2, "socks4a"),
            (2, 0, "socks5"),
            (2, 1, "socks5h"),
        ] {
            let input = format!(
                "[Sessions\\Proxy]\nHostName=server.test\nFSProtocol=2\nProxyMethod={method}\nProxyDNS={dns}\nProxyHost=proxy.test\nProxyUserName=work-user\n"
            );
            let sites = parse_sites(&input).unwrap();
            let profile = sites[0].1.as_ref().unwrap();
            assert_eq!(profile.proxy, format!("{scheme}://proxy.test:1080/"));
            assert_eq!(profile.proxy_user, "work-user");
            assert!(profile.proxy_credential.is_none());
        }
    }

    #[test]
    fn imports_a_telnet_proxy_command_without_copying_proxy_password() {
        let sites = parse_sites(concat!(
            "[Sessions\\Relay]\nHostName=server.test\nFSProtocol=2\n",
            "ProxyMethod=4\nProxyHost=proxy.test\nProxyPort=9823\n",
            "ProxyUsername=relay\nProxyPassword=secret\n",
            "ProxyTelnetCommand=%user\\n%pass\\nconnect %host %port\\r\\n\n",
        ))
        .unwrap();
        let profile = sites[0].1.as_ref().unwrap();
        assert_eq!(profile.proxy, "telnet://proxy.test:9823/");
        assert_eq!(
            profile.telnet_proxy_command,
            r"%user\n%pass\nconnect %host %port\r\n"
        );
        assert_eq!(profile.proxy_user, "relay");
        assert!(profile.proxy_credential.is_none());
    }

    #[test]
    fn imports_local_ssh_proxy_command_and_leaves_password_unstored() {
        let command = r#""C:\Program Files\Proxy\helper.exe" "%host" %port "%pass""#;
        for method in ["5", "Cmd"] {
            let ini = format!(
                "[Sessions\\Local]\nHostName=server.test\nFSProtocol=2\nProxyMethod={method}\nProxyLocalCommand={command}\nProxyDNS=0\nProxyUsername=relay\nProxyPassword=secret\n"
            );
            let sites = parse_sites(&ini).unwrap();
            let profile = sites[0].1.as_ref().unwrap();
            assert_eq!(profile.proxy, "local://command");
            assert_eq!(profile.local_proxy_command, command);
            assert!(profile.ssh_proxy_dns_locally);
            assert_eq!(profile.proxy_user, "relay");
            assert!(profile.proxy_credential.is_none());
        }
    }

    #[test]
    fn imports_ftp_gateway_sequences_without_proxy_passwords() {
        for (code, mode) in [
            (1, FtpGateway::Site),
            (2, FtpGateway::ProxyThenUserAtHost),
            (3, FtpGateway::Open),
            (4, FtpGateway::ProxyThenUser),
            (5, FtpGateway::UserAtHost),
            (6, FtpGateway::ProxyAtHostThenUser),
            (7, FtpGateway::UserAtHostAndProxy),
            (8, FtpGateway::UserAtProxyAtHost),
        ] {
            let profile = format!(
                "[Sessions\\Gateway]\nHostName=server.test\nFSProtocol=5\nFtpPasvMode=0\nFtpProxyLogonType={code}\nProxyHost=gateway.test:2121\nProxyUsername=proxyuser\nProxyPassword=secret\n"
            );
            let sites = parse_sites(&profile).unwrap();
            let site = sites[0].1.as_ref().unwrap();
            assert_eq!(site.ftp_gateway, mode);
            assert_eq!(site.proxy, "ftp://gateway.test:2121/");
            assert_eq!(
                site.proxy_user,
                if mode.supports_proxy_login() {
                    "proxyuser"
                } else {
                    ""
                }
            );
            assert!(site.proxy_credential.is_none());
            assert!(!site.active);
            assert!(!serde_json::to_string(site).unwrap().contains("secret"));
        }
    }

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
SynchronizeBrowsing=1
[Sessions\Archiv]
HostName=s3.test
FSProtocol=7
Ftps=3
S3DefaultRegion=eu-central-1
S3UrlStyle=1
S3RequesterPays=1
S3RoleArn=arn:aws:iam::123456789012:role/Archive
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
        assert!(profile.synchronize_browsing);
        assert!(!sites[1].1.as_ref().unwrap().synchronize_browsing);
        assert!(profile.credential.is_none() && profile.ssh_fingerprint.is_empty());
        assert_eq!(sites[1].1.as_ref().unwrap().s3_region, "eu-central-1");
        assert!(!sites[1].1.as_ref().unwrap().s3_virtual_host);
        assert!(sites[1].1.as_ref().unwrap().s3_requester_pays);
        assert_eq!(
            sites[1].1.as_ref().unwrap().s3_role_arn,
            "arn:aws:iam::123456789012:role/Archive"
        );
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
    fn imports_all_explicit_ssh_pins_or_rejects_the_profile() {
        let a = STANDARD_NO_PAD.encode([1; 32]);
        let b = STANDARD_NO_PAD.encode([2; 32]);
        let sites = parse_sites(&format!(
            "[Sessions\\Pinned]\nHostName=server.test\nSshHostKey=ssh-ed25519 255 {b}%3BSHA256:{a}\n\
             [Sessions\\Invalid]\nHostName=server.test\nSshHostKey=SHA256:{a};*\n"
        ))
        .unwrap();
        assert_eq!(
            sites[0].1.as_ref().unwrap().ssh_fingerprint,
            format!("SHA256:{a};SHA256:{b}")
        );
        assert!(sites[1].1.is_err());
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
