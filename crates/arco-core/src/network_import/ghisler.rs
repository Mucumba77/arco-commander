use super::{SESSION_LIMIT, invalid, require_sites};
use crate::profile::{self, FtpProfile, SshAgent};
use crate::winscp::{self, ImportedSite};
use std::collections::BTreeMap;
use std::io;
use std::path::Path;
use url::Url;
use zeroize::Zeroizing;

pub fn read_ghisler_sftp(path: &Path) -> io::Result<Vec<ImportedSite>> {
    parse(&Zeroizing::new(profile::read_tc_text(path)?))
}

fn parse(text: &str) -> io::Result<Vec<ImportedSite>> {
    let mut sections = Vec::<(&str, BTreeMap<String, &str>)>::new();
    for line in text.lines().map(str::trim) {
        if let Some(name) = line
            .strip_prefix('[')
            .and_then(|name| name.strip_suffix(']'))
        {
            if sections.len() >= SESSION_LIMIT {
                return Err(invalid(
                    "Die Total-Commander-Datei enthält mehr als 2000 Abschnitte.",
                ));
            }
            sections.push((name, BTreeMap::new()));
        } else if let Some((key, value)) = line.split_once('=')
            && let Some((_, fields)) = sections.last_mut()
        {
            let key = key.trim().to_ascii_lowercase();
            if matches!(
                key.as_str(),
                "server"
                    | "user"
                    | "protocol"
                    | "fingerprint"
                    | "privkeyfile"
                    | "useagent"
                    | "scponly"
                    | "keepaliveseconds"
                    | "proxynr"
                    | "proxytype"
                    | "sendcommand"
                    | "scpfordata"
            ) {
                fields.entry(key).or_insert(value.trim().trim_matches('"'));
            }
        }
    }
    let default_proxy = sections.iter().any(|(name, fields)| {
        name.eq_ignore_ascii_case("proxy")
            && fields.get("proxytype").is_some_and(|value| *value != "0")
    });
    let sites = sections
        .into_iter()
        .filter(|(_, fields)| fields.contains_key("server"))
        .map(|(name, fields)| (name.to_owned(), site(&fields, default_proxy)))
        .collect();
    require_sites(sites)
}

fn site(fields: &BTreeMap<String, &str>, default_proxy: bool) -> io::Result<FtpProfile> {
    let value = |key| fields.get(key).copied().unwrap_or_default();
    if !matches!(value("proxynr"), "" | "0" | "1")
        || (default_proxy && value("proxynr") != "0")
        || !value("sendcommand").is_empty()
        || (value("scpfordata") == "1" && value("scponly") != "1")
    {
        return Err(invalid(
            "Proxy, Anmeldebefehl oder gemischter SCP/SFTP-Modus müssen manuell eingerichtet werden.",
        ));
    }
    let server = value("server");
    let server = server
        .strip_prefix("sftp://")
        .or_else(|| server.strip_prefix("scp://"))
        .unwrap_or(server);
    let (authority, remote) = server
        .find(['/', '\\'])
        .map_or((server, ""), |index| server.split_at(index));
    if authority.contains(['@', '#', '?', '%']) || authority.chars().any(char::is_whitespace) {
        return Err(invalid("Ungültiger Total-Commander-Servername."));
    }
    let address = Url::parse(&format!("sftp://{authority}/"))
        .map_err(|_| invalid("Ungültiger Total-Commander-Servername."))?;
    let remote = remote.replace('\\', "/");
    let port = address
        .port()
        .map(|port| port.to_string())
        .unwrap_or_default();
    let data = [
        ("hostname", address.host_str().unwrap_or_default()),
        ("portnumber", &port),
        ("username", value("user")),
        ("remotedirectory", &remote),
        ("publickeyfile", value("privkeyfile")),
        ("sshhostkey", value("fingerprint")),
        (
            "fsprotocol",
            if value("scponly") == "1" { "0" } else { "1" },
        ),
        ("addressfamily", value("protocol")),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_owned(), value.to_owned()))
    .collect();
    let mut profile = winscp::site(&data, &BTreeMap::new())?;
    if profile.ssh_private_key.is_none() && value("useagent") == "1" {
        profile.ssh_agent = Some(SshAgent::Pageant);
    }
    profile.ssh_keepalive = value("keepaliveseconds")
        .parse()
        .ok()
        .filter(|seconds| (1..=300).contains(seconds));
    Ok(profile)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imports_endpoint_without_legacy_pins_or_commands() {
        let data = "[Team]\nserver=[::1]:2222/Archive 1\nuser=dev\npassword=secret\nfingerprint=00:11:22\nkeepaliveseconds=30\n[Other]\nserver=example.org\nsendcommand=touch file";
        let sites = parse(data).unwrap();
        let team = sites[0].1.as_ref().unwrap();
        assert_eq!(team.address, "sftp://[::1]:2222/Archive%201");
        assert_eq!(team.ssh_keepalive, Some(30));
        assert!(team.ssh_fingerprint.is_empty());
        assert_eq!(team.credential, None);
        assert!(sites[1].1.is_err());
    }
}
