use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};
use std::{cmp::Ordering, io};

#[derive(Debug, Deserialize, Serialize)]
pub struct WdxValue {
    pub display: String,
    pub key: WdxKey,
}

#[derive(Debug, Deserialize, Serialize)]
pub enum WdxKey {
    Integer(i64),
    Float(f64),
    Boolean(bool),
    DateTime(u64),
    Text(String),
}

impl WdxKey {
    fn compare(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Self::Integer(a), Self::Integer(b)) => a.cmp(b),
            (Self::Float(a), Self::Float(b)) => a.total_cmp(b),
            (Self::Boolean(a), Self::Boolean(b)) => a.cmp(b),
            (Self::DateTime(a), Self::DateTime(b)) => a.cmp(b),
            (Self::Text(a), Self::Text(b)) => a.cmp(b),
            _ => self.rank().cmp(&other.rank()),
        }
    }

    fn rank(&self) -> u8 {
        match self {
            Self::Integer(_) => 0,
            Self::Float(_) => 1,
            Self::Boolean(_) => 2,
            Self::DateTime(_) => 3,
            Self::Text(_) => 4,
        }
    }
}

impl Eq for WdxKey {}

impl PartialEq for WdxKey {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other).is_eq()
    }
}

impl PartialOrd for WdxKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for WdxKey {
    fn cmp(&self, other: &Self) -> Ordering {
        self.compare(other)
    }
}

pub fn wdx_order(a: Option<&WdxValue>, b: Option<&WdxValue>, descending: bool) -> Ordering {
    match (a, b) {
        (Some(a), Some(b)) if descending => b.key.compare(&a.key),
        (Some(a), Some(b)) => a.key.compare(&b.key),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

pub struct WdxMatcher(Matcher);

enum Matcher {
    Contains(String),
    Regex(Regex),
    Compare {
        comparison: Comparison,
        text: String,
        integer: Option<i64>,
        float: Option<f64>,
        boolean: Option<bool>,
    },
}

#[derive(Clone, Copy)]
enum Comparison {
    Equal,
    Different,
    Greater,
    AtLeast,
    Less,
    AtMost,
}

impl WdxMatcher {
    pub fn parse(query: &str) -> io::Result<Self> {
        let query = query.trim();
        if let Some(regex) = query.strip_prefix('~') {
            if regex.is_empty() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "Ein Suchausdruck fehlt.",
                ));
            }
            return RegexBuilder::new(regex)
                .case_insensitive(true)
                .build()
                .map(|regex| Self(Matcher::Regex(regex)))
                .map_err(io::Error::other);
        }
        for (symbol, comparison) in [
            (">=", Comparison::AtLeast),
            ("<=", Comparison::AtMost),
            ("!=", Comparison::Different),
            ("=", Comparison::Equal),
            (">", Comparison::Greater),
            ("<", Comparison::Less),
        ] {
            if let Some(value) = query.strip_prefix(symbol) {
                let text = value.trim().to_lowercase();
                let boolean = match text.as_str() {
                    "ja" | "true" | "1" => Some(true),
                    "nein" | "false" | "0" => Some(false),
                    _ => None,
                };
                return Ok(Self(Matcher::Compare {
                    comparison,
                    integer: text.parse().ok(),
                    float: text
                        .replace(',', ".")
                        .parse::<f64>()
                        .ok()
                        .filter(|number| number.is_finite()),
                    boolean,
                    text,
                }));
            }
        }
        Ok(Self(Matcher::Contains(query.to_lowercase())))
    }

    pub fn matches(&self, value: &WdxValue) -> bool {
        match &self.0 {
            Matcher::Contains(text) => value.display.to_lowercase().contains(text),
            Matcher::Regex(regex) => regex.is_match(&value.display),
            Matcher::Compare {
                comparison,
                text,
                integer,
                float,
                boolean,
            } => {
                let order = match &value.key {
                    WdxKey::Integer(value) => integer.map(|other| value.cmp(&other)),
                    WdxKey::Float(value) => float.and_then(|other| value.partial_cmp(&other)),
                    WdxKey::Boolean(value) => boolean.map(|other| value.cmp(&other)),
                    WdxKey::DateTime(_) => Some(value.display.to_lowercase().cmp(text)),
                    WdxKey::Text(value) => Some(value.cmp(text)),
                };
                order.is_some_and(|order| match comparison {
                    Comparison::Equal => order.is_eq(),
                    Comparison::Different => !order.is_eq(),
                    Comparison::Greater => order.is_gt(),
                    Comparison::AtLeast => !order.is_lt(),
                    Comparison::Less => order.is_lt(),
                    Comparison::AtMost => !order.is_gt(),
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numeric_comparisons_do_not_sort_as_text_or_round_large_integers() {
        let value = |number: i64| WdxValue {
            display: number.to_string(),
            key: WdxKey::Integer(number),
        };
        let small = value(2);
        let large = value(10);
        assert_eq!(wdx_order(Some(&small), Some(&large), false), Ordering::Less);
        assert_eq!(
            wdx_order(Some(&small), Some(&large), true),
            Ordering::Greater
        );
        assert_eq!(wdx_order(None, Some(&large), true), Ordering::Greater);
        assert!(WdxMatcher::parse(">= 10").unwrap().matches(&large));
        assert!(!WdxMatcher::parse(">= 10").unwrap().matches(&small));
        assert!(
            WdxMatcher::parse("> 9007199254740992")
                .unwrap()
                .matches(&value(9007199254740993))
        );
    }

    #[test]
    fn text_filters_support_substrings_equality_and_regular_expressions() {
        let value = WdxValue {
            display: "Grüße aus Köln".into(),
            key: WdxKey::Text("grüße aus köln".into()),
        };
        for query in ["KÖLN", "= GRÜẞE AUS KÖLN", "~^grüße.*köln$", "!= Berlin"] {
            assert!(WdxMatcher::parse(query).unwrap().matches(&value), "{query}");
        }
        assert!(!WdxMatcher::parse("= Köln").unwrap().matches(&value));
        assert!(WdxMatcher::parse("~[").is_err());
    }
}
