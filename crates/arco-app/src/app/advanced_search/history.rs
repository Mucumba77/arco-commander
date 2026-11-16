use super::*;

#[derive(Default)]
pub(crate) struct History {
    rows: Vec<Arc<Review>>,
    cursor: usize,
}
impl History {
    pub(super) fn push(&mut self, review: Arc<Review>) {
        self.rows.truncate(self.cursor.saturating_add(1));
        self.rows.push(review);
        self.cursor = self.rows.len() - 1;
        while self.rows.len() > 1
            && (self.rows.len() > 8
                || self
                    .rows
                    .iter()
                    .map(|row| row.found.directory.entries.len())
                    .sum::<usize>()
                    > 250000)
        {
            self.rows.remove(0);
            self.cursor -= 1;
        }
    }
    fn current(&self) -> Option<Arc<Review>> {
        self.rows.get(self.cursor).cloned()
    }
    fn navigate(&mut self, action: i32) {
        match action {
            0 if self.cursor > 0 => self.cursor -= 1,
            1 if self.cursor + 1 < self.rows.len() => self.cursor += 1,
            4 => {
                self.rows = self.current().into_iter().collect();
                self.cursor = 0;
            }
            _ => (),
        }
    }
    fn status(&self) -> String {
        format!(
            "{} / {}",
            if self.rows.is_empty() {
                0
            } else {
                self.cursor + 1
            },
            self.rows.len()
        )
    }
}

pub(super) fn publish_window_history(search: &SeparateSearch) {
    search
        .window
        .set_history_available(!search.history.rows.is_empty());
    search.window.set_history_back(search.history.cursor > 0);
    search
        .window
        .set_history_next(search.history.cursor + 1 < search.history.rows.len());
    search
        .window
        .set_history_status(search.history.status().into());
}

impl App {
    pub(super) fn publish_search_history(&self) {
        let ui = self.ui();
        ui.set_search_history_available(!self.search_history.rows.is_empty());
        ui.set_search_history_back(self.search_history.cursor > 0);
        ui.set_search_history_next(self.search_history.cursor + 1 < self.search_history.rows.len());
        ui.set_search_history_status(self.search_history.status().into());
    }

    pub(super) fn show_stored_search_review(&mut self, review: Arc<Review>) {
        let (title, list) = review_text(&review);
        let panel = &self.panels[review.setup.side];
        let current = panel.request == review.setup.request
            && panel.current().location() == review.setup.location();
        let ui = self.ui();
        ui.set_review_rows(review_rows(&review));
        ui.set_review_can_confirm(current && !review.found.directory.entries.is_empty());
        ui.set_review_can_edit(true);
        self.publish_search_history();
        self.dialog("search-results", Pending::SearchReview(review), Dialog {
            title: &title,
            description: if current { "Suchverlauf bleibt im Arbeitsspeicher. F2 sucht innerhalb dieser Treffer. Übernehmen zeigt sie als Dateiliste." } else { "Das ursprüngliche Dateifenster hat sich geändert. Dieses Ergebnis kann noch gelesen und kopiert werden." },
            value: &list, ..Default::default()
        });
    }

    pub(crate) fn search_result_action(&mut self, action: i32, _index: i32) {
        if !matches!(
            self.pending,
            Some(Pending::SearchReview(_) | Pending::AdvancedSearch(_))
        ) {
            return;
        }
        if matches!(action, 0 | 1 | 4 | 7) {
            self.search_history.navigate(action);
            if let Some(review) = self.search_history.current() {
                self.show_stored_search_review(review);
            }
            return;
        }
        let Some(review) = self.search_history.current() else {
            return;
        };
        if !matches!(action, 2 | 3) {
            return;
        }
        let mut setup = review.setup.clone();
        let mut options = review
            .criteria
            .clone()
            .unwrap_or_else(|| self.profile.search_last.clone());
        if action == 3 {
            if review.found.directory.entries.is_empty() {
                return;
            }
            global_archives::refine(&mut setup, &review.found.directory.entries);
            options.selected_only = true;
            options.roots.clear();
        }
        let ui = self.ui();
        ui.set_search_draft(to_draft(&options));
        ui.set_search_has_selection(!setup.selected.is_empty() || !setup.archive_scopes.is_empty());
        ui.set_search_archive(setup.archive.is_some());
        ui.set_search_base(setup.location().display().into());
        ui.set_search_notice(if action == 3 {
            "Nachsuche in den vorherigen Treffern. Tiefe 0 prüft nur die Einträge selbst.".into()
        } else {
            "".into()
        });
        self.pending = Some(Pending::AdvancedSearch(setup));
        ui.set_dialog_kind("advanced-search".into());
    }

    pub(super) fn separate_result_action(&mut self, id: u64, action: i32, _index: i32) {
        let Some(search) = self
            .separate_search
            .as_mut()
            .filter(|search| search.id == id && search.cancel.is_none())
        else {
            return;
        };
        if matches!(action, 0 | 1 | 4 | 7) {
            search.history.navigate(action);
            if let Some(review) = search.history.current() {
                let (title, list) = review_text(&review);
                search.window.set_result_rows(review_rows(&review));
                search.window.set_result_title(title.into());
                search.window.set_result_list(list.into());
                search
                    .window
                    .set_working_base(review.setup.location().display().into());
                search
                    .window
                    .set_can_use(!review.found.directory.entries.is_empty());
                search.window.set_has_result(true);
                search.window.set_notice("".into());
                search.review = Some(review);
            }
            publish_window_history(search);
            return;
        }
        let Some(review) = search.review.as_ref() else {
            return;
        };
        if !matches!(action, 2 | 3) {
            return;
        }
        let mut setup = review.setup.clone();
        let mut options = review
            .criteria
            .clone()
            .unwrap_or_else(|| search.criteria.clone());
        if action == 3 {
            if review.found.directory.entries.is_empty() {
                return;
            }
            global_archives::refine(&mut setup, &review.found.directory.entries);
            options.selected_only = true;
            options.roots.clear();
        }
        search.window.set_draft(to_draft(&options));
        search.window.set_base(setup.location().display().into());
        search
            .window
            .set_has_selection(!setup.selected.is_empty() || !setup.archive_scopes.is_empty());
        search.window.set_archive(setup.archive.is_some());
        search.window.set_has_result(false);
        search.window.set_notice(if action == 3 {
            "Nachsuche in den vorherigen Treffern. Tiefe 0 prüft nur die Einträge selbst.".into()
        } else {
            "".into()
        });
        search.setup = setup;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn review(name: &str) -> Arc<Review> {
        Arc::new(Review {
            setup: Setup {
                side: 0,
                request: 1,
                directory: PathBuf::from("root"),
                selected: Vec::new(),
                archive: None,
                refine: false,
                archive_scopes: Vec::new(),
                search_password: None,
            },
            found: Found {
                directory: Directory {
                    entries: vec![Entry::from_remote(
                        PathBuf::from("root").join(name),
                        Kind::File,
                        Some(1),
                        None,
                    )],
                    errors: Vec::new(),
                },
                cancelled: false,
            },
            duplicate_groups: None,
            criteria: Some(Options {
                names: name.into(),
                ..Default::default()
            }),
        })
    }
    #[test]
    fn history_keeps_shared_snapshots_and_branches_after_back_navigation() {
        let mut history = History::default();
        let a = review("a");
        history.push(a.clone());
        assert!(Arc::ptr_eq(&a, &history.current().unwrap()));
        history.push(review("b"));
        history.push(review("c"));
        history.navigate(0);
        assert_eq!(
            history.current().unwrap().criteria.as_ref().unwrap().names,
            "b"
        );
        history.push(review("d"));
        assert_eq!(history.rows.len(), 3);
        assert_eq!(
            history.current().unwrap().criteria.as_ref().unwrap().names,
            "d"
        );
        history.navigate(4);
        assert_eq!(history.rows.len(), 1);
        assert_eq!(
            history.current().unwrap().criteria.as_ref().unwrap().names,
            "d"
        );
        for index in 0..10 {
            history.push(review(&index.to_string()));
        }
        assert_eq!(history.rows.len(), 8);
        for _ in 0..20 {
            history.navigate(0);
        }
        assert_eq!(
            history.current().unwrap().criteria.as_ref().unwrap().names,
            "2"
        );
    }
}
