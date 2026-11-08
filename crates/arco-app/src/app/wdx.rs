use super::{App, Event, JobContext, Outcome, strings};
use crate::commands::{Dialog, Pending};
use crate::panel::FileModel;
use arco_core::files::{self, Entry, Kind, SortKey};
use arco_core::plugin::{self, Interface, Registration, Row, WdxColumn, WdxValue};
use arco_core::rename;
use regex::Regex;
use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

mod search;
pub(crate) use search::SearchTarget;

const MAX_COLUMNS: usize = 3;
const VALUE_BATCH: usize = 96;
pub(super) struct SortRequest {
    side: usize,
    source: Rc<Vec<Entry>>,
    index: usize,
    descending: bool,
}

pub(super) struct Running {
    side: usize,
    model: Rc<FileModel>,
    cancel: Arc<AtomicBool>,
}

pub(super) struct Values {
    rows: Vec<(usize, [String; MAX_COLUMNS])>,
    error: Option<String>,
}

impl App {
    fn active_wdx_columns(&self) -> Vec<(WdxColumn, Registration)> {
        self.profile
            .wdx_columns
            .iter()
            .filter_map(|column| {
                self.profile
                    .plugins
                    .iter()
                    .find(|entry| {
                        entry.enabled
                            && entry.interface == Interface::Wdx
                            && entry.path == column.plugin
                    })
                    .map(|entry| (column.clone(), entry.clone()))
            })
            .take(MAX_COLUMNS)
            .collect()
    }

    pub(super) fn invalidate_wdx_columns(&mut self) {
        self.wdx_sort_request = None;
        if let Some(running) = &self.wdx_pending {
            running.cancel.store(true, Ordering::Relaxed);
        }
        let mut changed = [false; 2];
        for (side, panel) in self.panels.iter_mut().enumerate() {
            changed[side] = panel.current().wdx_sort.is_some();
            for tab in &mut panel.tabs {
                tab.reset_wdx_sort(self.profile.show_hidden);
                tab.file_model().clear_wdx();
            }
        }
        self.ui().set_wdx_headings(strings(
            self.active_wdx_columns()
                .into_iter()
                .map(|(column, _)| column.field),
        ));
        for (side, changed) in changed.into_iter().enumerate() {
            if changed {
                self.publish(side);
            }
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
        let names: Vec<_> = fields
            .into_iter()
            .map(|row| row.name)
            .filter(|name| !name.is_empty() && !name.chars().any(char::is_control))
            .collect();
        if names.is_empty() {
            self.text(
                "WDX-Spalte",
                "Dieses Plugin liefert keine darstellbaren Felder.",
            );
            return;
        }
        let options: Vec<_> = names.iter().map(String::as_str).collect();
        let description = entry.name.clone();
        let (title, pending) = if let Some(target) = search {
            if !self.wdx_search_is_current(&target) {
                return;
            }
            (
                "Suchfeld auswählen",
                Pending::WdxSearchField(target, Box::new(entry), names.clone()),
            )
        } else {
            (
                "WDX-Spalte hinzufügen",
                Pending::WdxChoose(entry.path, names.clone()),
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
        let Some((column, plugin)) = self.active_wdx_columns().into_iter().nth(index) else {
            return;
        };
        if self.job.is_some() {
            self.status("Ein Auftrag läuft bereits.");
            return;
        }
        let tab = self.current();
        let descending = tab.wdx_sort == Some((index, false));
        let source = tab.entry_model();
        let mut entries = source.as_ref().clone();
        self.wdx_sort_request = Some(SortRequest {
            side: self.active,
            source,
            index,
            descending,
        });
        self.start_job("Sortiere nach WDX-Feld …", false, move |job| {
            let mut values = HashMap::new();
            let paths = entry_paths(&entries);
            scan_paths(&plugin, &column.field, &paths, &job, |path, value| {
                values.insert(path.to_path_buf(), value);
            })?;
            files::sort(&mut entries, SortKey::Name, false);
            entries.sort_by(|left, right| {
                (right.kind == Kind::Directory)
                    .cmp(&(left.kind == Kind::Directory))
                    .then_with(|| {
                        plugin::wdx_order(
                            values.get(&left.path),
                            values.get(&right.path),
                            descending,
                        )
                    })
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
            panel.current_mut().apply_wdx_sort(
                request.index,
                request.descending,
                entries,
                self.profile.show_hidden,
            );
            self.publish(request.side);
        }
    }

    pub(crate) fn copy_details_with_wdx(&mut self, entries: Vec<Entry>, mut lines: Vec<String>) {
        let columns = self.active_wdx_columns();
        if columns.is_empty() {
            self.report(arco_windows::copy_text(&lines.join("\r\n")));
            return;
        }
        self.start_job("Kopiere WDX-Dateidetails …", false, move |job| {
            let paths = entry_paths(&entries);
            for (column, plugin) in columns {
                let mut values = HashMap::new();
                scan_paths(&plugin, &column.field, &paths, &job, |path, value| {
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
            fields.push((token.to_owned(), plugin.clone(), field.to_owned()));
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
                let plan = rename::Plan::new_with_fields(
                    &paths,
                    &mask,
                    replacement
                        .as_ref()
                        .map(|(regex, replace)| (regex, replace.as_str())),
                    |path, field| values.get(field)?.get(path).cloned(),
                )?;
                Ok(Outcome::RenamePreview(plan))
            },
        );
        Ok(())
    }

    pub(crate) fn add_wdx_column(&mut self, plugin: PathBuf, field: &str) -> io::Result<()> {
        let column = WdxColumn {
            plugin,
            field: field.into(),
        };
        if self.profile.wdx_columns.contains(&column) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "Diese WDX-Spalte ist bereits eingerichtet.",
            ));
        }
        let mut columns = self.profile.wdx_columns.clone();
        if columns.len() >= MAX_COLUMNS {
            return Err(io::Error::other(
                "Es können bis zu drei WDX-Spalten eingerichtet werden.",
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
                format!("{plugin} · {}", column.field)
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
        self.status(format!("WDX-Spalte entfernt · {}", column.field));
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
            && tab.detailed
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
        if self.profile.wdx_columns.is_empty() {
            return;
        }
        let ui = self.ui();
        let Some((side, model, rows)) = [self.wdx_next_side, 1 - self.wdx_next_side]
            .into_iter()
            .filter(|&side| self.can_read_wdx(side))
            .find_map(|side| {
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
                    )
                })
            })
        else {
            return;
        };
        let columns = self.active_wdx_columns();
        if columns.is_empty() {
            return;
        }
        self.wdx_next_side = 1 - side;
        let cancel = Arc::new(AtomicBool::new(false));
        self.wdx_pending = Some(Running {
            side,
            model,
            cancel: cancel.clone(),
        });
        let sender = self.sender.clone();
        std::thread::spawn(move || {
            let mut result = Values {
                rows: rows
                    .iter()
                    .map(|(row, _)| (*row, Default::default()))
                    .collect(),
                error: None,
            };
            for (index, (column, plugin)) in columns.into_iter().enumerate() {
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
                match plugin::wdx_values(&plugin.path, column.field, files, &cancel) {
                    Ok(values) => {
                        for row in values {
                            if let Some(target) = row
                                .name
                                .parse::<usize>()
                                .ok()
                                .and_then(|row| targets.get(row))
                            {
                                result.rows[*target].1[index] = row.value;
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
    field: &str,
    paths: &[&Path],
    job: &JobContext,
    mut read: impl FnMut(&Path, WdxValue),
) -> io::Result<()> {
    let masks = (!plugin.masks.is_empty())
        .then(|| files::compile_masks(&plugin.masks))
        .transpose()?;
    for (index, batch) in paths.chunks(VALUE_BATCH).enumerate() {
        if job.cancel.load(Ordering::Relaxed) {
            return Err(io::ErrorKind::Interrupted.into());
        }
        job.progress(format!(
            "WDX · {field} · {}/{}",
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
            plugin::wdx_typed_values(&plugin.path, field.into(), files, &job.cancel)?
        {
            read(targets[index], value);
        }
    }
    Ok(())
}
