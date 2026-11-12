use filetime::{FileTime, set_file_handle_times, set_file_times};
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use std::time::SystemTime;
use tempfile::NamedTempFile;
use walkdir::WalkDir;

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Kind {
    Directory,
    File,
    Link,
    Other,
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub path: PathBuf,
    pub name: OsString,
    pub kind: Kind,
    pub size: Option<u64>,
    pub modified: Option<SystemTime>,
    pub hidden: bool,
    pub attributes: u32,
    pub comment: Option<std::sync::Arc<str>>,
    pub list_order: usize,
    sort_extension: String,
    sort_name: String,
}

impl Entry {
    pub fn with_display_name(mut self, name: OsString) -> Self {
        self.sort_name = name.to_string_lossy().to_lowercase();
        self.name = name;
        self
    }

    pub(crate) fn from_archive(
        path: PathBuf,
        directory: bool,
        size: u64,
        encrypted: bool,
        modified: Option<SystemTime>,
    ) -> Self {
        let mut entry = Self::from_remote(
            path,
            if directory {
                Kind::Directory
            } else {
                Kind::File
            },
            (!directory).then_some(size),
            modified,
        );
        entry.attributes = if encrypted { 0x4000 } else { 0 };
        entry
    }

    pub fn from_remote(
        path: PathBuf,
        kind: Kind,
        size: Option<u64>,
        modified: Option<SystemTime>,
    ) -> Self {
        let name = path.file_name().unwrap_or_default().to_os_string();
        Self {
            sort_name: name.to_string_lossy().to_lowercase(),
            sort_extension: if kind == Kind::Directory {
                String::new()
            } else {
                path.extension()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_lowercase()
            },
            path,
            name,
            kind,
            size,
            modified,
            hidden: false,
            attributes: 0,
            comment: None,
            list_order: 0,
        }
    }
    pub fn read(path: PathBuf, name: OsString) -> io::Result<Self> {
        let meta = fs::symlink_metadata(&path)?;
        Ok(Self::from_metadata(path, name, &meta))
    }

    fn from_metadata(path: PathBuf, name: OsString, meta: &Metadata) -> Self {
        let kind = if is_link(meta) {
            Kind::Link
        } else if meta.is_dir() {
            Kind::Directory
        } else if meta.is_file() {
            Kind::File
        } else {
            Kind::Other
        };
        let sort_name = name.to_string_lossy().to_lowercase();
        let sort_extension = (kind != Kind::Directory)
            .then(|| path.extension())
            .flatten()
            .map(|extension| extension.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        #[cfg(windows)]
        let attributes = {
            use std::os::windows::fs::MetadataExt;
            meta.file_attributes()
        };
        #[cfg(not(windows))]
        let attributes = u32::from(meta.permissions().readonly());
        Self {
            path,
            hidden: is_hidden(meta, &name),
            name,
            kind,
            size: (kind == Kind::File).then_some(meta.len()),
            modified: meta.modified().ok(),
            attributes,
            comment: None,
            list_order: 0,
            sort_extension,
            sort_name,
        }
    }
}

pub struct Directory {
    pub entries: Vec<Entry>,
    pub errors: Vec<String>,
}

pub fn list(path: &Path) -> io::Result<Directory> {
    list_directory(path, None)
}

pub fn list_cancellable(path: &Path, cancel: &AtomicBool) -> io::Result<Directory> {
    check_cancel(cancel)?;
    list_directory(path, Some(cancel))
}

fn list_directory(path: &Path, cancel: Option<&AtomicBool>) -> io::Result<Directory> {
    let mut directory = Directory {
        entries: Vec::new(),
        errors: Vec::new(),
    };
    for item in fs::read_dir(path)? {
        if let Some(cancel) = cancel {
            check_cancel(cancel)?;
        }
        let item = item.and_then(|entry| {
            let meta = entry.metadata()?;
            Ok(Entry::from_metadata(entry.path(), entry.file_name(), &meta))
        });
        match item {
            Ok(entry) => directory.entries.push(entry),
            Err(error) => directory.errors.push(error.to_string()),
        }
    }
    Ok(directory)
}

#[derive(Default)]
pub struct Usage {
    pub bytes: u64,
    pub files: usize,
    pub directories: usize,
    pub links: usize,
    pub errors: Vec<String>,
}

pub fn measure(paths: &[PathBuf], cancel: &AtomicBool) -> io::Result<Usage> {
    measure_with(paths, cancel, |_| {})
}

pub fn measure_with(
    paths: &[PathBuf],
    cancel: &AtomicBool,
    visit_file: impl FnMut(&Path),
) -> io::Result<Usage> {
    measure_ignoring(paths, cancel, None, visit_file)
}

pub fn measure_ignoring(
    paths: &[PathBuf],
    cancel: &AtomicBool,
    ignore: Option<&crate::ignore::IgnoreList>,
    mut visit_file: impl FnMut(&Path),
) -> io::Result<Usage> {
    let selected: BTreeSet<_> = paths.iter().map(PathBuf::as_path).collect();
    let mut result = Usage::default();
    for root in &selected {
        check_cancel(cancel)?;
        if root
            .ancestors()
            .skip(1)
            .any(|parent| selected.contains(parent))
        {
            continue;
        }
        let mut walk = WalkDir::new(root)
            .follow_links(false)
            .follow_root_links(false)
            .into_iter();
        while let Some(item) = walk.next() {
            check_cancel(cancel)?;
            let entry = match item {
                Ok(entry) => entry,
                Err(error) => {
                    result.errors.push(error.to_string());
                    continue;
                }
            };
            if ignore.is_some_and(|ignore| ignore.local(entry.path(), entry.file_type().is_dir())) {
                if entry.file_type().is_dir() {
                    walk.skip_current_dir();
                }
                continue;
            }
            match fs::symlink_metadata(entry.path()) {
                Ok(meta) if is_link(&meta) => {
                    if entry.file_type().is_dir() {
                        walk.skip_current_dir();
                    }
                    result.links += 1;
                }
                Ok(meta) if meta.is_dir() => result.directories += 1,
                Ok(meta) if meta.is_file() => {
                    result.bytes = result.bytes.checked_add(meta.len()).ok_or_else(|| {
                        io::Error::other("Dateigrößen überschreiten den Zählbereich.")
                    })?;
                    result.files += 1;
                    visit_file(entry.path());
                }
                Ok(_) => result.errors.push(format!(
                    "Nicht unterstützter Eintrag: {}",
                    entry.path().display()
                )),
                Err(error) => {
                    if entry.file_type().is_dir() {
                        walk.skip_current_dir();
                    }
                    result
                        .errors
                        .push(format!("{}: {error}", entry.path().display()));
                }
            }
        }
    }
    Ok(result)
}

pub fn branch(path: &Path, cancel: &AtomicBool) -> io::Result<Directory> {
    let mut directory = Directory {
        entries: Vec::new(),
        errors: Vec::new(),
    };
    read_branch(path, path, false, &mut directory, cancel)?;
    Ok(directory)
}

pub fn branch_selected(
    base: &Path,
    paths: Vec<PathBuf>,
    cancel: &AtomicBool,
) -> io::Result<Directory> {
    let mut directory = Directory {
        entries: Vec::new(),
        errors: Vec::new(),
    };
    let selected: BTreeSet<_> = paths.into_iter().collect();
    for path in &selected {
        check_cancel(cancel)?;
        let relative = path.strip_prefix(base).map_err(io::Error::other)?;
        if relative.as_os_str().is_empty()
            || relative
                .components()
                .any(|part| !matches!(part, Component::Normal(_)))
        {
            return Err(io::Error::other(
                "Auswahl liegt außerhalb des Ausgangsordners.",
            ));
        }
        if !path
            .ancestors()
            .skip(1)
            .any(|parent| selected.contains(parent))
        {
            for ancestor in path.ancestors().take_while(|ancestor| *ancestor != base) {
                regular_metadata(ancestor)?;
            }
            read_branch(base, path, true, &mut directory, cancel)?;
        }
    }
    Ok(directory)
}

fn read_branch(
    base: &Path,
    path: &Path,
    files_only: bool,
    directory: &mut Directory,
    cancel: &AtomicBool,
) -> io::Result<()> {
    let walker = WalkDir::new(path)
        .min_depth(usize::from(!files_only))
        .follow_links(false)
        .follow_root_links(!files_only);
    for item in walker.into_iter().filter_entry(|entry| {
        entry.depth() == 0 || entry.metadata().is_ok_and(|meta| !is_link(&meta))
    }) {
        check_cancel(cancel)?;
        match item.map_err(io::Error::other).and_then(|entry| {
            let name = entry
                .path()
                .strip_prefix(base)
                .map_err(io::Error::other)?
                .as_os_str();
            Entry::read(entry.path().to_path_buf(), name.to_os_string())
        }) {
            Ok(entry) if !files_only || entry.kind == Kind::File => directory.entries.push(entry),
            Ok(_) => (),
            Err(error) => directory.errors.push(error.to_string()),
        }
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SortKey {
    #[default]
    Name,
    Extension,
    Size,
    Modified,
    Attributes,
    Comment,
    Wdx(u8),
    Unsorted,
}

impl SortKey {
    pub fn column(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Extension => "extension",
            Self::Size => "size",
            Self::Modified => "time",
            Self::Attributes => "attributes",
            Self::Comment => "comment",
            Self::Wdx(_) => "",
            Self::Unsorted => "",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SortOrder {
    pub key: SortKey,
    pub descending: bool,
}

pub fn sort(entries: &mut [Entry], key: SortKey, descending: bool) {
    sort_by_columns(entries, &[SortOrder { key, descending }]);
}

pub fn sort_by_columns(entries: &mut [Entry], columns: &[SortOrder]) {
    sort_by_columns_with(entries, columns, |_, _, _, _| Ordering::Equal);
}

pub fn sort_by_columns_with(
    entries: &mut [Entry],
    columns: &[SortOrder],
    wdx: impl Fn(u8, &Entry, &Entry, bool) -> Ordering,
) {
    if let Some(order) = columns
        .first()
        .filter(|order| order.key == SortKey::Unsorted)
    {
        entries.sort_by(|left, right| {
            let order_by_index = left.list_order.cmp(&right.list_order);
            if order.descending {
                order_by_index.reverse()
            } else {
                order_by_index
            }
        });
        return;
    }
    entries.sort_by(|a, b| {
        let directory_order = (b.kind == Kind::Directory).cmp(&(a.kind == Kind::Directory));
        directory_order.then_with(|| {
            let name = || natural_cmp(&a.sort_name, &b.sort_name).then_with(|| a.name.cmp(&b.name));
            for column in columns {
                let order = match column.key {
                    SortKey::Name => name(),
                    SortKey::Extension => a.sort_extension.cmp(&b.sort_extension),
                    SortKey::Size => a.size.cmp(&b.size),
                    SortKey::Modified => a.modified.cmp(&b.modified),
                    SortKey::Attributes => a.attributes.cmp(&b.attributes),
                    SortKey::Comment => a.comment.cmp(&b.comment),
                    SortKey::Wdx(index) => {
                        let order = wdx(index, a, b, column.descending);
                        if !order.is_eq() {
                            return order;
                        }
                        continue;
                    }
                    SortKey::Unsorted => Ordering::Equal,
                };
                if !order.is_eq() {
                    return if column.descending {
                        order.reverse()
                    } else {
                        order
                    };
                }
            }
            let order = name();
            if columns
                .last()
                .is_some_and(|column| column.descending && !matches!(column.key, SortKey::Wdx(_)))
            {
                order.reverse()
            } else {
                order
            }
        })
    });
}

fn natural_cmp(a: &str, b: &str) -> Ordering {
    let mut a = a.as_bytes();
    let mut b = b.as_bytes();
    while let (Some(&left), Some(&right)) = (a.first(), b.first()) {
        if left.is_ascii_digit() && right.is_ascii_digit() {
            let a_end = a.iter().take_while(|c| c.is_ascii_digit()).count();
            let b_end = b.iter().take_while(|c| c.is_ascii_digit()).count();
            let a_num = trim_leading_zeroes(&a[..a_end]);
            let b_num = trim_leading_zeroes(&b[..b_end]);
            let order = a_num.len().cmp(&b_num.len()).then_with(|| a_num.cmp(b_num));
            if order != Ordering::Equal {
                return order;
            }
            a = &a[a_end..];
            b = &b[b_end..];
        } else {
            let order = left.cmp(&right);
            if order != Ordering::Equal {
                return order;
            }
            a = &a[1..];
            b = &b[1..];
        }
    }
    a.len().cmp(&b.len())
}

fn trim_leading_zeroes(value: &[u8]) -> &[u8] {
    &value[value.iter().take_while(|&&c| c == b'0').count()..]
}

pub fn compile_masks(patterns: &str) -> io::Result<GlobSet> {
    let mut set = GlobSetBuilder::new();
    for pattern in patterns.split(';').map(str::trim).filter(|s| !s.is_empty()) {
        let pattern = if pattern == "*.*" { "*" } else { pattern };
        let glob = GlobBuilder::new(pattern)
            .case_insensitive(true)
            .build()
            .map_err(io::Error::other)?;
        set.add(glob);
    }
    set.build().map_err(io::Error::other)
}

pub fn leaf_name(name: &OsStr) -> io::Result<&OsStr> {
    let mut components = Path::new(name).components();
    if let (Some(Component::Normal(part)), None) = (components.next(), components.next())
        && part == name
        && valid_windows_name(name)
    {
        return Ok(part);
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        "expected a single file name",
    ))
}

fn valid_windows_name(name: &OsStr) -> bool {
    let text = name.to_string_lossy();
    let stem = text
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end()
        .to_ascii_uppercase();
    let device_number = |prefix| {
        stem.strip_prefix(prefix).is_some_and(|suffix| {
            matches!(
                suffix,
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            )
        })
    };
    !text.ends_with(['.', ' '])
        && !text
            .chars()
            .any(|c| c.is_control() || "<>:\"/\\|?*".contains(c))
        && !matches!(
            stem.as_str(),
            "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
        )
        && !device_number("COM")
        && !device_number("LPT")
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Conflict {
    Fail,
    Skip,
    Replace,
    Newer,
    Larger,
    Rename,
    Append,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Transfer {
    Copy,
    Move,
}

#[derive(Default, Debug)]
pub struct TransferReport {
    pub completed: usize,
    pub skipped: usize,
    pub ignored: usize,
    pub errors: Vec<String>,
    pub cancelled: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ProgressPhase {
    #[default]
    Data,
    Finalizing,
}

pub struct Progress<'a> {
    pub path: &'a Path,
    pub copied: u64,
    pub total: u64,
    pub items: Option<(usize, usize)>,
    pub current: Option<(u64, u64)>,
    pub phase: ProgressPhase,
}

pub type NativeRename = fn(&Path, &Path) -> io::Result<()>;

pub struct FileConflict<'a> {
    pub source: &'a Path,
    pub target: &'a Path,
    pub source_metadata: &'a Metadata,
    pub target_metadata: &'a Metadata,
}

pub enum ConflictDecision {
    Apply(Conflict),
    RenameTo(OsString),
}

impl From<Conflict> for ConflictDecision {
    fn from(rule: Conflict) -> Self {
        Self::Apply(rule)
    }
}

pub type ResolveConflict<'a> = dyn Fn(FileConflict<'_>) -> io::Result<ConflictDecision> + 'a;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct NtfsCopyOptions {
    pub streams: bool,
    pub permissions: bool,
    pub owner: bool,
    pub audit: bool,
    pub hardlinks: bool,
    pub sparse: bool,
    pub creation_time: bool,
    pub copy_links: bool,
}

#[derive(Clone, Copy)]
pub struct TransferPolicy<'a> {
    pub conflict: Conflict,
    pub resolve_conflict: Option<&'a ResolveConflict<'a>>,
    pub ntfs: NtfsCopyOptions,
    pub descriptions: Option<crate::descriptions::ApplyAttributes>,
    pub ignore: Option<&'a crate::ignore::IgnoreList>,
    pub filter: Option<&'a crate::advanced_search::Query>,
    pub before_entry: Option<&'a dyn Fn(&Path) -> io::Result<()>>,
}

impl From<Conflict> for TransferPolicy<'_> {
    fn from(conflict: Conflict) -> Self {
        Self {
            conflict,
            resolve_conflict: None,
            ntfs: NtfsCopyOptions::default(),
            descriptions: None,
            ignore: None,
            filter: None,
            before_entry: None,
        }
    }
}

pub fn transfer<'a>(
    sources: &[PathBuf],
    destination: &Path,
    operation: Transfer,
    policy: impl Into<TransferPolicy<'a>>,
    cancel: &AtomicBool,
    rename: Option<NativeRename>,
    progress: impl FnMut(Progress<'_>),
) -> io::Result<TransferReport> {
    let destination = transfer_directory(destination)?;
    let items = sources.iter().map(|source| {
        let target = source
            .file_name()
            .map(|name| destination.join(name))
            .ok_or_else(|| io::Error::other("cannot transfer a root"));
        (source.as_path(), target)
    });
    Ok(transfer_items(
        items,
        operation,
        policy.into(),
        cancel,
        rename,
        progress,
    ))
}

pub fn transfer_masked(
    sources: &[PathBuf],
    destination: &Path,
    mask: &str,
    operation: Transfer,
    policy: TransferPolicy<'_>,
    cancel: &AtomicBool,
    rename: Option<NativeRename>,
    progress: impl FnMut(Progress<'_>),
) -> io::Result<TransferReport> {
    let destination = transfer_directory(destination)?;
    let mut source_paths = BTreeSet::new();
    let mut names = BTreeSet::new();
    let mut plan = Vec::with_capacity(sources.len());
    for source in sources {
        check_cancel(cancel)?;
        regular_metadata(source)?;
        let canonical = fs::canonicalize(source)?;
        if same_or_inside(&destination, &canonical) {
            return Err(io::Error::other(format!(
                "Zielordner liegt in der Auswahl: {}",
                source.display()
            )));
        }
        source_paths.insert(canonical);
        let name = source.file_name().and_then(OsStr::to_str).ok_or_else(|| {
            io::Error::other("Für die Zielmaske ist ein Unicode-Dateiname erforderlich.")
        })?;
        let name = masked_name(name, mask)?;
        let key = if cfg!(windows) {
            name.to_lowercase()
        } else {
            name.clone()
        };
        if !names.insert(key) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("Die Zielmaske erzeugt den Namen mehrfach: {name}"),
            ));
        }
        plan.push((source.as_path(), destination.join(name)));
    }
    let protected: BTreeSet<_> = source_paths
        .iter()
        .flat_map(|path| path.ancestors())
        .collect();
    for (_, target) in &plan {
        check_cancel(cancel)?;
        match fs::canonicalize(target) {
            Ok(path) if protected.contains(path.as_path()) => {
                return Err(io::Error::other(format!(
                    "Das Ziel überschneidet sich mit einer Quelle: {}",
                    target.display()
                )));
            }
            Ok(_) => (),
            Err(error) if error.kind() == io::ErrorKind::NotFound => (),
            Err(error) => return Err(error),
        }
    }
    Ok(transfer_items(
        plan.into_iter()
            .map(|(source, target)| (source, Ok(target))),
        operation,
        policy,
        cancel,
        rename,
        progress,
    ))
}

fn transfer_directory(destination: &Path) -> io::Result<PathBuf> {
    let destination = fs::canonicalize(destination)?;
    if !destination.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotADirectory,
            "target is not a directory",
        ));
    }
    Ok(destination)
}

fn masked_name(name: &str, mask: &str) -> io::Result<String> {
    fn apply(source: &str, mask: &str) -> String {
        let mut remaining = source.chars();
        let mut output = String::new();
        for character in mask.chars() {
            match character {
                '*' => output.extend(&mut remaining),
                '?' => output.extend(remaining.next()),
                literal => {
                    remaining.next();
                    output.push(literal);
                }
            }
        }
        output
    }
    let result = if let Some((stem, extension)) = mask.split_once('.') {
        let name = Path::new(name);
        let mut result = apply(name.file_stem().and_then(OsStr::to_str).unwrap_or(""), stem);
        let extension = apply(
            name.extension().and_then(OsStr::to_str).unwrap_or(""),
            extension,
        );
        if !extension.is_empty() {
            result.push('.');
            result.push_str(&extension);
        }
        result
    } else {
        apply(name, mask)
    };
    leaf_name(OsStr::new(&result))?;
    Ok(result)
}

fn transfer_items<'a>(
    items: impl IntoIterator<Item = (&'a Path, io::Result<PathBuf>)>,
    operation: Transfer,
    policy: TransferPolicy<'_>,
    cancel: &AtomicBool,
    rename: Option<NativeRename>,
    mut progress: impl FnMut(Progress<'_>),
) -> TransferReport {
    let mut report = TransferReport::default();
    let mut native = crate::native_copy::State::new(policy.ntfs);
    for (source, target) in items {
        if cancel.load(AtomicOrdering::Relaxed) {
            report.cancelled = true;
            break;
        }
        let outcome = target.and_then(|target| {
            transfer_item(
                source,
                &target,
                operation,
                &policy,
                &mut native,
                cancel,
                rename,
                &mut progress,
            )
        });
        match outcome {
            Ok((completed, ignored)) => {
                report.ignored += ignored;
                if completed {
                    report.completed += 1;
                } else {
                    report.skipped += 1;
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                report.cancelled = true;
                break;
            }
            Err(error) => report.errors.push(format!("{}: {error}", source.display())),
        }
    }
    report
}

pub fn copy_to_directories(
    sources: &[PathBuf],
    destinations: &[PathBuf],
    policy: TransferPolicy<'_>,
    cancel: &AtomicBool,
    mut progress: impl FnMut(&Path, Progress<'_>),
) -> io::Result<TransferReport> {
    let mut source_paths = BTreeSet::new();
    let mut names = BTreeSet::new();
    for source in sources {
        check_cancel(cancel)?;
        regular_metadata(source)?;
        source_paths.insert(fs::canonicalize(source)?);
        let name = source
            .file_name()
            .ok_or_else(|| io::Error::other("Ein Laufwerksstamm kann nicht kopiert werden."))?;
        let key = if cfg!(windows) {
            name.to_string_lossy().to_lowercase().into()
        } else {
            name.to_os_string()
        };
        if !names.insert(key) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!(
                    "Mehrere Quellen hätten denselben Zielnamen: {}",
                    name.to_string_lossy(),
                ),
            ));
        }
    }
    let mut targets = BTreeMap::new();
    for destination in destinations {
        check_cancel(cancel)?;
        let target = fs::canonicalize(destination)?;
        let metadata = regular_metadata(&target)?;
        if !metadata.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::NotADirectory,
                format!("Das Ziel ist kein Ordner: {}", destination.display(),),
            ));
        }
        if target
            .ancestors()
            .any(|ancestor| source_paths.contains(ancestor))
        {
            return Err(io::Error::other(format!(
                "Zielordner liegt in der Auswahl: {}",
                destination.display()
            )));
        }
        targets.insert(target, metadata);
    }
    let protected: BTreeSet<_> = source_paths
        .iter()
        .chain(targets.keys())
        .flat_map(|path| path.ancestors())
        .collect();
    for target in targets.keys() {
        for source in sources {
            check_cancel(cancel)?;
            let output = target.join(source.file_name().expect("validated source name"));
            match fs::canonicalize(&output) {
                Ok(path) if protected.contains(path.as_path()) => {
                    return Err(io::Error::other(format!(
                        "Das Kopierziel überschneidet sich mit einer Quelle oder einem Zielordner: {}",
                        output.display(),
                    )));
                }
                Ok(_) => (),
                Err(error) if error.kind() == io::ErrorKind::NotFound => (),
                Err(error) => return Err(error),
            }
        }
    }
    let mut report = TransferReport::default();
    for (target, original) in targets {
        if cancel.load(AtomicOrdering::Relaxed) {
            report.cancelled = true;
            break;
        }
        let outcome = regular_metadata(&target).and_then(|current| {
            if !current.is_dir() || !same_identity_hint(&current, &original) {
                return Err(io::Error::other("Der Zielordner hat sich geändert."));
            }
            transfer(
                sources,
                &target,
                Transfer::Copy,
                policy,
                cancel,
                None,
                |event| progress(&target, event),
            )
        });
        match outcome {
            Ok(batch) => {
                report.completed += batch.completed;
                report.skipped += batch.skipped;
                report.ignored += batch.ignored;
                report.errors.extend(
                    batch
                        .errors
                        .into_iter()
                        .map(|error| format!("{}: {error}", target.display())),
                );
                if batch.cancelled {
                    report.cancelled = true;
                    break;
                }
            }
            Err(error) => report.errors.push(format!("{}: {error}", target.display())),
        }
    }
    Ok(report)
}

fn transfer_item(
    source: &Path,
    target: &Path,
    operation: Transfer,
    policy: &TransferPolicy<'_>,
    native: &mut crate::native_copy::State,
    cancel: &AtomicBool,
    rename: Option<NativeRename>,
    progress: &mut impl FnMut(Progress<'_>),
) -> io::Result<(bool, usize)> {
    check_cancel(cancel)?;
    if let Some(before_entry) = policy.before_entry {
        before_entry(source)?;
    }
    let destination = target.parent().expect("a transfer target has a directory");
    let meta = fs::symlink_metadata(source)?;
    #[cfg(windows)]
    if operation == Transfer::Copy && policy.ntfs.copy_links && is_link(&meta) {
        return copy_junction(source, target, &meta, policy, cancel, progress);
    }
    let meta = checked_regular_metadata(source, meta)?;
    if policy
        .ignore
        .is_some_and(|ignore| ignore.local(source, meta.is_dir()))
    {
        return Ok((false, 1));
    }
    if meta.is_file()
        && let Some(filter) = policy.filter
        && !filter.matches(
            &Entry::from_metadata(
                source.to_path_buf(),
                source.file_name().unwrap_or_default().to_os_string(),
                &meta,
            ),
            cancel,
        )?
    {
        return Ok((false, 0));
    }
    let source_real = fs::canonicalize(source)?;
    if same_or_inside(destination, &source_real) || equivalent_path(&source_real, &target) {
        return Err(io::Error::other("source and target overlap"));
    }
    if operation == Transfer::Move
        && (meta.is_file() || (policy.filter.is_none() && policy.ignore.is_none()))
        && let Some(rename) = rename
    {
        match fs::symlink_metadata(&target) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                check_cancel(cancel)?;
                ensure_unchanged(source, &meta)?;
                match rename(source, &target) {
                    Ok(()) => {
                        if let Some(prepare) = policy.descriptions
                            && let Some(comment) =
                                crate::descriptions::copy_entry(source, target, prepare)?
                        {
                            crate::descriptions::remove_moved(source, &comment, prepare)?;
                        }
                        return Ok((true, 0));
                    }
                    Err(error) if error.kind() == io::ErrorKind::CrossesDevices => (),
                    Err(error) => return Err(error),
                }
            }
            Err(error) => return Err(error),
            Ok(_) => (),
        }
    }
    if meta.is_file() {
        let Some(published) =
            copy_file_with_state(source, target, *policy, native, cancel, progress)?
        else {
            return Ok((false, 0));
        };
        let comment = if let Some(prepare) = policy.descriptions
            && !published.appended
        {
            crate::descriptions::copy_entry(source, &published.path, prepare)?
        } else {
            None
        };
        if operation == Transfer::Move {
            check_cancel(cancel)?;
            ensure_unchanged(source, &meta)?;
            fs::remove_file(source)?;
            if let Some(prepare) = policy.descriptions
                && let Some(comment) = comment
            {
                crate::descriptions::remove_moved(source, &comment, prepare)?;
            }
        }
        return Ok((true, 0));
    }

    let mut plan = Vec::new();
    let mut matching = BTreeSet::new();
    let mut keep = BTreeSet::new();
    let mut sidecars = BTreeMap::new();
    let mut ignored = 0;
    for item in WalkDir::new(source)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| {
            if entry.depth() > 0
                && policy
                    .ignore
                    .is_some_and(|ignore| ignore.local(entry.path(), entry.file_type().is_dir()))
            {
                ignored += 1;
                if operation == Transfer::Move {
                    keep.extend(
                        entry
                            .path()
                            .ancestors()
                            .skip(1)
                            .take(entry.depth())
                            .map(Path::to_path_buf),
                    );
                }
                false
            } else {
                true
            }
        })
    {
        check_cancel(cancel)?;
        let item = item?;
        let from = item.into_path();
        let meta = regular_metadata(&from)?;
        if policy.descriptions.is_some() && meta.is_file() && crate::descriptions::is_sidecar(&from)
        {
            sidecars.insert(from.parent().expect("sidecar parent").to_path_buf(), from);
            continue;
        }
        if let Some(filter) = policy.filter
            && meta.is_file()
        {
            let name = from.file_name().unwrap_or_default().to_os_string();
            if filter.matches(&Entry::from_metadata(from.clone(), name, &meta), cancel)? {
                matching.extend(from.ancestors().map(Path::to_path_buf));
            }
        }
        let relative = from.strip_prefix(source).map_err(io::Error::other)?;
        let to = target.join(relative);
        plan.push((from, to, meta));
    }
    if policy.filter.is_some() {
        plan.retain(|(path, _, _)| {
            let selected = matching.contains(path);
            if !selected && operation == Transfer::Move {
                keep.extend(
                    path.ancestors()
                        .skip(1)
                        .take_while(|parent| parent.starts_with(source))
                        .map(Path::to_path_buf),
                );
            }
            selected
        });
        if plan.is_empty() {
            return Ok((false, ignored));
        }
    }

    #[cfg(windows)]
    native.check_audit_privilege()?;
    let mut skipped = false;
    let mut skipped_files = BTreeSet::new();
    let mut moved_comments = BTreeMap::new();
    #[cfg(windows)]
    let mut new_directories = BTreeMap::new();
    for (from, to, meta) in &plan {
        check_cancel(cancel)?;
        if meta.is_dir() {
            let _created = create_transfer_directory(to)?;
            if let Some(prepare) = policy.descriptions
                && let Some(comment) = crate::descriptions::copy_entry(from, to, prepare)?
                && operation == Transfer::Move
            {
                moved_comments.insert(from, comment);
            }
            #[cfg(windows)]
            if _created && let Some(identity) = native.directory_identity(to)? {
                new_directories.insert(to, identity);
            }
        } else {
            ensure_unchanged(from, meta)?;
            if let Some(published) =
                copy_file_with_state(from, to, *policy, native, cancel, progress)?
            {
                if let Some(prepare) = policy.descriptions
                    && !published.appended
                    && let Some(comment) =
                        crate::descriptions::copy_entry(from, &published.path, prepare)?
                    && operation == Transfer::Move
                {
                    moved_comments.insert(from, comment);
                }
            } else {
                skipped = true;
                if operation == Transfer::Move && policy.resolve_conflict.is_some() {
                    skipped_files.insert(from);
                    keep.extend(
                        from.ancestors()
                            .skip(1)
                            .take_while(|parent| parent.starts_with(source))
                            .map(Path::to_path_buf),
                    );
                }
            }
        }
    }
    for (_from, to, meta) in plan.iter().rev().filter(|(_, _, meta)| meta.is_dir()) {
        #[cfg(windows)]
        if let Some(identity) = new_directories.get(to) {
            ensure_unchanged(_from, meta)?;
            native.prepare_directory(_from, to, *identity, meta, cancel)?;
            continue;
        }
        set_file_times(
            to,
            FileTime::from_last_access_time(meta),
            FileTime::from_last_modification_time(meta),
        )?;
    }
    if operation == Transfer::Move && (!skipped || policy.resolve_conflict.is_some()) {
        check_cancel(cancel)?;
        for (from, _, meta) in &plan {
            ensure_unchanged(from, meta)?;
        }
        for (from, _, meta) in plan.iter().rev() {
            check_cancel(cancel)?;
            if meta.is_dir() {
                if !keep.contains(from) {
                    if let Some(sidecar) = sidecars.get(from)
                        && sidecar.try_exists()?
                    {
                        // Preserve records for entries that were not transferred and opaque metadata.
                        keep.extend(
                            from.ancestors()
                                .take_while(|parent| parent.starts_with(source))
                                .map(Path::to_path_buf),
                        );
                        skipped = true;
                        continue;
                    }
                    fs::remove_dir(from)?;
                } else {
                    continue;
                }
            } else if !skipped_files.contains(from) {
                ensure_unchanged(from, meta)?;
                fs::remove_file(from)?;
            } else {
                continue;
            }
            if let Some(prepare) = policy.descriptions
                && let Some(comment) = moved_comments.remove(from)
            {
                crate::descriptions::remove_moved(from, &comment, prepare)?;
            }
        }
    }
    Ok((
        !skipped && (operation == Transfer::Copy || ignored == 0),
        ignored,
    ))
}

#[cfg(windows)]
fn copy_junction(
    source: &Path,
    target: &Path,
    metadata: &Metadata,
    policy: &TransferPolicy<'_>,
    cancel: &AtomicBool,
    progress: &mut impl FnMut(Progress<'_>),
) -> io::Result<(bool, usize)> {
    if policy
        .ignore
        .is_some_and(|ignore| ignore.local(source, true))
    {
        return Ok((false, 1));
    }
    if policy.filter.is_some() {
        // A directory filter matches contents. Junction copies never inspect them.
        return Ok((false, 0));
    }
    let options = policy.ntfs;
    if options.streams
        || options.permissions
        || options.owner
        || options.audit
        || options.creation_time
    {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Die gewählten NTFS-Metadatenoptionen sind für Verzeichnisjunctions nicht verfügbar.",
        ));
    }
    let junction = crate::native_copy::junction::Junction::open(source)?;
    let target = match fs::symlink_metadata(target) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => target.to_path_buf(),
        Err(error) => return Err(error),
        Ok(existing) => {
            let decision = if let Some(resolve) = policy.resolve_conflict {
                resolve(FileConflict {
                    source,
                    target,
                    source_metadata: metadata,
                    target_metadata: &existing,
                })?
            } else {
                policy.conflict.into()
            };
            check_cancel(cancel)?;
            match decision {
                ConflictDecision::Apply(Conflict::Skip) => return Ok((false, 0)),
                ConflictDecision::Apply(Conflict::Rename) => unused_copy_name(target, cancel)?,
                ConflictDecision::RenameTo(name) => target.with_file_name(leaf_name(&name)?),
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        format!(
                            "Eine Verzeichnisjunction benötigt einen freien Zielnamen: {}",
                            target.display()
                        ),
                    ));
                }
            }
        }
    };
    progress(Progress {
        path: source,
        copied: 0,
        total: 0,
        items: None,
        current: None,
        phase: ProgressPhase::Data,
    });
    junction.copy_to(&target, cancel)?;
    if let Some(prepare) = policy.descriptions {
        crate::descriptions::copy_entry(source, &target, prepare)?;
    }
    Ok((true, 0))
}

#[cfg(test)]
fn copy_file<'a>(
    source: &Path,
    target: &Path,
    policy: impl Into<TransferPolicy<'a>>,
    cancel: &AtomicBool,
    progress: &mut impl FnMut(Progress<'_>),
) -> io::Result<bool> {
    let policy = policy.into();
    let mut native = crate::native_copy::State::new(policy.ntfs);
    copy_file_with_state(source, target, policy, &mut native, cancel, progress)
        .map(|result| result.is_some())
}

struct PublishedFile {
    path: PathBuf,
    appended: bool,
}

fn copy_file_with_state(
    source: &Path,
    target: &Path,
    policy: TransferPolicy<'_>,
    native: &mut crate::native_copy::State,
    cancel: &AtomicBool,
    progress: &mut impl FnMut(Progress<'_>),
) -> io::Result<Option<PublishedFile>> {
    let mut conflict = policy.conflict;
    let meta = regular_metadata(source)?;
    let previous = match fs::symlink_metadata(target) {
        Ok(previous) => {
            if let Some(resolve) = policy.resolve_conflict {
                let decision = resolve(FileConflict {
                    source,
                    target,
                    source_metadata: &meta,
                    target_metadata: &previous,
                })?;
                check_cancel(cancel)?;
                match decision {
                    ConflictDecision::Apply(rule) => conflict = rule,
                    ConflictDecision::RenameTo(name) => {
                        let target = target.with_file_name(leaf_name(&name)?);
                        copy_file_content(
                            source, &target, &meta, None, false, native, cancel, progress,
                        )?;
                        return Ok(Some(PublishedFile {
                            path: target,
                            appended: false,
                        }));
                    }
                }
            }
            if conflict == Conflict::Skip {
                return Ok(None);
            }
            if conflict == Conflict::Rename {
                let target = unused_copy_name(target, cancel)?;
                copy_file_content(
                    source, &target, &meta, None, false, native, cancel, progress,
                )?;
                return Ok(Some(PublishedFile {
                    path: target,
                    appended: false,
                }));
            }
            if conflict == Conflict::Fail || is_link(&previous) || !previous.is_file() {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    target.display().to_string(),
                ));
            }
            if (conflict == Conflict::Newer && meta.modified()? <= previous.modified()?)
                || (conflict == Conflict::Larger && meta.len() <= previous.len())
            {
                return Ok(None);
            }
            Some(previous)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    copy_file_content(
        source,
        target,
        &meta,
        previous.as_ref(),
        conflict == Conflict::Append,
        native,
        cancel,
        progress,
    )?;
    Ok(Some(PublishedFile {
        path: target.to_path_buf(),
        appended: conflict == Conflict::Append,
    }))
}

pub(crate) fn unused_copy_name(target: &Path, cancel: &AtomicBool) -> io::Result<PathBuf> {
    for index in 2..=10_000 {
        check_cancel(cancel)?;
        let candidate = numbered_copy_name(target, index)?;
        match fs::symlink_metadata(&candidate) {
            Ok(_) => (),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(candidate),
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::other("Kein freier Zielname gefunden."))
}

pub fn numbered_copy_name(target: &Path, index: usize) -> io::Result<PathBuf> {
    let mut name = target
        .file_stem()
        .ok_or_else(|| io::Error::other("Der Zielname fehlt."))?
        .to_os_string();
    name.push(format!(" ({index})"));
    if let Some(extension) = target.extension() {
        name.push(".");
        name.push(extension);
    }
    Ok(target.with_file_name(name))
}

pub(crate) fn copy_file_checked(
    source: &Path,
    target: &Path,
    meta: &Metadata,
    previous: Option<&Metadata>,
    cancel: &AtomicBool,
    progress: &mut impl FnMut(Progress<'_>),
) -> io::Result<()> {
    let mut native = crate::native_copy::State::new(NtfsCopyOptions::default());
    copy_file_content(
        source,
        target,
        meta,
        previous,
        false,
        &mut native,
        cancel,
        progress,
    )
}

pub fn append_file_checked(
    source: &Path,
    target: &Path,
    meta: &Metadata,
    previous: &Metadata,
    cancel: &AtomicBool,
) -> io::Result<()> {
    let mut native = crate::native_copy::State::new(NtfsCopyOptions::default());
    copy_file_content(
        source,
        target,
        meta,
        Some(previous),
        true,
        &mut native,
        cancel,
        &mut |_| {},
    )
}

pub fn copy_snapshot_to(
    source: &Path,
    expected: &Metadata,
    target: impl Write,
    cancel: &AtomicBool,
) -> io::Result<()> {
    ensure_unchanged(source, expected)?;
    let copied = crate::tools::copy_cancelled(
        open_for_read(source)?.take(expected.len().saturating_add(1)),
        target,
        cancel,
    )?;
    if copied != expected.len() {
        return Err(io::Error::other(
            "Die Dateigröße hat sich beim Kopieren geändert.",
        ));
    }
    ensure_unchanged(source, expected)
}

fn copy_file_content(
    source: &Path,
    target: &Path,
    meta: &Metadata,
    previous: Option<&Metadata>,
    append: bool,
    _native: &mut crate::native_copy::State,
    cancel: &AtomicBool,
    progress: &mut impl FnMut(Progress<'_>),
) -> io::Result<()> {
    let parent = target
        .parent()
        .ok_or_else(|| io::Error::other("missing target parent"))?;
    ensure_unchanged(source, meta)?;
    if let Some(previous) = previous {
        ensure_unchanged(target, previous)?;
    }
    let mut input = open_for_read(source)?;
    #[cfg(windows)]
    if !append && _native.try_link(&input, source, meta, target, previous, cancel)? {
        return Ok(());
    }
    #[cfg(windows)]
    _native.check_audit_privilege()?;
    let mut temp = NamedTempFile::new_in(parent)?;
    if append && let Some(previous) = previous {
        copy_snapshot_to(target, previous, &mut temp, cancel)?;
    }
    let mut copied = 0;
    progress(Progress {
        path: source,
        copied,
        total: meta.len(),
        items: None,
        current: None,
        phase: ProgressPhase::Data,
    });
    #[cfg(windows)]
    let sparse = !append
        && _native.sparse(&mut input, temp.as_file_mut(), meta, cancel, |copied| {
            progress(Progress {
                path: source,
                copied,
                total: meta.len(),
                items: None,
                current: None,
                phase: ProgressPhase::Data,
            });
        })?;
    #[cfg(not(windows))]
    let sparse = false;
    if !sparse {
        let mut buffer = vec![0; 256 * 1024];
        loop {
            check_cancel(cancel)?;
            let count = input.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            temp.write_all(&buffer[..count])?;
            copied += count as u64;
            progress(Progress {
                path: source,
                copied,
                total: meta.len(),
                items: None,
                current: None,
                phase: ProgressPhase::Data,
            });
        }
    }
    ensure_unchanged(source, meta)?;
    if !sparse && copied != meta.len() {
        return Err(io::Error::other("source size changed during copy"));
    }
    #[cfg(windows)]
    if append && let Some(previous) = previous {
        ensure_unchanged(target, previous)?;
        let existing = open_for_read(target)?;
        _native.prepare(&existing, target, (temp.path(), temp.as_file()), cancel)?;
    } else {
        _native.prepare(&input, source, (temp.path(), temp.as_file()), cancel)?;
    }
    set_file_handle_times(
        temp.as_file(),
        Some(FileTime::from_last_access_time(meta)),
        Some(FileTime::from_last_modification_time(meta)),
    )?;
    let published = publish_checked(temp, target, previous, cancel)?;
    #[cfg(windows)]
    _native.copy_basic_attributes(
        &published,
        meta,
        if append {
            previous.unwrap_or(meta)
        } else {
            meta
        },
    )?;
    #[cfg(not(windows))]
    published.set_permissions(meta.permissions())?;
    #[cfg(windows)]
    if !append {
        _native.remember(&input, &published, target)?;
    }
    Ok(())
}

pub fn publish_checked(
    temp: NamedTempFile,
    target: &Path,
    previous: Option<&Metadata>,
    cancel: &AtomicBool,
) -> io::Result<File> {
    temp.as_file().sync_all()?;
    check_cancel(cancel)?;
    let published = if let Some(previous) = previous {
        ensure_unchanged(target, previous)?;
        temp.persist(target)
    } else {
        temp.persist_noclobber(target)
    };
    published.map_err(|error| error.error)
}

fn open_for_read(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(1); // FILE_SHARE_READ
    }
    options.open(path)
}

fn equivalent_path(a: &Path, b: &Path) -> bool {
    fs::canonicalize(b).is_ok_and(|other| a == other)
}

fn same_or_inside(path: &Path, parent: &Path) -> bool {
    path.ancestors()
        .any(|ancestor| ancestor == parent || equivalent_path(parent, ancestor))
}

pub(crate) fn create_or_verify_directory(path: &Path) -> io::Result<()> {
    create_transfer_directory(path).map(|_| ())
}

fn create_transfer_directory(path: &Path) -> io::Result<bool> {
    match fs::create_dir(path) {
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            let metadata = fs::symlink_metadata(path)?;
            if metadata.is_dir() && !is_link(&metadata) {
                Ok(false)
            } else {
                Err(error)
            }
        }
        result => result.map(|()| true),
    }
}

pub fn regular_metadata(path: &Path) -> io::Result<Metadata> {
    checked_regular_metadata(path, fs::symlink_metadata(path)?)
}

fn checked_regular_metadata(path: &Path, meta: Metadata) -> io::Result<Metadata> {
    if is_link(&meta) || !(meta.is_file() || meta.is_dir()) {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!("unsupported file type: {}", path.display()),
        ));
    }
    Ok(meta)
}

pub fn ensure_unchanged(path: &Path, expected: &Metadata) -> io::Result<()> {
    let current = regular_metadata(path)?;
    if current.len() == expected.len()
        && current.is_dir() == expected.is_dir()
        && current.modified()? == expected.modified()?
        && same_identity_hint(&current, expected)
    {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "entry changed: {}",
            path.display()
        )))
    }
}

#[cfg(windows)]
pub(crate) fn same_identity_hint(left: &Metadata, right: &Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    left.creation_time() == right.creation_time()
}

#[cfg(unix)]
pub(crate) fn same_identity_hint(left: &Metadata, right: &Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    left.dev() == right.dev() && left.ino() == right.ino()
}

#[cfg(not(any(windows, unix)))]
pub(crate) fn same_identity_hint(_: &Metadata, _: &Metadata) -> bool {
    true
}

pub fn is_link(meta: &Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        meta.file_attributes() & 0x400 != 0 // FILE_ATTRIBUTE_REPARSE_POINT
    }
    #[cfg(not(windows))]
    meta.is_symlink()
}

fn is_hidden(meta: &Metadata, name: &OsStr) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        name.to_string_lossy().starts_with('.') || meta.file_attributes() & 0x6 != 0
    }
    #[cfg(not(windows))]
    {
        let _ = meta;
        name.to_string_lossy().starts_with('.')
    }
}

pub fn check_cancel(cancel: &AtomicBool) -> io::Result<()> {
    if cancel.load(AtomicOrdering::Relaxed) {
        Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(any(unix, windows))]
    #[test]
    fn marked_transfer_resolves_directory_aliases_once_and_rejects_source_overlap() {
        let fixture = tempfile::tempdir().unwrap();
        let source = fixture.path().join("source");
        let destination = fixture.path().join("target");
        let alias = fixture.path().join("alias");
        let unsafe_alias = fixture.path().join("source-alias");
        fs::create_dir(&source).unwrap();
        fs::create_dir(&destination).unwrap();
        fs::write(source.join("sample.txt"), "sample").unwrap();
        let create = |target: &Path, alias: &Path| {
            #[cfg(unix)]
            std::os::unix::fs::symlink(target, alias).unwrap();
            #[cfg(windows)]
            {
                let status = std::process::Command::new("cmd.exe")
                    .args(["/d", "/c", "mklink", "/J"])
                    .arg(alias)
                    .arg(target)
                    .status()
                    .unwrap();
                assert!(status.success());
            }
        };
        create(&destination, &alias);
        create(&source, &unsafe_alias);
        let cancel = AtomicBool::new(false);
        assert!(
            copy_to_directories(
                std::slice::from_ref(&source),
                &[alias.clone(), unsafe_alias],
                Conflict::Fail.into(),
                &cancel,
                |_, _| {}
            )
            .is_err()
        );
        assert!(!destination.join("source").exists());
        let report = copy_to_directories(
            &[source],
            &[alias, destination.clone()],
            Conflict::Fail.into(),
            &cancel,
            |_, _| {},
        )
        .unwrap();
        assert!(report.errors.is_empty(), "{report:?}");
        assert_eq!(report.completed, 1);
        assert_eq!(
            fs::read_to_string(destination.join("source/sample.txt")).unwrap(),
            "sample"
        );
    }

    #[test]
    fn recursive_move_merges_comments_and_keeps_skipped_source_comments() {
        use crate::descriptions;
        let fixture = tempfile::tempdir().unwrap();
        let source = fixture.path().join("source");
        let destination = fixture.path().join("target");
        let target = destination.join("source");
        fs::create_dir(&source).unwrap();
        fs::create_dir_all(&target).unwrap();
        let prepare = |_: &File, _: Option<&Metadata>| Ok(());
        for (directory, name, comment) in [
            (&source, "rename.txt", "Source note"),
            (&source, "skip.txt", "Skipped source"),
            (&target, "rename.txt", "Existing target"),
            (&target, "skip.txt", "Skipped target"),
            (&target, "unrelated.txt", "Unrelated"),
        ] {
            let path = directory.join(name);
            fs::write(&path, comment).unwrap();
            descriptions::set(&path, None, comment, prepare).unwrap();
        }
        descriptions::set(&source, None, "Folder note", prepare).unwrap();
        let resolve = |conflict: FileConflict<'_>| {
            Ok(if conflict.source.ends_with("rename.txt") {
                ConflictDecision::RenameTo("renamed.txt".into())
            } else {
                Conflict::Skip.into()
            })
        };
        let report = transfer(
            std::slice::from_ref(&source),
            &destination,
            Transfer::Move,
            TransferPolicy {
                descriptions: Some(prepare),
                resolve_conflict: Some(&resolve),
                ..Conflict::Fail.into()
            },
            &AtomicBool::new(false),
            None,
            |_| {},
        )
        .unwrap();
        assert!(report.errors.is_empty(), "{report:?}");
        assert!(!source.join("rename.txt").exists());
        assert_eq!(
            descriptions::read(&source).unwrap(),
            BTreeMap::from([("skip.txt".into(), "Skipped source".into())])
        );
        let comments = descriptions::read(&target).unwrap();
        assert_eq!(comments["renamed.txt"], "Source note");
        assert_eq!(comments["rename.txt"], "Existing target");
        assert_eq!(comments["skip.txt"], "Skipped target");
        assert_eq!(comments["unrelated.txt"], "Unrelated");
        assert_eq!(
            descriptions::read(&destination).unwrap()["source"],
            "Folder note"
        );
    }

    #[test]
    fn mixed_sort_combines_extension_typed_plugin_values_and_size() {
        use crate::plugin::{WdxKey, WdxValue, wdx_order};
        let value = |number: i64| WdxValue {
            display: number.to_string(),
            key: WdxKey::Integer(number),
        };
        let values = BTreeMap::from([
            ("a.txt", value(9)),
            ("b.txt", value(10)),
            ("c.txt", value(10)),
            ("a.png", value(0)),
        ]);
        let mut entries: Vec<_> = [
            ("a.txt", 10),
            ("b.txt", 1),
            ("c.txt", 5),
            ("a.png", 1),
            ("missing.txt", 99),
        ]
        .into_iter()
        .map(|(name, size)| Entry::from_remote(PathBuf::from(name), Kind::File, Some(size), None))
        .collect();
        let orders = [
            SortOrder {
                key: SortKey::Extension,
                descending: false,
            },
            SortOrder {
                key: SortKey::Wdx(1),
                descending: true,
            },
            SortOrder {
                key: SortKey::Size,
                descending: true,
            },
        ];
        sort_by_columns_with(&mut entries, &orders, |index, a, b, descending| {
            assert_eq!(index, 1);
            wdx_order(
                values.get(a.name.to_str().unwrap()),
                values.get(b.name.to_str().unwrap()),
                descending,
            )
        });
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.name.to_str().unwrap())
                .collect::<Vec<_>>(),
            ["a.png", "c.txt", "b.txt", "a.txt", "missing.txt"]
        );
    }

    #[test]
    fn conflict_custom_name_only_publishes_to_an_unused_leaf() {
        let fixture = tempfile::tempdir().unwrap();
        let source = fixture.path().join("source.txt");
        let target = fixture.path().join("target.txt");
        let alternate = fixture.path().join("alternate.txt");
        fs::write(&source, "new").unwrap();
        fs::write(&target, "old").unwrap();
        let copy_as = |name: &str| {
            let resolve = |_: FileConflict<'_>| Ok(ConflictDecision::RenameTo(name.into()));
            copy_file(
                &source,
                &target,
                TransferPolicy {
                    resolve_conflict: Some(&resolve),
                    ..Conflict::Fail.into()
                },
                &AtomicBool::new(false),
                &mut |_| {},
            )
        };
        assert!(copy_as("../outside.txt").is_err());
        assert!(copy_as("target.txt").is_err());
        assert!(copy_as("alternate.txt").unwrap());
        assert_eq!(fs::read_to_string(&alternate).unwrap(), "new");
        assert!(copy_as("alternate.txt").is_err());
        assert_eq!(fs::read_to_string(&target).unwrap(), "old");
    }

    #[test]
    fn conflict_resolver_does_not_overwrite_a_target_changed_during_confirmation() {
        let fixture = tempfile::tempdir().unwrap();
        let source = fixture.path().join("source.txt");
        let target = fixture.path().join("target.txt");
        fs::write(&source, "new").unwrap();
        fs::write(&target, "old").unwrap();
        let resolve = |conflict: FileConflict<'_>| {
            assert_eq!(conflict.source, source);
            assert_eq!(conflict.target, target);
            assert_eq!(conflict.target_metadata.len(), 3);
            fs::write(conflict.target, "changed meanwhile")?;
            Ok(Conflict::Replace.into())
        };
        let policy = TransferPolicy {
            resolve_conflict: Some(&resolve),
            ..Conflict::Fail.into()
        };
        assert!(
            copy_file(
                &source,
                &target,
                policy,
                &AtomicBool::new(false),
                &mut |_| {}
            )
            .is_err()
        );
        assert_eq!(fs::read_to_string(&target).unwrap(), "changed meanwhile");
    }

    #[test]
    fn per_file_skip_in_a_directory_move_keeps_only_the_skipped_source() {
        let fixture = tempfile::tempdir().unwrap();
        let source = fixture.path().join("source");
        let target = fixture.path().join("target");
        fs::create_dir(&source).unwrap();
        fs::create_dir_all(target.join("source")).unwrap();
        fs::write(source.join("keep.txt"), "source").unwrap();
        fs::write(source.join("move.txt"), "copy").unwrap();
        fs::write(target.join("source/keep.txt"), "target").unwrap();
        let resolve = |conflict: FileConflict<'_>| {
            assert!(conflict.source.ends_with("keep.txt"));
            Ok(Conflict::Skip.into())
        };
        let report = transfer(
            std::slice::from_ref(&source),
            &target,
            Transfer::Move,
            TransferPolicy {
                resolve_conflict: Some(&resolve),
                ..Conflict::Fail.into()
            },
            &AtomicBool::new(false),
            None,
            |_| {},
        )
        .unwrap();
        assert!(report.errors.is_empty(), "{report:?}");
        assert_eq!(report.skipped, 1);
        assert!(!source.join("move.txt").exists());
        assert_eq!(
            fs::read_to_string(source.join("keep.txt")).unwrap(),
            "source"
        );
        assert_eq!(
            fs::read_to_string(target.join("source/keep.txt")).unwrap(),
            "target"
        );
        assert_eq!(
            fs::read_to_string(target.join("source/move.txt")).unwrap(),
            "copy"
        );
    }

    #[test]
    fn filtered_move_keeps_nonmatching_source_files_and_directories() {
        let fixture = tempfile::tempdir().unwrap();
        let source = fixture.path().join("source");
        let target = fixture.path().join("target");
        fs::create_dir_all(source.join("nested")).unwrap();
        fs::create_dir(source.join("empty")).unwrap();
        fs::create_dir(&target).unwrap();
        fs::write(source.join("nested/move.txt"), b"move").unwrap();
        fs::write(source.join("nested/keep.bin"), b"keep").unwrap();
        let filter = crate::advanced_search::Options {
            names: "*.txt".into(),
            ..Default::default()
        }
        .compile()
        .unwrap();
        let report = transfer(
            std::slice::from_ref(&source),
            &target,
            Transfer::Move,
            TransferPolicy {
                conflict: Conflict::Fail,
                resolve_conflict: None,
                ntfs: Default::default(),
                descriptions: None,
                ignore: None,
                filter: Some(&filter),
                before_entry: None,
            },
            &AtomicBool::new(false),
            None,
            |_| {},
        )
        .unwrap();
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(
            fs::read(target.join("source/nested/move.txt")).unwrap(),
            b"move"
        );
        assert!(!source.join("nested/move.txt").exists());
        assert!(source.join("nested/keep.bin").is_file());
        assert!(!target.join("source/nested/keep.bin").exists());
        assert!(source.join("empty").is_dir());
        assert!(!target.join("source/empty").exists());
    }

    #[test]
    fn transfer_conflicts_use_metadata_and_preserve_existing_bytes() {
        let fixture = tempfile::tempdir().unwrap();
        let source = fixture.path().join("source.txt");
        let target = fixture.path().join("target.txt");
        fs::write(&source, b"new").unwrap();
        fs::write(&target, b"original").unwrap();
        filetime::set_file_mtime(&source, FileTime::from_unix_time(1_700_000_000, 0)).unwrap();
        filetime::set_file_mtime(&target, FileTime::from_unix_time(1_700_000_001, 0)).unwrap();
        let cancel = AtomicBool::new(false);
        for rule in [Conflict::Newer, Conflict::Larger] {
            assert!(!copy_file(&source, &target, rule, &cancel, &mut |_| {}).unwrap());
        }
        assert!(copy_file(&source, &target, Conflict::Rename, &cancel, &mut |_| {}).unwrap());
        assert_eq!(
            fs::read(target.with_file_name("target (2).txt")).unwrap(),
            b"new"
        );
        assert_eq!(fs::read(&target).unwrap(), b"original");
        assert!(copy_file(&source, &target, Conflict::Append, &cancel, &mut |_| {}).unwrap());
        assert_eq!(fs::read(target).unwrap(), b"originalnew");
    }

    #[test]
    fn target_masks_expand_names_and_reject_batch_collisions_before_copying() {
        assert_eq!(masked_name("Datei.txt", "A*.*").unwrap(), "Aatei.txt");
        assert_eq!(masked_name("Datei.txt", "*.a*").unwrap(), "Datei.axt");
        assert_eq!(
            masked_name("Datei.txt", "*-Kopie.*").unwrap(),
            "Datei-Kopie.txt"
        );
        assert_eq!(masked_name("a.tar.gz", "*.*.bak").unwrap(), "a.tar.gz.bak");
        assert_eq!(masked_name("README", "*.*").unwrap(), "README");
        assert!(masked_name("Datei.txt", "../*").is_err());
        let fixture = tempfile::tempdir().unwrap();
        let source = fixture.path().join("source");
        let target = fixture.path().join("target");
        fs::create_dir(&source).unwrap();
        fs::create_dir(&target).unwrap();
        let inputs = [source.join("A.txt"), source.join("B.txt")];
        fs::write(&inputs[0], b"1").unwrap();
        fs::write(&inputs[1], b"2").unwrap();
        let cancel = AtomicBool::new(false);
        assert!(
            transfer_masked(
                &inputs,
                &target,
                "same.bak",
                Transfer::Copy,
                Conflict::Replace.into(),
                &cancel,
                None,
                |_| {}
            )
            .is_err()
        );
        assert_eq!(fs::read_dir(&target).unwrap().count(), 0);
        let report = transfer_masked(
            &inputs,
            &target,
            "*.bak",
            Transfer::Move,
            Conflict::Fail.into(),
            &cancel,
            None,
            |_| {},
        )
        .unwrap();
        assert_eq!(report.completed, 2);
        assert_eq!(fs::read(target.join("A.bak")).unwrap(), b"1");
        assert_eq!(fs::read(target.join("B.bak")).unwrap(), b"2");
        assert!(!inputs.iter().any(|path| path.exists()));
    }

    #[test]
    fn copy_to_marked_directories_validates_all_targets_before_writing() {
        let fixture = tempfile::tempdir().unwrap();
        let base = fixture.path();
        let source = base.join("Bücher");
        let nested = source.join("unterordner");
        fs::create_dir_all(&nested).unwrap();
        fs::write(nested.join("inhalt.txt"), b"new").unwrap();
        fs::write(nested.join("ignored.tmp"), b"excluded").unwrap();
        let left = base.join("ziel-a");
        let right = base.join("ziel-b");
        fs::create_dir_all(right.join("Bücher").join("unterordner")).unwrap();
        fs::create_dir(&left).unwrap();
        let right_file = right.join("Bücher").join("unterordner").join("inhalt.txt");
        fs::write(&right_file, b"old").unwrap();
        let ignore = crate::ignore::IgnoreList::parse("*.tmp").unwrap();
        let policy = TransferPolicy {
            conflict: Conflict::Skip,
            resolve_conflict: None,
            ntfs: Default::default(),
            descriptions: None,
            ignore: Some(&ignore),
            filter: None,
            before_entry: None,
        };
        let cancel = AtomicBool::new(false);
        assert!(
            copy_to_directories(
                std::slice::from_ref(&source),
                &[left.clone(), nested],
                policy,
                &cancel,
                |_, _| {},
            )
            .is_err()
        );
        assert!(!left.join("Bücher").exists());
        let report = copy_to_directories(
            std::slice::from_ref(&source),
            &[left.clone(), right, left.clone()],
            policy,
            &cancel,
            |_, _| {},
        )
        .unwrap();
        assert_eq!(
            (report.completed, report.skipped, report.ignored),
            (1, 1, 2)
        );
        assert!(report.errors.is_empty(), "{report:?}");
        let copied = left.join("Bücher").join("unterordner");
        assert_eq!(fs::read(copied.join("inhalt.txt")).unwrap(), b"new");
        assert_eq!(fs::read(right_file).unwrap(), b"old");
        assert!(!copied.join("ignored.tmp").exists());
    }

    #[test]
    fn measure_counts_empty_directories_and_counts_overlapping_selections_once() {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path();
        let selected = base.join("selected");
        let nested = selected.join("nested");
        let empty = selected.join("empty");
        fs::create_dir_all(&nested).unwrap();
        fs::create_dir(&empty).unwrap();
        let file = nested.join("file.txt");
        fs::write(&file, b"four").unwrap();
        let separate = base.join("separate.txt");
        fs::write(&separate, b"one").unwrap();
        let roots = [selected, nested, file, separate];
        let usage = measure(&roots, &AtomicBool::new(false)).unwrap();
        assert_eq!((usage.bytes, usage.files, usage.directories), (7, 2, 3));
        assert!(usage.errors.is_empty());
        assert_eq!(
            measure(&roots, &AtomicBool::new(true))
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::Interrupted
        );
    }

    #[test]
    fn selected_branch_includes_only_selected_files_and_files_below_selected_folders() {
        let root = tempfile::tempdir().unwrap();
        let base = root.path();
        let selected = base.join("docs");
        fs::create_dir_all(selected.join("nested")).unwrap();
        let nested_file = selected.join("nested").join("a.txt");
        let selected_file = base.join("b.txt");
        for path in [&nested_file, &selected_file, &base.join("skip.txt")] {
            fs::write(path, b"data").unwrap();
        }
        let directory = branch_selected(
            base,
            vec![selected, nested_file.clone(), selected_file.clone()],
            &AtomicBool::new(false),
        )
        .unwrap();
        let paths: BTreeSet<_> = directory
            .entries
            .into_iter()
            .map(|entry| entry.path)
            .collect();
        assert_eq!(paths, BTreeSet::from([nested_file, selected_file]));
    }
    use tempfile::tempdir;

    #[test]
    fn copy_never_overwrites_without_permission() {
        let root = tempdir().unwrap();
        let source = root.path().join("ä.txt");
        let destination = root.path().join("target");
        fs::create_dir(&destination).unwrap();
        fs::write(&source, b"new").unwrap();
        fs::write(destination.join("ä.txt"), b"original").unwrap();
        let report = transfer(
            &[source],
            &destination,
            Transfer::Copy,
            Conflict::Fail,
            &AtomicBool::new(false),
            None,
            |_| {},
        )
        .unwrap();
        assert_eq!(report.errors.len(), 1);
        assert_eq!(fs::read(destination.join("ä.txt")).unwrap(), b"original");
    }

    #[test]
    fn cancelled_copy_does_not_publish_partial_destination() {
        let root = tempdir().unwrap();
        let source = root.path().join("file.txt");
        let destination = root.path().join("target");
        fs::create_dir(&destination).unwrap();
        fs::write(&source, vec![0; 512 * 1024]).unwrap();
        let cancel = AtomicBool::new(false);
        let report = transfer(
            &[source],
            &destination,
            Transfer::Copy,
            Conflict::Fail,
            &cancel,
            None,
            |event| {
                if event.copied > 0 {
                    cancel.store(true, AtomicOrdering::Relaxed);
                }
            },
        )
        .unwrap();
        assert!(report.cancelled);
        assert_eq!(fs::read_dir(destination).unwrap().count(), 0);
    }

    #[test]
    fn move_tree_keeps_source_when_a_conflict_was_skipped() {
        let root = tempdir().unwrap();
        let source = root.path().join("source");
        let target = root.path().join("target");
        fs::create_dir(&source).unwrap();
        fs::create_dir_all(target.join("source")).unwrap();
        fs::write(source.join("a"), b"source").unwrap();
        fs::write(target.join("source/a"), b"existing").unwrap();
        let report = transfer(
            std::slice::from_ref(&source),
            &target,
            Transfer::Move,
            Conflict::Skip,
            &AtomicBool::new(false),
            None,
            |_| {},
        )
        .unwrap();
        assert_eq!(report.skipped, 1);
        assert_eq!(fs::read(source.join("a")).unwrap(), b"source");
    }

    #[test]
    fn copy_and_move_skip_ignored_subtrees_and_keep_their_source_parents() {
        let fixture = tempdir().unwrap();
        let source = fixture.path().join("source");
        let copied = fixture.path().join("copied");
        let moved = fixture.path().join("moved");
        for path in [source.join("nested/cache"), copied.clone(), moved.clone()] {
            fs::create_dir_all(path).unwrap();
        }
        fs::write(source.join("nested/cache/data"), b"ignore").unwrap();
        fs::write(source.join("nested/keep"), b"keep").unwrap();
        let ignore = crate::ignore::IgnoreList::parse("cache\\").unwrap();
        let cancel = AtomicBool::new(false);
        let usage = measure_ignoring(
            std::slice::from_ref(&source),
            &cancel,
            Some(&ignore),
            |_| {},
        )
        .unwrap();
        assert_eq!((usage.bytes, usage.files), (4, 1));
        for (operation, target) in [(Transfer::Copy, copied), (Transfer::Move, moved)] {
            let report = transfer(
                std::slice::from_ref(&source),
                &target,
                operation,
                TransferPolicy {
                    conflict: Conflict::Fail,
                    resolve_conflict: None,
                    ntfs: Default::default(),
                    descriptions: None,
                    ignore: Some(&ignore),
                    filter: None,
                    before_entry: None,
                },
                &cancel,
                Some(|from, to| fs::rename(from, to)),
                |_| {},
            )
            .unwrap();
            assert!(report.errors.is_empty(), "{report:?}");
            assert_eq!(report.ignored, 1);
            assert_eq!(
                fs::read(target.join("source/nested/keep")).unwrap(),
                b"keep"
            );
            assert!(!target.join("source/nested/cache").exists());
        }
        assert!(!source.join("nested/keep").exists());
        assert_eq!(
            fs::read(source.join("nested/cache/data")).unwrap(),
            b"ignore"
        );
    }

    #[test]
    fn natural_comparison_handles_numbers_beyond_integer_limits() {
        assert_eq!(natural_cmp("file2.txt", "file10.txt"), Ordering::Less);
        assert_eq!(natural_cmp("p00000009", "p10"), Ordering::Less);
        assert_eq!(
            natural_cmp("p10000000000000000000000000000000000", "p20"),
            Ordering::Greater
        );
    }
}
