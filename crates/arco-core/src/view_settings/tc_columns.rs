use crate::files::{Entry, Kind, SortKey};
use crate::plugin::{WdxKey, WdxValue};
use crate::rename::FieldReference;
use chrono::{DateTime, Datelike, Local, Timelike};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt::Write;
use std::io;
use std::path::Path;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct TcColumnExpression {
    source: String,
    parts: Vec<Part>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Part {
    Text(String),
    Reference(String),
    Value {
        field: NativeField,
        range: Option<String>,
        prefix: String,
        suffix: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum NativeField {
    Name,
    FullName,
    Extension,
    Path,
    Size(u64),
    Modified(String),
    Attributes,
    Comment,
}

impl TcColumnExpression {
    pub fn parse(source: &str) -> io::Result<Self> {
        if source.is_empty()
            || source.len() > 4096
            || source.contains('|')
            || source.chars().any(char::is_control)
        {
            return Err(invalid());
        }
        let mut parts = Vec::new();
        let mut rest = source;
        while let Some((text, token)) = rest.split_once("[=") {
            if parts.len() >= 64 {
                return Err(invalid());
            }
            if !text.is_empty() {
                parts.push(Part::Text(text.into()));
            }
            let mut quoted = false;
            let end = token
                .char_indices()
                .find_map(|(index, ch)| {
                    if ch == '"' {
                        quoted = !quoted;
                    }
                    (ch == ']' && !quoted).then_some(index)
                })
                .ok_or_else(invalid)?;
            parts.push(parse_token(&token[..end])?);
            rest = &token[end + 1..];
        }
        if !rest.is_empty() {
            parts.push(Part::Text(rest.into()));
        }
        Ok(Self {
            source: source.into(),
            parts,
        })
    }

    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn sort_key(&self) -> Option<SortKey> {
        if self
            .parts
            .iter()
            .any(|part| matches!(part, Part::Reference(_)))
        {
            return None;
        }
        let mut fields = self.parts.iter().filter_map(|part| match part {
            Part::Value { field, .. } => Some(field),
            Part::Text(_) | Part::Reference(_) => None,
        });
        let field = fields.next()?;
        if fields.next().is_some() {
            return None;
        }
        Some(match field {
            NativeField::Name | NativeField::FullName => SortKey::Name,
            NativeField::Extension => SortKey::Extension,
            NativeField::Size(_) => SortKey::Size,
            NativeField::Modified(_) => SortKey::Modified,
            NativeField::Attributes => SortKey::Attributes,
            NativeField::Comment => SortKey::Comment,
            NativeField::Path => return None,
        })
    }

    pub(super) fn uses_comment(&self) -> bool {
        self.parts.iter().any(|part| {
            matches!(
                part,
                Part::Value {
                    field: NativeField::Comment,
                    ..
                }
            )
        })
    }

    pub fn render(&self, entry: &Entry, local: bool) -> String {
        self.render_parts(entry, local, &BTreeMap::new())
            .unwrap_or_default()
    }

    pub fn references(&self) -> Vec<&str> {
        let mut fields = Vec::new();
        for part in &self.parts {
            if let Part::Reference(token) = part {
                let reference = FieldReference::parse(token).expect("validated column reference");
                if !fields.contains(&reference.query) {
                    fields.push(reference.query);
                }
            }
        }
        fields
    }

    pub fn background(&self) -> bool {
        self.sort_key().is_none()
    }

    pub fn render_value(
        &self,
        entry: &Entry,
        local: bool,
        values: &BTreeMap<String, WdxValue>,
    ) -> io::Result<Option<WdxValue>> {
        let display = self.render_parts(entry, local, values)?;
        if display.is_empty() {
            return Ok(None);
        }
        let fields: Vec<_> = self
            .parts
            .iter()
            .filter(|part| !matches!(part, Part::Text(_)))
            .collect();
        let key = if let [Part::Reference(token)] = fields.as_slice() {
            let reference = FieldReference::parse(token)?;
            values.get(reference.query).map(|value| value.key.clone())
        } else {
            None
        }
        .unwrap_or_else(|| WdxKey::Text(display.to_lowercase()));
        Ok(Some(WdxValue { display, key }))
    }

    fn render_parts(
        &self,
        entry: &Entry,
        local: bool,
        values: &BTreeMap<String, WdxValue>,
    ) -> io::Result<String> {
        let mut output = String::new();
        for part in &self.parts {
            match part {
                Part::Text(text) => output.push_str(text),
                Part::Reference(token) => {
                    let field = FieldReference::parse(token)?;
                    if let Some(value) = field.render(values.get(field.query))? {
                        output.push_str(&value);
                    }
                }
                Part::Value {
                    field,
                    range,
                    prefix,
                    suffix,
                } => {
                    let Some(value) = field.value(entry, local) else {
                        continue;
                    };
                    let value = if let Some(range) = range {
                        crate::rename::name_range(&value, range).unwrap_or_default()
                    } else {
                        value
                    };
                    if !value.is_empty() {
                        output.push_str(prefix);
                        output.push_str(&value);
                        output.push_str(suffix);
                    }
                }
            }
        }
        if output.contains(['\r', '\n', '\t']) {
            output = output
                .replace("\r\n", " · ")
                .replace(['\r', '\n'], " · ")
                .replace('\t', " ");
        }
        Ok(output)
    }
}

impl TryFrom<String> for TcColumnExpression {
    type Error = io::Error;

    fn try_from(source: String) -> io::Result<Self> {
        Self::parse(&source)
    }
}

impl From<TcColumnExpression> for String {
    fn from(expression: TcColumnExpression) -> Self {
        expression.source
    }
}

fn parse_token(token: &str) -> io::Result<Part> {
    let original = token;
    let (prefix, token) = if let Some(rest) = token.strip_prefix('"') {
        rest.split_once('"').ok_or_else(invalid)?
    } else {
        ("", token)
    };
    let (field, suffix) = if let Some((field, suffix)) = token.split_once('"') {
        let suffix = suffix
            .strip_suffix('"')
            .filter(|suffix| !suffix.contains('"'))
            .ok_or_else(invalid)?;
        (field, suffix)
    } else {
        (token, "")
    };
    if field.contains(['[', ']']) {
        return Err(invalid());
    }
    let (field, range) = field
        .rsplit_once(':')
        .filter(|(_, range)| crate::rename::name_range("", range).is_ok())
        .map_or((field, None), |(field, range)| (field, Some(range.into())));
    let native = match NativeField::parse(field) {
        Ok(native) => native,
        Err(_) => {
            let reference = FieldReference::parse(original)?;
            let (plugin, field) = reference.query.split_once('.').ok_or_else(invalid)?;
            if plugin.eq_ignore_ascii_case("tc") {
                let (name, unit) = field
                    .split_once('.')
                    .map_or((field, None), |(n, u)| (n, Some(u)));
                if !crate::plugin::TC_METADATA_FIELDS
                    .iter()
                    .any(|candidate| candidate.eq_ignore_ascii_case(name))
                    || unit.is_some_and(|unit| {
                        !name.eq_ignore_ascii_case("size")
                            || ![
                                "bytes", "kbytes", "mbytes", "gbytes", "tbytes", "b", "kb", "mb",
                                "gb", "tb",
                            ]
                            .iter()
                            .any(|u| u.eq_ignore_ascii_case(unit))
                    })
                {
                    return Err(invalid());
                }
            }
            return Ok(Part::Reference(original.into()));
        }
    };
    Ok(Part::Value {
        field: native,
        range,
        prefix: prefix.into(),
        suffix: suffix.into(),
    })
}

impl NativeField {
    fn parse(field: &str) -> io::Result<Self> {
        let (plugin, field) = field.split_once('.').ok_or_else(invalid)?;
        if !plugin.eq_ignore_ascii_case("tc") {
            return Err(invalid());
        }
        let (field, format) = field
            .split_once('.')
            .map_or((field, None), |(field, format)| (field, Some(format)));
        match field.to_lowercase().as_str() {
            "name" if format.is_none() => Ok(Self::Name),
            "fullname" if format.is_none() => Ok(Self::FullName),
            "ext" | "extension" if format.is_none() => Ok(Self::Extension),
            "path" if format.is_none() => Ok(Self::Path),
            "attributes" | "attribute" if format.is_none() => Ok(Self::Attributes),
            "comment" | "kommentar" if format.is_none() => Ok(Self::Comment),
            "size" | "größe" => {
                let divisor = match format.unwrap_or("bytes").to_ascii_lowercase().as_str() {
                    "bytes" | "b" => 1,
                    "kbytes" | "kb" => 1024,
                    "mbytes" | "mb" => 1024_u64.pow(2),
                    "gbytes" | "gb" => 1024_u64.pow(3),
                    "tbytes" | "tb" => 1024_u64.pow(4),
                    _ => return Err(invalid()),
                };
                Ok(Self::Size(divisor))
            }
            "writedate" | "writedatetime" | "änderungsdatum" | "writetime" => {
                if let Some(format) = format {
                    if format.is_empty()
                        || format.len() > 96
                        || !format.chars().any(|ch| "YyMDhmsuvapP".contains(ch))
                    {
                        return Err(invalid());
                    }
                }
                let default = if field.eq_ignore_ascii_case("writetime") {
                    "h:m:s"
                } else {
                    "D.M.Y h:m"
                };
                Ok(Self::Modified(format.unwrap_or(default).into()))
            }
            _ => Err(invalid()),
        }
    }

    fn value(&self, entry: &Entry, local: bool) -> Option<String> {
        let name = Path::new(&entry.name);
        Some(match self {
            Self::Name if entry.kind == Kind::Directory => {
                entry.name.to_string_lossy().into_owned()
            }
            Self::Name => name.file_stem()?.to_string_lossy().into_owned(),
            Self::FullName => entry.name.to_string_lossy().into_owned(),
            Self::Extension if entry.kind != Kind::Directory => {
                name.extension()?.to_string_lossy().into_owned()
            }
            Self::Extension => return None,
            Self::Path if local && entry.path.is_absolute() => {
                let path = entry.path.parent()?.to_string_lossy();
                let mut path = if let Some(path) = path.strip_prefix(r"\\?\UNC\") {
                    format!(r"\\{path}")
                } else {
                    path.strip_prefix(r"\\?\").unwrap_or(&path).to_owned()
                };
                if !path.ends_with(['/', '\\']) {
                    path.push(if cfg!(windows) { '\\' } else { '/' });
                }
                path
            }
            Self::Path => return None,
            Self::Size(divisor) => size(entry.size?, *divisor),
            Self::Modified(format) => date(DateTime::<Local>::from(entry.modified?), format),
            Self::Attributes => [(1, 'r'), (32, 'a'), (4, 's'), (2, 'h')]
                .into_iter()
                .map(|(flag, label)| {
                    if entry.attributes & flag != 0 {
                        label
                    } else {
                        '-'
                    }
                })
                .collect(),
            Self::Comment => entry.comment.as_deref()?.to_owned(),
        })
    }
}

fn size(bytes: u64, divisor: u64) -> String {
    let whole = bytes / divisor;
    let decimals = (bytes % divisor) * 100 / divisor;
    match decimals {
        0 => whole.to_string(),
        number if number % 10 == 0 => format!("{whole}.{}", number / 10),
        number => format!("{whole}.{number:02}"),
    }
}

pub(crate) fn date(time: DateTime<Local>, format: &str) -> String {
    let mut output = String::new();
    for ch in format.chars() {
        match ch {
            'Y' => {
                let _ = write!(output, "{:04}", time.year());
            }
            'y' => {
                let _ = write!(output, "{:02}", time.year().rem_euclid(100));
            }
            'M' => {
                let _ = write!(output, "{:02}", time.month());
            }
            'D' => {
                let _ = write!(output, "{:02}", time.day());
            }
            'h' => {
                let _ = write!(output, "{:02}", time.hour());
            }
            'm' => {
                let _ = write!(output, "{:02}", time.minute());
            }
            's' => {
                let _ = write!(output, "{:02}", time.second());
            }
            'u' | 'v' => {
                let _ = write!(output, "{:03}", time.timestamp_subsec_millis());
            }
            'a' => {
                let _ = write!(output, "{:02}", time.hour12().1);
            }
            'P' => output.push_str(if time.hour12().0 { "pm" } else { "am" }),
            'p' => output.push(if time.hour12().0 { 'p' } else { 'a' }),
            literal => output.push(literal),
        }
    }
    output
}

fn invalid() -> io::Error {
    io::Error::other("Ungültiger oder noch nicht unterstützter TC-Spaltenausdruck.")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn combines_native_tokens_slices_and_conditional_text() {
        let expression = TcColumnExpression::parse(
            r#"[=tc.name:1-2] [="("tc.size.kbytes" KiB)"][=" · "tc.comment]"#,
        )
        .unwrap();
        let mut entry =
            Entry::from_remote(PathBuf::from("A😀-Foto.jpg"), Kind::File, Some(1536), None);
        assert_eq!(expression.render(&entry, false), "A😀 (1.5 KiB)");
        entry.comment = Some("Bild\nSommer".into());
        assert_eq!(
            expression.render(&entry, false),
            "A😀 (1.5 KiB) · Bild · Sommer"
        );
        assert_eq!(expression.sort_key(), None);
        assert!(expression.uses_comment());
    }

    #[test]
    fn preserves_date_formats_and_the_typed_sort_of_one_field() {
        let expression = TcColumnExpression::parse("[=tc.writedate.Y-M-D h:m:s.v]").unwrap();
        let entry = Entry::from_remote(
            PathBuf::from("text.txt"),
            Kind::File,
            None,
            Some(std::time::UNIX_EPOCH + std::time::Duration::from_millis(375)),
        );
        let rendered = expression.render(&entry, true);
        assert!(rendered.ends_with(".375"));
        assert_eq!(expression.sort_key(), Some(SortKey::Modified));
        let json = serde_json::to_string(&expression).unwrap();
        assert_eq!(
            serde_json::from_str::<TcColumnExpression>(&json).unwrap(),
            expression
        );
        assert!(TcColumnExpression::parse("[=wdx.Title]").is_ok());
        assert!(TcColumnExpression::parse("[=tc.size.unknown]").is_err());
    }

    #[test]
    fn resolves_composite_fields_formats_units_and_preserves_typed_single_field_sorting() {
        let entry = Entry::from_remote(PathBuf::from("sample.txt"), Kind::File, Some(12), None);
        let expression = TcColumnExpression::parse(
            r#"[=tc.name]: [="("P.Score.scaled._d3,")"] [=<fs>.Author:1-2]"#,
        )
        .unwrap();
        assert_eq!(expression.references(), ["P.Score.scaled", "<fs>.Author"]);
        assert!(expression.background());
        let mut values = BTreeMap::new();
        values.insert(
            "P.Score.scaled".into(),
            WdxValue {
                display: "1000".into(),
                key: WdxKey::Integer(1000),
            },
        );
        values.insert(
            "<fs>.Author".into(),
            WdxValue {
                display: "A😀BC".into(),
                key: WdxKey::Text("a😀bc".into()),
            },
        );
        assert_eq!(
            expression
                .render_value(&entry, false, &values)
                .unwrap()
                .unwrap()
                .display,
            "sample: (1,000) A😀"
        );
        let single = TcColumnExpression::parse(r#"[="("P.Score.scaled._d3,")"]"#).unwrap();
        let value = single.render_value(&entry, true, &values).unwrap().unwrap();
        assert_eq!(value.key, WdxKey::Integer(1000));
        assert_eq!(value.display, "(1,000)");
        assert!(
            single
                .render_value(&entry, true, &BTreeMap::new())
                .unwrap()
                .is_none()
        );
        assert!(TcColumnExpression::parse("[=tc.creationdate.Y-M-D]").is_ok());
        assert!(TcColumnExpression::parse("[=tc.does-not-exist]").is_err());
        assert!(TcColumnExpression::parse("[=tc.partner file with other extension]").is_err());
        assert!(TcColumnExpression::parse("[=tc.size.not-a-unit._d3]").is_err());
    }
}
