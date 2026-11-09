use super::{
    Column, Field, MAX_COLUMNS, Mode, ModeColors, Rule, Scope, TcColumnExpression, TcColumnLayout,
};
use crate::files::{Entry, Kind, SortKey, SortOrder};
use crate::plugin::WdxColumn;
use crate::profile::{self, TabView};
use crate::sync::FileMasks;
use globset::{GlobBuilder, GlobMatcher};
use std::{collections::BTreeMap, ffi::OsString, io, path::Path};

pub struct TcViews {
    pub columns: BTreeMap<String, Vec<Column>>,
    pub column_layouts: BTreeMap<u16, TcColumnLayout>,
    pub modes: BTreeMap<String, Mode>,
    pub rules: Vec<Rule>,
    pub automatic: Option<bool>,
    pub skipped: usize,
}

pub fn read_tc_views(
    path: &Path,
    resolve_variable: impl Fn(&str) -> Option<OsString>,
    resolve_wdx: impl Fn(&str) -> Option<WdxColumn>,
) -> io::Result<TcViews> {
    let text = profile::read_tc_text(path)?;
    let (column_sets, skipped) = read_columns(&text, resolve_wdx);
    let mut mode_rows = BTreeMap::<u16, [&str; 4]>::new();
    let mut rule_rows = BTreeMap::<u16, [&str; 2]>::new();
    let mut result = TcViews {
        columns: BTreeMap::new(),
        column_layouts: BTreeMap::new(),
        modes: BTreeMap::new(),
        rules: Vec::new(),
        automatic: None,
        skipped,
    };
    for (section, key, value) in profile::ini_entries(&text) {
        if section == "viewmodeswitch" && key == "enabled" {
            result.automatic = match value {
                "0" => Some(false),
                "1" => Some(true),
                _ => None,
            };
        }
        let Some((id, field)) = key.split_once('_') else {
            continue;
        };
        let Ok(id) = id.parse() else { continue };
        match (section.as_str(), field) {
            ("viewmodes", "name") => mode_rows.entry(id).or_default()[0] = value,
            ("viewmodes", "options") => mode_rows.entry(id).or_default()[1] = value,
            ("viewmodes", "icon") => mode_rows.entry(id).or_default()[2] = value,
            ("viewmodes", "commands") => mode_rows.entry(id).or_default()[3] = value,
            ("viewmodeswitch", "rules") => rule_rows.entry(id).or_default()[0] = value,
            ("viewmodeswitch", "mode") => rule_rows.entry(id).or_default()[1] = value,
            _ => {}
        }
    }
    let mut names = BTreeMap::new();
    for (id, [name, options, icon, commands]) in mode_rows {
        let Some((mode, partial)) = parse_mode(options, &column_sets) else {
            result.skipped += 1;
            continue;
        };
        let name = name.trim();
        if name.is_empty()
            || name.len() > 160
            || name.contains('|')
            || name.chars().any(char::is_control)
            || result.modes.contains_key(name)
            || result.modes.len() == 128
        {
            result.skipped += 1;
            continue;
        }
        if partial || !icon.is_empty() || !commands.is_empty() {
            result.skipped += 1;
        }
        names.insert(id, name.to_owned());
        result.modes.insert(name.to_owned(), mode);
    }
    for [rules, mode] in rule_rows.into_values() {
        let name = mode.parse::<u16>().ok().and_then(|id| names.get(&id));
        // An unadorned % is a rule operator. Expand paired path variables per operand.
        let expression = expand_expression(rules, &resolve_variable);
        if let (Some(mode), Some(expression)) = (name, expression)
            && result.rules.len() < 128
            && parse_expression(&expression).is_ok()
        {
            result.rules.push(Rule {
                mode: mode.clone(),
                tc_expression: Some(expression),
                ..Rule::default()
            });
        } else {
            result.skipped += 1;
        }
    }
    for (id, set) in column_sets {
        result.columns.insert(set.name.clone(), set.columns);
        result.column_layouts.insert(
            id,
            TcColumnLayout {
                name: set.name,
                sort_keys: set.sort_keys,
            },
        );
    }
    Ok(result)
}

struct TcColumnSet {
    name: String,
    columns: Vec<Column>,
    sort_keys: Vec<Option<SortKey>>,
}

fn read_columns(
    text: &str,
    resolve_wdx: impl Fn(&str) -> Option<WdxColumn>,
) -> (BTreeMap<u16, TcColumnSet>, usize) {
    let mut rows = BTreeMap::<u16, [&str; 4]>::new();
    let mut titles = Vec::new();
    for (section, key, value) in profile::ini_entries(text) {
        if section != "customfields" {
            continue;
        }
        if key == "titles" {
            titles = value.split('|').collect();
        }
        for (index, prefix) in ["headers", "contents", "widths", "options"]
            .into_iter()
            .enumerate()
        {
            if let Some(id) = key
                .strip_prefix(prefix)
                .and_then(|id| id.parse::<u16>().ok())
                && (1..=128).contains(&id)
            {
                rows.entry(id).or_default()[index] = value;
            }
        }
    }
    let mut result = BTreeMap::new();
    let mut used = std::collections::BTreeSet::new();
    let mut skipped = 0;
    for (id, [headers, contents, widths, options]) in rows {
        let title = titles
            .get(usize::from(id - 1))
            .map(|title| title.trim())
            .filter(|title| !title.is_empty() && !title.chars().any(char::is_control));
        let mut name = title.map_or_else(
            || format!("TC-Spalten {id}"),
            |title| title.chars().take(60).collect(),
        );
        while !used.insert(name.clone()) {
            name.push_str(&format!(" ({id})"));
        }
        let mut columns = Vec::new();
        let widths: Vec<_> = widths
            .split(',')
            .map(|value| value.trim().parse::<i32>().ok())
            .collect();
        let dpi = options
            .split('|')
            .nth(2)
            .and_then(|dpi| dpi.parse::<u32>().ok())
            .filter(|dpi| *dpi > 0)
            .unwrap_or(96);
        let size = |index: usize, default| {
            widths
                .get(index)
                .copied()
                .flatten()
                .map_or((default, None), |width| {
                    let scaled = u64::from(width.unsigned_abs()) * 96 / u64::from(dpi);
                    (scaled.clamp(40, 600) as u16, Some(width < 0))
                })
        };
        if widths.get(1).copied().flatten() != Some(0) {
            let (width, right_aligned) = size(1, 60);
            columns.push(Column {
                field: Field::Extension,
                title: "Erw.".into(),
                width,
                right_aligned,
            });
        }
        let mut sort_keys = Vec::new();
        let base_columns = columns.len();
        let mut headings = headers.split(r"\n");
        let mut wdx_index = 0;
        for (index, expression) in contents.split(r"\n").enumerate() {
            let heading = headings.next().unwrap_or_default().trim();
            let field = expression
                .trim()
                .strip_prefix("[=")
                .and_then(|value| value.strip_suffix(']'))
                .filter(|token| {
                    !token.contains(['[', ']', '|', '"', ':'])
                        && !token.chars().any(char::is_control)
                })
                .and_then(|token| {
                    tc_field(token).or_else(|| {
                        let (plugin, _) = token.split_once('.')?;
                        (!plugin.eq_ignore_ascii_case("tc"))
                            .then(|| resolve_wdx(token))
                            .flatten()
                            .map(Field::Wdx)
                    })
                })
                .or_else(|| {
                    TcColumnExpression::parse(expression)
                        .ok()
                        .map(Field::TcExpression)
                });
            let Some(field) = field.filter(|_| columns.len() < MAX_COLUMNS) else {
                skipped += 1;
                sort_keys.push(None);
                continue;
            };
            let key = match field {
                Field::Extension => Some(SortKey::Extension),
                Field::Size => Some(SortKey::Size),
                Field::Modified => Some(SortKey::Modified),
                Field::Attributes => Some(SortKey::Attributes),
                Field::Comment => Some(SortKey::Comment),
                Field::TcExpression(ref expression) => expression.sort_key(),
                Field::Wfx(_) => None,
                Field::Wdx(_) => {
                    let key = SortKey::Wdx(wdx_index);
                    wdx_index += 1;
                    Some(key)
                }
            };
            let title = if heading.is_empty()
                || heading.contains('|')
                || heading.chars().any(char::is_control)
            {
                field.label()
            } else {
                heading.chars().take(80).collect()
            };
            let (width, right_aligned) = size(index + 2, 110);
            columns.push(Column {
                field,
                title,
                width,
                right_aligned,
            });
            sort_keys.push(key);
        }
        if columns.len() > base_columns {
            result.insert(
                id,
                TcColumnSet {
                    name,
                    columns,
                    sort_keys,
                },
            );
        }
    }
    (result, skipped)
}

fn tc_field(token: &str) -> Option<Field> {
    Some(match token.to_lowercase().as_str() {
        "tc.ext" => Field::Extension,
        "tc.size" | "tc.größe" => Field::Size,
        "tc.writedate" | "tc.writetime" | "tc.writedatetime" | "tc.änderungsdatum" => {
            Field::Modified
        }
        "tc.attributes" | "tc.attribute" => Field::Attributes,
        "tc.comment" | "tc.kommentar" => Field::Comment,
        _ => return None,
    })
}

pub fn parse_wfx_default_view(
    view: &crate::plugin::WfxDefaultView,
    fields: &[crate::plugin::WfxField],
) -> Option<Vec<Column>> {
    let widths: Vec<_> = view
        .widths
        .split(',')
        .map(|part| part.trim().parse::<i32>().ok())
        .collect();
    let width = |index: usize| widths.get(index).copied().flatten();
    let mut headings = view.headers.split(r"\n");
    let mut columns = Vec::new();
    let extension = width(1).unwrap_or(60);
    if extension != 0 {
        columns.push(Column {
            field: Field::Extension,
            title: "Erw.".into(),
            width: extension.unsigned_abs().clamp(40, 600) as u16,
            right_aligned: Some(extension < 0),
        });
    }
    let base_columns = columns.len();
    for (index, expression) in view.contents.split(r"\n").enumerate() {
        if columns.len() >= MAX_COLUMNS {
            break;
        }
        let heading = headings.next().unwrap_or_default().trim();
        let expression = expression.trim();
        let Some(field) = wfx_view_field(expression, fields) else {
            continue;
        };
        let title = if heading.is_empty() || heading.chars().any(char::is_control) {
            field.label()
        } else {
            heading.chars().take(80).collect()
        };
        let width = width(index + 2).unwrap_or(110);
        if width != 0 {
            columns.push(Column {
                field,
                title,
                width: width.unsigned_abs().clamp(40, 600) as u16,
                right_aligned: Some(width < 0),
            });
        }
    }
    if columns.is_empty() || (columns.len() == base_columns && !view.contents.trim().is_empty()) {
        return None;
    }
    Some(columns)
}

fn wfx_view_field(expression: &str, fields: &[crate::plugin::WfxField]) -> Option<Field> {
    if let Some(token) = expression
        .strip_prefix("[=")
        .and_then(|text| text.strip_suffix(']'))
    {
        if let Some(field) = tc_field(token) {
            return Some(field);
        }
        if let Some((plugin, selection)) = token.split_once('.')
            && (plugin.eq_ignore_ascii_case("<fs>") || plugin.eq_ignore_ascii_case("fs"))
        {
            let (name, unit) = selection
                .split_once('.')
                .map_or((selection, None), |(name, unit)| (name, Some(unit)));
            return fields
                .iter()
                .find(|field| {
                    field.selection.field.eq_ignore_ascii_case(name)
                        && unit.is_none_or(|unit| {
                            field
                                .selection
                                .unit
                                .as_ref()
                                .is_some_and(|candidate| candidate.eq_ignore_ascii_case(unit))
                        })
                })
                .cloned()
                .map(Field::Wfx);
        }
    }
    expression
        .contains("[=")
        .then(|| {
            TcColumnExpression::parse(expression)
                .ok()
                .map(Field::TcExpression)
        })
        .flatten()
}

fn parse_mode(options: &str, column_sets: &BTreeMap<u16, TcColumnSet>) -> Option<(Mode, bool)> {
    let mut fields = options.split('|');
    let view_id = fields.next()?;
    let inherit_view = matches!(view_id, "" | "-1");
    let set = view_id
        .parse::<u16>()
        .ok()
        .and_then(|id| id.checked_sub(5))
        .and_then(|id| column_sets.get(&id));
    let view = if inherit_view || set.is_some() {
        TabView::Details
    } else {
        TabView::from_tc_id(view_id)?
    };
    let primary = fields.next().unwrap_or_default();
    let descending = fields.next().unwrap_or_default() == "1";
    let mut sort = Vec::new();
    let key = |index| match index {
        100.. => set.and_then(|set| set.sort_keys.get(index as usize - 100).copied().flatten()),
        _ => profile::tc_standard_sort(index),
    };
    let mut partial = false;
    if !matches!(primary, "" | "-1") {
        if let Some(key) = primary.parse().ok().and_then(key) {
            sort.push(SortOrder { key, descending });
        } else {
            partial = true;
        }
    }
    for order in fields
        .next()
        .unwrap_or_default()
        .split(',')
        .filter(|order| !order.is_empty())
    {
        if let Some((key, descending)) = order.parse::<i32>().ok().and_then(|value| {
            let index = value.checked_abs()?.checked_sub(1)?;
            let key = match set {
                Some(set) if (2..100).contains(&index) => {
                    set.sort_keys.get(index as usize - 2).copied().flatten()
                }
                _ => key(index),
            }?;
            Some((key, value < 0))
        }) {
            if !sort.iter().any(|order| order.key == key) {
                sort.push(SortOrder { key, descending });
            }
        } else {
            partial = true;
        }
    }
    let (colors, invalid_colors) = ModeColors::from_tc(fields);
    partial |= invalid_colors;
    Some((
        Mode {
            view,
            inherit_view,
            sort,
            colors,
            columns: set.map(|set| set.name.clone()),
        },
        partial,
    ))
}

fn expand_expression(
    expression: &str,
    resolve: &impl Fn(&str) -> Option<OsString>,
) -> Option<String> {
    let mut result = String::new();
    for (i, group) in expression.split("||").enumerate() {
        if i != 0 {
            result.push_str("||");
        }
        for (j, term) in group.split("|&").enumerate() {
            if j != 0 {
                result.push_str("|&");
            }
            let term = term.trim();
            let split = term.starts_with('%') as usize;
            result.push_str(&term[..split]);
            let body = &term[split..];
            if body.contains('%') {
                result.push_str(&profile::expand_tc_path(body, resolve)?.into_string().ok()?);
            } else {
                result.push_str(body);
            }
        }
    }
    Some(result)
}

pub(super) struct Expression(Vec<Vec<Term>>);

enum Term {
    Scope(Scope),
    DriveTypes(u8),
    Match {
        kind: u8,
        folder: Option<GlobMatcher>,
        files: Option<FileMasks>,
    },
}

pub(super) fn parse_expression(value: &str) -> io::Result<Expression> {
    if value.len() > 4096 || value.chars().any(char::is_control) {
        return Err(invalid());
    }
    let mut groups = Vec::new();
    let mut terms = 0;
    for alternative in value.split("||") {
        let mut group = Vec::new();
        for term in alternative.split("|&") {
            terms += 1;
            if terms > 24 {
                return Err(invalid());
            }
            group.push(parse_term(term.trim())?);
        }
        groups.push(group);
    }
    Ok(Expression(groups))
}

fn parse_term(value: &str) -> io::Result<Term> {
    if let Some(types) = value.strip_prefix('L').filter(|value| !value.is_empty()) {
        let mut mask = 0;
        for kind in types.bytes() {
            if !(b'1'..=b'6').contains(&kind) {
                return Err(invalid());
            }
            mask |= 1 << (kind - b'0');
        }
        return Ok(Term::DriveTypes(mask));
    }
    let scope = match value {
        "D" => Scope::Local,
        "L" => Scope::Drive,
        "U" => Scope::Unc,
        "V" => Scope::Shell,
        "F" => Scope::Ftp,
        "A" => Scope::Archive,
        "P" => Scope::Wfx,
        "S" => Scope::Search,
        _ => {
            let kind = *value
                .as_bytes()
                .first()
                .filter(|kind| b"+-%?".contains(kind))
                .ok_or_else(invalid)?;
            let condition = &value[1..];
            let (folder, files) = match condition.rsplit_once('\\') {
                Some((_, "*")) if matches!(kind, b'+' | b'-') => (condition, ""),
                Some((path, masks)) => (&condition[..path.len() + 1], masks),
                None => (condition, ""),
            };
            let recursive =
                folder.ends_with("\\*") || (!files.is_empty() && folder.ends_with("\\*\\"));
            let folder = folder.replace('\\', "/");
            let folder = folder.trim_end_matches('/');
            let folder = if matches!(folder, "" | "*" | "*.*") {
                None
            } else {
                let pattern = if recursive && let Some(prefix) = folder.strip_suffix("/*") {
                    format!("{prefix}/**")
                } else {
                    format!("{folder}/")
                };
                Some(
                    GlobBuilder::new(&pattern)
                        .literal_separator(true)
                        .case_insensitive(true)
                        .backslash_escape(false)
                        .build()
                        .map_err(io::Error::other)?
                        .compile_matcher(),
                )
            };
            let files = (!files.trim().is_empty())
                .then(|| FileMasks::from_tc(files))
                .transpose()?;
            if files.is_none() && matches!(kind, b'%' | b'?') {
                return Err(invalid());
            }
            return Ok(Term::Match {
                kind,
                folder,
                files,
            });
        }
    };
    Ok(Term::Scope(scope))
}

fn invalid() -> io::Error {
    io::Error::other("Ungültige oder noch nicht unterstützte TC-Ansichtsregel.")
}

impl Expression {
    pub(super) fn matches(&self, scope: Scope, path: &str, entries: &[Entry]) -> bool {
        self.matches_with_drive(scope, path, entries, super::request_tc_drive_type)
    }

    fn matches_with_drive(
        &self,
        scope: Scope,
        path: &str,
        entries: &[Entry],
        drive_type: impl Fn(&str) -> Option<u8>,
    ) -> bool {
        let normalized = path.replace('\\', "/");
        let folder_path = format!("{}/", normalized.trim_end_matches('/'));
        let drive = std::cell::OnceCell::new();
        self.0.iter().any(|group| {
            group.iter().all(|term| match term {
                Term::Scope(Scope::Local) => matches!(scope, Scope::Local | Scope::Unc),
                Term::Scope(Scope::Drive) => {
                    scope == Scope::Local && normalized.as_bytes().get(1) == Some(&b':')
                }
                Term::Scope(required) => scope == *required,
                Term::DriveTypes(mask) => {
                    scope == Scope::Local
                        && drive
                            .get_or_init(|| drive_type(&normalized))
                            .is_some_and(|kind| mask & (1 << kind) != 0)
                }
                Term::Match {
                    kind,
                    folder,
                    files,
                } => {
                    let path_matches = folder
                        .as_ref()
                        .is_none_or(|mask| mask.is_match(&folder_path));
                    if let Some(files) = files {
                        if !path_matches {
                            return false;
                        }
                        let (total, count) = entries
                            .iter()
                            .filter(|entry| entry.kind == Kind::File)
                            .fold((0usize, 0usize), |(total, count), entry| {
                                (
                                    total + 1,
                                    count + usize::from(files.includes(Path::new(&entry.name))),
                                )
                            });
                        match kind {
                            b'+' => total != 0 && count == total,
                            b'-' => count == 0,
                            b'%' => total != 0 && count >= total.div_ceil(2),
                            _ => count != 0,
                        }
                    } else {
                        path_matches != (*kind == b'-')
                    }
                }
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, path::PathBuf};

    #[test]
    fn applies_usb_and_external_disk_rules_only_to_lettered_local_locations() {
        let rule = parse_expression(r"L12|&+X:\Fotos\*").unwrap();
        assert!(rule.matches_with_drive(Scope::Local, r"X:\Fotos\Aktuell", &[], |_| Some(2)));
        assert!(rule.matches_with_drive(Scope::Local, r"X:\Fotos", &[], |_| Some(1)));
        assert!(!rule.matches_with_drive(Scope::Local, r"X:\Fotos", &[], |_| Some(3)));
        assert!(!rule.matches_with_drive(Scope::Local, r"X:\Dokumente", &[], |_| Some(2)));
        assert!(
            !rule.matches_with_drive(Scope::Unc, r"\\host\Fotos", &[], |_| panic!(
                "UNC drive probe"
            ))
        );
        assert!(parse_expression("L0").is_err());
        assert!(parse_expression("L7").is_err());
    }

    #[test]
    fn imports_tc_columns_and_preserves_sort_positions_when_a_field_is_missing() {
        let file = tempfile::NamedTempFile::new().unwrap();
        fs::write(
            file.path(),
            concat!(
                "[CustomFields]\nTitles=Media\n",
                "Headers1=Anmerkung\\nFehlt\\nAbmessung\n",
                "Contents1=[=tc.comment]\\n[=missing.field]\\n[=image.Width.Pixel]\n",
                "Widths1=300,80,150,180,-240\nOptions1=-1|0|144\n",
                "[ViewModes]\n1_name=Medien\n1_options=6|102|1|3,-4,1\n",
            ),
        )
        .unwrap();
        let imported = read_tc_views(
            file.path(),
            |_| None,
            |token| {
                (token == "image.Width.Pixel").then(|| WdxColumn {
                    plugin: PathBuf::from("image.wdx64"),
                    selection: crate::plugin::WdxField {
                        field: "Width".into(),
                        unit: Some("Pixel".into()),
                    },
                })
            },
        )
        .unwrap();
        let columns = &imported.columns["Media"];
        assert_eq!(
            columns
                .iter()
                .map(|col| col.title.as_str())
                .collect::<Vec<_>>(),
            ["Erw.", "Anmerkung", "Abmessung"]
        );
        assert_eq!(columns[2].width, 160);
        assert_eq!(columns[2].right_aligned, Some(true));
        assert_eq!(
            imported.column_layouts[&1].sort_keys,
            [Some(SortKey::Comment), None, Some(SortKey::Wdx(0))]
        );
        let mode = &imported.modes["Medien"];
        assert_eq!(mode.columns.as_deref(), Some("Media"));
        assert_eq!(
            mode.sort,
            [
                SortOrder {
                    key: SortKey::Wdx(0),
                    descending: true
                },
                SortOrder {
                    key: SortKey::Comment,
                    descending: false
                },
                SortOrder {
                    key: SortKey::Name,
                    descending: false
                },
            ]
        );
        assert_eq!(imported.skipped, 2);
    }

    #[test]
    fn preserves_the_base_view_and_loads_tc_colorrefs() {
        let sets = BTreeMap::new();
        let (mode, partial) = parse_mode("-1|-1|0||255|65280|16711680|0|-1", &sets).unwrap();
        assert!(!partial);
        assert!(mode.inherit_view);
        assert_eq!(
            parse_mode("4|-1|0", &sets).unwrap().0.view,
            TabView::Comments
        );
        assert_eq!(
            parse_mode("5|-1|0", &sets).unwrap().0.view,
            TabView::Thumbnails
        );
        assert_eq!(
            mode.colors,
            Some(ModeColors {
                tab: Some(0xff0000),
                light: [Some(0x00ff00), Some(0x0000ff)],
                dark: [Some(0), None],
            })
        );
    }

    #[test]
    fn imports_tc_modes_and_rules_in_ini_order_with_and_precedence() {
        let file = tempfile::NamedTempFile::new().unwrap();
        fs::write(file.path(), concat!(
            "[ViewModes]\n",
            "0_name=Standard\n0_options=1|-1|0||-1|-1|-1\n",
            "1_name=Fotos\n1_options=0|2|1|1,-4|-1|-1|-1\n",
            "[ViewModeSwitch]\nEnabled=1\n",
            "4_rules=L\n4_mode=0\n",
            "2_rules=U|&+\\\\server\\Fotos\\*||L|&+%BASE%\\Fotos\\*|&%*.*\\*.jpg *.png | alt*\n2_mode=1\n",
        )).unwrap();
        let imported = read_tc_views(
            file.path(),
            |name| (name == "BASE").then(|| "C:".into()),
            |_| None,
        )
        .unwrap();
        assert_eq!(imported.skipped, 0);
        assert_eq!(imported.automatic, Some(true));
        assert_eq!(imported.rules[0].mode, "Fotos");
        assert_eq!(imported.rules[1].mode, "Standard");
        let short = &imported.modes["Fotos"];
        assert_eq!(short.view, TabView::Short);
        assert_eq!(
            short
                .sort
                .iter()
                .map(|order| (order.key, order.descending))
                .collect::<Vec<_>>(),
            [
                (SortKey::Size, true),
                (SortKey::Name, false),
                (SortKey::Modified, true)
            ]
        );
        let rule = &imported.rules[0];
        let file = |name| Entry::from_remote(PathBuf::from(name), Kind::File, None, None);
        let photos = [file("bild.png"), file("datei.txt")];
        assert!(rule.matches(Scope::Local, r"c:\Fotos", &photos).unwrap());
        assert!(
            rule.matches(Scope::Local, r"C:\Fotos\2026\Sommer", &photos)
                .unwrap()
        );
        assert!(rule.matches(Scope::Unc, r"\\server\Fotos", &[]).unwrap());
        assert!(!rule.matches(Scope::Local, r"C:\FotosAlt", &photos).unwrap());
        assert!(
            !rule
                .matches(
                    Scope::Local,
                    r"C:\Fotos",
                    &[file("alt.jpg"), file("datei.txt")]
                )
                .unwrap()
        );
        assert!(!rule.matches(Scope::Local, r"C:\Fotos", &[]).unwrap());
    }
}
