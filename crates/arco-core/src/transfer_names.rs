use crate::files::{leaf_name, windows_device_name};
use serde::{Deserialize, Serialize};
use std::ffi::OsStr;
use std::fmt::Write as _;
use std::io;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NameCase {
    #[default]
    Preserve,
    Lower,
    Upper,
    FirstUpper,
    LowerDos,
}

impl NameCase {
    pub const ALL: [Self; 5] = [
        Self::Preserve,
        Self::Lower,
        Self::Upper,
        Self::FirstUpper,
        Self::LowerDos,
    ];

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "preserve" => Self::Preserve,
            "lower" => Self::Lower,
            "upper" => Self::Upper,
            "first-upper" => Self::FirstUpper,
            "lower-dos" => Self::LowerDos,
            _ => return None,
        })
    }

    pub fn index(self) -> i32 {
        Self::ALL.iter().position(|case| *case == self).unwrap() as i32
    }

    pub fn from_index(index: i32) -> Self {
        Self::ALL.get(index as usize).copied().unwrap_or_default()
    }

    fn convert(self, name: &str, upload: bool) -> String {
        match self {
            Self::Upper => name.to_uppercase(),
            Self::Lower => name.to_lowercase(),
            Self::FirstUpper => {
                let mut chars = name.chars();
                chars
                    .next()
                    .into_iter()
                    .flat_map(char::to_uppercase)
                    .chain(chars.as_str().to_lowercase().chars())
                    .collect()
            }
            Self::LowerDos if upload && dos_upper_name(name) => name.to_ascii_lowercase(),
            _ => name.to_owned(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct NameRules {
    pub case: NameCase,
    pub windows: bool,
}

impl NameRules {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    pub fn download(self, remote: &str) -> io::Result<String> {
        remote_leaf(remote)?;
        let name = self.case.convert(remote, false);
        let name = if self.windows {
            encode_windows(&name)
        } else {
            name
        };
        validate_local(&name)?;
        Ok(name)
    }

    pub fn upload(self, local: &str) -> io::Result<String> {
        validate_local(local)?;
        let name = if self.windows {
            decode_windows(local)
        } else {
            local.to_owned()
        };
        let name = self.case.convert(&name, true);
        remote_leaf(&name)?;
        Ok(name)
    }
}

pub fn remote_leaf(name: &str) -> io::Result<()> {
    if name.is_empty()
        || matches!(name, "." | "..")
        || name.contains(['/', '\\'])
        || name.chars().any(char::is_control)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Der Servername muss ein einzelner gültiger Dateiname sein.",
        ));
    }
    Ok(())
}

fn validate_local(name: &str) -> io::Result<()> {
    leaf_name(OsStr::new(name)).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("„{name}“ ist kein gültiger Windows-Dateiname."),
        )
    })?;
    if name.encode_utf16().count() > 255 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Der Windows-Zielname ist länger als 255 Zeichen.",
        ));
    }
    Ok(())
}

const ESCAPED: &str = "%<>:\"|?*";

fn encode_windows(name: &str) -> String {
    let reserved = windows_device_name(name);
    let suffix = name.trim_end_matches(['.', ' ']).len();
    let extension = reserved.then(|| name.find('.').unwrap_or(name.len()));
    let mut encoded = String::with_capacity(name.len());
    for (index, ch) in name.char_indices() {
        if extension == Some(index) {
            encoded.push_str("%00");
        }
        if ESCAPED.contains(ch) || index >= suffix {
            let _ = write!(encoded, "%{:02X}", ch as u32);
        } else {
            encoded.push(ch);
        }
    }
    if extension == Some(name.len()) {
        encoded.push_str("%00");
    }
    encoded
}

fn decode_windows(name: &str) -> String {
    let mut result = String::with_capacity(name.len());
    let mut input = name.chars();
    while let Some(ch) = input.next() {
        let decoded = input
            .as_str()
            .get(..2)
            .filter(|_| ch == '%')
            .and_then(|digits| u8::from_str_radix(digits, 16).ok());
        match decoded.map(char::from) {
            Some('\0') => {
                input.nth(1);
            }
            Some(literal) if ESCAPED.contains(literal) || matches!(literal, '.' | ' ') => {
                input.nth(1);
                result.push(literal);
            }
            _ => result.push(ch),
        }
    }
    // Accept a decoding only when the resulting name encodes back to the source.
    // This keeps ordinary percent text, encoded separators and stray %00 tokens literal.
    if remote_leaf(&result).is_ok() && encode_windows(&result).eq_ignore_ascii_case(name) {
        result
    } else {
        name.to_owned()
    }
}

fn dos_upper_name(name: &str) -> bool {
    let (base, ext) = name
        .split_once('.')
        .map_or((name, None), |(base, ext)| (base, Some(ext)));
    let valid = |part: &str, max: usize| {
        (1..=max).contains(&part.len())
            && part.bytes().all(|byte| {
                byte.is_ascii_uppercase()
                    || byte.is_ascii_digit()
                    || b"!#$%&'()-@^_{}~".contains(&byte)
            })
    };
    valid(base, 8) && ext.is_none_or(|ext| valid(ext, 3))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reversible_windows_names_leave_separators_and_incomplete_tokens_literal() {
        let rules = NameRules {
            windows: true,
            ..Default::default()
        };
        for (raw, safe) in [
            ("C:", "C%3A"),
            ("Plan?.txt", "Plan%3F.txt"),
            ("NUL.txt", "NUL%00.txt"),
            ("COM¹ .txt", "COM¹ %00.txt"),
            ("report. ", "report%2E%20"),
            ("%00", "%2500"),
            ("PRN", "PRN%00"),
            ("literal%3A.txt", "literal%253A.txt"),
        ] {
            assert_eq!(rules.download(raw).unwrap(), safe);
            assert_eq!(rules.upload(safe).unwrap(), raw);
        }
        for literal in ["%2E%2E", "a%2Fb", "%5C", "safe%00", "normal%20name.txt"] {
            assert_eq!(rules.upload(literal).unwrap(), literal);
        }
        assert!(rules.download("..").is_err());
        assert!(rules.download("outside/name").is_err());
    }

    #[test]
    fn applies_case_in_the_appropriate_direction() {
        let dos = NameRules {
            case: NameCase::LowerDos,
            ..Default::default()
        };
        assert_eq!(dos.upload("README.TXT").unwrap(), "readme.txt");
        assert_eq!(dos.upload("LongFilename.TXT").unwrap(), "LongFilename.TXT");
        assert_eq!(dos.download("README.TXT").unwrap(), "README.TXT");
        let upper = NameRules {
            case: NameCase::Upper,
            windows: true,
        };
        assert_eq!(upper.download("Straße?.txt").unwrap(), "STRASSE%3F.TXT");
    }
}
