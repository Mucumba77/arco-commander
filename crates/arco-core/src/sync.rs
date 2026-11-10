use crate::files::{self, TransferReport};
use crate::tools::{self, Hash};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, Metadata};
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, SystemTime};
use walkdir::WalkDir;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub enum Mode {
    #[default]
    BothWays,
    LeftToRight,
    Mirror,
    RightToLeft,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Options {
    pub mode: Mode,
    pub recursive: bool,
    pub empty_dirs: bool,
    pub selected_only: bool,
    pub by_content: bool,
    pub ignore_date: bool,
    pub masks: String,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            mode: Mode::BothWays,
            recursive: true,
            empty_dirs: true,
            selected_only: false,
            by_content: false,
            ignore_date: false,
            masks: String::new(),
        }
    }
}

impl Options {
    pub fn validate(&self) -> io::Result<()> {
        FileMasks::new(&self.masks).map(|_| ())
    }
}

pub struct FileMasks {
    include: globset::GlobSet,
    exclude: globset::GlobSet,
}

impl FileMasks {
    pub fn new(value: &str) -> io::Result<Self> {
        let (include, exclude) = value.split_once('|').unwrap_or((value, ""));
        if exclude.contains('|') {
            return Err(io::Error::other(
                "Trenne Einschluss und Ausschluss mit einem einzelnen |.",
            ));
        }
        Ok(Self {
            include: files::compile_masks(include)?,
            exclude: files::compile_masks(exclude)?,
        })
    }

    pub fn from_tc(value: &str) -> io::Result<Self> {
        let mut normalized = String::with_capacity(value.len());
        let mut quoted = false;
        for character in value.chars() {
            match character {
                '"' => quoted = !quoted,
                ' ' | '\t' if !quoted => normalized.push(';'),
                character => normalized.push(character),
            }
        }
        if quoted {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Die Dateimaske enthält ein offenes Anführungszeichen.",
            ));
        }
        Self::new(&normalized)
    }

    pub fn includes(&self, path: &Path) -> bool {
        path.file_name().is_some_and(|name| {
            (self.include.is_empty() || self.include.is_match(name)) && !self.exclude.is_match(name)
        })
    }

    fn is_empty(&self) -> bool {
        self.include.is_empty() && self.exclude.is_empty()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Action {
    Skip,
    CopyLeft,
    CopyRight,
    DeleteRight,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Comparison {
    Equal,
    Different,
    OnlyLeft,
    OnlyRight,
    TypeConflict,
    Protected,
}

#[derive(Clone, Copy)]
pub struct EntryInfo {
    pub size: Option<u64>,
    pub modified: Option<SystemTime>,
}

impl EntryInfo {
    pub fn is_dir(self) -> bool {
        self.size.is_none()
    }
}

pub struct Entry {
    path: PathBuf,
    info: EntryInfo,
    meta: Option<Metadata>,
}

impl Entry {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn metadata(&self) -> Option<&Metadata> {
        self.meta.as_ref()
    }
}

#[derive(Default)]
pub struct Tree(BTreeMap<PathBuf, Entry>);

impl Tree {
    pub fn discard_subtrees(&mut self, paths: &BTreeSet<PathBuf>) {
        let paths: BTreeSet<_> = paths.iter().map(|path| key(path)).collect();
        self.0
            .retain(|path, _| !path.ancestors().any(|parent| paths.contains(parent)));
    }
    pub fn insert_remote(&mut self, path: PathBuf, info: EntryInfo) -> io::Result<()> {
        if !relative_path(&path) {
            return Err(io::Error::other("Ungültiger Pfad im entfernten Ordner."));
        }
        for part in path.iter() {
            files::leaf_name(part)?;
        }
        if let Some(parent) = path.parent().filter(|path| !path.as_os_str().is_empty())
            && !self
                .0
                .get(&key(parent))
                .is_some_and(|entry| entry.info.is_dir())
        {
            return Err(io::Error::other(
                "Der Elternordner fehlt im entfernten Dateibaum.",
            ));
        }
        self.insert(Entry {
            path,
            info,
            meta: None,
        })
    }

    fn insert(&mut self, entry: Entry) -> io::Result<()> {
        match self.0.entry(key(&entry.path)) {
            std::collections::btree_map::Entry::Vacant(slot) => {
                slot.insert(entry);
            }
            std::collections::btree_map::Entry::Occupied(_) => {
                return Err(io::Error::other(format!(
                    "Kollidierende Dateinamen: {}",
                    entry.path.display()
                )));
            }
        }
        Ok(())
    }
}

pub struct Row {
    pub relative: PathBuf,
    pub comparison: Comparison,
    pub suggested: Action,
    pub action: Action,
    entries: [Option<Metadata>; 2],
    info: [Option<EntryInfo>; 2],
    paths: [PathBuf; 2],
    allowed: [bool; 3],
}

impl Row {
    pub fn prevent_delete(&mut self) {
        self.allowed[2] = false;
        if self.action == Action::DeleteRight {
            self.action = Action::Skip;
            self.suggested = Action::Skip;
        }
    }
    pub fn can(&self, action: Action) -> bool {
        match action {
            Action::Skip => true,
            Action::CopyLeft => self.allowed[0],
            Action::CopyRight => self.allowed[1],
            Action::DeleteRight => self.allowed[2],
        }
    }

    pub fn metadata(&self, side: usize) -> Option<&Metadata> {
        self.entries[side].as_ref()
    }

    pub fn info(&self, side: usize) -> Option<EntryInfo> {
        self.info[side]
    }

    pub fn path(&self, side: usize) -> &Path {
        &self.paths[side]
    }

    pub fn toggle(&mut self, action: Action) {
        if self.can(action) {
            self.action = if self.action == action {
                Action::Skip
            } else {
                action
            };
        }
    }
}

pub struct Plan {
    roots: [PathBuf; 2],
    root_metadata: [Metadata; 2],
    pub rows: Vec<Row>,
}

pub struct LocalRoot {
    path: PathBuf,
    metadata: Metadata,
}

impl LocalRoot {
    pub fn open(path: &Path) -> io::Result<Self> {
        let path = fs::canonicalize(path)?;
        let metadata = directory_metadata(&path)?;
        Ok(Self { path, metadata })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn checked_path(&self, relative: &Path) -> io::Result<PathBuf> {
        if !relative_path(relative) {
            return Err(io::Error::other("Ungültiger relativer Zielpfad."));
        }
        check_parents(&self.path, &self.metadata, relative)?;
        Ok(self.path.join(relative))
    }

    pub fn verify_absent(&self, relative: &Path) -> io::Result<()> {
        let target = match self.checked_path(relative) {
            Ok(target) => target,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return check_parents(&self.path, &self.metadata, Path::new(""));
            }
            Err(error) => return Err(error),
        };
        match fs::symlink_metadata(target) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
            Ok(_) => Err(io::Error::other(
                "Der Eintrag ist inzwischen lokal vorhanden.",
            )),
        }
    }

    pub fn scan(&self, options: &Options, scope: &Scope, cancel: &AtomicBool) -> io::Result<Tree> {
        self.scan_ignoring(options, scope, |_, _| false, cancel)
    }

    pub fn scan_ignoring(
        &self,
        options: &Options,
        scope: &Scope,
        ignored: impl Fn(&Path, bool) -> bool,
        cancel: &AtomicBool,
    ) -> io::Result<Tree> {
        scan(&self.path, options.recursive, scope, ignored, cancel)
    }
}

#[cfg(windows)]
fn key(path: &Path) -> PathBuf {
    PathBuf::from(path.to_string_lossy().to_lowercase())
}

#[cfg(not(windows))]
fn key(path: &Path) -> PathBuf {
    path.to_path_buf()
}

pub fn compare(
    roots: [&Path; 2],
    options: &Options,
    selected: &[PathBuf],
    cancel: &AtomicBool,
) -> io::Result<Plan> {
    compare_ignoring(roots, options, selected, None, cancel)
}

pub fn compare_ignoring(
    roots: [&Path; 2],
    options: &Options,
    selected: &[PathBuf],
    ignore: Option<&crate::ignore::IgnoreList>,
    cancel: &AtomicBool,
) -> io::Result<Plan> {
    let roots = [fs::canonicalize(roots[0])?, fs::canonicalize(roots[1])?];
    let [left_key, right_key] = roots.each_ref().map(|root| key(root));
    if left_key.starts_with(&right_key) || right_key.starts_with(&left_key) {
        return Err(io::Error::other("Die beiden Ordner müssen getrennt sein."));
    }
    let root_metadata = [
        directory_metadata(&roots[0])?,
        directory_metadata(&roots[1])?,
    ];
    let scope = Scope::new(options.selected_only, selected)?;
    // Exclude a matched relative path on both sides. Otherwise a rule for an
    // absolute path could make an existing file look like a mirror deletion.
    let read = |side: usize| {
        scan(
            &roots[side],
            options.recursive,
            &scope,
            |path, directory| {
                ignore.is_some_and(|ignore| {
                    let current = roots[side].join(path);
                    let other = roots[1 - side].join(path);
                    ignore.local(&current, directory)
                        || ignore.local(&other, false)
                        || (ignore.local(&other, true) && other.is_dir())
                })
            },
            cancel,
        )
    };
    let [left, right] = [read(0)?, read(1)?];
    let rows = compare_trees([&left, &right], options, &scope, cancel, |a, b| {
        let left = roots[0].join(&a.path);
        let right = roots[1].join(&b.path);
        let equal = tools::checksum(&left, Hash::Blake3, cancel)?
            == tools::checksum(&right, Hash::Blake3, cancel)?;
        files::ensure_unchanged(&left, a.meta.as_ref().expect("local metadata"))?;
        files::ensure_unchanged(&right, b.meta.as_ref().expect("local metadata"))?;
        Ok(equal)
    })?;
    Ok(Plan {
        roots,
        root_metadata,
        rows,
    })
}

pub fn compare_trees(
    trees: [&Tree; 2],
    options: &Options,
    scope: &Scope,
    cancel: &AtomicBool,
    mut same_content: impl FnMut(&Entry, &Entry) -> io::Result<bool>,
) -> io::Result<Vec<Row>> {
    let masks = FileMasks::new(&options.masks)?;
    let maps = trees.map(|tree| &tree.0);
    let mut relevant = BTreeSet::new();
    for (id, entry) in maps.iter().flat_map(|map| map.iter()) {
        files::check_cancel(cancel)?;
        if !scope.includes(id) {
            continue;
        }
        let wanted = if entry.info.is_dir() {
            options.recursive && options.empty_dirs
        } else {
            masks.includes(id)
        };
        if wanted {
            relevant.extend(
                id.ancestors()
                    .take_while(|part| !part.as_os_str().is_empty())
                    .map(Path::to_path_buf),
            );
        }
    }
    let mut rows = Vec::with_capacity(relevant.len());
    for id in relevant {
        files::check_cancel(cancel)?;
        let [left, right] = maps.each_ref().map(|map| map.get(&id));
        let entry = left
            .or(right)
            .expect("A relevant entry or its ancestor was scanned");
        let relative = entry.path.clone();
        let paths = maps.each_ref().map(|map| resolved_path(&relative, map));
        let blocked = id.ancestors().skip(1).any(|parent| {
            maps.iter()
                .any(|map| map.get(parent).is_some_and(|entry| !entry.info.is_dir()))
        });
        let (comparison, suggested) = match (left, right) {
            _ if blocked => (Comparison::TypeConflict, Action::Skip),
            (Some(a), Some(b)) if a.info.is_dir() != b.info.is_dir() => {
                (Comparison::TypeConflict, Action::Skip)
            }
            (Some(a), Some(_)) if a.info.is_dir() => (Comparison::Equal, Action::Skip),
            (Some(a), Some(b)) => compare_file(a, b, options, &mut same_content)?,
            (Some(_), None) => (
                Comparison::OnlyLeft,
                if options.mode == Mode::RightToLeft {
                    Action::Skip
                } else {
                    Action::CopyRight
                },
            ),
            (None, Some(b)) => {
                let suggested = match options.mode {
                    Mode::BothWays | Mode::RightToLeft => Action::CopyLeft,
                    Mode::Mirror
                        if !b.info.is_dir()
                            || (options.empty_dirs && masks.is_empty() && scope.includes(&id)) =>
                    {
                        Action::DeleteRight
                    }
                    _ => Action::Skip,
                };
                (Comparison::OnlyRight, suggested)
            }
            (None, None) => unreachable!("A relevant path must be present on one side"),
        };
        let copy = !matches!(comparison, Comparison::Equal | Comparison::TypeConflict);
        rows.push(Row {
            relative,
            comparison,
            suggested,
            action: suggested,
            entries: [
                left.and_then(|e| e.meta.clone()),
                right.and_then(|e| e.meta.clone()),
            ],
            info: [left.map(|e| e.info), right.map(|e| e.info)],
            paths,
            allowed: [
                copy && right.is_some()
                    && matches!(options.mode, Mode::BothWays | Mode::RightToLeft),
                copy && left.is_some() && options.mode != Mode::RightToLeft,
                suggested == Action::DeleteRight,
            ],
        });
    }
    Ok(rows)
}

fn relative_path(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && path
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
}

pub struct Scope(Option<BTreeSet<PathBuf>>);

impl Scope {
    pub fn new(enabled: bool, selected: &[PathBuf]) -> io::Result<Self> {
        if !enabled {
            return Ok(Self(None));
        }
        if selected.is_empty() {
            return Err(io::Error::other(
                "Markiere Dateien oder Ordner in mindestens einem Dateifenster.",
            ));
        }
        if selected.iter().any(|path| !relative_path(path)) {
            return Err(io::Error::other("Ungültiger relativer Auswahlpfad."));
        }
        Ok(Self(Some(selected.iter().map(|path| key(path)).collect())))
    }

    fn includes(&self, path: &Path) -> bool {
        self.0
            .as_ref()
            .is_none_or(|paths| path.ancestors().any(|part| paths.contains(part)))
    }

    pub fn visits(&self, path: &Path) -> bool {
        let Some(selected) = &self.0 else {
            return true;
        };
        let relative = key(path);
        self.includes(&relative)
            || selected
                .range(relative.clone()..)
                .next()
                .is_some_and(|path| path.starts_with(&relative))
    }
}

fn scan(
    root: &Path,
    recursive: bool,
    scope: &Scope,
    ignored: impl Fn(&Path, bool) -> bool,
    cancel: &AtomicBool,
) -> io::Result<Tree> {
    let mut entries = Tree::default();
    let walker = WalkDir::new(root)
        .min_depth(1)
        .max_depth(if recursive { usize::MAX } else { 1 })
        .follow_links(false);
    for item in walker.into_iter().filter_entry(|entry| {
        if entry.depth() == 0 {
            return true;
        }
        let path = entry
            .path()
            .strip_prefix(root)
            .expect("WalkDir path under root");
        scope.visits(path) && !ignored(path, entry.file_type().is_dir())
    }) {
        files::check_cancel(cancel)?;
        let item = item?;
        let path = item
            .path()
            .strip_prefix(root)
            .map_err(io::Error::other)?
            .to_path_buf();
        let meta = files::regular_metadata(item.path())?;
        let info = EntryInfo {
            size: meta.is_file().then_some(meta.len()),
            modified: meta.modified().ok(),
        };
        entries.insert(Entry {
            path,
            info,
            meta: Some(meta),
        })?;
    }
    Ok(entries)
}

fn resolved_path(path: &Path, map: &BTreeMap<PathBuf, Entry>) -> PathBuf {
    for part in path
        .ancestors()
        .take_while(|part| !part.as_os_str().is_empty())
    {
        if let Some(existing) = map.get(&key(part)) {
            let rest = path.strip_prefix(part).expect("part is an ancestor");
            return if rest.as_os_str().is_empty() {
                existing.path.clone()
            } else {
                existing.path.join(rest)
            };
        }
    }
    path.to_path_buf()
}

fn compare_file(
    a: &Entry,
    b: &Entry,
    options: &Options,
    same_content: &mut impl FnMut(&Entry, &Entry) -> io::Result<bool>,
) -> io::Result<(Comparison, Action)> {
    let modified = if options.ignore_date {
        Ordering::Equal
    } else {
        let (left, right) = a.info.modified.zip(b.info.modified).ok_or_else(|| {
            io::Error::other(
                "Der Dateivergleich benötigt einen Änderungszeitstempel auf beiden Seiten.",
            )
        })?;
        compare_time(left, right)
    };
    let same_size = a.info.size == b.info.size;
    let equal = if options.by_content && same_size && modified.is_eq() {
        same_content(a, b)?
    } else {
        same_size && !options.by_content && modified.is_eq()
    };
    if equal {
        return Ok((Comparison::Equal, Action::Skip));
    }
    let action = if options.mode == Mode::RightToLeft {
        Action::CopyLeft
    } else if options.mode != Mode::BothWays || modified.is_gt() {
        Action::CopyRight
    } else if modified.is_lt() {
        Action::CopyLeft
    } else {
        Action::Skip
    };
    Ok((Comparison::Different, action))
}

fn compare_time(left: SystemTime, right: SystemTime) -> Ordering {
    let difference = left
        .duration_since(right)
        .unwrap_or_else(|error| error.duration());
    if difference <= Duration::from_secs(2) {
        Ordering::Equal
    } else {
        left.cmp(&right)
    }
}

#[derive(Clone, Copy)]
pub enum MarkRule {
    Newer,
    Unique,
    Size,
}

#[derive(Default)]
pub struct PanelMarks {
    pub marked: [BTreeSet<PathBuf>; 2],
    pub equal: [BTreeSet<PathBuf>; 2],
}

pub fn compare_panels(
    roots: [&Path; 2],
    include_folders: bool,
    show_hidden: bool,
    rule: MarkRule,
    cancel: &AtomicBool,
) -> io::Result<PanelMarks> {
    let mut maps = [BTreeMap::new(), BTreeMap::new()];
    for (map, root) in maps.iter_mut().zip(roots) {
        let directory = files::list(root)?;
        if let Some(error) = directory.errors.first() {
            return Err(io::Error::other(error.clone()));
        }
        for entry in directory.entries {
            files::check_cancel(cancel)?;
            if !show_hidden && entry.hidden {
                continue;
            }
            if entry.kind == files::Kind::File
                || (include_folders && entry.kind == files::Kind::Directory)
            {
                let name = key(Path::new(&entry.name));
                if map.insert(name, entry).is_some() {
                    return Err(io::Error::other(
                        "Dateinamen unterscheiden sich nur in Groß-/Kleinschreibung.",
                    ));
                }
            }
        }
    }
    let [left, right] = &maps;
    let mut result = PanelMarks::default();
    for (name, a) in left {
        files::check_cancel(cancel)?;
        let Some(b) = right.get(name) else {
            result.marked[0].insert(a.path.clone());
            continue;
        };
        if a.kind != b.kind {
            result.marked[0].insert(a.path.clone());
            result.marked[1].insert(b.path.clone());
        } else if a.kind == files::Kind::File {
            let modified = a
                .modified
                .zip(b.modified)
                .map_or(Ordering::Equal, |(a, b)| compare_time(a, b));
            let same_size = a.size == b.size;
            if same_size && modified.is_eq() {
                result.equal[0].insert(a.path.clone());
                result.equal[1].insert(b.path.clone());
            }
            match rule {
                MarkRule::Newer if modified.is_gt() => {
                    result.marked[0].insert(a.path.clone());
                }
                MarkRule::Newer if modified.is_lt() => {
                    result.marked[1].insert(b.path.clone());
                }
                MarkRule::Size if !same_size => {
                    result.marked[0].insert(a.path.clone());
                    result.marked[1].insert(b.path.clone());
                }
                _ => (),
            }
        }
    }
    for (name, b) in right {
        files::check_cancel(cancel)?;
        if !left.contains_key(name) {
            result.marked[1].insert(b.path.clone());
        }
    }
    Ok(result)
}

fn directory_metadata(path: &Path) -> io::Result<Metadata> {
    let meta = files::regular_metadata(path)?;
    if !meta.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotADirectory,
            path.display().to_string(),
        ));
    }
    Ok(meta)
}

fn check_parents(root: &Path, expected: &Metadata, relative: &Path) -> io::Result<()> {
    let meta = directory_metadata(root)?;
    if !files::same_identity_hint(&meta, expected) {
        return Err(io::Error::other(
            "Der Stammordner hat sich seit dem Vergleich geändert.",
        ));
    }
    let mut dir = root.to_path_buf();
    if let Some(parent) = relative.parent() {
        for component in parent.components() {
            dir.push(component);
            directory_metadata(&dir)?;
        }
    }
    Ok(())
}

impl Plan {
    pub fn roots(&self) -> &[PathBuf; 2] {
        &self.roots
    }

    pub fn disable_actions(&mut self) {
        for row in &mut self.rows {
            row.suggested = Action::Skip;
            row.action = Action::Skip;
            row.allowed = [false; 3];
        }
    }

    pub fn protect_path(&mut self, side: usize, file: &Path) -> io::Result<()> {
        let file = key(&fs::canonicalize(file)?);
        let root = key(&self.roots[side]);
        if let Ok(relative) = file.strip_prefix(root) {
            for row in &mut self.rows {
                if key(&row.paths[side]) == relative {
                    row.action = Action::Skip;
                    row.suggested = Action::Skip;
                    row.comparison = Comparison::Protected;
                    row.allowed = [false; 3];
                }
            }
        }
        Ok(())
    }

    pub fn toggle(&mut self, index: usize, action: Action) {
        if let Some(row) = self.rows.get_mut(index) {
            row.toggle(action);
        }
    }

    pub fn apply(&self, cancel: &AtomicBool, mut progress: impl FnMut(&Path)) -> TransferReport {
        let mut report = TransferReport::default();
        let copies = self
            .rows
            .iter()
            .filter(|row| matches!(row.action, Action::CopyLeft | Action::CopyRight));
        for row in copies {
            if !apply_row(&mut report, row, cancel, &mut progress, || {
                self.copy(row, cancel)
            }) {
                return report;
            }
        }
        let deletes = self
            .rows
            .iter()
            .rev()
            .filter(|row| row.action == Action::DeleteRight);
        if !report.errors.is_empty() {
            report.skipped += deletes.count();
            return report;
        }
        for row in deletes {
            if !apply_row(&mut report, row, cancel, &mut progress, || self.delete(row)) {
                break;
            }
        }
        report
    }

    fn check_parents(&self, side: usize, relative: &Path) -> io::Result<()> {
        check_parents(&self.roots[side], &self.root_metadata[side], relative)
    }

    fn copy(&self, row: &Row, cancel: &AtomicBool) -> io::Result<()> {
        let from = usize::from(row.action == Action::CopyLeft);
        let to = 1 - from;
        self.check_parents(from, &row.paths[from])?;
        self.check_parents(to, &row.paths[to])?;
        let source = self.roots[from].join(&row.paths[from]);
        let target = self.roots[to].join(&row.paths[to]);
        let before = row.entries[from].as_ref().expect("copy source");
        files::ensure_unchanged(&source, before)?;
        if before.is_dir() {
            return fs::create_dir(target);
        }
        files::copy_file_checked(
            &source,
            &target,
            before,
            row.entries[to].as_ref(),
            cancel,
            &mut |_| {},
        )?;
        Ok(())
    }

    fn delete(&self, row: &Row) -> io::Result<()> {
        match self.check_parents(0, &row.paths[0]) {
            Ok(()) => match fs::symlink_metadata(self.roots[0].join(&row.paths[0])) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => (),
                Err(error) => return Err(error),
                Ok(_) => {
                    return Err(io::Error::other(
                        "Der Eintrag ist inzwischen links vorhanden.",
                    ));
                }
            },
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                // A missing left subtree is valid only while its root remains the same.
                self.check_parents(0, Path::new(""))?;
            }
            Err(error) => return Err(error),
        }
        self.check_parents(1, &row.paths[1])?;
        let target = self.roots[1].join(&row.paths[1]);
        let before = row.entries[1].as_ref().expect("delete target");
        if before.is_dir() {
            let meta = directory_metadata(&target)?;
            if !files::same_identity_hint(&meta, before) {
                return Err(io::Error::other(
                    "Der Zielordner wurde seit dem Vergleich ersetzt.",
                ));
            }
            // Never remove children that were not individually approved and checked.
            fs::remove_dir(target)
        } else {
            files::ensure_unchanged(&target, before)?;
            fs::remove_file(target)
        }
    }
}

fn apply_row(
    report: &mut TransferReport,
    row: &Row,
    cancel: &AtomicBool,
    progress: &mut impl FnMut(&Path),
    apply: impl FnOnce() -> io::Result<()>,
) -> bool {
    let result = files::check_cancel(cancel).and_then(|()| {
        progress(&row.relative);
        apply()
    });
    match result {
        Ok(()) => report.completed += 1,
        Err(error) if error.kind() == io::ErrorKind::Interrupted => report.cancelled = true,
        Err(error) => report
            .errors
            .push(format!("{}: {error}", row.relative.display())),
    }
    !report.cancelled
}

#[cfg(test)]
mod tests {
    use super::*;
    use filetime::{FileTime, set_file_mtime};

    fn setup() -> (tempfile::TempDir, [PathBuf; 2], AtomicBool) {
        let temp = tempfile::tempdir().unwrap();
        let roots = [temp.path().join("left"), temp.path().join("right")];
        for root in &roots {
            fs::create_dir(root).unwrap();
        }
        (temp, roots, AtomicBool::new(false))
    }

    fn file(root: &Path, name: &str, bytes: &[u8], modified: i64) -> PathBuf {
        let path = root.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, bytes).unwrap();
        set_file_mtime(&path, FileTime::from_unix_time(modified, 0)).unwrap();
        path
    }

    fn plan(
        roots: &[PathBuf; 2],
        options: &Options,
        selected: &[PathBuf],
        cancel: &AtomicBool,
    ) -> Plan {
        compare(
            roots.each_ref().map(PathBuf::as_path),
            options,
            selected,
            cancel,
        )
        .unwrap()
    }

    #[test]
    fn mirror_excludes_ignored_paths_on_both_sides() {
        let (_fixture, roots, cancel) = setup();
        let left = file(&roots[0], "private/data.txt", b"private", 1000);
        let right = file(&roots[1], "private/data.txt", b"copy", 1000);
        file(&roots[0], "public.txt", b"public", 1000);
        let ignored_folder = fs::canonicalize(left.parent().unwrap()).unwrap();
        let ignore =
            crate::ignore::IgnoreList::parse(&format!("{}\\", ignored_folder.display())).unwrap();
        let plan = compare_ignoring(
            roots.each_ref().map(PathBuf::as_path),
            &Options {
                recursive: true,
                mode: Mode::Mirror,
                ..Options::default()
            },
            &[],
            Some(&ignore),
            &cancel,
        )
        .unwrap();
        assert_eq!(plan.rows.len(), 1);
        assert_eq!(plan.rows[0].relative, Path::new("public.txt"));
        assert_eq!(fs::read(right).unwrap(), b"copy");
    }

    #[test]
    fn selected_masked_comparison_can_choose_the_direction_for_equal_dates() {
        let (_temp, roots, cancel) = setup();
        let [left, right] = &roots;
        let old = 1_700_000_000;
        let source = file(left, "docs/edit.txt", b"left!", old);
        let target = file(right, "docs/edit.txt", b"right", old);
        file(left, "docs/new.txt", b"new", old);
        file(left, "docs/private.txt", b"private", old);
        file(left, "docs/skip.bin", b"excluded", old);
        file(left, "other/not-selected.txt", b"other", old);
        fs::create_dir(left.join("docs").join("empty")).unwrap();
        let options = Options {
            selected_only: true,
            by_content: true,
            ignore_date: true,
            masks: "*.txt | private*".into(),
            ..Options::default()
        };
        let mut plan = plan(&roots, &options, &["docs".into()], &cancel);
        let conflict = plan
            .rows
            .iter()
            .position(|row| row.relative == Path::new("docs/edit.txt"))
            .unwrap();
        assert_eq!(plan.rows[conflict].comparison, Comparison::Different);
        assert_eq!(plan.rows[conflict].action, Action::Skip);
        plan.toggle(conflict, Action::CopyLeft);
        let report = plan.apply(&cancel, |_| {});
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(report.completed, 3);
        assert_eq!(fs::read(source).unwrap(), fs::read(target).unwrap());
        assert!(right.join("docs").join("empty").is_dir());
        assert!(right.join("docs").join("new.txt").is_file());
        assert!(!right.join("docs").join("private.txt").exists());
        assert!(!right.join("docs").join("skip.bin").exists());
        assert!(!right.join("other").exists());
    }

    #[test]
    fn mirror_does_not_delete_if_a_copy_failed_or_if_left_has_a_new_file() {
        let (_temp, roots, cancel) = setup();
        let [left, right] = &roots;
        let source = file(left, "update.txt", b"new", 1_700_000_020);
        let target = file(right, "update.txt", b"old", 1_700_000_000);
        let extra = file(right, "extra.txt", b"backup", 1_700_000_000);
        let options = Options {
            mode: Mode::Mirror,
            ..Options::default()
        };
        let preview = plan(&roots, &options, &[], &cancel);
        fs::write(&source, b"changed after preview").unwrap();
        let report = preview.apply(&cancel, |_| {});
        assert_eq!(report.completed, 0);
        assert_eq!(report.errors.len(), 1);
        assert_eq!(report.skipped, 1);
        assert_eq!(fs::read(&target).unwrap(), b"old");
        assert!(extra.is_file());

        let preview = plan(&roots, &options, &[], &cancel);
        file(left, "extra.txt", b"now exists", 1_700_000_040);
        let report = preview.apply(&cancel, |_| {});
        assert_eq!(report.errors.len(), 1);
        assert!(extra.is_file());
        assert_eq!(fs::read(&target).unwrap(), b"changed after preview");
    }

    #[test]
    fn mirror_removes_only_reviewed_entries_and_never_recursively_deletes_a_folder() {
        let (_temp, roots, cancel) = setup();
        let [_, right] = &roots;
        let old = 1_700_000_000;
        let first = file(right, "extra/old.txt", b"remove", old);
        let nested = file(right, "other/sub/nested.txt", b"remove", old);
        let options = Options {
            mode: Mode::Mirror,
            ..Options::default()
        };
        let preview = plan(&roots, &options, &[], &cancel);
        let new = file(right, "extra/new.txt", b"keep", old);
        let report = preview.apply(&cancel, |_| {});
        assert_eq!(report.errors.len(), 1);
        assert!(!first.exists());
        assert!(!nested.exists());
        assert!(!right.join("other").exists());
        assert_eq!(fs::read(new).unwrap(), b"keep");
    }

    #[test]
    fn file_folder_conflicts_block_descendants_and_nonrecursive_compare_skips_them() {
        let (_temp, roots, cancel) = setup();
        let [left, right] = &roots;
        file(left, "clash/file.txt", b"left", 1_700_000_000);
        let target = file(right, "clash", b"right", 1_700_000_000);
        let options = Options {
            mode: Mode::Mirror,
            ..Options::default()
        };
        let preview = plan(&roots, &options, &[], &cancel);
        assert_eq!(preview.rows.len(), 2);
        assert!(preview.rows.iter().all(|row| row.comparison == Comparison::TypeConflict && row.action == Action::Skip));
        let report = preview.apply(&cancel, |_| {});
        assert_eq!(report.completed, 0);
        assert_eq!(fs::read(target).unwrap(), b"right");
        let options = Options {
            recursive: false,
            ..options
        };
        let preview = plan(&roots, &options, &[], &cancel);
        assert_eq!(preview.rows.len(), 1);
    }

    #[test]
    fn panel_comparison_marks_newer_unique_and_different_size_by_rule() {
        let (_temp, roots, cancel) = setup();
        let [left, right] = &roots;
        let old = 1_700_000_000;
        let same_left = file(left, "same.txt", b"abc", old);
        let same_right = file(right, "same.txt", b"xyz", old + 1);
        let newer_left = file(left, "newer.txt", b"new", old + 10);
        file(right, "newer.txt", b"old", old);
        let size_left = file(left, "size.txt", b"small", old);
        let size_right = file(right, "size.txt", b"larger", old);
        let single = file(right, "single.txt", b"single", old);
        let dir = left.join("unique");
        fs::create_dir(&dir).unwrap();
        let paths = roots.each_ref().map(PathBuf::as_path);
        let newer = compare_panels(paths, false, true, MarkRule::Newer, &cancel).unwrap();
        assert_eq!(
            newer.marked,
            [
                BTreeSet::from([newer_left]),
                BTreeSet::from([single.clone()])
            ]
        );
        assert_eq!(
            newer.equal,
            [BTreeSet::from([same_left]), BTreeSet::from([same_right])]
        );
        let size = compare_panels(paths, false, true, MarkRule::Size, &cancel).unwrap();
        assert_eq!(
            size.marked,
            [
                BTreeSet::from([size_left]),
                BTreeSet::from([size_right, single.clone()])
            ]
        );
        let unique = compare_panels(paths, true, true, MarkRule::Unique, &cancel).unwrap();
        assert_eq!(
            unique.marked,
            [BTreeSet::from([dir]), BTreeSet::from([single])]
        );
    }
}
