use super::{App, JobDescription, Outcome};
use crate::commands::{Dialog, Pending};
use crate::panel::FtpLocation;
use crate::{PropertiesData, ReviewRow, display_path, human_size, locale::tr};
use arco_core::files::{self, Entry, Kind};
use arco_windows::ftp;
use chrono::{DateTime, Local};
use std::{
    fs, io,
    path::PathBuf,
    time::{Duration, Instant, SystemTime},
};

pub(crate) struct Sheet {
    pub title: &'static str,
    pub path: String,
    pub summary: String,
    pub rows: Vec<(String, String, String)>,
    pub local: Vec<PathBuf>,
    pub ftp: Option<(FtpLocation, Vec<ftp::Entry>)>,
}

impl Sheet {
    pub fn virtual_entry(title: &'static str, path: String, entry: Option<&Entry>) -> Self {
        let mut sheet = Self {
            title,
            path,
            summary: tr("Virtueller Eintrag").into(),
            rows: Vec::new(),
            local: Vec::new(),
            ftp: None,
        };
        if let Some(entry) = entry {
            sheet.field("Name", entry.name.to_string_lossy().into_owned());
            sheet.field("Typ", tr(kind_name(entry.kind)).into());
            sheet.field(
                "Größe",
                entry
                    .size
                    .map(human_size)
                    .unwrap_or_else(|| tr("Nicht ermittelt").into()),
            );
            sheet.field("Geändert", time(entry.modified));
            sheet.field(
                "Attribute",
                if entry.attributes == 0 {
                    tr("Nicht geliefert").into()
                } else {
                    attributes(entry.attributes)
                },
            );
        }
        sheet
    }

    pub fn field(&mut self, label: &str, value: String) {
        self.rows.push((tr(label).into(), value, String::new()));
    }
}

impl App {
    pub(crate) fn open_local_properties(&mut self) {
        let tab = self.current();
        let paths = if tab.parent_row(tab.cursor) {
            vec![tab.path.clone()]
        } else {
            tab.paths()
        };
        if paths.is_empty() {
            return;
        }
        self.read_properties_paths(paths);
    }

    fn read_properties_paths(&mut self, paths: Vec<PathBuf>) {
        let description = JobDescription::new(
            crate::OperationKind::Metadata,
            selection(&paths),
            String::new(),
        );
        self.start_operation_job("Eigenschaften lesen", false, description, move |job| {
            let mut last = Instant::now() - Duration::from_secs(1);
            let total = paths.len();
            let sheet = read_local(paths, &job.cancel, |path, processed| {
                if processed == total || last.elapsed() >= Duration::from_millis(80) {
                    last = Instant::now();
                    job.entry_progress(
                        display_path(path),
                        processed,
                        total,
                        "Eigenschaften lesen …",
                    );
                }
            })?;
            Ok(Outcome::Properties(Box::new(sheet)))
        });
    }

    pub(crate) fn show_properties(&mut self, sheet: Sheet) {
        let rows = sheet
            .rows
            .iter()
            .map(|(label, value, detail)| ReviewRow {
                primary: tr(label).into(),
                secondary: localized_value(label, value).into(),
                detail: detail.as_str().into(),
                kind: "file".into(),
            })
            .collect::<Vec<_>>();
        let path = if sheet.local.len() > 1 {
            selection(&sheet.local)
        } else {
            sheet.path.clone()
        };
        let summary = if !sheet.local.is_empty() {
            format!("{} · {}", tr("Ausgewählte Einträge"), sheet.local.len())
        } else {
            tr(&sheet.summary).into()
        };
        self.ui().set_properties_data(PropertiesData {
            title: tr(sheet.title).into(),
            path: path.into(),
            summary: summary.into(),
            rows: slint::ModelRc::new(slint::VecModel::from(rows)),
            local: !sheet.local.is_empty(),
            native: sheet.local.len() == 1,
            ftp_permissions: sheet
                .ftp
                .as_ref()
                .is_some_and(|(_, entries)| !entries.is_empty()),
            notice: "".into(),
        });
        self.dialog(
            "properties",
            Pending::Properties(Box::new(sheet)),
            Dialog::default(),
        );
    }

    pub(super) fn properties_action(&mut self, action: i32) {
        let Some(Pending::Properties(sheet)) = &self.pending else {
            return;
        };
        match action {
            0 => {
                let mut text = format!("{}\n{}\n", tr(sheet.title), sheet.path);
                for (label, value, detail) in &sheet.rows {
                    text.push_str(&format!(
                        "{}: {}\n",
                        tr(label),
                        localized_value(label, value)
                    ));
                    if !detail.is_empty() {
                        text.push_str(&format!("{detail}\n"));
                    }
                }
                self.report(arco_windows::copy_text(&text));
            }
            1 if !sheet.local.is_empty() => {
                let paths = sheet.local.clone();
                self.open_metadata_paths(paths);
            }
            2 if sheet.local.len() == 1 => {
                let result = arco_windows::properties(&sheet.local[0]);
                if let Err(error) = result {
                    let ui = self.ui();
                    let mut data = ui.get_properties_data();
                    data.notice = error.to_string().into();
                    ui.set_properties_data(data);
                }
            }
            3 => {
                if let Some((location, entries)) = &sheet.ftp {
                    let location = location.clone();
                    let entries = entries.clone();
                    self.open_ftp_permissions(location, entries);
                }
            }
            4 if !sheet.local.is_empty() => {
                let paths = sheet.local.clone();
                self.close_dialog();
                self.read_properties_paths(paths);
            }
            _ => {}
        }
    }
}

pub(super) fn selection(paths: &[PathBuf]) -> String {
    if let [path] = paths {
        display_path(path)
    } else {
        format!("{} · {}", tr("Ausgewählte Einträge"), paths.len())
    }
}

pub(super) fn time(value: Option<SystemTime>) -> String {
    value
        .map(|value| {
            DateTime::<Local>::from(value)
                .format("%Y-%m-%d %H:%M:%S")
                .to_string()
        })
        .unwrap_or_else(|| tr("Nicht verfügbar").into())
}

fn kind_name(kind: Kind) -> &'static str {
    match kind {
        Kind::File => "Datei",
        Kind::Directory => "Ordner",
        Kind::Link => "Verknüpfung",
        Kind::Other => "Anderer Eintrag",
    }
}

fn localized_value(label: &str, value: &str) -> String {
    if matches!(
        label,
        "Typ"
            | "Attribute"
            | "Umfang"
            | "Dateigröße der Auswahl"
            | "Schreibgeschützt (R)"
            | "Versteckt (H)"
            | "System (S)"
            | "Archiv (A)"
            | "Lesefehler"
            | "Unix-Rechte"
    ) || value == "Ordnerinhalte nicht eingerechnet"
        || value == "Nicht verfügbar"
    {
        value.split(" · ").map(tr).collect::<Vec<_>>().join(" · ")
    } else {
        value.into()
    }
}

fn attributes(flags: u32) -> String {
    let values = [
        (1, "Schreibgeschützt (R)"),
        (2, "Versteckt (H)"),
        (4, "System (S)"),
        (32, "Archiv (A)"),
        (1024, "Verknüpfung"),
        (2048, "Komprimiert"),
        (16384, "Verschlüsselt"),
    ];
    let values = values
        .into_iter()
        .filter(|(flag, _)| flags & flag != 0)
        .map(|(_, name)| tr(name))
        .collect::<Vec<_>>();
    if values.is_empty() {
        tr("Keine besonderen Attribute").into()
    } else {
        values.join(" · ")
    }
}

fn flags(metadata: &fs::Metadata) -> u32 {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes()
    }
    #[cfg(not(windows))]
    {
        u32::from(metadata.permissions().readonly())
    }
}

fn read_local(
    paths: Vec<PathBuf>,
    cancel: &std::sync::atomic::AtomicBool,
    mut progress: impl FnMut(&std::path::Path, usize),
) -> io::Result<Sheet> {
    let mut sheet = Sheet {
        title: "Dateieigenschaften",
        path: selection(&paths),
        summary: String::new(),
        rows: Vec::new(),
        local: paths.clone(),
        ftp: None,
    };
    let mut files = 0;
    let mut folders = 0;
    let mut links = 0;
    let mut bytes = 0u64;
    let mut errors = Vec::new();
    let mut counts = [0usize; 4];
    for (index, path) in paths.iter().enumerate() {
        files::check_cancel(cancel)?;
        progress(path, index);
        match fs::symlink_metadata(path) {
            Ok(metadata) => {
                let attrs = flags(&metadata);
                let link = metadata.file_type().is_symlink() || attrs & 1024 != 0;
                if link {
                    links += 1;
                } else if metadata.is_dir() {
                    folders += 1;
                } else {
                    files += 1;
                    bytes = bytes.saturating_add(metadata.len());
                }
                for (count, flag) in counts.iter_mut().zip([1, 2, 4, 32]) {
                    *count += usize::from(attrs & flag != 0);
                }
                if paths.len() == 1 {
                    sheet.field(
                        "Name",
                        path.file_name()
                            .unwrap_or(path.as_os_str())
                            .to_string_lossy()
                            .into_owned(),
                    );
                    sheet.field(
                        "Typ",
                        tr(if link {
                            "Verknüpfung"
                        } else if metadata.is_dir() {
                            "Ordner"
                        } else {
                            "Datei"
                        })
                        .into(),
                    );
                    sheet.field(
                        "Größe",
                        if metadata.is_file() && !link {
                            format!(
                                "{} · {} {}",
                                human_size(metadata.len()),
                                metadata.len(),
                                tr("Bytes")
                            )
                        } else {
                            tr("Ordnerinhalte nicht eingerechnet").into()
                        },
                    );
                    sheet.field("Erstellt", time(metadata.created().ok()));
                    sheet.field("Geändert", time(metadata.modified().ok()));
                    sheet.field("Letzter Zugriff", time(metadata.accessed().ok()));
                    sheet.field("Attribute", attributes(attrs));
                    if link && let Ok(target) = fs::read_link(path) {
                        sheet.field("Verknüpfungsziel", display_path(&target));
                    }
                }
            }
            Err(error) => errors.push(format!("{}: {error}", display_path(path))),
        }
    }
    files::check_cancel(cancel)?;
    progress(paths.last().unwrap(), paths.len());
    if paths.len() > 1 {
        sheet.field("Dateien", files.to_string());
        sheet.field("Ordner", folders.to_string());
        sheet.field("Verknüpfungen", links.to_string());
        sheet.field(
            "Dateigröße der Auswahl",
            if errors.is_empty() {
                human_size(bytes)
            } else {
                format!("{} · {}", human_size(bytes), tr("Unvollständig"))
            },
        );
        sheet.field("Umfang", tr("Ordnerinhalte nicht eingerechnet").into());
        for (count, label) in counts.into_iter().zip([
            "Schreibgeschützt (R)",
            "Versteckt (H)",
            "System (S)",
            "Archiv (A)",
        ]) {
            sheet.field(
                label,
                if !errors.is_empty() {
                    tr("Nicht vollständig ermittelt").into()
                } else if count == 0 {
                    tr("Nicht gesetzt").into()
                } else if count == paths.len() {
                    tr("Gesetzt").into()
                } else {
                    tr("Unterschiedlich").into()
                },
            );
        }
    }
    sheet.summary = format!(
        "{}: {} · {}: {} · {}: {}",
        tr("Dateien"),
        files,
        tr("Ordner"),
        folders,
        tr("Verknüpfungen"),
        links
    );
    for error in errors {
        sheet
            .rows
            .push(("Lesefehler".into(), tr("Nicht verfügbar").into(), error));
    }
    Ok(sheet)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    #[test]
    fn properties_count_only_selected_files_and_do_not_recurse() {
        let fixture = tempfile::tempdir().unwrap();
        let file = fixture.path().join("one.txt");
        let dir = fixture.path().join("folder");
        fs::write(&file, b"abc").unwrap();
        fs::create_dir(&dir).unwrap();
        fs::write(dir.join("hidden-child"), vec![0; 1000]).unwrap();
        let sheet = read_local(vec![file, dir], &AtomicBool::new(false), |_, _| {}).unwrap();
        assert!(
            sheet
                .rows
                .iter()
                .any(|(key, value, _)| key == "Dateigröße der Auswahl" && value == "3 B")
        );
        assert!(
            sheet
                .rows
                .iter()
                .any(|(key, value, _)| key == "Ordner" && value == "1")
        );
        let cancel = AtomicBool::new(false);
        let result = read_local(sheet.local, &cancel, |_, _| {
            cancel.store(true, Ordering::Relaxed)
        });
        assert_eq!(result.err().unwrap().kind(), io::ErrorKind::Interrupted);
    }
}
