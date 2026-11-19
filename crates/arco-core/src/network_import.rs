use crate::profile::{self, FtpProfile};
use crate::winscp::{self, ImportedSite};
use roxmltree::{Document, Node};
use std::collections::BTreeMap;
use std::io;
use std::path::Path;
use zeroize::Zeroizing;

mod ghisler;
mod openssh;
pub use ghisler::read_ghisler_sftp;
pub use openssh::{KnownHosts, read_openssh};

pub const WINSCP_FIELDS: &[&str] = &[
    "HostName",
    "PortNumber",
    "UserName",
    "FSProtocol",
    "Ftps",
    "RemoteDirectory",
    "LocalDirectory",
    "PublicKeyFile",
    "SshHostKey",
    "AddressFamily",
    "Timeout",
    "FtpPasvMode",
    "ProxyMethod",
    "ProxyHost",
    "ProxyPort",
    "ProxyUsername",
    "Tunnel",
    "S3DefaultRegion",
    "TlsClientCertificateFile",
    "Workspace",
    "AuthGSSAPIKEX",
];
pub const PUTTY_FIELDS: &[&str] = &[
    "Protocol",
    "HostName",
    "PortNumber",
    "UserName",
    "PublicKeyFile",
    "ProxyMethod",
    "LogicalHostName",
    "AddressFamily",
];
pub const SESSION_LIMIT: usize = 2000;

pub fn registry_sites(
    sessions: Vec<(String, BTreeMap<String, String>)>,
    host_keys: &BTreeMap<String, String>,
    win_scp: bool,
) -> io::Result<Vec<ImportedSite>> {
    let mut cache = BTreeMap::new();
    for (key, value) in host_keys {
        cache
            .entry(winscp::unmunge(key).to_ascii_lowercase())
            .and_modify(|prior| *prior = None)
            .or_insert(Some(value.as_str()));
    }
    let mut sites = Vec::new();
    for (name, raw_fields) in sessions {
        let name = winscp::unmunge(&name);
        if name.eq_ignore_ascii_case("Default Settings") {
            continue;
        }
        let fields = raw_fields
            .into_iter()
            .map(|(key, value)| {
                let value = if win_scp {
                    winscp::unmunge(&value)
                } else {
                    value
                };
                (key.to_ascii_lowercase(), value)
            })
            .collect();
        let profile = if win_scp {
            winscp::site(&fields, &cache)
        } else {
            putty_site(fields, &cache)
        };
        sites.push((name, profile));
    }
    require_sites(sites)
}

fn putty_site(
    mut fields: BTreeMap<String, String>,
    cache: &BTreeMap<String, Option<&str>>,
) -> io::Result<FtpProfile> {
    if fields
        .get("protocol")
        .is_none_or(|protocol| !protocol.eq_ignore_ascii_case("ssh"))
    {
        return Err(invalid(
            "Nur SSH-Sitzungen können als SFTP-Profile übernommen werden.",
        ));
    }
    if fields.get("proxymethod").is_some_and(|value| value != "0")
        || fields
            .get("logicalhostname")
            .is_some_and(|value| !value.is_empty())
    {
        return Err(invalid(
            "Proxy oder abweichender SSH-Hostname müssen manuell eingerichtet werden.",
        ));
    }
    fields.remove("proxymethod");
    // PuTTY enables opportunistic GSSAPI key exchange by default.
    fields.remove("authgssapikex");
    // The default TryAgent option permits a password fallback in PuTTY.
    // Arco's explicitly selected Pageant mode would require a running agent.
    winscp::site(&fields, cache)
}

pub fn read_filezilla(path: &Path) -> io::Result<Vec<ImportedSite>> {
    parse_filezilla(&Zeroizing::new(profile::read_tc_text(path)?))
}

fn parse_filezilla(xml: &str) -> io::Result<Vec<ImportedSite>> {
    let document = Document::parse_with_options(
        xml,
        roxmltree::ParsingOptions {
            allow_dtd: false,
            nodes_limit: 150_000,
            ..Default::default()
        },
    )
    .map_err(|_| invalid("Die FileZilla-XML konnte nicht gelesen werden."))?;
    let root = document.root_element();
    let servers = root
        .children()
        .find(|node| node.has_tag_name("Servers"))
        .filter(|_| root.has_tag_name("FileZilla3"))
        .ok_or_else(|| invalid("Die XML enthält keinen FileZilla-Servermanager."))?;
    let mut sites = Vec::new();
    for server in servers
        .descendants()
        .filter(|node| node.has_tag_name("Server"))
    {
        if sites.len() == SESSION_LIMIT {
            return Err(invalid(
                "Die Datei enthält mehr als 2000 Verbindungsprofile.",
            ));
        }
        let mut labels = Vec::new();
        for folder in server
            .ancestors()
            .skip(1)
            .take_while(|node| *node != servers)
        {
            if !folder.has_tag_name("Folder") || labels.len() >= 32 {
                return Err(invalid(
                    "Die FileZilla-Datei enthält eine ungültige Ordnerstruktur.",
                ));
            }
            let name = folder
                .children()
                .filter(Node::is_text)
                .filter_map(|node| node.text())
                .map(str::trim)
                .find(|name| !name.is_empty())
                .unwrap_or_else(|| field(folder, "Name"));
            labels.push(name);
        }
        labels.reverse();
        let name = field(server, "Name");
        labels.push(if name.is_empty() {
            field(server, "Host")
        } else {
            name
        });
        sites.push((labels.join("/"), filezilla_site(server)));
    }
    require_sites(sites)
}

fn field<'a>(node: Node<'a, '_>, key: &str) -> &'a str {
    node.children()
        .find(|child| child.has_tag_name(key))
        .and_then(|child| child.text())
        .unwrap_or("")
        .trim()
}

fn filezilla_site(server: Node<'_, '_>) -> io::Result<FtpProfile> {
    let value = |key| field(server, key);
    let (protocol, tls) = match value("Protocol") {
        // Default FileZilla FTP negotiates explicit TLS when offered. Require it in Arco.
        "" | "0" | "4" => ("5", "3"),
        "1" => ("1", ""),
        "3" => ("5", "1"),
        "6" => ("5", "0"),
        _ => {
            return Err(invalid(
                "Dieses FileZilla-Protokoll kann nicht übernommen werden.",
            ));
        }
    };
    let user = match value("Logontype") {
        "" | "0" if protocol == "5" => "anonymous",
        "1" | "2" | "3" | "5" => value("User"),
        _ => {
            return Err(invalid(
                "Diese FileZilla-Anmeldeart braucht eine manuelle Einrichtung.",
            ));
        }
    };
    if !value("Account").is_empty()
        || !value("PostLoginCommands").is_empty()
        || server.children().any(|node| {
            node.has_tag_name("PostLoginCommands") && node.first_element_child().is_some()
        })
    {
        return Err(invalid(
            "FTP-Konto oder Anmeldebefehle müssen manuell eingerichtet werden.",
        ));
    }
    let remote = filezilla_directory(value("RemoteDir"))?;
    let fields = [
        ("hostname", value("Host")),
        ("portnumber", value("Port")),
        ("username", user),
        ("fsprotocol", protocol),
        ("ftps", tls),
        ("localdirectory", value("LocalDir")),
        ("remotedirectory", &remote),
        ("publickeyfile", value("Keyfile")),
        (
            "ftppasvmode",
            if value("PasvMode") == "MODE_ACTIVE" {
                "0"
            } else {
                "1"
            },
        ),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_owned(), value.to_owned()))
    .collect();
    winscp::site(&fields, &BTreeMap::new())
}

fn filezilla_directory(value: &str) -> io::Result<String> {
    if value.is_empty() || value.starts_with('/') {
        return Ok(value.to_owned());
    }
    let error = || invalid("Dieser FileZilla-Startordner braucht eine manuelle Zuordnung.");
    let (kind, mut rest) = value.split_once(' ').ok_or_else(error)?;
    if !matches!(kind, "0" | "1") {
        return Err(error());
    }
    if rest == "0" {
        return Ok("/".to_owned());
    }
    rest = rest.strip_prefix("0 ").ok_or_else(error)?;
    let mut path = String::new();
    while !rest.is_empty() {
        let (length, tail) = rest.split_once(' ').ok_or_else(error)?;
        let length = length
            .parse::<usize>()
            .ok()
            .filter(|length| *length > 0)
            .ok_or_else(error)?;
        // FileZilla's Windows XML measures each segment in UTF-16 code units.
        let mut units = 0;
        let end = tail
            .char_indices()
            .find_map(|(index, ch)| {
                units += ch.len_utf16();
                (units >= length).then_some(index + ch.len_utf8())
            })
            .filter(|_| units == length)
            .ok_or_else(error)?;
        let (segment, after) = tail.split_at(end);
        if segment.contains('/') {
            return Err(error());
        }
        path.push('/');
        path.push_str(segment);
        rest = if after.is_empty() {
            ""
        } else {
            after.strip_prefix(' ').ok_or_else(error)?
        };
    }
    Ok(path)
}

fn require_sites(sites: Vec<ImportedSite>) -> io::Result<Vec<ImportedSite>> {
    if sites.is_empty() {
        return Err(invalid(
            "Die Quelle enthält keine gespeicherten Verbindungsprofile.",
        ));
    }
    Ok(sites)
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::FtpSecurity;

    #[test]
    fn filezilla_import_preserves_folders_and_unicode_without_passwords() {
        let xml = r#"<FileZilla3><Servers><Folder>Team<Server><Name>Build</Name><Host>build.example</Host>
        <Port>2222</Port><Protocol>1</Protocol><Logontype>1</Logontype><User>work</User>
        <Pass>private</Pass><RemoteDir>1 0 4 a 🗄 7 release</RemoteDir>
        </Server></Folder><Server><Name>Unsupported</Name><Host>other.example</Host>
        <Protocol>2</Protocol></Server></Servers></FileZilla3>"#;
        let sites = parse_filezilla(xml).unwrap();
        let site = sites[0].1.as_ref().unwrap();
        assert_eq!(sites[0].0, "Team/Build");
        assert_eq!(
            site.address,
            "sftp://build.example:2222/a%20%F0%9F%97%84/release"
        );
        assert_eq!(site.security, FtpSecurity::Sftp);
        assert_eq!(site.credential, None);
        assert!(sites[1].1.is_err());
        assert!(
            parse_filezilla("<!DOCTYPE FileZilla3 SYSTEM 'file:///secret'><FileZilla3/>").is_err()
        );
    }

    #[test]
    fn registry_sources_decode_only_their_own_session_fields() {
        let fields = |pairs: &[(&str, &str)]| {
            pairs
                .iter()
                .map(|(key, value)| (key.to_string(), value.to_string()))
                .collect()
        };
        let win_scp = vec![(
            "Build%20Team".into(),
            fields(&[("HostName", "build.example"), ("UserName", "dev%2Bops")]),
        )];
        let putty = vec![(
            "Work%20Lab".into(),
            fields(&[
                ("Protocol", "ssh"),
                ("HostName", "lab.example"),
                ("UserName", "dev%2Bops"),
                ("AuthGSSAPIKEX", "1"),
                ("TryAgent", "1"),
            ]),
        )];
        let win_scp = registry_sites(win_scp, &BTreeMap::new(), true).unwrap();
        let putty = registry_sites(putty, &BTreeMap::new(), false).unwrap();
        assert_eq!(win_scp[0].0, "Build Team");
        assert_eq!(putty[0].0, "Work Lab");
        assert_eq!(win_scp[0].1.as_ref().unwrap().user, "dev+ops");
        assert_eq!(putty[0].1.as_ref().unwrap().user, "dev%2Bops");
        assert!(putty[0].1.as_ref().unwrap().ssh_agent.is_none());
        let kerberos = vec![(
            "Kerberos".into(),
            fields(&[
                ("Protocol", "ssh"),
                ("HostName", "lab.example"),
                ("AuthGSSAPIKEX", "1"),
            ]),
        )];
        let kerberos = registry_sites(kerberos, &BTreeMap::new(), true).unwrap();
        let kerberos = kerberos[0].1.as_ref().unwrap();
        assert!(kerberos.ssh_kerberos_kex);
        assert_eq!(kerberos.ssh_kerberos.as_deref(), Some(""));
        assert!(kerberos.ssh_fingerprint.is_empty());
    }
}
