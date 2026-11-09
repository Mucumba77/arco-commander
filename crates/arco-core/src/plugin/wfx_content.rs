use super::{
    Interface, Operation, PromptHandler, Row, WdxField, WdxValue, inspect_host, inspect_interactive,
};
use crate::files::leaf_name;
use serde::{Deserialize, Serialize};
use std::ffi::OsStr;
use std::io;
use std::path::Path;
use std::sync::atomic::AtomicBool;

pub const WFX_CONTENT_BATCH: usize = 24;
pub const WFX_CONTENT_COLUMNS: usize = 29;

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct WfxField {
    #[serde(flatten)]
    pub selection: WdxField,
    #[serde(default)]
    pub descending: bool,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct WfxDefaultView {
    pub contents: String,
    pub headers: String,
    pub widths: String,
    pub options: String,
}

pub fn wfx_default_view(
    plugin: &Path,
    cancel: &AtomicBool,
) -> io::Result<Option<(WfxDefaultView, Vec<WfxField>)>> {
    let rows = inspect_host(
        Interface::Wfx,
        plugin,
        Path::new(""),
        Operation::WfxDefaultView,
        cancel,
        |_| (),
    )?;
    let mut rows = rows.into_iter();
    let Some(first) = rows.next() else {
        return Ok(None);
    };
    let view = first
        .wfx_default_view
        .ok_or_else(|| invalid("Die WFX-Standardansicht fehlt in der Host-Antwort."))?;
    Ok(Some((view, field_selections(rows))))
}

pub fn wfx_content_fields(
    plugin: &Path,
    cancel: &AtomicBool,
    prompt: &mut PromptHandler<'_>,
) -> io::Result<Vec<WfxField>> {
    let rows = inspect_interactive(
        Interface::Wfx,
        plugin,
        Path::new(""),
        Operation::WfxContentFields,
        cancel,
        |_| (),
        Some(prompt),
    )?;
    Ok(field_selections(rows))
}

fn field_selections(rows: impl IntoIterator<Item = Row>) -> Vec<WfxField> {
    let valid_name = |text: &str| {
        !text.is_empty() && !text.contains(['.', '|', ':']) && !text.chars().any(char::is_control)
    };
    let mut fields = Vec::new();
    for row in rows {
        if !valid_name(&row.name) {
            continue;
        }
        let mut units = row
            .value
            .split('|')
            .filter(|unit| valid_name(unit))
            .peekable();
        if units.peek().is_none() {
            fields.push(WfxField {
                selection: WdxField {
                    field: row.name,
                    unit: None,
                },
                descending: row.wfx_descending,
            });
        } else {
            fields.extend(units.map(|unit| WfxField {
                selection: WdxField {
                    field: row.name.clone(),
                    unit: Some(unit.to_owned()),
                },
                descending: row.wfx_descending,
            }));
        }
    }
    fields
}

pub fn wfx_content_paths(
    directory: &str,
    names: &[String],
    fields: &[WdxField],
) -> io::Result<Vec<String>> {
    if fields.is_empty()
        || fields.len() > WFX_CONTENT_COLUMNS
        || names.is_empty()
        || names.len() > WFX_CONTENT_BATCH
    {
        return Err(invalid("Ungültige Anzahl von WFX-Feldern oder Einträgen."));
    }
    let suffix = directory
        .strip_prefix('\\')
        .ok_or_else(|| invalid("Ein WFX-Pfad beginnt mit einem Rückstrich."))?;
    if !suffix.is_empty() {
        for part in suffix.split('\\') {
            validate_leaf(part)?;
        }
    }
    names
        .iter()
        .map(|name| {
            validate_leaf(name)?;
            let path = format!("{}\\{name}", directory.trim_end_matches('\\'));
            if path.encode_utf16().count() > 1023 {
                return Err(invalid("Ein WFX-Pfad überschreitet das SDK-Limit."));
            }
            Ok(path)
        })
        .collect()
}

pub fn wfx_content_values(
    plugin: &Path,
    directory: &str,
    names: Vec<String>,
    fields: Vec<WdxField>,
    cancel: &AtomicBool,
    prompt: Option<&mut PromptHandler<'_>>,
) -> io::Result<Vec<Vec<Option<WdxValue>>>> {
    wfx_content_paths(directory, &names, &fields)?;
    let count = names.len();
    let width = fields.len();
    let rows = inspect_interactive(
        Interface::Wfx,
        plugin,
        Path::new(directory),
        Operation::WfxContentValues { fields, names },
        cancel,
        |_| (),
        prompt,
    )?;
    if rows.len() != count {
        return Err(invalid(
            "Der WFX-Host meldet eine unerwartete Anzahl an Spaltenzeilen.",
        ));
    }
    rows.into_iter()
        .enumerate()
        .map(|(index, row)| {
            if row.name.parse::<usize>().ok() != Some(index) || row.wfx_content.len() != width {
                return Err(invalid("Der WFX-Host meldet ungültige Spaltenzeilen."));
            }
            Ok(row.wfx_content)
        })
        .collect()
}

fn validate_leaf(name: &str) -> io::Result<()> {
    if name.contains(['\\', '/']) || name.chars().any(char::is_control) {
        return Err(invalid("Ungültiger WFX-Name."));
    }
    leaf_name(OsStr::new(name))?;
    Ok(())
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_only_leaf_names_in_the_requested_wfx_namespace() {
        let fields = [WdxField {
            field: "Test".into(),
            unit: None,
        }];
        assert_eq!(
            wfx_content_paths("\\Ordner", &["Ä 2".into()], &fields).unwrap(),
            ["\\Ordner\\Ä 2"]
        );
        assert_eq!(
            wfx_content_paths("\\", &["x".into()], &fields).unwrap(),
            ["\\x"]
        );
        assert!(wfx_content_paths("\\", &["..\\x".into()], &fields).is_err());
        assert!(wfx_content_paths("\\Ordner\\..", &["x".into()], &fields).is_err());
        assert!(wfx_content_paths("C:\\", &["x".into()], &fields).is_err());
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "requires ARCO_TEST_WFX_COLUMNS_X86 and ARCO_TEST_WFX_COLUMNS_X64 SDK fixtures"]
    fn wfx_columns_support_ansi_unicode_named_units_and_default_order() {
        let cancel = AtomicBool::new(false);
        for (env, name, label) in [
            ("ARCO_TEST_WFX_COLUMNS_X86", "plain file.txt", "ANSI label"),
            ("ARCO_TEST_WFX_COLUMNS_X64", "漢 file.txt", "Unicode 漢"),
        ] {
            let file = std::path::PathBuf::from(std::env::var_os(env).expect(env));
            let fields = wfx_content_fields(&file, &cancel, &mut |_| None).unwrap();
            assert_eq!(fields.len(), 4);
            assert_eq!(fields[1].selection.unit.as_deref(), Some("KiB"));
            assert!(fields[1].descending);
            assert!(!fields[2].descending);
            assert_eq!(fields[3].selection.unit, None);
            let rows = wfx_content_values(
                &file,
                "\\Shelf",
                vec![name.into(), "empty.txt".into()],
                fields[1..]
                    .iter()
                    .map(|field| field.selection.clone())
                    .collect(),
                &cancel,
                None,
            )
            .unwrap();
            let amount = rows[0][0].as_ref().unwrap();
            assert_eq!(amount.key, crate::plugin::WdxKey::Integer(3));
            assert_eq!(rows[0][1].as_ref().unwrap().display, label);
            assert_eq!(rows[0][2].as_ref().unwrap().display, "Two");
            assert!(rows[1].iter().all(Option::is_none));
        }
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "requires ARCO_TEST_WFX_COLUMNS_X86 and ARCO_TEST_WFX_COLUMNS_X64 SDK fixtures"]
    fn wfx_default_views_decode_ansi_and_unicode_layouts() {
        use crate::view_settings::{Field, parse_wfx_default_view};

        let cancel = AtomicBool::new(false);
        for (env, amount, label) in [
            ("ARCO_TEST_WFX_COLUMNS_X86", "Amount", "Label"),
            ("ARCO_TEST_WFX_COLUMNS_X64", "Größe", "漢"),
        ] {
            let plugin = std::path::PathBuf::from(std::env::var_os(env).expect(env));
            let (view, fields) = wfx_default_view(&plugin, &cancel).unwrap().unwrap();
            assert_eq!(view.options, "-1|1");
            let columns = parse_wfx_default_view(&view, &fields).unwrap();
            assert_eq!(columns.len(), 3);
            assert_eq!(columns[0].title, amount);
            assert_eq!(columns[0].width, 80);
            assert_eq!(columns[0].right_aligned, Some(true));
            assert_eq!(columns[0].field, Field::Wfx(fields[1].clone()));
            assert_eq!(columns[1].title, label);
            assert_eq!(columns[1].field, Field::Wfx(fields[2].clone()));
            assert_eq!(columns[2].field, Field::Modified);
        }
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "requires ARCO_TEST_WFX_NO_DEFAULT_VIEW SDK fixture"]
    fn wfx_default_view_missing_export_is_an_ordinary_fallback() {
        let env = "ARCO_TEST_WFX_NO_DEFAULT_VIEW";
        let plugin = std::path::PathBuf::from(std::env::var_os(env).expect(env));
        let cancel = AtomicBool::new(false);
        assert!(wfx_default_view(&plugin, &cancel).unwrap().is_none());
        assert!(
            !wfx_content_fields(&plugin, &cancel, &mut |_| None)
                .unwrap()
                .is_empty()
        );
    }
}
