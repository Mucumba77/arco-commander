use super::{App, Directory, Found, Outcome, Review};
use crate::commands::{Dialog, Pending};
use crate::{DuplicateFolder, DuplicateForm, ReviewRow, display_path};
use arco_core::tools::{self, DuplicateSelection};
use slint::{Model, ModelRc, VecModel};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;

pub(crate) struct State {
    review: Box<Review>,
    folders: Vec<PathBuf>,
    requested: Option<DuplicateSelection>,
    applied: Option<(DuplicateSelection, HashSet<PathBuf>)>,
}

fn key(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/").to_lowercase()
}

impl State {
    fn selection(&self, form: &DuplicateForm) -> io::Result<DuplicateSelection> {
        let mut folders = Vec::new();
        let mut ids = HashSet::new();
        for row in form.folders.iter() {
            let id = usize::try_from(row.id)
                .map_err(|_| io::Error::other("Ungültiger Ordner in der Duplikatauswahl."))?;
            let folder = self
                .folders
                .get(id)
                .filter(|_| ids.insert(id))
                .ok_or_else(|| {
                    io::Error::other("Der Duplikatordner fehlt oder ist doppelt vorhanden.")
                })?;
            if row.selected {
                folders.push(folder.clone());
            }
        }
        let choice =
            |value| u8::try_from(value).map_err(|_| io::Error::other("Ungültige Auswahlregel."));
        Ok(DuplicateSelection {
            names: form.names.to_string(),
            exclude: form.exclude.to_string(),
            folders,
            include_subfolders: form.recursive,
            date: choice(form.date)?,
            size: choice(form.size)?,
            keep_one: form.keep_one,
            keep: [
                choice(form.keep_first)?,
                choice(form.keep_second)?,
                choice(form.keep_third)?,
            ],
        })
    }
}

impl App {
    pub(super) fn show_duplicate_selection(&mut self, review: Box<Review>) {
        let folders = review
            .found
            .directory
            .entries
            .iter()
            .filter_map(|entry| entry.path.parent())
            .map(|path| (key(path), path.to_path_buf()))
            .collect::<BTreeMap<_, _>>()
            .into_values()
            .collect::<Vec<_>>();
        let rows = folders
            .iter()
            .enumerate()
            .map(|(index, path)| DuplicateFolder {
                id: index as i32,
                path: display_path(path).into(),
                selected: true,
            })
            .collect::<Vec<_>>();
        let form = DuplicateForm {
            names: "*".into(),
            recursive: true,
            folders: Rc::new(VecModel::from(rows)).into(),
            keep_one: true,
            keep_first: 3,
            keep_second: 5,
            keep_third: 0,
            ..Default::default()
        };
        let ui = self.ui();
        ui.set_duplicate_form(form);
        ui.set_duplicate_generation(0);
        ui.set_duplicate_summary("Vorschau auswählen und prüfen.".into());
        ui.set_review_rows(rows_for(&review, &HashSet::new()));
        self.dialog(
            "duplicate-selection",
            Pending::DuplicateSelection(Box::new(State {
                review,
                folders,
                requested: None,
                applied: None,
            })),
            Dialog {
                title: "Duplikate markieren",
                ..Default::default()
            },
        );
    }

    pub(crate) fn duplicate_selection_from_panel(&mut self) -> bool {
        let Some(groups) = self.current().duplicate_groups.clone() else {
            return false;
        };
        let entries = self
            .current()
            .all_entries()
            .iter()
            .map(|entry| (entry.path.clone(), entry.clone()))
            .collect::<HashMap<_, _>>();
        let groups = groups
            .into_iter()
            .map(|group| {
                group
                    .into_iter()
                    .filter_map(|path| entries.get(&path).cloned())
                    .collect::<Vec<_>>()
            })
            .filter(|group| !group.is_empty())
            .collect::<Vec<_>>();
        let counts = groups.iter().map(Vec::len).collect();
        let entries = groups.into_iter().flatten().collect();
        let review = Review {
            criteria: None,
            setup: self.search_setup(),
            found: Found {
                directory: Directory {
                    entries,
                    errors: Vec::new(),
                },
                cancelled: false,
            },
            duplicate_groups: Some(counts),
        };
        self.show_duplicate_selection(Box::new(review));
        true
    }

    pub(crate) fn duplicate_action(&mut self, action: i32, form: DuplicateForm, index: i32) {
        if self.job.is_some() {
            return;
        }
        if action == 2 {
            self.close_dialog();
            return;
        }
        if !matches!(self.pending, Some(Pending::DuplicateSelection(_))) {
            return;
        }
        if action == 9 {
            let text = self
                .ui()
                .get_review_rows()
                .iter()
                .map(|row| format!("{}\t{}\n{}", row.primary, row.secondary, row.detail))
                .collect::<Vec<_>>()
                .join("\n\n");
            let result = arco_windows::copy_text(&text);
            self.ui().set_dialog_notice(match result {
                Ok(()) => "In die Zwischenablage kopiert.".into(),
                Err(error) => error.to_string().into(),
            });
            return;
        }
        let result = if action >= 3 {
            self.duplicate_folders(action, &form, index)
        } else if action == 1 {
            self.accept_duplicate_selection(&form)
        } else {
            self.preview_duplicate_selection(&form)
        };
        if let Err(error) = result {
            self.ui().set_dialog_notice(error.to_string().into());
        }
    }

    fn duplicate_folders(
        &mut self,
        action: i32,
        form: &DuplicateForm,
        index: i32,
    ) -> io::Result<()> {
        let Some(Pending::DuplicateSelection(state)) = &self.pending else {
            return Ok(());
        };
        let mut rows = form.folders.iter().collect::<Vec<_>>();
        let mut changes = Vec::new();
        for (at, row) in rows.iter().enumerate() {
            let path = state
                .folders
                .get(row.id as usize)
                .ok_or_else(|| io::Error::other("Der Ordner fehlt."))?;
            let selected = match action {
                3 if at == index as usize => Some(!row.selected),
                4 | 5
                    if tools::duplicate_folder_matches(
                        path,
                        &form.pattern,
                        form.direct_parent,
                    )? =>
                {
                    Some(action == 4)
                }
                6 => Some(true),
                7 => Some(false),
                8 => Some(!row.selected),
                _ => None,
            };
            if let Some(value) = selected {
                changes.push((path.clone(), value));
            }
        }
        for row in &mut rows {
            let path = &state.folders[row.id as usize];
            for (parent, selected) in &changes {
                if path == parent || (form.recursive && path.starts_with(parent)) {
                    row.selected = *selected;
                }
            }
        }
        for (index, row) in rows.into_iter().enumerate() {
            form.folders.set_row_data(index, row);
        }
        self.ui().set_dialog_notice("".into());
        Ok(())
    }

    fn preview_duplicate_selection(&mut self, form: &DuplicateForm) -> io::Result<()> {
        let Some(Pending::DuplicateSelection(state)) = &mut self.pending else {
            return Ok(());
        };
        let options = state.selection(form)?;
        let entries = state.review.found.directory.entries.clone();
        let groups = state.review.duplicate_groups.clone().unwrap_or_default();
        state.requested = Some(options.clone());
        state.applied = None;
        let profiles = self
            .profile
            .search_profiles
            .iter()
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect::<HashMap<_, _>>();
        let filters = [&options.names, &options.exclude]
            .into_iter()
            .filter_map(|value| value.trim().strip_prefix('>'))
            .map(|name| {
                let name = name.trim();
                let options = profiles
                    .get(name)
                    .ok_or_else(|| io::Error::other("Der Suchfilter fehlt."))?;
                self.wdx_filter(options)
                    .map(|filter| (name.to_owned(), filter))
            })
            .collect::<io::Result<HashMap<_, _>>>()?;
        let revision = self.ui().get_dialog_revision();
        self.ui()
            .set_dialog_notice("Auswahl wird vorbereitet …".into());
        let description = super::super::JobDescription::new(
            crate::OperationKind::Search,
            format!("{} Dateien", entries.len()),
            String::new(),
        );
        self.start_operation_job(
            "Duplikatauswahl vorbereiten",
            false,
            description,
            move |job| {
                let mut cache = HashMap::new();
                let result = tools::select_duplicates_with_profiles(
                    &entries,
                    &groups,
                    &options,
                    &profiles,
                    &job.cancel,
                    |name, path| {
                        let key = (name.to_owned(), path.to_path_buf());
                        if let Some(value) = cache.get(&key) {
                            return Ok(*value);
                        }
                        let value = filters
                            .get(name)
                            .and_then(Option::as_ref)
                            .map_or(Ok(true), |filter| filter.matches_file(path, &job.cancel))?;
                        cache.insert(key, value);
                        Ok(value)
                    },
                )
                .map_err(|error| error.to_string());
                Ok(Outcome::DuplicateSelection(revision, result))
            },
        );
        Ok(())
    }

    pub(crate) fn complete_duplicate_selection(
        &mut self,
        revision: i32,
        result: Result<HashSet<PathBuf>, String>,
    ) {
        if self.ui().get_dialog_revision() != revision
            || self.ui().get_dialog_kind() != "duplicate-selection"
        {
            return;
        }
        let Some(Pending::DuplicateSelection(state)) = &mut self.pending else {
            return;
        };
        match result {
            Ok(marks) => {
                let Some(options) = state.requested.take() else {
                    return;
                };
                let rows = rows_for(&state.review, &marks);
                let summary = format!(
                    "{} / {} Dateien markiert · {} Gruppen",
                    marks.len(),
                    state.review.found.directory.entries.len(),
                    state.review.duplicate_groups.as_ref().map_or(0, Vec::len)
                );
                state.applied = Some((options, marks));
                let ui = self.ui();
                ui.set_review_rows(rows);
                ui.set_duplicate_summary(summary.into());
                ui.set_dialog_notice("".into());
                ui.set_duplicate_generation(ui.get_duplicate_generation().wrapping_add(1).max(1));
            }
            Err(error) => self.ui().set_dialog_notice(error.into()),
        }
    }

    fn accept_duplicate_selection(&mut self, form: &DuplicateForm) -> io::Result<()> {
        let Some(Pending::DuplicateSelection(state)) = &self.pending else {
            return Ok(());
        };
        let options = state.selection(form)?;
        if !state
            .applied
            .as_ref()
            .is_some_and(|(applied, _)| *applied == options)
        {
            return Err(io::Error::other(
                "Aktualisiere zuerst die Vorschau der Duplikatauswahl.",
            ));
        }
        let Some(Pending::DuplicateSelection(mut state)) = self.pending.take() else {
            return Ok(());
        };
        let marks = state.applied.take().unwrap().1;
        let side = state.review.setup.side;
        let request = state.review.setup.request;
        let location = state.review.setup.location();
        self.close_dialog();
        self.use_search_results(*state.review, 0);
        if self.panels[side].request == request
            && self.panels[side].current().location() == location
        {
            self.panels[side]
                .current_mut()
                .replace_selection(|entry| marks.contains(&entry.path));
            self.publish(side);
        }
        Ok(())
    }
}

fn rows_for(review: &Review, marks: &HashSet<PathBuf>) -> ModelRc<ReviewRow> {
    let mut rows = super::review_rows(review).iter().collect::<Vec<_>>();
    for (row, entry) in rows.iter_mut().zip(&review.found.directory.entries) {
        row.primary = format!(
            "{} {}",
            if marks.contains(&entry.path) {
                "✓"
            } else {
                "—"
            },
            entry.name.to_string_lossy()
        )
        .into();
    }
    Rc::new(VecModel::from(rows)).into()
}
