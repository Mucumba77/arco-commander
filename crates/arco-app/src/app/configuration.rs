//! Typed drafts for view settings and internal associations. Editing a row never
//! writes a profile or launches a program; only the final save publishes changes.
use super::associations::Setup as Association;
use super::view_setup::Setup as View;
use super::{App, associations, navigation, strings, view_setup};
use crate::{
    ConfigurationData, ConfigurationForm, ConfigurationItem, ConfigurationKind, NavigationData,
    NavigationForm, NavigationRow,
    commands::{Dialog, Pending},
    display_path,
    locale::tr,
};
use arco_core::{
    files::{SortKey, SortOrder},
    profile::{FileAssociation, TabView},
    view_settings::{Column, Field, MAX_COLUMNS, Mode, Rule, Scope, Settings, TcColumnExpression},
};
use slint::{ModelRc, VecModel};
use std::io;

const SCOPES: [Scope; 10] = [
    Scope::Any,
    Scope::Local,
    Scope::Drive,
    Scope::Unc,
    Scope::Archive,
    Scope::Ftp,
    Scope::Wfx,
    Scope::Shell,
    Scope::Search,
    Scope::Branch,
];
const SORTS: [(SortKey, &str); 7] = [
    (SortKey::Name, "Name"),
    (SortKey::Extension, "Erweiterung"),
    (SortKey::Size, "Größe"),
    (SortKey::Modified, "Datum"),
    (SortKey::Attributes, "Attribute"),
    (SortKey::Comment, "Kommentar"),
    (SortKey::Unsorted, "Unsortiert"),
];

pub(crate) struct Draft {
    kind: ConfigurationKind,
    form: ConfigurationForm,
    rows: Vec<ConfigurationItem>,
    original_form: ConfigurationForm,
    original_rows: Vec<ConfigurationItem>,
    selected: Option<usize>,
    fields: Vec<Field>,
    sort_keys: Vec<(SortKey, String)>,
    sort_defaults: Vec<(SortKey, bool)>,
    sets: Vec<String>,
    modes: Vec<String>,
    original_name: Option<String>,
    title: String,
    description: String,
}

fn text_row(label: impl Into<String>, detail: impl Into<String>, choice: usize) -> NavigationRow {
    NavigationRow {
        label: label.into().into(),
        detail: detail.into().into(),
        choice: choice as i32,
        kind: 4,
    }
}

impl Draft {
    fn new(kind: ConfigurationKind, source: &Dialog<'_>, settings: &Settings) -> Self {
        Self {
            kind,
            form: Default::default(),
            rows: Vec::new(),
            original_form: Default::default(),
            original_rows: Vec::new(),
            selected: None,
            fields: [
                Field::Extension,
                Field::Size,
                Field::Modified,
                Field::Attributes,
                Field::Comment,
            ]
            .into(),
            sort_keys: SORTS.iter().map(|(k, l)| (*k, tr(l).into())).collect(),
            sort_defaults: Vec::new(),
            sets: settings.columns.keys().cloned().collect(),
            modes: settings.modes.keys().cloned().collect(),
            original_name: None,
            title: tr(source.title).into(),
            description: String::new(),
        }
    }
    fn snapshot(&mut self) {
        self.original_form = self.form.clone();
        self.original_rows = self.rows.clone();
        self.selected = (!self.rows.is_empty()).then_some(0);
    }
    fn merge(&mut self, form: ConfigurationForm, item: ConfigurationItem) {
        self.form = form;
        if let Some(row) = self.selected.and_then(|i| self.rows.get_mut(i)) {
            *row = item;
        }
    }
    fn dirty(&self) -> bool {
        self.form != self.original_form || self.rows != self.original_rows
    }
    fn default_row(&self) -> ConfigurationItem {
        match self.kind {
            ConfigurationKind::Columns => ConfigurationItem {
                title: tr("Erweiterung").into(),
                width: "110".into(),
                ..Default::default()
            },
            ConfigurationKind::Colors => ConfigurationItem {
                contains: "*.txt".into(),
                light: "#154B60".into(),
                dark: "#9AD9EB".into(),
                ..Default::default()
            },
            _ => Default::default(),
        }
    }
    fn change(&mut self, action: i32, index: i32) -> io::Result<()> {
        match action {
            0 => {
                let index = usize::try_from(index)
                    .ok()
                    .filter(|&i| i < self.rows.len())
                    .ok_or_else(|| io::Error::other("Der Eintrag fehlt."))?;
                self.selected = Some(index);
            }
            1 => {
                if self.kind == ConfigurationKind::Rules && self.modes.is_empty() {
                    return Err(io::Error::other("Lege zuerst einen Ansichtsmodus an."));
                }
                let limit = match self.kind {
                    ConfigurationKind::Columns => MAX_COLUMNS,
                    ConfigurationKind::Mode | ConfigurationKind::Sort => {
                        MAX_COLUMNS + SORTS.len() - 1
                    }
                    ConfigurationKind::Colors => 130,
                    _ => 128,
                };
                if self.rows.len() >= limit {
                    return Err(io::Error::other(format!(
                        "{} {limit}.",
                        tr("Maximale Anzahl Einträge:")
                    )));
                }
                self.rows.push(self.default_row());
                self.selected = Some(self.rows.len() - 1);
            }
            2 => {
                if let Some(i) = self.selected.filter(|&i| i < self.rows.len()) {
                    self.rows.remove(i);
                    self.selected = if self.rows.is_empty() {
                        None
                    } else {
                        Some(i.min(self.rows.len() - 1))
                    };
                }
            }
            3 | 4 => {
                if let Some(i) = self.selected.filter(|&i| i < self.rows.len()) {
                    let next = if action == 3 {
                        i.saturating_sub(1)
                    } else {
                        (i + 1).min(self.rows.len() - 1)
                    };
                    self.rows.swap(i, next);
                    self.selected = Some(next);
                }
            }
            5 => {
                let field = usize::try_from(index)
                    .ok()
                    .filter(|&i| i < self.sort_keys.len())
                    .ok_or_else(|| io::Error::other("Wähle Sortierfeld und Richtung."))?;
                if let Some(row) = self.selected.and_then(|i| self.rows.get_mut(i)) {
                    row.field = index;
                    if let Some((_, descending)) = self
                        .sort_defaults
                        .iter()
                        .find(|(key, _)| *key == self.sort_keys[field].0)
                    {
                        row.direction = i32::from(*descending);
                    }
                }
            }
            _ => return Err(io::Error::other("Unbekannte Bearbeitungsaktion.")),
        }
        Ok(())
    }
    fn set_columns(&mut self, columns: &[Column], fields: impl IntoIterator<Item = Field>) {
        for field in fields
            .into_iter()
            .chain(columns.iter().map(|c| c.field.clone()))
        {
            if !matches!(field, Field::TcExpression(_)) && !self.fields.contains(&field) {
                self.fields.push(field);
            }
        }
        self.rows = columns
            .iter()
            .map(|column| {
                let (field, expression) = match &column.field {
                    Field::TcExpression(expr) => (self.fields.len(), expr.source().to_owned()),
                    field => (
                        self.fields.iter().position(|f| f == field).unwrap(),
                        String::new(),
                    ),
                };
                ConfigurationItem {
                    field: field as i32,
                    expression: expression.into(),
                    title: column.title.clone().into(),
                    width: column.width.to_string().into(),
                    alignment: match column.right_aligned {
                        None => 0,
                        Some(false) => 1,
                        Some(true) => 2,
                    },
                    ..Default::default()
                }
            })
            .collect();
    }
    fn set_sorts(&mut self, orders: &[SortOrder]) {
        for order in orders {
            if !self.sort_keys.iter().any(|(key, _)| *key == order.key) {
                let label = match order.key {
                    SortKey::Wdx(i) => format!(
                        "{} {}",
                        tr("Nicht verfügbares Sortierfeld"),
                        usize::from(i) + 1
                    ),
                    _ => tr("Nicht verfügbares Sortierfeld").into(),
                };
                self.sort_keys.push((order.key, label));
            }
        }
        self.rows = orders
            .iter()
            .map(|order| ConfigurationItem {
                field: self
                    .sort_keys
                    .iter()
                    .position(|(k, _)| *k == order.key)
                    .unwrap_or(0) as i32,
                direction: i32::from(order.descending),
                ..Default::default()
            })
            .collect();
    }
    fn row_label(&self, item: &ConfigurationItem, i: usize) -> NavigationRow {
        let (label, detail) = match self.kind {
            ConfigurationKind::Columns => (
                item.title.to_string(),
                format!("{} · {} px", self.field_label(item.field), item.width),
            ),
            ConfigurationKind::Rules => (
                self.modes
                    .get(item.mode.max(0) as usize)
                    .cloned()
                    .unwrap_or_else(|| tr("Ansichtsmodus wählen").into()),
                if item.tc {
                    format!("TC · {}", item.expression)
                } else {
                    format!(
                        "{} · {} · {}",
                        SCOPES
                            .get(item.scope.max(0) as usize)
                            .unwrap_or(&Scope::Any)
                            .key(),
                        item.path,
                        item.contains
                    )
                },
            ),
            ConfigurationKind::Colors => (
                item.contains.to_string(),
                format!("{} / {}", item.light, item.dark),
            ),
            ConfigurationKind::Association => {
                (self.form.name.to_string(), item.contains.to_string())
            }
            _ => (
                self.sort_keys
                    .get(item.field.max(0) as usize)
                    .map(|(_, l)| l.clone())
                    .unwrap_or_default(),
                tr(if item.direction == 0 {
                    "Aufsteigend"
                } else {
                    "Absteigend"
                })
                .into(),
            ),
        };
        text_row(format!("{} · {label}", i + 1), detail, i)
    }
    fn field_label(&self, index: i32) -> String {
        self.fields
            .get(index.max(0) as usize)
            .map(field_label)
            .unwrap_or_else(|| tr("TC-Spaltenausdruck").into())
    }
    fn data(&self, notice: &str, revision: i32) -> ConfigurationData {
        let fields = if matches!(self.kind, ConfigurationKind::Sort | ConfigurationKind::Mode) {
            self.sort_keys.iter().map(|(_, l)| l.clone()).collect()
        } else {
            self.fields
                .iter()
                .map(field_label)
                .chain(std::iter::once(tr("TC-Spaltenausdruck").into()))
                .collect::<Vec<_>>()
        };
        ConfigurationData {
            kind: self.kind,
            title: self.title.clone().into(),
            description: self.description.clone().into(),
            form: self.form.clone(),
            item: self
                .selected
                .and_then(|i| self.rows.get(i))
                .cloned()
                .unwrap_or_default(),
            rows: ModelRc::new(VecModel::from(
                self.rows
                    .iter()
                    .enumerate()
                    .map(|(i, r)| self.row_label(r, i))
                    .collect::<Vec<_>>(),
            )),
            selected: self.selected.map_or(-1, |i| i as i32),
            fields: strings(fields),
            sets: strings(
                std::iter::once(tr("Standardspalten").to_owned()).chain(self.sets.iter().cloned()),
            ),
            modes: strings(self.modes.iter().cloned()),
            read_only_name: self.original_name.is_some(),
            dirty: self.dirty(),
            notice: notice.into(),
            revision,
        }
    }
    fn columns(&mut self) -> io::Result<Vec<Column>> {
        if self.rows.len() > MAX_COLUMNS {
            return Err(io::Error::other(
                "Ein Satz enthält höchstens 29 zusätzliche Spalten.",
            ));
        }
        if self.rows.is_empty() {
            return Err(io::Error::other("Füge mindestens eine Spalte hinzu."));
        }
        let mut columns = Vec::new();
        for (i, item) in self.rows.iter().enumerate() {
            let result = (|| {
                let field = if item.field as usize == self.fields.len() {
                    Field::TcExpression(TcColumnExpression::parse(&item.expression)?)
                } else {
                    self.fields
                        .get(usize::try_from(item.field).unwrap_or(usize::MAX))
                        .cloned()
                        .ok_or_else(|| io::Error::other("Wähle ein gültiges Spaltenfeld."))?
                };
                let width = item
                    .width
                    .trim()
                    .parse::<u16>()
                    .ok()
                    .filter(|v| (40..=600).contains(v))
                    .ok_or_else(|| {
                        io::Error::other("Spaltenbreiten liegen zwischen 40 und 600 Pixeln.")
                    })?;
                let title = if item.title.trim().is_empty() {
                    field.label()
                } else {
                    item.title.trim().to_owned()
                };
                if title.chars().count() > 80 || title.chars().any(char::is_control) {
                    return Err(io::Error::other(
                        "Spaltentitel benötigen höchstens 80 Zeichen ohne Steuerzeichen.",
                    ));
                }
                let alignment = match item.alignment {
                    0 => None,
                    1 => Some(false),
                    2 => Some(true),
                    _ => return Err(io::Error::other("Wähle eine gültige Ausrichtung.")),
                };
                Ok(Column {
                    title,
                    field,
                    width,
                    right_aligned: alignment,
                })
            })();
            match result {
                Ok(column) => columns.push(column),
                Err(error) => {
                    self.selected = Some(i);
                    return Err(entry_error(i, error));
                }
            }
        }
        Ok(columns)
    }
    fn sorts(&mut self) -> io::Result<Vec<SortOrder>> {
        let mut orders: Vec<SortOrder> = Vec::new();
        for (i, item) in self.rows.iter().enumerate() {
            let key = self
                .sort_keys
                .get(usize::try_from(item.field).unwrap_or(usize::MAX))
                .map(|(key, _)| *key);
            if key.is_none() || !matches!(item.direction, 0 | 1) {
                self.selected = Some(i);
                return Err(entry_error(
                    i,
                    io::Error::other("Wähle Sortierfeld und Richtung."),
                ));
            }
            let key = key.unwrap();
            if orders.iter().any(|order| {
                order.key == key || order.key == SortKey::Unsorted || key == SortKey::Unsorted
            }) {
                self.selected = Some(i);
                return Err(entry_error(
                    i,
                    io::Error::other(
                        "Sortierfelder dürfen einmal vorkommen. Unsortiert steht allein.",
                    ),
                ));
            }
            orders.push(SortOrder {
                key,
                descending: item.direction == 1,
            });
        }
        Ok(orders)
    }
    fn rules(&mut self) -> io::Result<Vec<Rule>> {
        if self.rows.len() > 128 {
            return Err(io::Error::other("Erlaubt sind höchstens 128 Regeln."));
        }
        let mut rules = Vec::new();
        for (i, item) in self.rows.iter().enumerate() {
            let result = (|| {
                let mode = self
                    .modes
                    .get(usize::try_from(item.mode).unwrap_or(usize::MAX))
                    .cloned()
                    .ok_or_else(|| io::Error::other("Lege zuerst einen Ansichtsmodus an."))?;
                let rule = if item.tc {
                    Rule {
                        mode,
                        tc_expression: Some(item.expression.trim().into()),
                        ..Default::default()
                    }
                } else {
                    for value in [&item.path, &item.contains, &item.excludes, &item.minimum] {
                        one_line(value)?;
                    }
                    Rule {
                        mode,
                        scope: *SCOPES
                            .get(usize::try_from(item.scope).unwrap_or(usize::MAX))
                            .ok_or_else(|| {
                                io::Error::other("Wähle einen gültigen Geltungsbereich.")
                            })?,
                        path: item.path.trim().into(),
                        contains: item.contains.trim().into(),
                        excludes: item.excludes.trim().into(),
                        any: item.any,
                        minimum_percent: if item.minimum.trim().is_empty() {
                            None
                        } else {
                            Some(item.minimum.trim().parse::<u8>().map_err(|_| {
                                io::Error::other(
                                    "Der Mindestanteil muss eine ganze Prozentzahl sein.",
                                )
                            })?)
                        },
                        tc_expression: None,
                    }
                };
                rule.validate()?;
                Ok(rule)
            })();
            match result {
                Ok(rule) => rules.push(rule),
                Err(error) => {
                    self.selected = Some(i);
                    return Err(entry_error(i, error));
                }
            }
        }
        Ok(rules)
    }
}

fn field_label(field: &Field) -> String {
    match field {
        Field::Wdx(column) => format!(
            "WDX · {} · {}",
            column
                .plugin
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy(),
            column.selection.label()
        ),
        Field::Wfx(field) => format!("WFX · {}", field.selection.label()),
        _ => tr(&field.label()).into(),
    }
}
fn entry_error(i: usize, error: io::Error) -> io::Error {
    io::Error::other(format!("{} {}: {error}", tr("Eintrag"), i + 1))
}
fn one_line(value: &str) -> io::Result<()> {
    if value.chars().any(char::is_control) {
        Err(io::Error::other(
            "Ein einzeiliges Feld enthält Steuerzeichen.",
        ))
    } else {
        Ok(())
    }
}

impl App {
    pub(crate) fn present_configuration(
        &mut self,
        pending: &Pending,
        source: &Dialog<'_>,
    ) -> Option<&'static str> {
        let settings = &self.profile.views;
        let mut list = NavigationData {
            title: tr(source.title).into(),
            description: tr(source.description).into(),
            primary: tr("Anwenden").into(),
            ..Default::default()
        };
        let mut rows = Vec::new();
        let draft = match pending {
            Pending::ViewSetup(View::Choose { modes, names }) => {
                list.manager = 8;
                list.entry_offset = 2;
                list.can_import = true;
                list.delete_note=tr(if *modes {"Beim Entfernen eines Ansichtsmodus werden auch seine automatischen Regeln entfernt."}else{"Beim Entfernen eines Spaltensatzes werden zugehörige Ansichten auf Standardspalten zurückgesetzt."}).into();
                rows.push(text_row(
                    tr("Standardansicht"),
                    tr("Im aktiven Tab anwenden"),
                    0,
                ));
                rows.extend(names.iter().enumerate().map(|(i, name)| {
                    text_row(
                        name.clone(),
                        if *modes {
                            settings.modes.get(name).map(mode_label).unwrap_or_default()
                        } else {
                            settings
                                .columns
                                .get(name)
                                .map(|cols| {
                                    cols.iter()
                                        .map(|c| c.title.as_str())
                                        .collect::<Vec<_>>()
                                        .join(" · ")
                                })
                                .unwrap_or_default()
                        },
                        i + 2,
                    )
                }));
                None
            }
            Pending::ViewSetup(View::ChooseWfxColumns(plugin, names)) => {
                list.manager = 8;
                list.entry_offset = 3;
                rows.push(text_row(tr("Standardansicht"), plugin.name.clone(), 0));
                rows.push(text_row(
                    tr("Plugin-Standardansicht"),
                    plugin.name.clone(),
                    2,
                ));
                rows.extend(
                    names
                        .iter()
                        .enumerate()
                        .map(|(i, name)| text_row(name.clone(), plugin.name.clone(), i + 3)),
                );
                None
            }
            Pending::ViewSetup(View::Manage { .. } | View::ManageWfxColumns(..)) => {
                list.manager = 5;
                list.primary = tr("Auswählen").into();
                rows.extend(
                    source
                        .options
                        .iter()
                        .enumerate()
                        .map(|(i, l)| text_row(tr(l), "", i)),
                );
                None
            }
            Pending::Associations(Association::Choose) => {
                list.manager = 9;
                list.primary = tr("Bearbeiten").into();
                list.description=tr("Die erste passende Standardaktion gewinnt. Reihenfolge ändern oder Aktionen bearbeiten; Speichern startet kein Programm.").into();
                rows.extend(self.profile.associations.iter().enumerate().map(|(i, a)| {
                    text_row(
                        a.name.clone(),
                        format!(
                            "{} · {} · {}",
                            a.masks,
                            display_path(&a.program),
                            tr(if a.default_open {
                                "Standardaktion"
                            } else {
                                "Zusätzliche Aktion"
                            })
                        ),
                        i + 1,
                    )
                }));
                None
            }
            Pending::Associations(Association::Manage(_)) => {
                list.manager = 5;
                list.primary = tr("Auswählen").into();
                rows.extend(
                    source
                        .options
                        .iter()
                        .enumerate()
                        .map(|(i, l)| text_row(tr(l), "", i)),
                );
                None
            }
            Pending::Associations(Association::Verbs(path, verbs)) => {
                list.primary = tr("Ausführen").into();
                rows.extend(verbs.iter().enumerate().map(|(i, a)| {
                    text_row(
                        a.name.clone(),
                        format!("{} → {}", display_path(path), display_path(&a.program)),
                        i,
                    )
                }));
                None
            }
            Pending::ViewSetup(View::ImportTc)
            | Pending::Associations(Association::Shortcut { .. }) => {
                list.mode = 3;
                list.primary = tr(if matches!(pending, Pending::ViewSetup(_)) {
                    "Vorschau laden"
                } else {
                    "Erstellen"
                })
                .into();
                list.first_label = tr(source.placeholder).into();
                list.initial = NavigationForm {
                    name: source.value.into(),
                    ..Default::default()
                };
                list.file_mode = i32::from(matches!(pending, Pending::ViewSetup(_)));
                self.install_navigation(list, rows);
                return Some("navigation-editor");
            }
            Pending::ViewSetup(View::ApplyTc(imported)) => {
                list.review = true;
                list.primary = tr("Übernehmen").into();
                rows.extend(imported.columns.iter().map(|(name, c)| {
                    text_row(
                        format!("{} · {name}", tr("Spaltensatz")),
                        c.iter()
                            .map(|c| c.title.as_str())
                            .collect::<Vec<_>>()
                            .join(" · "),
                        0,
                    )
                }));
                rows.extend(imported.modes.iter().map(|(name, m)| {
                    text_row(
                        format!("{} · {name}", tr("Ansichtsmodus")),
                        mode_label(m),
                        0,
                    )
                }));
                rows.extend(imported.rules.iter().enumerate().map(|(i, r)| {
                    text_row(
                        format!("{} {} · {}", tr("Regel"), i + 1, r.mode),
                        r.tc_expression.clone().unwrap_or_else(|| r.path.clone()),
                        0,
                    )
                }));
                None
            }
            Pending::ViewSetup(View::EditColumns { name, fields }) => {
                let mut d = Draft::new(ConfigurationKind::Columns, source, settings);
                d.original_name = name.clone();
                d.form.name = name
                    .clone()
                    .unwrap_or_else(|| tr("Meine Spalten").into())
                    .into();
                let columns = name
                    .as_ref()
                    .and_then(|n| settings.columns.get(n))
                    .cloned()
                    .unwrap_or_else(default_columns);
                d.set_columns(&columns, fields.iter().cloned().map(Field::Wdx));
                d.description=tr("Spalten einzeln bearbeiten und sortieren. Plugin-Felder und importierte Ausdrücke bleiben vollständig erhalten.").into();
                Some(d)
            }
            Pending::ViewSetup(View::EditWfxColumns(plugin, name, fields)) => {
                let mut d = Draft::new(ConfigurationKind::Columns, source, settings);
                d.original_name = name.clone();
                d.form.name = name
                    .clone()
                    .unwrap_or_else(|| tr("Meine WFX-Spalten").into())
                    .into();
                let columns = name
                    .as_ref()
                    .and_then(|n| settings.wfx_columns.get(&plugin.path)?.sets.get(n))
                    .cloned()
                    .unwrap_or_else(|| {
                        fields
                            .first()
                            .map(|f| {
                                vec![Column {
                                    field: Field::Wfx(f.clone()),
                                    title: f.selection.label(),
                                    width: 150,
                                    right_aligned: None,
                                }]
                            })
                            .unwrap_or_else(default_columns)
                    });
                d.set_columns(&columns, fields.iter().cloned().map(Field::Wfx));
                d.description = plugin.name.clone();
                Some(d)
            }
            Pending::ViewSetup(View::EditMode(name)) => {
                let mut d = Draft::new(ConfigurationKind::Mode, source, settings);
                d.original_name = name.clone();
                d.form.name = name
                    .clone()
                    .unwrap_or_else(|| tr("Meine Ansicht").into())
                    .into();
                let mode = name
                    .as_ref()
                    .and_then(|n| settings.modes.get(n))
                    .cloned()
                    .unwrap_or_default();
                d.form.view = if mode.inherit_view {
                    4
                } else {
                    match mode.view {
                        TabView::Details => 0,
                        TabView::Short => 1,
                        TabView::Thumbnails => 2,
                        TabView::Comments => 3,
                    }
                };
                d.form.column = mode
                    .columns
                    .as_ref()
                    .and_then(|name| d.sets.iter().position(|n| n == name))
                    .map_or(0, |i| i as i32 + 1);
                d.sort_keys.extend(
                    (0..MAX_COLUMNS).map(|i| (SortKey::Wdx(i as u8), format!("WDX {}", i + 1))),
                );
                d.set_sorts(&mode.sort);
                d.description=tr("Darstellung, Spaltensatz und Sortierung gemeinsam festlegen. Importierte Farben bleiben erhalten.").into();
                Some(d)
            }
            Pending::ViewSetup(View::EditRules) => {
                let mut d = Draft::new(ConfigurationKind::Rules, source, settings);
                d.form.automatic = settings.automatic;
                d.rows = settings
                    .rules
                    .iter()
                    .map(|r| ConfigurationItem {
                        mode: d.modes.iter().position(|n| n == &r.mode).unwrap_or(0) as i32,
                        scope: SCOPES.iter().position(|s| *s == r.scope).unwrap_or(0) as i32,
                        path: r.path.clone().into(),
                        contains: r.contains.clone().into(),
                        excludes: r.excludes.clone().into(),
                        minimum: r
                            .minimum_percent
                            .map(|p| p.to_string())
                            .unwrap_or_default()
                            .into(),
                        any: r.any,
                        tc: r.tc_expression.is_some(),
                        expression: r.tc_expression.clone().unwrap_or_default().into(),
                        ..Default::default()
                    })
                    .collect();
                d.description=tr("Bedingungen und Priorität ohne Textformat bearbeiten. Die erste passende Regel bestimmt die Ansicht.").into();
                Some(d)
            }
            Pending::Associations(Association::Edit(index)) => {
                let mut d = Draft::new(ConfigurationKind::Association, source, settings);
                let entry = index
                    .and_then(|i| self.profile.associations.get(i))
                    .cloned()
                    .unwrap_or_else(|| FileAssociation {
                        name: tr("Datei anzeigen").into(),
                        masks: "*.txt".into(),
                        program: r"C:\Windows\System32\notepad.exe".into(),
                        default_open: false,
                        arguments: vec!["{datei}".into()],
                    });
                d.form = ConfigurationForm {
                    name: entry.name.into(),
                    program: display_path(&entry.program).into(),
                    default_open: entry.default_open,
                    args: !entry.arguments.is_empty(),
                    arguments: entry.arguments.join("\n").into(),
                    ..Default::default()
                };
                d.rows = vec![ConfigurationItem {
                    contains: entry.masks.into(),
                    ..Default::default()
                }];
                d.description=tr("Dateimasken, Programm und Argumente getrennt bearbeiten. Diese Zuordnung gilt nur innerhalb von Arco Commander.").into();
                Some(d)
            }
            Pending::ViewSetup(View::Sort) => {
                let mut d = Draft::new(ConfigurationKind::Sort, source, settings);
                if self.current().wfx.is_some() {
                    d.sort_defaults.extend(
                        self.active_wfx_columns(self.active)
                            .iter()
                            .enumerate()
                            .map(|(i, f)| (SortKey::Wdx(i as u8), f.descending)),
                    );
                    d.sort_keys.extend(
                        self.active_wfx_columns(self.active)
                            .iter()
                            .enumerate()
                            .map(|(i, f)| {
                                (
                                    SortKey::Wdx(i as u8),
                                    format!("WFX · {}", f.selection.label()),
                                )
                            }),
                    );
                } else {
                    d.sort_keys
                        .extend(
                            self.active_wdx_columns(self.active)
                                .iter()
                                .map(|(i, f, _)| {
                                    (
                                        SortKey::Wdx(*i as u8),
                                        format!("WDX · {}", f.selection.label()),
                                    )
                                }),
                        );
                }
                d.set_sorts(self.current().current_sort_orders());
                d.description=tr("Sortierfelder in Prioritätsreihenfolge anordnen. Leere Listen verwenden die Standardsortierung.").into();
                Some(d)
            }
            Pending::ViewSetup(View::Colors) => {
                let mut d = Draft::new(ConfigurationKind::Colors, source, settings);
                for (label, pair) in [("Auswahl", settings.selection), ("Cursor", settings.cursor)]
                {
                    if let Some(pair) = pair {
                        d.rows.push(ConfigurationItem {
                            contains: label.into(),
                            light: format!("#{:06X}", pair.light).into(),
                            dark: format!("#{:06X}", pair.dark).into(),
                            ..Default::default()
                        });
                    }
                }
                d.rows
                    .extend(settings.colors.iter().map(|r| ConfigurationItem {
                        contains: r.masks.clone().into(),
                        light: format!("#{:06X}", r.color.light).into(),
                        dark: format!("#{:06X}", r.color.dark).into(),
                        ..Default::default()
                    }));
                d.description=tr("Farben für helle und dunkle Darstellung getrennt festlegen. Leere Listen verwenden die Standardfarben.").into();
                Some(d)
            }
            _ => return None,
        };
        if let Some(mut draft) = draft {
            draft.snapshot();
            self.configuration_draft = Some(draft);
            self.publish_configuration("");
            Some("configuration")
        } else {
            self.install_navigation(list, rows);
            Some("navigation-list")
        }
    }

    pub(crate) fn configuration_list_action(
        &mut self,
        action: i32,
        choice: Option<usize>,
    ) -> io::Result<()> {
        let pending = self
            .pending
            .as_ref()
            .and_then(navigation::clone_pending)
            .ok_or_else(|| io::Error::other("Die Konfiguration fehlt."))?;
        match pending {
            Pending::ViewSetup(View::Choose { modes, names }) => {
                if action == 1 {
                    return self.submit_view_setup(View::Choose { modes, names }, "", 1);
                }
                if action == 8 {
                    self.view_setup_action("view-rules-import");
                    return Ok(());
                }
                if choice == Some(0) && action == 0 {
                    return self.submit_view_setup(View::Choose { modes, names }, "", 0);
                }
                let name = choice
                    .and_then(|i| i.checked_sub(2))
                    .and_then(|i| names.get(i))
                    .cloned()
                    .ok_or_else(|| io::Error::other("Die gespeicherte Ansicht fehlt."))?;
                self.submit_view_setup(
                    View::Manage { modes, name },
                    "",
                    match action {
                        0 => 0,
                        2 => 1,
                        5 => 2,
                        _ => return Ok(()),
                    },
                )
            }
            Pending::ViewSetup(View::ChooseWfxColumns(plugin, names)) => {
                if action == 1 {
                    if self.job.is_some() {
                        return Err(io::Error::other("Ein Auftrag läuft bereits."));
                    }
                    if !self.wfx_enabled(&plugin) {
                        return Err(io::Error::other("Dieses WFX-Plugin ist deaktiviert."));
                    }
                    return self.submit_view_setup(View::ChooseWfxColumns(plugin, names), "", 1);
                }
                if action == 2 && self.job.is_some() {
                    return Err(io::Error::other("Ein Auftrag läuft bereits."));
                }
                if action == 2 && !self.wfx_enabled(&plugin) {
                    return Err(io::Error::other("Dieses WFX-Plugin ist deaktiviert."));
                }
                if matches!(choice, Some(0 | 2)) && action == 0 {
                    return self.submit_view_setup(
                        View::ChooseWfxColumns(plugin, names),
                        "",
                        choice.unwrap(),
                    );
                }
                let name = choice
                    .and_then(|i| i.checked_sub(3))
                    .and_then(|i| names.get(i))
                    .cloned()
                    .ok_or_else(|| {
                        io::Error::other("Der WFX-Spaltensatz ist nicht mehr vorhanden.")
                    })?;
                let plugin = *plugin;
                self.manage_wfx_columns(
                    plugin.clone(),
                    name,
                    match action {
                        0 => 0,
                        2 => 1,
                        5 => 2,
                        _ => return Ok(()),
                    },
                )?;
                if action == 5 {
                    self.choose_wfx_columns(plugin);
                }
                Ok(())
            }
            Pending::Associations(Association::Choose) => {
                if action == 1 {
                    return self.submit_association(Association::Choose, "", 0);
                }
                let index = choice
                    .and_then(|i| i.checked_sub(1))
                    .ok_or_else(|| io::Error::other("Die Zuordnung fehlt."))?;
                self.submit_association(
                    Association::Manage(index),
                    "",
                    match action {
                        0 | 2 => 0,
                        3 => 1,
                        4 => 2,
                        5 => 3,
                        _ => return Ok(()),
                    },
                )
            }
            other => self.execute(other, "", "", choice.unwrap_or(0), 0, ""),
        }
    }

    fn publish_configuration(&self, notice: &str) {
        if let Some(draft) = &self.configuration_draft {
            let revision = self.ui().get_configuration_data().revision.wrapping_add(1);
            self.ui()
                .set_configuration_data(draft.data(notice, revision));
        }
    }
    pub(crate) fn edit_configuration(
        &mut self,
        action: i32,
        index: i32,
        form: ConfigurationForm,
        item: ConfigurationItem,
    ) {
        let Some(draft) = &mut self.configuration_draft else {
            return;
        };
        draft.merge(form, item);
        let notice = draft
            .change(action, index)
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        self.publish_configuration(&notice);
    }
    pub(crate) fn submit_configuration(
        &mut self,
        form: ConfigurationForm,
        item: ConfigurationItem,
    ) {
        let Some(mut draft) = self.configuration_draft.take() else {
            return;
        };
        draft.merge(form, item);
        let pending = self.pending.as_ref().and_then(navigation::clone_pending);
        let result = pending
            .ok_or_else(|| io::Error::other("Die Konfiguration fehlt."))
            .and_then(|pending| self.save_configuration(&mut draft, pending));
        if let Err(error) = result {
            self.configuration_draft = Some(draft);
            self.publish_configuration(tr(&error.to_string()));
        }
        self.publish(self.active);
    }
    fn save_configuration(&mut self, d: &mut Draft, pending: Pending) -> io::Result<()> {
        match pending {
            Pending::ViewSetup(View::EditColumns { name, .. }) => {
                let key = view_setup::name_line(Some(&d.form.name), name.as_deref())?;
                let columns = d.columns()?;
                let mut settings = self.profile.views.clone();
                update_columns(&mut settings, name.as_deref(), key, columns)?;
                self.save_view_settings(settings)?;
                self.choose_view(false);
            }
            Pending::ViewSetup(View::EditWfxColumns(plugin, name, _)) => {
                let key = view_setup::name_line(Some(&d.form.name), name.as_deref())?;
                let columns = d.columns()?;
                self.save_wfx_column_values((*plugin).clone(), name, key, columns)?;
                self.choose_wfx_columns(*plugin);
            }
            Pending::ViewSetup(View::EditMode(name)) => {
                let key = view_setup::name_line(Some(&d.form.name), name.as_deref())?;
                let mut settings = self.profile.views.clone();
                if name.is_none() && settings.modes.contains_key(&key) {
                    return Err(io::Error::other("Dieser Ansichtsmodus existiert bereits."));
                }
                if settings.modes.len() >= 128 && !settings.modes.contains_key(&key) {
                    return Err(io::Error::other("Erlaubt sind höchstens 128 Ansichtsmodi."));
                }
                let mode = build_mode(d, &settings, name.as_deref())?;
                settings.modes.insert(key, mode);
                self.save_view_settings(settings)?;
                self.choose_view(true);
            }
            Pending::ViewSetup(View::EditRules) => {
                let rules = d.rules()?;
                let mut settings = self.profile.views.clone();
                settings.rules = rules;
                settings.automatic = d.form.automatic;
                self.save_view_settings(settings)?;
                self.status("Automatische Ansichtsregeln gespeichert.");
                self.close_dialog();
            }
            Pending::Associations(Association::Edit(index)) => {
                let text = association_text(d)?;
                self.submit_association(Association::Edit(index), &text, 0)?;
            }
            Pending::ViewSetup(View::Sort) => {
                let orders = d.sorts()?;
                let value = orders
                    .iter()
                    .map(|order| {
                        let key = match order.key {
                            SortKey::Wdx(i) => format!(
                                "{}{}",
                                if self.current().wfx.is_some() {
                                    "wfx"
                                } else {
                                    "wdx"
                                },
                                i + 1
                            ),
                            _ => sort_key(order.key).into(),
                        };
                        format!("{}{key}", if order.descending { "-" } else { "+" })
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                self.submit_view_setup(View::Sort, &value, 0)?;
                self.close_dialog();
            }
            Pending::ViewSetup(View::Colors) => {
                let mut text = String::new();
                for (i, row) in d.rows.iter().enumerate() {
                    if let Err(error) = (|| {
                        one_line(&row.contains)?;
                        one_line(&row.light)?;
                        one_line(&row.dark)?;
                        Ok::<_, io::Error>(())
                    })() {
                        d.selected = Some(i);
                        return Err(entry_error(i, error));
                    }
                    text.push_str(&format!(
                        "{} | {} | {}\n",
                        row.contains, row.light, row.dark
                    ));
                }
                self.submit_view_setup(View::Colors, &text, 0)?;
                self.close_dialog();
            }
            _ => return Err(io::Error::other("Die Konfiguration hat sich geändert.")),
        }
        Ok(())
    }
}

fn default_columns() -> Vec<Column> {
    vec![
        Column {
            field: Field::Size,
            title: tr("Größe").into(),
            width: 90,
            right_aligned: None,
        },
        Column {
            field: Field::Modified,
            title: tr("Geändert").into(),
            width: 130,
            right_aligned: None,
        },
    ]
}
fn mode_label(mode: &Mode) -> String {
    format!(
        "{} · {} · {} {}",
        tr(if mode.inherit_view {
            "Ansicht beibehalten"
        } else {
            match mode.view {
                TabView::Details => "Details",
                TabView::Short => "Kurz",
                TabView::Thumbnails => "Miniaturen",
                TabView::Comments => "Kommentare",
            }
        }),
        mode.columns.as_deref().unwrap_or(tr("Standardspalten")),
        mode.sort.len(),
        tr("Sortierfelder")
    )
}
fn mode_from_form(form: &ConfigurationForm, sets: &[String]) -> io::Result<Mode> {
    let view = match form.view {
        0 | 4 => TabView::Details,
        1 => TabView::Short,
        2 => TabView::Thumbnails,
        3 => TabView::Comments,
        _ => return Err(io::Error::other("Unbekannte Ansicht.")),
    };
    let columns = if form.column == 0 {
        None
    } else {
        Some(
            sets.get(usize::try_from(form.column - 1).unwrap_or(usize::MAX))
                .cloned()
                .ok_or_else(|| io::Error::other("Der Spaltensatz fehlt."))?,
        )
    };
    Ok(Mode {
        view,
        inherit_view: form.view == 4,
        columns,
        ..Default::default()
    })
}
fn update_columns(
    settings: &mut Settings,
    original: Option<&str>,
    key: String,
    columns: Vec<Column>,
) -> io::Result<()> {
    if original.is_none() && settings.columns.contains_key(&key) {
        return Err(io::Error::other("Dieser Spaltensatz existiert bereits."));
    }
    if settings.columns.len() >= 128 && !settings.columns.contains_key(&key) {
        return Err(io::Error::other("Erlaubt sind höchstens 128 Spaltensätze."));
    }
    if settings.columns.get(&key).is_some_and(|previous| {
        !previous
            .iter()
            .map(|c| &c.field)
            .eq(columns.iter().map(|c| &c.field))
    }) {
        settings.tc_columns.retain(|_, layout| layout.name != key);
    }
    settings.columns.insert(key, columns);
    Ok(())
}
fn build_mode(d: &mut Draft, settings: &Settings, original: Option<&str>) -> io::Result<Mode> {
    let mut mode = mode_from_form(&d.form, &d.sets)?;
    mode.sort = d.sorts()?;
    mode.colors = original
        .and_then(|name| settings.modes.get(name))
        .and_then(|mode| mode.colors);
    if mode.view != TabView::Details
        && (mode.columns.is_some()
            || mode
                .sort
                .iter()
                .any(|order| matches!(order.key, SortKey::Wdx(_))))
    {
        return Err(io::Error::other(
            "Spaltensätze benötigen die Detailansicht.",
        ));
    }
    Ok(mode)
}
fn sort_key(key: SortKey) -> &'static str {
    match key {
        SortKey::Name => "name",
        SortKey::Extension => "erweiterung",
        SortKey::Size => "größe",
        SortKey::Modified => "datum",
        SortKey::Attributes => "attribute",
        SortKey::Comment => "kommentar",
        _ => "unsortiert",
    }
}
fn association_text(d: &Draft) -> io::Result<String> {
    for text in [&d.form.name, &d.form.program] {
        one_line(text)?;
    }
    let masks = d
        .rows
        .first()
        .map(|r| r.contains.as_str())
        .unwrap_or_default();
    one_line(masks)?;
    let mut text = format!(
        "{}\n{}\n{}\n{}",
        d.form.name,
        masks,
        d.form.program,
        if d.form.default_open { "ja" } else { "nein" }
    );
    if d.form.args {
        text.push('\n');
        text.push_str(&d.form.arguments);
        text.push('\n');
    }
    associations::parse_association(&text)?;
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use arco_core::plugin::{WdxColumn, WdxField, WfxField};
    use arco_core::view_settings::{ModeColors, TcColumnLayout};

    fn draft(kind: ConfigurationKind) -> Draft {
        Draft::new(kind, &Dialog::default(), &Settings::default())
    }

    #[test]
    fn typed_columns_keep_plugin_identity_expressions_and_alignment() {
        let wdx = Field::Wdx(WdxColumn {
            plugin: "Details.wdx64".into(),
            selection: WdxField {
                field: "Title".into(),
                unit: Some("localized".into()),
            },
        });
        let wfx = Field::Wfx(WfxField {
            selection: WdxField {
                field: "Size".into(),
                unit: Some("KiB".into()),
            },
            descending: true,
        });
        let tc = Field::TcExpression(TcColumnExpression::parse("[=tc.name] [=tc.size]").unwrap());
        let columns = vec![
            Column {
                field: wdx,
                title: "Titel | Zusatz".into(),
                width: 162,
                right_aligned: None,
            },
            Column {
                field: wfx,
                title: "WFX".into(),
                width: 180,
                right_aligned: Some(true),
            },
            Column {
                field: tc,
                title: "Ausdruck".into(),
                width: 220,
                right_aligned: Some(false),
            },
        ];
        let mut d = draft(ConfigurationKind::Columns);
        d.set_columns(&columns, []);
        assert_eq!(d.columns().unwrap(), columns);
        let mut profile = arco_core::profile::Profile::default();
        profile.views = Settings {
            columns: [("Metadaten".into(), columns.clone())].into(),
            ..Default::default()
        };
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("profile.json");
        profile.save(&path).unwrap();
        assert_eq!(
            arco_core::profile::Profile::load(&path)
                .unwrap()
                .views
                .columns["Metadaten"],
            columns
        );
    }

    #[test]
    fn invalid_rows_remain_in_the_draft_and_navigate_to_the_error() {
        let columns = default_columns();
        let mut d = draft(ConfigurationKind::Columns);
        d.set_columns(&columns, []);
        d.snapshot();
        d.change(4, 0).unwrap();
        assert!(d.dirty());
        assert_eq!(d.columns().unwrap()[0], columns[1]);
        d.rows[1].width = "601".into();
        let before = d.rows.clone();
        assert!(d.columns().is_err());
        assert_eq!(d.selected, Some(1));
        assert_eq!(d.rows, before);
        assert_eq!(d.original_rows[0].width, "90");
        d.rows[1].width = "90".into();
        d.change(3, 0).unwrap();
        assert!(!d.dirty());
    }

    #[test]
    fn column_layout_metadata_survives_cosmetic_edits_but_not_field_reordering() {
        let mut settings = Settings::default();
        let columns = default_columns();
        settings.columns.insert("Details".into(), columns.clone());
        settings.tc_columns.insert(
            101,
            TcColumnLayout {
                name: "Details".into(),
                sort_keys: vec![
                    Some(SortKey::Name),
                    Some(SortKey::Size),
                    Some(SortKey::Modified),
                ],
            },
        );
        let mut changed = columns.clone();
        changed[0].title = "Neue Größe".into();
        changed[0].width = 180;
        update_columns(
            &mut settings,
            Some("Details"),
            "Details".into(),
            changed.clone(),
        )
        .unwrap();
        assert!(settings.tc_columns.contains_key(&101));
        changed.swap(0, 1);
        update_columns(&mut settings, Some("Details"), "Details".into(), changed).unwrap();
        assert!(settings.tc_columns.is_empty());
        assert!(update_columns(&mut settings, None, "Details".into(), columns).is_err());
    }

    #[test]
    fn rules_keep_mask_exclusions_and_complete_tc_boolean_expressions() {
        let mut d = draft(ConfigurationKind::Rules);
        d.modes = vec!["Fotos".into()];
        let expression = r"U||D|&+C:\Media\*";
        d.rows = vec![
            ConfigurationItem {
                mode: 0,
                scope: 1,
                path: "C:/Media/**".into(),
                contains: "*.jpg;*.png | temp*".into(),
                minimum: "42".into(),
                ..Default::default()
            },
            ConfigurationItem {
                tc: true,
                expression: expression.into(),
                ..Default::default()
            },
        ];
        d.snapshot();
        let rules = d.rules().unwrap();
        assert_eq!(rules[0].contains, "*.jpg;*.png | temp*");
        assert_eq!(rules[0].minimum_percent, Some(42));
        assert_eq!(rules[1].tc_expression.as_deref(), Some(expression));
        d.rows[0].minimum = "101".into();
        assert!(d.rules().is_err());
        assert_eq!(d.selected, Some(0));
        assert_eq!(d.rows[0].minimum, "101");
        d.rows[0].minimum = "42".into();
        d.change(4, 0).unwrap();
        assert_eq!(
            d.rules().unwrap()[0].tc_expression.as_deref(),
            Some(expression)
        );
    }

    #[test]
    fn mode_editing_keeps_imported_colors_and_rejects_incompatible_views() {
        let colors = ModeColors {
            tab: Some(0x123456),
            light: [Some(0xffffff), None],
            dark: [None, Some(0x111111)],
        };
        let mut settings = Settings::default();
        settings.modes.insert(
            "Fotos".into(),
            Mode {
                colors: Some(colors),
                ..Default::default()
            },
        );
        let mut d = draft(ConfigurationKind::Mode);
        d.sets = vec!["Details".into()];
        d.form.column = 1;
        d.form.view = 4;
        d.set_sorts(&[SortOrder {
            key: SortKey::Size,
            descending: true,
        }]);
        let mode = build_mode(&mut d, &settings, Some("Fotos")).unwrap();
        assert_eq!(mode.colors, Some(colors));
        assert!(mode.inherit_view);
        assert_eq!(mode.sort[0].key, SortKey::Size);
        d.form.view = 1;
        assert!(build_mode(&mut d, &settings, Some("Fotos")).is_err());
        assert_eq!(d.form.column, 1);
    }

    #[test]
    fn sorting_keeps_missing_plugin_keys_and_rejects_duplicates_and_unsorted_mixes() {
        let mut d = draft(ConfigurationKind::Sort);
        let plugin = SortOrder {
            key: SortKey::Wdx(7),
            descending: true,
        };
        d.set_sorts(&[plugin]);
        assert_eq!(d.sorts().unwrap(), vec![plugin]);
        d.rows.push(d.rows[0].clone());
        assert!(d.sorts().is_err());
        assert_eq!(d.selected, Some(1));
        d.set_sorts(&[
            SortOrder {
                key: SortKey::Unsorted,
                descending: false,
            },
            SortOrder {
                key: SortKey::Name,
                descending: false,
            },
        ]);
        assert!(d.sorts().is_err());
    }

    #[test]
    fn association_forms_preserve_empty_arguments_and_reject_line_injection() {
        for arguments in [
            vec![],
            vec![String::new()],
            vec!["{datei}".into(), "a b".into(), String::new()],
        ] {
            let mut d = draft(ConfigurationKind::Association);
            d.form = ConfigurationForm {
                name: "Öffnen".into(),
                program: display_path(&std::env::temp_dir().join("Editor.exe")).into(),
                args: !arguments.is_empty(),
                arguments: arguments.join("\n").into(),
                default_open: true,
                ..Default::default()
            };
            d.rows = vec![ConfigurationItem {
                contains: "*.txt;*.md | intern*".into(),
                ..Default::default()
            }];
            let entry = associations::parse_association(&association_text(&d).unwrap()).unwrap();
            assert_eq!(entry.arguments, arguments);
            assert_eq!(
                associations::parse_association(&associations::format_association(&entry))
                    .unwrap()
                    .arguments,
                arguments
            );
            d.form.name = "Falsch\n*.exe".into();
            assert!(association_text(&d).is_err());
        }
    }

    #[test]
    fn choosing_wfx_sort_fields_applies_the_plugin_default_and_allows_an_override() {
        let mut d = draft(ConfigurationKind::Sort);
        let key = SortKey::Wdx(0);
        d.sort_keys.push((key, "WFX · Amount".into()));
        d.sort_defaults.push((key, true));
        d.rows.push(ConfigurationItem::default());
        d.snapshot();
        let index = d.sort_keys.len() - 1;
        d.change(5, index as i32).unwrap();
        assert!(d.sorts().unwrap()[0].descending);
        let mut item = d.rows[0].clone();
        item.direction = 0;
        d.merge(d.form.clone(), item);
        assert!(!d.sorts().unwrap()[0].descending);
    }

    #[test]
    fn color_editor_keeps_capacity_for_file_rules_and_both_background_rules() {
        let mut d = draft(ConfigurationKind::Colors);
        for _ in 0..130 {
            d.change(1, 0).unwrap();
        }
        assert!(d.change(1, 0).is_err());
        assert_eq!(d.rows.len(), 130);
    }
}
