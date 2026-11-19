use crate::profile::FtpTransfer;
use crate::sync::FileMasks;
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::io;
use std::path::Path;

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TransferPreset {
    pub name: String,
    pub settings: FtpTransfer,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rule: Option<TransferRule>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TransferRule {
    pub host: String,
    pub user: String,
    pub remote: String,
    pub local: String,
}

pub struct TransferContext<'a> {
    pub host: &'a str,
    pub user: &'a str,
    pub remote: &'a str,
    pub local: Option<&'a Path>,
}

pub fn validate(presets: &[TransferPreset]) -> io::Result<()> {
    let mut names = BTreeSet::new();
    for preset in presets {
        let name = preset.name.trim();
        if name.is_empty()
            || name != preset.name
            || name.chars().count() > 80
            || name.chars().any(char::is_control)
        {
            return Err(invalid(
                "Gib der Transfervorgabe einen Namen mit höchstens 80 Zeichen.",
            ));
        }
        if !names.insert(name.to_lowercase()) {
            return Err(invalid(
                "Dieser Name ist bereits für eine Transfervorgabe vergeben.",
            ));
        }
        if preset
            .settings
            .speed_kib
            .is_some_and(|rate| !(1..=1_048_576).contains(&rate))
        {
            return Err(invalid(
                "Das Durchsatzlimit muss zwischen 1 und 1048576 KiB/s liegen.",
            ));
        }
        if preset.settings.masks.len() > 4096 {
            return Err(invalid("Die Transfermaske ist zu lang."));
        }
        FileMasks::new(&preset.settings.masks)?;
        if let Some(rule) = &preset.rule {
            Rule::new(rule)?;
        }
    }
    Ok(())
}

pub fn select<'a>(
    presets: &'a [TransferPreset],
    explicit: Option<&str>,
    context: TransferContext<'_>,
) -> io::Result<Option<&'a TransferPreset>> {
    if let Some(name) = explicit {
        return presets
            .iter()
            .find(|preset| preset.name == name)
            .map(Some)
            .ok_or_else(|| invalid(format!("Die Transfervorgabe „{name}“ fehlt.")));
    }
    let local = context
        .local
        .map(|path| local_path(&path.to_string_lossy()));
    let remote = context.remote.trim_end_matches('/');
    for preset in presets {
        if let Some(rule) = &preset.rule {
            let rule = Rule::new(rule)?;
            if rule.host.matches(Some(context.host.trim_end_matches('.')))
                && rule.user.matches(Some(context.user))
                && rule
                    .remote
                    .matches(Some(if remote.is_empty() { "/" } else { remote }))
                && rule.local.matches(local.as_deref())
            {
                return Ok(Some(preset));
            }
        }
    }
    Ok(None)
}

fn local_path(value: &str) -> String {
    let value = value.replace('\\', "/");
    let path = if let Some(share) = value.strip_prefix("//?/UNC/") {
        format!("//{share}")
    } else {
        value.strip_prefix("//?/").unwrap_or(&value).to_owned()
    };
    if (path.len() == 3 && path.ends_with(":/")) || path == "/" {
        path
    } else {
        path.trim_end_matches('/').to_owned()
    }
}

struct Rule {
    host: Mask,
    user: Mask,
    remote: Mask,
    local: Mask,
}

impl Rule {
    fn new(rule: &TransferRule) -> io::Result<Self> {
        Ok(Self {
            host: Mask::new(&rule.host, true, false)?,
            user: Mask::new(&rule.user, false, false)?,
            remote: Mask::new(&rule.remote, false, true)?,
            local: Mask::new(&rule.local.replace('\\', "/"), true, true)?,
        })
    }
}

struct Mask {
    include: GlobSet,
    exclude: GlobSet,
}

impl Mask {
    fn new(value: &str, insensitive: bool, path: bool) -> io::Result<Self> {
        if value.len() > 4096 {
            return Err(invalid("Die Maske der Auswahlregel ist zu lang."));
        }
        let (include, exclude) = value.split_once('|').unwrap_or((value, ""));
        if exclude.contains('|') {
            return Err(invalid("Trenne Einschluss und Ausschluss mit einem |."));
        }
        let compile = |part: &str| -> io::Result<GlobSet> {
            let mut builder = GlobSetBuilder::new();
            for pattern in part
                .split(';')
                .map(str::trim)
                .filter(|part| !part.is_empty())
            {
                let pattern = if path && pattern != "/" && !pattern.ends_with(":/") {
                    pattern.trim_end_matches('/')
                } else {
                    pattern
                };
                builder.add(
                    GlobBuilder::new(pattern)
                        .case_insensitive(insensitive)
                        .literal_separator(path && pattern != "*")
                        .build()
                        .map_err(io::Error::other)?,
                );
            }
            builder.build().map_err(io::Error::other)
        };
        Ok(Self {
            include: compile(include)?,
            exclude: compile(exclude)?,
        })
    }

    fn matches(&self, value: Option<&str>) -> bool {
        value.map_or(
            self.include.is_empty() && self.exclude.is_empty(),
            |value| {
                (self.include.is_empty() || self.include.is_match(value))
                    && !self.exclude.is_match(value)
            },
        )
    }
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordered_rules_match_all_fields_and_skip_missing_folders() {
        let rule = TransferRule {
            host: "*.example.org | backup.example.org".into(),
            user: "dev;publish".into(),
            remote: "/Projects/**".into(),
            local: r"C:\Projects\**".into(),
        };
        let presets = [
            TransferPreset {
                name: "Projects".into(),
                rule: Some(rule),
                ..Default::default()
            },
            TransferPreset {
                name: "Fallback".into(),
                rule: Some(TransferRule::default()),
                ..Default::default()
            },
            TransferPreset {
                name: "Manual".into(),
                ..Default::default()
            },
        ];
        validate(&presets).unwrap();
        let choose = |host, user, remote, local, explicit| {
            select(
                &presets,
                explicit,
                TransferContext {
                    host,
                    user,
                    remote,
                    local,
                },
            )
            .unwrap()
            .unwrap()
            .name
            .as_str()
        };
        let local = Some(Path::new(r"\\?\c:\PROJECTS\Team\build\"));
        assert_eq!(
            choose(
                "FTP.EXAMPLE.ORG.",
                "dev",
                "/Projects/Team/build/",
                local,
                None
            ),
            "Projects"
        );
        assert_eq!(
            choose("backup.example.org", "dev", "/Projects/Team/", local, None),
            "Fallback"
        );
        assert_eq!(
            choose("ftp.example.org", "other", "/Projects/Team/", local, None),
            "Fallback"
        );
        assert_eq!(
            choose("ftp.example.org", "dev", "/projects/Team/", local, None),
            "Fallback"
        );
        assert_eq!(
            choose("ftp.example.org", "dev", "/Projects/Team/", None, None),
            "Fallback"
        );
        assert_eq!(
            choose(
                "ftp.example.org",
                "dev",
                "/Projects/Team/",
                local,
                Some("Manual")
            ),
            "Manual"
        );
    }

    #[test]
    fn ambiguous_names_or_invalid_patterns_cannot_be_saved() {
        let first = TransferPreset {
            name: "Upload".into(),
            ..Default::default()
        };
        let mut second = TransferPreset {
            name: "upload".into(),
            ..Default::default()
        };
        assert!(validate(&[first.clone(), second.clone()]).is_err());
        second.name = "Second".into();
        second.rule = Some(TransferRule {
            remote: "[broken".into(),
            ..Default::default()
        });
        assert!(validate(&[first, second]).is_err());
    }
}
