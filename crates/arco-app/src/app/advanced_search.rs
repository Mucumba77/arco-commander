use super::{App, Outcome, strings};
use crate::commands::{Dialog, Pending};
use crate::panel::Location;
use crate::{SearchDraft, display_path};
use arco_core::advanced_search::{self, Found, Options};
use std::fmt::Write;
use std::io;
use std::path::PathBuf;

pub(crate) struct Setup {
    side: usize,
    request: u64,
    directory: PathBuf,
    selected: Vec<PathBuf>,
}

pub(crate) struct Review {
    setup: Setup,
    found: Found,
}

impl App {
    pub(crate) fn configure_everything_dialog(&mut self) {
        let value = self
            .profile
            .everything_cli
            .as_deref()
            .map(display_path)
            .unwrap_or_default();
        self.dialog("input", Pending::EverythingPath, Dialog {
            title: "Everything-Suche",
            description: "Absoluter Pfad zur Everything-Kommandozeile es.exe. Leerlassen deaktiviert die Anbindung.",
            value: &value, placeholder: "Pfad zu es.exe", ..Default::default()
        });
    }

    pub(crate) fn save_everything_path(&mut self, value: &str) -> io::Result<()> {
        if !self.save_profile {
            return Err(io::Error::other("Das Arco-Profil ist nicht verfügbar."));
        }
        let value = value.trim().trim_matches('"');
        let executable = (!value.is_empty()).then(|| PathBuf::from(value));
        if let Some(path) = &executable
            && (!path.is_absolute()
                || !path
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("exe")))
        {
            return Err(io::Error::other("Gib den absoluten EXE-Pfad an."));
        }
        let mut draft = self.profile_snapshot();
        draft.everything_cli = executable;
        draft.save(&self.profile_path)?;
        self.profile = draft;
        self.status("Everything-Einstellung gespeichert.");
        Ok(())
    }

    pub(crate) fn open_advanced_search(&mut self) {
        let tab = self.current();
        let setup = Setup {
            side: self.active,
            request: self.panels[self.active].request,
            directory: tab.path.clone(),
            selected: tab.marked_paths(),
        };
        let ui = self.ui();
        let mut options = self.profile.search_last.clone();
        options.selected_only &= !setup.selected.is_empty();
        ui.set_search_draft(to_draft(&options));
        ui.set_search_notice("".into());
        ui.set_search_has_selection(!setup.selected.is_empty());
        ui.set_search_base(display_path(&setup.directory).into());
        self.publish_search_profiles();
        self.pending = Some(Pending::AdvancedSearch(setup));
        ui.set_dialog_kind("advanced-search".into());
    }

    fn publish_search_profiles(&self) {
        self.ui().set_search_profiles(strings(
            self.profile.search_profiles.keys().map(String::as_str),
        ));
    }

    pub(super) fn configure_search(&mut self, data: SearchDraft, name: &str, operation: i32) {
        if let Err(error) = self.configure_search_result(data, name, operation) {
            self.ui().set_search_notice(error.to_string().into());
        }
    }

    fn configure_search_result(
        &mut self,
        data: SearchDraft,
        name: &str,
        operation: i32,
    ) -> io::Result<()> {
        let Some(Pending::AdvancedSearch(setup)) = self.pending.as_ref() else {
            return Ok(());
        };
        let ui = self.ui();
        ui.set_search_notice("".into());
        if operation == 0 {
            let mut options = self
                .profile
                .search_profiles
                .get(name)
                .cloned()
                .ok_or_else(|| io::Error::other("Wähle ein Suchprofil."))?;
            options.selected_only &= !setup.selected.is_empty();
            ui.set_search_draft(to_draft(&options));
            return Ok(());
        }
        let options = from_draft(data);
        let indexed = options
            .everything()
            .map(|(scoped, expression)| (scoped, expression.to_owned()));
        let mut local_options = options.clone();
        if indexed.is_some() {
            local_options.names.clear();
        }
        if operation == 1 || operation == 3 {
            if !self.save_profile {
                return Err(io::Error::other("Das Arco-Profil ist nicht verfügbar."));
            }
            let mut draft = self.profile_snapshot();
            let name = name.trim();
            if operation == 3 {
                if draft.search_profiles.remove(name).is_none() {
                    return Err(io::Error::other("Wähle ein Suchprofil."));
                }
            } else {
                local_options.compile()?;
                if name.is_empty()
                    || name.chars().count() > 80
                    || name.chars().any(char::is_control)
                {
                    return Err(io::Error::other(
                        "Gib einen Profilnamen mit höchstens 80 Zeichen ein.",
                    ));
                }
                draft.search_profiles.insert(name.to_owned(), options);
            }
            draft.save(&self.profile_path)?;
            self.profile = draft;
            self.publish_search_profiles();
            ui.set_search_notice(
                if operation == 3 {
                    "Suchprofil entfernt."
                } else {
                    "Suchprofil gespeichert."
                }
                .into(),
            );
            return Ok(());
        }
        let query = local_options.compile()?;
        if self.job.is_some() {
            return Err(io::Error::other("Ein Auftrag läuft bereits."));
        }
        let roots = if options.selected_only {
            if setup.selected.is_empty() {
                return Err(io::Error::other(
                    "Markiere vor der Suche Dateien oder Ordner.",
                ));
            }
            setup.selected.clone()
        } else {
            let paths: Vec<_> = options
                .roots
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(|line| setup.directory.join(line.trim_matches('"')))
                .collect();
            if paths.is_empty() {
                vec![setup.directory.clone()]
            } else {
                paths
            }
        };
        let Some(Pending::AdvancedSearch(setup)) = self.pending.take() else {
            return Ok(());
        };
        self.profile.search_last = options.clone();
        self.save();
        self.close_dialog();
        let show_hidden = self.profile.show_hidden;
        let ignore = self.ignore_list();
        let executable = self.profile.everything_cli.clone();
        self.start_job("Erweiterte Suche …", false, move |job| {
            let found = if let Some((scoped, expression)) = indexed {
                let result = executable.ok_or_else(|| io::Error::other("Everything ist nicht eingerichtet."))
                    .and_then(|executable| arco_core::everything::search(
                        &executable, scoped.then_some(roots.as_slice()), &expression, &query, show_hidden, ignore.as_deref(), &job.cancel,
                    ));
                match result {
                    Ok(directory) => Found { directory, cancelled: false },
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => return Err(error),
                    Err(error) => {
                        local_options.names = fallback_names(&expression).ok_or_else(|| io::Error::other(format!("{error} Für diese Suchsyntax ist Everything erforderlich.")))?;
                        let mut found = advanced_search::search(
                            &roots, options.selected_only, &local_options.compile()?, show_hidden, ignore.as_deref(), &job.cancel,
                        );
                        found.directory.errors.insert(0, format!("{error} Stattdessen wurden die ausgewählten lokalen Suchorte durchsucht."));
                        found
                    }
                }
            } else {
                advanced_search::search(&roots, options.selected_only, &query, show_hidden, ignore.as_deref(), &job.cancel)
            };
            Ok(Outcome::SearchReview(Box::new(Review { setup, found })))
        });
        Ok(())
    }

    pub(super) fn show_search_review(&mut self, review: Box<Review>) {
        let found = &review.found;
        let mut list = String::new();
        for entry in found.directory.entries.iter().take(1000) {
            writeln!(list, "{}", display_path(&entry.path)).unwrap();
        }
        if found.directory.entries.len() > 1000 {
            writeln!(
                list,
                "… Weitere Treffer können im Dateifenster angezeigt werden."
            )
            .unwrap();
        }
        if !found.directory.errors.is_empty() {
            writeln!(
                list,
                "\nLesefehler\n{}",
                found
                    .directory
                    .errors
                    .iter()
                    .take(30)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("\n")
            )
            .unwrap();
        }
        let title = format!(
            "{} Suchtreffer{}",
            found.directory.entries.len(),
            if found.cancelled {
                " · Suche abgebrochen"
            } else {
                ""
            }
        );
        self.dialog("search-results", Pending::SearchReview(review), Dialog {
            title: &title, description: "Übernimm die Treffer als Dateiliste oder behalte die vorherige Ansicht.",
            value: &list, ..Default::default()
        });
    }

    pub(crate) fn use_search_results(&mut self, review: Review) {
        self.completed(Outcome::Entries {
            side: review.setup.side,
            location: Location::Local(review.setup.directory),
            request: review.setup.request,
            entries: review.found.directory,
            scope: arco_core::view_settings::Scope::Search,
            label: if review.found.cancelled {
                "Teilergebnis".into()
            } else {
                "Suchtreffer".into()
            },
        });
    }
}

fn fallback_names(expression: &str) -> Option<String> {
    if expression.contains([':', '<', '>', '|', '&', '"', '\\', '/', '!', '[', ']']) {
        None
    } else if expression.is_empty() {
        Some("*".into())
    } else if expression.contains(['*', '?', ';']) {
        Some(expression.to_owned())
    } else {
        Some(format!("*{expression}*"))
    }
}

fn to_draft(options: &Options) -> SearchDraft {
    SearchDraft {
        names: options.names.as_str().into(),
        content: options.content.as_str().into(),
        content_mode: i32::from(options.content_mode),
        encoding: i32::from(options.encoding),
        match_case: options.match_case,
        minimum_bytes: options.minimum_bytes.as_str().into(),
        maximum_bytes: options.maximum_bytes.as_str().into(),
        modified_from: options.modified_from.as_str().into(),
        modified_to: options.modified_to.as_str().into(),
        attributes: options.attributes.as_str().into(),
        depth: options.depth.as_str().into(),
        roots: options.roots.as_str().into(),
        selected_only: options.selected_only,
    }
}

fn from_draft(data: SearchDraft) -> Options {
    Options {
        names: data.names.to_string(),
        content: data.content.to_string(),
        content_mode: data.content_mode.clamp(0, 3) as u8,
        encoding: data.encoding.clamp(0, 4) as u8,
        match_case: data.match_case,
        minimum_bytes: data.minimum_bytes.to_string(),
        maximum_bytes: data.maximum_bytes.to_string(),
        modified_from: data.modified_from.to_string(),
        modified_to: data.modified_to.to_string(),
        attributes: data.attributes.to_string(),
        depth: data.depth.to_string(),
        roots: data.roots.to_string(),
        selected_only: data.selected_only,
    }
}
