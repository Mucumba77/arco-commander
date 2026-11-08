use crate::{FileRow, PanelData, PathItem, display_path, human_size};
use arco_core::archive_view::Catalog;
use arco_core::files::{self, Entry, Kind, SortKey};
use arco_core::profile::{SavedPanel, SavedTab, TabLock};
use arco_windows::ftp;
use chrono::{DateTime, Local};
use slint::{Model, ModelNotify, ModelRc, ModelTracker, VecModel};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Location {
    Local(PathBuf),
    Archive { file: PathBuf, folder: PathBuf },
    Ftp(FtpLocation),
    Wfx(WfxLocation),
}

#[derive(Clone, Debug)]
pub struct WfxLocation {
    pub plugin: Arc<arco_core::plugin::Registration>,
    pub directory: String,
}

impl PartialEq for WfxLocation {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.plugin, &other.plugin) && self.directory == other.directory
    }
}

impl Eq for WfxLocation {}

impl WfxLocation {
    pub fn parent(&self) -> Option<Self> {
        if self.directory == "\\" {
            return None;
        }
        let (parent, _) = self.directory.rsplit_once('\\')?;
        Some(self.at(if parent.is_empty() { "\\" } else { parent }))
    }

    pub fn at(&self, directory: &str) -> Self {
        Self {
            plugin: self.plugin.clone(),
            directory: directory.into(),
        }
    }

    pub fn child(&self, name: &str) -> String {
        format!("{}\\{name}", self.directory.trim_end_matches('\\'))
    }
}

#[derive(Clone)]
pub struct FtpLocation {
    pub session: Arc<ftp::Session>,
    pub directory: ftp::Url,
}

impl PartialEq for FtpLocation {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.session, &other.session) && self.directory == other.directory
    }
}

impl Eq for FtpLocation {}

impl std::fmt::Debug for FtpLocation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_tuple("FTP")
            .field(&self.directory.as_str())
            .finish()
    }
}

impl FtpLocation {
    pub fn at(&self, directory: ftp::Url) -> Self {
        Self {
            session: self.session.clone(),
            directory,
        }
    }

    pub fn parent(&self) -> Option<Self> {
        let parent = self.session.parent_directory(&self.directory).ok()?;
        (parent != self.directory).then(|| self.at(parent))
    }
}

impl Location {
    pub fn parent(&self) -> Option<Self> {
        match self {
            Self::Local(path) => path.parent().map(|path| Self::Local(path.to_path_buf())),
            Self::Archive { file, folder } if folder.as_os_str().is_empty() => {
                file.parent().map(|path| Self::Local(path.to_path_buf()))
            }
            Self::Archive { file, folder } => folder.parent().map(|folder| Self::Archive {
                file: file.clone(),
                folder: folder.to_path_buf(),
            }),
            Self::Ftp(ftp) => ftp.parent().map(Self::Ftp),
            Self::Wfx(wfx) => wfx.parent().map(Self::Wfx),
        }
    }

    pub fn row_key(&self) -> PathBuf {
        match self {
            Self::Local(path) => path.clone(),
            Self::Archive { file, folder } if folder.as_os_str().is_empty() => file.clone(),
            Self::Archive { folder, .. } => folder.clone(),
            Self::Ftp(ftp) => ftp::Session::directory_name(&ftp.directory)
                .map_or_else(PathBuf::new, PathBuf::from),
            Self::Wfx(wfx) => PathBuf::from(wfx.directory.rsplit('\\').next().unwrap_or_default()),
        }
    }

    pub fn display(&self) -> String {
        match self {
            Self::Local(path) => display_path(path),
            Self::Archive { file, folder } => {
                format!("{}::{}", display_path(file), display_path(folder))
            }
            Self::Ftp(ftp) => ftp.directory.as_str().to_owned(),
            Self::Wfx(wfx) => format!("{}::{}", wfx.plugin.name, wfx.directory),
        }
    }
}

#[derive(Clone)]
pub struct ArchiveTab {
    pub file: PathBuf,
    pub folder: PathBuf,
    pub catalog: Arc<Catalog>,
}

pub struct FtpTab {
    pub location: FtpLocation,
    pub listing: ftp::Listing,
}

impl FtpTab {
    pub fn entry(&self, path: &Path) -> Option<&ftp::Entry> {
        let name = path.to_str()?;
        self.listing.entries.iter().find(|entry| entry.name == name)
    }
}

pub struct Panel {
    pub tabs: Vec<Tab>,
    pub tab: usize,
    pub request: u64,
    pub loading: bool,
    pub pending_parent: Option<Location>,
}

impl Panel {
    pub fn from_saved(saved: SavedPanel, fallback: &Path) -> Self {
        let mut active = 0;
        let mut tabs: Vec<_> = saved
            .tabs
            .into_iter()
            .enumerate()
            .filter(|(_, path)| path.is_absolute())
            .enumerate()
            .map(|(position, (index, path))| {
                if index <= saved.active_tab {
                    active = position;
                }
                let mut tab = Tab::new(path);
                if let Some(options) = saved.tab_options.get(index) {
                    tab.set_lock(options.lock);
                    tab.options.title.clone_from(&options.title);
                }
                tab
            })
            .collect();
        if tabs.is_empty() {
            tabs.push(Tab::new(fallback.to_path_buf()));
        }
        Self {
            tabs,
            tab: active,
            request: 0,
            loading: false,
            pending_parent: None,
        }
    }

    pub fn current(&self) -> &Tab {
        &self.tabs[self.tab]
    }

    pub fn current_mut(&mut self) -> &mut Tab {
        &mut self.tabs[self.tab]
    }

    pub fn next_parent(&self) -> Option<Location> {
        if let Some(pending) = &self.pending_parent {
            pending.parent()
        } else {
            self.current().location().parent()
        }
    }

    pub fn saved(&self) -> SavedPanel {
        let mut tab_options: Vec<_> = self
            .tabs
            .iter()
            .map(|tab| {
                let mut options = tab.options.clone();
                if !tab
                    .lock_base
                    .as_ref()
                    .map_or_else(|| tab.is_local(), |base| matches!(base, Location::Local(_)))
                {
                    options.lock = TabLock::Unlocked;
                }
                options
            })
            .collect();
        while tab_options
            .last()
            .is_some_and(|options| *options == SavedTab::default())
        {
            tab_options.pop();
        }
        SavedPanel {
            tabs: self
                .tabs
                .iter()
                .map(|tab| {
                    if let Some(Location::Local(base)) = &tab.lock_base {
                        base.clone()
                    } else {
                        tab.path.clone()
                    }
                })
                .collect(),
            active_tab: self.tab,
            tab_options,
        }
    }

    pub fn snapshot(&self) -> PanelData {
        let tab = self.current();
        let tabs: Vec<_> = self
            .tabs
            .iter()
            .map(|tab| {
                let mut item = path_item(&tab.location(), 14);
                if !tab.options.title.is_empty() {
                    item.label = short_label(&tab.options.title, 14);
                }
                item.locked = tab.options.lock != TabLock::Unlocked;
                item
            })
            .collect();
        let crumbs = tab
            .crumbs()
            .into_iter()
            .map(|path| path_item(&path, 26))
            .collect::<Vec<_>>();
        PanelData {
            path: tab.location().display().into(),
            rows: ModelRc::from(tab.model.clone()),
            tabs: ModelRc::new(VecModel::from(tabs)),
            crumbs: ModelRc::new(VecModel::from(crumbs)),
            tab: self.tab as i32,
            filter: tab.filter.as_str().into(),
            detailed: tab.detailed,
            sort_column: match (tab.wdx_sort, tab.sort) {
                (Some(_), _) => "",
                (_, SortKey::Name) => "name",
                (_, SortKey::Extension) => "extension",
                (_, SortKey::Size) => "size",
                (_, SortKey::Modified) => "time",
                (_, SortKey::Attributes) => "attributes",
            }
            .into(),
            wdx_sort_index: tab.wdx_sort.map_or(-1, |(index, _)| index as i32),
            descending: tab
                .wdx_sort
                .map_or(tab.descending, |(_, descending)| descending),
            volume: tab.volume.as_str().into(),
            drive: if tab.ftp.is_some() || tab.wfx.is_some() {
                "".into()
            } else {
                tab.path
                    .ancestors()
                    .last()
                    .map(display_path)
                    .unwrap_or_default()
                    .into()
            },
            local: tab.is_local(),
        }
    }
}

fn path_item(path: &Location, label_width: usize) -> PathItem {
    let full = path.display();
    let key = path.row_key();
    let label = key.file_name().map(|name| name.to_string_lossy());
    let label = label.as_deref().unwrap_or(&full);
    PathItem {
        label: short_label(label, label_width),
        path: full.into(),
        locked: false,
    }
}

fn short_label(label: &str, width: usize) -> slint::SharedString {
    let mut chars = label.chars();
    let mut short: String = chars.by_ref().take(width).collect();
    if chars.next().is_some() {
        short.push('…');
    }
    short.into()
}

pub struct Tab {
    pub path: PathBuf,
    pub options: SavedTab,
    pub lock_base: Option<Location>,
    pub archive: Option<ArchiveTab>,
    pub ftp: Option<FtpTab>,
    pub wfx: Option<WfxLocation>,
    pub back: Vec<Location>,
    pub forward: Vec<Location>,
    pub filter: String,
    pub detailed: bool,
    pub cursor: usize,
    pub anchor: usize,
    pub sort: SortKey,
    pub descending: bool,
    pub(crate) wdx_sort: Option<(usize, bool)>,
    pub extra: String,
    pub loaded: bool,
    pub volume: String,
    entries: Rc<Vec<Entry>>,
    model: Rc<FileModel>,
    selection: Rc<RefCell<BTreeSet<PathBuf>>>,
    previous_selection: BTreeSet<PathBuf>,
    equal_filter: Option<BTreeSet<PathBuf>>,
    selected_size: u64,
    visible_size: u64,
    visible_folders: usize,
}

impl Tab {
    pub fn set_lock(&mut self, mode: TabLock) {
        self.options.lock = mode;
        self.lock_base = (mode == TabLock::ReturnToBase).then(|| self.location());
    }

    pub fn cursor_display(&self) -> i32 {
        if self.model.row_count() == 0 {
            -1
        } else {
            self.cursor as i32
        }
    }

    pub fn summary(&self) -> String {
        let count = self.selection.borrow().len();
        format!(
            "{} / {} · {count} / {} markiert · {} Ordner{}{}",
            human_size(self.selected_size),
            human_size(self.visible_size),
            self.model.visible.len(),
            self.visible_folders,
            self.extra,
            if self.has_comparison_filter() {
                " · Gleiche ausgeblendet"
            } else {
                ""
            },
        )
    }

    pub fn new(path: PathBuf) -> Self {
        let cursor = usize::from(path.parent().is_some());
        let entries = Rc::new(Vec::new());
        let selection = Rc::new(RefCell::new(BTreeSet::new()));
        let model = Rc::new(FileModel::new(
            entries.clone(),
            Vec::new(),
            selection.clone(),
            false,
        ));
        Self {
            path,
            options: SavedTab::default(),
            lock_base: None,
            archive: None,
            ftp: None,
            wfx: None,
            back: Vec::new(),
            forward: Vec::new(),
            filter: String::new(),
            detailed: true,
            cursor,
            anchor: cursor,
            sort: SortKey::Name,
            descending: false,
            wdx_sort: None,
            extra: String::new(),
            loaded: false,
            volume: String::new(),
            entries,
            model,
            selection,
            previous_selection: BTreeSet::new(),
            equal_filter: None,
            selected_size: 0,
            visible_size: 0,
            visible_folders: 0,
        }
    }

    pub fn location(&self) -> Location {
        if let Some(wfx) = &self.wfx {
            return Location::Wfx(wfx.clone());
        }
        if let Some(ftp) = &self.ftp {
            return Location::Ftp(ftp.location.clone());
        }
        self.archive.as_ref().map_or_else(
            || Location::Local(self.path.clone()),
            |archive| Location::Archive {
                file: archive.file.clone(),
                folder: archive.folder.clone(),
            },
        )
    }

    pub fn is_local(&self) -> bool {
        self.archive.is_none() && self.ftp.is_none() && self.wfx.is_none()
    }

    pub fn crumbs(&self) -> Vec<Location> {
        let mut location = Some(self.location());
        let mut crumbs = Vec::new();
        while let Some(current) = location {
            location = current.parent();
            crumbs.push(current);
        }
        crumbs.reverse();
        crumbs
    }

    pub fn change_location(&mut self, location: &Location, archive: Option<ArchiveTab>) {
        self.path = match location {
            Location::Local(path) => path.clone(),
            Location::Archive { file, .. } => file.parent().unwrap_or(&self.path).to_path_buf(),
            Location::Ftp(_) | Location::Wfx(_) => self.path.clone(),
        };
        self.archive = archive;
        self.ftp = None;
        self.wfx = if let Location::Wfx(wfx) = location {
            Some(wfx.clone())
        } else {
            None
        };
        self.cursor = usize::from(location.parent().is_some());
        self.anchor = self.cursor;
        self.filter.clear();
        self.selection.borrow_mut().clear();
        self.previous_selection.clear();
        self.selected_size = 0;
    }

    pub fn populate(&mut self, entries: Vec<Entry>, show_hidden: bool, extra: String) {
        let current = self
            .model
            .entry(self.cursor)
            .map(|entry| entry.path.clone());
        self.populate_with_focus(entries, show_hidden, extra, current.as_deref());
    }

    pub fn populate_with_focus(
        &mut self,
        mut entries: Vec<Entry>,
        show_hidden: bool,
        extra: String,
        focus: Option<&Path>,
    ) {
        self.wdx_sort = None;
        files::sort(&mut entries, self.sort, self.descending);
        self.entries = Rc::new(entries);
        self.extra = extra;
        self.loaded = true;
        self.rebuild(show_hidden, focus);
    }

    pub fn filter(&mut self, value: String, show_hidden: bool) {
        let current = self
            .model
            .entry(self.cursor)
            .map(|entry| entry.path.clone());
        self.filter = value;
        self.rebuild(show_hidden, current.as_deref());
    }

    pub fn set_sort(&mut self, key: SortKey, show_hidden: bool) {
        if self.sort == key && self.wdx_sort.is_none() {
            self.descending = !self.descending;
        } else {
            self.sort = key;
            self.descending = false;
        }
        self.populate(
            self.entries.as_ref().clone(),
            show_hidden,
            self.extra.clone(),
        );
    }

    pub(crate) fn entry_model(&self) -> Rc<Vec<Entry>> {
        self.entries.clone()
    }

    pub(crate) fn apply_wdx_sort(
        &mut self,
        index: usize,
        descending: bool,
        entries: Vec<Entry>,
        show_hidden: bool,
    ) {
        let current = self
            .display_row(self.cursor)
            .map(|entry| entry.path.clone());
        self.wdx_sort = Some((index, descending));
        self.entries = Rc::new(entries);
        self.rebuild(show_hidden, current.as_deref());
    }

    pub(crate) fn reset_wdx_sort(&mut self, show_hidden: bool) {
        if self.wdx_sort.is_some() {
            self.populate(
                self.entries.as_ref().clone(),
                show_hidden,
                self.extra.clone(),
            );
        }
    }

    pub fn has_comparison_filter(&self) -> bool {
        self.equal_filter.is_some()
    }

    pub fn clear_comparison_filter(&mut self) {
        self.equal_filter = None;
    }

    pub fn set_comparison_filter(&mut self, paths: Option<BTreeSet<PathBuf>>, show_hidden: bool) {
        self.equal_filter = paths;
        let focus = self
            .display_row(self.cursor)
            .map(|entry| entry.path.clone());
        self.rebuild(show_hidden, focus.as_deref());
    }

    fn rebuild(&mut self, show_hidden: bool, current: Option<&Path>) {
        let filter = self.filter.to_lowercase();
        let masks = filter
            .contains(['*', '?', ';'])
            .then(|| files::compile_masks(&filter).ok())
            .flatten();
        let visible: Vec<_> = self
            .entries
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| {
                let matched = if let Some(masks) = &masks {
                    masks.is_match(&entry.name)
                } else {
                    filter.is_empty()
                        || entry
                            .name
                            .to_string_lossy()
                            .to_lowercase()
                            .contains(&filter)
                };
                (matched
                    && (show_hidden || !entry.hidden)
                    && self
                        .equal_filter
                        .as_ref()
                        .is_none_or(|paths| !paths.contains(&entry.path)))
                .then_some(index)
            })
            .collect();
        let mut selected = self.selection.borrow_mut();
        let mut preserved = BTreeSet::new();
        self.visible_size = 0;
        self.selected_size = 0;
        self.visible_folders = 0;
        for &index in &visible {
            let entry = &self.entries[index];
            let size = entry.size.unwrap_or_default();
            self.visible_size = self.visible_size.saturating_add(size);
            self.visible_folders += usize::from(entry.kind == Kind::Directory);
            if selected.contains(&entry.path) {
                preserved.insert(entry.path.clone());
                self.selected_size = self.selected_size.saturating_add(size);
            }
        }
        *selected = preserved;
        let parent = self.location().parent().is_some();
        let offset = usize::from(parent);
        self.cursor = current
            .and_then(|path| {
                visible
                    .iter()
                    .position(|&index| self.entries[index].path == path)
                    .map(|index| index + offset)
            })
            .unwrap_or_else(|| self.cursor.min((visible.len() + offset).saturating_sub(1)));
        self.anchor = self.cursor;
        self.model = Rc::new(FileModel::new(
            self.entries.clone(),
            visible,
            self.selection.clone(),
            parent,
        ));
    }

    pub fn current(&self) -> Option<&Entry> {
        self.row(self.cursor)
    }

    pub fn row(&self, index: usize) -> Option<&Entry> {
        self.is_local().then(|| self.model.entry(index)).flatten()
    }

    pub fn display_row(&self, index: usize) -> Option<&Entry> {
        self.model.entry(index)
    }

    pub fn display_rows(&self) -> impl Iterator<Item = &Entry> {
        self.model.visible.iter().map(|&index| &self.entries[index])
    }

    pub(crate) fn file_model(&self) -> Rc<FileModel> {
        self.model.clone()
    }

    pub fn parent_row(&self, index: usize) -> bool {
        self.model.parent && index == 0
    }

    pub fn drag_paths(&self, row: usize) -> Vec<PathBuf> {
        let Some(entry) = self.row(row) else {
            return Vec::new();
        };
        if self.selection.borrow().contains(&entry.path) {
            self.paths()
        } else {
            vec![entry.path.clone()]
        }
    }

    pub fn context_paths(&mut self, row: usize) -> Vec<PathBuf> {
        let paths = self.drag_paths(row);
        if paths.is_empty() {
            return paths;
        }
        self.focus_context_row(row);
        paths
    }

    pub fn focus_context_row(&mut self, row: usize) {
        if self
            .display_row(row)
            .is_some_and(|entry| self.selection.borrow().contains(&entry.path))
        {
            self.cursor = row;
        } else {
            self.select_row(row, false, false);
        }
    }

    pub fn paths(&self) -> Vec<PathBuf> {
        if !self.is_local() {
            return Vec::new();
        }
        self.selected_paths()
    }

    pub fn selected_paths(&self) -> Vec<PathBuf> {
        if self.selection.borrow().is_empty() {
            return self
                .display_row(self.cursor)
                .map(|entry| entry.path.clone())
                .into_iter()
                .collect();
        }
        self.marked_paths()
    }

    pub fn selected_directories(&self) -> Vec<PathBuf> {
        let marked = self.selection.borrow();
        if marked.is_empty() {
            return self
                .display_row(self.cursor)
                .filter(|entry| entry.kind == Kind::Directory)
                .map(|entry| entry.path.clone())
                .into_iter()
                .collect();
        }
        self.model
            .visible
            .iter()
            .filter_map(|&index| self.entries.get(index))
            .filter(|entry| entry.kind == Kind::Directory && marked.contains(&entry.path))
            .map(|entry| entry.path.clone())
            .collect()
    }

    pub fn marked_paths(&self) -> Vec<PathBuf> {
        let selected = self.selection.borrow();
        self.model
            .visible
            .iter()
            .map(|&index| &self.entries[index].path)
            .filter(|path| selected.contains(*path))
            .cloned()
            .collect()
    }

    pub fn remember_selection(&mut self) {
        self.previous_selection = self.selection.borrow().clone();
    }

    pub fn visible_directories(&self) -> Vec<PathBuf> {
        self.model
            .visible
            .iter()
            .filter_map(|&index| {
                let entry = &self.entries[index];
                (entry.kind == Kind::Directory).then(|| entry.path.clone())
            })
            .collect()
    }

    pub fn set_folder_sizes(&mut self, sizes: &BTreeMap<PathBuf, u64>, show_hidden: bool) {
        let mut entries = self.entries.as_ref().clone();
        for entry in &mut entries {
            if entry.kind == Kind::Directory
                && let Some(size) = sizes.get(&entry.path)
            {
                entry.size = Some(*size);
            }
        }
        self.populate(entries, show_hidden, self.extra.clone());
    }

    pub fn restore_selection(&mut self) {
        let saved = std::mem::take(&mut self.previous_selection);
        let current = std::mem::take(&mut *self.selection.borrow_mut());
        self.previous_selection = current;
        self.replace_selection(|entry| saved.contains(&entry.path));
    }

    pub fn replace_selection(&mut self, mut selected: impl FnMut(&Entry) -> bool) {
        let mut marked = self.selection.borrow_mut();
        marked.clear();
        self.selected_size = 0;
        for &index in &self.model.visible {
            let entry = &self.entries[index];
            if selected(entry) {
                marked.insert(entry.path.clone());
                self.selected_size = self
                    .selected_size
                    .saturating_add(entry.size.unwrap_or_default());
            }
        }
        drop(marked);
        self.model.notify.reset();
    }

    pub fn invert(&mut self) {
        self.invert_where(|_| true);
    }

    pub fn invert_where(&mut self, mut matches: impl FnMut(&Entry) -> bool) {
        let old = self.selection.borrow().clone();
        self.replace_selection(|entry| old.contains(&entry.path) ^ matches(entry));
        self.previous_selection = old;
    }

    pub fn adjust_selection(&mut self, mut matches: impl FnMut(&Entry) -> bool, add: bool) {
        self.remember_selection();
        let mut marked = self.selection.borrow_mut();
        for &index in &self.model.visible {
            let entry = &self.entries[index];
            if matches(entry) {
                let size = entry.size.unwrap_or_default();
                if add && marked.insert(entry.path.clone()) {
                    self.selected_size = self.selected_size.saturating_add(size);
                } else if !add && marked.remove(&entry.path) {
                    self.selected_size = self.selected_size.saturating_sub(size);
                }
            }
        }
        drop(marked);
        self.model.notify.reset();
    }

    pub fn select_row(&mut self, row: usize, toggle: bool, range: bool) {
        if row >= self.model.row_count() {
            return;
        }
        self.cursor = row;
        if range {
            let start = self.anchor.min(row);
            let end = self.anchor.max(row);
            let mut selected = self.selection.borrow_mut();
            for index in start..=end {
                if let Some(entry) = self.model.entry(index)
                    && selected.insert(entry.path.clone())
                {
                    self.selected_size = self
                        .selected_size
                        .saturating_add(entry.size.unwrap_or_default());
                }
            }
            drop(selected);
            self.model.notify.reset();
        } else if toggle {
            self.toggle_current();
            self.anchor = row;
        } else {
            self.replace_selection(|_| false);
            self.anchor = row;
        }
    }

    pub fn cursor_step(&mut self, delta: isize, mark: bool) {
        if mark {
            self.toggle_current();
        }
        self.cursor = self
            .cursor
            .saturating_add_signed(delta)
            .min(self.model.row_count().saturating_sub(1));
    }

    pub fn cursor_end(&mut self) {
        self.cursor = self.model.row_count().saturating_sub(1);
    }

    pub fn toggle_current(&mut self) {
        if let Some(entry) = self.model.entry(self.cursor) {
            let mut marked = self.selection.borrow_mut();
            let size = entry.size.unwrap_or_default();
            if !marked.remove(&entry.path) {
                marked.insert(entry.path.clone());
                self.selected_size = self.selected_size.saturating_add(size);
            } else {
                self.selected_size = self.selected_size.saturating_sub(size);
            }
            drop(marked);
            self.model.notify.row_changed(self.cursor);
        }
    }
}

pub(crate) struct FileModel {
    entries: Rc<Vec<Entry>>,
    visible: Vec<usize>,
    selected: Rc<RefCell<BTreeSet<PathBuf>>>,
    parent: bool,
    notify: ModelNotify,
    wdx: RefCell<BTreeMap<usize, [slint::SharedString; 3]>>,
}

impl FileModel {
    fn new(
        entries: Rc<Vec<Entry>>,
        visible: Vec<usize>,
        selected: Rc<RefCell<BTreeSet<PathBuf>>>,
        parent: bool,
    ) -> Self {
        Self {
            entries,
            visible,
            selected,
            parent,
            notify: ModelNotify::default(),
            wdx: RefCell::new(BTreeMap::new()),
        }
    }

    fn entry(&self, index: usize) -> Option<&Entry> {
        index
            .checked_sub(usize::from(self.parent))
            .and_then(|index| self.visible.get(index))
            .map(|&index| &self.entries[index])
    }

    pub(crate) fn wdx_missing(&self, first: usize, count: usize) -> Vec<(usize, PathBuf)> {
        let cached = self.wdx.borrow();
        (first..first.saturating_add(count).min(self.row_count()))
            .filter(|row| !cached.contains_key(row))
            .filter_map(|row| {
                self.entry(row)
                    .filter(|entry| matches!(entry.kind, Kind::File | Kind::Directory))
                    .map(|entry| (row, entry.path.clone()))
            })
            .collect()
    }

    pub(crate) fn apply_wdx(&self, values: Vec<(usize, [String; 3])>) {
        let rows: Vec<_> = values.iter().map(|(row, _)| *row).collect();
        self.wdx.borrow_mut().extend(
            values
                .into_iter()
                .map(|(row, values)| (row, values.map(Into::into))),
        );
        for row in rows {
            self.notify.row_changed(row);
        }
    }

    pub(crate) fn clear_wdx(&self) {
        let mut cache = self.wdx.borrow_mut();
        if !cache.is_empty() {
            cache.clear();
            drop(cache);
            self.notify.reset();
        }
    }
}

impl Model for FileModel {
    type Data = FileRow;

    fn row_count(&self) -> usize {
        self.visible.len() + usize::from(self.parent)
    }

    fn row_data(&self, row: usize) -> Option<Self::Data> {
        if self.parent && row == 0 {
            return Some(FileRow {
                name: "[..]".into(),
                full_name: "Übergeordneter Ordner".into(),
                info: "<DIR>".into(),
                icon: "parent".into(),
                ..FileRow::default()
            });
        }
        let entry = self.entry(row)?;
        let icon = file_glyph(entry);
        let name = Path::new(&entry.name);
        let extension = if entry.kind == Kind::Directory {
            None
        } else {
            name.extension()
        };
        let stem = extension.and_then(|_| name.file_stem()).map(|stem| {
            name.parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .map(|parent| parent.join(stem).into_os_string())
                .unwrap_or_else(|| stem.to_os_string())
        });
        let [wdx_first, wdx_second, wdx_third] =
            self.wdx.borrow().get(&row).cloned().unwrap_or_default();
        Some(FileRow {
            name: stem
                .as_deref()
                .unwrap_or(&entry.name)
                .to_string_lossy()
                .as_ref()
                .into(),
            full_name: entry.name.to_string_lossy().as_ref().into(),
            extension: extension
                .unwrap_or_default()
                .to_string_lossy()
                .as_ref()
                .into(),
            info: entry
                .size
                .map(human_size)
                .unwrap_or_else(|| {
                    if entry.kind == Kind::Directory {
                        "<DIR>".into()
                    } else {
                        String::new()
                    }
                })
                .into(),
            attributes: if cfg!(windows) {
                [(1, 'r'), (32, 'a'), (4, 's'), (2, 'h')]
                    .into_iter()
                    .map(|(flag, label)| {
                        if entry.attributes & flag != 0 {
                            label
                        } else {
                            '-'
                        }
                    })
                    .collect::<String>()
                    .into()
            } else {
                if entry.attributes & 1 != 0 {
                    "r---".into()
                } else {
                    "----".into()
                }
            },
            time: entry
                .modified
                .map(|time| {
                    DateTime::<Local>::from(time)
                        .format("%d.%m.%y %H:%M")
                        .to_string()
                })
                .unwrap_or_default()
                .into(),
            icon: icon.into(),
            selected: self.selected.borrow().contains(&entry.path),
            wdx_first,
            wdx_second,
            wdx_third,
        })
    }

    fn model_tracker(&self) -> &dyn ModelTracker {
        &self.notify
    }
}

fn file_glyph(entry: &Entry) -> &'static str {
    match entry.kind {
        Kind::Directory => "folder",
        Kind::Link => "link",
        _ => {
            let extension = entry
                .path
                .extension()
                .and_then(|value| value.to_str())
                .unwrap_or_default()
                .to_ascii_lowercase();
            match extension.as_str() {
                "lnk" | "url" => "link",
                "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "svg" | "avif" => "image",
                "zip" | "7z" | "tar" | "gz" | "rar" | "cab" | "arj" | "lz" | "lzh" | "xz"
                | "zst" | "br" => "archive",
                "rs" | "js" | "ts" | "py" | "c" | "cpp" | "h" | "json" | "toml" | "ini" | "xml"
                | "html" => "code",
                "mp3" | "wav" | "ogg" | "flac" | "mp4" | "mkv" | "mov" | "webm" => "media",
                _ => "file",
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ftp_locations_keep_remote_rows_out_of_the_windows_shell() {
        let fixture = tempfile::tempdir().unwrap();
        let local = fixture.path().to_path_buf();
        let session = Arc::new(
            ftp::Connection::new(
                "ftp://localhost/root/Test%20ordner/",
                "arco",
                ftp::Security::Plain,
            )
            .unwrap()
            .authenticate("fixture-only")
            .unwrap(),
        );
        let ftp = FtpLocation {
            directory: session.initial_directory(),
            session,
        };
        let location = Location::Ftp(ftp.clone());
        assert_eq!(location.row_key(), Path::new("Test ordner"));
        assert_eq!(
            location.parent().unwrap().display(),
            "ftp://localhost/root/"
        );
        let mut panel = Panel::from_saved(SavedPanel::default(), &local);
        let tab = panel.current_mut();
        tab.change_location(&location, None);
        tab.ftp = Some(FtpTab {
            location: ftp,
            listing: ftp::Listing {
                entries: Vec::new(),
                format: ftp::ListFormat::Mlsd,
            },
        });
        tab.populate(
            vec![Entry::from_remote(
                PathBuf::from("Probe.txt"),
                Kind::File,
                Some(10),
                None,
            )],
            true,
            String::new(),
        );
        assert!(tab.parent_row(0));
        assert_eq!(tab.selected_paths(), [PathBuf::from("Probe.txt")]);
        assert!(tab.current().is_none());
        assert!(tab.context_paths(1).is_empty());
        assert!(tab.drag_paths(1).is_empty());
        assert!(tab.paths().is_empty());
        assert_eq!(panel.saved().tabs, [local]);
    }

    #[test]
    fn archive_rows_cannot_be_passed_to_local_shell_operations() {
        let fixture = tempfile::tempdir().unwrap();
        let base = fixture.path();
        let source = base.join("Probe.txt");
        let file = base.join("Beispiel.zip");
        std::fs::write(&source, b"test").unwrap();
        let cancel = std::sync::atomic::AtomicBool::new(false);
        arco_core::archives::create_zip(&[source], base, &file, None, &cancel).unwrap();
        let catalog = Arc::new(Catalog::read(&file, &cancel).unwrap());
        let folder = PathBuf::new();
        let rows = catalog.rows(&folder).unwrap();
        let location = Location::Archive {
            file: file.clone(),
            folder: folder.clone(),
        };
        let mut tab = Tab::new(base.to_path_buf());
        tab.change_location(
            &location,
            Some(ArchiveTab {
                file,
                folder,
                catalog,
            }),
        );
        tab.populate(rows, true, String::new());
        tab.select_row(1, false, false);

        assert_eq!(tab.selected_paths(), [PathBuf::from("Probe.txt")]);
        assert!(tab.current().is_none());
        assert!(tab.paths().is_empty());
        assert!(tab.context_paths(1).is_empty());
        assert!(tab.drag_paths(1).is_empty());
        assert_eq!(location.parent(), Some(Location::Local(base.to_path_buf())));
    }

    #[test]
    fn parent_row_stays_visible_and_outside_file_selections() {
        let fixture = tempfile::tempdir().unwrap();
        let first = fixture.path().join("Erster.TXT");
        let second = fixture.path().join("Zweiter.bin");
        std::fs::write(&first, b"erste Datei").unwrap();
        std::fs::write(&second, b"andere Datei").unwrap();
        let mut tab = Tab::new(fixture.path().to_path_buf());
        tab.populate(
            files::list(fixture.path()).unwrap().entries,
            true,
            String::new(),
        );

        assert!(tab.parent_row(0));
        assert_eq!(tab.model.row_count(), 3);
        assert_eq!(tab.current().unwrap().path, first);
        tab.select_row(0, false, false);
        tab.toggle_current();
        assert!(tab.paths().is_empty());
        assert!(tab.drag_paths(0).is_empty());

        tab.select_row(2, false, true);
        assert_eq!(tab.paths(), [first.clone(), second]);
        assert_eq!(tab.selected_size, 23);
        tab.filter("*.txt".into(), true);
        tab.set_sort(SortKey::Extension, true);
        assert!(tab.parent_row(0));
        assert_eq!(tab.model.row_count(), 2);
        assert_eq!(tab.paths(), [first]);
        assert_eq!(tab.model.row_data(1).unwrap().extension.as_str(), "TXT");
    }

    #[test]
    fn root_listing_has_no_synthetic_parent() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().ancestors().last().unwrap().to_path_buf();
        let mut tab = Tab::new(root);
        tab.populate(Vec::new(), false, String::new());
        assert!(!tab.parent_row(0));
        assert_eq!(tab.model.row_count(), 0);
    }

    #[test]
    fn repeated_parent_navigation_advances_past_the_pending_folder() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().ancestors().last().unwrap();
        let child = root.join("a").join("b");
        let mut panel = Panel::from_saved(SavedPanel::default(), &child);
        let parent = Location::Local(root.join("a"));
        assert_eq!(panel.next_parent(), Some(parent.clone()));
        panel.pending_parent = Some(parent);
        let target = panel.next_parent().unwrap();
        assert_eq!(target, Location::Local(root.to_path_buf()));
        assert_eq!(panel.current().path, child);
        panel.pending_parent = Some(target);
        assert!(panel.next_parent().is_none());
    }

    #[test]
    fn locked_tab_persists_its_base_and_title_even_after_visiting_a_subfolder() {
        let fixture = tempfile::tempdir().unwrap();
        let base = fixture.path().to_owned();
        let mut panel = Panel::from_saved(SavedPanel::default(), &base);
        let tab = panel.current_mut();
        tab.options.title = "Projekt".into();
        tab.set_lock(TabLock::ReturnToBase);
        tab.change_location(&Location::Local(base.join("subfolder")), None);

        let saved = panel.saved();
        assert_eq!(saved.tabs.as_slice(), std::slice::from_ref(&base));
        let mut profile = arco_core::profile::Profile::default();
        profile
            .tab_groups
            .insert("Arbeit".into(), [saved, SavedPanel::default()]);
        let file = base.join("profile.json");
        profile.save(&file).unwrap();
        let mut profile = arco_core::profile::Profile::load(&file).unwrap();
        let [saved, _] = profile.tab_groups.remove("Arbeit").unwrap();
        let restored = Panel::from_saved(saved, &base);
        assert_eq!(restored.current().options.title, "Projekt");
        assert_eq!(restored.current().lock_base, Some(Location::Local(base)));
        assert_eq!(restored.current().options.lock, TabLock::ReturnToBase);
    }

    #[test]
    fn wdx_values_belong_to_one_file_model() {
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path().join("file.txt");
        std::fs::write(&path, b"file").unwrap();
        let mut tab = Tab::new(fixture.path().to_owned());
        tab.populate(
            files::list(fixture.path()).unwrap().entries,
            false,
            String::new(),
        );
        let model = tab.file_model();
        assert_eq!(model.wdx_missing(0, 20), [(1, path.clone())]);
        model.apply_wdx(vec![(1, ["4 bytes".into(), String::new(), String::new()])]);
        assert_eq!(model.row_data(1).unwrap().wdx_first.as_str(), "4 bytes");
        assert!(model.wdx_missing(0, 20).is_empty());
        tab.populate(
            files::list(fixture.path()).unwrap().entries,
            false,
            String::new(),
        );
        assert!(!Rc::ptr_eq(&model, &tab.file_model()));
        assert_eq!(tab.file_model().wdx_missing(0, 20), [(1, path)]);
    }

    #[test]
    fn context_menu_keeps_the_marked_group_only_when_opened_on_it() {
        let fixture = tempfile::tempdir().unwrap();
        let paths = ["A.txt", "B.txt", "C.txt"].map(|name| fixture.path().join(name));
        for path in &paths {
            std::fs::write(path, []).unwrap();
        }
        let mut tab = Tab::new(fixture.path().to_path_buf());
        tab.populate(
            files::list(fixture.path()).unwrap().entries,
            true,
            String::new(),
        );
        tab.select_row(1, true, false);
        tab.select_row(2, true, false);

        assert!(tab.context_paths(0).is_empty());
        assert_eq!(tab.context_paths(1), paths[..2]);
        assert_eq!(tab.paths(), paths[..2]);
        assert_eq!(tab.context_paths(3), paths[2..]);
        assert_eq!(tab.paths(), paths[2..]);
    }

    #[test]
    fn file_only_inversion_preserves_marked_folders_and_can_be_restored() {
        let fixture = tempfile::tempdir().unwrap();
        let folder = fixture.path().join("A");
        let file = fixture.path().join("B.txt");
        std::fs::create_dir(&folder).unwrap();
        std::fs::write(&file, b"content").unwrap();
        let mut tab = Tab::new(fixture.path().to_path_buf());
        tab.populate(
            files::list(fixture.path()).unwrap().entries,
            true,
            String::new(),
        );
        tab.adjust_selection(|entry| entry.kind == Kind::Directory, true);
        tab.invert_where(|entry| entry.kind != Kind::Directory);
        assert_eq!(tab.paths(), [folder.clone(), file]);
        assert_eq!(tab.selected_size, 7);
        tab.restore_selection();
        assert_eq!(tab.paths(), [folder]);
        assert_eq!(tab.selected_size, 0);
    }
}
