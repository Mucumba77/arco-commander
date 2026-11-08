use super::{App, Outcome};
use crate::commands::{Dialog, Pending};
use crate::{display_path, human_size};
use arco_core::{advanced_search, files, tools};
use chrono::{DateTime, Local};
use std::fmt::Write;
use std::io;
use std::path::PathBuf;
use std::sync::atomic::Ordering;

impl App {
    pub(crate) fn print_file(&mut self) {
        let path = self
            .viewer
            .as_ref()
            .map(|viewer| viewer.path.clone())
            .or_else(|| {
                self.current()
                    .current()
                    .filter(|entry| entry.kind == files::Kind::File)
                    .map(|entry| entry.path.clone())
            });
        let Some(path) = path else {
            self.status("Wähle eine Textdatei.");
            return;
        };
        let staged = self
            .staged_preview
            .as_ref()
            .filter(|preview| preview.path == path);
        let storage = staged.map(|preview| preview.storage.clone());
        let title = staged.map_or_else(|| display_path(&path), |preview| preview.title.clone());
        self.start_job("Bereite Textdruck vor …", false, move |job| {
            let _storage = storage;
            let text = tools::read_text_file(&path, 4 * 1024 * 1024, &job.cancel)?;
            Ok(Outcome::PrintPreview(title, text))
        });
    }

    pub(crate) fn print_directory(&mut self, recursive: bool) {
        let base = self.current().path.clone();
        let mut paths = self.current().marked_paths();
        if paths.is_empty() {
            paths = self
                .current()
                .display_rows()
                .map(|entry| entry.path.clone())
                .collect();
        }
        if paths.is_empty() {
            self.status("Es sind keine Einträge zum Drucken sichtbar.");
            return;
        }
        if recursive {
            self.dialog(
                "input",
                Pending::PrintDirectory(base, paths),
                Dialog {
                    title: "Dateiliste mit Unterordnern drucken",
                    description: "Leere Ordnertiefe berücksichtigt alle Unterordner.",
                    placeholder: "Dateimaske",
                    value: "*",
                    second_placeholder: "Maximale Unterordnerebenen",
                    ..Default::default()
                },
            );
        } else {
            self.prepare_print_directory(base, paths, None);
        }
    }

    pub(crate) fn prepare_print_directory(
        &mut self,
        base: PathBuf,
        paths: Vec<PathBuf>,
        query: Option<advanced_search::Query>,
    ) {
        let hidden = self.profile.show_hidden;
        let ignore = self.ignore_list();
        self.start_job("Bereite Dateilisten-Druck vor …", false, move |job| {
            let title = format!("Dateiliste · {}", display_path(&base));
            let mut text = format!("{title}\n\n");
            let mut rows = Vec::new();
            let mut warnings = Vec::new();
            if let Some(query) = query {
                let found = advanced_search::search(
                    &paths,
                    true,
                    &query,
                    hidden,
                    ignore.as_deref(),
                    &job.cancel,
                );
                if found.cancelled {
                    return Err(io::ErrorKind::Interrupted.into());
                }
                for entry in found.directory.entries {
                    rows.push((
                        entry.path,
                        entry.kind == files::Kind::Directory,
                        entry.size,
                        entry.modified,
                    ));
                }
                warnings = found.directory.errors;
            } else {
                for path in paths {
                    if job.cancel.load(Ordering::Relaxed) {
                        return Err(io::ErrorKind::Interrupted.into());
                    }
                    match files::regular_metadata(&path) {
                        Ok(metadata) => rows.push((
                            path,
                            metadata.is_dir(),
                            metadata.is_file().then_some(metadata.len()),
                            metadata.modified().ok(),
                        )),
                        Err(error) => warnings.push(format!("{} · {error}", display_path(&path))),
                    }
                }
            }
            rows.sort_unstable_by(|left, right| left.0.cmp(&right.0));
            for (path, directory, size, modified) in rows {
                let name = display_path(path.strip_prefix(&base).unwrap_or(&path));
                let size = if directory {
                    "<ORDNER>".into()
                } else {
                    size.map(human_size).unwrap_or_default()
                };
                let modified = modified
                    .map(|time| {
                        DateTime::<Local>::from(time)
                            .format("%Y-%m-%d %H:%M")
                            .to_string()
                    })
                    .unwrap_or_default();
                writeln!(text, "{size:>12}  {modified:16}  {name}").unwrap();
                if text.len() > 16 * 1024 * 1024 {
                    return Err(io::Error::other(
                        "Die Dateiliste ist für den Druck zu groß.",
                    ));
                }
            }
            if !warnings.is_empty() {
                writeln!(text, "\n{} Lesefehler", warnings.len()).unwrap();
                for warning in warnings.iter().take(30) {
                    writeln!(text, "{warning}").unwrap();
                }
            }
            Ok(Outcome::PrintPreview(title, text))
        });
    }

    pub(super) fn show_print_preview(&mut self, title: String, body: String) {
        let mut preview: String = body.chars().take(120_000).collect();
        if preview.len() < body.len() {
            preview.push_str("\n… Die Vorschau zeigt den Anfang des Druckdokuments.");
        }
        self.dialog("print-preview", Pending::PrintText(title.clone(), body), Dialog {
            title: &title, description: "Drucken öffnet die Windows-Druckereinstellungen. Die Seiteneinteilung richtet sich nach dem gewählten Drucker.",
            value: &preview, ..Default::default()
        });
    }
}
