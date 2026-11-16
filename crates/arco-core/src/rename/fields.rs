use crate::plugin::{WdxKey, WdxValue};
use std::io;

pub struct FieldReference<'a> {
    pub query: &'a str,
    prefix: &'a str,
    suffix: &'a str,
    range: Option<&'a str>,
    format: Option<&'a str>,
    conditional: bool,
}

impl<'a> FieldReference<'a> {
    pub fn parse(token: &'a str) -> io::Result<Self> {
        let conditional = token.contains('"');
        let (prefix, token) = if let Some(rest) = token.strip_prefix('"') {
            rest.split_once('"').ok_or_else(invalid)?
        } else {
            ("", token)
        };
        let (token, suffix) = if let Some((token, suffix)) = token.split_once('"') {
            (
                token,
                suffix
                    .strip_suffix('"')
                    .filter(|s| !s.contains('"'))
                    .ok_or_else(invalid)?,
            )
        } else {
            (token, "")
        };
        let (token, range) = if let Some((token, range)) = token
            .rsplit_once(':')
            .filter(|(_, range)| super::name_range("", range).is_ok())
        {
            (token, Some(range))
        } else {
            (token, None)
        };
        let (_, field) = token
            .split_once('.')
            .filter(|(plugin, field)| !plugin.is_empty() && !field.is_empty())
            .ok_or_else(invalid)?;
        let (query, format) = if let Some((_, format)) = field.split_once('.').filter(|(_, f)| {
            f.starts_with('_')
                || ((f.contains(['Y', 'M', 'D'])
                    || (f.contains('h') && f.contains('m'))
                    || (f.contains('m') && f.contains('s')))
                    && f.chars().all(|ch| "YyMDhmsuvapP .-_,/:".contains(ch)))
        }) {
            (&token[..token.len() - format.len() - 1], Some(format))
        } else {
            (token, None)
        };
        if let Some(format) = format.and_then(|f| f.strip_prefix('_')) {
            numeric(0, format)?;
        }
        Ok(Self {
            query,
            prefix,
            suffix,
            range,
            format,
            conditional,
        })
    }

    pub fn render(&self, value: Option<&WdxValue>) -> io::Result<Option<String>> {
        let Some(value) = value.filter(|value| !value.display.is_empty()) else {
            return Ok(self.conditional.then(String::new));
        };
        let mut text = match self.format {
            Some(format) if format.starts_with('_') => match value.key {
                WdxKey::Integer(number) => numeric(number, &format[1..])?,
                WdxKey::Float(number) => floating(number, &format[1..])?,
                _ => {
                    return Err(io::Error::other(
                        "Numerische Formatierung benötigt ein numerisches WDX-Feld.",
                    ));
                }
            },
            Some(format) => match value.key {
                WdxKey::DateTime(ticks) => {
                    let unix_ticks = i128::from(ticks) - 116444736000000000_i128;
                    let seconds =
                        i64::try_from(unix_ticks.div_euclid(10_000_000)).map_err(|_| invalid())?;
                    let nanos = (unix_ticks.rem_euclid(10_000_000) * 100) as u32;
                    let date = chrono::DateTime::from_timestamp(seconds, nanos)
                        .ok_or_else(invalid)?
                        .with_timezone(&chrono::Local);
                    crate::view_settings::tc_date(date, format)
                }
                WdxKey::Text(_) => {
                    let date = chrono::NaiveDate::parse_from_str(&value.display, "%Y-%m-%d")
                        .ok()
                        .and_then(|date| date.and_hms_opt(0, 0, 0))
                        .or_else(|| {
                            chrono::NaiveTime::parse_from_str(&value.display, "%H:%M:%S")
                                .ok()
                                .map(|time| {
                                    chrono::NaiveDate::from_ymd_opt(1970, 1, 1)
                                        .unwrap()
                                        .and_time(time)
                                })
                        });
                    use chrono::TimeZone;
                    let date = date.and_then(|date| chrono::Local.from_local_datetime(&date).earliest()).ok_or_else(|| io::Error::other("Das WDX-Feld enthält kein gültiges Datum oder keine gültige Uhrzeit."))?;
                    crate::view_settings::tc_date(date, format)
                }
                _ => {
                    return Err(io::Error::other(
                        "Datumsformatierung benötigt ein Datum-/Zeitfeld.",
                    ));
                }
            },
            None => value.display.clone(),
        };
        if let Some(range) = self.range {
            text = super::name_range(&text, range)?;
        }
        Ok(Some(if text.is_empty() {
            text
        } else {
            format!("{}{text}{}", self.prefix, self.suffix)
        }))
    }
}

fn numeric(number: i64, format: &str) -> io::Result<String> {
    let mut chars = format.chars();
    let radix = chars
        .next()
        .filter(|ch| "dxXob".contains(*ch))
        .ok_or_else(invalid)?;
    let mut next = chars.next();
    let zeros = next == Some('0');
    if zeros {
        next = chars.next();
    }
    let group = match next {
        Some(ch) => ch.to_digit(10).filter(|n| *n > 0).ok_or_else(invalid)? as usize,
        None if !zeros => 0,
        _ => return Err(invalid()),
    };
    let separator = chars.next().map(String::from).unwrap_or_else(|| {
        if radix == 'd' {
            crate::plugin::grouping_separator()
        } else {
            " ".into()
        }
    });
    if chars.next().is_some() {
        return Err(invalid());
    }
    let value = match radix {
        'd' => number.unsigned_abs().to_string(),
        'x' => format!("{:x}", number as u64),
        'X' => format!("{:X}", number as u64),
        'o' => format!("{:o}", number as u64),
        'b' => format!("{:b}", number as u64),
        _ => unreachable!(),
    };
    let value = if zeros && radix != 'd' {
        let digits: usize = match radix {
            'x' | 'X' => 16,
            'o' => 22,
            'b' => 64,
            _ => 0,
        };
        format!("{value:0>digits$}")
    } else if group > 0 && radix != 'd' {
        let digits = value.len().div_ceil(group) * group;
        format!("{value:0>digits$}")
    } else {
        value
    };
    let mut output = String::new();
    if radix == 'd' && number < 0 {
        output.push('-');
    }
    for (index, ch) in value.chars().enumerate() {
        if group > 0 && index > 0 && (value.len() - index).is_multiple_of(group) {
            output.push_str(&separator);
        }
        output.push(ch);
    }
    Ok(output)
}

fn invalid() -> io::Error {
    io::Error::other("Ungültiger WDX-Feldausdruck oder Zahlenformat.")
}

fn floating(number: f64, format: &str) -> io::Result<String> {
    numeric(0, format)?;
    if !number.is_finite() {
        return Err(invalid());
    }
    let Some(rest) = format.strip_prefix('d') else {
        if number.fract() != 0.0
            || !(-9223372036854775808.0..9223372036854775808.0).contains(&number)
        {
            return Err(io::Error::other(
                "Hexadezimale, oktale und binäre Formate benötigen einen ganzzahligen Wert im 64-Bit-Bereich.",
            ));
        }
        return numeric(number as i64, format);
    };
    let mut chars = rest.strip_prefix('0').unwrap_or(rest).chars();
    let group = chars.next().and_then(|ch| ch.to_digit(10)).unwrap_or(0) as usize;
    let separator = chars
        .next()
        .map(String::from)
        .unwrap_or_else(crate::plugin::grouping_separator);
    let text = number.abs().to_string();
    let (integer, fraction) = text
        .split_once('.')
        .map_or((text.as_str(), None), |(a, b)| (a, Some(b)));
    let mut output = if number.is_sign_negative() {
        "-".to_owned()
    } else {
        String::new()
    };
    for (index, ch) in integer.chars().enumerate() {
        if group > 0 && index > 0 && (integer.len() - index).is_multiple_of(group) {
            output.push_str(&separator);
        }
        output.push(ch);
    }
    if let Some(fraction) = fraction {
        output.push_str(&crate::plugin::decimal_separator());
        output.push_str(fraction);
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sdk_masks_preserve_types_ranges_and_conditional_text() {
        let value = WdxValue {
            display: "33532878".into(),
            key: WdxKey::Integer(0x01ffabce),
        };
        for (token, expected) in [
            ("P.Size._X2", "01 FF AB CE"),
            ("P.Size._x04", "0000 0000 01ff abce"),
            ("P.Size._d3,", "33,532,878"),
        ] {
            let reference = FieldReference::parse(token).unwrap();
            assert_eq!(reference.query, "P.Size");
            assert_eq!(reference.render(Some(&value)).unwrap().unwrap(), expected);
        }
        let reference = FieldReference::parse(r#""("P.Title:2-4")""#).unwrap();
        assert_eq!(reference.query, "P.Title");
        assert_eq!(reference.render(None).unwrap(), Some(String::new()));
        let text = WdxValue {
            display: "A😀BCD".into(),
            key: WdxKey::Text(String::new()),
        };
        assert_eq!(
            reference.render(Some(&text)).unwrap().as_deref(),
            Some("(😀BC)")
        );
        assert!(FieldReference::parse("P.Size._z3").is_err());
        assert!(FieldReference::parse("P.Size._x00").is_err());
        let decimal = WdxValue {
            display: "1234567.25".into(),
            key: WdxKey::Float(1234567.25),
        };
        assert_eq!(
            FieldReference::parse("P.Size._d3,")
                .unwrap()
                .render(Some(&decimal))
                .unwrap()
                .as_deref(),
            Some(format!("1,234,567{}25", crate::plugin::decimal_separator()).as_str())
        );
        let decimal = WdxValue {
            display: "-42".into(),
            key: WdxKey::Float(-42.0),
        };
        assert_eq!(
            FieldReference::parse("P.Size._d3,")
                .unwrap()
                .render(Some(&decimal))
                .unwrap()
                .as_deref(),
            Some("-42")
        );
        assert!(
            FieldReference::parse("P.Size._x2")
                .unwrap()
                .render(Some(&text))
                .is_err()
        );
    }
}
