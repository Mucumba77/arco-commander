use super::{App, Event, Outcome, strings};
use crate::commands::{Dialog, Pending};
use crate::panel::{ArchivePassword, ArchiveTab, Location};
use crate::{SearchDraft, SearchWindow, display_path};
use arco_core::advanced_search::{self, Found, Options};
use arco_core::files::{self, Directory, Entry, Kind};
use slint::ComponentHandle;
use std::fmt::Write;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

mod worker;
pub(crate) use worker::run as run_search_worker;

#[derive(Clone)]
pub(crate) struct Setup {
    side: usize,
    request: u64,
    directory: PathBuf,
    selected: Vec<PathBuf>,
    archive: Option<ArchiveTab>,
}

impl Setup {
    fn location(&self) -> Location {
        self.archive.as_ref().map_or_else(
            || Location::Local(self.directory.clone()),
            |archive| Location::Archive {
                file: archive.file.clone(),
                folder: archive.folder.clone(),
            },
        )
    }
}

pub(crate) struct Review {
    setup: Setup,
    found: Found,
    duplicate_groups: Option<Vec<usize>>,
}

pub(super) struct SeparateSearch {
    id: u64,
    window: SearchWindow,
    setup: Setup,
    cancel: Option<Arc<AtomicBool>>,
    review: Option<Review>,
}

pub(super) enum WindowAction {
    Configure(Options, String, i32, Option<ArchivePassword>),
    Cancel,
    Close,
    Use,
    Finished(io::Result<Found>),
}

struct SearchTask {
    query: advanced_search::Query,
    roots: Vec<PathBuf>,
    options: Options,
    indexed: Option<(bool, String)>,
    show_hidden: bool,
    ignore: Option<Arc<arco_core::ignore::IgnoreList>>,
    executable: Option<PathBuf>,
    archive: Option<ArchiveTab>,
    password: Option<ArchivePassword>,
    wdx: Option<super::wdx::search::ProfileFilter>,
}

impl SearchTask {
    fn run(mut self, cancel: &AtomicBool) -> io::Result<Found> {
        if let Some(archive) = &self.archive {
            return self.run_archive(archive, cancel);
        }
        let mut found = self.run_local(cancel)?;
        if let Some(wdx) = &self.wdx {
            if found.cancelled {
                found.directory.entries.clear();
            } else {
                wdx.filter(&mut found.directory.entries, cancel)?;
            }
        }
        Ok(found)
    }

    fn run_archive(&self, archive: &ArchiveTab, cancel: &AtomicBool) -> io::Result<Found> {
        use arco_core::archive_view::{self, Selection};
        let content = self.query.requires_content();
        let reads_files = content || self.wdx.is_some();
        let password = self
            .password
            .as_deref()
            .map(|password| password.as_str())
            .or(archive.password());
        archive.catalog.search_candidates(
            &self.roots,
            &self.query,
            self.show_hidden,
            cancel,
            |row| {
                if !reads_files {
                    return Ok(true);
                }
                if row.kind != Kind::File
                    || self
                        .wdx
                        .as_ref()
                        .is_some_and(|filter| !filter.accepts_name(row))
                {
                    return Ok(false);
                }
                let staging = tempfile::tempdir()?;
                let selection = Selection::preview(&row.path)?;
                archive_view::extract(&archive.file, staging.path(), &selection, password, cancel)?;
                let file = staging.path().join(&row.name);
                if content {
                    let mut entry = row.clone();
                    entry.path.clone_from(&file);
                    if !self.query.matches(&entry, cancel)? {
                        return Ok(false);
                    }
                }
                self.wdx
                    .as_ref()
                    .map_or(Ok(true), |filter| filter.matches_file(&file, cancel))
            },
        )
    }

    fn run_local(&mut self, cancel: &AtomicBool) -> io::Result<Found> {
        let mut fallback = None;
        if let Some((scoped, expression)) = self.indexed.take() {
            let result = self
                .executable
                .take()
                .ok_or_else(|| io::Error::other("Everything ist nicht eingerichtet."))
                .and_then(|executable| {
                    arco_core::everything::search(
                        &executable,
                        scoped.then_some(self.roots.as_slice()),
                        &expression,
                        &self.query,
                        self.show_hidden,
                        self.ignore.as_deref(),
                        cancel,
                    )
                });
            match result {
                Ok(directory) => {
                    return Ok(Found {
                        directory,
                        cancelled: false,
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => return Err(error),
                Err(error) => {
                    self.options.names = fallback_names(&expression).ok_or_else(|| {
                        io::Error::other(format!(
                            "{error} Für diese Suchsyntax ist Everything erforderlich."
                        ))
                    })?;
                    self.query = self.options.compile()?;
                    fallback = Some(format!(
                        "{error} Stattdessen wurden die ausgewählten lokalen Suchorte durchsucht."
                    ));
                }
            }
        }
        let mut found = advanced_search::search(
            &self.roots,
            self.options.selected_only,
            &self.query,
            self.show_hidden,
            self.ignore.as_deref(),
            cancel,
        );
        if let Some(message) = fallback {
            found.directory.errors.insert(0, message);
        }
        Ok(found)
    }
}

pub(crate) fn duplicate_result(
    side: usize,
    request: u64,
    directory: PathBuf,
    groups: Vec<Vec<PathBuf>>,
    cancel: &AtomicBool,
) -> io::Result<Outcome> {
    let mut entries = Vec::new();
    let mut duplicate_groups = Vec::with_capacity(groups.len());
    for group in groups {
        duplicate_groups.push(group.len());
        for path in group {
            files::check_cancel(cancel)?;
            let name = path.file_name().unwrap_or_default().to_os_string();
            let entry = Entry::read(path, name)?;
            if entry.kind != Kind::File {
                return Err(io::Error::other(
                    "Eine Datei hat sich während der Duplikatsuche geändert.",
                ));
            }
            entries.push(entry);
        }
    }
    Ok(Outcome::SearchReview(Box::new(Review {
        setup: Setup {
            side,
            request,
            directory,
            selected: Vec::new(),
            archive: None,
        },
        found: Found {
            directory: Directory {
                entries,
                errors: Vec::new(),
            },
            cancelled: false,
        },
        duplicate_groups: Some(duplicate_groups),
    })))
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

    fn search_setup(&self) -> Setup {
        let tab = self.current();
        Setup {
            side: self.active,
            request: self.panels[self.active].request,
            directory: tab.path.clone(),
            selected: tab.marked_paths(),
            archive: tab.archive.clone(),
        }
    }

    pub(crate) fn open_advanced_search(&mut self) {
        let setup = self.search_setup();
        let ui = self.ui();
        let mut options = self.profile.search_last.clone();
        options.selected_only &= !setup.selected.is_empty();
        ui.set_search_draft(to_draft(&options));
        ui.set_search_notice("".into());
        ui.set_search_has_selection(!setup.selected.is_empty());
        ui.set_search_archive(setup.archive.is_some());
        ui.set_search_base(setup.location().display().into());
        self.publish_search_profiles();
        self.pending = Some(Pending::AdvancedSearch(setup));
        ui.set_dialog_kind("advanced-search".into());
    }

    pub(super) fn publish_search_profiles(&self) {
        let profiles = strings(self.profile.search_profiles.keys().map(String::as_str));
        let wdx = strings(self.profile.wdx_search_profiles.keys().map(String::as_str));
        self.ui().set_search_profiles(profiles.clone());
        self.ui().set_search_wdx_profiles(wdx.clone());
        if let Some(search) = &self.separate_search {
            search.window.set_profiles(profiles);
            search.window.set_wdx_profiles(wdx);
        }
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
        let setup = setup.clone();
        let password = search_password(&data);
        let options = from_draft(data);
        if operation != 2 {
            let (draft, notice) = self.search_profile(&setup, options, name, operation)?;
            if let Some(draft) = draft {
                ui.set_search_draft(draft);
            }
            ui.set_search_notice(notice.into());
            return Ok(());
        }
        if self.job.is_some() {
            return Err(io::Error::other("Ein Auftrag läuft bereits."));
        }
        let task = self.search_task(&setup, options.clone(), password)?;
        self.profile.search_last = options;
        self.save();
        self.close_dialog();
        self.start_job("Erweiterte Suche …", false, move |job| {
            let found = task.run(&job.cancel)?;
            Ok(Outcome::SearchReview(Box::new(Review {
                setup,
                found,
                duplicate_groups: None,
            })))
        });
        Ok(())
    }

    fn search_profile(
        &mut self,
        setup: &Setup,
        options: Options,
        name: &str,
        operation: i32,
    ) -> io::Result<(Option<SearchDraft>, &'static str)> {
        if operation == 0 {
            let mut options = self
                .profile
                .search_profiles
                .get(name)
                .cloned()
                .ok_or_else(|| io::Error::other("Wähle ein Suchprofil."))?;
            options.selected_only &= !setup.selected.is_empty();
            return Ok((Some(to_draft(&options)), ""));
        }
        if matches!(operation, 1 | 3) {
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
                compile_search(&options)?;
                self.wdx_filter(&options)?;
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
            return Ok((
                None,
                if operation == 3 {
                    "Suchprofil entfernt."
                } else {
                    "Suchprofil gespeichert."
                },
            ));
        }
        Err(io::ErrorKind::InvalidInput.into())
    }

    fn wdx_filter(
        &self,
        options: &Options,
    ) -> io::Result<Option<super::wdx::search::ProfileFilter>> {
        let name = options.wdx_profile.trim();
        if name.is_empty() {
            return Ok(None);
        }
        let profile = self
            .profile
            .wdx_search_profiles
            .get(name)
            .ok_or_else(|| io::Error::other(format!("Das WDX-Suchprofil „{name}“ fehlt.")))?;
        super::wdx::search::ProfileFilter::compile(profile, &self.profile.plugins).map(Some)
    }

    fn search_task(
        &self,
        setup: &Setup,
        options: Options,
        password: Option<ArchivePassword>,
    ) -> io::Result<SearchTask> {
        let query = compile_search(&options)?;
        let wdx = self.wdx_filter(&options)?;
        if setup.archive.is_some() && options.everything().is_some() {
            return Err(io::Error::other(
                "Everything durchsucht Dateisysteme. Entferne das Präfix für die Suche im Archiv.",
            ));
        }
        let roots = if options.selected_only {
            if setup.selected.is_empty() {
                return Err(io::Error::other(
                    "Markiere vor der Suche Dateien oder Ordner.",
                ));
            }
            setup.selected.clone()
        } else {
            let base = setup
                .archive
                .as_ref()
                .map_or(setup.directory.as_path(), |archive| {
                    archive.folder.as_path()
                });
            let paths = options
                .roots
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(|line| {
                    let path = Path::new(line.trim_matches('"'));
                    if setup.archive.is_some()
                        && path
                            .components()
                            .any(|part| !matches!(part, Component::Normal(_) | Component::CurDir))
                    {
                        return Err(io::Error::other(
                            "Suchorte im Archiv müssen relativ zum geöffneten Archivordner sein.",
                        ));
                    }
                    Ok(base.join(path))
                })
                .collect::<io::Result<Vec<_>>>()?;
            if paths.is_empty() {
                vec![base.to_path_buf()]
            } else {
                paths
            }
        };
        if roots.len() > 256 && setup.archive.is_some() {
            return Err(io::Error::other("Wähle bis zu 256 Suchorte im Archiv."));
        }
        Ok(SearchTask {
            query,
            roots,
            indexed: options
                .everything()
                .map(|(scoped, expression)| (scoped, expression.to_owned())),
            options,
            show_hidden: self.profile.show_hidden,
            ignore: self.ignore_list(),
            executable: self.profile.everything_cli.clone(),
            archive: setup.archive.clone(),
            password,
            wdx,
        })
    }

    pub(crate) fn open_separate_search(&mut self) {
        if let Some(search) = &self.separate_search {
            use slint::winit_030::WinitWindowAccessor as _;
            search
                .window
                .window()
                .with_winit_window(|window| window.focus_window());
            return;
        }
        if !self.current().is_local() && self.current().archive.is_none() {
            self.status("Öffne für die Suche einen lokalen Ordner oder ein Archiv.");
            return;
        }
        if let Err(error) = self.show_separate_search() {
            self.status(format!(
                "Das Suchfenster konnte nicht geöffnet werden. {error}"
            ));
        }
    }

    fn show_separate_search(&mut self) -> Result<(), slint::PlatformError> {
        let window = SearchWindow::new()?;
        let setup = self.search_setup();
        self.next_request += 1;
        let id = self.next_request;
        let mut options = self.profile.search_last.clone();
        options.selected_only &= !setup.selected.is_empty();
        window.set_draft(to_draft(&options));
        window.set_has_selection(!setup.selected.is_empty());
        window.set_archive(setup.archive.is_some());
        window.set_base(setup.location().display().into());
        window
            .global::<crate::Colors>()
            .set_light(self.effective_light_theme());
        let sender = self.sender.clone();
        window.on_configure(move |data, name, operation| {
            let password = search_password(&data);
            let action =
                WindowAction::Configure(from_draft(data), name.to_string(), operation, password);
            let _ = sender.send(Event::SeparateSearch(id, Box::new(action)));
        });
        let sender = self.sender.clone();
        window.on_cancel_search(move || {
            let _ = sender.send(Event::SeparateSearch(id, Box::new(WindowAction::Cancel)));
        });
        let sender = self.sender.clone();
        window.on_use_results(move || {
            let _ = sender.send(Event::SeparateSearch(id, Box::new(WindowAction::Use)));
        });
        let sender = self.sender.clone();
        window.on_close_search(move || {
            let _ = sender.send(Event::SeparateSearch(id, Box::new(WindowAction::Close)));
        });
        let sender = self.sender.clone();
        window.window().on_close_requested(move || {
            let _ = sender.send(Event::SeparateSearch(id, Box::new(WindowAction::Close)));
            slint::CloseRequestResponse::KeepWindowShown
        });
        window.show()?;
        self.separate_search = Some(SeparateSearch {
            id,
            window,
            setup,
            cancel: None,
            review: None,
        });
        self.publish_search_profiles();
        Ok(())
    }

    pub(super) fn close_separate_search(&mut self) {
        if let Some(search) = self.separate_search.take() {
            if let Some(cancel) = search.cancel {
                cancel.store(true, Ordering::Relaxed);
            }
            let _ = search.window.hide();
        }
    }

    pub(super) fn update_search_theme(&self) {
        if let Some(search) = &self.separate_search {
            search
                .window
                .global::<crate::Colors>()
                .set_light(self.effective_light_theme());
        }
    }

    pub(super) fn separate_search_action(&mut self, id: u64, action: WindowAction) {
        let Some(search) = self
            .separate_search
            .as_ref()
            .filter(|search| search.id == id)
        else {
            return;
        };
        match action {
            WindowAction::Close => self.close_separate_search(),
            WindowAction::Cancel => {
                if let Some(cancel) = &search.cancel {
                    cancel.store(true, Ordering::Relaxed);
                    search
                        .window
                        .set_notice("Suchlauf wird abgebrochen …".into());
                }
            }
            WindowAction::Use => {
                let Some(review) = search.review.as_ref() else {
                    return;
                };
                let panel = &self.panels[review.setup.side];
                if panel.request != review.setup.request
                    || panel.current().location() != review.setup.location()
                {
                    search
                        .window
                        .set_notice("Das ursprüngliche Dateifenster hat sich geändert.".into());
                } else if self.pending.is_some() {
                    search
                        .window
                        .set_notice("Schließe zuerst den Dialog im Hauptfenster.".into());
                } else {
                    let mut review = self
                        .separate_search
                        .as_mut()
                        .unwrap()
                        .review
                        .take()
                        .unwrap();
                    review.found.directory.errors.clear();
                    self.close_separate_search();
                    self.use_search_results(review, 0);
                }
            }
            WindowAction::Finished(result) => {
                let search = self.separate_search.as_mut().unwrap();
                search.cancel = None;
                search.window.set_busy(false);
                match result {
                    Ok(found) => {
                        let review = Review {
                            setup: search.setup.clone(),
                            found,
                            duplicate_groups: None,
                        };
                        let (title, list) = review_text(&review);
                        search.window.set_notice("".into());
                        search.window.set_result_title(title.into());
                        search.window.set_result_list(list.into());
                        search.window.set_has_result(true);
                        search
                            .window
                            .set_can_use(!review.found.directory.entries.is_empty());
                        search.review = Some(review);
                    }
                    Err(error) => search.window.set_notice(error.to_string().into()),
                }
            }
            WindowAction::Configure(options, name, operation, password)
                if search.cancel.is_none() =>
            {
                let setup = search.setup.clone();
                let result = if operation == 2 {
                    self.start_separate_search(id, &setup, options, password)
                } else {
                    self.search_profile(&setup, options, &name, operation)
                        .map(|(draft, notice)| {
                            if let Some(search) = &self.separate_search {
                                if let Some(draft) = draft {
                                    search.window.set_draft(draft);
                                }
                                search.window.set_notice(notice.into());
                            }
                        })
                };
                if let Err(error) = result
                    && let Some(search) = &self.separate_search
                {
                    search.window.set_notice(error.to_string().into());
                }
            }
            WindowAction::Configure(..) => {}
        }
    }

    fn start_separate_search(
        &mut self,
        id: u64,
        setup: &Setup,
        options: Options,
        password: Option<ArchivePassword>,
    ) -> io::Result<()> {
        let task = self.search_task(setup, options.clone(), password)?;
        let request = worker::Request::from_task(&task, &self.profile);
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        let sender = self.sender.clone();
        std::thread::Builder::new()
            .name("arco-search".into())
            .spawn(move || {
                let result = worker::spawn(request, task.archive.as_ref(), &worker_cancel);
                let _ = sender.send(Event::SeparateSearch(
                    id,
                    Box::new(WindowAction::Finished(result)),
                ));
            })?;
        let search = self.separate_search.as_mut().unwrap();
        search.window.set_draft(to_draft(&options));
        search.window.set_notice("Dateisuche läuft …".into());
        search.window.set_has_result(false);
        search.window.set_can_use(false);
        search.window.set_busy(true);
        search.cancel = Some(cancel);
        search.review = None;
        self.profile.search_last = options;
        self.save();
        Ok(())
    }

    pub(super) fn show_search_review(&mut self, review: Box<Review>) {
        let (title, list) = review_text(&review);
        let options = if review.duplicate_groups.is_some() {
            std::iter::once("Treffer übernehmen")
                .chain(DUPLICATE_MARK_OPTIONS.iter().map(|(label, _)| *label))
                .map(crate::locale::tr)
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        self.dialog("search-results", Pending::SearchReview(review), Dialog {
            title: &title, description: "Übernimm die Treffer als Dateiliste oder behalte die vorherige Ansicht.",
            value: &list, options: &options, ..Default::default()
        });
    }

    pub(crate) fn use_search_results(&mut self, review: Review, option: usize) {
        let side = review.setup.side;
        let location = review.setup.location();
        let request = review.setup.request;
        let marks = review.duplicate_groups.as_deref().and_then(|groups| {
            let (_, rule) = DUPLICATE_MARK_OPTIONS.get(option.checked_sub(1)?)?;
            Some(duplicate_marks(
                &review.found.directory.entries,
                groups,
                *rule,
            ))
        });
        self.completed(Outcome::Entries {
            side,
            location: location.clone(),
            request,
            entries: review.found.directory,
            scope: arco_core::view_settings::Scope::Search,
            label: if review.found.cancelled {
                "Teilergebnis".into()
            } else if review.duplicate_groups.is_some() {
                "Duplikate".into()
            } else {
                "Suchtreffer".into()
            },
        });
        let panel = &mut self.panels[side];
        if let Some(marks) = marks
            && panel.request == request
            && panel.current().location() == location
        {
            panel
                .current_mut()
                .replace_selection(|entry| marks.contains(&entry.path));
            self.publish(side);
        }
    }
}

#[derive(Clone, Copy)]
enum GroupChoice {
    Min,
    Max,
    AboveMin,
    BelowMax,
}

#[derive(Clone, Copy)]
enum DuplicateRule {
    SingleDate { newest: bool },
    Date(GroupChoice),
    Size(GroupChoice),
}

const DUPLICATE_MARK_OPTIONS: [(&str, DuplicateRule); 10] = [
    (
        "Älteste Datei je Gruppe markieren",
        DuplicateRule::SingleDate { newest: false },
    ),
    (
        "Neueste Datei je Gruppe markieren",
        DuplicateRule::SingleDate { newest: true },
    ),
    (
        "Alle ältesten Dateien je Gruppe markieren",
        DuplicateRule::Date(GroupChoice::Min),
    ),
    (
        "Alle neuesten Dateien je Gruppe markieren",
        DuplicateRule::Date(GroupChoice::Max),
    ),
    (
        "Dateien älter als die neueste markieren",
        DuplicateRule::Date(GroupChoice::BelowMax),
    ),
    (
        "Dateien neuer als die älteste markieren",
        DuplicateRule::Date(GroupChoice::AboveMin),
    ),
    (
        "Alle kleinsten Dateien je Gruppe markieren",
        DuplicateRule::Size(GroupChoice::Min),
    ),
    (
        "Alle größten Dateien je Gruppe markieren",
        DuplicateRule::Size(GroupChoice::Max),
    ),
    (
        "Dateien größer als die kleinste markieren",
        DuplicateRule::Size(GroupChoice::AboveMin),
    ),
    (
        "Dateien kleiner als die größte markieren",
        DuplicateRule::Size(GroupChoice::BelowMax),
    ),
];

fn duplicate_marks(
    entries: &[Entry],
    groups: &[usize],
    rule: DuplicateRule,
) -> std::collections::HashSet<PathBuf> {
    let mut start = 0usize;
    let mut marks = std::collections::HashSet::new();
    for &count in groups {
        let end = start.saturating_add(count).min(entries.len());
        let group = &entries[start..end];
        start = end;
        match rule {
            DuplicateRule::SingleDate { newest } => {
                if let Some(entry) = group
                    .iter()
                    .filter(|entry| entry.modified.is_some())
                    .min_by(|left, right| {
                        let order = left.modified.cmp(&right.modified);
                        let order = if newest { order.reverse() } else { order };
                        order.then_with(|| left.path.cmp(&right.path))
                    })
                {
                    marks.insert(entry.path.clone());
                }
            }
            DuplicateRule::Date(choice) => {
                marks.extend(group_marks(group, |entry| entry.modified, choice));
            }
            DuplicateRule::Size(choice) => {
                marks.extend(group_marks(group, |entry| entry.size, choice));
            }
        }
    }
    marks
}

fn group_marks<'a, V: Ord + Copy + 'a>(
    group: &'a [Entry],
    metric: fn(&Entry) -> Option<V>,
    choice: GroupChoice,
) -> impl Iterator<Item = PathBuf> + 'a {
    let values = group.iter().filter_map(metric);
    let threshold = match choice {
        GroupChoice::Min | GroupChoice::AboveMin => values.min(),
        GroupChoice::Max | GroupChoice::BelowMax => values.max(),
    };
    group.iter().filter_map(move |entry| {
        let value = metric(entry)?;
        let threshold = threshold?;
        let matches = match choice {
            GroupChoice::Min | GroupChoice::Max => value == threshold,
            GroupChoice::AboveMin => value > threshold,
            GroupChoice::BelowMax => value < threshold,
        };
        matches.then(|| entry.path.clone())
    })
}

#[cfg(test)]
#[test]
fn duplicate_selection_keeps_group_boundaries_and_ignores_unknown_times() {
    use std::time::{Duration, UNIX_EPOCH};
    let entry = |name: &str, seconds: Option<u64>| {
        Entry::from_remote(
            name.into(),
            Kind::File,
            None,
            seconds.map(|seconds| UNIX_EPOCH + Duration::from_secs(seconds)),
        )
    };
    let entries = [
        entry("b", Some(1)),
        entry("a", Some(1)),
        entry("c", Some(2)),
        entry("d", None),
        entry("e", Some(9)),
        entry("f", Some(8)),
    ];
    assert_eq!(
        duplicate_marks(
            &entries,
            &[3, 3],
            DuplicateRule::SingleDate { newest: false }
        ),
        [PathBuf::from("a"), PathBuf::from("f")]
            .into_iter()
            .collect()
    );
    assert_eq!(
        duplicate_marks(
            &entries,
            &[3, 3],
            DuplicateRule::SingleDate { newest: true }
        ),
        [PathBuf::from("c"), PathBuf::from("e")]
            .into_iter()
            .collect()
    );
}

#[cfg(test)]
#[test]
fn duplicate_extremes_mark_ties_and_relative_choices_leave_extreme_ties_unmarked() {
    use std::time::{Duration, UNIX_EPOCH};
    let entry = |name: &str, size, seconds: Option<u64>| {
        Entry::from_remote(
            name.into(),
            Kind::File,
            size,
            seconds.map(|seconds| UNIX_EPOCH + Duration::from_secs(seconds)),
        )
    };
    let entries = [
        entry("a", Some(8), Some(1)),
        entry("b", Some(9), Some(1)),
        entry("c", Some(9), Some(2)),
        entry("d", None, None),
        entry("e", Some(7), Some(8)),
        entry("f", Some(7), Some(9)),
        entry("g", Some(5), Some(9)),
        entry("h", Some(4), Some(3)),
        entry("i", Some(4), Some(3)),
    ];
    let marks = |rule| duplicate_marks(&entries, &[4, 3, 2], rule);
    let expected = |names: &[&str]| {
        names
            .iter()
            .copied()
            .map(PathBuf::from)
            .collect::<std::collections::HashSet<_>>()
    };
    assert_eq!(
        marks(DuplicateRule::Date(GroupChoice::Min)),
        expected(&["a", "b", "e", "h", "i"])
    );
    assert_eq!(
        marks(DuplicateRule::Date(GroupChoice::Max)),
        expected(&["c", "f", "g", "h", "i"])
    );
    assert_eq!(
        marks(DuplicateRule::Date(GroupChoice::AboveMin)),
        expected(&["c", "f", "g"])
    );
    assert_eq!(
        marks(DuplicateRule::Date(GroupChoice::BelowMax)),
        expected(&["a", "b", "e"])
    );
    assert_eq!(
        marks(DuplicateRule::Size(GroupChoice::Min)),
        expected(&["a", "g", "h", "i"])
    );
    assert_eq!(
        marks(DuplicateRule::Size(GroupChoice::Max)),
        expected(&["b", "c", "e", "f", "h", "i"])
    );
    assert_eq!(
        marks(DuplicateRule::Size(GroupChoice::AboveMin)),
        expected(&["b", "c", "e", "f"])
    );
    assert_eq!(
        marks(DuplicateRule::Size(GroupChoice::BelowMax)),
        expected(&["a", "g"])
    );
}

fn compile_search(options: &Options) -> io::Result<advanced_search::Query> {
    if options.everything().is_some() {
        let mut local = options.clone();
        local.names.clear();
        local.compile()
    } else {
        options.compile()
    }
}

fn review_text(review: &Review) -> (String, String) {
    let found = &review.found;
    let mut list = String::new();
    let mut entries = found.directory.entries.iter().take(1000);
    if let Some(groups) = &review.duplicate_groups {
        for (index, &count) in groups.iter().enumerate() {
            if entries.len() == 0 {
                break;
            }
            writeln!(list, "Gruppe {}", index + 1).unwrap();
            for entry in entries.by_ref().take(count) {
                writeln!(list, "  {}", display_path(&entry.path)).unwrap();
            }
        }
    } else {
        for entry in entries {
            writeln!(list, "{}", display_path(&entry.path)).unwrap();
        }
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
    let title = if let Some(groups) = &review.duplicate_groups {
        format!(
            "{} Duplikatgruppen · {} Dateien",
            groups.len(),
            found.directory.entries.len()
        )
    } else {
        format!(
            "{} Suchtreffer{}",
            found.directory.entries.len(),
            if found.cancelled {
                " · Suche abgebrochen"
            } else {
                ""
            }
        )
    };
    (title, list)
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
        wdx_profile: options.wdx_profile.as_str().into(),
        archive_password: "".into(),
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
        wdx_profile: data.wdx_profile.to_string(),
    }
}

fn search_password(data: &SearchDraft) -> Option<ArchivePassword> {
    (!data.archive_password.is_empty())
        .then(|| Arc::new(zeroize::Zeroizing::new(data.archive_password.to_string())))
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use arco_core::plugin::{Registration, WdxField};
    use arco_core::profile::{WdxSearchField, WdxSearchProfile};
    use std::fs;

    #[test]
    #[ignore = "requires the WDX test fixture in ARCO_TEST_WDX_DUPLICATES"]
    fn shared_search_profile_filters_local_and_encrypted_archive_files_with_wdx() {
        let fixture = tempfile::tempdir().unwrap();
        let base = fixture.path();
        let source = base.join("docs");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("Treffer.txt"), b"arco content").unwrap();
        fs::write(source.join("NurName.txt"), b"other content").unwrap();
        fs::write(source.join("FalschesFeld.md"), b"arco content").unwrap();
        let cancel = AtomicBool::new(false);
        let zip = base.join("test.zip");
        arco_core::archives::create_zip(
            std::slice::from_ref(&source),
            base,
            &zip,
            Some("fixture"),
            &cancel,
        )
        .unwrap();
        let catalog = Arc::new(arco_core::archive_view::Catalog::read(&zip, &cancel).unwrap());
        let plugin_path = PathBuf::from(std::env::var_os("ARCO_TEST_WDX_DUPLICATES").unwrap());
        let mut plugin = Registration::read(&plugin_path, "Fixture", "").unwrap();
        plugin.enabled = true;
        let wdx = WdxSearchProfile {
            masks: "*.txt".into(),
            all: true,
            conditions: vec![WdxSearchField {
                plugin: plugin.path.clone(),
                selection: WdxField {
                    field: "Score".into(),
                    unit: Some("scaled".into()),
                },
                expression: "= 41".into(),
            }],
        };
        let options = Options {
            names: "*.txt;*.md".into(),
            content: "arco".into(),
            content_mode: 1,
            wdx_profile: "Fixture".into(),
            ..Options::default()
        };
        let mut draft = to_draft(&options);
        draft.archive_password = "fixture".into();
        let password = search_password(&draft);
        let options = from_draft(draft);
        assert_eq!(options.wdx_profile, "Fixture");
        assert_eq!(to_draft(&options).archive_password, "");
        for archive in [
            None,
            Some(ArchiveTab {
                file: zip,
                folder: PathBuf::from("docs"),
                catalog,
                password: None,
            }),
        ] {
            let roots = vec![
                archive
                    .as_ref()
                    .map_or_else(|| source.clone(), |archive| archive.folder.clone()),
            ];
            let found = SearchTask {
                query: options.compile().unwrap(),
                options: options.clone(),
                roots,
                indexed: None,
                show_hidden: false,
                ignore: None,
                executable: None,
                archive,
                password: password.clone(),
                wdx: Some(
                    super::super::wdx::search::ProfileFilter::compile(
                        &wdx,
                        std::slice::from_ref(&plugin),
                    )
                    .unwrap(),
                ),
            }
            .run(&cancel)
            .unwrap();
            assert!(
                found.directory.errors.is_empty(),
                "{:?}",
                found.directory.errors
            );
            let names: Vec<_> = found
                .directory
                .entries
                .iter()
                .map(|entry| entry.name.to_str().unwrap())
                .collect();
            assert_eq!(names, ["Treffer.txt"]);
        }
    }
}
