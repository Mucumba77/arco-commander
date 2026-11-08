use crate::{archives, external_archives::ExternalArchiver, files, tools};
use chrono::{Local, NaiveDateTime, TimeZone};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, Metadata};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::SystemTime;

const MAX_NODES: usize = 500_000;
const MAX_EXTRACT: u64 = 256 * 1024 * 1024 * 1024;
pub const MAX_PREVIEW: u64 = 64 * 1024 * 1024;

pub(crate) fn local_timestamp(time: NaiveDateTime) -> Option<SystemTime> {
    Local.from_local_datetime(&time).earliest().map(Into::into)
}

pub(crate) fn preserve_timestamp(file: &File, timestamp: Option<SystemTime>) -> io::Result<()> {
    if let Some(time) = timestamp {
        filetime::set_file_handle_times(
            file,
            None,
            Some(filetime::FileTime::from_system_time(time)),
        )?;
    }
    Ok(())
}

pub fn is_zip(path: &Path) -> bool {
    path.extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("zip") || ext.eq_ignore_ascii_case("jar"))
}

pub fn is_supported(path: &Path) -> bool {
    is_zip(path) || crate::external_archives::is_supported(path)
}

#[derive(Clone, Debug)]
pub struct Item {
    pub path: PathBuf,
    pub directory: bool,
    pub size: u64,
    pub encrypted: bool,
    pub modified: Option<SystemTime>,
}

pub struct Catalog {
    file: PathBuf,
    metadata: Metadata,
    folders: BTreeMap<PathBuf, Vec<files::Entry>>,
    encrypted: bool,
}

impl Catalog {
    pub fn read(file: &Path, cancel: &AtomicBool) -> io::Result<Self> {
        let metadata = files::regular_metadata(file)?;
        if !metadata.is_file() {
            return Err(io::Error::other(
                "Das Archiv muss eine reguläre Datei sein.",
            ));
        }
        let entries: Vec<_> = if is_zip(file) {
            archives::list_zip(file, cancel)?
                .into_iter()
                .map(|item| Item {
                    path: item.path,
                    directory: item.directory,
                    size: item.size,
                    encrypted: item.encrypted,
                    modified: item.modified,
                })
                .collect()
        } else {
            ExternalArchiver::discover(cancel)?
                .list(file, cancel)?
                .into_iter()
                .map(|item| Item {
                    path: item.path,
                    directory: item.directory,
                    size: item.size,
                    encrypted: item.encrypted,
                    modified: item.modified,
                })
                .collect()
        };
        let encrypted = entries.iter().any(|item| item.encrypted);
        let folders = index(entries, cancel)?;
        files::ensure_unchanged(file, &metadata)?;
        Ok(Self {
            file: file.to_path_buf(),
            metadata,
            folders,
            encrypted,
        })
    }

    pub fn unchanged(&self, file: &Path) -> bool {
        self.file == file && files::ensure_unchanged(file, &self.metadata).is_ok()
    }

    pub fn contains_encrypted_entries(&self) -> bool {
        self.encrypted
    }

    pub fn rows(&self, folder: &Path) -> io::Result<Vec<files::Entry>> {
        self.folders
            .get(folder)
            .cloned()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "Ordner im Archiv fehlt."))
    }

    pub fn search(
        &self,
        folder: &Path,
        query: &tools::Search,
        password: Option<&str>,
        cancel: &AtomicBool,
    ) -> io::Result<files::Directory> {
        files::ensure_unchanged(&self.file, &self.metadata)?;
        if query.text.is_some() && !is_zip(&self.file) {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Textsuche in Archiven unterstützt ZIP und JAR.",
            ));
        }
        let mut entries = Vec::new();
        for (parent, rows) in self.folders.range(folder.to_path_buf()..) {
            if !parent.starts_with(folder) {
                break;
            }
            files::check_cancel(cancel)?;
            if query.max_depth.is_some_and(|depth| {
                parent.components().count() - folder.components().count() >= depth
            }) {
                continue;
            }
            for row in rows {
                files::check_cancel(cancel)?;
                if (query.show_hidden || !row.hidden)
                    && (query.names.is_empty() || query.names.is_match(&row.name))
                    && query
                        .min_size
                        .is_none_or(|min| row.size.is_some_and(|size| size >= min))
                    && query
                        .max_size
                        .is_none_or(|max| row.size.is_some_and(|size| size <= max))
                {
                    entries.push(row.clone());
                }
            }
        }
        let mut errors = Vec::new();
        if let Some(pattern) = &query.text {
            let (matched, skipped) =
                archives::search_text(&self.file, &entries, pattern, password, cancel)?;
            entries.retain(|entry| matched.contains(&entry.path));
            errors = skipped;
        }
        files::ensure_unchanged(&self.file, &self.metadata)?;
        Ok(files::Directory { entries, errors })
    }
}

fn index(
    entries: Vec<Item>,
    cancel: &AtomicBool,
) -> io::Result<BTreeMap<PathBuf, Vec<files::Entry>>> {
    let mut nodes = BTreeMap::<String, Item>::new();
    for entry in entries {
        files::check_cancel(cancel)?;
        for parent in entry
            .path
            .ancestors()
            .skip(1)
            .filter(|path| !path.as_os_str().is_empty())
        {
            let node = nodes.entry(fold(parent)).or_insert_with(|| Item {
                path: parent.to_path_buf(),
                directory: true,
                size: 0,
                encrypted: false,
                modified: None,
            });
            if node.path != parent || !node.directory {
                return Err(io::Error::other("Mehrdeutiger Ordnerpfad im Archiv."));
            }
        }
        if let Some(existing) = nodes.insert(fold(&entry.path), entry.clone())
            && (existing.path != entry.path || !existing.directory || !entry.directory)
        {
            return Err(io::Error::other("Mehrdeutiger Eintrag im Archiv."));
        }
        if nodes.len() > MAX_NODES {
            return Err(io::Error::other("Das Archiv enthält zu viele Einträge."));
        }
    }
    let mut folders = BTreeMap::<PathBuf, Vec<files::Entry>>::from([(PathBuf::new(), Vec::new())]);
    for item in nodes.into_values() {
        files::check_cancel(cancel)?;
        if item.directory {
            folders.entry(item.path.clone()).or_default();
        }
        if let Some(parent) = item.path.parent() {
            folders
                .entry(parent.to_path_buf())
                .or_default()
                .push(files::Entry::from_archive(
                    item.path,
                    item.directory,
                    item.size,
                    item.encrypted,
                    item.modified,
                ));
        }
    }
    Ok(folders)
}

fn fold(path: &Path) -> String {
    path.to_string_lossy().to_lowercase()
}

#[derive(Clone)]
pub struct Selection {
    folder: PathBuf,
    roots: BTreeSet<PathBuf>,
    limit: u64,
}

impl Selection {
    pub fn new(folder: PathBuf, roots: Vec<PathBuf>) -> io::Result<Self> {
        if roots.is_empty()
            || roots
                .iter()
                .any(|path| path == &folder || !path.starts_with(&folder))
        {
            return Err(io::Error::other(
                "Wähle Einträge aus dem aktuellen Archivordner.",
            ));
        }
        for path in std::iter::once(&folder).chain(&roots) {
            for component in path.components() {
                let std::path::Component::Normal(name) = component else {
                    return Err(io::Error::other("Ungültiger Archivpfad."));
                };
                files::leaf_name(name)?;
            }
        }
        Ok(Self {
            folder,
            roots: roots.into_iter().collect(),
            limit: MAX_EXTRACT,
        })
    }

    pub fn preview(file: &Path) -> io::Result<Self> {
        let parent = file
            .parent()
            .ok_or_else(|| io::Error::other("Archivdatei fehlt."))?;
        let mut selection = Self::new(parent.to_path_buf(), vec![file.to_path_buf()])?;
        selection.limit = MAX_PREVIEW;
        Ok(selection)
    }
}

pub(crate) fn project<T>(
    entries: Vec<T>,
    selection: Option<&Selection>,
    info: impl Fn(&T) -> (&Path, u64, bool),
    cancel: &AtomicBool,
) -> io::Result<Vec<(T, PathBuf)>> {
    let mut found = BTreeSet::new();
    let mut names = BTreeMap::new();
    let mut total = 0u64;
    let mut projected = Vec::new();
    for entry in entries {
        files::check_cancel(cancel)?;
        let (path, size, directory) = info(&entry);
        let output = if let Some(selection) = selection {
            let Ok(relative) = path.strip_prefix(&selection.folder) else {
                continue;
            };
            let mut include = false;
            for parent in path
                .ancestors()
                .take_while(|parent| *parent != selection.folder)
            {
                if let Some(root) = selection.roots.get(parent) {
                    found.insert(root);
                    include = true;
                }
            }
            if !include {
                continue;
            }
            if selection.limit == MAX_PREVIEW && (directory || !selection.roots.contains(path)) {
                return Err(io::Error::other(
                    "Die Vorschau benötigt genau eine Archivdatei.",
                ));
            }
            relative.to_path_buf()
        } else {
            path.to_path_buf()
        };
        register_path(&mut names, &output, directory)?;
        total = total
            .checked_add(size)
            .filter(|total| *total <= selection.map_or(MAX_EXTRACT, |selection| selection.limit))
            .ok_or_else(|| io::Error::other("Auswahl überschreitet das Größenlimit."))?;
        projected.push((entry, output));
    }
    if selection.is_some_and(|selection| selection.roots.len() != found.len()) {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "Ein ausgewählter Archiveintrag fehlt. Aktualisiere die Ansicht.",
        ));
    }
    Ok(projected)
}

pub(crate) fn register_path(
    names: &mut BTreeMap<String, (PathBuf, bool)>,
    path: &Path,
    directory: bool,
) -> io::Result<()> {
    use std::collections::btree_map::Entry;
    for part in path
        .ancestors()
        .take_while(|part| !part.as_os_str().is_empty())
    {
        let is_dir = part != path || directory;
        match names.entry(fold(part)) {
            Entry::Vacant(slot) => {
                slot.insert((part.to_path_buf(), is_dir));
            }
            Entry::Occupied(slot) if slot.get().0 == part && slot.get().1 && is_dir => (),
            Entry::Occupied(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!("Kollidierender Archivpfad: {}", part.display()),
                ));
            }
        }
    }
    if names.len() > MAX_NODES {
        return Err(io::Error::other("Das Archiv enthält zu viele Einträge."));
    }
    Ok(())
}

pub(crate) fn check_destination<T>(
    destination: &Path,
    entries: &[(T, PathBuf)],
    is_dir: impl Fn(&T) -> bool,
    cancel: &AtomicBool,
) -> io::Result<PathBuf> {
    let base = fs::canonicalize(destination)?;
    if !base.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotADirectory,
            "Ein Zielordner wird benötigt.",
        ));
    }
    let mut checked_dirs = BTreeSet::new();
    for (entry, relative) in entries {
        files::check_cancel(cancel)?;
        let mut current = base.clone();
        let mut components = relative.components().peekable();
        while let Some(component) = components.next() {
            current.push(component);
            let directory = components.peek().is_some() || is_dir(entry);
            if directory && !checked_dirs.insert(current.clone()) {
                continue;
            }
            match files::regular_metadata(&current) {
                Ok(metadata) if directory && metadata.is_dir() => (),
                Err(error) if error.kind() == io::ErrorKind::NotFound => (),
                Err(error) => return Err(error),
                Ok(_) => {
                    return Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        format!("Ziel existiert bereits: {}", current.display()),
                    ));
                }
            }
        }
    }
    Ok(base)
}

pub fn extract(
    file: &Path,
    destination: &Path,
    selection: &Selection,
    password: Option<&str>,
    cancel: &AtomicBool,
) -> io::Result<usize> {
    if is_zip(file) {
        archives::extract_zip_selection(file, destination, password, Some(selection), cancel)
    } else {
        ExternalArchiver::discover(cancel)?.extract_selection(
            file,
            destination,
            Some(selection),
            cancel,
        )
    }
}
