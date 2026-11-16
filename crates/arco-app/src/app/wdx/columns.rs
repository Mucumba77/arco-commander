use super::*;
use arco_core::view_settings::TcColumnExpression;
use std::collections::BTreeMap;
#[derive(Clone)]
pub(in crate::app) struct ValueColumn {
    pub selection: WdxField,
    pub descending: bool,
    source: Source,
}
#[derive(Clone)]
enum Source {
    Wdx(Registration, WdxField),
    Wfx(WdxField),
    Expression(TcColumnExpression, Vec<(String, Source)>),
    Native(String),
    Missing(String),
}
impl App {
    pub(in crate::app) fn value_columns(&self, side: usize) -> Vec<(usize, ValueColumn)> {
        let local = self.panels[side].current().is_local();
        let wfx = self.panels[side].current().wfx.is_some();
        let resolve = |token: &str| {
            if token
                .split_once('.')
                .is_some_and(|(prefix, _)| prefix.eq_ignore_ascii_case("tc"))
            {
                Source::Native(token.into())
            } else if !local {
                match token.split_once('.') {
                    Some((name, field))
                        if wfx
                            && (name.eq_ignore_ascii_case("<fs>")
                                || name.eq_ignore_ascii_case("fs")) =>
                    {
                        let (field, unit) = field
                            .split_once('.')
                            .map_or((field, None), |(field, unit)| (field, Some(unit.into())));
                        Source::Wfx(WdxField {
                            field: field.into(),
                            unit,
                        })
                    }
                    _ => Source::Missing(format!(
                        "Dieses Feld ist im virtuellen Dateisystem nicht verfügbar: {token}"
                    )),
                }
            } else {
                match self.resolve_wdx_token(token) {
                    Ok((plugin, field)) => Source::Wdx(plugin, field),
                    Err(error) => Source::Missing(error.to_string()),
                }
            }
        };
        if let Some(columns) = self.active_columns(side) {
            columns
                .iter()
                .filter(|column| column.field.requires_values())
                .enumerate()
                .map(|(index, column)| {
                    let (selection, descending, source) = match &column.field {
                        Field::Wdx(column) => (
                            column.selection.clone(),
                            false,
                            self.wdx_plugin(column).map_or_else(
                                || {
                                    Source::Missing(format!(
                                        "Das WDX-Plugin fehlt oder ist deaktiviert: {}",
                                        column.plugin.display()
                                    ))
                                },
                                |plugin| Source::Wdx(plugin.clone(), column.selection.clone()),
                            ),
                        ),
                        Field::Wfx(field) => (
                            field.selection.clone(),
                            field.descending,
                            Source::Wfx(field.selection.clone()),
                        ),
                        Field::TcExpression(expression) => (
                            WdxField {
                                field: column.title.clone(),
                                unit: None,
                            },
                            false,
                            Source::Expression(
                                expression.clone(),
                                expression
                                    .references()
                                    .into_iter()
                                    .map(|token| (token.into(), resolve(token)))
                                    .collect(),
                            ),
                        ),
                        _ => unreachable!("requires_values"),
                    };
                    (
                        index,
                        ValueColumn {
                            selection,
                            descending,
                            source,
                        },
                    )
                })
                .collect()
        } else if local {
            self.profile
                .wdx_columns
                .iter()
                .filter_map(|column| {
                    self.wdx_plugin(column).map(|plugin| ValueColumn {
                        selection: column.selection.clone(),
                        descending: false,
                        source: Source::Wdx(plugin.clone(), column.selection.clone()),
                    })
                })
                .take(DEFAULT_COLUMNS)
                .enumerate()
                .collect()
        } else {
            Vec::new()
        }
    }
}
/// All file/SDK access is confined to workers. One query is shared by all columns
/// within a bounded batch; virtual paths are never sent to a local file reader.
pub(super) fn read_columns(
    columns: &[(usize, ValueColumn)],
    entries: &[Entry],
    location: Option<&crate::panel::WfxLocation>,
    local: bool,
    cancel: &AtomicBool,
    mut prompts: Option<&mut plugin::PromptHandler<'_>>,
) -> io::Result<HashMap<usize, Vec<Option<WdxValue>>>> {
    let mut cache = HashMap::new();
    if let Some(location) = location {
        preload_wfx(columns, entries, location, cancel, &mut prompts, &mut cache)?;
    }
    let mut values = HashMap::new();
    for (index, column) in columns {
        files::check_cancel(cancel)?;
        values.insert(
            *index,
            read_source(
                &column.source,
                entries,
                location,
                local,
                cancel,
                prompts
                    .as_mut()
                    .map(|p| &mut **p as &mut plugin::PromptHandler),
                &mut cache,
            )?,
        );
    }
    Ok(values)
}

pub(super) fn read_display_columns(
    columns: &[(usize, ValueColumn)],
    entries: &[Entry],
    location: Option<&crate::panel::WfxLocation>,
    local: bool,
    cancel: &AtomicBool,
) -> (HashMap<usize, Vec<Option<WdxValue>>>, Option<String>) {
    let mut cache = HashMap::new();
    let mut error = None;
    if let Some(location) = location
        && let Err(reason) = preload_wfx(columns, entries, location, cancel, &mut None, &mut cache)
    {
        error = Some(reason.to_string());
        cache.clear();
    }
    let mut result = HashMap::new();
    for (index, column) in columns {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        match read_source(
            &column.source,
            entries,
            location,
            local,
            cancel,
            None,
            &mut cache,
        ) {
            Ok(values) => {
                result.insert(*index, values);
            }
            Err(reason) => {
                error = Some(reason.to_string());
            }
        }
    }
    (result, error)
}

fn preload_wfx(
    columns: &[(usize, ValueColumn)],
    entries: &[Entry],
    location: &crate::panel::WfxLocation,
    cancel: &AtomicBool,
    prompts: &mut Option<&mut plugin::PromptHandler<'_>>,
    cache: &mut HashMap<String, Vec<Option<WdxValue>>>,
) -> io::Result<()> {
    fn collect(source: &Source, fields: &mut Vec<WdxField>) {
        match source {
            Source::Wfx(field) if !fields.contains(field) => fields.push(field.clone()),
            Source::Expression(_, bindings) => {
                for (_, source) in bindings {
                    collect(source, fields);
                }
            }
            _ => (),
        }
    }
    let mut fields = Vec::new();
    for (_, column) in columns {
        collect(&column.source, &mut fields);
    }
    if fields.is_empty() {
        return Ok(());
    }
    if fields.len() > 128 {
        return Err(io::Error::other(
            "Eine Ansicht darf höchstens 128 verschiedene Inhaltsfelder abfragen.",
        ));
    }
    let mut values = vec![vec![None; entries.len()]; fields.len()];
    for (batch, chunk) in entries.chunks(plugin::WFX_CONTENT_BATCH).enumerate() {
        files::check_cancel(cancel)?;
        let names = chunk
            .iter()
            .map(|entry| wfx_name(&entry.path))
            .collect::<io::Result<Vec<_>>>()?;
        let rows = plugin::wfx_content_values(
            &location.plugin.path,
            &location.directory,
            names,
            fields.clone(),
            cancel,
            prompts
                .as_mut()
                .map(|prompt| &mut **prompt as &mut plugin::PromptHandler),
        )?;
        for (row, content) in rows.into_iter().enumerate() {
            for (column, value) in content.into_iter().enumerate() {
                values[column][batch * plugin::WFX_CONTENT_BATCH + row] = value;
            }
        }
    }
    for (field, values) in fields.into_iter().zip(values) {
        cache.insert(format!("wfx:{:?}:{:?}", field.field, field.unit), values);
    }
    check_cache(cache)
}

fn check_cache(cache: &HashMap<String, Vec<Option<WdxValue>>>) -> io::Result<()> {
    let mut bytes = 0usize;
    for value in cache.values().flatten().flatten() {
        let key_bytes = match &value.key {
            plugin::WdxKey::Text(text) => text.len(),
            _ => 16,
        };
        bytes = bytes
            .saturating_add(value.display.len())
            .saturating_add(key_bytes);
        if value.display.len() > 65536 || bytes > 16 * 1024 * 1024 {
            return Err(io::Error::other(
                "Die Inhaltsfelder überschreiten das Leselimit (64 KiB je Feld, 16 MiB je Abfrage).",
            ));
        }
    }
    Ok(())
}
fn read_source(
    source: &Source,
    entries: &[Entry],
    location: Option<&crate::panel::WfxLocation>,
    local: bool,
    cancel: &AtomicBool,
    mut prompts: Option<&mut plugin::PromptHandler<'_>>,
    cache: &mut HashMap<String, Vec<Option<WdxValue>>>,
) -> io::Result<Vec<Option<WdxValue>>> {
    let key = match source {
        Source::Native(token) => format!("tc:{token}"),
        Source::Wdx(plugin, field) => {
            format!("wdx:{:?}:{:?}:{:?}", plugin.path, field.field, field.unit)
        }
        Source::Wfx(field) => format!("wfx:{:?}:{:?}", field.field, field.unit),
        Source::Missing(reason) => return Err(io::Error::other(reason.clone())),
        Source::Expression(expression, bindings) => {
            let mut rows = vec![BTreeMap::new(); entries.len()];
            for (token, source) in bindings {
                for (row, value) in rows.iter_mut().zip(read_source(
                    source,
                    entries,
                    location,
                    local,
                    cancel,
                    prompts
                        .as_mut()
                        .map(|p| &mut **p as &mut plugin::PromptHandler),
                    cache,
                )?) {
                    if let Some(value) = value {
                        row.insert(token.clone(), value);
                    }
                }
            }
            let result: Vec<Option<WdxValue>> = entries
                .iter()
                .zip(rows)
                .map(|(entry, row)| expression.render_value(entry, local, &row))
                .collect::<io::Result<_>>()?;
            let bytes: usize = result
                .iter()
                .flatten()
                .map(|value| value.display.len())
                .sum();
            if bytes > 16 * 1024 * 1024
                || result
                    .iter()
                    .flatten()
                    .any(|value| value.display.len() > 65536)
            {
                return Err(io::Error::other(
                    "Der zusammengesetzte Spaltentext überschreitet das Leselimit.",
                ));
            }
            return Ok(result);
        }
    };
    if let Some(values) = cache.get(&key) {
        return Ok(values.clone());
    }
    if cache.len() >= 128 {
        return Err(io::Error::other(
            "Eine Ansicht darf höchstens 128 verschiedene Inhaltsfelder abfragen.",
        ));
    }
    let mut values = vec![None; entries.len()];
    match source {
        Source::Native(token) => {
            for (entry, value) in entries.iter().zip(&mut values) {
                files::check_cancel(cancel)?;
                *value = if !local {
                    plugin::tc_entry_field(token, entry)?
                } else {
                    let metadata = std::fs::symlink_metadata(&entry.path)?;
                    plugin::tc_field_with_cancel(token, &entry.path, &metadata, cancel)?
                };
            }
        }
        Source::Wdx(plugin, field) if local => {
            let targets: Vec<_> = entries
                .iter()
                .enumerate()
                .filter(|(_, entry)| plugin.matches(Interface::Wdx, &entry.path))
                .collect();
            let paths = targets
                .iter()
                .map(|(_, entry)| entry.path.clone())
                .collect();
            for (index, value) in
                plugin::wdx_typed_field_values(&plugin.path, field.clone(), paths, cancel)?
            {
                if let Some((target, _)) = targets.get(index) {
                    values[*target] = Some(value);
                }
            }
        }
        Source::Wfx(field) if location.is_some() => {
            let location = location.unwrap();
            for (batch, chunk) in entries.chunks(plugin::WFX_CONTENT_BATCH).enumerate() {
                files::check_cancel(cancel)?;
                let names = chunk
                    .iter()
                    .map(|entry| wfx_name(&entry.path))
                    .collect::<io::Result<Vec<_>>>()?;
                for (index, row) in plugin::wfx_content_values(
                    &location.plugin.path,
                    &location.directory,
                    names,
                    vec![field.clone()],
                    cancel,
                    prompts
                        .as_mut()
                        .map(|p| &mut **p as &mut plugin::PromptHandler),
                )?
                .into_iter()
                .enumerate()
                {
                    values[batch * plugin::WFX_CONTENT_BATCH + index] =
                        row.into_iter().next().flatten();
                }
            }
        }
        _ => {
            return Err(io::Error::other(
                "Dieses Inhaltsfeld ist in diesem Dateifenster nicht verfügbar.",
            ));
        }
    }
    if values
        .iter()
        .flatten()
        .any(|value| value.display.len() > 65536)
    {
        return Err(io::Error::other("Ein Inhaltsfeld überschreitet 64 KiB."));
    }
    cache.insert(key.clone(), values.clone());
    if let Err(error) = check_cache(cache) {
        cache.remove(&key);
        return Err(error);
    }
    Ok(values)
}

#[cfg(test)]
mod tests {
    use super::*;
    use arco_core::files::Kind;

    fn expression(source: &str, bindings: Vec<(String, Source)>) -> ValueColumn {
        ValueColumn {
            selection: WdxField {
                field: "Test".into(),
                unit: None,
            },
            descending: false,
            source: Source::Expression(TcColumnExpression::parse(source).unwrap(), bindings),
        }
    }

    #[test]
    fn background_columns_combine_local_metadata_preserve_numeric_keys_and_isolate_missing_plugins()
    {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("Résumé.txt");
        std::fs::write(&path, "example").unwrap();
        let entry = Entry::read(path.clone(), path.file_name().unwrap().into()).unwrap();
        let columns = vec![
            (
                0,
                expression(
                    "[=tc.fullname] ([=tc.path length])",
                    vec![(
                        "tc.path length".into(),
                        Source::Native("tc.path length".into()),
                    )],
                ),
            ),
            (
                1,
                expression(
                    "[=tc.path length]",
                    vec![(
                        "tc.path length".into(),
                        Source::Native("tc.path length".into()),
                    )],
                ),
            ),
            (
                2,
                ValueColumn {
                    selection: WdxField {
                        field: "Missing".into(),
                        unit: None,
                    },
                    descending: false,
                    source: Source::Missing("Plugin disabled".into()),
                },
            ),
        ];
        let cancel = AtomicBool::new(false);
        let (mut values, error) = read_display_columns(&columns, &[entry], None, true, &cancel);
        assert_eq!(error.as_deref(), Some("Plugin disabled"));
        let name = values.remove(&0).unwrap().remove(0).unwrap().display;
        assert!(name.starts_with("Résumé.txt ("));
        let number = values.remove(&1).unwrap().remove(0).unwrap();
        assert!(matches!(number.key, plugin::WdxKey::Integer(value) if value > 0));
        assert!(!values.contains_key(&2));
        cancel.store(true, Ordering::Relaxed);
        assert_eq!(
            read_columns(&columns, &[], None, true, &cancel, None)
                .unwrap_err()
                .kind(),
            io::ErrorKind::Interrupted
        );
    }

    #[test]
    fn virtual_columns_never_open_paths_as_local_files() {
        let entry = Entry::from_remote(
            "unavailable-provider:/outside/virtual.txt".into(),
            Kind::File,
            Some(2048),
            None,
        );
        let column = expression(
            "[=tc.fullname] [=tc.creationdate.Y-M-D] [=tc.size.kbytes._d1]",
            vec![
                (
                    "tc.creationdate".into(),
                    Source::Native("tc.creationdate".into()),
                ),
                (
                    "tc.size.kbytes".into(),
                    Source::Native("tc.size.kbytes".into()),
                ),
            ],
        );
        let (values, error) = read_display_columns(
            &[(0, column)],
            &[entry],
            None,
            false,
            &AtomicBool::new(false),
        );
        assert!(error.is_none());
        assert_eq!(values[&0][0].as_ref().unwrap().display, "virtual.txt  2");
    }
    #[test]
    #[ignore = "requires WFX column fixtures for x86/x64 and ARCO_TEST_WDX_COLUMNS"]
    fn composite_sdk_columns_roundtrip_ansi_unicode_typed_keys_and_local_wdx() {
        for (variable, name, label) in [
            ("ARCO_TEST_WFX_COLUMNS_X86", "plain file.txt", "ANSI label"),
            ("ARCO_TEST_WFX_COLUMNS_X64", "漢 file.txt", "Unicode 漢"),
        ] {
            let path = std::path::PathBuf::from(std::env::var_os(variable).expect(variable));
            let mut registration = Registration::read(&path, "Fixture", "*.*").unwrap();
            registration.enabled = true;
            let location = crate::panel::WfxLocation {
                plugin: std::sync::Arc::new(registration),
                directory: "\\Shelf".into(),
            };
            let amount = WdxField {
                field: "Amount".into(),
                unit: Some("KiB".into()),
            };
            let text = WdxField {
                field: "Label".into(),
                unit: None,
            };
            let columns = vec![
                (
                    0,
                    expression(
                        "[=tc.fullname]: [=<fs>.Amount.KiB] / [=fs.Label]",
                        vec![
                            ("<fs>.Amount.KiB".into(), Source::Wfx(amount.clone())),
                            ("fs.Label".into(), Source::Wfx(text)),
                        ],
                    ),
                ),
                (
                    1,
                    ValueColumn {
                        selection: amount.clone(),
                        descending: true,
                        source: Source::Wfx(amount),
                    },
                ),
            ];
            let entry = Entry::from_remote(
                format!("\\Shelf\\{name}").into(),
                Kind::File,
                Some(3072),
                None,
            );
            let (values, error) = read_display_columns(
                &columns,
                &[entry],
                Some(&location),
                false,
                &AtomicBool::new(false),
            );
            assert!(error.is_none(), "{error:?}");
            assert_eq!(
                values[&0][0].as_ref().unwrap().display,
                format!("{name}: 3 / {label}")
            );
            assert_eq!(
                values[&1][0].as_ref().unwrap().key,
                plugin::WdxKey::Integer(3)
            );
        }
        let path = std::path::PathBuf::from(
            std::env::var_os("ARCO_TEST_WDX_COLUMNS").expect("WDX fixture"),
        );
        let mut registration = Registration::read(&path, "Score", "*.*").unwrap();
        registration.enabled = true;
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("Example.txt");
        std::fs::write(&path, "example").unwrap();
        let entry = Entry::read(path.clone(), path.file_name().unwrap().into()).unwrap();
        let column = expression(
            "[=tc.fullname] = [=Score.Score.scaled]",
            vec![(
                "Score.Score.scaled".into(),
                Source::Wdx(
                    registration,
                    WdxField {
                        field: "Score".into(),
                        unit: Some("scaled".into()),
                    },
                ),
            )],
        );
        let (values, error) = read_display_columns(
            &[(0, column)],
            &[entry],
            None,
            true,
            &AtomicBool::new(false),
        );
        assert!(error.is_none(), "{error:?}");
        assert_eq!(values[&0][0].as_ref().unwrap().display, "Example.txt = 41");
    }
}
