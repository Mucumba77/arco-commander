use super::{SavedPanel, SavedTab, TabView, VirtualTab, ini_value};
use crate::files::{SortKey, SortOrder};
use crate::view_settings::{Column, Field, MAX_COLUMNS};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;
use std::io;

const MAX_SETS: usize = 128;
const MAX_TAB_VIEWS: usize = 512;
const MAX_METADATA: usize = 256 * 1024;

#[derive(Default, Deserialize, Serialize)]
pub(super) struct PortableViews {
    sets: BTreeMap<String, Vec<Column>>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(super) options: BTreeMap<String, crate::view_settings::ColumnOptions>,
    tabs: [BTreeMap<usize, PortableView>; 2],
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(super) wfx: BTreeMap<String, crate::view_settings::WfxColumnSets>,
}

#[derive(Deserialize, Serialize)]
struct PortableView {
    columns: String,
    sort: Vec<SortOrder>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    wfx: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    wfx_view: Option<super::WfxView>,
}

impl PortableViews {
    pub(super) fn record(
        &mut self,
        side: usize,
        index: usize,
        tab: &SavedTab,
        definitions: &BTreeMap<String, Vec<Column>>,
    ) {
        if let Some(VirtualTab::Wfx { plugin, .. }) = &tab.virtual_tab {
            if tab.view != TabView::Details || !valid_sort(&tab.sort) {
                return;
            }
            let Some(sets) = self.wfx.get(plugin) else {
                return;
            };
            let selected = tab
                .wfx_view
                .as_ref()
                .filter(|selection| selection.plugin.eq_ignore_ascii_case(plugin))
                .map(|selection| selection.view.clone())
                .unwrap_or_else(|| {
                    sets.active
                        .clone()
                        .map(super::WfxView::Custom)
                        .unwrap_or_else(|| {
                            if sets.uses_plugin_default() {
                                super::WfxView::PluginDefault
                            } else {
                                super::WfxView::Standard
                            }
                        })
                });
            let columns = match &selected {
                super::WfxView::Custom(name) => name.clone(),
                _ => String::new(),
            };
            self.tabs[side].insert(
                index,
                PortableView {
                    columns,
                    sort: tab.sort.clone(),
                    wfx: Some(plugin.clone()),
                    wfx_view: Some(selected),
                },
            );
            return;
        }
        let Some(name) = tab.column_set.as_ref().filter(|_| {
            tab.view == TabView::Details && !matches!(tab.virtual_tab, Some(VirtualTab::Wfx { .. }))
        }) else {
            return;
        };
        let Some(columns) = definitions
            .get(name)
            .filter(|columns| valid_columns(name, columns))
        else {
            return;
        };
        let view = PortableView {
            columns: name.clone(),
            sort: tab.sort.clone(),
            wfx: None,
            wfx_view: None,
        };
        if !valid_sort(&view.sort) {
            return;
        }
        self.sets
            .entry(name.clone())
            .or_insert_with(|| columns.clone());
        self.tabs[side].insert(index, view);
    }

    pub(super) fn write_to(&self, text: &mut String) -> io::Result<()> {
        if self.sets.is_empty() && self.wfx.is_empty() {
            return Ok(());
        }
        let encoded = serde_json::to_string(self).map_err(io::Error::other)?;
        if encoded.len() > MAX_METADATA || !self.valid() {
            return Err(io::Error::other(
                "Die eingebetteten Spaltensätze überschreiten das Exportlimit.",
            ));
        }
        write!(text, "[ArcoTabViews]\r\nV1={encoded}\r\n").expect("write tab views to a string");
        Ok(())
    }

    pub(super) fn read_from(text: &str, source: usize) -> io::Result<Self> {
        let Some(encoded) = ini_value(text, "arcotabviews", "v1") else {
            return Ok(Self::default());
        };
        if encoded.len() > MAX_METADATA {
            return Err(io::Error::other(
                "Die eingebetteten Spaltensätze überschreiten das Importlimit.",
            ));
        }
        let mut views: Self = serde_json::from_str(encoded).map_err(io::Error::other)?;
        if !views.valid() {
            return Err(io::Error::other("Ungültige eingebettete Spaltensätze."));
        }
        if source == 1 {
            views.tabs.swap(0, 1);
        }
        Ok(views)
    }

    fn valid(&self) -> bool {
        self.sets.len() <= MAX_SETS
            && self.options.len() <= self.sets.len()
            && self
                .options
                .iter()
                .all(|(name, options)| self.sets.contains_key(name) && valid_options(options))
            && self.wfx.len() <= MAX_SETS
            && self.wfx.iter().all(|(plugin, sets)| {
                !plugin.is_empty()
                    && plugin.len() <= 160
                    && !plugin.chars().any(char::is_control)
                    && sets.sets.len() <= MAX_SETS
                    && sets.tc_layouts.len() <= MAX_SETS
                    && sets.tc_layouts.iter().all(|(id, layout)| {
                        (1..=128).contains(id)
                            && sets.sets.contains_key(&layout.name)
                            && layout.sort_keys.len() <= MAX_COLUMNS
                    })
                    && sets.options.len() <= sets.sets.len()
                    && sets.options.iter().all(|(name, options)| {
                        sets.sets.contains_key(name) && valid_options(options)
                    })
                    && sets
                        .sets
                        .iter()
                        .all(|(name, columns)| valid_columns(name, columns))
                    && sets
                        .active
                        .as_ref()
                        .is_none_or(|name| sets.sets.contains_key(name))
            })
            && self.tabs.iter().map(BTreeMap::len).sum::<usize>() <= MAX_TAB_VIEWS
            && self
                .sets
                .iter()
                .all(|(name, columns)| valid_columns(name, columns))
            && self.tabs.iter().flatten().all(|(_, view)| {
                valid_sort(&view.sort)
                    && match (&view.wfx, &view.wfx_view) {
                        (Some(plugin), Some(super::WfxView::Custom(name))) => self
                            .wfx
                            .get(plugin)
                            .is_some_and(|sets| sets.sets.contains_key(name)),
                        (Some(plugin), Some(_)) => self.wfx.contains_key(plugin),
                        (None, None) => self.sets.contains_key(&view.columns),
                        _ => false,
                    }
            })
    }

    pub(super) fn contains(&self, side: usize, index: u32) -> bool {
        self.tabs[side].contains_key(&(index as usize))
    }

    pub(super) fn retain_tabs(&mut self, positions: [BTreeMap<u32, usize>; 2]) {
        for (tabs, positions) in self.tabs.iter_mut().zip(positions) {
            *tabs = std::mem::take(tabs)
                .into_iter()
                .filter_map(|(index, view)| {
                    let original = u32::try_from(index).ok()?;
                    Some((*positions.get(&original)?, view))
                })
                .collect();
        }
        let referenced = self
            .tabs
            .iter()
            .flat_map(BTreeMap::values)
            .filter(|view| view.wfx.is_none())
            .map(|view| view.columns.as_str())
            .collect::<BTreeSet<_>>();
        self.sets
            .retain(|name, _| referenced.contains(name.as_str()));
        self.options
            .retain(|name, _| referenced.contains(name.as_str()));
    }

    pub(super) fn set_count(&self) -> usize {
        self.sets.len() + self.wfx.values().map(|sets| sets.sets.len()).sum::<usize>()
    }

    pub(super) fn install(
        self,
        panels: &mut [SavedPanel; 2],
        columns: &mut BTreeMap<String, Vec<Column>>,
    ) -> io::Result<BTreeMap<String, String>> {
        let mut staged = columns.clone();
        let mut names = BTreeMap::new();
        for (name, definition) in self.sets {
            let stem = name.chars().take(64).collect::<String>();
            let candidate = std::iter::once(name.clone())
                .chain((1..).map(|suffix| format!("{stem} (Arco {suffix})")))
                .find(|candidate| {
                    staged
                        .get(candidate)
                        .is_none_or(|existing| *existing == definition)
                })
                .expect("a finite collection cannot occupy every name");
            staged.entry(candidate.clone()).or_insert(definition);
            names.insert(name, candidate);
        }
        if staged.len() > MAX_SETS && staged.len() > columns.len() {
            return Err(io::Error::other(
                "Der Import überschreitet das Limit von 128 Spaltensätzen.",
            ));
        }
        *columns = staged;
        for (panel, tabs) in panels.iter_mut().zip(self.tabs) {
            for (index, view) in tabs {
                if let Some(tab) = panel.tab_options.get_mut(index) {
                    tab.view = TabView::Details;
                    if let (Some(plugin), Some(selected)) = (view.wfx, view.wfx_view) {
                        if matches!(&tab.virtual_tab, Some(VirtualTab::Wfx { plugin: current, .. }) if current.eq_ignore_ascii_case(&plugin))
                        {
                            tab.wfx_view = Some(super::WfxTabView {
                                plugin,
                                view: selected,
                            });
                        }
                    } else {
                        tab.column_set = names.get(&view.columns).cloned();
                    }
                    tab.sort = view.sort;
                }
            }
        }
        Ok(names)
    }
}

fn valid_columns(name: &str, columns: &[Column]) -> bool {
    !name.is_empty()
        && name.trim() == name
        && name.chars().count() <= 80
        && !name.contains('|')
        && !name.chars().any(char::is_control)
        && !columns.is_empty()
        && columns.len() <= MAX_COLUMNS
        && columns.iter().all(|column| {
            matches!(
                column.field,
                Field::Extension
                    | Field::Size
                    | Field::Modified
                    | Field::Attributes
                    | Field::Comment
                    | Field::TcExpression(_)
                    | Field::Wdx(_)
                    | Field::Wfx(_)
            ) && match &column.field {
                Field::Wdx(field) => {
                    field.plugin.is_absolute()
                        && field.plugin.as_os_str().len() <= 32767
                        && !field.plugin.to_string_lossy().contains('\0')
                        && valid_field(&field.selection)
                }
                Field::Wfx(field) => valid_field(&field.selection),
                _ => true,
            } && !column.title.is_empty()
                && column.title.chars().count() <= 80
                && !column.title.contains('|')
                && !column.title.chars().any(char::is_control)
                && (40..=600).contains(&column.width)
        })
}

fn valid_field(field: &crate::plugin::WdxField) -> bool {
    let valid = |value: &str| {
        !value.is_empty() && value.len() <= 512 && !value.chars().any(char::is_control)
    };
    valid(&field.field) && field.unit.as_deref().is_none_or(valid)
}

fn valid_options(options: &crate::view_settings::ColumnOptions) -> bool {
    (40..=32767).contains(&options.name_width)
        && options
            .reference_width
            .is_none_or(|width| (40..=32767).contains(&width))
}

fn valid_sort(sort: &[SortOrder]) -> bool {
    sort.len() <= MAX_COLUMNS + 2
        && sort.iter().enumerate().all(|(index, entry)| {
            matches!(
                entry.key,
                SortKey::Name
                    | SortKey::Extension
                    | SortKey::Size
                    | SortKey::Modified
                    | SortKey::Attributes
                    | SortKey::Comment
                    | SortKey::Unsorted
                    | SortKey::Wdx(0..=28)
            ) && !sort[..index]
                .iter()
                .any(|previous| previous.key == entry.key)
        })
}
