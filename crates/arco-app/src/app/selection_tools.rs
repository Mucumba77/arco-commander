use super::App;
use crate::commands::{Dialog, Pending};
use crate::display_path;
use arco_core::{files, profile};
use std::collections::BTreeSet;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

impl App {
    pub(crate) fn quick_find_dialog(&mut self) {
        self.quick_find_with_text("");
    }

    pub(super) fn quick_find_with_text(&mut self, text: &str) {
        self.dialog("input", Pending::QuickFind, Dialog {
            title: "Schnellsuche",
            description: "Springt zum nächsten sichtbaren Treffer und behält Auswahl und Filter bei.",
            placeholder: "Suchtext oder regulärer Ausdruck",
            value: text,
            options: &["Anfang des Namens", "Teil des Namens", "Regulärer Ausdruck"],
            ..Default::default()
        });
    }

    pub(crate) fn quick_find(&mut self, value: &str, mode: usize) -> io::Result<()> {
        if value.is_empty() {
            return Err(io::Error::other("Gib einen Suchtext ein."));
        }
        let regex = (mode == 2)
            .then(|| {
                regex::RegexBuilder::new(value)
                    .case_insensitive(true)
                    .build()
            })
            .transpose()
            .map_err(io::Error::other)?;
        let value = value.to_lowercase();
        let found = self.current_mut().find_visible(|entry| {
            let name = entry.name.to_string_lossy();
            match &regex {
                Some(regex) => regex.is_match(&name),
                None if mode == 1 => name.to_lowercase().contains(&value),
                None => name.to_lowercase().starts_with(&value),
            }
        });
        self.status(if found {
            "Nächster Treffer."
        } else {
            "Kein sichtbarer Treffer."
        });
        Ok(())
    }

    pub(crate) fn selection_dialog(&mut self, save: bool) {
        if !self.current().is_local() {
            self.status("Dateilisten benötigen ein lokales Dateifenster.");
            return;
        }
        let root = self.current().path.clone();
        let paths = if save {
            self.current().paths()
        } else {
            Vec::new()
        };
        if save && paths.is_empty() {
            self.status("Wähle Dateien oder Ordner.");
            return;
        }
        self.dialog("input", Pending::SelectionFile(root, save.then_some(paths)), Dialog {
            title: if save { "Auswahl in Dateiliste speichern" } else { "Auswahl aus Dateiliste laden" },
            description: "Je Zeile ein Dateiname oder Pfad. Relative Dateilisten liegen im aktiven Ordner.",
            value: if save { "auswahl.txt" } else { "" },
            placeholder: "Pfad der Dateiliste",
            ..Default::default()
        });
    }

    pub(crate) fn selection_file(
        &mut self,
        root: PathBuf,
        paths: Option<Vec<PathBuf>>,
        name: &str,
    ) -> io::Result<()> {
        if name.trim().is_empty() {
            return Err(io::Error::other("Gib den Pfad der Dateiliste ein."));
        }
        let target = root.join(name.trim().trim_matches('"'));
        if let Some(paths) = paths {
            let parent = target
                .parent()
                .ok_or_else(|| io::Error::other("Der Zielordner fehlt."))?;
            let mut output = tempfile::NamedTempFile::new_in(parent)?;
            for path in paths {
                let path = display_path(&path);
                if path.contains(['\r', '\n']) {
                    return Err(io::Error::other(
                        "Die Dateiliste kann keine Namen mit Zeilenumbrüchen speichern.",
                    ));
                }
                writeln!(output, "{path}")?;
            }
            output.as_file().sync_all()?;
            output
                .persist_noclobber(&target)
                .map_err(|error| error.error)?;
            self.refresh(self.active);
            self.status("Dateiliste gespeichert.");
        } else {
            files::regular_metadata(&target)?;
            self.select_from_text(&profile::read_tc_text(&target)?)?;
        }
        Ok(())
    }

    pub(crate) fn select_from_text(&mut self, text: &str) -> io::Result<()> {
        let root = self.current().path.clone();
        let expected: BTreeSet<_> = text
            .lines()
            .map(|line| line.trim_start_matches('\u{feff}').trim().trim_matches('"'))
            .filter(|line| !line.is_empty())
            .map(selection_key)
            .collect();
        if expected.is_empty() {
            return Err(io::Error::other("Die Dateiliste ist leer."));
        }
        let tab = self.current_mut();
        tab.remember_selection();
        tab.replace_selection(|entry| {
            expected.contains(&selection_key(&display_path(&entry.path)))
                || expected.contains(&selection_key(&entry.name.to_string_lossy()))
                || entry.path.strip_prefix(&root).ok().is_some_and(|relative| {
                    expected.contains(&selection_key(&display_path(relative)))
                })
        });
        self.status(format!(
            "{} Einträge markiert.",
            self.current().marked_paths().len()
        ));
        Ok(())
    }
}

fn selection_key(text: &str) -> String {
    if cfg!(windows) {
        text.replace('/', "\\").to_lowercase()
    } else {
        Path::new(text).to_string_lossy().into_owned()
    }
}
