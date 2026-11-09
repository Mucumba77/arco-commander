use super::ListArgument;
use crate::display_path;
use crate::panel::{Location, Tab};
use arco_core::profile::VirtualTab;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Default)]
struct ListOptions {
    both_panels: bool,
    marked_only: bool,
    omit_empty: bool,
}

pub(super) struct TcArguments<'a> {
    arguments: Vec<Vec<Part<'a>>>,
    lists: ListOptions,
    archive_paths: bool,
}

enum Part<'a> {
    Text(&'a str),
    Value(Value<'a>, Option<Substring>),
    Selection(SelectionArgument),
    List(ListArgument),
    PanelOrder(bool),
    StripEnvironmentQuotes,
}

#[derive(Clone, Copy)]
enum Value<'a> {
    Field(char),
    Environment(&'a str),
    Compare { index: usize, short: bool },
}

#[derive(Clone, Copy)]
struct Substring {
    start: i32,
    length: Option<i32>,
    name_part: bool,
}

#[derive(Clone, Copy)]
struct SelectionArgument {
    target: bool,
    full_path: Option<bool>,
    short_name: bool,
    range: SelectionRange,
}

#[derive(Clone, Copy)]
enum SelectionRange {
    First(usize),
    Forward { first: usize, last: usize },
    FromEnd { first: usize, last: usize },
}

impl<'a> TcArguments<'a> {
    pub(super) fn parse(arguments: &'a [String]) -> io::Result<Self> {
        let mut parsed = Self {
            arguments: Vec::with_capacity(arguments.len()),
            lists: ListOptions::default(),
            archive_paths: false,
        };
        let mut list_count = 0;
        for argument in arguments {
            let parts = parsed.parse_argument(argument)?;
            list_count += parts
                .iter()
                .filter(|part| matches!(part, Part::List(_)))
                .count();
            if parts
                .iter()
                .filter(|part| matches!(part, Part::List(_) | Part::Selection(_)))
                .count()
                > 1
            {
                return Err(io::Error::other(
                    "Pro Programmargument ist ein TC-Mehrdateiplatzhalter möglich.",
                ));
            }
            if !parts.is_empty() || argument.is_empty() {
                parsed.arguments.push(parts);
            }
        }
        if list_count > 1 || (list_count == 0 && parsed.lists.both_panels) {
            return Err(io::Error::other(
                "Ein TC-Aufruf unterstützt eine Listendatei. %a benötigt eine Listenmaske.",
            ));
        }
        Ok(parsed)
    }

    fn parse_argument(&mut self, mut argument: &'a str) -> io::Result<Vec<Part<'a>>> {
        let mut parts = Vec::new();
        while let Some((text, parameter)) = argument.split_once('%') {
            if !text.is_empty() {
                parts.push(Part::Text(text));
            }
            let Some(first) = parameter.as_bytes().first().copied().filter(u8::is_ascii) else {
                return Err(invalid(parameter));
            };
            let remainder = &parameter[1..];
            match first {
                b'a' => self.lists.both_panels = true,
                b'Y' => self.lists.marked_only = true,
                b'y' => self.lists.omit_empty = true,
                b'Z' => self.archive_paths = true,
                b'%' => parts.push(Part::Text("%")),
                b'X' | b'x' => parts.push(Part::PanelOrder(first == b'X')),
                b'q' => parts.push(Part::StripEnvironmentQuotes),
                _ => {
                    let (part, rest) = parse_placeholder(parameter)?;
                    parts.push(part);
                    argument = rest;
                    continue;
                }
            }
            argument = remainder;
        }
        if !argument.is_empty() {
            parts.push(Part::Text(argument));
        }
        Ok(parts)
    }

    pub(super) fn expand(
        &self,
        tabs: [&Tab; 2],
        active: usize,
    ) -> io::Result<(Vec<String>, Option<tempfile::TempPath>)> {
        let mut arguments = Vec::new();
        let mut list_file = None;
        let mut source = active;
        let mut strip_quotes = false;
        for parts in &self.arguments {
            let list_entries = self.list_entries(parts, tabs, active, source)?;
            let omit_argument =
                self.lists.omit_empty && list_entries.as_ref().is_some_and(Vec::is_empty);
            let mut text = String::new();
            let mut selection = None;
            let mut prefix = String::new();
            let mut before_extension = None;
            let mut list_entries = list_entries;
            let mut emitted = parts.is_empty();
            for part in parts {
                match part {
                    Part::PanelOrder(physical) => source = if *physical { 0 } else { active },
                    Part::StripEnvironmentQuotes => strip_quotes = !strip_quotes,
                    _ if omit_argument => (),
                    Part::Text(value) => {
                        text.push_str(value);
                        emitted = true;
                    }
                    Part::Value(Value::Field('E'), None)
                        if selection.is_some() && before_extension.is_none() =>
                    {
                        before_extension = Some(std::mem::take(&mut text));
                    }
                    Part::Value(value, substring) => {
                        let value =
                            expand_value(*value, tabs, source, self.archive_paths, strip_quotes)?;
                        if let Some(substring) = substring {
                            text.push_str(&substring.apply(&value));
                        } else {
                            text.push_str(&value);
                        }
                        emitted = true;
                    }
                    Part::Selection(argument) => {
                        selection = Some((*argument, source ^ usize::from(argument.target)));
                        prefix = std::mem::take(&mut text);
                    }
                    Part::List(argument) => {
                        let file =
                            argument.write(list_entries.take().expect("parsed list entries"))?;
                        text.push_str(checked_path(&file)?);
                        list_file = Some(file);
                        emitted = true;
                    }
                }
            }
            if let Some((selection, side)) = selection {
                let tab = local_tab(tabs[side])?;
                let paths = if self.lists.marked_only && !self.lists.omit_empty {
                    tab.marked_paths()
                } else {
                    tab.paths()
                };
                for path in selection.range.select(paths) {
                    let name = selection.name(&path)?;
                    arguments.push(decorate_selection(
                        &prefix,
                        &name,
                        before_extension.as_deref(),
                        &text,
                    ));
                }
            } else if emitted && !omit_argument {
                arguments.push(text);
            }
        }
        Ok((arguments, list_file))
    }

    fn list_entries(
        &self,
        parts: &[Part<'_>],
        tabs: [&Tab; 2],
        active: usize,
        mut source: usize,
    ) -> io::Result<Option<Vec<PathBuf>>> {
        for part in parts {
            match part {
                Part::PanelOrder(physical) => source = if *physical { 0 } else { active },
                Part::List(_) => {
                    let panel = local_tab(tabs[source])?;
                    let mut entries = if self.lists.marked_only || self.lists.omit_empty {
                        panel.marked_paths()
                    } else {
                        panel.paths()
                    };
                    if self.lists.both_panels {
                        entries.extend(local_tab(tabs[1 - source])?.marked_paths());
                    }
                    if self.lists.marked_only && self.lists.omit_empty && entries.is_empty() {
                        entries = panel.paths();
                    }
                    return Ok(Some(entries));
                }
                _ => (),
            }
        }
        Ok(None)
    }
}

fn parse_placeholder(parameter: &str) -> io::Result<(Part<'_>, &str)> {
    if let Some((argument, rest)) = SelectionArgument::parse(parameter)? {
        return Ok((Part::Selection(argument), rest));
    }
    for token in ["UL", "UF", "WL", "WF", "L", "F", "l", "f", "D", "d"] {
        if let Some(rest) = parameter.strip_prefix(token) {
            let list = ListArgument::parse(&format!("%{token}")).expect("known list token");
            return Ok((Part::List(list), rest));
        }
    }
    let (value, rest) = match parameter.as_bytes().first().copied() {
        Some(
            key @ (b'P' | b'T' | b'N' | b'M' | b'O' | b'E' | b'p' | b't' | b'n' | b'm' | b'o'
            | b'e' | b'v' | b'V'),
        ) => (Value::Field(char::from(key)), &parameter[1..]),
        Some(key @ (b'C' | b'c')) => {
            let Some(index @ b'1'..=b'9') = parameter.as_bytes().get(1).copied() else {
                return Err(invalid(parameter));
            };
            (
                Value::Compare {
                    index: usize::from(index - b'1'),
                    short: key == b'c',
                },
                &parameter[2..],
            )
        }
        Some(b'|') => {
            let (name, rest) = parameter[1..]
                .split_once('|')
                .filter(|(name, _)| !name.is_empty() && !name.contains(['\0', '=', '%']))
                .ok_or_else(|| invalid(parameter))?;
            (Value::Environment(name), rest)
        }
        _ => return Err(invalid(parameter)),
    };
    let (substring, rest) = Substring::parse(rest)?;
    Ok((Part::Value(value, substring), rest))
}

impl SelectionArgument {
    fn parse(parameter: &str) -> io::Result<Option<(Self, &str)>> {
        let (full_path, selection) = match parameter.as_bytes() {
            [
                path @ (b'P' | b'T' | b'p' | b't'),
                b'%',
                b'S' | b'R' | b's' | b'r',
                ..,
            ] => (Some(path.is_ascii_lowercase()), &parameter[2..]),
            _ => (None, parameter),
        };
        let Some(key @ (b'S' | b'R' | b's' | b'r')) = selection.as_bytes().first().copied() else {
            return Ok(None);
        };
        let (range, rest) = SelectionRange::parse(&selection[1..])?;
        Ok(Some((
            Self {
                target: key.eq_ignore_ascii_case(&b'R'),
                full_path,
                short_name: key.is_ascii_lowercase(),
                range,
            },
            rest,
        )))
    }

    fn name(self, path: &Path) -> io::Result<String> {
        let short_name = self
            .short_name
            .then(|| arco_windows::short_path(path))
            .transpose()?;
        let name = short_name
            .as_deref()
            .unwrap_or(path)
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| {
                io::Error::other("Der ausgewählte Dateiname ist nicht in Unicode darstellbar.")
            })?;
        let Some(short_parent) = self.full_path else {
            return Ok(name.to_owned());
        };
        if !short_parent && !self.short_name {
            checked_path(path)?;
            return Ok(display_path(path));
        }
        let parent = path
            .parent()
            .ok_or_else(|| io::Error::other("Der ausgewählte Pfad hat keinen Elternordner."))?;
        let parent = if short_parent {
            arco_windows::short_path(parent)?.join(name)
        } else {
            parent.join(name)
        };
        Ok(display_path(Path::new(checked_path(&parent)?)))
    }
}

impl SelectionRange {
    fn parse(input: &str) -> io::Result<(Self, &str)> {
        match input.as_bytes() {
            [sign @ (b'+' | b'-'), b'0'..=b'9', ..] => {
                let (first, rest) = positive_integer(&input[1..]).ok_or_else(|| invalid(input))?;
                let (last, rest) = match rest.strip_prefix('-') {
                    Some(rest) if rest.as_bytes().first().is_some_and(u8::is_ascii_digit) => {
                        positive_integer(rest).ok_or_else(|| invalid(input))?
                    }
                    Some(rest) => (if *sign == b'+' { usize::MAX } else { 1 }, rest),
                    None => (first, rest),
                };
                let range = match *sign {
                    b'+' if first <= last => Self::Forward { first, last },
                    b'-' if first >= last => Self::FromEnd { first, last },
                    _ => return Err(invalid(input)),
                };
                Ok((range, rest))
            }
            [b'0'..=b'9', ..] => positive_integer(input)
                .map(|(count, rest)| (Self::First(count), rest))
                .ok_or_else(|| invalid(input)),
            _ => Ok((Self::First(usize::MAX), input)),
        }
    }

    fn select<T>(self, paths: Vec<T>) -> impl Iterator<Item = T> {
        let length = paths.len();
        let (skip, take) = match self {
            Self::First(limit) => (0, limit),
            Self::Forward { first, last } => (first - 1, last - first + 1),
            Self::FromEnd { first, last } => {
                let start = length.saturating_sub(first);
                let end = length.saturating_sub(last - 1);
                (start, end - start)
            }
        };
        paths.into_iter().skip(skip).take(take)
    }
}

fn positive_integer(input: &str) -> Option<(usize, &str)> {
    let length = input.bytes().take_while(u8::is_ascii_digit).count();
    let value = input[..length]
        .parse::<usize>()
        .ok()
        .filter(|value| *value > 0)?;
    Some((value, &input[length..]))
}

impl Substring {
    fn parse(input: &str) -> io::Result<(Option<Self>, &str)> {
        let Some(value) = input.strip_prefix(":~") else {
            return Ok((None, input));
        };
        let (start, rest) = signed_integer(value).ok_or_else(|| invalid(input))?;
        let name_part = start == 0 && value.starts_with('-');
        let (length, rest) = if let Some(length) = rest.strip_prefix(',') {
            let (length, rest) = signed_integer(length).ok_or_else(|| invalid(input))?;
            (Some(length), rest)
        } else {
            (None, rest)
        };
        Ok((
            Some(Self {
                start,
                length,
                name_part,
            }),
            rest.strip_prefix('~').unwrap_or(rest),
        ))
    }

    fn apply(self, value: &str) -> String {
        if self.name_part {
            let length = self
                .length
                .map_or(usize::MAX, |length| length.unsigned_abs() as usize);
            let part = if self.length.is_some_and(|length| length < 0) {
                Path::new(value).extension()
            } else {
                Path::new(value).file_stem()
            };
            return part
                .and_then(|part| part.to_str())
                .unwrap_or_default()
                .chars()
                .take(length)
                .collect();
        }
        let characters: Vec<_> = value.chars().collect();
        let count = characters.len();
        let start = if self.start < 0 {
            count.saturating_sub(self.start.unsigned_abs() as usize)
        } else {
            (self.start as usize).min(count)
        };
        let end = match self.length {
            Some(length) if length < 0 => count.saturating_sub(length.unsigned_abs() as usize),
            Some(length) => start.saturating_add(length as usize).min(count),
            None => count,
        };
        characters[start..end.max(start)].iter().collect()
    }
}

fn signed_integer(input: &str) -> Option<(i32, &str)> {
    let sign = usize::from(input.starts_with(['+', '-']));
    let digits = input[sign..].bytes().take_while(u8::is_ascii_digit).count();
    let length = sign + digits;
    (digits != 0)
        .then(|| (input[..length].parse().ok(), &input[length..]))
        .and_then(|(value, rest)| Some((value?, rest)))
}

fn expand_value(
    value: Value<'_>,
    tabs: [&Tab; 2],
    source: usize,
    archive_paths: bool,
    strip_quotes: bool,
) -> io::Result<String> {
    let field = match value {
        Value::Environment(name) => {
            let value = match std::env::var(name) {
                Ok(value) => value,
                Err(std::env::VarError::NotPresent) => String::new(),
                Err(error) => return Err(io::Error::new(io::ErrorKind::InvalidData, error)),
            };
            return Ok(if strip_quotes {
                value.replace('"', "")
            } else {
                value
            });
        }
        Value::Compare { index, short } => return compare_path(tabs, source, index, short),
        Value::Field(field) => field,
    };
    if matches!(field, 'v' | 'V') {
        return wfx_argument(tabs[source], field == 'V');
    }
    let upper = field.to_ascii_uppercase();
    let short = field.is_ascii_lowercase();
    let tab = tabs[source ^ usize::from(matches!(upper, 'T' | 'M'))];
    if matches!(upper, 'P' | 'T') {
        if archive_paths && let Location::Archive { file, .. } = tab.location() {
            return format_path(file, short);
        }
        let path = local_tab(tab)?.path.clone();
        let mut path = format_path(path, short)?;
        if !path.ends_with(['\\', '/']) {
            path.push('\\');
        }
        return Ok(path);
    }
    let entry = tab.current().ok_or_else(|| {
        io::Error::other("Der TC-Platzhalter benötigt einen fokussierten Dateinamen.")
    })?;
    let short_path = short
        .then(|| arco_windows::short_path(&entry.path))
        .transpose()?;
    let name = short_path
        .as_deref()
        .and_then(Path::file_name)
        .unwrap_or(&entry.name);
    let name = Path::new(name);
    let component = match upper {
        'O' => name.file_stem(),
        'E' => name.extension(),
        _ => Some(name.as_os_str()),
    };
    checked_path(Path::new(component.unwrap_or_default())).map(str::to_owned)
}

fn wfx_argument(tab: &Tab, full_path: bool) -> io::Result<String> {
    let location = tab
        .wfx
        .as_ref()
        .ok_or_else(|| io::Error::other("%v und %V benötigen ein WFX-Dateifenster."))?;
    let entry = tab
        .display_row(tab.cursor)
        .ok_or_else(|| io::Error::other("Wähle eine Datei oder einen Ordner im WFX-Fenster."))?;
    let name = arco_core::files::leaf_name(&entry.name)?
        .to_str()
        .filter(|name| !name.contains(['\\', '/']) && !name.chars().any(char::is_control))
        .ok_or_else(|| io::Error::other("Ungültiger virtueller Dateiname."))?;
    if full_path {
        VirtualTab::Wfx {
            plugin: location.plugin.name.clone(),
            directory: location.child(name),
        }
        .tc_path()
        .ok_or_else(|| io::Error::other("Ungültiger virtueller WFX-Pfad."))
    } else {
        Ok(name.to_owned())
    }
}

fn compare_path(tabs: [&Tab; 2], source: usize, index: usize, short: bool) -> io::Result<String> {
    let first_panel = local_tab(tabs[source])?;
    let marked = first_panel.marked_paths();
    let path = if index >= 2 || marked.len() >= 2 {
        marked.get(index).cloned()
    } else {
        let first = marked
            .into_iter()
            .next()
            .or_else(|| first_panel.current().map(|entry| entry.path.clone()));
        if index == source {
            first
        } else {
            let other = local_tab(tabs[1 - source])?;
            other.marked_paths().into_iter().next().or_else(|| {
                let candidate = other.path.join(first.as_ref()?.file_name()?);
                candidate.is_file().then_some(candidate)
            })
        }
    };
    path.map(|path| format_path(path, short))
        .transpose()
        .map(Option::unwrap_or_default)
}

fn decorate_selection(
    prefix: &str,
    name: &str,
    before_extension: Option<&str>,
    suffix: &str,
) -> String {
    if let Some(before_extension) = before_extension
        && let Some(extension) = Path::new(name)
            .extension()
            .and_then(|extension| extension.to_str())
    {
        let stem = &name[..name.len() - extension.len() - 1];
        return format!("{prefix}{stem}{before_extension}.{extension}{suffix}");
    }
    format!(
        "{prefix}{name}{}{suffix}",
        before_extension.unwrap_or_default()
    )
}

fn local_tab(tab: &Tab) -> io::Result<&Tab> {
    tab.is_local()
        .then_some(tab)
        .ok_or_else(|| io::Error::other("Dieser TC-Platzhalter benötigt ein lokales Dateifenster."))
}

fn checked_path(path: &Path) -> io::Result<&str> {
    path.to_str()
        .ok_or_else(|| io::Error::other("Der TC-Platzhalter benötigt einen Unicode-Pfad."))
}

fn format_path(path: PathBuf, short: bool) -> io::Result<String> {
    let path = if short {
        arco_windows::short_path(&path)?
    } else {
        path
    };
    checked_path(&path)?;
    Ok(display_path(&path))
}

fn invalid(parameter: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("Ungültiger oder nicht unterstützter TC-Parameter bei „%{parameter}“."),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use arco_core::files::{Entry, Kind};

    fn arguments(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|part| (*part).to_owned()).collect()
    }

    fn panel(path: &str, name: Option<&str>) -> Tab {
        let path = PathBuf::from(path);
        let mut tab = Tab::new(path.clone());
        if let Some(name) = name {
            tab.populate(
                vec![Entry::from_remote(
                    path.join(name),
                    Kind::File,
                    Some(5),
                    None,
                )],
                true,
                String::new(),
            );
            tab.select_row(1, false, false);
        }
        tab
    }

    #[test]
    fn switches_order_and_keeps_selected_names_in_single_arguments() {
        let left = panel("C:/Left", Some("Datei %T mit Leerzeichen.txt"));
        let right = panel("C:/Right", None);
        let original = arguments(&[
            "%X%P:~0,-1",
            "%T:~0,-1",
            "%x%P:~0,-1",
            "%Xpre_%S_tag%E.bak",
            "%%",
        ]);
        let (expanded, _) = TcArguments::parse(&original)
            .unwrap()
            .expand([&left, &right], 1)
            .unwrap();
        assert_eq!(
            expanded,
            [
                "C:/Left",
                "C:/Right",
                "C:/Right",
                "pre_Datei %T mit Leerzeichen_tag.txt.bak",
                "%"
            ]
        );
    }

    #[test]
    fn keeps_attached_list_prefixes_and_omits_only_empty_list_arguments() {
        let left = panel("C:/Left", None);
        let right = panel("C:/Right", None);
        let original = arguments(&["/O=%y%a%WL", "%X%P:~0,-1", ""]);
        let (expanded, list) = TcArguments::parse(&original)
            .unwrap()
            .expand([&left, &right], 1)
            .unwrap();
        assert_eq!(expanded, ["C:/Left", ""]);
        assert!(list.is_none());

        let original = arguments(&["/O=%Y%a%UL"]);
        let (expanded, list) = TcArguments::parse(&original)
            .unwrap()
            .expand([&left, &right], 0)
            .unwrap();
        let list = list.unwrap();
        assert_eq!(expanded, [format!("/O={}", list.to_str().unwrap())]);
        assert_eq!(std::fs::read(&list).unwrap(), b"\xef\xbb\xbf");
    }

    #[test]
    fn selects_ranges_and_slices_unicode_without_reinterpreting_expanded_text() {
        for (range, expected) in [
            ("", &[1, 2, 3, 4][..]),
            ("+2-3", &[2, 3][..]),
            ("+3-", &[3, 4][..]),
            ("-3-2", &[2, 3][..]),
            ("-2-", &[3, 4][..]),
            ("-1", &[4][..]),
            ("-5", &[][..]),
        ] {
            let (range, _) = SelectionRange::parse(range).unwrap();
            assert_eq!(range.select(vec![1, 2, 3, 4]).collect::<Vec<_>>(), expected);
        }
        let (slice, rest) = Substring::parse(":~1,2~34").unwrap();
        assert_eq!(slice.unwrap().apply("A😀B"), "😀B");
        assert_eq!(rest, "34");
        let (slice, _) = Substring::parse(":~-0,-2").unwrap();
        assert_eq!(slice.unwrap().apply("Datei.txt"), "tx");
        assert!(TcArguments::parse(&arguments(&["%S0"])).is_err());
        assert!(TcArguments::parse(&arguments(&["%ä"])).is_err());
    }

    #[test]
    fn expands_wfx_names_without_using_the_local_fallback_or_plugin_dll_path() {
        use crate::panel::WfxLocation;
        use arco_core::plugin::{Architecture, Interface, Registration};
        use std::sync::Arc;

        let left = panel("C:/Left", None);
        let mut right = panel("C:/Local fallback", None);
        right.wfx = Some(WfxLocation {
            plugin: Arc::new(Registration {
                name: "Mein Plugin".into(),
                path: PathBuf::from("C:/Plugin/test.wfx64"),
                interface: Interface::Wfx,
                architecture: Architecture::X64,
                enabled: true,
                masks: String::new(),
                tc_detection: None,
                package: None,
            }),
            directory: "\\Unterordner".into(),
        });
        let name = "Datei ä %T.txt";
        right.populate(
            vec![Entry::from_remote(name.into(), Kind::File, Some(5), None)],
            true,
            String::new(),
        );
        right.select_row(1, false, false);
        let original = arguments(&["%v", "/virtual=%V", "%v:~0,5"]);
        let parser = TcArguments::parse(&original).unwrap();
        let (expanded, list) = parser.expand([&left, &right], 1).unwrap();
        assert_eq!(
            expanded,
            [
                name,
                r"/virtual=\\\Mein Plugin\Unterordner\Datei ä %T.txt",
                "Datei"
            ]
        );
        assert!(list.is_none());
        assert!(parser.expand([&left, &right], 0).is_err());
        right.select_row(0, false, false);
        assert!(parser.expand([&left, &right], 1).is_err());
    }
}
