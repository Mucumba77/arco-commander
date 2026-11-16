use super::{BUILTIN_FIELDS, Setup, name_line, parse_column_specs};
use crate::app::{App, Event, Outcome};
use crate::commands::{Dialog, Pending};
use crate::locale::tr;
use arco_core::files::SortKey;
use arco_core::plugin::{self, Registration, WfxField};
use arco_core::profile::{WfxTabView, WfxView};
use arco_core::view_settings::{Column, Field, WfxColumnSets, parse_wfx_view};
use std::collections::HashMap;
use std::fmt::Write as _;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
#[derive(Default)]
pub(in crate::app) struct DefaultViews {
    pub(super) cached: HashMap<PathBuf, Option<arco_core::view_settings::CustomView>>,
    loading: bool,
}
impl App {
    pub(in crate::app) fn wfx_tab_columns(&self, tab: &crate::panel::Tab) -> Option<&[Column]> {
        let location = tab.wfx.as_ref()?;
        if let Some(name) = tab
            .view_override
            .as_ref()
            .filter(|mode| !mode.inherit_view)
            .and_then(|mode| mode.columns.as_ref())
        {
            return self
                .wfx_sets(&location.plugin.path)
                .and_then(|sets| sets.sets.get(name))
                .or_else(|| self.profile.views.columns.get(name))
                .map(Vec::as_slice);
        }
        match tab.options.wfx_selection(&location.plugin.name) {
            Some(WfxView::Standard) => None,
            Some(WfxView::PluginDefault) => self
                .wfx_default_views
                .cached
                .get(&location.plugin.path)?
                .as_ref()
                .map(|view| view.columns.as_slice()),
            Some(WfxView::Custom(name)) => self
                .wfx_sets(&location.plugin.path)?
                .sets
                .get(name)
                .map(Vec::as_slice),
            None => self.wfx_view_columns(&location.plugin.path),
        }
    }

    fn wants_tab_default_view(&self, tab: &crate::panel::Tab) -> bool {
        let Some(location) = &tab.wfx else {
            return false;
        };
        if tab
            .view_override
            .as_ref()
            .is_some_and(|mode| !mode.inherit_view && mode.columns.is_some())
        {
            return false;
        }
        match tab.options.wfx_selection(&location.plugin.name) {
            Some(WfxView::PluginDefault) => true,
            Some(_) => false,
            None => self.wants_wfx_default_view(&location.plugin.path),
        }
    }
    pub(in crate::app) fn wfx_sets(&self, path: &Path) -> Option<&WfxColumnSets> {
        self.profile.views.wfx_columns.get(path).or_else(|| {
            let plugin = self
                .profile
                .plugins
                .iter()
                .find(|plugin| plugin.path == path)?;
            self.profile
                .views
                .portable_wfx
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case(&plugin.name))
                .map(|(_, sets)| sets)
        })
    }
    pub(super) fn wfx_view_columns(&self, path: &Path) -> Option<&[Column]> {
        if let Some(sets) = self.wfx_sets(path) {
            if let Some(columns) = sets.active_columns() {
                return Some(columns);
            }
            if !sets.uses_plugin_default() {
                return None;
            }
        }
        self.wfx_default_views
            .cached
            .get(path)?
            .as_ref()
            .map(|view| view.columns.as_slice())
    }
    fn wants_wfx_default_view(&self, path: &Path) -> bool {
        self.wfx_sets(path)
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
                && self.wants_tab_default_view(tab)
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
                .and_then(|(view, fields)| parse_wfx_view(&view, &fields));
            let _ = sender.send(Event::WfxDefaultView(path, columns));
        });
    }
    pub(in crate::app) fn apply_wfx_default_view(
        &mut self,
        path: PathBuf,
        columns: Option<arco_core::view_settings::CustomView>,
    ) {
        self.wfx_default_views.loading = false;
        let found = columns.is_some();
        self.wfx_default_views.cached.insert(path.clone(), columns);
        if found {
            for side in 0..2 {
                let tab = self.panels[side].current();
                if tab.detailed()
                    && self.wants_tab_default_view(tab)
                    && tab.wfx.as_ref().is_some_and(|location| {
                        location.plugin.path == path && self.wfx_enabled(&location.plugin)
                    })
                {
                    self.panels[side].current_mut().wdx_sorted = false;
                    self.panels[side].current().file_model().clear_wdx();
                    self.publish(side);
                }
            }
        }
    }
    pub(in crate::app) fn active_wfx_columns(&self, side: usize) -> Vec<WfxField> {
        if self.panels[side].current().wfx.is_none() {
            return Vec::new();
        }
        self.value_columns(side)
            .into_iter()
            .map(|(_, column)| WfxField {
                selection: column.selection,
                descending: column.descending,
            })
            .collect()
    }
    pub(in crate::app) fn choose_wfx_columns(&mut self, plugin: Registration) {
        let sets = self.wfx_sets(&plugin.path);
        let names: Vec<_> = sets
            .into_iter()
            .flat_map(|sets| sets.sets.keys().cloned())
            .collect();
        let options: Vec<_> = ["Standardansicht", "Neu anlegen", "Plugin-Standardansicht"]
            .into_iter()
            .map(tr)
            .chain(names.iter().map(String::as_str))
            .collect();
        let selected = self.current().options.wfx_selection(&plugin.name);
        let active_name = match selected {
            Some(WfxView::Custom(name)) => name.as_str(),
            Some(WfxView::Standard) => tr("Standardansicht"),
            Some(WfxView::PluginDefault) => tr("Plugin-Standardansicht"),
            None => sets
                .and_then(|sets| sets.active.as_deref())
                .unwrap_or_else(|| {
                    if self.wants_wfx_default_view(&plugin.path) {
                        tr("Plugin-Standardansicht")
                    } else {
                        tr("Standardansicht")
                    }
                }),
        };
        let description = format!("{} · Aktiver Spaltensatz: {}", plugin.name, active_name);
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
                sets.active = None;
                sets.plugin_default = use_default;
                self.commit_wfx_columns(&plugin, sets)?;
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
    pub(in crate::app) fn manage_wfx_columns(
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
            0 => {
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
        self.start_operation_job(
            "Lese WFX-Spaltenfelder …",
            false,
            crate::app::JobDescription::new(
                crate::OperationKind::Plugin,
                plugin.name.clone(),
                String::new(),
            ),
            move |job| {
                let fields =
                    plugin::wfx_content_fields(&plugin.path, &job.cancel, &mut |prompt| {
                        job.prompt(&plugin.name, prompt)
                    })?;
                if fields.is_empty() {
                    return Err(io::Error::other(
                        "Das WFX-Plugin liefert keine Spaltenfelder.",
                    ));
                }
                Ok(Outcome::WfxColumnFields(Box::new(plugin), name, fields))
            },
        );
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
        self.save_wfx_column_values(plugin, original, name, columns, None)
    }
    pub(in crate::app) fn save_wfx_column_values(
        &mut self,
        plugin: Registration,
        original: Option<String>,
        name: String,
        columns: Vec<Column>,
        options: Option<arco_core::view_settings::ColumnOptions>,
    ) -> io::Result<()> {
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
        if sets
            .sets
            .get(&name)
            .is_some_and(|previous| *previous != columns)
        {
            // Original TC column positions no longer describe an edited layout.
            sets.tc_layouts.retain(|_, layout| layout.name != name);
        }
        sets.sets.insert(name.clone(), columns);
        if let Some(options) = options {
            sets.options.insert(name.clone(), options);
        }
        sets.active = Some(name);
        self.commit_wfx_columns(&plugin, sets)
    }
    fn wfx_column_sets(&self, plugin: &Path) -> WfxColumnSets {
        self.wfx_sets(plugin).cloned().unwrap_or_default()
    }
    fn commit_wfx_columns(
        &mut self,
        plugin: &Registration,
        mut sets: WfxColumnSets,
    ) -> io::Result<()> {
        sets.tc_layouts
            .retain(|_, layout| sets.sets.contains_key(&layout.name));
        if !self.wfx_enabled(plugin) {
            return Err(io::Error::other("Dieses WFX-Plugin ist deaktiviert."));
        }
        if !self.save_profile {
            return Err(io::Error::other("Das Arco-Profil ist nicht verfügbar."));
        }
        let selected = sets.active.clone().map(WfxView::Custom).unwrap_or_else(|| {
            if sets.uses_plugin_default() {
                WfxView::PluginDefault
            } else {
                WfxView::Standard
            }
        });
        let names: std::collections::BTreeSet<_> = sets.sets.keys().cloned().collect();
        let applies = self
            .current()
            .wfx
            .as_ref()
            .is_some_and(|location| location.plugin.same_file(plugin));
        let mut profile = self.profile_snapshot();
        if applies
            && let Some(tab) = profile.panels[self.active]
                .tab_options
                .get_mut(self.panels[self.active].tab)
        {
            tab.wfx_view = Some(WfxTabView {
                plugin: plugin.name.clone(),
                view: selected.clone(),
            });
        }
        for panel in profile
            .panels
            .iter_mut()
            .chain(profile.tab_groups.values_mut().flatten())
        {
            for tab in &mut panel.tab_options {
                if matches!(tab.wfx_selection(&plugin.name), Some(WfxView::Custom(name)) if !names.contains(name))
                {
                    tab.wfx_view = None;
                }
            }
        }
        profile.views.wfx_columns.insert(plugin.path.clone(), sets);
        profile.save(&self.profile_path)?;
        self.profile = profile;
        let current_side = self.active;
        for (side, panel) in self.panels.iter_mut().enumerate() {
            for (index, tab) in panel.tabs.iter_mut().enumerate() {
                if tab
                    .wfx
                    .as_ref()
                    .is_some_and(|location| location.plugin.same_file(plugin))
                {
                    if applies && side == current_side && index == panel.tab {
                        tab.options.wfx_view = Some(WfxTabView {
                            plugin: plugin.name.clone(),
                            view: selected.clone(),
                        });
                        tab.options.view = arco_core::profile::TabView::Details;
                        tab.manual_view = true;
                        tab.view_override = None;
                    } else if matches!(tab.options.wfx_selection(&plugin.name), Some(WfxView::Custom(name)) if !names.contains(name))
                    {
                        tab.options.wfx_view = None;
                    }
                }
            }
        }
        let counts: Vec<Vec<_>> = self
            .panels
            .iter()
            .map(|panel| {
                panel
                    .tabs
                    .iter()
                    .map(|tab| {
                        self.wfx_tab_columns(tab).map_or(0, |columns| {
                            columns
                                .iter()
                                .filter(|column| column.field.requires_values())
                                .count()
                        })
                    })
                    .collect()
            })
            .collect();
        for (side, panel) in self.panels.iter_mut().enumerate() {
            for (index, tab) in panel.tabs.iter_mut().enumerate() {
                if tab
                    .wfx
                    .as_ref()
                    .is_some_and(|location| location.plugin.same_file(plugin))
                {
                    let mut orders = tab.current_sort_orders().to_vec();
                    let previous = orders.len();
                    orders.retain(|order| !matches!(order.key, SortKey::Wdx(column) if usize::from(column) >= counts[side][index]));
                    if orders.len() != previous {
                        tab.set_sort_orders(orders, self.profile.show_hidden);
                    }
                }
            }
        }
        self.invalidate_wdx_columns();
        self.close_dialog();
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
            ..Default::default()
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
