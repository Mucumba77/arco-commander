use super::{App, JobContext, scan_paths};
use crate::app::duplicate_result;
use crate::commands::{Dialog, Pending};
use arco_core::ignore::IgnoreList;
use arco_core::plugin::{Registration, WdxField, WdxKey, WdxValue};
use arco_core::{files, rename, tools};
use std::collections::{BTreeMap, HashMap};
use std::fs::Metadata;
use std::io;
use std::path::{Path, PathBuf};

enum Field {
    Name,
    Extension,
    Size,
    WriteDate,
    WriteTime,
    CreationDate,
    AccessDate,
    Native(String),
    Plugin(Registration, WdxField),
}

struct Criterion {
    field: Field,
    range: Option<String>,
}

struct Candidate {
    path: PathBuf,
    metadata: Metadata,
    keys: Vec<WdxKey>,
}

impl App {
    pub(crate) fn duplicate_fields_dialog(&mut self, path: PathBuf) {
        self.dialog(
            "input",
            Pending::DuplicateFields(path),
            Dialog {
                title: "Duplikate nach Plugin-Feldern",
                description: "Alle Felder müssen übereinstimmen. Beispiele [=tc.name:1-3] [=tc.ext] [=Plugin.Feld.Einheit]. Dateien mit fehlenden Feldwerten werden ausgelassen.",
                placeholder: "Ein bis 16 Felder",
                options: &[
                    "Nur Felder",
                    "Felder und Name",
                    "Felder und Größe",
                    "Felder und Inhalt",
                    "Felder, Name und Größe",
                    "Felder, Name und Inhalt",
                ],
                ..Default::default()
            },
        );
    }

    pub(crate) fn find_duplicate_fields(
        &mut self,
        path: PathBuf,
        value: &str,
        option: usize,
    ) -> io::Result<()> {
        use tools::DuplicateMode::*;
        let mode = [
            None,
            Some(Name),
            Some(Size),
            Some(Content),
            Some(NameAndSize),
            Some(NameAndContent),
        ]
        .get(option)
        .copied()
        .ok_or_else(|| io::Error::other("Wähle ein Duplikatkriterium."))?;
        let criteria = parse_criteria(value, |token| {
            let (plugin, field) = self.resolve_wdx_token(token)?;
            Ok(Field::Plugin(plugin, field))
        })?;
        let ignore = self.ignore_list();
        let side = self.active;
        let request = self.panels[side].request;
        let description = crate::app::JobDescription::new(
            crate::OperationKind::Search,
            crate::display_path(&path),
            String::new(),
        );
        self.start_operation_job(
            "Suche Duplikate nach Plugin-Feldern …",
            false,
            description,
            move |job| {
                let groups = find(&path, &criteria, mode, ignore.as_deref(), &job)?;
                duplicate_result(side, request, path, groups, &job.cancel)
            },
        );
        Ok(())
    }
}

fn parse_criteria(
    mut value: &str,
    resolve: impl Fn(&str) -> io::Result<Field>,
) -> io::Result<Vec<Criterion>> {
    let mut criteria = Vec::new();
    loop {
        value = value.trim_start();
        if value.is_empty() {
            break;
        }
        let (token, remaining) = value
            .strip_prefix("[=")
            .and_then(|rest| rest.split_once(']'))
            .ok_or_else(|| {
                io::Error::other("Erwartet wird eine Liste aus [=Plugin.Feld]-Ausdrücken.")
            })?;
        let (token, range) = if let Some((token, range)) = token.rsplit_once(':') {
            rename::name_range("", range)?;
            (token, Some(range.to_owned()))
        } else {
            (token, None)
        };
        let field = match token.to_ascii_lowercase().as_str() {
            "tc.name" => Field::Name,
            "tc.ext" => Field::Extension,
            "tc.size" => Field::Size,
            "tc.writedate" => Field::WriteDate,
            "tc.writetime" => Field::WriteTime,
            "tc.creationdate" => Field::CreationDate,
            "tc.accessdate" => Field::AccessDate,
            _ if token
                .split_once('.')
                .is_some_and(|(p, _)| p.eq_ignore_ascii_case("tc")) =>
            {
                let field = token.split_once('.').unwrap().1.split('.').next().unwrap();
                if !arco_core::plugin::TC_METADATA_FIELDS
                    .iter()
                    .any(|name| name.eq_ignore_ascii_case(field))
                {
                    return Err(io::Error::other(format!(
                        "Unbekanntes internes Dateifeld: {token}"
                    )));
                }
                Field::Native(token.into())
            }
            _ => resolve(token)?,
        };
        criteria.push(Criterion { field, range });
        if criteria.len() > 16 {
            return Err(io::Error::other("Bis zu 16 Felder sind möglich."));
        }
        value = remaining;
    }
    if criteria.is_empty() {
        return Err(io::Error::other("Gib mindestens ein Duplikatfeld an."));
    }
    Ok(criteria)
}

fn key(value: WdxValue, range: Option<&str>) -> Option<WdxKey> {
    if value.display.is_empty() {
        return None;
    }
    if let Some(range) = range {
        let text = rename::name_range(&value.display, range).ok()?;
        return (!text.is_empty()).then(|| WdxKey::Text(text.to_lowercase()));
    }
    match value.key {
        WdxKey::Float(n) if !n.is_finite() => None,
        WdxKey::Float(n) if n == 0.0 => Some(WdxKey::Float(0.0)),
        key => Some(key),
    }
}

fn builtin(field: &Field, candidate: &Candidate) -> Option<WdxValue> {
    let value = match field {
        Field::Name => candidate.path.file_stem()?.to_str()?,
        Field::Extension => candidate.path.extension()?.to_str()?,
        Field::Size => {
            let size = i64::try_from(candidate.metadata.len()).ok()?;
            return Some(WdxValue {
                display: size.to_string(),
                key: WdxKey::Integer(size),
            });
        }
        Field::WriteDate | Field::WriteTime | Field::CreationDate | Field::AccessDate => {
            let time = match field {
                Field::CreationDate => candidate.metadata.created(),
                Field::AccessDate => candidate.metadata.accessed(),
                _ => candidate.metadata.modified(),
            }
            .ok()?;
            let pattern = if matches!(field, Field::WriteTime) {
                "%H:%M:%S"
            } else {
                "%Y-%m-%d"
            };
            let display = chrono::DateTime::<chrono::Local>::from(time)
                .format(pattern)
                .to_string();
            return Some(WdxValue {
                key: WdxKey::Text(display.clone()),
                display,
            });
        }
        Field::Plugin(..) | Field::Native(_) => return None,
    };
    Some(WdxValue {
        display: value.into(),
        key: WdxKey::Text(value.to_lowercase()),
    })
}

fn find(
    path: &Path,
    criteria: &[Criterion],
    mode: Option<tools::DuplicateMode>,
    ignore: Option<&IgnoreList>,
    job: &JobContext,
) -> io::Result<Vec<Vec<PathBuf>>> {
    let mut candidates: Vec<_> =
        tools::duplicate_files(&[path.to_path_buf()], ignore, &job.cancel)?
            .into_iter()
            .map(|(path, metadata)| Candidate {
                path,
                metadata,
                keys: Vec::with_capacity(criteria.len()),
            })
            .collect();
    for criterion in criteria {
        let range = criterion.range.as_deref();
        if let Field::Plugin(plugin, field) = &criterion.field {
            let paths: Vec<_> = candidates
                .iter()
                .map(|candidate| candidate.path.as_path())
                .collect();
            let mut values = HashMap::new();
            scan_paths(plugin, field, &paths, job, |path, value| {
                if let Some(key) = key(value, range) {
                    values.insert(path.to_path_buf(), key);
                }
            })?;
            candidates.retain_mut(|candidate| {
                values.remove(&candidate.path).is_some_and(|key| {
                    candidate.keys.push(key);
                    true
                })
            });
        } else {
            let mut filtered = Vec::with_capacity(candidates.len());
            for mut candidate in candidates {
                files::check_cancel(&job.cancel)?;
                let value = if let Field::Native(token) = &criterion.field {
                    let value = arco_core::plugin::tc_field_with_cancel(
                        token,
                        &candidate.path,
                        &candidate.metadata,
                        &job.cancel,
                    )?;
                    value.map(|mut value| {
                        if matches!(value.key, WdxKey::DateTime(_)) {
                            value.key = WdxKey::Text(value.display.to_lowercase());
                        }
                        value
                    })
                } else {
                    builtin(&criterion.field, &candidate)
                };
                if let Some(key) = value.and_then(|value| key(value, range)) {
                    candidate.keys.push(key);
                    filtered.push(candidate);
                }
            }
            candidates = filtered;
        }
        files::check_cancel(&job.cancel)?;
    }
    let mut grouped = BTreeMap::<Vec<WdxKey>, Vec<PathBuf>>::new();
    for candidate in candidates {
        files::check_cancel(&job.cancel)?;
        files::ensure_unchanged(&candidate.path, &candidate.metadata)?;
        grouped
            .entry(candidate.keys)
            .or_default()
            .push(candidate.path);
    }
    let mut result = Vec::new();
    for mut group in grouped.into_values().filter(|group| group.len() > 1) {
        files::check_cancel(&job.cancel)?;
        if let Some(mode) = mode {
            result.extend(tools::duplicates(&group, mode, None, &job.cancel)?);
        } else {
            group.sort();
            result.push(group);
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::{Arc, atomic::AtomicBool, mpsc};

    #[test]
    fn duplicate_fields_combine_ranges_and_content_and_exclude_missing_values() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path();
        for (file, data) in [
            ("Ähnlich-a.txt", "same"),
            ("ähnlich-b.txt", "same"),
            ("ähnlich-c.txt", "diff"),
            ("ähnlich-d.md", "same"),
            ("ähnlich-e", "same"),
        ] {
            fs::write(path.join(file), data).unwrap();
        }
        let (sender, _receiver) = mpsc::channel();
        let job = JobContext {
            cancel: Arc::new(AtomicBool::new(false)),
            sender,
        };
        let resolve = |_: &str| Err(io::Error::other("unexpected plugin"));
        let criteria = parse_criteria("[=tc.name:1-3] [=tc.ext] [=tc.size]", resolve).unwrap();
        let groups = find(path, &criteria, None, None, &job).unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].len(), 3);
        let groups = find(
            path,
            &criteria,
            Some(tools::DuplicateMode::Content),
            None,
            &job,
        )
        .unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].len(), 2);
        assert!(parse_criteria("[=tc.name:0]", resolve).is_err());
        assert!(
            key(
                WdxValue {
                    display: String::new(),
                    key: WdxKey::Text(String::new())
                },
                None
            )
            .is_none()
        );
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "requires the WDX test fixture in ARCO_TEST_WDX_DUPLICATES"]
    fn duplicate_fields_read_typed_plugin_values_with_named_units() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path();
        for file in ["a.txt", "b.txt", "c.md"] {
            fs::write(path.join(file), b"same").unwrap();
        }
        let plugin = PathBuf::from(std::env::var_os("ARCO_TEST_WDX_DUPLICATES").unwrap());
        let plugin = Registration::read(&plugin, "Fixture", "*.txt").unwrap();
        let criteria = parse_criteria("[=Fixture.Score.scaled] [=tc.size]", |token| {
            assert_eq!(token, "Fixture.Score.scaled");
            Ok(Field::Plugin(
                plugin.clone(),
                WdxField {
                    field: "Score".into(),
                    unit: Some("scaled".into()),
                },
            ))
        })
        .unwrap();
        let (sender, _receiver) = mpsc::channel();
        let job = JobContext {
            cancel: Arc::new(AtomicBool::new(false)),
            sender,
        };
        let groups = find(path, &criteria, None, None, &job).unwrap();
        assert_eq!(groups, vec![vec![path.join("a.txt"), path.join("b.txt")]]);
    }
}
