use super::{BUILTIN_FIELDS, Setup, name_line, parse_column_specs};
use crate::app::{App, Event, Outcome};
use crate::commands::{Dialog, Pending};
use crate::locale::tr;
use arco_core::files::SortKey;
use arco_core::plugin::{self, Registration, WfxField};
use arco_core::view_settings::{Column, Field, WfxColumnSets, parse_wfx_default_view};
use std::collections::HashMap;
use std::fmt::Write as _;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

#[derive(Default)]
pub(in crate::app) struct DefaultViews {
    cached: HashMap<PathBuf, Option<Vec<Column>>>,
    loading: bool,
}

impl App {
    pub(super) fn wfx_view_columns(&self, path: &Path) -> Option<&[Column]> {
        if let Some(sets) = self.profile.views.wfx_columns.get(path) {
            if let Some(columns) = sets.active_columns() {
                return Some(columns);
            }
            if !sets.uses_plugin_default() {
                return None;
            }
        }
        self.wfx_default_views.cached.get(path)?.as_deref()
    }

    fn wants_wfx_default_view(&self, path: &Path) -> bool {
        self.profile
            .views
            .wfx_columns
            .get(path)
            .is_none_or(WfxColumnSets::uses_plugin_default)
    }

    pub(in crate::app) fn update_wfx_default_views(&mut self) {
        if self.wfx_default_views.loading {
            return;
        }
        let Some(path) = [self.active, 1 - self.active].into_iter().find_map(|side| {
            let panel = &self.panels[side];
            let tab = panel.current();
            let location = tab.wfx.as_ref()?;
            let path = &location.plugin.path;
            (tab.detailed()
                && tab.loaded
                && !panel.loading
                && (!self.quick_open || side == self.active)
                && self.wfx_enabled(&location.plugin)
                && self.wants_wfx_default_view(path)
                && !self.wfx_default_views.cached.contains_key(path))
            .then(|| path.clone())
        }) else {
            return;
        };
        self.wfx_default_views.loading = true;
        let sender = self.sender.clone();
        std::thread::spawn(move || {
            let columns = plugin::wfx_default_view(&path, &AtomicBool::new(false))
                .ok()
                .flatten()
                .and_then(|(view, fields)| parse_wfx_default_view(&view, &fields));
            let _ = sender.send(Event::WfxDefaultView(path, columns));
        });
    }

    pub(in crate::app) fn apply_wfx_default_view(
        &mut self,
        path: PathBuf,
        columns: Option<Vec<Column>>,
    ) {
        self.wfx_default_views.loading = false;
        let found = columns.is_some();
        self.wfx_default_views.cached.insert(path.clone(), columns);
        if found && self.wants_wfx_default_view(&path) {
            for side in 0..2 {
                let tab = self.panels[side].current();
                if tab.detailed()
                    && tab.wfx.as_ref().is_some_and(|location| {
                        location.plugin.path == path && self.wfx_enabled(&location.plugin)
                    })
                {
                    self.publish(side);
                }
            }
        }
    }

    pub(in crate::app) fn active_wfx_columns(&self, side: usize) -> Vec<WfxField> {
        if self.panels[side].current().wfx.is_none() {
            return Vec::new();
        }
        self.active_columns(side)
            .unwrap_or_default()
            .iter()
            .filter_map(|column| match &column.field {
                Field::Wfx(field) => Some(field.clone()),
                _ => None,
            })
            .collect()
    }

    pub(super) fn choose_wfx_columns(&mut self, plugin: Registration) {
        let sets = self.profile.views.wfx_columns.get(&plugin.path);
        let names: Vec<_> = sets
            .into_iter()
            .flat_map(|sets| sets.sets.keys().cloned())
            .collect();
        let options: Vec<_> = ["Standardansicht", "Neu anlegen", "Plugin-Standardansicht"]
            .into_iter()
            .map(tr)
            .chain(names.iter().map(String::as_str))
            .collect();
        let description = format!(
            "{} · Aktiver Spaltensatz: {}",
            plugin.name,
            sets.and_then(|sets| sets.active.as_deref())
                .unwrap_or_else(|| {
                    if self.wants_wfx_default_view(&plugin.path)
                        && self
                            .wfx_default_views
                            .cached
                            .get(&plugin.path)
                            .is_some_and(Option::is_some)
                    {
                        tr("Plugin-Standardansicht")
                    } else {
                        tr("Standardansicht")
                    }
                })
        );
        self.dialog(
            "confirm",
            Pending::ViewSetup(Setup::ChooseWfxColumns(Box::new(plugin), names.clone())),
            Dialog {
                title: "WFX-Spaltensätze",
                description: &description,
                options: &options,
                ..Dialog::default()
            },
        );
    }

    pub(super) fn select_wfx_columns(
        &mut self,
        plugin: Registration,
        names: &[String],
        option: usize,
    ) -> io::Result<()> {
        match option {
            0 | 2 => {
                let mut sets = self.wfx_column_sets(&plugin.path);
                let use_default = Some(option == 2);
                if sets.active.take().is_some() || sets.plugin_default != use_default {
                    sets.plugin_default = use_default;
                    self.commit_wfx_columns(&plugin, sets)?;
                }
            }
            1 => self.read_wfx_columns(plugin, None),
            _ => {
                if let Some(name) = names.get(option - 3) {
                    let description = format!("{} · {name}", plugin.name);
                    self.dialog(
                        "confirm",
                        Pending::ViewSetup(Setup::ManageWfxColumns(Box::new(plugin), name.clone())),
                        Dialog {
                            title: "WFX-Spaltensätze",
                            description: &description,
                            options: &[
                                tr("Für dieses WFX-Plugin verwenden"),
                                tr("Bearbeiten"),
                                tr("Löschen"),
                            ],
                            ..Dialog::default()
                        },
                    );
                }
            }
        }
        Ok(())
    }

    pub(super) fn manage_wfx_columns(
        &mut self,
        plugin: Registration,
        name: String,
        option: usize,
    ) -> io::Result<()> {
        let mut sets = self.wfx_column_sets(&plugin.path);
        if !sets.sets.contains_key(&name) {
            return Err(io::Error::other(
                "Der WFX-Spaltensatz ist nicht mehr vorhanden.",
            ));
        }
        match option {
            0 if sets.active.as_ref() != Some(&name) => {
                sets.active = Some(name);
                self.commit_wfx_columns(&plugin, sets)?;
            }
            1 => self.read_wfx_columns(plugin, Some(name)),
            2 => {
                sets.sets.remove(&name);
                if sets.active.as_ref() == Some(&name) {
                    sets.active = None;
                }
                self.commit_wfx_columns(&plugin, sets)?;
            }
            _ => (),
        }
        Ok(())
    }

    fn read_wfx_columns(&mut self, plugin: Registration, name: Option<String>) {
        if !self.wfx_enabled(&plugin) {
            self.status("Dieses WFX-Plugin ist deaktiviert.");
            return;
        }
        self.start_job("Lese WFX-Spaltenfelder …", false, move |job| {
            let fields = plugin::wfx_content_fields(&plugin.path, &job.cancel, &mut |prompt| {
                job.prompt(&plugin.name, prompt)
            })?;
            if fields.is_empty() {
                return Err(io::Error::other(
                    "Das WFX-Plugin liefert keine Spaltenfelder.",
                ));
            }
            Ok(Outcome::WfxColumnFields(Box::new(plugin), name, fields))
        });
    }

    pub(in crate::app) fn edit_wfx_columns(
        &mut self,
        plugin: Registration,
        name: Option<String>,
        mut fields: Vec<WfxField>,
    ) {
        let sets = self.wfx_column_sets(&plugin.path);
        let existing = name.as_ref().and_then(|name| sets.sets.get(name));
        let mut value = name.clone().unwrap_or_else(|| "Meine WFX-Spalten".into());
        if let Some(columns) = existing {
            for column in columns {
                let key = match &column.field {
                    Field::Wfx(field) => {
                        let index = fields
                            .iter()
                            .position(|candidate| candidate.selection == field.selection)
                            .unwrap_or_else(|| {
                                fields.push(field.clone());
                                fields.len() - 1
                            });
                        format!("wfx{}", index + 1)
                    }
                    Field::TcExpression(expression) => format!("tc:{}", expression.source()),
                    Field::Wdx(_) => continue,
                    field => BUILTIN_FIELDS
                        .iter()
                        .find(|(_, native)| native == field)
                        .map_or_else(|| field.key().to_owned(), |(key, _)| (*key).into()),
                };
                let alignment = match column.right_aligned {
                    Some(true) => " | rechts",
                    Some(false) => " | links",
                    None => "",
                };
                let _ = write!(
                    value,
                    "\n{key} | {} | {}{alignment}",
                    column.title, column.width
                );
            }
        } else {
            let _ = write!(
                value,
                "\nwfx1 | {} | 150",
                fields[0].selection.label().replace('|', " ")
            );
        }
        let mut description = format!(
            "{}\nErste Zeile: Name. Danach je Spalte Feld | Titel | Breite | links oder rechts (optional).",
            plugin.name
        );
        for (index, field) in fields.iter().enumerate() {
            let _ = write!(
                description,
                "\nwfx{} = {}",
                index + 1,
                field.selection.label()
            );
        }
        self.dialog(
            "multiline",
            Pending::ViewSetup(Setup::EditWfxColumns(Box::new(plugin), name, fields)),
            Dialog {
                title: "WFX-Spaltensatz bearbeiten",
                description: &description,
                value: &value,
                ..Dialog::default()
            },
        );
    }

    pub(super) fn save_wfx_columns(
        &mut self,
        plugin: Registration,
        original: Option<String>,
        fields: &[WfxField],
        value: &str,
    ) -> io::Result<()> {
        let mut lines = value.lines();
        let name = name_line(lines.next(), original.as_deref())?;
        let columns = parse_wfx_columns(lines, fields)?;
        let mut sets = self.wfx_column_sets(&plugin.path);
        if original.is_none() && sets.sets.contains_key(&name) {
            return Err(io::Error::other(
                "Dieser WFX-Spaltensatz existiert bereits.",
            ));
        }
        if sets.sets.len() >= 128 && !sets.sets.contains_key(&name) {
            return Err(io::Error::other(
                "Für ein WFX-Plugin sind höchstens 128 Spaltensätze möglich.",
            ));
        }
        sets.sets.insert(name.clone(), columns);
        sets.active = Some(name);
        self.commit_wfx_columns(&plugin, sets)
    }

    fn wfx_column_sets(&self, plugin: &Path) -> WfxColumnSets {
        self.profile
            .views
            .wfx_columns
            .get(plugin)
            .cloned()
            .unwrap_or_default()
    }

    fn commit_wfx_columns(&mut self, plugin: &Registration, sets: WfxColumnSets) -> io::Result<()> {
        if !self.wfx_enabled(plugin) {
            return Err(io::Error::other("Dieses WFX-Plugin ist deaktiviert."));
        }
        if !self.save_profile {
            return Err(io::Error::other("Das Arco-Profil ist nicht verfügbar."));
        }
        let mut profile = self.profile_snapshot();
        if sets.sets.is_empty() && sets.plugin_default.is_none() {
            profile.views.wfx_columns.remove(&plugin.path);
        } else {
            profile.views.wfx_columns.insert(plugin.path.clone(), sets);
        }
        profile.save(&self.profile_path)?;
        self.profile = profile;
        let show_hidden = self.profile.show_hidden;
        for panel in &mut self.panels {
            for tab in &mut panel.tabs {
                if tab
                    .wfx
                    .as_ref()
                    .is_some_and(|location| location.plugin.same_file(plugin))
                {
                    let mut orders = tab.current_sort_orders().to_vec();
                    let previous = orders.len();
                    orders.retain(|order| !matches!(order.key, SortKey::Wdx(_)));
                    if orders.len() != previous {
                        tab.set_sort_orders(orders, show_hidden);
                    }
                }
            }
        }
        self.invalidate_wdx_columns();
        self.status("WFX-Spaltensatz gespeichert.");
        Ok(())
    }
}

fn parse_wfx_columns<'a>(
    lines: impl Iterator<Item = &'a str>,
    fields: &[WfxField],
) -> io::Result<Vec<Column>> {
    parse_column_specs(lines, |key| {
        key.strip_prefix("wfx")
            .and_then(|index| index.parse::<usize>().ok()?.checked_sub(1))
            .and_then(|index| fields.get(index))
            .cloned()
            .map(Field::Wfx)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use arco_core::plugin::WdxField;

    #[test]
    fn named_wfx_sets_keep_fields_units_alignment_and_active_name() {
        let field = WfxField {
            selection: WdxField {
                field: "Amount".into(),
                unit: Some("KiB".into()),
            },
            descending: true,
        };
        let columns = parse_wfx_columns(
            ["wfx1 | Belegt | 180 | rechts", "erweiterung | Erw. | 60"].into_iter(),
            std::slice::from_ref(&field),
        )
        .unwrap();
        let sets = WfxColumnSets {
            sets: [("Belegung".into(), columns)].into(),
            active: Some("Belegung".into()),
            plugin_default: Some(true),
        };
        let saved = serde_json::to_string(&sets).unwrap();
        let loaded: WfxColumnSets = serde_json::from_str(&saved).unwrap();
        let active = loaded.active_columns().unwrap();
        assert_eq!(active[0].field, Field::Wfx(field));
        assert_eq!(active[0].right_aligned, Some(true));
        assert_eq!(active[1].field, Field::Extension);
        assert!(!loaded.uses_plugin_default());
    }

    #[test]
    fn explicit_standard_view_survives_empty_wfx_sets_and_old_profiles() {
        assert!(WfxColumnSets::default().uses_plugin_default());
        let legacy: WfxColumnSets =
            serde_json::from_str(r#"{"sets":{"Custom":[]},"active":null}"#).unwrap();
        assert!(!legacy.uses_plugin_default());
        let saved = serde_json::to_string(&WfxColumnSets {
            plugin_default: Some(false),
            ..Default::default()
        })
        .unwrap();
        let restored: WfxColumnSets = serde_json::from_str(&saved).unwrap();
        assert!(!restored.uses_plugin_default());
    }
}
