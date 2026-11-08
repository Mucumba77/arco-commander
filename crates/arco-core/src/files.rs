use filetime::{FileTime, set_file_handle_times, set_file_times};
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use std::time::SystemTime;
use tempfile::NamedTempFile;
use walkdir::WalkDir;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
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
    sort_extension: String,
    sort_name: String,
}

impl Entry {
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
        }
    }
    pub(crate) fn read(path: PathBuf, name: OsString) -> io::Result<Self> {
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

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SortKey {
    #[default]
    Name,
    Extension,
    Size,
    Modified,
    Attributes,
}

pub fn sort(entries: &mut [Entry], key: SortKey, descending: bool) {
    entries.sort_by(|a, b| {
        let directory_order = (b.kind == Kind::Directory).cmp(&(a.kind == Kind::Directory));
        let order = match key {
            SortKey::Name => Ordering::Equal,
            SortKey::Extension => a.sort_extension.cmp(&b.sort_extension),
            SortKey::Size => a.size.cmp(&b.size),
            SortKey::Modified => a.modified.cmp(&b.modified),
            SortKey::Attributes => a.attributes.cmp(&b.attributes),
        }
        .then_with(|| natural_cmp(&a.sort_name, &b.sort_name))
        .then_with(|| a.name.cmp(&b.name));
        directory_order.then(if descending { order.reverse() } else { order })
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
    pub errors: Vec<String>,
    pub cancelled: bool,
}

pub struct Progress<'a> {
    pub path: &'a Path,
    pub copied: u64,
    pub total: u64,
}

pub type NativeRename = fn(&Path, &Path) -> io::Result<()>;

pub fn transfer(
    sources: &[PathBuf],
    destination: &Path,
    operation: Transfer,
    conflict: Conflict,
    cancel: &AtomicBool,
    rename: Option<NativeRename>,
    mut progress: impl FnMut(Progress<'_>),
) -> io::Result<TransferReport> {
    let destination = fs::canonicalize(destination)?;
    if !destination.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotADirectory,
            "target is not a directory",
        ));
    }
    let mut report = TransferReport::default();
    for source in sources {
        if cancel.load(AtomicOrdering::Relaxed) {
            report.cancelled = true;
            break;
        }
        let outcome = transfer_item(
            source,
            &destination,
            operation,
            conflict,
            cancel,
            rename,
            &mut progress,
        );
        match outcome {
            Ok(true) => report.completed += 1,
            Ok(false) => report.skipped += 1,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                report.cancelled = true;
                break;
            }
            Err(error) => report.errors.push(format!("{}: {error}", source.display())),
        }
    }
    Ok(report)
}

fn transfer_item(
    source: &Path,
    destination: &Path,
    operation: Transfer,
    conflict: Conflict,
    cancel: &AtomicBool,
    rename: Option<NativeRename>,
    progress: &mut impl FnMut(Progress<'_>),
) -> io::Result<bool> {
    let name = source
        .file_name()
        .ok_or_else(|| io::Error::other("cannot transfer a root"))?;
    let target = destination.join(name);
    let meta = regular_metadata(source)?;
    let source_real = fs::canonicalize(source)?;
    if same_or_inside(destination, &source_real) || equivalent_path(&source_real, &target) {
        return Err(io::Error::other("source and target overlap"));
    }
    if operation == Transfer::Move
        && let Some(rename) = rename
    {
        match fs::symlink_metadata(&target) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                check_cancel(cancel)?;
                ensure_unchanged(source, &meta)?;
                match rename(source, &target) {
                    Ok(()) => return Ok(true),
                    Err(error) if error.kind() == io::ErrorKind::CrossesDevices => (),
                    Err(error) => return Err(error),
                }
            }
            Err(error) => return Err(error),
            Ok(_) => (),
        }
    }
    if meta.is_file() {
        let copied = copy_file(source, &target, conflict, cancel, progress)?;
        if copied && operation == Transfer::Move {
            check_cancel(cancel)?;
            ensure_unchanged(source, &meta)?;
            fs::remove_file(source)?;
        }
        return Ok(copied);
    }

    let mut plan = Vec::new();
    for item in WalkDir::new(source).follow_links(false) {
        check_cancel(cancel)?;
        let item = item?;
        let from = item.into_path();
        let meta = regular_metadata(&from)?;
        let relative = from.strip_prefix(source).map_err(io::Error::other)?;
        let to = target.join(relative);
        plan.push((from, to, meta));
    }

    let mut skipped = false;
    for (from, to, meta) in &plan {
        check_cancel(cancel)?;
        if meta.is_dir() {
            create_or_verify_directory(to)?;
        } else {
            ensure_unchanged(from, meta)?;
            skipped |= !copy_file(from, to, conflict, cancel, progress)?;
        }
    }
    for (_, to, meta) in plan.iter().rev().filter(|(_, _, meta)| meta.is_dir()) {
        set_file_times(
            to,
            FileTime::from_last_access_time(meta),
            FileTime::from_last_modification_time(meta),
        )?;
    }
    if !skipped && operation == Transfer::Move {
        check_cancel(cancel)?;
        for (from, _, meta) in &plan {
            ensure_unchanged(from, meta)?;
        }
        for (from, _, meta) in plan.iter().rev() {
            check_cancel(cancel)?;
            if meta.is_dir() {
                fs::remove_dir(from)?;
            } else {
                ensure_unchanged(from, meta)?;
                fs::remove_file(from)?;
            }
        }
    }
    Ok(!skipped)
}

pub(crate) fn copy_file(
    source: &Path,
    target: &Path,
    conflict: Conflict,
    cancel: &AtomicBool,
    progress: &mut impl FnMut(Progress<'_>),
) -> io::Result<bool> {
    let previous = match fs::symlink_metadata(target) {
        Ok(meta) => {
            if conflict == Conflict::Skip {
                return Ok(false);
            }
            if conflict != Conflict::Replace || is_link(&meta) || !meta.is_file() {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    target.display().to_string(),
                ));
            }
            Some(meta)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    let meta = regular_metadata(source)?;
    copy_file_checked(source, target, &meta, previous.as_ref(), cancel, progress)?;
    Ok(true)
}

pub(crate) fn copy_file_checked(
    source: &Path,
    target: &Path,
    meta: &Metadata,
    previous: Option<&Metadata>,
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
    let mut temp = NamedTempFile::new_in(parent)?;
    let mut buffer = vec![0; 256 * 1024];
    let mut copied = 0;
    progress(Progress {
        path: source,
        copied,
        total: meta.len(),
    });
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
        });
    }
    ensure_unchanged(source, meta)?;
    if copied != meta.len() {
        return Err(io::Error::other("source size changed during copy"));
    }
    set_file_handle_times(
        temp.as_file(),
        Some(FileTime::from_last_access_time(meta)),
        Some(FileTime::from_last_modification_time(meta)),
    )?;
    let published = publish_checked(temp, target, previous, cancel)?;
    published.set_permissions(meta.permissions())?;
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
    match fs::create_dir(path) {
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            let metadata = fs::symlink_metadata(path)?;
            if metadata.is_dir() && !is_link(&metadata) {
                Ok(())
            } else {
                Err(error)
            }
        }
        result => result,
    }
}

pub fn regular_metadata(path: &Path) -> io::Result<Metadata> {
    let meta = fs::symlink_metadata(path)?;
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

pub(crate) fn check_cancel(cancel: &AtomicBool) -> io::Result<()> {
    if cancel.load(AtomicOrdering::Relaxed) {
        Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn natural_comparison_handles_numbers_beyond_integer_limits() {
        assert_eq!(natural_cmp("file2.txt", "file10.txt"), Ordering::Less);
        assert_eq!(natural_cmp("p00000009", "p10"), Ordering::Less);
        assert_eq!(
            natural_cmp("p10000000000000000000000000000000000", "p20"),
            Ordering::Greater
        );
    }
}
