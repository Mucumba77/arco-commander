use super::Language;
use serde::de::{MapAccess, Visitor};
use serde::{Deserialize, Deserializer};
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

#[derive(Debug)]
enum Part {
    Text(String),
    Value(String),
}

struct Template {
    source: Vec<Part>,
    target: Vec<Part>,
    literal: usize,
}

struct Catalog {
    exact: BTreeMap<String, String>,
    templates: Vec<Template>,
}

struct Unique(BTreeMap<String, String>);
impl<'de> Deserialize<'de> for Unique {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Entries;
        impl<'de> Visitor<'de> for Entries {
            type Value = Unique;
            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("a unique message translation map")
            }
            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Unique, M::Error> {
                let mut entries = BTreeMap::new();
                while let Some((source, target)) = map.next_entry::<String, String>()? {
                    if entries.insert(source, target).is_some() {
                        return Err(serde::de::Error::custom("duplicate message translation"));
                    }
                }
                Ok(Unique(entries))
            }
        }
        deserializer.deserialize_map(Entries)
    }
}

impl Catalog {
    fn parse(json: &str) -> Result<Self, String> {
        let Unique(exact) = serde_json::from_str(json).map_err(|error| error.to_string())?;
        let mut templates = Vec::new();
        for (source, target) in &exact {
            if source.is_empty() || target.is_empty() {
                return Err("empty message translation".into());
            }
            let source = parts(source)?;
            let target = parts(target)?;
            let values = |parts: &[Part]| {
                parts
                    .iter()
                    .filter_map(|part| match part {
                        Part::Value(key) => Some(key.clone()),
                        _ => None,
                    })
                    .collect::<BTreeSet<_>>()
            };
            let keys = values(&source);
            if keys != values(&target) {
                return Err("message placeholders differ between source and translation".into());
            }
            let literal = source
                .iter()
                .map(|part| match part {
                    Part::Text(value) => value.len(),
                    _ => 0,
                })
                .sum();
            let minimum_literal = if matches!(source.first(), Some(Part::Text(_))) {
                4
            } else {
                8
            };
            if !keys.is_empty() && keys.len() <= 16 && literal >= minimum_literal {
                templates.push(Template {
                    source,
                    target,
                    literal,
                });
            }
        }
        templates.sort_by_key(|template| std::cmp::Reverse(template.literal));
        Ok(Self { exact, templates })
    }
}

fn parts(text: &str) -> Result<Vec<Part>, String> {
    let mut result = Vec::new();
    let mut literal = String::new();
    let mut rest = text;
    let mut index = 0;
    while !rest.is_empty() {
        if let Some(tail) = rest.strip_prefix("{{") {
            literal.push('{');
            rest = tail;
        } else if let Some(tail) = rest.strip_prefix("}}") {
            literal.push('}');
            rest = tail;
        } else if let Some(tail) = rest.strip_prefix('{') {
            let (field, tail) = tail.split_once('}').ok_or("unclosed message placeholder")?;
            if field.contains('{') {
                return Err("nested message placeholder".into());
            }
            let (name, format) = field
                .split_once(':')
                .map_or((field, ""), |(name, format)| (name, format));
            let name = if name.is_empty() {
                let name = index.to_string();
                index += 1;
                name
            } else {
                name.to_owned()
            };
            if !literal.is_empty() {
                result.push(Part::Text(std::mem::take(&mut literal)));
            }
            result.push(Part::Value(format!("{name}:{format}")));
            rest = tail;
        } else {
            let character = rest.chars().next().unwrap();
            if character == '}' {
                return Err("unescaped closing message brace".into());
            }
            literal.push(character);
            rest = &rest[character.len_utf8()..];
        }
    }
    if !literal.is_empty() {
        result.push(Part::Text(literal));
    }
    Ok(result)
}

fn catalog(language: Language) -> Option<&'static Catalog> {
    static EN: OnceLock<Catalog> = OnceLock::new();
    static FR: OnceLock<Catalog> = OnceLock::new();
    let (slot, source) = match language {
        Language::German => return None,
        Language::English => (&EN, include_str!("../../i18n/en-messages.json")),
        Language::French => (&FR, include_str!("../../i18n/fr-messages.json")),
    };
    Some(slot.get_or_init(|| Catalog::parse(source).expect("validated message catalog")))
}

pub(super) fn exact(language: Language, text: &str) -> Option<&'static str> {
    catalog(language)?.exact.get(text).map(String::as_str)
}

fn captures<'a>(
    parts: &[Part],
    text: &'a str,
    values: &mut BTreeMap<String, &'a str>,
    budget: &mut usize,
) -> bool {
    if *budget == 0 {
        return false;
    }
    *budget -= 1;
    let Some((part, tail)) = parts.split_first() else {
        return text.is_empty();
    };
    match part {
        Part::Text(literal) => text
            .strip_prefix(literal)
            .is_some_and(|rest| captures(tail, rest, values, budget)),
        Part::Value(key) => {
            let boundaries: Vec<_> = match tail.first() {
                Some(Part::Text(literal)) => text
                    .match_indices(literal)
                    .map(|(offset, _)| offset)
                    .take(128)
                    .collect(),
                None => vec![text.len()],
                Some(Part::Value(_)) => return false,
            };
            for end in boundaries {
                let value = &text[..end];
                if values.get(key).is_some_and(|previous| *previous != value) {
                    continue;
                }
                let previous = values.insert(key.clone(), value);
                if captures(tail, &text[end..], values, budget) {
                    return true;
                }
                if let Some(previous) = previous {
                    values.insert(key.clone(), previous);
                } else {
                    values.remove(key);
                }
            }
            false
        }
    }
}

pub(super) fn message(language: Language, text: &str) -> Cow<'_, str> {
    render(language, text, 0)
}

fn render(language: Language, text: &str, depth: usize) -> Cow<'_, str> {
    // Bare filesystem paths and virtual addresses are data, even when a file
    // name happens to contain a complete UI message.
    let drive_path = text.as_bytes().get(1) == Some(&b':')
        && text.as_bytes().first().is_some_and(u8::is_ascii_alphabetic);
    if text.starts_with(['/', '\\'])
        || drive_path
        || text.starts_with("shell:")
        || text.starts_with("ftp://")
        || text.starts_with("ftps://")
    {
        return Cow::Borrowed(text);
    }
    let translated = super::translate(language, text);
    if translated != text {
        return Cow::Borrowed(translated);
    }
    let Some(catalog) = catalog(language) else {
        return Cow::Borrowed(text);
    };
    if depth > 3 || text.len() > 65536 {
        return Cow::Borrowed(text);
    }
    for template in &catalog.templates {
        if let Some(Part::Text(prefix)) = template.source.first()
            && !text.starts_with(prefix)
        {
            continue;
        }
        let mut values = BTreeMap::new();
        if !captures(&template.source, text, &mut values, &mut 512) {
            continue;
        }
        let mut output = String::new();
        for part in &template.target {
            match part {
                Part::Text(literal) => output.push_str(literal),
                Part::Value(key) => {
                    if let Some(value) = values.get(key) {
                        // Paths, names, user text and formatted numbers stay opaque.
                        // Only explicitly named backend error messages are translated.
                        if matches!(key.split(':').next(), Some("error" | "reason" | "message")) {
                            output.push_str(&render(language, value, depth + 1));
                        } else {
                            output.push_str(value);
                        }
                    }
                }
            }
        }
        return Cow::Owned(output);
    }
    if depth == 0 && text.contains('\n') {
        let mut changed = false;
        let lines = text
            .split_inclusive('\n')
            .enumerate()
            .map(|(index, line)| {
                if index >= 256 {
                    return line.to_owned();
                }
                let (body, suffix) = line
                    .strip_suffix('\n')
                    .map_or((line, ""), |body| (body, "\n"));
                let result = render(language, body, 1);
                changed |= result != body;
                format!("{result}{suffix}")
            })
            .collect::<String>();
        if changed {
            return Cow::Owned(lines);
        }
    }
    Cow::Borrowed(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn catalogs_preserve_all_format_fields_and_reject_duplicate_or_incomplete_translations() {
        assert!(catalog(Language::English).is_some());
        assert!(catalog(Language::French).is_some());
        assert!(Catalog::parse(r#"{"one":"a","one":"b"}"#).is_err());
        assert!(Catalog::parse(r#"{"Missing {path}":"Absent {name}"}"#).is_err());
    }
    #[test]
    fn translated_messages_keep_paths_and_user_data_and_translate_nested_errors() {
        assert_eq!(
            message(
                Language::English,
                "Profil konnte nicht gelesen werden: Ungültiger Archivpfad."
            ),
            "Could not read profile: Invalid archive path."
        );
        let source =
            "Die Datei „C:\\Daten\\Abbrechen.txt“ konnte nicht geöffnet werden: access denied";
        assert_eq!(
            message(Language::French, source),
            "Impossible d’ouvrir le fichier « C:\\Daten\\Abbrechen.txt » : access denied"
        );
        assert_eq!(
            message(Language::English, r"C:\Daten\Ungültiger Archivpfad."),
            r"C:\Daten\Ungültiger Archivpfad."
        );
        assert_eq!(message(Language::German, source), source);
    }
}
