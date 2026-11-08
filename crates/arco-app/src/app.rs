use crate::commands::{COMMANDS, Dialog, Pending};
use crate::panel::{ArchiveTab, FtpTab, Location, Panel, ShellLocation, Tab};
use crate::preview;
use crate::shortcuts;
use crate::{Colors, MainWindow, SettingsData, display_path, human_size};
use arco_core::archive_view::{self, Catalog};
use arco_core::files::{self, Directory, Kind, Transfer, TransferReport};
use arco_core::profile::{self, LayoutSettings, ListFont, Profile, TabLock, TcImport};
use arco_core::rename;
use arco_core::tools::{PreviewEncoding, PreviewMode};
use arco_windows::DiskSpace;
use arco_windows::ftp;
use slint::winit_030::{
    EventResult, WinitWindowAccessor, winit::event::WindowEvent, winit::window::Theme,
};
use slint::{ComponentHandle, Model, ModelRc, SharedString, Timer, TimerMode, VecModel};
use std::cell::RefCell;
use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant, SystemTime};

mod administration;
mod advanced_search;
mod archive_actions;
mod associations;
mod buttonbars;
mod directory_tree;
mod favorites;
mod folder_actions;
mod ftp_edit;
mod ftp_panel;
mod ftp_profiles;
mod ftp_sync;
mod ignore;
mod keyboard;
mod lister;
mod metadata_actions;
mod packer_profiles;
mod plugin_files;
mod plugins;
mod print_actions;
mod rename_setup;
mod selection_tools;
mod shell_panel;
mod sync_actions;
mod tabs;
mod text_compare;
mod thumbnails;
mod transfer_queue;
mod view_setup;
mod wdx;
mod wfx_panel;
mod wfx_prompts;
mod wlx;
pub(crate) use administration::Setup as AdministrationSetup;
pub(crate) use advanced_search::{Review as AdvancedSearchReview, Setup as AdvancedSearchSetup};
pub(crate) use archive_actions::{ArchiveInput, StagedPreview};
pub(crate) use associations::Setup as AssociationSetup;
pub(crate) use favorites::{Setup as FavoriteSetup, label as favorite_label, tc_profile};
pub(crate) use ftp_edit::Draft as FtpDraft;
pub(crate) use ftp_panel::FtpEdit;
pub(crate) use ftp_sync::modified as ftp_modified;
pub(crate) use lister::ViewerRequest;
pub(crate) use plugin_files::PluginFileAction;
pub(crate) use rename_setup::Setup as RenameSetup;
pub(crate) use sync_actions::{SyncReview, SyncSetup};
pub(crate) use transfer_queue::ConflictTicket;
pub(crate) use transfer_queue::TransferTask;
pub(crate) use view_setup::Setup as ViewSetup;
pub(crate) use wdx::SearchTarget as WdxSearchTarget;
pub(crate) use wdx::{EditInput as WdxEditInput, EditTarget as WdxEditTarget};
pub(crate) use wfx_prompts::PromptTicket;

const FILE_FONT_SIZES: [u8; 4] = [12, 14, 16, 18];

pub enum Navigation {
    Refresh,
    Jump,
    Up,
    Back,
    Forward,
    FtpProfile {
        local: PathBuf,
        opposite_request: u64,
    },
}

pub enum Outcome {
    Notice(String),
    Text(String, String),
    SearchReview(Box<AdvancedSearchReview>),
    FileComment(PathBuf, Option<String>),
    PrintPreview(String, String),
    TransferFinished(TransferReport),
    ClipboardText(String),
    Entries {
        side: usize,
        location: Location,
        request: u64,
        entries: Directory,
        label: String,
        scope: arco_core::view_settings::Scope,
    },
    Report(String, TransferReport),
    RenamePreview(rename::Plan),
    RenameNameList(rename_setup::NameList, Option<tempfile::TempDir>),
    Renamed(rename::Plan),
    Sync(Box<SyncReview>),
    ComparedPanels {
        roots: [PathBuf; 2],
        requests: [u64; 2],
        marks: arco_core::sync::PanelMarks,
        hide_equal: bool,
    },
    FolderSizes {
        side: usize,
        request: u64,
        root: PathBuf,
        sizes: std::collections::BTreeMap<PathBuf, u64>,
        skipped: usize,
    },
    Ftp(FtpView),
    FtpEditReady(Box<FtpDraft>),
    FtpEditUploaded(Box<FtpDraft>),
    Imported(TcImport),
    ImportedTabs(String, Box<arco_core::profile::TcTabs>),
    TabFile(Box<arco_core::profile::TcTabs>, bool),
    ImportedFtp(Vec<(String, arco_core::profile::TcFtpSite)>),
    PluginPreview(String, arco_core::plugin::HostProcess),
    PluginPackage(Box<arco_core::plugin::Package>),
    PluginImport(arco_core::plugin::TcRegistrations),
    WdxFields(
        Box<arco_core::plugin::Registration>,
        Vec<arco_core::plugin::Row>,
        Option<WdxSearchTarget>,
    ),
    WdxSorted(Vec<files::Entry>),
    WdxEditable(wdx::EditTarget, Vec<arco_core::plugin::WdxEditableField>),
    IgnoreImported(String),
    Preview {
        path: PathBuf,
        mode: PreviewMode,
        page: arco_core::tools::Preview,
    },
    ViewerPage(Viewer, arco_core::tools::Preview),
    TextCompared(Arc<arco_core::text_compare::Session>, usize, bool),
    BinaryCompared(
        Arc<arco_core::tools::BinaryComparison>,
        arco_core::tools::BinaryPage,
        bool,
    ),
    LocalShares(Vec<arco_windows::LocalShare>),
    VolumeLabel(arco_windows::Volume),
    Image(PathBuf, image::RgbaImage),
    RichPreview(PathBuf, preview::Rich),
    StagedPreview(StagedPreview, preview::Content),
}

enum Event {
    Load {
        side: usize,
        id: u64,
        target: Location,
        navigation: Navigation,
        directory: io::Result<Box<LoadedDirectory>>,
    },
    Progress(String),
    PluginPrompt(PromptTicket),
    TransferConflict(ConflictTicket),
    WdxValues(wdx::Values),
    Thumbnails(thumbnails::Loaded),
    TreeLoaded(directory_tree::Loaded),
    QuickPreview(QuickTarget, wlx::QuickResult),
    Finished {
        outcome: Result<Outcome, String>,
        refresh: bool,
    },
}

struct DirectoryRequest {
    id: u64,
    target: Location,
    navigation: Navigation,
    cached: Option<Arc<Catalog>>,
    password: Option<crate::panel::ArchivePassword>,
    comments: bool,
    hidden: bool,
}

struct LoadedDirectory {
    directory: Directory,
    space: Option<DiskSpace>,
    archive: Option<ArchiveTab>,
    ftp: Option<FtpTab>,
    shell: std::collections::BTreeMap<PathBuf, arco_windows::ShellFolder>,
}

fn load_directory(
    target: &mut Location,
    cached: Option<Arc<Catalog>>,
    password: Option<crate::panel::ArchivePassword>,
    comments: bool,
    hidden: bool,
    job: &JobContext,
) -> io::Result<LoadedDirectory> {
    let cancel = &job.cancel;
    match target {
        Location::Local(path) => {
            *path = std::path::absolute(&*path)?;
            let mut directory = files::list_cancellable(path, cancel)?;
            if comments {
                match arco_core::descriptions::read(path) {
                    Ok(comments) => {
                        for entry in &mut directory.entries {
                            entry.comment = comments
                                .get(&arco_core::descriptions::key(&entry.name))
                                .map(|text| Arc::from(text.as_str()));
                        }
                    }
                    Err(error) => directory.errors.push(error.to_string()),
                }
            }
            if cancel.load(Ordering::Relaxed) {
                return Err(io::ErrorKind::Interrupted.into());
            }
            let space = arco_windows::disk_space(path);
            Ok(LoadedDirectory {
                directory,
                space,
                archive: None,
                ftp: None,
                shell: Default::default(),
            })
        }
        Location::Archive { file, folder } => {
            *file = std::path::absolute(&*file)?;
            let catalog = match cached {
                Some(catalog) if catalog.unchanged(file) => catalog,
                _ => Arc::new(Catalog::read_with_password(
                    file,
                    password.as_ref().map(|password| password.as_str()),
                    cancel,
                )?),
            };
            let entries = catalog.rows(folder)?;
            Ok(LoadedDirectory {
                directory: Directory {
                    entries,
                    errors: Vec::new(),
                },
                space: None,
                archive: Some(ArchiveTab {
                    file: file.clone(),
                    folder: folder.clone(),
                    catalog,
                    password,
                }),
                ftp: None,
                shell: Default::default(),
            })
        }
        Location::Ftp(location) => ftp_panel::read_panel(location, cancel),
        Location::Wfx(location) => wfx_panel::read_panel(location, job),
        Location::Shell(location) => shell_panel::read_panel(location, hidden, cancel),
    }
}

#[derive(Clone, Eq, PartialEq)]
struct QuickTarget {
    path: PathBuf,
    modified: Option<SystemTime>,
    size: Option<u64>,
    archive: Option<(PathBuf, u64, bool)>,
}

#[derive(Clone)]
pub struct JobContext {
    pub cancel: Arc<AtomicBool>,
    sender: Sender<Event>,
}

impl JobContext {
    pub fn progress(&self, value: String) {
        let _ = self.sender.send(Event::Progress(value));
    }
}

#[derive(Clone)]
pub struct Viewer {
    pub path: PathBuf,
    pub mode: PreviewMode,
    pub offset: u64,
    pub next: u64,
    pub previous: Vec<u64>,
    pub length: u64,
    pub encoding: PreviewEncoding,
    pub last_hit: Option<(String, u64)>,
}

pub struct FtpView {
    pub session: Arc<ftp::Session>,
    pub directory: ftp::Url,
    pub listing: ftp::Listing,
}

pub type FtpEndpoint = (Arc<ftp::Session>, ftp::Url);

pub struct App {
    pub window: slint::Weak<MainWindow>,
    pub panels: [Panel; 2],
    closed_tabs: Vec<tabs::ClosedTab>,
    pub active: usize,
    pub profile: Profile,
    system_light: Option<bool>,
    file_colors: Rc<Vec<arco_core::view_settings::CompiledColor>>,
    ignore: Option<Arc<arco_core::ignore::IgnoreList>>,
    profile_path: PathBuf,
    save_profile: bool,
    pub pending: Option<Pending>,
    plugin_prompts: std::collections::VecDeque<PromptTicket>,
    wdx_pending: Option<wdx::Running>,
    wdx_sort_request: Option<wdx::SortRequest>,
    wdx_next_side: usize,
    thumbnail_jobs: [Option<Rc<crate::panel::FileModel>>; 2],
    trees: [Option<directory_tree::Tree>; 2],
    tree_loading: [bool; 2],
    pub undo: Option<rename::Plan>,
    pub viewer: Option<Viewer>,
    pub staged_preview: Option<StagedPreview>,
    listers: Vec<crate::ListerWindow>,
    pub(crate) preview_image_path: Option<PathBuf>,
    quick_open: bool,
    quick_requested: Option<QuickTarget>,
    quick_running: bool,
    quick_ready: bool,
    quick_cancel: Option<Arc<AtomicBool>>,
    quick_embedded: Option<Box<wlx::EmbeddedPreview>>,
    quick_skip_plugin: bool,
    plugin_previews: Vec<(String, arco_core::plugin::HostProcess)>,
    pub ftp: Option<FtpView>,
    pub fxp_source: Option<FtpEndpoint>,
    ftp_drafts: Vec<FtpDraft>,
    sender: Sender<Event>,
    receiver: Receiver<Event>,
    pub job: Option<Arc<AtomicBool>>,
    transfers: transfer_queue::Queue,
    next_request: u64,
    loader_cancel: [Option<Arc<AtomicBool>>; 2],
    waiting_loads: [Option<DirectoryRequest>; 2],
    drops: Option<(usize, Vec<PathBuf>)>,
    last_pointer_open: Option<(Instant, (i32, i32))>,
    context_menu_open: bool,
    bindings: shortcuts::Bindings,
    shortcut_editor: Option<keyboard::Editor>,
    #[cfg(windows)]
    shortcut_hook: Option<arco_windows::ShortcutHook>,
    timer: Timer,
    drive_timer: Timer,
}

pub fn run(
    window: &MainWindow,
    profile: Profile,
    fallback: PathBuf,
    profile_path: PathBuf,
    profile_error: Option<String>,
) -> Rc<RefCell<App>> {
    let (sender, receiver) = mpsc::channel();
    let panels = profile
        .panels
        .clone()
        .map(|panel| Panel::from_saved(panel, &fallback));
    let active = profile.active_panel.min(1);
    let bindings = shortcuts::effective(&profile.shortcuts);
    let file_colors = Rc::new(profile.views.compile_colors());
    let ignored = profile
        .ignore_enabled
        .then(|| arco_core::ignore::IgnoreList::parse(&profile.ignore_rules))
        .transpose();
    let (ignore, ignore_error) = match ignored {
        Ok(ignore) => (ignore.map(Arc::new), None),
        Err(error) => (None, Some(error)),
    };
    let app = Rc::new(RefCell::new(App {
        window: window.as_weak(),
        panels,
        closed_tabs: Vec::new(),
        active,
        profile,
        system_light: None,
        file_colors,
        ignore,
        profile_path,
        save_profile: profile_error.is_none(),
        pending: None,
        plugin_prompts: Default::default(),
        wdx_pending: None,
        wdx_sort_request: None,
        wdx_next_side: 0,
        thumbnail_jobs: [None, None],
        trees: [None, None],
        tree_loading: [false; 2],
        undo: None,
        viewer: None,
        staged_preview: None,
        listers: Vec::new(),
        preview_image_path: None,
        quick_open: false,
        quick_requested: None,
        quick_running: false,
        quick_ready: false,
        quick_cancel: None,
        quick_embedded: None,
        quick_skip_plugin: false,
        plugin_previews: Vec::new(),
        ftp: None,
        fxp_source: None,
        ftp_drafts: Vec::new(),
        sender,
        receiver,
        job: None,
        transfers: Default::default(),
        next_request: 0,
        loader_cancel: [None, None],
        waiting_loads: [None, None],
        drops: None,
        last_pointer_open: None,
        context_menu_open: false,
        bindings,
        shortcut_editor: None,
        #[cfg(windows)]
        shortcut_hook: None,
        timer: Timer::default(),
        drive_timer: Timer::default(),
    }));
    let weak = Rc::downgrade(&app);
    window.on_action(move |name| {
        if let Some(app) = weak.upgrade() {
            app.borrow_mut().action(name.as_str());
        }
    });
    let weak = Rc::downgrade(&app);
    window.on_command_line(move |line| {
        weak.upgrade()
            .is_some_and(|app| app.borrow_mut().command_line(line.as_str()))
    });
    let weak = Rc::downgrade(&app);
    window.on_panel_action(move |side, name| {
        if let Some(app) = weak.upgrade() {
            let mut app = app.borrow_mut();
            app.set_active(side as usize);
            app.action(name.as_str());
        }
    });
    let weak = Rc::downgrade(&app);
    window.on_navigate(move |side, path| {
        if let Some(app) = weak.upgrade() {
            let mut app = app.borrow_mut();
            app.set_active(side as usize);
            app.navigate_text(path.as_str());
        }
    });
    let weak = Rc::downgrade(&app);
    window.on_select_crumb(move |side, index| {
        if let Some(app) = weak.upgrade() {
            let mut app = app.borrow_mut();
            let side = side as usize;
            let path = app.panels[side]
                .current()
                .crumbs()
                .into_iter()
                .nth(index as usize);
            if let Some(path) = path {
                app.set_active(side);
                app.load_location(side, path, Navigation::Jump);
            }
        }
    });
    let weak = Rc::downgrade(&app);
    window.on_filter(move |side, value| {
        if let Some(app) = weak.upgrade() {
            let mut app = app.borrow_mut();
            let side = side as usize;
            let show_hidden = app.profile.show_hidden;
            app.set_active(side);
            app.panels[side]
                .current_mut()
                .filter(value.to_string(), show_hidden);
            app.publish(side);
        }
    });
    let weak = Rc::downgrade(&app);
    window.on_click_row(move |side, row, toggle, range, open| {
        if let Some(app) = weak.upgrade() {
            let mut app = app.borrow_mut();
            let side = side as usize;
            let row = row as usize;
            let panel = &app.panels[side];
            if panel.loading
                || (panel.current().display_row(row).is_none() && !panel.current().parent_row(row))
            {
                return;
            }
            if open && app.is_repeated_pointer_open() {
                return;
            }
            app.set_active(side);
            app.leave_tree_focus(side);
            app.current_mut().select_row(row, toggle, range);
            app.publish(side);
            if open {
                app.open_row(row);
            } else {
                app.last_pointer_open = None;
            }
        }
    });
    let weak = Rc::downgrade(&app);
    window.on_apply_settings(move |value, close| {
        if let Some(app) = weak.upgrade() {
            app.borrow_mut().apply_settings(value, close);
        }
    });
    let weak = Rc::downgrade(&app);
    window.on_show_drives(move |side| {
        if let Some(app) = weak.upgrade() {
            app.borrow().show_drives(side as usize);
        }
    });
    let weak = Rc::downgrade(&app);
    window.on_choose_drive(move |value| {
        if let Some(app) = weak.upgrade() {
            app.borrow_mut().choose_drive(Path::new(value.as_str()));
        }
    });
    let weak = Rc::downgrade(&app);
    window.on_drive_letter(move |value| {
        let value = value.as_bytes();
        if let [letter] = value
            && letter.is_ascii_alphabetic()
            && let Some(app) = weak.upgrade()
        {
            let drive = format!("{}:\\", char::from(letter.to_ascii_uppercase()));
            return app.borrow_mut().choose_drive(Path::new(&drive));
        }
        false
    });
    let weak = Rc::downgrade(&app);
    window.on_context_row(move |side, row, extended| {
        let Some(app) = weak.upgrade() else { return };
        let Some(point) = arco_windows::cursor_screen() else {
            return;
        };
        queue_context_menu(&app, side as usize, row as usize, point, extended);
    });
    let weak = Rc::downgrade(&app);
    window.on_keyboard_context(move |x, y| {
        let Some(app) = weak.upgrade() else { return };
        let (side, row, point) = {
            let app = app.borrow();
            let Some((left, top, scale)) = window_origin(app.ui().window()) else {
                return;
            };
            (
                app.active,
                app.current().cursor,
                (
                    left + (x * scale).round() as i32,
                    top + (y * scale).round() as i32,
                ),
            )
        };
        queue_context_menu(&app, side, row, point, false);
    });
    let weak = Rc::downgrade(&app);
    window.on_drag_row(move |side, row| {
        let Some(app) = weak.upgrade() else { return };
        let paths = app.borrow().panels[side as usize]
            .current()
            .drag_paths(row as usize);
        if paths.is_empty() {
            return;
        }
        let weak = Rc::downgrade(&app);
        Timer::single_shot(Duration::ZERO, move || {
            if let Err(error) = arco_windows::drag_copy(&paths)
                && let Some(app) = weak.upgrade()
            {
                app.borrow().status(format!("Dateien ziehen: {error}"));
            }
        });
    });
    let weak = Rc::downgrade(&app);
    window.on_tree_row(move |side, index, operation| {
        if let Some(app) = weak.upgrade() {
            app.borrow_mut()
                .select_tree_row(side as usize, index as usize, operation);
        }
    });
    let weak = Rc::downgrade(&app);
    window.on_custom_button(move |bar, index| {
        if let Some(app) = weak.upgrade() {
            app.borrow_mut().run_button(bar as usize, index as usize);
        }
    });
    keyboard::connect_editor(window, &app);
    let weak = Rc::downgrade(&app);
    window.on_search_configure(move |data, name, operation| {
        if let Some(app) = weak.upgrade() {
            app.borrow_mut()
                .configure_search(data, name.as_str(), operation);
        }
    });
    let weak = Rc::downgrade(&app);
    let mut modifiers = Default::default();
    window.window().on_winit_window_event(move |window, event| {
        let theme = match event {
            WindowEvent::ThemeChanged(theme) => Some(*theme),
            WindowEvent::Focused(true) | WindowEvent::Resized(_) => {
                window.with_winit_window(|native| native.theme()).flatten()
            }
            _ => None,
        };
        if let Some(theme) = theme
            && let Some(app) = weak.upgrade()
            && let Ok(mut app) = app.try_borrow_mut()
        {
            app.system_light = Some(theme == Theme::Light);
            if app.profile.system_theme {
                app.ui()
                    .global::<Colors>()
                    .set_light(app.effective_light_theme());
            }
        }
        #[cfg(windows)]
        if matches!(event, WindowEvent::Resized(_) | WindowEvent::Focused(true)) {
            keyboard::ensure_native_hook(window, &weak);
        }
        if keyboard::window_key(&weak, event, &mut modifiers) {
            return EventResult::PreventDefault;
        }
        if let WindowEvent::DroppedFile(path) = event
            && let Some(app) = weak.upgrade()
            && let Ok(mut app) = app.try_borrow_mut()
        {
            let side = app.drop_side(window);
            app.drops
                .get_or_insert_with(|| (side, Vec::new()))
                .1
                .push(path.clone());
        }
        EventResult::Propagate
    });
    let weak = Rc::downgrade(&app);
    window.on_select_tab(move |side, tab| {
        if let Some(app) = weak.upgrade() {
            let mut app = app.borrow_mut();
            app.set_active(side as usize);
            app.select_tab(tab as usize);
        }
    });
    let weak = Rc::downgrade(&app);
    window.on_submit(move |value, second, option| {
        if let Some(app) = weak.upgrade() {
            app.borrow_mut()
                .submit(value.as_str(), second.as_str(), option);
        }
    });
    let weak = Rc::downgrade(&app);
    window.on_ntfs_apply(move |options| {
        if let Some(app) = weak.upgrade() {
            app.borrow_mut().apply_ntfs_options(options);
        }
    });
    {
        let app = Rc::downgrade(&app);
        window.on_viewer_control(move |operation, value, option| {
            if let Some(app) = app.upgrade() {
                app.borrow_mut()
                    .viewer_control(operation, value.as_str(), option);
            }
        });
    }
    let weak = Rc::downgrade(&app);
    window.on_diff_navigate(move |delta| {
        if let Some(app) = weak.upgrade() {
            app.borrow_mut().navigate_comparison(delta);
        }
    });
    let weak = Rc::downgrade(&app);
    window.on_diff_options(move |ignore_case, ignore_whitespace, ignore_line_endings| {
        if let Some(app) = weak.upgrade() {
            app.borrow_mut()
                .update_comparison(arco_core::text_compare::Options {
                    ignore_case,
                    ignore_whitespace,
                    ignore_line_endings,
                });
        }
    });
    let weak = Rc::downgrade(&app);
    window.on_diff_save(move |side, edited, text| {
        if let Some(app) = weak.upgrade() {
            app.borrow_mut()
                .save_comparison_change(side, edited, text.to_string());
        }
    });
    let weak = Rc::downgrade(&app);
    window.on_directory_loaded(move || {
        if let Some(app) = weak.upgrade() {
            app.borrow_mut().drain();
        }
    });
    let weak = Rc::downgrade(&app);
    window.on_ftp_profile_select(move |index| {
        if let Some(app) = weak.upgrade() {
            app.borrow_mut().select_ftp_profile(index);
        }
    });
    let weak = Rc::downgrade(&app);
    window.on_ftp_profile_operate(move |operation, data, password| {
        if let Some(app) = weak.upgrade() {
            app.borrow_mut()
                .operate_ftp_profile(&operation, data, &password);
        }
    });
    let weak = Rc::downgrade(&app);
    window.on_plugin_operate(move |operation, index| {
        if let Some(app) = weak.upgrade() {
            app.borrow_mut().operate_plugin(&operation, index);
        }
    });
    let weak = Rc::downgrade(&app);
    window.on_ftp_browse(move |operation, index| {
        if let Some(app) = weak.upgrade() {
            app.borrow_mut().browse_ftp(operation.as_str(), index);
        }
    });
    let weak = Rc::downgrade(&app);
    window.on_sync_configure(move |data, preset, operation, left, right| {
        if let Some(app) = weak.upgrade() {
            app.borrow_mut().configure_sync(
                data,
                preset.as_str(),
                operation,
                [left.as_str(), right.as_str()],
            );
        }
    });
    let weak = Rc::downgrade(&app);
    window.on_sync_choose(move |index, action| {
        if let Some(app) = weak.upgrade() {
            app.borrow_mut().choose_sync_action(index, action);
        }
    });
    let weak = Rc::downgrade(&app);
    window.on_sync_filter(move |filter| {
        if let Some(app) = weak.upgrade() {
            app.borrow_mut().filter_sync(filter);
        }
    });
    let weak = Rc::downgrade(&app);
    window.on_sync_run(move |confirm_deletes| {
        if let Some(app) = weak.upgrade() {
            app.borrow_mut().run_sync(confirm_deletes);
        }
    });
    let weak = Rc::downgrade(&app);
    window.window().on_close_requested(move || {
        if let Some(app) = weak.upgrade() {
            let mut app = app.borrow_mut();
            if app.job.is_some() {
                app.status(
                    "Ein Auftrag läuft. Beende ihn über Abbrechen oder warte auf das Ergebnis.",
                );
                return slint::CloseRequestResponse::KeepWindowShown;
            }
            app.save();
            for lister in app.listers.drain(..) {
                lister.invoke_close_lister();
            }
        }
        slint::CloseRequestResponse::HideWindow
    });
    {
        let mut state = app.borrow_mut();
        window.set_drives(strings(
            arco_windows::drives().iter().map(|path| display_path(path)),
        ));
        window.set_tools_menu(strings(
            std::iter::once("Werkzeuge").chain(COMMANDS.iter().map(|(label, _)| *label)),
        ));
        state.sync_shortcut_labels();
        state.sync_ui_settings();
        state.invalidate_wdx_columns();
        window.set_split_ratio(state.profile.split_ratio.clamp(0.37, 0.63));
        state.set_active(active);
        window.set_separate_tree_mode(i32::from(state.profile.separate_tree_mode.min(2)));
        state.refresh_both();
        if let Some(error) = profile_error {
            state.status(format!("Profil konnte nicht gelesen werden: {error}"));
        } else if let Some(error) = ignore_error {
            state.status(format!(
                "Ignorierliste konnte nicht gelesen werden: {error}"
            ));
        }
        let weak = Rc::downgrade(&app);
        state
            .timer
            .start(TimerMode::Repeated, Duration::from_millis(30), move || {
                if let Some(app) = weak.upgrade() {
                    app.borrow_mut().drain();
                }
            });
        let window = window.as_weak();
        state
            .drive_timer
            .start(TimerMode::Repeated, Duration::from_secs(2), move || {
                if let Some(window) = window.upgrade() {
                    let drives: Vec<_> = arco_windows::drives()
                        .iter()
                        .map(|path| SharedString::from(display_path(path)))
                        .collect();
                    if !window.get_drives().iter().eq(drives.iter().cloned()) {
                        window.set_drives(ModelRc::new(VecModel::from(drives)));
                    }
                }
            });
    }
    app
}

fn window_origin(window: &slint::Window) -> Option<(i32, i32, f32)> {
    window
        .with_winit_window(|native| {
            let position = native.inner_position().ok()?;
            Some((position.x, position.y, native.scale_factor() as f32))
        })
        .flatten()
}

fn queue_context_menu(
    app: &Rc<RefCell<App>>,
    side: usize,
    row: usize,
    point: (i32, i32),
    extended: bool,
) {
    let paths = {
        let mut app = app.borrow_mut();
        if app.context_menu_open || app.panels[side].loading {
            return;
        }
        if !app.panels[side].current().is_local() && app.panels[side].current().shell.is_none() {
            if app.panels[side].current().display_row(row).is_none() {
                return;
            }
            app.set_active(side);
            app.current_mut().focus_context_row(row);
            app.publish(side);
            if app.current().ftp.is_some() {
                app.ftp_panel_context_dialog();
            } else if app.current().wfx.is_some() {
                app.wfx_context_dialog();
            } else {
                app.archive_context_dialog();
            }
            return;
        }
        let tab = app.panels[side].current_mut();
        let paths = if tab.shell.is_some() {
            let Some(entry) = tab.display_row(row) else {
                return;
            };
            let paths = vec![entry.path.clone()];
            tab.focus_context_row(row);
            paths
        } else {
            tab.context_paths(row)
        };
        if paths.is_empty() {
            return;
        }
        app.context_menu_open = true;
        app.last_pointer_open = None;
        app.set_active(side);
        app.publish(side);
        paths
    };
    let weak = Rc::downgrade(app);
    // The native popup runs a message loop. Release the application borrow first.
    Timer::single_shot(Duration::ZERO, move || {
        let result = arco_windows::show_context_menu(&paths, point, extended);
        if let Some(app) = weak.upgrade() {
            let mut app = app.borrow_mut();
            app.context_menu_open = false;
            match result {
                Ok(true) => app.refresh_both(),
                Ok(false) => (),
                Err(error) => app.status(format!("Windows-Kontextmenü: {error}")),
            }
        }
    });
}

impl App {
    pub(crate) fn effective_light_theme(&self) -> bool {
        if self.profile.system_theme {
            self.system_light.unwrap_or(self.profile.light_mode)
        } else {
            self.profile.light_mode
        }
    }

    pub fn sync_ui_settings(&self) {
        let profile = &self.profile;
        let layout = &profile.layout;
        let ui = self.ui();
        ui.set_settings(SettingsData {
            toolbar: layout.toolbar,
            panel_toolbar: layout.panel_toolbar,
            drive_buttons: layout.drive_buttons,
            drive_list: layout.drive_list,
            folder_tabs: layout.folder_tabs,
            current_path: layout.current_path,
            breadcrumbs: layout.breadcrumbs,
            history_buttons: layout.history_buttons,
            column_headers: layout.column_headers,
            panel_status: layout.panel_status,
            command_line: layout.command_line,
            function_keys: layout.function_keys,
            numpad_folders: layout.numpad_folders,
            quick_find_start: match layout.quick_find_start {
                profile::QuickFindStart::Shortcut => 0,
                profile::QuickFindStart::Typing => 1,
                profile::QuickFindStart::Alt => 2,
                profile::QuickFindStart::CtrlAlt => 3,
            },
            stacked: profile.stacked,
            light: profile.light_mode,
            system_theme: profile.system_theme,
            show_hidden: profile.show_hidden,
            list_font: match profile.list_font {
                ListFont::SegoeUi => 0,
                ListFont::Arial => 1,
                ListFont::Verdana => 2,
            },
            list_font_size: FILE_FONT_SIZES
                .iter()
                .position(|&size| size == profile.list_font_size)
                .unwrap_or(1) as i32,
        });
        ui.global::<Colors>()
            .set_light(self.effective_light_theme());
        self.apply_color_settings();
        ui.set_stacked(profile.stacked);
        self.publish_buttonbars();
    }

    pub fn set_hidden(&mut self, show: bool) {
        if self.profile.show_hidden == show {
            return;
        }
        self.profile.show_hidden = show;
        for side in 0..2 {
            for tab in &mut self.panels[side].tabs {
                if tab.loaded {
                    tab.filter(tab.filter.clone(), show);
                }
            }
            self.publish(side);
            if self.panels[side].current().shell.is_some() {
                self.refresh(side);
            } else {
                self.invalidate_tree(side);
            }
        }
    }

    fn apply_settings(&mut self, value: SettingsData, close: bool) {
        self.apply_shortcuts();
        self.profile.layout = LayoutSettings {
            toolbar: value.toolbar,
            panel_toolbar: value.panel_toolbar,
            drive_buttons: value.drive_buttons,
            drive_list: value.drive_list,
            folder_tabs: value.folder_tabs,
            current_path: value.current_path,
            breadcrumbs: value.breadcrumbs,
            history_buttons: value.history_buttons,
            column_headers: value.column_headers,
            panel_status: value.panel_status,
            command_line: value.command_line,
            function_keys: value.function_keys,
            numpad_folders: value.numpad_folders,
            quick_find_start: match value.quick_find_start {
                1 => profile::QuickFindStart::Typing,
                2 => profile::QuickFindStart::Alt,
                3 => profile::QuickFindStart::CtrlAlt,
                _ => profile::QuickFindStart::Shortcut,
            },
        };
        self.profile.stacked = value.stacked;
        self.profile.light_mode = value.light;
        self.profile.system_theme = value.system_theme;
        self.profile.list_font = match value.list_font {
            1 => ListFont::Arial,
            2 => ListFont::Verdana,
            _ => ListFont::SegoeUi,
        };
        self.profile.list_font_size = FILE_FONT_SIZES
            .get(value.list_font_size as usize)
            .copied()
            .unwrap_or(14);
        self.set_hidden(value.show_hidden);
        self.sync_ui_settings();
        self.status("Einstellungen angewendet.");
        self.save();
        if close {
            self.close_settings();
        }
    }

    fn show_drives(&self, side: usize) {
        let drives = arco_windows::drives();
        let Some(panel) = self.panels.get(side) else {
            return;
        };
        let root = panel.current().path.ancestors().last();
        let selected = drives
            .iter()
            .position(|path| {
                root.is_some_and(|root| {
                    display_path(root).eq_ignore_ascii_case(&display_path(path))
                })
            })
            .unwrap_or(0);
        let ui = self.ui();
        ui.set_drives(strings(drives.iter().map(|path| display_path(path))));
        ui.set_drive_selection(selected as i32);
        ui.set_drive_side(side as i32);
    }

    fn choose_drive(&mut self, drive: &Path) -> bool {
        let ui = self.ui();
        let side = ui.get_drive_side() as usize;
        if side >= self.panels.len() || !arco_windows::drives().iter().any(|path| path == drive) {
            return false;
        }
        ui.set_drive_side(-1);
        self.set_active(side);
        self.load(side, drive.to_path_buf(), Navigation::Jump);
        ui.invoke_focus_panels();
        true
    }

    fn is_repeated_pointer_open(&mut self) -> bool {
        let Some(point) = arco_windows::cursor_screen() else {
            return false;
        };
        let now = Instant::now();
        if self.last_pointer_open.is_some_and(|(last, previous)| {
            arco_windows::is_double_click(previous, point, now.duration_since(last))
        }) {
            return true;
        }
        self.last_pointer_open = Some((now, point));
        false
    }

    pub fn ui(&self) -> MainWindow {
        self.window
            .upgrade()
            .expect("window outlives app callbacks")
    }

    pub fn status(&self, text: impl Into<SharedString>) {
        self.ui().set_status(text.into());
    }

    pub fn set_active(&mut self, side: usize) {
        self.active = side.min(1);
        self.ui().set_active_panel(self.active as i32);
    }

    pub fn current(&self) -> &Tab {
        self.panels[self.active].current()
    }

    pub fn current_mut(&mut self) -> &mut Tab {
        self.panels[self.active].current_mut()
    }

    pub fn target(&self) -> &Path {
        &self.panels[1 - self.active].current().path
    }

    pub fn publish(&self, side: usize) {
        let mut data = self.panels[side].snapshot();
        self.publish_columns(side, &mut data);
        if side == 0 {
            self.ui().set_left(data);
        } else {
            self.ui().set_right(data);
        }
        self.publish_cursor(side, true);
        self.publish_busy(side);
        self.publish_tree(side);
    }

    pub fn publish_cursor(&self, side: usize, selection_changed: bool) {
        let ui = self.ui();
        let tab = self.panels[side].current();
        if side == 0 {
            ui.set_left_cursor(tab.cursor_display());
            if selection_changed {
                ui.set_left_summary(tab.summary().into());
            }
        } else {
            ui.set_right_cursor(tab.cursor_display());
            if selection_changed {
                ui.set_right_summary(tab.summary().into());
            }
        }
    }

    fn publish_busy(&self, side: usize) {
        let ui = self.ui();
        if side == 0 {
            ui.set_left_busy(self.panels[side].loading);
        } else {
            ui.set_right_busy(self.panels[side].loading);
        }
    }

    pub fn save(&mut self) {
        if !self.save_profile {
            return;
        }
        self.profile.panels = [self.panels[0].saved(), self.panels[1].saved()];
        self.profile.active_panel = self.active;
        self.profile.split_ratio = self.ui().get_split_ratio();
        if let Err(error) = self.profile.save(&self.profile_path) {
            self.status(format!("Profil speichern: {error}"));
        }
    }

    fn profile_snapshot(&self) -> Profile {
        let mut snapshot = self.profile.clone();
        snapshot.panels = self.panels.each_ref().map(Panel::saved);
        snapshot.active_panel = self.active;
        snapshot.split_ratio = self.ui().get_split_ratio();
        snapshot
    }

    pub fn select_tab(&mut self, index: usize) {
        let panel = &mut self.panels[self.active];
        if index >= panel.tabs.len() {
            return;
        }
        if index == panel.tab {
            self.return_to_tab_base();
            return;
        }
        panel.tab = index;
        panel.loading = false;
        panel.pending_parent = None;
        panel.request = 0;
        self.waiting_loads[self.active] = None;
        if let Some(cancel) = &self.loader_cancel[self.active] {
            cancel.store(true, Ordering::Relaxed);
        }
        self.publish(self.active);
        if !self.return_to_tab_base() && (!self.current().loaded || !self.current().is_local()) {
            self.refresh(self.active);
        }
    }

    pub fn navigate_text(&mut self, value: &str) {
        let value = value.trim().trim_matches('"');
        if value.is_empty() {
            return;
        }
        if cfg!(windows) && ShellLocation::is_address(value) {
            self.open_shell_panel(self.active, value);
            return;
        }
        if let Some(wfx) = self.current().wfx.clone()
            && !Path::new(value).is_absolute()
            && !(value.len() == 2 && value.as_bytes().get(1) == Some(&b':'))
        {
            self.navigate_wfx(wfx, value);
            return;
        }
        if let Some(ftp) = self.current().ftp.as_ref()
            && !Path::new(value).is_absolute()
            && !(value.len() == 2 && value.as_bytes().get(1) == Some(&b':'))
        {
            let location = ftp.location.clone();
            self.navigate_ftp_text(location, value);
            return;
        }
        if let Some((archive, folder)) = value.split_once("::") {
            let archive = self.current().path.join(archive);
            let folder = PathBuf::from(folder.trim_start_matches(['/', '\\']));
            if archive_view::is_openable(&archive) && !folder.has_root() {
                self.load_location(
                    self.active,
                    Location::Archive {
                        file: archive,
                        folder,
                    },
                    Navigation::Jump,
                );
            } else {
                self.status("Ungültiger Archivpfad.");
            }
            return;
        }
        let mut path = PathBuf::from(value);
        if path.as_os_str().is_empty() {
            return;
        }
        #[cfg(windows)]
        if value.as_bytes().get(1) == Some(&b':') && value.len() == 2 {
            path.push("\\");
        }
        if path.is_relative() {
            if self.current().archive.is_some() {
                let mut location = self.current().location();
                for component in path.components() {
                    match component {
                        std::path::Component::CurDir => (),
                        std::path::Component::ParentDir => {
                            if let Some(parent) = location.parent() {
                                location = parent;
                            }
                        }
                        std::path::Component::Normal(name) => match &mut location {
                            Location::Local(path) => path.push(name),
                            Location::Archive { folder, .. } => folder.push(name),
                            Location::Ftp(_) | Location::Wfx(_) | Location::Shell(_) => return,
                        },
                        _ => return,
                    }
                }
                self.load_location(self.active, location, Navigation::Jump);
                return;
            }
            path = self.current().path.join(path);
        }
        self.load(self.active, path, Navigation::Jump);
    }

    pub fn open_path(&mut self, path: &Path) {
        if path.is_dir() {
            self.load(self.active, path.to_path_buf(), Navigation::Jump);
        } else if let Some(result) = self.open_internal_association(path) {
            self.report(result);
        } else if archive_view::is_supported(path) {
            self.open_archive(path.to_path_buf());
        } else if let Err(error) = arco_windows::open(path) {
            self.status(format!("Öffnen: {error}"));
        }
    }

    pub fn go_up(&mut self) {
        if let Some(parent) = self.panels[self.active].next_parent() {
            self.load_location(self.active, parent, Navigation::Up);
        }
    }

    pub fn open_row(&mut self, row: usize) {
        if self.panels[self.active].loading {
            return;
        }
        let tab = self.current();
        if tab.parent_row(row) {
            self.go_up();
        } else if tab.shell.is_some() {
            self.open_shell_panel_row(row);
        } else if tab.wfx.is_some() {
            self.open_wfx_row(row);
        } else if let Some(ftp) = &tab.ftp {
            if let Some(entry) = tab
                .display_row(row)
                .and_then(|entry| ftp.entry(&entry.path))
            {
                let (location, entry) = (ftp.location.clone(), entry.clone());
                self.open_ftp_entry(location, entry);
            }
        } else if let Some(archive) = tab.archive.as_ref() {
            if let Some(entry) = tab.display_row(row) {
                let member = entry.path.clone();
                if entry.kind == Kind::Directory {
                    self.load_location(
                        self.active,
                        Location::Archive {
                            file: archive.file.clone(),
                            folder: member,
                        },
                        Navigation::Jump,
                    );
                } else {
                    self.archive_preview_dialog(member, false);
                }
            }
        } else if let Some(entry) = tab.row(row) {
            let path = entry.path.clone();
            if entry.kind == Kind::Other {
                self.status("Nicht unterstützter Dateityp.");
            } else {
                self.open_path(&path);
            }
        }
    }

    pub fn refresh(&mut self, side: usize) {
        self.invalidate_tree(side);
        self.load_location(
            side,
            self.panels[side].current().location(),
            Navigation::Refresh,
        );
    }

    pub fn refresh_both(&mut self) {
        self.refresh(0);
        self.refresh(1);
    }

    pub fn load(&mut self, side: usize, target: PathBuf, navigation: Navigation) {
        self.load_location(side, Location::Local(target), navigation);
    }

    pub fn load_location(&mut self, side: usize, target: Location, navigation: Navigation) {
        self.load_location_with_password(side, target, navigation, None);
    }

    pub(crate) fn load_location_with_password(
        &mut self,
        side: usize,
        target: Location,
        mut navigation: Navigation,
        password: Option<crate::panel::ArchivePassword>,
    ) {
        if let Location::Wfx(location) = &target
            && !self.wfx_enabled(&location.plugin)
        {
            self.status("Dieses WFX-Plugin ist deaktiviert oder wurde entfernt.");
            return;
        }
        let panel = &mut self.panels[side];
        let password = password.or_else(|| match (&target, &panel.current().archive) {
            (Location::Archive { file, .. }, Some(archive)) if file == &archive.file => {
                archive.password.clone()
            }
            _ => None,
        });
        let cached = match (&target, &panel.current().archive, &navigation) {
            (Location::Archive { file, .. }, Some(archive), mode)
                if file == &archive.file && !matches!(mode, Navigation::Refresh) =>
            {
                Some(archive.catalog.clone())
            }
            _ => None,
        };
        if panel.current().options.lock == TabLock::Fixed
            && !matches!(navigation, Navigation::Refresh)
            && panel.current().location() != target
        {
            panel.tabs.push(Tab::new(panel.current().path.clone()));
            panel.tab = panel.tabs.len() - 1;
            if matches!(navigation, Navigation::Back | Navigation::Forward) {
                navigation = Navigation::Jump;
            }
        }
        self.next_request += 1;
        panel.request = self.next_request;
        panel.loading = true;
        panel.pending_parent = matches!(navigation, Navigation::Up).then(|| target.clone());
        let request = DirectoryRequest {
            id: panel.request,
            target,
            navigation,
            cached,
            password,
            comments: panel.current().shows_comments() || self.profile.views.needs_comments(),
            hidden: self.profile.show_hidden,
        };
        if panel.current().loaded {
            self.publish_busy(side);
        } else {
            self.publish(side);
        }
        if let Some(cancel) = &self.loader_cancel[side] {
            cancel.store(true, Ordering::Relaxed);
            self.waiting_loads[side] = Some(request);
        } else {
            self.spawn_loader(side, request);
        }
    }

    fn spawn_loader(&mut self, side: usize, request: DirectoryRequest) {
        let cancel = Arc::new(AtomicBool::new(false));
        self.loader_cancel[side] = Some(cancel.clone());
        let sender = self.sender.clone();
        let window = self.window.clone();
        std::thread::spawn(move || {
            let DirectoryRequest {
                id,
                mut target,
                navigation,
                cached,
                password,
                comments,
                hidden,
            } = request;
            let job = JobContext {
                cancel,
                sender: sender.clone(),
            };
            let directory =
                load_directory(&mut target, cached, password, comments, hidden, &job).map(Box::new);
            if sender
                .send(Event::Load {
                    side,
                    id,
                    target,
                    navigation,
                    directory,
                })
                .is_ok()
            {
                let _ = window.upgrade_in_event_loop(|window| window.invoke_directory_loaded());
            }
        });
    }

    pub fn start_job(
        &mut self,
        label: &str,
        refresh: bool,
        work: impl FnOnce(JobContext) -> io::Result<Outcome> + Send + 'static,
    ) {
        if self.job.is_some() {
            self.status("Ein Auftrag läuft bereits.");
            return;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        let sender = self.sender.clone();
        self.job = Some(cancel.clone());
        self.ui().set_job_status(label.into());
        self.ui().set_job_active(true);
        std::thread::spawn(move || {
            let context = JobContext {
                cancel,
                sender: sender.clone(),
            };
            let outcome = work(context).map_err(|error| {
                if error.kind() == io::ErrorKind::Interrupted {
                    "Auftrag abgebrochen.".into()
                } else {
                    error.to_string()
                }
            });
            let _ = sender.send(Event::Finished { outcome, refresh });
        });
    }

    fn drain(&mut self) {
        self.listers.retain(|lister| lister.window().is_visible());
        let mut preview_failed = None;
        self.plugin_previews
            .retain_mut(|(name, host)| match host.try_wait() {
                Ok(None) => true,
                result => {
                    if !matches!(result, Ok(Some(status)) if status.success()) {
                        preview_failed = Some(name.clone());
                    }
                    false
                }
            });
        if let Some(name) = preview_failed {
            self.status(format!("WLX-Plugin „{name}“ wurde unerwartet beendet."));
        }
        while let Ok(event) = self.receiver.try_recv() {
            match event {
                Event::Load {
                    side,
                    id,
                    target,
                    navigation,
                    directory,
                } => {
                    self.loader_cancel[side] = None;
                    self.loaded(side, id, target, navigation, directory);
                    if let Some(request) = self.waiting_loads[side].take() {
                        self.spawn_loader(side, request);
                    }
                }
                Event::Progress(text) => self.ui().set_job_status(text.into()),
                Event::PluginPrompt(ticket) => self.plugin_prompts.push_back(ticket),
                Event::TransferConflict(ticket) => self.transfers.prompt = Some(ticket),
                Event::WdxValues(values) => self.apply_wdx_values(values),
                Event::Thumbnails(loaded) => self.apply_thumbnails(loaded),
                Event::TreeLoaded(loaded) => self.tree_loaded(loaded),
                Event::QuickPreview(target, result) => {
                    self.quick_running = false;
                    let cancelled = self
                        .quick_cancel
                        .take()
                        .is_some_and(|cancel| cancel.load(Ordering::Relaxed));
                    if !cancelled
                        && self.quick_requested.as_ref() == Some(&target)
                        && self.quick_target().as_ref() == Some(&target)
                    {
                        self.quick_ready = true;
                        self.accept_quick_result(result);
                    }
                }
                Event::Finished { outcome, refresh } => {
                    self.job = None;
                    self.finish_virtual_transfer();
                    self.ui().set_job_active(false);
                    if refresh {
                        self.refresh_both();
                    }
                    match outcome {
                        Ok(outcome) => self.completed(outcome),
                        Err(error) => self.status(error),
                    }
                    self.finish_wdx_sort_attempt();
                }
            }
        }
        self.open_next_plugin_prompt();
        self.open_next_transfer_conflict();
        self.advance_transfers();
        self.update_wdx_columns();
        self.update_thumbnails();
        self.update_trees();
        self.update_quick_preview();
        if let Some((side, paths)) = self.drops.take() {
            if self.ui().get_dialog_kind().is_empty()
                && !self.ui().get_settings_open()
                && self.ui().get_drive_side() < 0
                && self.job.is_none()
            {
                self.set_active(side);
                if !self.current().is_local() {
                    self.status("Wechsle für Drag-and-drop in einen lokalen Zielordner.");
                } else {
                    self.transfer_dialog(paths, self.current().path.clone(), Transfer::Copy);
                }
            } else {
                self.status(
                    "Schließe den Dialog oder beende den laufenden Auftrag vor dem Ablegen.",
                );
            }
        }
        self.update_wdx_sort();
    }

    pub fn toggle_quick_preview(&mut self) {
        self.stop_quick_plugin();
        self.quick_open = !self.quick_open;
        self.quick_requested = None;
        self.quick_ready = false;
        let ui = self.ui();
        ui.set_quick_open(self.quick_open);
        ui.set_quick_title("".into());
        ui.set_quick_image(slint::Image::default());
        ui.set_quick_text("Wähle eine Datei im aktiven Fenster.".into());
        ui.set_quick_note("".into());
    }

    fn quick_target(&self) -> Option<QuickTarget> {
        if !self.quick_open || self.panels[self.active].loading {
            return None;
        }
        let tab = self.current();
        if tab.ftp.is_some() || tab.wfx.is_some() {
            return None;
        }
        let entry = tab
            .display_row(tab.cursor)
            .filter(|entry| entry.kind == Kind::File)?;
        Some(QuickTarget {
            path: entry.path.clone(),
            modified: entry.modified,
            size: entry.size,
            archive: tab.archive.as_ref().map(|archive| {
                (
                    archive.file.clone(),
                    self.panels[self.active].request,
                    entry.attributes & 0x4000 != 0,
                )
            }),
        })
    }

    fn update_quick_preview(&mut self) {
        let target = self.quick_target();
        if self.quick_requested != target {
            self.stop_quick_plugin();
            self.quick_skip_plugin = false;
            self.quick_requested = target.clone();
            self.quick_ready = false;
            let ui = self.ui();
            ui.set_quick_image(slint::Image::default());
            ui.set_quick_note("".into());
            if let Some(target) = &target {
                let name = target.path.file_name().unwrap_or(target.path.as_os_str());
                ui.set_quick_title(name.to_string_lossy().as_ref().into());
                ui.set_quick_text("Vorschau wird geladen …".into());
            } else {
                ui.set_quick_title("".into());
                ui.set_quick_text("Wähle eine Datei im aktiven Fenster.".into());
            }
        }
        self.position_quick_plugin();
        if !self.quick_running
            && !self.quick_ready
            && let Some(target) = target
        {
            self.quick_running = true;
            let cancel = Arc::new(AtomicBool::new(false));
            self.quick_cancel = Some(cancel.clone());
            let plugin = self.quick_plugin(&target);
            let sender = self.sender.clone();
            std::thread::spawn(move || {
                let plugin_error = if let Some((plugin, parent)) = plugin {
                    match arco_core::plugin::embedded_view(
                        &plugin.path,
                        &target.path,
                        parent,
                        &cancel,
                    ) {
                        Ok((host, window)) => {
                            let preview =
                                wlx::EmbeddedPreview::new(plugin.name, parent, window, host);
                            let _ = sender.send(Event::QuickPreview(
                                target,
                                wlx::QuickResult::Embedded(Box::new(preview)),
                            ));
                            return;
                        }
                        Err(error) if !cancel.load(Ordering::Relaxed) => {
                            Some(format!("WLX-Plugin „{}“ · {error}", plugin.name))
                        }
                        Err(_) => None,
                    }
                } else {
                    None
                };
                if cancel.load(Ordering::Relaxed) {
                    let _ = sender.send(Event::QuickPreview(
                        target,
                        wlx::QuickResult::Internal(Err(io::ErrorKind::Interrupted.into()), None),
                    ));
                    return;
                }
                let result = match &target.archive {
                    Some((_, _, true)) => Err(io::Error::other(
                        "Verschlüsselter Eintrag. Verwende F3 für die Passwortabfrage.",
                    )),
                    Some((archive, _, false)) => {
                        archive_actions::read_archive_preview(archive, &target.path)
                    }
                    None => preview::read_raw(&target.path),
                };
                let _ = sender.send(Event::QuickPreview(
                    target,
                    wlx::QuickResult::Internal(result, plugin_error),
                ));
            });
        }
    }

    fn show_quick_result(&self, result: io::Result<preview::Content>) {
        let ui = self.ui();
        match result {
            Ok(preview::Content::Page(mode, page)) => {
                let mode = if mode == PreviewMode::Hex {
                    "Hex"
                } else {
                    "Text"
                };
                ui.set_quick_note(
                    format!(
                        "{mode} · {} von {} Byte · F3 für die Vollansicht",
                        page.next, page.length
                    )
                    .into(),
                );
                ui.set_quick_text(page.text.into());
            }
            Ok(preview::Content::Image(image)) => {
                ui.set_quick_text("".into());
                ui.set_quick_note("Bild · F3 für die Vollansicht".into());
                ui.set_quick_image(preview::to_slint(&image));
            }
            Ok(preview::Content::Rich(content)) => {
                ui.set_quick_text("".into());
                ui.set_quick_note(
                    format!("{} · F3 für die formatierte Vorschau", content.label()).into(),
                );
            }
            Err(error) => ui.set_quick_text(format!("Vorschau: {error}").into()),
        }
    }

    fn drop_side(&self, window: &slint::Window) -> usize {
        let Some((x, y)) = arco_windows::cursor_screen() else {
            return self.active;
        };
        let Some((left, top, scale)) = window_origin(window) else {
            return self.active;
        };
        self.ui()
            .invoke_panel_at((x - left) as f32 / scale, (y - top) as f32 / scale) as usize
    }

    fn loaded(
        &mut self,
        side: usize,
        id: u64,
        target: Location,
        navigation: Navigation,
        directory: io::Result<Box<LoadedDirectory>>,
    ) {
        let panel = &mut self.panels[side];
        if panel.request != id {
            return;
        }
        panel.loading = false;
        panel.pending_parent = None;
        match directory {
            Ok(loaded) => {
                let LoadedDirectory {
                    directory,
                    space,
                    archive,
                    ftp,
                    shell,
                } = *loaded;
                let tab = panel.current_mut();
                let previous = tab.location();
                let focus = if previous == target {
                    tab.display_row(tab.cursor).map(|entry| entry.path.clone())
                } else {
                    let mut child = previous.clone();
                    while matches!(navigation, Navigation::Up)
                        && let Some(parent) = child.parent()
                        && parent != target
                    {
                        child = parent;
                    }
                    (child.parent().as_ref() == Some(&target)).then(|| child.row_key())
                };
                if previous != target {
                    match navigation {
                        Navigation::Jump | Navigation::Up | Navigation::FtpProfile { .. } => {
                            tab.back.push(previous);
                            tab.forward.clear();
                        }
                        Navigation::Back => {
                            tab.back.pop();
                            tab.forward.push(previous);
                        }
                        Navigation::Forward => {
                            tab.forward.pop();
                            tab.back.push(previous);
                        }
                        Navigation::Refresh => (),
                    }
                    tab.change_location(&target, archive);
                    tab.manual_view = false;
                } else {
                    tab.archive = archive;
                    if let Location::Shell(shell) = &target {
                        tab.shell = Some(shell.clone());
                    }
                }
                tab.ftp = ftp;
                tab.shell_entries = shell;
                tab.view_scope = None;
                if !tab.manual_view {
                    tab.view_override = view_setup::automatic_view(
                        &self.profile.views,
                        &target,
                        None,
                        &directory.entries,
                    );
                }
                tab.volume = space
                    .map(|space| {
                        format!(
                            "{} frei von {}",
                            human_size(space.available),
                            human_size(space.total)
                        )
                    })
                    .unwrap_or_else(|| {
                        if let Some(ftp) = &tab.ftp {
                            match ftp.listing.format {
                                ftp::ListFormat::Mlsd => "FTP · MLSD",
                                ftp::ListFormat::List => "FTP · LIST",
                                ftp::ListFormat::Names => "FTP · Namensliste",
                            }
                            .into()
                        } else if tab.archive.is_some() {
                            "Archiv".into()
                        } else if let Some(wfx) = &tab.wfx {
                            format!("WFX · {}", wfx.plugin.name)
                        } else if tab.shell.is_some() {
                            "Windows-Shell".into()
                        } else {
                            String::new()
                        }
                    });
                tab.clear_comparison_filter();
                tab.ignore = self.ignore.clone();
                tab.colors = self.file_colors.clone();
                tab.populate_with_focus(
                    directory.entries,
                    self.profile.show_hidden,
                    String::new(),
                    focus.as_deref(),
                );
                if !directory.errors.is_empty() {
                    self.status(format!(
                        "{} Einträge konnten nicht gelesen werden.",
                        directory.errors.len()
                    ));
                }
                if let Navigation::FtpProfile {
                    local,
                    opposite_request,
                } = navigation
                    && self.panels[1 - side].request == opposite_request
                {
                    self.load(1 - side, local, Navigation::Jump);
                }
            }
            Err(error) => {
                if archive_view::needs_header_password(&error)
                    && side == self.active
                    && self.pending.is_none()
                {
                    self.dialog("input", Pending::ArchiveUnlock(side, target, navigation), Dialog {
                        title: "Verschlüsseltes 7z-Archiv öffnen",
                        description: "Gib das Passwort ein, um die Dateinamen im Archiv anzuzeigen.",
                        placeholder: "Archivpasswort",
                        secret: true,
                        ..Dialog::default()
                    });
                    self.publish(side);
                    return;
                }
                self.status(format!("{}: {error}", target.display()));
                if matches!(navigation, Navigation::Refresh)
                    && error.kind() == io::ErrorKind::NotFound
                    && let Location::Archive { file, folder } = target
                {
                    let fallback = if folder.as_os_str().is_empty() {
                        file.parent()
                            .map(|path| Location::Local(path.to_path_buf()))
                    } else {
                        Some(Location::Archive {
                            file,
                            folder: PathBuf::new(),
                        })
                    };
                    if let Some(fallback) = fallback {
                        self.load_location(side, fallback, Navigation::Refresh);
                    }
                }
            }
        }
        self.publish(side);
    }

    fn completed(&mut self, outcome: Outcome) {
        match outcome {
            Outcome::SearchReview(review) => self.show_search_review(review),
            Outcome::FileComment(path, comment) => self.show_comment(path, comment),
            Outcome::PrintPreview(title, body) => self.show_print_preview(title, body),
            Outcome::TransferFinished(report) => self.complete_transfer(report),
            Outcome::Notice(message) => self.status(message),
            Outcome::Text(title, body) => self.text(&title, &body),
            Outcome::ClipboardText(body) => self.report(arco_windows::copy_text(&body)),
            Outcome::Entries {
                side,
                location,
                request,
                entries,
                label,
                scope,
            } => {
                if self.panels[side].request == request
                    && self.panels[side].current().location() == location
                {
                    let warnings = if entries.errors.is_empty() {
                        String::new()
                    } else {
                        format!(" · {} Lesefehler", entries.errors.len())
                    };
                    let tab = self.panels[side].current_mut();
                    tab.clear_selected_filter();
                    tab.manual_view = false;
                    tab.view_scope = Some(scope);
                    tab.view_override = view_setup::automatic_view(
                        &self.profile.views,
                        &location,
                        Some(scope),
                        &entries.entries,
                    );
                    tab.populate(
                        entries.entries,
                        self.profile.show_hidden,
                        format!(" · {label}{warnings}"),
                    );
                    self.publish(side);
                    if !entries.errors.is_empty() {
                        self.text("Lesefehler bei der Suche", &entries.errors.join("\n"));
                    }
                } else {
                    self.status("Ansicht hat sich während der Suche geändert.");
                }
            }
            Outcome::Report(label, report) => {
                let mut summary = format!(
                    "{label} · {} abgeschlossen, {} übersprungen",
                    report.completed, report.skipped
                );
                if report.ignored > 0 {
                    summary.push_str(&format!(
                        " · {} laut Ignorierliste ausgelassen",
                        report.ignored
                    ));
                }
                if report.cancelled {
                    summary.push_str(" · abgebrochen");
                }
                if report.errors.is_empty() {
                    self.status(summary);
                } else {
                    self.text(&summary, &report.errors.join("\n"));
                }
            }
            Outcome::RenamePreview(plan) => self.rename_review(plan),
            Outcome::RenameNameList(list, directory) => self.show_rename_names(list, directory),
            Outcome::Renamed(plan) => {
                self.status(format!("{} Einträge umbenannt.", plan.entries().len()));
                self.undo = plan.inverse().ok();
            }
            Outcome::Sync(plan) => self.show_sync(plan),
            Outcome::ComparedPanels {
                roots,
                requests,
                marks,
                hide_equal,
            } => {
                self.show_panel_comparison(&roots, requests, marks, hide_equal);
            }
            Outcome::FolderSizes {
                side,
                request,
                root,
                sizes,
                skipped,
            } => {
                self.show_folder_sizes(side, request, &root, sizes, skipped);
            }
            Outcome::Ftp(view) => self.show_ftp(view),
            Outcome::FtpEditReady(draft) => self.open_downloaded_ftp_draft(*draft),
            Outcome::FtpEditUploaded(draft) => self.finish_ftp_upload(*draft),
            Outcome::Imported(import) => self.show_tc_import(import),
            Outcome::ImportedTabs(name, import) => self.review_tab_import(name, *import),
            Outcome::TabFile(import, replace) => self.apply_tab_file(*import, replace),
            Outcome::ImportedFtp(sites) => self.review_ftp_import(sites),
            Outcome::PluginPreview(name, host) => {
                self.status(format!("WLX-Vorschau geöffnet · {name}"));
                self.plugin_previews.push((name, host));
            }
            Outcome::PluginPackage(package) => self.review_plugin_package(*package),
            Outcome::PluginImport(import) => self.review_plugin_import(import),
            Outcome::WdxFields(entry, fields, search) => {
                self.choose_wdx_field(*entry, fields, search)
            }
            Outcome::WdxSorted(entries) => self.apply_wdx_order(entries),
            Outcome::WdxEditable(target, fields) => self.choose_wdx_edit_field(target, fields),
            Outcome::IgnoreImported(rules) => self.edit_ignore_rules(
                &rules,
                Some(
                    "Importierte Liste prüfen und speichern, um die bisherigen Regeln zu ersetzen.",
                ),
            ),
            Outcome::Preview { path, mode, page } => self.show_preview(path, mode, page),
            Outcome::ViewerPage(viewer, page) => {
                if self
                    .viewer
                    .as_ref()
                    .is_some_and(|open| open.path == viewer.path)
                {
                    self.show_viewer_page(viewer, page);
                }
            }
            Outcome::TextCompared(session, index, must_be_open) => {
                if !must_be_open || matches!(self.pending, Some(Pending::TextCompare(_, _))) {
                    self.show_text_comparison(session, index);
                }
            }
            Outcome::BinaryCompared(session, page, must_be_open) => {
                if !must_be_open || matches!(self.pending, Some(Pending::BinaryCompare(_, _))) {
                    self.show_binary_comparison(session, page);
                }
            }
            Outcome::LocalShares(shares) => self.show_shares(shares),
            Outcome::VolumeLabel(volume) => self.show_volume_label(volume),
            Outcome::Image(path, image) => {
                let ui = self.ui();
                ui.set_dialog_title(display_path(&path).into());
                ui.set_dialog_description("".into());
                ui.set_dialog_options(strings(std::iter::empty::<&str>()));
                ui.set_viewer_image(preview::to_slint(&image));
                self.preview_image_path = Some(path);
                ui.set_dialog_kind("image".into());
            }
            Outcome::RichPreview(path, content) => self.open_rich_lister(path, content),
            Outcome::StagedPreview(file, content) => self.show_staged_preview(file, content),
        }
    }

    pub fn cancel(&self) {
        if let Some(cancel) = &self.job {
            cancel.store(true, Ordering::Relaxed);
            self.ui().set_job_status("Abbruch angefordert …".into());
        }
    }

    pub fn text(&mut self, title: &str, body: &str) {
        self.pending = None;
        let ui = self.ui();
        ui.set_dialog_kind("text".into());
        ui.set_dialog_title(title.into());
        ui.set_dialog_description("".into());
        ui.set_dialog_options(strings(std::iter::empty::<&str>()));
        ui.set_dialog_value(body.into());
    }

    pub fn new_tab(&mut self) {
        let location = self.current().location();
        let current = self.current().path.clone();
        let panel = &mut self.panels[self.active];
        panel.tabs.push(Tab::new(current));
        let index = panel.tabs.len() - 1;
        panel.tab = index;
        self.load_location(self.active, location, Navigation::Jump);
    }

    pub fn close_tab(&mut self) {
        let panel = &mut self.panels[self.active];
        if panel.tabs.len() <= 1 {
            return;
        }
        let removed = panel.tabs.remove(panel.tab);
        panel.tab = panel.tab.min(panel.tabs.len() - 1);
        panel.loading = false;
        panel.pending_parent = None;
        panel.request = 0;
        self.waiting_loads[self.active] = None;
        if let Some(cancel) = &self.loader_cancel[self.active] {
            cancel.store(true, Ordering::Relaxed);
        }
        self.remember_closed_tab(self.active, removed);
        self.publish(self.active);
        if !self.return_to_tab_base() && (!self.current().loaded || !self.current().is_local()) {
            self.refresh(self.active);
        }
    }
}

pub fn strings<I, S>(values: I) -> ModelRc<SharedString>
where
    I: IntoIterator<Item = S>,
    S: Into<SharedString>,
{
    let list: Vec<_> = values.into_iter().map(Into::into).collect();
    ModelRc::new(VecModel::from(list))
}
