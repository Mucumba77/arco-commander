use super::{App, Dialog, Outcome, Pending};
use arco_core::{
    files,
    plugin::{self, Interface, Registration, WdxEditableField, WdxField},
};
use std::{fmt::Write as _, fs::Metadata, path::PathBuf};

pub struct EditTarget {
    entry: Registration,
    files: Vec<(PathBuf, Metadata)>,
}

pub enum EditInput {
    Text(String),
    Plugin(usize),
}

impl App {
    pub(crate) fn start_wdx_edit(&mut self) {
        let paths = self.current().paths();
        if !self.current().is_local() || paths.is_empty() || paths.len() > 128 {
            self.status("Wähle zwischen 1 und 128 lokale Dateien für die WDX-Feldänderung.");
            return;
        }
        let plugins: Vec<_> = self
            .profile
            .plugins
            .iter()
            .filter(|entry| paths.iter().all(|path| entry.matches(Interface::Wdx, path)))
            .cloned()
            .collect();
        if plugins.is_empty() {
            self.status("Für die Auswahl ist kein passendes WDX-Plugin aktiv.");
            return;
        }
        let names: Vec<_> = plugins.iter().map(|entry| entry.name.clone()).collect();
        let options: Vec<_> = names.iter().map(String::as_str).collect();
        self.dialog(
            "confirm",
            Pending::WdxEditPlugin(plugins, paths),
            Dialog {
                title: "WDX-Feld ändern",
                description: "Wähle das Plugin, das die Metadaten ändern soll.",
                options: &options,
                ..Dialog::default()
            },
        );
    }

    pub(crate) fn read_wdx_editable(&mut self, entry: Registration, paths: Vec<PathBuf>) {
        self.start_job("Lese beschreibbare WDX-Felder …", false, move |job| {
            let files = paths
                .into_iter()
                .map(|path| {
                    let metadata = files::regular_metadata(&path)?;
                    if !metadata.is_file() {
                        return Err(std::io::Error::other(
                            "WDX-Feldänderung erwartet lokale Dateien.",
                        ));
                    }
                    Ok((path, metadata))
                })
                .collect::<std::io::Result<_>>()?;
            let fields = plugin::wdx_editable_fields(&entry.path, &job.cancel)?;
            Ok(Outcome::WdxEditable(EditTarget { entry, files }, fields))
        });
    }

    pub(in crate::app) fn choose_wdx_edit_field(
        &mut self,
        target: EditTarget,
        fields: Vec<WdxEditableField>,
    ) {
        if fields.is_empty() {
            self.text(
                "WDX-Feld ändern",
                "Das Plugin meldet keine beschreibbaren Felder.",
            );
            return;
        }
        let labels: Vec<_> = fields.iter().map(|field| field.selection.label()).collect();
        let options: Vec<_> = labels.iter().map(String::as_str).collect();
        self.dialog(
            "confirm",
            Pending::WdxEditField(target, fields),
            Dialog {
                title: "Beschreibbares WDX-Feld wählen",
                options: &options,
                ..Dialog::default()
            },
        );
    }

    pub(crate) fn wdx_edit_value(&mut self, target: EditTarget, field: WdxEditableField) {
        if field.editor {
            self.dialog(
                "confirm",
                Pending::WdxEditMode(target, field),
                Dialog {
                    title: "WDX-Eingabemodus",
                    description: "Der Plugin-Editor startet mit dem Wert der ersten Datei, wenn das Plugin ihn bereitstellt.",
                    options: &["Plugin-Editor öffnen", "Wert direkt eingeben"],
                    ..Dialog::default()
                },
            );
        } else {
            self.wdx_edit_text(target, field);
        }
    }

    pub(crate) fn wdx_edit_text(&mut self, target: EditTarget, field: WdxEditableField) {
        let title = field.selection.label();
        let description = match field.kind {
            1 | 2 => "Ganze Zahl",
            3 => "Dezimalzahl",
            4 => "Datum im Format JJJJ-MM-TT",
            5 => "Uhrzeit im Format HH:MM:SS",
            6 | 7 => "Wähle den neuen Wert.",
            10 => "Zeitstempel in UTC im Format JJJJ-MM-TT HH:MM:SS",
            _ => "Neuer Textwert. Eine leere Eingabe setzt einen leeren Text.",
        };
        let options: Vec<String> = if field.kind == 6 {
            vec!["Ja".into(), "Nein".into()]
        } else {
            field.choices.clone()
        };
        let options: Vec<_> = options.iter().map(String::as_str).collect();
        self.dialog(
            if options.is_empty() {
                "input"
            } else {
                "confirm"
            },
            Pending::WdxEditValue(target, field),
            Dialog {
                title: &title,
                description,
                options: &options,
                ..Dialog::default()
            },
        );
    }

    pub(crate) fn confirm_wdx_edit(
        &mut self,
        target: EditTarget,
        field: WdxEditableField,
        value: &str,
        option: usize,
    ) {
        let value = match field.kind {
            6 => ["Ja", "Nein"].get(option).copied(),
            7 => field.choices.get(option).map(String::as_str),
            _ => Some(value),
        };
        let Some(value) = value else { return };
        self.confirm_wdx_input(target, field.selection, EditInput::Text(value.to_owned()));
    }

    pub(crate) fn open_wdx_editor(&mut self, target: EditTarget, field: WdxEditableField) {
        if let Some(parent) = self.native_window_handle() {
            self.confirm_wdx_input(target, field.selection, EditInput::Plugin(parent));
        } else {
            self.status("Für den WDX-Editor fehlt das Windows-Hauptfenster.");
        }
    }

    fn confirm_wdx_input(&mut self, target: EditTarget, field: WdxField, input: EditInput) {
        let mut description = format!(
            "{} setzt „{}“ in {} Datei(en). Das Plugin schreibt direkt in die Dateien.",
            target.entry.name,
            field.label(),
            target.files.len()
        );
        if matches!(input, EditInput::Plugin(_)) {
            description.push_str(" Bestätige den neuen Wert im Plugin-Editor, um ihn anzuwenden.");
        }
        self.dialog(
            "confirm",
            Pending::WdxEditApply(target, field, input),
            Dialog {
                title: "Metadaten ändern",
                description: &description,
                ..Dialog::default()
            },
        );
    }

    pub(crate) fn apply_wdx_edit(&mut self, target: EditTarget, field: WdxField, input: EditInput) {
        self.start_job("Ändere WDX-Feld …", true, move |job| {
            let mut paths = Vec::with_capacity(target.files.len());
            for (path, metadata) in target.files {
                files::ensure_unchanged(&path, &metadata)?;
                paths.push(path);
            }
            let progress = |value| job.progress(value);
            let rows = match input {
                EditInput::Text(value) => plugin::wdx_set(
                    &target.entry.path,
                    field,
                    value,
                    paths,
                    &job.cancel,
                    progress,
                ),
                EditInput::Plugin(parent) => plugin::wdx_edit(
                    &target.entry.path,
                    field,
                    paths,
                    parent,
                    &job.cancel,
                    progress,
                ),
            }?;
            let mut report = String::new();
            for row in rows {
                let _ = writeln!(report, "{} · {}", row.value, row.name);
            }
            Ok(Outcome::Text("WDX-Feldänderung".into(), report))
        });
    }
}
