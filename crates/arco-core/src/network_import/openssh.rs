use super::{SESSION_LIMIT, invalid, require_sites};
use crate::profile::{self, FtpProfile, SshAgent};
use crate::ssh_host_keys::HostKeyPins;
use crate::winscp::{self, ImportedSite};
use base64::{
    Engine,
    engine::general_purpose::{STANDARD, STANDARD_NO_PAD},
};
use globset::{GlobBuilder, GlobMatcher};
use hmac::{Hmac, KeyInit, Mac};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};
use url::Url;

pub fn read_openssh(path: &Path) -> io::Result<Vec<ImportedSite>> {
    let keys = match KnownHosts::read(&path.with_file_name("known_hosts")) {
        Ok(keys) => keys,
        Err(error) if error.kind() == io::ErrorKind::NotFound => KnownHosts::default(),
        Err(error) => return Err(error),
    };
    parse_config(&profile::read_tc_text(path)?, &keys)
}

#[derive(Default)]
pub struct KnownHosts(Vec<KnownHost>);

struct KnownHost {
    hosts: String,
    revoked: bool,
    fingerprint: String,
}

impl KnownHosts {
    pub fn read(path: &Path) -> io::Result<Self> {
        Self::parse(&profile::read_tc_text(path)?)
    }

    fn parse(text: &str) -> io::Result<Self> {
        let mut keys = Vec::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut fields = line.split_whitespace();
            let Some(mut hosts) = fields.next() else {
                continue;
            };
            let revoked = hosts == "@revoked";
            if revoked {
                hosts = fields
                    .next()
                    .ok_or_else(|| invalid("Ungültiger @revoked-Eintrag in known_hosts."))?;
            } else if hosts.starts_with('@') {
                continue;
            }
            let (Some(kind), Some(encoded)) = (fields.next(), fields.next()) else {
                return Err(invalid(
                    "Ein known_hosts-Eintrag enthält keinen öffentlichen Schlüssel.",
                ));
            };
            if !matches!(
                kind,
                "ssh-ed25519"
                    | "ecdsa-sha2-nistp256"
                    | "ecdsa-sha2-nistp384"
                    | "ecdsa-sha2-nistp521"
                    | "ssh-rsa"
            ) {
                continue;
            }
            if encoded.len() > 8192 || keys.len() >= 20_000 {
                return Err(invalid("known_hosts überschreitet die Schlüsselgrenze."));
            }
            let blob = decode(encoded)
                .ok_or_else(|| invalid("Ungültiger öffentlicher Schlüssel in known_hosts."))?;
            let len = u32::try_from(kind.len()).unwrap_or_default().to_be_bytes();
            if !blob.starts_with(&len) || blob.get(4..4 + kind.len()) != Some(kind.as_bytes()) {
                return Err(invalid(
                    "Schlüsseltyp und Schlüsseldaten in known_hosts stimmen nicht überein.",
                ));
            }
            keys.push(KnownHost {
                hosts: hosts.to_owned(),
                revoked,
                fingerprint: format!("SHA256:{}", STANDARD_NO_PAD.encode(Sha256::digest(blob))),
            });
        }
        Ok(Self(keys))
    }

    pub fn fingerprint(&self, endpoint: &str) -> io::Result<Option<String>> {
        let url = Url::parse(endpoint).map_err(|_| invalid("Ungültige SSH-Adresse."))?;
        let host = url
            .host_str()
            .ok_or_else(|| invalid("Die SSH-Adresse enthält keinen Server."))?;
        let host = host.trim_start_matches('[').trim_end_matches(']');
        let port = url.port().unwrap_or(22);
        let identity = if port == 22 {
            host.to_owned()
        } else {
            format!("[{host}]:{port}")
        };
        self.lookup(&identity)
    }

    fn lookup(&self, identity: &str) -> io::Result<Option<String>> {
        let revoked: BTreeSet<_> = self
            .0
            .iter()
            .filter(|key| key.revoked)
            .map(|key| key.fingerprint.as_str())
            .collect();
        let mut candidates = BTreeSet::new();
        for key in &self.0 {
            if !key.revoked && known_host_matches(&key.hosts, identity) {
                if revoked.contains(key.fingerprint.as_str()) {
                    return Err(invalid(
                        "known_hosts kennzeichnet einen passenden Schlüssel als widerrufen.",
                    ));
                }
                candidates.insert(key.fingerprint.as_str());
            }
        }
        if candidates.is_empty() {
            return Ok(None);
        }
        let pins = candidates.into_iter().collect::<Vec<_>>().join(";");
        Ok(Some(HostKeyPins::parse(&pins)?.to_string()))
    }
}

fn decode(value: &str) -> Option<Vec<u8>> {
    STANDARD
        .decode(value)
        .or_else(|_| STANDARD_NO_PAD.decode(value))
        .ok()
}

fn known_host_matches(list: &str, identity: &str) -> bool {
    if let Some((salt, hash)) = list
        .strip_prefix("|1|")
        .and_then(|value| value.split_once('|'))
    {
        let (Some(salt), Some(hash)) = (decode(salt), decode(hash)) else {
            return false;
        };
        if salt.len() != 20 || hash.len() != 20 {
            return false;
        }
        let Ok(mut mac) = Hmac::<sha1_modern::Sha1>::new_from_slice(&salt) else {
            return false;
        };
        mac.update(identity.as_bytes());
        return mac.verify_slice(&hash).is_ok();
    }
    // Copy only concrete host keys. A wildcard is too broad for an explicit profile pin.
    list.split(',')
        .any(|pattern| pattern.eq_ignore_ascii_case(identity))
        && !list.split(',').any(|pattern| pattern.starts_with('!'))
}

#[derive(Default)]
struct Section {
    patterns: Vec<(bool, GlobMatcher)>,
    fields: Vec<(String, String)>,
}

impl Section {
    fn matches(&self, alias: &str) -> bool {
        let mut matched = self.patterns.is_empty();
        for (excluded, pattern) in &self.patterns {
            if pattern.is_match(alias) {
                if *excluded {
                    return false;
                }
                matched = true;
            }
        }
        matched
    }
}

fn parse_config(config: &str, keys: &KnownHosts) -> io::Result<Vec<ImportedSite>> {
    let mut sections = vec![Section::default()];
    let mut aliases = BTreeMap::new();
    for (index, line) in config.lines().enumerate() {
        if index >= 12_000 {
            return Err(invalid(
                "Die OpenSSH-Konfiguration enthält mehr als 12000 Zeilen.",
            ));
        }
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let key_end = trimmed
            .find(|ch: char| ch.is_whitespace() || ch == '=')
            .unwrap_or(trimmed.len());
        let directive = trimmed[..key_end].to_ascii_lowercase();
        let value = trimmed[key_end..]
            .trim_start()
            .trim_start_matches('=')
            .trim_start();
        let words = config_words(value)?;
        if matches!(directive.as_str(), "include" | "match") {
            return Err(invalid(
                "OpenSSH Include und Match brauchen einen separaten Export ohne diese Anweisungen.",
            ));
        }
        if directive == "host" {
            let mut section = Section::default();
            for word in words {
                let excluded = word.starts_with('!');
                let pattern = word.strip_prefix('!').unwrap_or(&word);
                if !pattern.contains(['*', '?']) && !excluded {
                    aliases
                        .entry(word.to_ascii_lowercase())
                        .or_insert_with(|| word.clone());
                }
                let escaped = pattern
                    .replace('[', "\\[")
                    .replace(']', "\\]")
                    .replace('{', "\\{")
                    .replace('}', "\\}");
                let glob = GlobBuilder::new(&escaped)
                    .case_insensitive(true)
                    .build()
                    .map_err(|_| invalid("Ungültiges Host-Muster in der OpenSSH-Konfiguration."))?;
                section.patterns.push((excluded, glob.compile_matcher()));
            }
            if section.patterns.is_empty() || aliases.len() > SESSION_LIMIT {
                return Err(invalid(
                    "Ungültiger Host-Abschnitt oder mehr als 2000 OpenSSH-Profile.",
                ));
            }
            sections.push(section);
        } else if !words.is_empty() {
            sections
                .last_mut()
                .expect("global section")
                .fields
                .push((directive, words.join(" ")));
        }
    }
    require_sites(
        aliases
            .into_values()
            .map(|alias| {
                let result = ssh_site(&alias, &sections, keys);
                (alias, result)
            })
            .collect(),
    )
}

fn config_words(input: &str) -> io::Result<Vec<String>> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quoted = false;
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '"' => quoted = !quoted,
            '\\' if chars
                .peek()
                .is_some_and(|next| matches!(next, '"' | '#' | ' ' | '\\')) =>
            {
                word.push(chars.next().expect("peeked character"));
            }
            '#' if !quoted => break,
            ch if ch.is_whitespace() && !quoted => {
                if !word.is_empty() {
                    words.push(std::mem::take(&mut word));
                }
            }
            ch => word.push(ch),
        }
    }
    if quoted {
        return Err(invalid(
            "Ein Anführungszeichen in der OpenSSH-Konfiguration ist offen.",
        ));
    }
    if !word.is_empty() {
        words.push(word);
    }
    Ok(words)
}

fn ssh_site(alias: &str, sections: &[Section], keys: &KnownHosts) -> io::Result<FtpProfile> {
    let mut options = BTreeMap::new();
    let mut identity_files = BTreeSet::new();
    for section in sections.iter().filter(|section| section.matches(alias)) {
        for (key, value) in &section.fields {
            options.entry(key.as_str()).or_insert(value.as_str());
            if key == "identityfile" {
                identity_files.insert(value.as_str());
            }
        }
    }
    let value = |key| options.get(key).copied().unwrap_or_default();
    for key in [
        "proxyjump",
        "proxycommand",
        "identityagent",
        "canonicaldomains",
        "userknownhostsfile",
        "globalknownhostsfile",
        "knownhostscommand",
        "revokedhostkeys",
        "hostkeyalgorithms",
        "kexalgorithms",
        "requiredrsasize",
        "certificatefile",
    ] {
        if !matches!(value(key), "" | "none") {
            return Err(invalid(
                "Die SSH-Konfiguration enthält Verbindungs-, Schlüssel- oder Proxyvorgaben, die manuell eingerichtet werden müssen.",
            ));
        }
    }
    if value("canonicalizehostname") == "yes" || !value("gssapiserveridentity").is_empty() {
        return Err(invalid(
            "Ein abweichender Kerberos-Dienstprinzipal oder DNS-Kanonisierung braucht eine manuelle Einrichtung.",
        ));
    }
    if identity_files.len() > 1 {
        return Err(invalid(
            "Mehrere OpenSSH-Schlüsseldateien brauchen eine manuelle Zuordnung.",
        ));
    }
    let keyfile = match identity_files.first().copied().unwrap_or_default() {
        "" | "none" => PathBuf::new(),
        key if key.contains(['%', '$']) => {
            return Err(invalid("Der OpenSSH-Schlüsselpfad enthält Platzhalter."));
        }
        key if key.starts_with("~/") || key.starts_with("~\\") => {
            let home = std::env::var_os("USERPROFILE")
                .or_else(|| std::env::var_os("HOME"))
                .ok_or_else(|| {
                    invalid("Das Benutzerverzeichnis für die SSH-Schlüsseldatei fehlt.")
                })?;
            PathBuf::from(home).join(&key[2..])
        }
        key => PathBuf::from(key),
    };
    let host = if value("hostname").is_empty() {
        alias.to_owned()
    } else {
        value("hostname").replace("%h", alias)
    };
    let fields = [
        ("hostname", host),
        ("portnumber", value("port").to_owned()),
        (
            "username",
            if value("user").is_empty() {
                std::env::var("USERNAME")
                    .or_else(|_| std::env::var("USER"))
                    .unwrap_or_default()
            } else {
                value("user").to_owned()
            },
        ),
        (
            "publickeyfile",
            keyfile
                .into_os_string()
                .into_string()
                .map_err(|_| invalid("Ungültiger SSH-Schlüsselpfad."))?,
        ),
        ("timeout", value("connecttimeout").to_owned()),
        (
            "addressfamily",
            match value("addressfamily") {
                "inet" => "1",
                "inet6" => "2",
                _ => "",
            }
            .to_owned(),
        ),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_owned(), value))
    .collect();
    let mut site = winscp::site(&fields, &BTreeMap::new())?;
    let pin = if value("hostkeyalias").is_empty() {
        keys.fingerprint(&site.address)?
    } else {
        keys.lookup(value("hostkeyalias"))?
    };
    site.ssh_fingerprint = pin.unwrap_or_default();
    site.ssh_keepalive = value("serveraliveinterval")
        .parse()
        .ok()
        .filter(|seconds| (1..=300).contains(seconds));
    site.ssh_kerberos_kex = value("gssapikeyexchange") == "yes";
    if site.ssh_kerberos_kex && site.ssh_private_key.is_some() {
        return Err(invalid(
            "GSSAPI-Schlüsselaustausch in Kombination mit einer privaten SSH-Schlüsseldatei braucht eine manuelle Zuordnung.",
        ));
    }
    if site.ssh_kerberos_kex || value("gssapiauthentication") == "yes" {
        site.ssh_kerberos = Some(String::new());
        site.ssh_kerberos_delegate = value("gssapidelegatecredentials") == "yes";
    } else if value("kbdinteractiveauthentication") == "yes" {
        site.ssh_interactive = true;
    } else if site.ssh_private_key.is_none()
        && value("pubkeyauthentication") == "yes"
        && value("identityagent") != "none"
    {
        site.ssh_agent = Some(SshAgent::OpenSsh);
    }
    Ok(site)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ed25519(byte: u8) -> String {
        let mut blob = b"\0\0\0\x0bssh-ed25519\0\0\0\x20".to_vec();
        blob.extend([byte; 32]);
        STANDARD.encode(blob)
    }

    #[test]
    fn ssh_config_uses_first_value_and_host_port_cache_binding() {
        let good = ed25519(1);
        let other = ed25519(2);
        let keys = KnownHosts::parse(&format!(
            "build.example ssh-ed25519 {other}\n[build.example]:2222 ssh-ed25519 {good}"
        ))
        .unwrap();
        let sites = parse_config("Host build\n HostName=build.example\n Port 2222\n User dev\n Host * !build\n User ignored\n Host *\n User fallback\n ServerAliveInterval 30", &keys).unwrap();
        let profile = sites[0].1.as_ref().unwrap();
        assert_eq!(profile.user, "dev");
        assert_eq!(profile.address, "sftp://build.example:2222/");
        assert_eq!(profile.ssh_keepalive, Some(30));
        assert_eq!(
            Some(&profile.ssh_fingerprint),
            keys.lookup("[build.example]:2222").unwrap().as_ref()
        );
        assert_ne!(
            Some(&profile.ssh_fingerprint),
            keys.lookup("build.example").unwrap().as_ref()
        );
        assert!(parse_config("Include other.conf\nHost build", &keys).is_err());
        assert!(
            parse_config("Host build\nProxyCommand example", &keys).unwrap()[0]
                .1
                .is_err()
        );
    }

    #[test]
    fn known_hosts_binds_hashes_preserves_alternatives_and_rejects_revoked_keys() {
        let salt = [7; 20];
        let mut mac = Hmac::<sha1_modern::Sha1>::new_from_slice(&salt).unwrap();
        mac.update(b"[build.example]:2222");
        let hash = mac.finalize().into_bytes();
        let pattern = format!("|1|{}|{}", STANDARD.encode(salt), STANDARD.encode(hash));
        let key = ed25519(1);
        let other = ed25519(2);
        let keys = KnownHosts::parse(&format!("{pattern} ssh-ed25519 {key}")).unwrap();
        assert!(keys.lookup("build.example").unwrap().is_none());
        assert!(keys.lookup("[build.example]:2222").unwrap().is_some());
        let keys = KnownHosts::parse(&format!(
            "build.example ssh-ed25519 {key}\nbuild.example ssh-ed25519 {other}"
        ))
        .unwrap();
        assert_eq!(
            keys.lookup("build.example")
                .unwrap()
                .unwrap()
                .split(';')
                .count(),
            2
        );
        let keys = KnownHosts::parse(&format!(
            "build.example ssh-ed25519 {key}\n@revoked * ssh-ed25519 {key}"
        ))
        .unwrap();
        assert!(keys.lookup("build.example").is_err());
    }
}
