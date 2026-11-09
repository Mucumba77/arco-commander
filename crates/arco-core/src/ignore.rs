use regex::{RegexSet, RegexSetBuilder};
use std::{io, path::Path};

#[derive(Clone, Copy)]
pub enum Source<'a> {
    Local,
    Wfx(&'a str),
    Ftp {
        server: &'a str,
        profile: Option<&'a str>,
    },
}

enum Scope {
    Local,
    Wfx(Option<String>),
    Ftp(Option<String>),
    FtpProfile(String),
}

impl Scope {
    fn accepts(&self, source: Source<'_>) -> bool {
        match (self, source) {
            (Self::Local, Source::Local) => true,
            (Self::Wfx(name), Source::Wfx(plugin)) => name
                .as_ref()
                .is_none_or(|name| name == &plugin.to_lowercase()),
            (Self::Ftp(host), Source::Ftp { server, .. }) => host
                .as_ref()
                .is_none_or(|host| host.eq_ignore_ascii_case(server)),
            (Self::FtpProfile(name), Source::Ftp { profile, .. }) => {
                profile.is_some_and(|profile| name == &profile.to_lowercase())
            }
            _ => false,
        }
    }
}

pub struct IgnoreList {
    rules: Vec<(Scope, bool)>,
    names: RegexSet,
    paths: RegexSet,
    rooted: RegexSet,
}

impl IgnoreList {
    pub fn read_tc(path: &Path) -> io::Result<String> {
        let data = crate::profile::read_tc_text(path)?;
        Self::parse(&data)?;
        Ok(data)
    }

    pub fn parse(text: &str) -> io::Result<Self> {
        if text.len() > 128 * 1024 {
            return Err(io::Error::other(
                "Die Ignorierliste ist größer als 128 KiB.",
            ));
        }
        let mut rules = Vec::new();
        let mut names = Vec::new();
        let mut paths = Vec::new();
        let mut rooted = Vec::new();
        // Keep each regex index aligned with its rule across the three match sets.
        let never = r"\b\B";
        for (line, text) in text.trim_start_matches('\u{feff}').lines().enumerate() {
            let text = text.trim();
            if text.is_empty() {
                continue;
            }
            let (scope, masks) = scope(text);
            let masks = split_masks(masks)
                .map_err(|error| io::Error::other(format!("Zeile {}: {error}", line + 1)))?;
            for mask in masks {
                let normalized = normalize(&mask);
                let pattern = normalized.trim_end_matches('/');
                if pattern.is_empty() || pattern.chars().any(char::is_control) {
                    return Err(io::Error::other(format!(
                        "Ungültige Ignorierregel in Zeile {}.",
                        line + 1
                    )));
                }
                if rules.len() >= 4096 {
                    return Err(io::Error::other(
                        "Die Ignorierliste erlaubt bis zu 4096 Regeln.",
                    ));
                }
                let full_path = pattern.starts_with("//") || is_drive_path(pattern);
                let path = pattern.contains('/');
                let regex = mask_regex(pattern);
                match (full_path, path) {
                    (true, _) => {
                        names.push(never.to_owned());
                        paths.push(regex);
                        rooted.push(never.to_owned());
                    }
                    (false, true) => {
                        names.push(never.to_owned());
                        paths.push(never.to_owned());
                        rooted.push(if pattern.starts_with('/') {
                            regex
                        } else {
                            format!("^/{rest}", rest = &regex[1..])
                        });
                    }
                    (false, false) => {
                        names.push(regex);
                        paths.push(never.to_owned());
                        rooted.push(never.to_owned());
                    }
                }
                rules.push((parse_scope(scope), normalized.ends_with('/')));
            }
        }
        let compile = |patterns: Vec<String>| {
            RegexSetBuilder::new(patterns)
                .case_insensitive(true)
                .build()
                .map_err(io::Error::other)
        };
        Ok(Self {
            rules,
            names: compile(names)?,
            paths: compile(paths)?,
            rooted: compile(rooted)?,
        })
    }

    pub fn local(&self, path: &Path, directory: bool) -> bool {
        path.to_str()
            .is_some_and(|path| self.matches(Source::Local, path, directory))
    }

    pub(crate) fn has_scope(&self, source: Source<'_>) -> bool {
        self.rules.iter().any(|(scope, _)| scope.accepts(source))
    }

    pub fn matches(&self, source: Source<'_>, path: &str, directory: bool) -> bool {
        let path = normalize(path);
        let path = path.trim_end_matches('/');
        let name = path.rsplit('/').next().unwrap_or(path);
        let rooted = if is_drive_path(path) {
            &path[2..]
        } else if matches!(source, Source::Local) && path.starts_with("//") {
            path[2..]
                .splitn(3, '/')
                .nth(2)
                .map_or("", |relative| &path[path.len() - relative.len() - 1..])
        } else {
            path
        };
        self.names
            .matches(name)
            .iter()
            .chain(self.paths.matches(path).iter())
            .chain(self.rooted.matches(rooted).iter())
            .any(|index| {
                let (scope, folder_only) = &self.rules[index];
                (!folder_only || directory) && scope.accepts(source)
            })
    }
}

fn is_drive_path(path: &str) -> bool {
    path.as_bytes().get(1..3) == Some(b":/")
}

fn normalize(text: &str) -> String {
    let normalized = text.replace('\\', "/");
    if let Some(unc) = normalized.strip_prefix("//?/UNC/") {
        format!("//{unc}")
    } else {
        match normalized.strip_prefix("//?/") {
            Some(path) => path.to_owned(),
            None => normalized,
        }
    }
}

fn scope(text: &str) -> (&str, &str) {
    if let Some((prefix, rest)) = text.split_once(':') {
        if prefix == "FTP" {
            if let Some((host, mask)) = rest.split_once(':') {
                return (&text[..4 + host.len()], mask);
            }
            return ("FTP", rest);
        }
        if !(prefix.len() == 1
            && prefix.as_bytes()[0].is_ascii_alphabetic()
            && rest.starts_with(['\\', '/']))
            && !prefix.contains(['\\', '/', '"'])
        {
            return (prefix, rest);
        }
    }
    ("", text)
}

fn parse_scope(scope: &str) -> Scope {
    match scope {
        "" => Scope::Local,
        "*" => Scope::Wfx(None),
        "FTP" => Scope::Ftp(None),
        scope => {
            if let Some(name) = scope.strip_prefix("FTP:>") {
                Scope::FtpProfile(name.to_lowercase())
            } else if let Some(host) = scope.strip_prefix("FTP:") {
                Scope::Ftp(Some(host.to_owned()))
            } else {
                Scope::Wfx(Some(scope.to_lowercase()))
            }
        }
    }
}

fn split_masks(text: &str) -> Result<Vec<String>, &'static str> {
    let split =
        text.contains(['*', '?']) && !text.trim_end_matches(['\\', '/']).contains(['\\', '/']);
    let mut masks = Vec::new();
    let mut quoted = false;
    let mut mask = String::new();
    for ch in text.chars() {
        match ch {
            '"' => quoted = !quoted,
            ' ' | '\t' if split && !quoted => {
                if !mask.is_empty() {
                    masks.push(std::mem::take(&mut mask));
                }
            }
            _ => mask.push(ch),
        }
    }
    if quoted {
        return Err("Ein schließendes Anführungszeichen fehlt.");
    }
    if !mask.is_empty() {
        masks.push(mask);
    }
    if masks.is_empty() {
        return Err("Die Dateimaske fehlt.");
    }
    Ok(masks)
}

fn mask_regex(pattern: &str) -> String {
    let mut regex = String::from("^");
    let mut segments = pattern.split('/').peekable();
    let mut first = true;
    while let Some(segment) = segments.next() {
        if segment == "*" && !first && segments.peek().is_some() {
            regex.push_str("(?:[^/]+/)*");
            continue;
        }
        for ch in segment.chars() {
            match ch {
                '*' => regex.push_str("[^/]*"),
                '?' => regex.push_str("[^/]"),
                _ => regex.push_str(&regex::escape(&ch.to_string())),
            }
        }
        if segments.peek().is_some() {
            regex.push('/');
        }
        first = false;
    }
    regex.push('$');
    regex
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supports_total_commander_scopes_and_path_masks() {
        let rules = IgnoreList::parse(concat!(
            "\u{feff}*.bak *.old\n",
            r#""System Volume*""#,
            "\n",
            r"\RECYCLER\",
            "\n",
            r"C:\work\*\private?.txt",
            "\n",
            r"\\NAS\share\local.dat",
            "\n",
            r"*:\*\thumbs.db",
            "\n",
            r"SFTP:\host\*\temp\",
            "\n",
            "FTP:sample.org:*.log\n",
            "FTP:>Bilder:cache\n"
        ))
        .unwrap();
        let local = |path, dir| rules.matches(Source::Local, path, dir);
        assert!(local(r"D:\cache\x.OLD", false));
        assert!(local(r"D:\System Volume Information", true));
        assert!(local(r"D:\RECYCLER", true));
        assert!(!local(r"D:\nested\RECYCLER", true));
        assert!(!local(r"D:\RECYCLER", false));
        assert!(local(r"C:\work\one\two\private1.txt", false));
        assert!(local(r"C:\work\private2.txt", false));
        assert!(local(r"\\?\UNC\NAS\share\local.dat", false));
        assert!(!local(r"D:\work\private1.txt", false));
        assert!(rules.matches(Source::Wfx("sftp"), r"\host\test\TEMP", true));
        assert!(rules.matches(Source::Wfx("WebDAV"), r"\host\thumbs.db", false));
        assert!(!rules.matches(Source::Wfx("WebDAV"), r"\host\temp", true));
        assert!(!rules.matches(Source::Wfx("sftp"), r"\host\x.old", false));
        let ftp = Source::Ftp {
            server: "Sample.Org",
            profile: Some("bilder"),
        };
        assert!(rules.matches(ftp, "/logs/test.log", false));
        assert!(rules.matches(ftp, "/cache", true));
        assert!(!rules.matches(
            Source::Ftp {
                server: "other.org",
                profile: None
            },
            "/cache",
            true
        ));
    }
}
