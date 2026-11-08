use super::{App, Event, JobContext, Outcome};
use crate::commands::{Dialog, Pending};
use crate::panel::FileModel;
use arco_core::files::{self, Entry, SortKey};
use arco_core::plugin::{self, Interface, Registration, Row, WdxColumn, WdxField, WdxValue};
use arco_core::rename;
use arco_core::view_settings::{Field, MAX_COLUMNS};
use regex::Regex;
use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

mod edit;
mod search;
pub(crate) use edit::{EditInput, EditTarget};
pub(crate) use search::SearchTarget;

const DEFAULT_COLUMNS: usize = 3;
const VALUE_BATCH: usize = 96;
pub(super) struct SortRequest {
    side: usize,
    source: Rc<Vec<Entry>>,
}

pub(super) struct Running {
    side: usize,
    model: Rc<FileModel>,
    cancel: Arc<AtomicBool>,
}

pub(super) struct Values {
    rows: Vec<(usize, Vec<String>)>,
    error: Option<String>,
}

impl App {
    fn wdx_plugin(&self, column: &WdxColumn) -> Option<&Registration> {
        self.profile.plugins.iter().find(|entry| {
            entry.enabled && entry.interface == Interface::Wdx && entry.path == column.plugin
        })
    }

    pub(super) fn active_wdx_columns(&self, side: usize) -> Vec<(usize, WdxColumn, Registration)> {
        if let Some(columns) = self.active_columns(side) {
            columns
                .iter()
                .filter_map(|column| match &column.field {
                    Field::Wdx(column) => Some(column),
                    _ => None,
                })
                .take(MAX_COLUMNS)
                .enumerate()
                .filter_map(|(index, column)| {
                    self.wdx_plugin(column)
                        .map(|plugin| (index, column.clone(), plugin.clone()))
                })
                .collect()
        } else {
            self.profile
                .wdx_columns
                .iter()
                .filter_map(|column| {
                    self.wdx_plugin(column)
                        .map(|plugin| (column.clone(), plugin.clone()))
                })
                .take(DEFAULT_COLUMNS)
                .enumerate()
                .map(|(index, (column, plugin))| (index, column, plugin))
                .collect()
        }
    }

    pub(super) fn invalidate_wdx_columns(&mut self) {
        self.wdx_sort_request = None;
        if let Some(running) = &self.wdx_pending {
            running.cancel.store(true, Ordering::Relaxed);
        }
        for panel in &mut self.panels {
            for tab in &mut panel.tabs {
                tab.reset_wdx_sort(self.profile.show_hidden);
                tab.file_model().clear_wdx();
            }
        }
        for side in 0..2 {
            self.publish(side);
        }
    }

    pub(super) fn query_wdx_fields(&mut self, entry: Registration) {
        self.read_wdx_fields(entry, None);
    }

    pub(crate) fn read_wdx_fields(&mut self, entry: Registration, search: Option<SearchTarget>) {
        self.close_dialog();
        self.start_job("Lese WDX-Felder …", false, move |job| {
            let rows = plugin::wdx_fields(&entry.path, &job.cancel)?;
            Ok(Outcome::WdxFields(Box::new(entry), rows, search))
        });
    }

    pub(super) fn choose_wdx_field(
        &mut self,
        entry: Registration,
        fields: Vec<Row>,
        search: Option<SearchTarget>,
    ) {
        let mut selections = Vec::new();
        for Row { name, value, .. } in fields {
            if name.is_empty() || name.chars().any(char::is_control) {
                continue;
            }
            let mut units = value
                .split('|')
                .filter(|unit| !unit.is_empty() && !unit.chars().any(char::is_control))
                .peekable();
            if units.peek().is_none() {
                selections.push(WdxField {
                    field: name,
                    unit: None,
                });
            } else {
                selections.extend(units.map(|unit| WdxField {
                    field: name.clone(),
                    unit: Some(unit.into()),
                }));
            }
        }
        if selections.is_empty() {
            self.text(
                "WDX-Spalte",
                "Dieses Plugin liefert keine darstellbaren Felder.",
            );
            return;
        }
        let labels: Vec<_> = selections.iter().map(WdxField::label).collect();
        let options: Vec<_> = labels.iter().map(String::as_str).collect();
        let description = entry.name.clone();
        let (title, pending) = if let Some(target) = search {
            if !self.wdx_search_is_current(&target) {
                return;
            }
            (
                "Suchfeld auswählen",
                Pending::WdxSearchField(target, Box::new(entry), selections),
            )
        } else {
            (
                "WDX-Spalte hinzufügen",
                Pending::WdxChoose(entry.path, selections),
            )
        };
        self.dialog(
            "confirm",
            pending,
            Dialog {
                title,
                description: &description,
                options: &options,
                ..Dialog::default()
            },
        );
    }

    pub(crate) fn sort_wdx(&mut self, index: usize) {
        if !self.can_read_wdx(self.active) {
            return;
        }
        if !self
            .active_wdx_columns(self.active)
            .into_iter()
            .any(|(position, _, _)| position == index)
        {
            return;
        }
        let show_hidden = self.profile.show_hidden;
        self.current_mut()
            .set_sort(SortKey::Wdx(index as u8), show_hidden);
        self.save();
    }

    pub(super) fn update_wdx_sort(&mut self) {
        if self.job.is_some()
            || !self.ui().get_dialog_kind().is_empty()
            || self.ui().get_settings_open()
        {
            return;
        }
        let Some(side) = [self.active, 1 - self.active].into_iter().find(|&side| {
            let tab = self.panels[side].current();
            self.can_read_wdx(side)
                && !tab.wdx_sorted
                && tab
                    .current_sort_orders()
                    .iter()
                    .any(|order| matches!(order.key, SortKey::Wdx(_)))
        }) else {
            return;
        };
        let tab = self.panels[side].current();
        let orders = tab.current_sort_orders().to_vec();
        let columns = self.active_wdx_columns(side);
        let specs: Vec<_> = orders
            .iter()
            .filter_map(|order| {
                let SortKey::Wdx(index) = order.key else {
                    return None;
                };
                let (_, column, plugin) = columns
                    .iter()
                    .find(|(pos, _, _)| *pos == usize::from(index))?;
                Some((index, column.clone(), plugin.clone()))
            })
            .collect();
        if specs.is_empty() {
            self.panels[side].current_mut().wdx_sorted = true;
            return;
        }
        let source = tab.entry_model();
        let mut entries = source.as_ref().clone();
        self.wdx_sort_request = Some(SortRequest { side, source });
        self.start_job("Lese WDX-Sortierwerte …", false, move |job| {
            let mut values = HashMap::<u8, HashMap<PathBuf, WdxValue>>::new();
            let paths = entry_paths(&entries);
            for (index, column, plugin) in specs {
                let mut field = HashMap::new();
                scan_paths(&plugin, &column.selection, &paths, &job, |path, value| {
                    field.insert(path.to_path_buf(), value);
                })?;
                values.insert(index, field);
            }
            files::sort_by_columns_with(&mut entries, &orders, |index, left, right, descending| {
                let field = values.get(&index);
                plugin::wdx_order(
                    field.and_then(|values| values.get(&left.path)),
                    field.and_then(|values| values.get(&right.path)),
                    descending,
                )
            });
            Ok(Outcome::WdxSorted(entries))
        });
    }

    pub(super) fn apply_wdx_order(&mut self, entries: Vec<Entry>) {
        let Some(request) = self.wdx_sort_request.take() else {
            return;
        };
        let panel = &mut self.panels[request.side];
        if !panel.loading && Rc::ptr_eq(&request.source, &panel.current().entry_model()) {
            panel
                .current_mut()
                .apply_wdx_sort(entries, self.profile.show_hidden);
            self.publish(request.side);
        }
    }

    pub(super) fn finish_wdx_sort_attempt(&mut self) {
        if let Some(request) = self.wdx_sort_request.take() {
            let panel = &mut self.panels[request.side];
            if !panel.loading && Rc::ptr_eq(&request.source, &panel.current().entry_model()) {
                panel.current_mut().wdx_sorted = true;
            }
        }
    }

    pub(crate) fn copy_details_with_wdx(&mut self, entries: Vec<Entry>, mut lines: Vec<String>) {
        let columns = self.active_wdx_columns(self.active);
        if columns.is_empty() {
            self.report(arco_windows::copy_text(&lines.join("\r\n")));
            return;
        }
        self.start_job("Kopiere WDX-Dateidetails …", false, move |job| {
            let paths = entry_paths(&entries);
            for (_, column, plugin) in columns {
                let mut values = HashMap::new();
                scan_paths(&plugin, &column.selection, &paths, &job, |path, value| {
                    values.insert(path.to_path_buf(), value.display);
                })?;
                for (entry, line) in entries.iter().zip(&mut lines) {
                    line.push('\t');
                    if let Some(value) = values.get(&entry.path) {
                        if value.contains(['\t', '\r', '\n', '"']) {
                            line.push('"');
                            line.push_str(&value.replace('"', "\"\""));
                            line.push('"');
                        } else {
                            line.push_str(value);
                        }
                    }
                }
            }
            Ok(Outcome::ClipboardText(lines.join("\r\n")))
        });
    }

    pub(crate) fn plan_wdx_rename(
        &mut self,
        paths: Vec<PathBuf>,
        mask: &str,
        replacement: Option<(Regex, String)>,
        clipboard: Option<String>,
    ) -> io::Result<()> {
        let mut fields = Vec::new();
        for token in rename::wdx_fields(mask)? {
            let (name, field) = token
                .split_once('.')
                .filter(|(name, field)| !name.is_empty() && !field.is_empty())
                .ok_or_else(|| io::Error::other("Ein WDX-Platzhalter benötigt [=Plugin.Feld]."))?;
            let plugin = self
                .profile
                .plugins
                .iter()
                .find(|plugin| {
                    plugin.enabled
                        && plugin.interface == Interface::Wdx
                        && (plugin.name.eq_ignore_ascii_case(name)
                            || plugin.path.file_stem().is_some_and(|stem| {
                                stem.eq_ignore_ascii_case(std::ffi::OsStr::new(name))
                            }))
                })
                .ok_or_else(|| {
                    io::Error::other(format!(
                        "Das WDX-Plugin „{name}“ fehlt oder ist deaktiviert."
                    ))
                })?;
            let (field, unit) = match field.split_once('.') {
                Some((field, unit)) if !field.is_empty() && !unit.is_empty() => {
                    (field, Some(unit.into()))
                }
                Some(_) => {
                    return Err(io::Error::other(
                        "Ein WDX-Platzhalter benötigt [=Plugin.Feld.Einheit].",
                    ));
                }
                None => (field, None),
            };
            fields.push((
                token.to_owned(),
                plugin.clone(),
                WdxField {
                    field: field.into(),
                    unit,
                },
            ));
        }
        let mask = mask.to_owned();
        self.start_job(
            "Lese WDX-Werte für die Umbenennung …",
            false,
            move |job| {
                let metadata = paths
                    .iter()
                    .map(|path| files::regular_metadata(path))
                    .collect::<io::Result<Vec<_>>>()?;
                let mut values = HashMap::new();
                let refs = paths.iter().map(PathBuf::as_path).collect::<Vec<_>>();
                for (token, plugin, field) in fields {
                    let mut column = HashMap::new();
                    scan_paths(&plugin, &field, &refs, &job, |path, value| {
                        column.insert(path.to_path_buf(), value.display);
                    })?;
                    values.insert(token, column);
                }
                for (path, metadata) in paths.iter().zip(&metadata) {
                    files::ensure_unchanged(path, metadata)?;
                }
                let plan = rename::Plan::new_with_clipboard(
                    &paths,
                    &mask,
                    replacement
                        .as_ref()
                        .map(|(regex, replace)| (regex, replace.as_str())),
                    clipboard.as_deref(),
                    |path, field| values.get(field)?.get(path).cloned(),
                )?;
                Ok(Outcome::RenamePreview(plan))
            },
        );
        Ok(())
    }

    pub(crate) fn add_wdx_column(
        &mut self,
        plugin: PathBuf,
        selection: WdxField,
    ) -> io::Result<()> {
        let field = selection.label();
        let column = WdxColumn { plugin, selection };
        if self.profile.wdx_columns.contains(&column) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "Diese WDX-Spalte ist bereits eingerichtet.",
            ));
        }
        let mut columns = self.profile.wdx_columns.clone();
        if columns.len() >= MAX_COLUMNS {
            return Err(io::Error::other(
                "Es können bis zu 29 WDX-Spalten eingerichtet werden.",
            ));
        }
        columns.push(column);
        self.commit_wdx_columns(columns)?;
        self.status(format!("WDX-Spalte hinzugefügt · {field}"));
        Ok(())
    }

    pub(crate) fn show_wdx_columns(&mut self) {
        let columns = self.profile.wdx_columns.clone();
        if columns.is_empty() {
            self.text("WDX-Spalten", "Wähle ein aktives WDX-Plugin in der Plugin-Verwaltung und klicke auf „WDX-Spalte hinzufügen“.");
            return;
        }
        let labels: Vec<_> = columns
            .iter()
            .map(|column| {
                let plugin = self
                    .profile
                    .plugins
                    .iter()
                    .find(|entry| entry.path == column.plugin)
                    .map_or("Plugin fehlt", |entry| entry.name.as_str());
                format!("{plugin} · {}", column.selection.label())
            })
            .collect();
        let options: Vec<_> = labels.iter().map(String::as_str).collect();
        self.dialog(
            "confirm",
            Pending::WdxRemove(columns),
            Dialog {
                title: "WDX-Spalte entfernen",
                description: "Wähle die Spalte, die aus dem Profil entfernt werden soll.",
                options: &options,
                ..Dialog::default()
            },
        );
    }

    pub(crate) fn remove_wdx_column(&mut self, column: &WdxColumn) -> io::Result<()> {
        let mut columns = self.profile.wdx_columns.clone();
        columns.retain(|item| item != column);
        self.commit_wdx_columns(columns)?;
        self.status(format!(
            "WDX-Spalte entfernt · {}",
            column.selection.label()
        ));
        Ok(())
    }

    fn commit_wdx_columns(&mut self, columns: Vec<WdxColumn>) -> io::Result<()> {
        if !self.save_profile {
            return Err(io::Error::other(
                "Das Arco-Profil konnte beim Start nicht geladen werden.",
            ));
        }
        let mut profile = self.profile_snapshot();
        profile.wdx_columns = columns;
        profile.save(&self.profile_path)?;
        self.profile = profile;
        self.invalidate_wdx_columns();
        Ok(())
    }

    fn can_read_wdx(&self, side: usize) -> bool {
        let panel = &self.panels[side];
        let tab = panel.current();
        !panel.loading
            && tab.loaded
            && tab.is_local()
            && tab.detailed()
            && (!self.quick_open || self.active == side)
    }

    pub(super) fn update_wdx_columns(&mut self) {
        if let Some(running) = &self.wdx_pending {
            if !self.can_read_wdx(running.side)
                || !Rc::ptr_eq(
                    &running.model,
                    &self.panels[running.side].current().file_model(),
                )
            {
                running.cancel.store(true, Ordering::Relaxed);
            }
            return;
        }
        let ui = self.ui();
        let Some((side, model, rows, columns)) = [self.wdx_next_side, 1 - self.wdx_next_side]
            .into_iter()
            .filter(|&side| self.can_read_wdx(side))
            .find_map(|side| {
                let columns = self.active_wdx_columns(side);
                if columns.is_empty() {
                    return None;
                }
                let (first, count) = if side == 0 {
                    (ui.get_left_wdx_first(), ui.get_left_wdx_count())
                } else {
                    (ui.get_right_wdx_first(), ui.get_right_wdx_count())
                };
                let model = self.panels[side].current().file_model();
                let rows = model.wdx_missing(
                    first.saturating_sub(4).max(0) as usize,
                    count.clamp(1, 512) as usize + 8,
                );
                (!rows.is_empty()).then(|| {
                    (
                        side,
                        model,
                        rows.into_iter().take(VALUE_BATCH).collect::<Vec<_>>(),
                        columns,
                    )
                })
            })
        else {
            return;
        };
        self.wdx_next_side = 1 - side;
        let cancel = Arc::new(AtomicBool::new(false));
        self.wdx_pending = Some(Running {
            side,
            model,
            cancel: cancel.clone(),
        });
        let sender = self.sender.clone();
        std::thread::spawn(move || {
            let width = columns.last().map_or(0, |(index, _, _)| index + 1);
            let mut result = Values {
                rows: rows
                    .iter()
                    .map(|(row, _)| (*row, vec![String::new(); width]))
                    .collect(),
                error: None,
            };
            for (index, column, plugin) in columns {
                if cancel.load(Ordering::Relaxed) {
                    break;
                }
                let (targets, files): (Vec<_>, Vec<_>) = rows
                    .iter()
                    .enumerate()
                    .filter(|(_, (_, path))| plugin.matches(Interface::Wdx, path))
                    .map(|(row, (_, path))| (row, path.clone()))
                    .unzip();
                if files.is_empty() {
                    continue;
                }
                match plugin::wdx_typed_field_values(&plugin.path, column.selection, files, &cancel)
                {
                    Ok(values) => {
                        for (row, value) in values {
                            if let Some(target) = targets.get(row) {
                                result.rows[*target].1[index] = value.display;
                            }
                        }
                    }
                    Err(error) => result.error = Some(format!("WDX · {} · {error}", plugin.name)),
                }
            }
            let _ = sender.send(Event::WdxValues(result));
        });
    }

    pub(super) fn apply_wdx_values(&mut self, values: Values) {
        let Some(running) = self.wdx_pending.take() else {
            return;
        };
        if !running.cancel.load(Ordering::Relaxed)
            && self.can_read_wdx(running.side)
            && Rc::ptr_eq(
                &running.model,
                &self.panels[running.side].current().file_model(),
            )
        {
            running.model.apply_wdx(values.rows);
            if let Some(error) = values.error {
                self.status(error);
            }
        }
    }
}

fn entry_paths(entries: &[Entry]) -> Vec<&Path> {
    entries.iter().map(|entry| entry.path.as_path()).collect()
}

fn scan_paths(
    plugin: &Registration,
    field: &WdxField,
    paths: &[&Path],
    job: &JobContext,
    mut read: impl FnMut(&Path, WdxValue),
) -> io::Result<()> {
    let masks = (!plugin.masks.is_empty())
        .then(|| files::compile_masks(&plugin.masks))
        .transpose()?;
    let label = field.label();
    for (index, batch) in paths.chunks(VALUE_BATCH).enumerate() {
        if job.cancel.load(Ordering::Relaxed) {
            return Err(io::ErrorKind::Interrupted.into());
        }
        job.progress(format!(
            "WDX · {label} · {}/{}",
            index * VALUE_BATCH + batch.len(),
            paths.len()
        ));
        let targets = batch
            .iter()
            .copied()
            .filter(|path| {
                masks
                    .as_ref()
                    .is_none_or(|masks| path.file_name().is_some_and(|name| masks.is_match(name)))
            })
            .collect::<Vec<_>>();
        if targets.is_empty() {
            continue;
        }
        let files = targets.iter().map(|path| path.to_path_buf()).collect();
        for (index, value) in
            plugin::wdx_typed_field_values(&plugin.path, field.clone(), files, &job.cancel)?
        {
            read(targets[index], value);
        }
    }
    Ok(())
}
