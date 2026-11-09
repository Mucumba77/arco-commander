use super::{App, strings};
use crate::commands::{Dialog, Pending};
use crate::panel::{FileModel, Location};
use crate::{Colors, FileColumn, FileRow, PanelData, file_color};
use arco_core::files::{Entry, SortKey, SortOrder};
use arco_core::plugin::{Registration, WdxColumn, WfxField};
use arco_core::profile::TabView;
use arco_core::view_settings::{
    ColorPair, Column, Field, FileColor, MAX_COLUMNS, Mode, Rule, Scope, Settings,
    TcColumnExpression, request_tc_drive_type, tc_drive_changed_since, tc_drive_revision,
};
use slint::{ComponentHandle, Model, ModelRc, ModelTracker, VecModel};
use std::io;
use std::rc::Rc;

mod wfx;
pub(super) use wfx::DefaultViews as WfxDefaultViews;

const BUILTIN_FIELDS: &[(&str, Field)] = &[
    ("erweiterung", Field::Extension),
    ("größe", Field::Size),
    ("datum", Field::Modified),
    ("attribute", Field::Attributes),
    ("kommentar", Field::Comment),
];
const SORT_KEYS: &[(&str, SortKey)] = &[
    ("name", SortKey::Name),
    ("erweiterung", SortKey::Extension),
    ("größe", SortKey::Size),
    ("datum", SortKey::Modified),
    ("attribute", SortKey::Attributes),
    ("kommentar", SortKey::Comment),
    ("unsortiert", SortKey::Unsorted),
];

pub(crate) enum Setup {
    Choose {
        modes: bool,
        names: Vec<String>,
    },
    Manage {
        modes: bool,
        name: String,
    },
    EditColumns {
        name: Option<String>,
        fields: Vec<WdxColumn>,
    },
    ChooseWfxColumns(Box<Registration>, Vec<String>),
    ManageWfxColumns(Box<Registration>, String),
    EditWfxColumns(Box<Registration>, Option<String>, Vec<WfxField>),
    EditMode(Option<String>),
    EditRules,
    ImportTc,
    ApplyTc(Box<arco_core::view_settings::TcViews>),
    Colors,
    Sort,
}

pub(super) fn automatic_view(
    settings: &Settings,
    location: &Location,
    scope: Option<Scope>,
    entries: &[Entry],
) -> Option<Mode> {
    let scope = scope.unwrap_or_else(|| match location {
        Location::Local(path) if crate::display_path(path).starts_with(r"\\") => Scope::Unc,
        Location::Local(_) => Scope::Local,
        Location::Archive { .. } => Scope::Archive,
        Location::Ftp(_) => Scope::Ftp,
        Location::Wfx(_) => Scope::Wfx,
        Location::Shell(_) => Scope::Shell,
    });
    settings
        .automatic_mode(scope, &location.display(), entries)
        .cloned()
}

struct ExpressionRows {
    source: Rc<FileModel>,
    expressions: Vec<TcColumnExpression>,
    local: bool,
}

impl Model for ExpressionRows {
    type Data = FileRow;

    fn row_count(&self) -> usize {
        self.source.row_count()
    }

    fn row_data(&self, index: usize) -> Option<FileRow> {
        let mut row = self.source.row_data(index)?;
        if let Some(entry) = self.source.entry(index) {
            row.expression_values = strings(
                self.expressions
                    .iter()
                    .map(|column| column.render(entry, self.local)),
            );
        }
        Some(row)
    }

    fn model_tracker(&self) -> &dyn ModelTracker {
        self.source.model_tracker()
    }
}

impl App {
    pub(super) fn poll_tc_drive_views(&mut self) {
        let settings = &self.profile.views;
        if !settings.uses_tc_drive_types() {
            return;
        }
        let eligible = |tab: &crate::panel::Tab| {
            tab.loaded && tab.is_local() && !tab.manual_view && tab.view_scope.is_none()
        };
        for panel in &self.panels {
            let tab = panel.current();
            if eligible(tab) && !panel.loading {
                request_tc_drive_type(&crate::display_path(&tab.path));
            }
        }
        let revision = tc_drive_revision();
        let previous = self.view_drive_revision;
        if revision == previous {
            return;
        }
        self.view_drive_revision = revision;
        let mut changed = [false; 2];
        for (side, panel) in self.panels.iter_mut().enumerate() {
            for (index, tab) in panel.tabs.iter_mut().enumerate() {
                let current = index == panel.tab;
                if !eligible(tab)
                    || (current && panel.loading)
                    || !tc_drive_changed_since(&crate::display_path(&tab.path), previous)
                {
                    continue;
                }
                let entries = tab.entry_model();
                let mode = automatic_view(settings, &tab.location(), None, &entries);
                if mode != tab.view_override {
                    tab.view_override = mode;
                    tab.resort(self.profile.show_hidden);
                    changed[side] |= current;
                }
            }
        }
        for (side, changed) in changed.into_iter().enumerate() {
            if changed {
                self.publish(side);
            }
        }
    }

    pub(super) fn active_columns(&self, side: usize) -> Option<&[Column]> {
        let tab = self.panels[side].current();
        if !tab.detailed() {
            return None;
        }
        let columns = if let Some(location) = &tab.wfx {
            if !self.wfx_enabled(&location.plugin) {
                return None;
            }
            self.wfx_view_columns(&location.plugin.path)?
        } else {
            self.profile
                .views
                .columns
                .get(tab.column_set()?)?
                .as_slice()
        };
        Some(&columns[..columns.len().min(MAX_COLUMNS)])
    }

    pub(super) fn publish_columns(&self, side: usize, data: &mut PanelData) {
        if let Some(columns) = self.active_columns(side) {
            let mut index = 0i32;
            let mut expressions = Vec::new();
            let columns: Vec<_> = columns
                .iter()
                .map(|column| {
                    let key = column.field.key();
                    let expression_index = if let Field::TcExpression(expression) = &column.field {
                        let index = expressions.len() as i32;
                        expressions.push(expression.clone());
                        index
                    } else {
                        -1
                    };
                    let (wdx_index, action) =
                        if matches!(column.field, Field::Wdx(_) | Field::Wfx(_)) {
                            let wdx_index = index;
                            index += 1;
                            (wdx_index, format!("sort-wdx:{wdx_index}"))
                        } else if !key.is_empty() {
                            (-1, format!("sort-{key}"))
                        } else {
                            (-1, String::new())
                        };
                    let width = i32::from(column.width.clamp(40, 600));
                    data.custom_width += width + 2;
                    data.custom_extension |= column.field == Field::Extension;
                    FileColumn {
                        title: column.title.as_str().into(),
                        key: key.into(),
                        action: action.into(),
                        width,
                        wdx_index,
                        expression_index,
                        right_aligned: column
                            .right_aligned
                            .unwrap_or(matches!(column.field, Field::Size | Field::Modified)),
                    }
                })
                .collect();
            data.custom_columns = ModelRc::new(VecModel::from(columns));
            if !expressions.is_empty() {
                let tab = self.panels[side].current();
                data.rows = ModelRc::from(Rc::new(ExpressionRows {
                    source: tab.file_model(),
                    expressions,
                    local: tab.is_local(),
                }));
            }
        } else if data.local && data.detailed {
            data.wdx_headings = strings(
                self.active_wdx_columns(side)
                    .into_iter()
                    .map(|(_, column, _)| column.selection.label()),
            );
        }
    }

    pub(crate) fn view_setup_action(&mut self, command: &str) -> bool {
        match command {
            "column-sets" => {
                if let Some(location) = &self.current().wfx {
                    self.choose_wfx_columns((*location.plugin).clone());
                } else {
                    self.choose_view(false);
                }
            }
            "view-modes" => self.choose_view(true),
            "auto-view-rules" => self.edit_view_rules(),
            "view-rules-import" => self.dialog(
                "input",
                Pending::ViewSetup(Setup::ImportTc),
                Dialog {
                    title: "TC-Ansichten importieren",
                    description: "Liest Ansichtsmodi und automatische Regeln aus einer wincmd.ini.",
                    placeholder: "Absoluter Pfad zur wincmd.ini",
                    ..Default::default()
                },
            ),
            "file-colors" => self.edit_file_colors(),
            "auto-view-toggle" => {
                let mut settings = self.profile.views.clone();
                settings.automatic = !settings.automatic;
                let enabled = settings.automatic;
                if let Err(error) = self.save_view_settings(settings) {
                    self.text("Automatische Ansichten", &error.to_string());
                } else {
                    self.status(if enabled {
                        "Automatische Ansichten aktiviert."
                    } else {
                        "Automatische Ansichten deaktiviert."
                    });
                }
            }
            "multi-sort" => {
                let wfx = self.current().wfx.is_some();
                let sort = sort_text_in(self.current().current_sort_orders(), wfx);
                let mut description = crate::locale::tr(
                    "Je Zeile ein Sortierfeld in Prioritätsreihenfolge. Ein Minus kehrt die Richtung um. Felder name, erweiterung, größe, datum, attribute, kommentar, unsortiert.",
                ).to_owned();
                let fields: Vec<_> = if wfx {
                    description.push('\n');
                    description.push_str(crate::locale::tr(
                        "Bei WFX gilt ohne Vorzeichen die Plugin-Vorgabe. + sortiert aufsteigend, - absteigend.",
                    ));
                    self.active_wfx_columns(self.active)
                        .into_iter()
                        .enumerate()
                        .map(|(index, field)| {
                            format!("wfx{} · {}", index + 1, field.selection.label())
                        })
                        .collect()
                } else {
                    self.active_wdx_columns(self.active)
                        .into_iter()
                        .map(|(index, column, _)| {
                            format!("wdx{} · {}", index + 1, column.selection.label())
                        })
                        .collect()
                };
                for field in fields {
                    description.push('\n');
                    description.push_str(&field);
                }
                self.dialog(
                    "multiline",
                    Pending::ViewSetup(Setup::Sort),
                    Dialog {
                        title: "Mehrfachsortierung",
                        description: &description,
                        value: &sort,
                        ..Default::default()
                    },
                );
            }
            _ => return false,
        }
        true
    }

    fn choose_view(&mut self, modes: bool) {
        let settings = &self.profile.views;
        let names: Vec<_> = if modes {
            settings.modes.keys().cloned().collect()
        } else {
            settings.columns.keys().cloned().collect()
        };
        let options: Vec<_> = ["Standardansicht", "Neu anlegen"]
            .into_iter()
            .chain(names.iter().map(String::as_str))
            .collect();
        self.dialog("confirm", Pending::ViewSetup(Setup::Choose { modes, names: names.clone() }), Dialog {
            title: if modes { "Ansichtsmodi" } else { "Spaltensätze" },
            description: "Wähle eine gespeicherte Ansicht für das aktive Dateifenster oder lege eine neue an.",
            options: &options, ..Default::default()
        });
    }

    fn manage_view(&mut self, modes: bool, name: String) {
        let title = name.clone();
        self.dialog(
            "confirm",
            Pending::ViewSetup(Setup::Manage { modes, name }),
            Dialog {
                title: &title,
                options: &["Im aktiven Tab anwenden", "Bearbeiten", "Löschen"],
                ..Default::default()
            },
        );
    }

    fn edit_columns(&mut self, name: Option<String>) {
        let columns = name
            .as_ref()
            .and_then(|name| self.profile.views.columns.get(name));
        let mut fields = self.profile.wdx_columns.clone();
        if let Some(columns) = columns {
            for column in columns {
                if let Field::Wdx(field) = &column.field
                    && !fields.contains(field)
                {
                    fields.push(field.clone());
                }
            }
        }
        let mut value = name.clone().unwrap_or_else(|| "Meine Spalten".into());
        if let Some(columns) = columns {
            for column in columns {
                let key = match &column.field {
                    Field::Wdx(field) => format!(
                        "wdx{}",
                        fields
                            .iter()
                            .position(|candidate| candidate == field)
                            .unwrap()
                            + 1
                    ),
                    Field::TcExpression(expression) => format!("tc:{}", expression.source()),
                    Field::Wfx(_) => continue,
                    field => BUILTIN_FIELDS
                        .iter()
                        .find(|(_, candidate)| candidate == field)
                        .unwrap()
                        .0
                        .into(),
                };
                value.push_str(&format!("\n{key} | {} | {}", column.title, column.width));
                if let Some(right_aligned) = column.right_aligned {
                    value.push_str(if right_aligned {
                        " | rechts"
                    } else {
                        " | links"
                    });
                }
            }
        } else {
            value.push_str("\ngröße | Größe | 90\ndatum | Geändert | 130");
        }
        let mut description = String::from(
            "Erste Zeile ist der Name. Danach je Spalte Feld | Titel | Breite in Pixeln | links oder rechts (optional). Felder sind erweiterung, größe, datum, attribute und kommentar. Mit tc: kann ein TC-Ausdruck folgen. Die Namensspalte steht links.",
        );
        for (index, field) in fields.iter().enumerate() {
            description.push_str(&format!(
                "\nwdx{} = {} · {}",
                index + 1,
                field
                    .plugin
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy(),
                field.selection.label()
            ));
        }
        self.dialog(
            "multiline",
            Pending::ViewSetup(Setup::EditColumns { name, fields }),
            Dialog {
                title: "Spaltensatz bearbeiten",
                description: &description,
                value: &value,
                ..Default::default()
            },
        );
    }

    fn edit_mode(&mut self, name: Option<String>) {
        let mode = name
            .as_ref()
            .and_then(|name| self.profile.views.modes.get(name))
            .cloned()
            .unwrap_or_default();
        let view = if mode.inherit_view {
            "beibehalten"
        } else {
            match mode.view {
                TabView::Details => "details",
                TabView::Short => "kurz",
                TabView::Thumbnails => "miniaturen",
                TabView::Comments => "kommentare",
            }
        };
        let value = format!(
            "{}\nAnsicht={view}\nSpalten={}\nSortierung={}",
            name.as_deref().unwrap_or("Meine Ansicht"),
            mode.columns.as_deref().unwrap_or(""),
            sort_text(&mode.sort).replace('\n', ",")
        );
        let sets = self
            .profile
            .views
            .columns
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(", ");
        let description = format!(
            "Für Ansicht sind details, kurz, miniaturen, kommentare oder beibehalten möglich. Ein Spaltensatz benötigt details. Sortierfelder durch Komma trennen. Vorhandene Spaltensätze sind {sets}."
        );
        self.dialog(
            "multiline",
            Pending::ViewSetup(Setup::EditMode(name)),
            Dialog {
                title: "Ansichtsmodus bearbeiten",
                description: &description,
                value: &value,
                ..Default::default()
            },
        );
    }

    fn edit_view_rules(&mut self) {
        let settings = &self.profile.views;
        let mut value = format!("Aktiv={}", if settings.automatic { "ja" } else { "nein" });
        for rule in &settings.rules {
            if let Some(expression) = &rule.tc_expression {
                value.push_str(&format!("\nTC | {} | {expression}", rule.mode));
                continue;
            }
            value.push_str(&format!(
                "\n{} | {} | {} | {} | {} | {} | {}",
                rule.mode,
                rule.scope.key(),
                rule.path,
                rule.contains,
                rule.excludes,
                if rule.any { "oder" } else { "und" },
                rule.minimum_percent
                    .map_or(String::new(), |percent| percent.to_string()),
            ));
        }
        let modes = settings
            .modes
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(", ");
        let description = format!(
            "Je Zeile Modus | Bereich | Pfadmaske | enthält | enthält nicht | und/oder | Mindestanteil in Prozent. Alternativ TC | Modus | Regel. Die erste passende Regel gewinnt. Bereiche alle, lokal, laufwerk, unc, archiv, ftp, wfx, shell, suche, zweig. Vorhandene Modi {modes}"
        );
        self.dialog(
            "multiline",
            Pending::ViewSetup(Setup::EditRules),
            Dialog {
                title: "Automatische Ansichtsregeln",
                description: &description,
                value: &value,
                ..Default::default()
            },
        );
    }

    fn edit_file_colors(&mut self) {
        let settings = &self.profile.views;
        let mut value = String::new();
        for (label, pair) in [("Auswahl", settings.selection), ("Cursor", settings.cursor)] {
            if let Some(pair) = pair {
                value.push_str(&format!(
                    "{label} | #{:06X} | #{:06X}\n",
                    pair.light, pair.dark
                ));
            }
        }
        for rule in &settings.colors {
            value.push_str(&format!(
                "{} | #{:06X} | #{:06X}\n",
                rule.masks, rule.color.light, rule.color.dark
            ));
        }
        self.dialog("multiline", Pending::ViewSetup(Setup::Colors), Dialog {
            title: "Datei- und Auswahlfarben",
            description: "Je Zeile Dateimaske | Farbe hell | Farbe dunkel. Farben als #RRGGBB. Auswahl und Cursor steuern den Hintergrund. Die erste passende Dateimaske setzt die Schriftfarbe. Eine leere Liste verwendet die Standardfarben.",
            value: &value, ..Default::default()
        });
    }

    pub(crate) fn submit_view_setup(
        &mut self,
        setup: Setup,
        value: &str,
        option: usize,
    ) -> io::Result<()> {
        match setup {
            Setup::ChooseWfxColumns(plugin, names) => {
                self.select_wfx_columns(*plugin, &names, option)?;
            }
            Setup::ManageWfxColumns(plugin, name) => {
                self.manage_wfx_columns(*plugin, name, option)?;
            }
            Setup::EditWfxColumns(plugin, name, fields) => {
                self.save_wfx_columns(*plugin, name, &fields, value)?;
            }
            Setup::Choose { modes, names } => match option {
                0 => self.apply_view(Mode::default()),
                1 if modes => self.edit_mode(None),
                1 => self.edit_columns(None),
                _ => {
                    if let Some(name) = names.get(option - 2) {
                        self.manage_view(modes, name.clone());
                    }
                }
            },
            Setup::Manage { modes, name } => match option {
                0 => self.apply_named_view(&name, modes)?,
                1 if modes => self.edit_mode(Some(name)),
                1 => self.edit_columns(Some(name)),
                2 => {
                    let mut settings = self.profile.views.clone();
                    if modes {
                        settings.modes.remove(&name);
                        settings.rules.retain(|rule| rule.mode != name);
                    } else {
                        settings.columns.remove(&name);
                        settings.tc_columns.retain(|_, layout| layout.name != name);
                        for mode in settings.modes.values_mut() {
                            if mode.columns.as_deref() == Some(&name) {
                                mode.columns = None;
                            }
                        }
                    }
                    self.save_view_settings(settings)?;
                    self.choose_view(modes);
                }
                _ => (),
            },
            Setup::EditColumns { name, fields } => {
                let mut lines = value.lines();
                let key = name_line(lines.next(), name.as_deref())?;
                let columns = parse_columns(lines, &fields)?;
                let mut settings = self.profile.views.clone();
                if name.is_none() && settings.columns.contains_key(&key) {
                    return Err(io::Error::other("Dieser Spaltensatz existiert bereits."));
                }
                if settings.columns.get(&key).is_some_and(|previous| {
                    !previous
                        .iter()
                        .map(|column| &column.field)
                        .eq(columns.iter().map(|column| &column.field))
                }) {
                    settings.tc_columns.retain(|_, layout| layout.name != key);
                }
                settings.columns.insert(key, columns);
                self.save_view_settings(settings)?;
                self.choose_view(false);
            }
            Setup::EditMode(name) => {
                let mut lines = value.lines();
                let key = name_line(lines.next(), name.as_deref())?;
                let mut settings = self.profile.views.clone();
                if name.is_none() && settings.modes.contains_key(&key) {
                    return Err(io::Error::other("Dieser Ansichtsmodus existiert bereits."));
                }
                let mut mode = parse_mode(lines, &settings)?;
                mode.colors = name
                    .as_ref()
                    .and_then(|name| settings.modes.get(name))
                    .and_then(|mode| mode.colors);
                settings.modes.insert(key, mode);
                self.save_view_settings(settings)?;
                self.choose_view(true);
            }
            Setup::EditRules => {
                let mut settings = self.profile.views.clone();
                parse_rules(value, &mut settings)?;
                self.save_view_settings(settings)?;
                self.status("Automatische Ansichtsregeln gespeichert.");
            }
            Setup::ImportTc => {
                let path = super::plugins::user_path(value);
                if !path.is_absolute() {
                    return Err(io::Error::other("Gib einen absoluten INI-Pfad ein."));
                }
                let imported = arco_core::view_settings::read_tc_views(
                    &path,
                    super::favorites::tc_path_variables(&path),
                    |token| {
                        self.resolve_wdx_token(token)
                            .ok()
                            .map(|(plugin, selection)| WdxColumn {
                                plugin: plugin.path,
                                selection,
                            })
                    },
                )?;
                if imported.modes.is_empty() && imported.columns.is_empty() {
                    return Err(io::Error::other(
                        "Die INI enthält keine übernehmbaren Spaltensätze oder Ansichtsmodi.",
                    ));
                }
                let description = format!(
                    "{} Spaltensätze, {} Modi und {} Regeln anhängen? {} Einträge oder Optionen konnten nicht vollständig übernommen werden.",
                    imported.columns.len(),
                    imported.modes.len(),
                    imported.rules.len(),
                    imported.skipped
                );
                let list = imported
                    .columns
                    .keys()
                    .map(|name| format!("Spaltensatz · {name}"))
                    .chain(
                        imported
                            .modes
                            .keys()
                            .map(|name| format!("Ansichtsmodus · {name}")),
                    )
                    .collect::<Vec<_>>()
                    .join("\n");
                self.dialog(
                    "review",
                    Pending::ViewSetup(Setup::ApplyTc(Box::new(imported))),
                    Dialog {
                        title: "TC-Ansichten importieren",
                        description: &description,
                        value: &list,
                        ..Default::default()
                    },
                );
            }
            Setup::ApplyTc(imported) => {
                let mut settings = self.profile.views.clone();
                if settings.columns.len() + imported.columns.len() > 128
                    || settings.modes.len() + imported.modes.len() > 128
                    || settings.rules.len() + imported.rules.len() > 128
                {
                    return Err(io::Error::other(
                        "Der Import überschreitet das Limit von 128 Spaltensätzen, Modi oder Regeln.",
                    ));
                }
                let mut column_names = std::collections::BTreeMap::new();
                for (name, columns) in imported.columns {
                    let candidate = import_view_name(&name, &settings.columns);
                    column_names.insert(name, candidate.clone());
                    settings.columns.insert(candidate, columns);
                }
                if !imported.column_layouts.is_empty() {
                    settings.tc_columns = imported.column_layouts;
                    for layout in settings.tc_columns.values_mut() {
                        if let Some(name) = column_names.get(&layout.name) {
                            layout.name.clone_from(name);
                        }
                    }
                }
                let mut names = std::collections::BTreeMap::new();
                for (name, mut mode) in imported.modes {
                    let candidate = import_view_name(&name, &settings.modes);
                    if let Some(columns) = &mut mode.columns
                        && let Some(name) = column_names.get(columns)
                    {
                        columns.clone_from(name);
                    }
                    names.insert(name, candidate.clone());
                    settings.modes.insert(candidate, mode);
                }
                if settings.rules.is_empty() {
                    settings.automatic = imported.automatic.unwrap_or(settings.automatic);
                }
                settings
                    .rules
                    .extend(imported.rules.into_iter().map(|mut rule| {
                        rule.mode = names[&rule.mode].clone();
                        rule
                    }));
                self.save_view_settings(settings)?;
                self.status("TC-Spaltensätze, Ansichtsmodi und Regeln importiert.");
            }
            Setup::Colors => {
                let mut settings = self.profile.views.clone();
                parse_colors(value, &mut settings)?;
                self.save_view_settings(settings)?;
                self.status("Datei- und Auswahlfarben gespeichert.");
            }
            Setup::Sort => {
                let sort = if self.current().wfx.is_some() {
                    parse_sort_in(value, Some(&self.active_wfx_columns(self.active)))?
                } else {
                    let sort = parse_sort(value)?;
                    let columns = self.active_wdx_columns(self.active);
                    if sort.iter().any(|order| matches!(order.key, SortKey::Wdx(index) if !self.current().is_local()
                        || !self.current().detailed() || !columns.iter().any(|(pos, _, _)| *pos == usize::from(index)))) {
                        return Err(io::Error::other("Die angegebene WDX-Spalte fehlt in der lokalen Detailansicht."));
                    }
                    sort
                };
                let hidden = self.profile.show_hidden;
                self.current_mut().set_sort_orders(sort, hidden);
                self.save();
            }
        }
        Ok(())
    }

    pub(crate) fn apply_named_view(&mut self, name: &str, modes: bool) -> io::Result<()> {
        let mode = if modes {
            self.profile.views.modes.get(name).cloned()
        } else {
            self.profile.views.columns.contains_key(name).then(|| Mode {
                columns: Some(name.to_owned()),
                ..Mode::default()
            })
        }
        .ok_or_else(|| io::Error::other("Die gespeicherte Ansicht fehlt."))?;
        self.apply_view(mode);
        Ok(())
    }

    fn apply_view(&mut self, mode: Mode) {
        let side = self.active;
        let hidden = self.profile.show_hidden;
        let tab = self.current_mut();
        let (view, columns) = if mode.inherit_view {
            (tab.view(), tab.column_set().map(str::to_owned))
        } else {
            (mode.view, mode.columns)
        };
        tab.set_view(
            view,
            columns,
            (!mode.sort.is_empty()).then_some(mode.sort),
            hidden,
        );
        tab.options.view_colors = mode.colors;
        if self.current().view_scope.is_none()
            && self.current().is_local()
            && (self.current().shows_comments()
                || self.active_columns(side).is_some_and(|columns| {
                    columns.iter().any(|column| column.field.needs_comment())
                }))
        {
            self.refresh(side);
        }
        self.publish(side);
        self.save();
    }

    fn save_view_settings(&mut self, settings: Settings) -> io::Result<()> {
        if !self.save_profile {
            return Err(io::Error::other("Das Arco-Profil ist nicht verfügbar."));
        }
        let mut profile = self.profile_snapshot();
        profile.views = settings;
        for panel in profile
            .panels
            .iter_mut()
            .chain(profile.tab_groups.values_mut().flatten())
        {
            for tab in &mut panel.tab_options {
                if tab
                    .column_set
                    .as_ref()
                    .is_some_and(|name| !profile.views.columns.contains_key(name))
                {
                    tab.column_set = None;
                }
            }
        }
        profile.save(&self.profile_path)?;
        self.profile = profile;
        self.file_colors = Rc::new(self.profile.views.compile_colors());
        self.apply_color_settings();
        for panel in &mut self.panels {
            for tab in &mut panel.tabs {
                tab.colors = self.file_colors.clone();
                if tab
                    .options
                    .column_set
                    .as_ref()
                    .is_some_and(|name| !self.profile.views.columns.contains_key(name))
                {
                    tab.options.column_set = None;
                }
                tab.manual_view = false;
                let entries = tab.entry_model();
                tab.view_override = automatic_view(
                    &self.profile.views,
                    &tab.location(),
                    tab.view_scope,
                    &entries,
                );
                tab.resort(self.profile.show_hidden);
            }
        }
        self.invalidate_wdx_columns();
        for side in 0..2 {
            let tab = self.panels[side].current();
            if tab.view_scope.is_none()
                && tab.is_local()
                && (tab.shows_comments()
                    || self.active_columns(side).is_some_and(|columns| {
                        columns.iter().any(|column| column.field.needs_comment())
                    }))
            {
                self.refresh(side);
            }
        }
        Ok(())
    }

    pub(super) fn apply_color_settings(&self) {
        let ui = self.ui();
        let colors = ui.global::<Colors>();
        let settings = &self.profile.views;
        colors.set_custom_selection(settings.selection.is_some());
        if let Some(pair) = settings.selection {
            colors.set_selection_light(file_color(pair.light));
            colors.set_selection_dark(file_color(pair.dark));
        }
        colors.set_custom_cursor(settings.cursor.is_some());
        if let Some(pair) = settings.cursor {
            colors.set_cursor_light(file_color(pair.light));
            colors.set_cursor_dark(file_color(pair.dark));
        }
    }
}

fn parse_colors(value: &str, settings: &mut Settings) -> io::Result<()> {
    let parse_rgb = |raw: &str| -> io::Result<u32> {
        let raw = raw.strip_prefix('#').unwrap_or(raw);
        if raw.len() != 6 {
            return Err(io::Error::other("Farben benötigen sechs Hexziffern."));
        }
        u32::from_str_radix(raw, 16).map_err(io::Error::other)
    };
    let mut colors = Vec::new();
    let mut selection = None;
    let mut cursor = None;
    for line in value.lines().map(str::trim).filter(|line| !line.is_empty()) {
        let mut parts = line.rsplitn(3, '|').map(str::trim);
        let dark = parse_rgb(parts.next().unwrap_or_default())?;
        let light = parse_rgb(parts.next().unwrap_or_default())?;
        let rule = parts
            .next()
            .filter(|part| !part.is_empty())
            .ok_or_else(|| {
                io::Error::other("Gib eine Dateimaske oder Auswahl beziehungsweise Cursor an.")
            })?;
        let pair = ColorPair { light, dark };
        if rule.eq_ignore_ascii_case("Auswahl") {
            selection = Some(pair);
        } else if rule.eq_ignore_ascii_case("Cursor") {
            cursor = Some(pair);
        } else {
            arco_core::sync::FileMasks::new(rule)?;
            if colors.len() == 128 {
                return Err(io::Error::other(
                    "Es können bis zu 128 Dateifarbregeln gespeichert werden.",
                ));
            }
            colors.push(FileColor {
                masks: rule.into(),
                color: pair,
            });
        }
    }
    settings.colors = colors;
    settings.selection = selection;
    settings.cursor = cursor;
    Ok(())
}

fn name_line(value: Option<&str>, existing: Option<&str>) -> io::Result<String> {
    let name = value.unwrap_or_default().trim();
    if name.is_empty()
        || name.chars().count() > 80
        || name.contains('|')
        || name.chars().any(char::is_control)
    {
        return Err(io::Error::other(
            "Gib einen Namen mit höchstens 80 Zeichen und ohne | ein.",
        ));
    }
    if existing.is_some_and(|existing| existing != name) {
        return Err(io::Error::other(
            "Lass beim Bearbeiten den Namen unverändert.",
        ));
    }
    Ok(name.into())
}

fn import_view_name<T>(name: &str, existing: &std::collections::BTreeMap<String, T>) -> String {
    if !existing.contains_key(name) {
        return name.into();
    }
    (1..)
        .map(|number| format!("{name} (TC {number})"))
        .find(|candidate| !existing.contains_key(candidate))
        .unwrap()
}

fn parse_columns<'a>(
    lines: impl Iterator<Item = &'a str>,
    fields: &[WdxColumn],
) -> io::Result<Vec<Column>> {
    parse_column_specs(lines, |key| {
        key.strip_prefix("wdx")
            .and_then(|index| index.parse::<usize>().ok()?.checked_sub(1))
            .and_then(|index| fields.get(index))
            .cloned()
            .map(Field::Wdx)
    })
}

fn parse_column_specs<'a>(
    lines: impl Iterator<Item = &'a str>,
    plugin_field: impl Fn(&str) -> Option<Field>,
) -> io::Result<Vec<Column>> {
    let mut columns = Vec::new();
    for line in lines.map(str::trim).filter(|line| !line.is_empty()) {
        let mut parts = line.split('|').map(str::trim);
        let source = parts.next().unwrap_or_default();
        let key = source.to_lowercase();
        let field = if let Some(expression) = source.strip_prefix("tc:") {
            Field::TcExpression(TcColumnExpression::parse(expression)?)
        } else {
            BUILTIN_FIELDS
                .iter()
                .find(|(name, _)| *name == key)
                .map(|(_, field)| field.clone())
                .or_else(|| plugin_field(&key))
                .ok_or_else(|| io::Error::other(format!("Unbekanntes Spaltenfeld „{key}“.")))?
        };
        let title = parts
            .next()
            .filter(|title| !title.is_empty())
            .map_or_else(|| field.label(), str::to_owned);
        let width = match parts.next() {
            Some(width) => width
                .parse::<u16>()
                .ok()
                .filter(|width| (40..=600).contains(width))
                .ok_or_else(|| {
                    io::Error::other("Spaltenbreiten liegen zwischen 40 und 600 Pixeln.")
                })?,
            None => 110,
        };
        let right_aligned = match parts.next() {
            None | Some("") => None,
            Some("links") => Some(false),
            Some("rechts") => Some(true),
            Some(_) => return Err(io::Error::other("Ausrichtung muss links oder rechts sein.")),
        };
        if parts.next().is_some()
            || title.chars().any(char::is_control)
            || title.chars().count() > 80
        {
            return Err(io::Error::other(
                "Gib Feld, Titel und Breite an. Titel sind auf 80 Zeichen begrenzt.",
            ));
        }
        columns.push(Column {
            field,
            title,
            width,
            right_aligned,
        });
        if columns.len() > MAX_COLUMNS {
            return Err(io::Error::other(
                "Ein Satz enthält höchstens 29 zusätzliche Spalten.",
            ));
        }
    }
    if columns.is_empty() {
        return Err(io::Error::other("Füge mindestens eine Spalte hinzu."));
    }
    Ok(columns)
}

fn parse_mode<'a>(lines: impl Iterator<Item = &'a str>, settings: &Settings) -> io::Result<Mode> {
    let mut mode = Mode::default();
    for line in lines.map(str::trim).filter(|line| !line.is_empty()) {
        let (key, value) = line
            .split_once('=')
            .ok_or_else(|| io::Error::other("Erwartet wird Einstellung=Wert."))?;
        let value = value.trim();
        match key.trim().to_lowercase().as_str() {
            "ansicht" => {
                mode.inherit_view = value.eq_ignore_ascii_case("beibehalten");
                mode.view = match value.to_lowercase().as_str() {
                    "beibehalten" | "details" => TabView::Details,
                    "kurz" => TabView::Short,
                    "miniaturen" => TabView::Thumbnails,
                    "kommentare" => TabView::Comments,
                    _ => return Err(io::Error::other("Unbekannte Ansicht.")),
                }
            }
            "spalten" if value.is_empty() => mode.columns = None,
            "spalten" if settings.columns.contains_key(value) => mode.columns = Some(value.into()),
            "spalten" => return Err(io::Error::other("Der Spaltensatz fehlt.")),
            "sortierung" => mode.sort = parse_sort(value)?,
            _ => return Err(io::Error::other(format!("Unbekannte Einstellung „{key}“."))),
        }
    }
    if (mode.columns.is_some()
        || mode
            .sort
            .iter()
            .any(|order| matches!(order.key, SortKey::Wdx(_))))
        && mode.view != TabView::Details
    {
        return Err(io::Error::other(
            "Spaltensätze benötigen die Detailansicht.",
        ));
    }
    Ok(mode)
}

fn parse_rules(value: &str, settings: &mut Settings) -> io::Result<()> {
    let mut lines = value
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'));
    let automatic = match lines.next().unwrap_or_default().to_lowercase().as_str() {
        "aktiv=ja" => true,
        "aktiv=nein" => false,
        _ => {
            return Err(io::Error::other(
                "Die erste Zeile lautet Aktiv=ja oder Aktiv=nein.",
            ));
        }
    };
    let mut rules = Vec::new();
    for line in lines {
        if rules.len() == 128 {
            return Err(io::Error::other("Erlaubt sind höchstens 128 Regeln."));
        }
        if let Some(("TC", rest)) = line.split_once('|').map(|(head, rest)| (head.trim(), rest)) {
            let (mode, expression) = rest
                .split_once('|')
                .ok_or_else(|| io::Error::other("Erwartet wird TC | Modus | Regel."))?;
            let mode = mode.trim();
            if !settings.modes.contains_key(mode) {
                return Err(io::Error::other(format!(
                    "Der Ansichtsmodus „{mode}“ fehlt."
                )));
            }
            let rule = Rule {
                mode: mode.into(),
                tc_expression: Some(expression.trim().into()),
                ..Rule::default()
            };
            rule.validate()?;
            rules.push(rule);
            continue;
        }
        let mut parts = line.split('|').map(str::trim);
        let mode = parts.next().unwrap_or_default();
        if !settings.modes.contains_key(mode) {
            return Err(io::Error::other(format!(
                "Der Ansichtsmodus „{mode}“ fehlt."
            )));
        }
        let scope = match parts.next().unwrap_or_default() {
            "" => Scope::Any,
            scope => Scope::parse(scope)
                .ok_or_else(|| io::Error::other(format!("Unbekannter Bereich „{scope}“.")))?,
        };
        let path = parts.next().unwrap_or_default().into();
        let contains = parts.next().unwrap_or_default().into();
        let excludes = parts.next().unwrap_or_default().into();
        let any = match parts.next().unwrap_or_default().to_lowercase().as_str() {
            "" | "und" => false,
            "oder" => true,
            _ => {
                return Err(io::Error::other(
                    "Regelbedingungen lassen sich mit und oder oder verknüpfen.",
                ));
            }
        };
        let minimum_percent = match parts.next().unwrap_or_default() {
            "" => None,
            number => Some(number.parse().map_err(|_| {
                io::Error::other("Der Mindestanteil muss eine ganze Prozentzahl sein.")
            })?),
        };
        let rule = Rule {
            mode: mode.into(),
            scope,
            path,
            contains,
            excludes,
            any,
            minimum_percent,
            tc_expression: None,
        };
        if parts.next().is_some() {
            return Err(io::Error::other(
                "Erlaubt sind höchstens 128 Regeln mit je sieben Feldern.",
            ));
        }
        rule.validate()?;
        rules.push(rule);
    }
    settings.automatic = automatic;
    settings.rules = rules;
    Ok(())
}

fn sort_text(orders: &[SortOrder]) -> String {
    sort_text_in(orders, false)
}

fn sort_text_in(orders: &[SortOrder], wfx: bool) -> String {
    orders
        .iter()
        .filter_map(|order| {
            if let SortKey::Wdx(index) = order.key {
                return Some(format!(
                    "{}{}{}",
                    if order.descending {
                        "-"
                    } else if wfx {
                        "+"
                    } else {
                        ""
                    },
                    if wfx { "wfx" } else { "wdx" },
                    usize::from(index) + 1
                ));
            }
            SORT_KEYS
                .iter()
                .find(|(_, key)| *key == order.key)
                .map(|(label, _)| format!("{}{label}", if order.descending { "-" } else { "" }))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn parse_sort(value: &str) -> io::Result<Vec<SortOrder>> {
    parse_sort_in(value, None)
}

fn parse_sort_in(value: &str, wfx: Option<&[WfxField]>) -> io::Result<Vec<SortOrder>> {
    let mut orders = Vec::new();
    for part in value
        .split([',', '\n'])
        .map(str::trim)
        .filter(|part| !part.is_empty())
    {
        let mut descending = part.starts_with('-');
        let field = part.strip_prefix(['-', '+']).unwrap_or(part).to_lowercase();
        let key = SORT_KEYS
            .iter()
            .find(|(name, _)| *name == field)
            .map(|(_, key)| *key)
            .or_else(|| {
                field
                    .strip_prefix(if wfx.is_some() { "wfx" } else { "wdx" })
                    .and_then(|number| number.parse::<u8>().ok())
                    .filter(|number| (1..=MAX_COLUMNS as u8).contains(number))
                    .map(|number| SortKey::Wdx(number - 1))
            })
            .ok_or_else(|| io::Error::other(format!("Unbekanntes Sortierfeld „{field}“.")))?;
        if let (SortKey::Wdx(index), Some(fields)) = (key, wfx) {
            let field = fields.get(usize::from(index)).ok_or_else(|| {
                io::Error::other("Die angegebene WFX-Spalte fehlt in der Detailansicht.")
            })?;
            if !part.starts_with(['-', '+']) {
                descending = field.descending;
            }
        }
        if orders.iter().any(|order: &SortOrder| {
            order.key == key || order.key == SortKey::Unsorted || key == SortKey::Unsorted
        }) {
            return Err(io::Error::other(
                "Sortierfelder dürfen einmal vorkommen. Unsortiert steht allein.",
            ));
        }
        orders.push(SortOrder { key, descending });
    }
    Ok(orders)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::panel::Tab;
    use arco_core::files::Kind;
    use arco_core::plugin::WdxField;
    use std::path::PathBuf;

    #[test]
    fn wfx_multisort_uses_plugin_defaults_and_explicit_directions() {
        let fields = [
            WfxField {
                selection: WdxField {
                    field: "Rank".into(),
                    unit: None,
                },
                descending: true,
            },
            WfxField {
                selection: WdxField {
                    field: "Label".into(),
                    unit: None,
                },
                descending: false,
            },
        ];
        let sort = parse_sort_in("wfx1, -größe\n-wfx2", Some(&fields)).unwrap();
        assert_eq!(sort_text_in(&sort, true), "-wfx1\n-größe\n-wfx2");
        let sort = parse_sort_in("+wfx1,wfx2", Some(&fields)).unwrap();
        let text = sort_text_in(&sort, true);
        assert_eq!(text, "+wfx1\n+wfx2");
        assert_eq!(parse_sort_in(&text, Some(&fields)).unwrap(), sort);
        for invalid in ["wfx3", "wdx1", "wfx1,-wfx1", "unsortiert,wfx2"] {
            assert!(parse_sort_in(invalid, Some(&fields)).is_err(), "{invalid}");
        }
        assert!(parse_sort("wfx1").is_err());
    }

    #[test]
    fn colors_respect_mask_exclusions_and_light_dark_pairs() {
        let mut settings = Settings::default();
        parse_colors(
            "Auswahl | #AED0C9 | #19403D\n*.txt | ignoriert* | #284634 | #B2F09E",
            &mut settings,
        )
        .unwrap();
        assert_eq!(
            settings.selection,
            Some(ColorPair {
                light: 0xaed0c9,
                dark: 0x19403d
            })
        );
        let colors = settings.compile_colors();
        assert_eq!(colors.len(), 1);
        assert!(colors[0].matches(std::ffi::OsStr::new("Datei.txt")));
        assert!(!colors[0].matches(std::ffi::OsStr::new("ignoriert.txt")));
        assert!(parse_colors("* | #XYZ | #000000", &mut settings).is_err());
    }

    #[test]
    fn named_columns_keep_wdx_field_identity_and_custom_order() {
        let field = WdxColumn {
            plugin: PathBuf::from("Details.wdx64"),
            selection: WdxField {
                field: "Title".into(),
                unit: Some("localized".into()),
            },
        };
        let columns = parse_columns(
            ["wdx1 | Titel | 162", "erweiterung | Erw. | 56", "größe"].into_iter(),
            std::slice::from_ref(&field),
        )
        .unwrap();
        let mut settings = Settings::default();
        settings.columns.insert("Metadaten".into(), columns);
        let mode = parse_mode(
            [
                "Ansicht=details",
                "Spalten=Metadaten",
                "Sortierung=erweiterung,-größe",
            ]
            .into_iter(),
            &settings,
        )
        .unwrap();
        settings.modes.insert("Medien".into(), mode);
        parse_rules(
            "Aktiv=ja\nMedien | lokal | C:/Musik/** | *.mp3 | *.tmp",
            &mut settings,
        )
        .unwrap();
        let fixture = tempfile::tempdir().unwrap();
        let profile = fixture.path().join("profile.json");
        let mut saved = arco_core::profile::Profile::default();
        saved.views = settings;
        saved.save(&profile).unwrap();
        let loaded = arco_core::profile::Profile::load(&profile).unwrap().views;
        let columns = &loaded.columns["Metadaten"];
        assert_eq!(columns[0].field, Field::Wdx(field));
        assert_eq!(columns[0].width, 162);
        assert_eq!(columns[1].field, Field::Extension);
        assert_eq!(
            loaded.modes["Medien"].sort[1],
            SortOrder {
                key: SortKey::Size,
                descending: true
            }
        );
        assert!(parse_sort("unsortiert,-größe").is_err());
    }

    #[test]
    fn automatic_mode_leaves_the_manual_tab_view_and_original_row_order_available() {
        let mut tab = Tab::new(PathBuf::from("C:/test"));
        tab.options.column_set = Some("Basis".into());
        tab.view_override = Some(Mode {
            columns: Some("Auto".into()),
            sort: parse_sort("erweiterung,-größe").unwrap(),
            ..Mode::default()
        });
        let file = |name: &str, size| {
            Entry::from_remote(PathBuf::from(name), Kind::File, Some(size), None)
        };
        tab.populate(
            vec![file("a.txt", 1), file("b.txt", 3), file("a.csv", 9)],
            true,
            String::new(),
        );
        let names = |tab: &Tab| {
            tab.entry_model()
                .iter()
                .map(|entry| entry.name.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(tab.column_set(), Some("Auto"));
        assert_eq!(names(&tab), ["a.csv", "b.txt", "a.txt"]);
        tab.view_override = None;
        tab.set_sort_orders(parse_sort("unsortiert").unwrap(), true);
        assert_eq!(tab.column_set(), Some("Basis"));
        assert_eq!(names(&tab), ["a.txt", "b.txt", "a.csv"]);
    }
}
