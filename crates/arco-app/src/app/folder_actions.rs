use super::{App, Outcome};
use crate::{display_path, human_size};
use arco_core::files;
use std::collections::{BTreeMap, HashSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

impl App {
    pub(crate) fn measure_files(&mut self, show_in_list: bool) {
        let side = self.active;
        let panel = &self.panels[side];
        if panel.loading {
            self.status("Warte, bis das Dateifenster geladen ist.");
            return;
        }
        let tab = panel.current();
        let paths = if show_in_list {
            tab.visible_directories()
        } else {
            tab.paths()
        };
        if paths.is_empty() {
            self.status(if show_in_list {
                "Keine Ordner in der Dateiliste."
            } else {
                "Wähle Dateien oder Ordner."
            });
            return;
        }
        let root = tab.path.clone();
        let request = panel.request;
        self.start_job("Berechne Dateigrößen …", false, move |job| {
            if !show_in_list {
                let mut allocation = 0u64;
                let mut allocation_errors = Vec::new();
                let mut seen_links = HashSet::new();
                let usage = files::measure_with(&paths, &job.cancel, |path| {
                    match arco_windows::file_allocation(path) {
                        Ok(file) if file.hardlink_id.is_none_or(|id| seen_links.insert(id)) => {
                            if let Some(total) = allocation.checked_add(file.bytes) {
                                allocation = total;
                            } else {
                                allocation_errors.push("Belegter Speicher überschreitet den Zählbereich.".into());
                            }
                        }
                        Ok(_) => (),
                        Err(error) => allocation_errors.push(format!("{}: {error}", display_path(path))),
                    }
                })?;
                let allocated_label = if allocation_errors.is_empty() && usage.errors.is_empty() {
                    "Belegter Speicher"
                } else {
                    "Belegter Speicher (teilweise ermittelt)"
                };
                let mut text = format!(
                    "{}\n\nLogische Gesamtgröße    {}\n{allocated_label}    {}\nDateien    {}\nOrdner    {}\nÜbersprungene Verknüpfungen    {}",
                    display_path(&root), human_size(usage.bytes), human_size(allocation), usage.files, usage.directories, usage.links,
                );
                if !usage.errors.is_empty() {
                    writeln!(text, "\n\n{} Lesefehler", usage.errors.len()).unwrap();
                    text.push_str(&usage.errors.join("\n"));
                }
                if !allocation_errors.is_empty() {
                    writeln!(text, "\n\n{} Fehler bei der Speicherabfrage", allocation_errors.len()).unwrap();
                    text.push_str(&allocation_errors.join("\n"));
                }
                return Ok(Outcome::Text("Größe der Auswahl".into(), text));
            }
            let mut sizes = BTreeMap::new();
            let mut skipped = 0;
            let mut last = Instant::now() - Duration::from_secs(1);
            for path in paths {
                if last.elapsed() >= Duration::from_millis(150) {
                    last = Instant::now();
                    job.progress(format!("Berechne {}", display_path(&path)));
                }
                let usage = files::measure(std::slice::from_ref(&path), &job.cancel)?;
                if usage.directories > 0 && usage.errors.is_empty() {
                    sizes.insert(path, usage.bytes);
                } else {
                    skipped += 1;
                }
            }
            Ok(Outcome::FolderSizes { side, request, root, sizes, skipped })
        });
    }

    pub(super) fn show_folder_sizes(
        &mut self,
        side: usize,
        request: u64,
        root: &Path,
        sizes: BTreeMap<PathBuf, u64>,
        skipped: usize,
    ) {
        let panel = &mut self.panels[side];
        if panel.request != request
            || panel.loading
            || panel.current().path != root
            || !panel.current().is_local()
        {
            self.status("Das Dateifenster hat sich während der Berechnung geändert.");
            return;
        }
        panel
            .current_mut()
            .set_folder_sizes(&sizes, self.profile.show_hidden);
        self.publish(side);
        self.status(format!(
            "{} Ordnergrößen berechnet · {skipped} Ordner übersprungen.",
            sizes.len()
        ));
    }
}
