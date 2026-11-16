//! TC's documented WLX detection language. No plugin code runs in the UI process.
use super::{Interface, Registration, probe};
use crate::files;
use encoding_rs::WINDOWS_1252;
use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock, atomic::AtomicBool};
use std::time::SystemTime;

pub const WLX_DETECT_BYTES: usize = 8192;

#[derive(Clone, Debug)]
pub struct WlxDetection {
    expression: Option<Expression>,
    pub multimedia: bool,
}

#[derive(Clone, Debug)]
enum Expression {
    Number(u64),
    Text(Vec<u8>),
    Extension,
    Size,
    Force,
    Multimedia,
    Byte(usize),
    Find(Vec<u8>, bool),
    Not(Box<Self>),
    Compare(Box<Self>, Comparison, Box<Self>),
    And(Box<Self>, Box<Self>),
    Or(Box<Self>, Box<Self>),
}

#[derive(Clone, Copy, Debug)]
enum Comparison {
    Equal,
    Different,
    Less,
    Greater,
    LessEqual,
    GreaterEqual,
}

enum Value<'a> {
    Number(u64),
    Text(&'a [u8]),
    Missing,
}

impl Value<'_> {
    fn truth(&self) -> bool {
        match self {
            Self::Number(value) => *value != 0,
            Self::Text(value) => !value.is_empty(),
            Self::Missing => false,
        }
    }
    fn compare(&self, other: &Self, comparison: Comparison) -> bool {
        let order = match (self, other) {
            (Self::Number(left), Self::Number(right)) => Some(left.cmp(right)),
            (Self::Text(left), Self::Text(right)) => Some(left.cmp(right)),
            (Self::Number(left), Self::Text(right)) if right.len() == 1 => {
                Some(left.cmp(&u64::from(right[0])))
            }
            (Self::Text(left), Self::Number(right)) if left.len() == 1 => {
                Some(u64::from(left[0]).cmp(right))
            }
            _ => None,
        };
        order.is_some_and(|order| match comparison {
            Comparison::Equal => order.is_eq(),
            Comparison::Different => !order.is_eq(),
            Comparison::Less => order.is_lt(),
            Comparison::Greater => order.is_gt(),
            Comparison::LessEqual => !order.is_gt(),
            Comparison::GreaterEqual => !order.is_lt(),
        })
    }
}

pub struct WlxFile {
    extension: Vec<u8>,
    size: u64,
    header: Vec<u8>,
}

impl WlxFile {
    pub fn read(path: &Path, cancel: &AtomicBool) -> io::Result<Self> {
        files::check_cancel(cancel)?;
        let metadata = files::regular_metadata(path)?;
        if !metadata.is_file() {
            return Err(io::Error::other("Die WLX-Erkennung benötigt eine Datei."));
        }
        let mut header = Vec::new();
        File::open(path)?
            .take(WLX_DETECT_BYTES as u64)
            .read_to_end(&mut header)?;
        files::ensure_unchanged(path, &metadata)?;
        files::check_cancel(cancel)?;
        Ok(Self::new(path, metadata.len(), header))
    }
    fn new(path: &Path, size: u64, header: Vec<u8>) -> Self {
        let extension = path
            .extension()
            .unwrap_or_default()
            .to_string_lossy()
            .to_uppercase();
        Self {
            extension: WINDOWS_1252.encode(&extension).0.into_owned(),
            size,
            header,
        }
    }
}

impl WlxDetection {
    pub fn compile(source: &str) -> io::Result<Self> {
        if source.len() > WLX_DETECT_BYTES {
            return Err(invalid());
        }
        let mut parser = Parser {
            input: source.trim(),
            depth: 0,
            operators: 0,
            multimedia: false,
        };
        let expression = if parser.input.is_empty() {
            None
        } else {
            Some(parser.or()?)
        };
        if !parser.input.trim().is_empty() {
            return Err(invalid());
        }
        Ok(Self {
            expression,
            multimedia: parser.multimedia,
        })
    }
    pub fn for_registration(registration: &Registration, cancel: &AtomicBool) -> io::Result<Self> {
        if registration.interface != Interface::Wlx {
            return Err(invalid());
        }
        if let Some(rule) = &registration.tc_detection {
            return Self::compile(rule);
        }
        type Key = (PathBuf, u64, Option<SystemTime>);
        static CACHE: OnceLock<Mutex<HashMap<Key, String>>> = OnceLock::new();
        let metadata = files::regular_metadata(&registration.path)?;
        let key = (
            registration.path.clone(),
            metadata.len(),
            metadata.modified().ok(),
        );
        let cache = CACHE.get_or_init(Mutex::default);
        let cached = cache.lock().map_err(|_| invalid())?.get(&key).cloned();
        let rule = if let Some(rule) = cached {
            rule
        } else {
            let rows = probe(Interface::Wlx, &registration.path, cancel)?;
            let rule = rows
                .into_iter()
                .find(|row| row.name == "TC-Erkennung")
                .map(|row| row.value)
                .unwrap_or_default();
            let mut cache = cache.lock().map_err(|_| invalid())?;
            if cache.len() >= 256 {
                cache.clear();
            }
            cache.insert(key, rule.clone());
            rule
        };
        Self::compile(&rule)
    }
    pub fn matches(&self, file: &WlxFile, force: bool) -> bool {
        self.expression
            .as_ref()
            .is_none_or(|expression| expression.value(file, force).truth())
    }
}

impl Expression {
    fn value<'a>(&'a self, file: &'a WlxFile, force: bool) -> Value<'a> {
        let boolean = |value| Value::Number(u64::from(value));
        match self {
            Self::Number(value) => Value::Number(*value),
            Self::Text(value) => Value::Text(value),
            Self::Extension => Value::Text(&file.extension),
            Self::Size => Value::Number(file.size),
            Self::Force => boolean(force),
            Self::Multimedia => Value::Number(1),
            Self::Byte(index) => file
                .header
                .get(*index)
                .map_or(Value::Missing, |value| Value::Number(u64::from(*value))),
            Self::Find(needle, false) => boolean(
                needle.is_empty() || file.header.windows(needle.len()).any(|part| part == needle),
            ),
            Self::Find(needle, true) => boolean(
                WINDOWS_1252
                    .decode(&file.header)
                    .0
                    .to_lowercase()
                    .contains(&WINDOWS_1252.decode(needle).0.to_lowercase()),
            ),
            Self::Not(value) => boolean(!value.value(file, force).truth()),
            Self::Compare(left, comparison, right) => boolean(
                left.value(file, force)
                    .compare(&right.value(file, force), *comparison),
            ),
            Self::And(left, right) => {
                boolean(left.value(file, force).truth() && right.value(file, force).truth())
            }
            Self::Or(left, right) => {
                boolean(left.value(file, force).truth() || right.value(file, force).truth())
            }
        }
    }
}

struct Parser<'a> {
    input: &'a str,
    depth: usize,
    operators: usize,
    multimedia: bool,
}

impl Parser<'_> {
    fn operator(&mut self) -> io::Result<()> {
        self.operators += 1;
        if self.operators > 256 {
            Err(invalid())
        } else {
            Ok(())
        }
    }
    fn consume(&mut self, token: &str) -> bool {
        self.input = self.input.trim_start();
        if let Some(rest) = self.input.strip_prefix(token) {
            self.input = rest;
            true
        } else {
            false
        }
    }
    fn or(&mut self) -> io::Result<Expression> {
        let mut left = self.and()?;
        while self.consume("|") {
            self.operator()?;
            left = Expression::Or(Box::new(left), Box::new(self.and()?));
        }
        Ok(left)
    }
    fn and(&mut self) -> io::Result<Expression> {
        let mut left = self.compare()?;
        while self.consume("&") {
            self.operator()?;
            left = Expression::And(Box::new(left), Box::new(self.compare()?));
        }
        Ok(left)
    }
    fn compare(&mut self) -> io::Result<Expression> {
        let left = self.atom()?;
        for (token, comparison) in [
            ("!=", Comparison::Different),
            ("<=", Comparison::LessEqual),
            (">=", Comparison::GreaterEqual),
            ("=", Comparison::Equal),
            ("<", Comparison::Less),
            (">", Comparison::Greater),
        ] {
            if self.consume(token) {
                self.operator()?;
                return Ok(Expression::Compare(
                    Box::new(left),
                    comparison,
                    Box::new(self.atom()?),
                ));
            }
        }
        Ok(left)
    }
    fn atom(&mut self) -> io::Result<Expression> {
        self.input = self.input.trim_start();
        if self.consume("!") {
            self.depth += 1;
            if self.depth > 64 {
                return Err(invalid());
            }
            let value = self.atom()?;
            self.depth -= 1;
            return Ok(Expression::Not(Box::new(value)));
        }
        if self.consume("(") {
            self.depth += 1;
            if self.depth > 64 {
                return Err(invalid());
            }
            let result = self.or()?;
            self.depth -= 1;
            if !self.consume(")") {
                return Err(invalid());
            }
            return Ok(result);
        }
        if self.input.starts_with('"') {
            return Ok(Expression::Text(self.text()?));
        }
        if self.consume("[") {
            let index = self.number()?;
            if !self.consume("]") || index >= WLX_DETECT_BYTES as u64 {
                return Err(invalid());
            }
            return Ok(Expression::Byte(index as usize));
        }
        if self
            .input
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_digit)
        {
            return self.number().map(Expression::Number);
        }
        let length = self
            .input
            .bytes()
            .take_while(u8::is_ascii_alphabetic)
            .count();
        let word = self.input[..length].to_ascii_uppercase();
        self.input = &self.input[length..];
        match word.as_str() {
            "EXT" => Ok(Expression::Extension),
            "SIZE" => Ok(Expression::Size),
            "FORCE" => Ok(Expression::Force),
            "MULTIMEDIA" => {
                self.multimedia = true;
                Ok(Expression::Multimedia)
            }
            "FIND" | "FINDI" => {
                if !self.consume("(") {
                    return Err(invalid());
                }
                let needle = self.text()?;
                if !self.consume(")") {
                    return Err(invalid());
                }
                Ok(Expression::Find(needle, word == "FINDI"))
            }
            _ => Err(invalid()),
        }
    }
    fn number(&mut self) -> io::Result<u64> {
        self.input = self.input.trim_start();
        let length = self.input.bytes().take_while(u8::is_ascii_digit).count();
        let result = self.input[..length].parse().map_err(|_| invalid())?;
        self.input = &self.input[length..];
        Ok(result)
    }
    fn text(&mut self) -> io::Result<Vec<u8>> {
        if !self.consume("\"") {
            return Err(invalid());
        }
        let length = self.input.find('"').ok_or_else(invalid)?;
        let (value, _, errors) = WINDOWS_1252.encode(&self.input[..length]);
        if errors {
            return Err(invalid());
        }
        let value = value.into_owned();
        self.input = &self.input[length + 1..];
        Ok(value)
    }
}

fn invalid() -> io::Error {
    io::Error::other("Ungültige TC-Erkennungsregel für das WLX-Plugin.")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sdk_examples_cover_signatures_precedence_force_and_multimedia() {
        let wave = WlxFile::new(
            Path::new("test.wav"),
            2_000_000,
            b"RIFF\x00\x00WAVEfmt".to_vec(),
        );
        for rule in [
            "EXT=\"WAV\" | EXT=\"AVI\"",
            "EXT=\"WAV\" & [0]=\"R\" & [1]=\"I\" & [2]=\"F\" & [3]=\"F\" & FIND(\"WAVEfmt\")",
            "0 & 1 | 1 & 1",
            "!(SIZE<1000000)",
        ] {
            assert!(
                WlxDetection::compile(rule).unwrap().matches(&wave, false),
                "{rule}"
            );
        }
        let rule = WlxDetection::compile("EXT=\"WAV\" & (SIZE<1000000 | FORCE)").unwrap();
        assert!(!rule.matches(&wave, false));
        assert!(rule.matches(&wave, true));
        let rule = WlxDetection::compile("MULTIMEDIA & (EXT=\"WAV\" | EXT=\"MP3\")").unwrap();
        assert!(rule.multimedia && rule.matches(&wave, false));
        let zip = WlxFile::new(Path::new("data.bin"), 4, b"PK\x03\x04".to_vec());
        assert!(
            WlxDetection::compile("([0]=\"P\" & [1]=\"K\" & [2]=3 & [3]=4) | ([2]=7 & [3]=8)")
                .unwrap()
                .matches(&zip, false)
        );
    }
    #[test]
    fn detection_rejects_malformed_rules_and_unavailable_bytes() {
        let file = WlxFile::new(Path::new("a.txt"), 15, b"<body>Gr\xfc\xdfe".to_vec());
        assert!(
            WlxDetection::compile("FINDI(\"<BODY>\") & FINDI(\"GRÜßE\")")
                .unwrap()
                .matches(&file, false)
        );
        assert!(
            !WlxDetection::compile("[100]!=0 | SIZE=\"15\"")
                .unwrap()
                .matches(&file, false)
        );
        for rule in [
            "EXT=",
            "0 | BAD",
            "1 | (",
            "[8192]=1",
            "FIND(SIZE)",
            "1 &",
            "\"unterminated",
            "SIZE>18446744073709551616",
        ] {
            assert!(WlxDetection::compile(rule).is_err(), "{rule}");
        }
        assert!(WlxDetection::compile(&format!("{}1{}", "(".repeat(65), ")".repeat(65))).is_err());
    }
}
