use super::{counter, name_range};
use serde::{Deserialize, Serialize};
use std::io;
use std::path::{Component, Path};

pub(super) fn full_path(path: &Path) -> String {
    let value = path.to_string_lossy();
    if let Some(rest) = value.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else {
        value.strip_prefix(r"\\?\").unwrap_or(&value).to_owned()
    }
}

pub(super) fn ancestor(path: &Path, specification: &str) -> io::Result<String> {
    let (index, range) = specification
        .split_once('_')
        .map_or((specification, None), |(index, range)| (index, Some(range)));
    let forward = index.starts_with('+');
    let index = index
        .trim_start_matches('+')
        .parse::<usize>()
        .ok()
        .filter(|index| *index <= 9)
        .ok_or_else(|| {
            io::Error::other(
                "Verwende [B0] bis [B9] oder [B+0] bis [B+9], optional mit _Zeichenbereich.",
            )
        })?;
    let value = if forward {
        path.parent()
            .unwrap_or(path)
            .components()
            .filter_map(|component| match component {
                #[cfg(windows)]
                Component::Prefix(prefix) => Some(
                    prefix
                        .as_os_str()
                        .to_string_lossy()
                        .trim_end_matches(':')
                        .to_owned(),
                ),
                Component::Normal(value) => Some(value.to_string_lossy().into_owned()),
                _ => None,
            })
            .nth(index)
            .unwrap_or_default()
    } else {
        path.ancestors()
            .nth(index + 1)
            .and_then(Path::file_name)
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned()
    };
    range.map_or(Ok(value.clone()), |range| name_range(&value, range))
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CounterSettings {
    pub start: String,
    pub step: String,
    pub width: String,
    pub every: String,
}

impl Default for CounterSettings {
    fn default() -> Self {
        Self {
            start: "1".into(),
            step: "1".into(),
            width: "1".into(),
            every: "1".into(),
        }
    }
}

impl CounterSettings {
    pub fn pattern(&self) -> io::Result<String> {
        let step =
            self.step.trim().parse::<i128>().map_err(|_| {
                io::Error::other("Die Zählerschrittweite muss eine ganze Zahl sein.")
            })?;
        let every = self
            .every
            .trim()
            .parse::<usize>()
            .ok()
            .filter(|n| *n > 0)
            .ok_or_else(|| {
                io::Error::other("Die Zählergruppe muss mindestens eine Datei enthalten.")
            })?;
        let start = self.start.trim();
        if start.is_empty()
            || !start
                .bytes()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == b'-')
        {
            return Err(io::Error::other(
                "Gib eine Zahl oder Buchstaben als Zählerstart an.",
            ));
        }
        let pattern = format!("{start}{step:+}/{every}:{}", self.width.trim());
        counter(&pattern, 0, 1)?;
        Ok(pattern)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub enum NameCase {
    #[default]
    Original,
    Upper,
    Lower,
    First,
    Title,
    EnglishTitle,
}

impl NameCase {
    pub fn index(self) -> i32 {
        match self {
            Self::Original => 0,
            Self::Upper => 1,
            Self::Lower => 2,
            Self::First => 3,
            Self::Title => 4,
            Self::EnglishTitle => 5,
        }
    }
    pub fn from_index(value: i32) -> io::Result<Self> {
        [
            Self::Original,
            Self::Upper,
            Self::Lower,
            Self::First,
            Self::Title,
            Self::EnglishTitle,
        ]
        .get(usize::try_from(value).unwrap_or(usize::MAX))
        .copied()
        .ok_or_else(|| io::Error::other("Wähle eine gültige Schreibweise."))
    }
    pub(super) fn apply(self, value: String) -> String {
        match self {
            Self::Original => value,
            Self::Upper => value.to_uppercase(),
            Self::Lower => value.to_lowercase(),
            Self::First => {
                let mut chars = value.chars();
                chars
                    .next()
                    .map(|first| {
                        first
                            .to_uppercase()
                            .chain(chars.flat_map(char::to_lowercase))
                            .collect()
                    })
                    .unwrap_or_default()
            }
            Self::Title | Self::EnglishTitle => {
                let mut output = String::new();
                let mut start = true;
                for ch in value.chars() {
                    if start {
                        output.extend(ch.to_uppercase());
                    } else {
                        output.extend(ch.to_lowercase());
                    }
                    start = !ch.is_alphanumeric();
                }
                if self == Self::EnglishTitle {
                    english_title(&mut output);
                }
                output
            }
        }
    }
}

pub(super) fn environment(specification: &str) -> io::Result<String> {
    let (name, suffix) = specification
        .strip_prefix('%')
        .and_then(|value| value.split_once('%'))
        .filter(|(name, _)| !name.is_empty() && !name.contains(['\r', '\n', '\0']))
        .ok_or_else(|| {
            io::Error::other(
                "Umgebungsvariablen benötigen [%NAME%], optional mit Bereich oder Pfadtrenner.",
            )
        })?;
    let value = std::env::var_os(name)
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_else(|| format!("%{name}%"));
    environment_value(&value, suffix)
}

fn environment_value(value: &str, suffix: &str) -> io::Result<String> {
    if suffix.is_empty() {
        return Ok(value.to_owned());
    }
    if suffix == "\\" {
        return Ok(value
            .trim_end_matches(['\\', '/'])
            .rsplit(['\\', '/'])
            .next()
            .unwrap_or_default()
            .to_owned());
    }
    if suffix.chars().count() == 1
        && !suffix.starts_with(|ch: char| ch.is_ascii_digit() || matches!(ch, '-' | ' '))
    {
        return Ok(value.replace(['\\', '/'], suffix).replace(':', ""));
    }
    name_range(value, suffix)
}

pub(super) fn english_title(value: &mut str) {
    let words = value
        .char_indices()
        .chain(std::iter::once((value.len(), ' ')))
        .fold((Vec::new(), None), |(mut words, mut start), (index, ch)| {
            if ch.is_alphanumeric() {
                start.get_or_insert(index);
            } else if let Some(begin) = start.take() {
                words.push(begin..index);
            }
            (words, start)
        })
        .0;
    for range in words.iter().skip(1).take(words.len().saturating_sub(2)) {
        let word = &value[range.clone()];
        if [
            "a", "an", "the", "at", "by", "for", "in", "of", "on", "to", "from", "up", "and", "as",
            "but", "or", "nor", "yet", "so", "vs",
        ]
        .iter()
        .any(|candidate| word.eq_ignore_ascii_case(candidate))
        {
            value[range.clone()].make_ascii_lowercase();
        }
    }
}

pub(super) fn normalize(value: String) -> io::Result<String> {
    use unicode_normalization::UnicodeNormalization;
    Ok(value.nfc().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn environment_modifiers_preserve_unicode_and_apply_path_rules() {
        let value = r"C:\Überblick\Dokumente";
        assert_eq!(environment_value(value, "\\").unwrap(), "Dokumente");
        assert_eq!(
            environment_value(value, "_").unwrap(),
            "C_Überblick_Dokumente"
        );
        assert_eq!(environment_value("Äpfel", "2,3").unwrap(), "pfe");
        assert_eq!(environment_value("abc", "02-9").unwrap(), "000000bc");
    }
    #[cfg(windows)]
    #[test]
    fn ancestor_masks_count_from_both_ends_and_support_ranges() {
        let path = Path::new(r"C:\work\Bilder\Sommer\probe.txt");
        for (mask, expected) in [
            ("0", "Sommer"),
            ("1", "Bilder"),
            ("2_2-4", "ork"),
            ("+0", "C"),
            ("+1", "work"),
            ("+2_1,3", "Bil"),
        ] {
            assert_eq!(ancestor(path, mask).unwrap(), expected, "{mask}");
        }
        assert!(ancestor(path, "+10").is_err());
        assert_eq!(normalize("cafe\u{301}".into()).unwrap(), "café");
    }
    #[test]
    fn english_title_preserves_first_last_and_non_english_words() {
        let mut value = "The Lord Of The Rings And".to_owned();
        english_title(&mut value);
        assert_eq!(value, "The Lord of the Rings And");
    }
}
