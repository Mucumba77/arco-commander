use crate::{archive_parts, archives, external_archives::ExternalArchiver, files, tools};
use crate::{legacy_archives, seven_zip};
use chrono::{Local, NaiveDateTime, TimeZone};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, Metadata};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::SystemTime;

const MAX_NODES: usize = 500_000;
pub(crate) const MAX_EXTRACT: u64 = 256 * 1024 * 1024 * 1024;
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
    archive_parts::kind(path).is_some_and(|ext| ext.eq_ignore_ascii_case("zip"))
        || path.extension().is_some_and(|ext| {
            ["zip", "jar", "xpi"]
                .iter()
                .any(|kind| ext.eq_ignore_ascii_case(kind))
                || (ext.eq_ignore_ascii_case("res") && is_mozilla_zip(path))
        })
}

fn is_mozilla_zip(path: &Path) -> bool {
    let mut signature = [0; 8];
    File::open(path)
        .and_then(|mut file| file.read_exact(&mut signature))
        .is_ok()
        && &signature[4..] == b"PK\x01\x02"
}

pub fn is_supported(path: &Path) -> bool {
    is_zip(path)
        || is_brotli(path)
        || (!path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("exe"))
            && legacy_archives::Format::of(path).is_some())
        || crate::external_archives::is_supported(path)
}

pub fn is_openable(path: &Path) -> bool {
    is_supported(path)
        || crate::external_archives::is_sfx(path)
        || archives::is_zip_sfx(path)
        || legacy_archives::Format::of(path).is_some()
}

fn is_zip_container(path: &Path) -> bool {
    is_zip(path) || archives::is_zip_sfx(path)
}

pub fn supports_password(path: &Path) -> bool {
    is_zip_container(path) || seven_zip::can_read(path)
}

pub fn uses_legacy_adapter(path: &Path) -> bool {
    legacy_archives::Format::of(path).is_some()
}

pub fn pack_7z(
    paths: &[PathBuf],
    base: &Path,
    target: &Path,
    password: Option<&str>,
    cancel: &AtomicBool,
) -> io::Result<()> {
    seven_zip::pack(paths, base, target, password, cancel)
}

pub fn pack_encrypted_7z_sfx(
    paths: &[PathBuf],
    base: &Path,
    target: &Path,
    password: &str,
    cancel: &AtomicBool,
) -> io::Result<()> {
    seven_zip::pack_sfx(paths, base, target, password, cancel)
}

pub fn pack_split(
    paths: &[PathBuf],
    base: &Path,
    target: &Path,
    part_size: u64,
    password: Option<&str>,
    cancel: &AtomicBool,
) -> io::Result<usize> {
    if part_size == 0 {
        return Err(io::Error::other("Die Teilgröße muss positiv sein."));
    }
    let parent = fs::canonicalize(
        target
            .parent()
            .ok_or_else(|| io::Error::other("Zielordner fehlt."))?,
    )?;
    let leaf = target
        .file_name()
        .ok_or_else(|| io::Error::other("Archivname fehlt."))?;
    files::leaf_name(leaf)?;
    for source in paths {
        if parent.starts_with(fs::canonicalize(source)?) {
            return Err(io::Error::other("Das Zielarchiv liegt im Quellordner."));
        }
    }
    let prefix = format!("{}.", leaf.to_string_lossy().to_lowercase());
    for entry in fs::read_dir(&parent)? {
        files::check_cancel(cancel)?;
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_lowercase();
        if name.strip_prefix(&prefix).is_some_and(|extension| {
            extension.len() >= 3 && extension.bytes().all(|byte| byte.is_ascii_digit())
        }) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("Archivteil existiert bereits: {}", entry.path().display()),
            ));
        }
    }
    let working = tempfile::tempdir_in(&parent)?;
    let archive = working.path().join(leaf);
    if seven_zip::is_7z(target) {
        seven_zip::pack(paths, base, &archive, password, cancel)?;
    } else if target
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("zip"))
    {
        archives::create_zip(paths, base, &archive, password, cancel)?;
    } else {
        return Err(io::Error::other(
            "ZIP oder 7z als Mehrteilerformat erforderlich.",
        ));
    }
    if fs::metadata(&archive)?.len().div_ceil(part_size) > 9_999 {
        return Err(io::Error::other(
            "Das Archiv würde mehr als 9999 Teile benötigen.",
        ));
    }
    let parts = tools::split(&archive, working.path(), part_size, cancel)?;
    test(&parts[0], password, cancel)?;
    let parts = parts
        .into_iter()
        .map(|part| {
            let target = parent.join(part.file_name().expect("split part name"));
            (part, target)
        })
        .collect();
    tools::publish_parts(parts, cancel)
}

pub fn is_editable(path: &Path) -> bool {
    archive_parts::kind(path).is_some()
        || seven_zip::is_7z(path)
        || path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("zip") || ext.eq_ignore_ascii_case("ace"))
}

fn supports_sync_write(path: &Path) -> bool {
    archive_parts::kind(path).is_some()
        || seven_zip::is_7z(path)
        || path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("zip"))
}

pub fn supports_encrypted_edit(path: &Path) -> bool {
    archive_parts::kind(path).is_some()
        || seven_zip::is_7z(path)
        || path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("zip"))
}

pub fn needs_header_password(error: &io::Error) -> bool {
    error
        .get_ref()
        .is_some_and(|error| error.is::<seven_zip::HeaderPassword>())
}

pub fn edit(
    file: &Path,
    change: archives::ZipEdit,
    password: Option<&str>,
    cancel: &AtomicBool,
) -> io::Result<usize> {
    if archive_parts::kind(file).is_some() {
        archive_parts::rewrite(file, password, &[], cancel, |joined| {
            edit(joined, change, password, cancel)
        })
    } else if seven_zip::is_7z(file) {
        seven_zip::edit(file, change, password, cancel)
    } else if matches!(
        legacy_archives::Format::of(file),
        Some(legacy_archives::Format::Ace)
    ) {
        crate::ace::edit(file, change, cancel)
    } else {
        archives::edit_zip_with_password(file, change, password, cancel)
    }
}

pub fn append(
    file: &Path,
    folder: &Path,
    paths: &[PathBuf],
    base: &Path,
    password: Option<&str>,
    cancel: &AtomicBool,
) -> io::Result<usize> {
    if archive_parts::kind(file).is_some() {
        archive_parts::rewrite(file, password, paths, cancel, |joined| {
            append(joined, folder, paths, base, password, cancel)
        })
    } else if seven_zip::is_7z(file) {
        seven_zip::append(file, folder, paths, base, password, cancel)
    } else if matches!(
        legacy_archives::Format::of(file),
        Some(legacy_archives::Format::Ace)
    ) {
        crate::ace::append(file, folder, paths, base, cancel)
    } else {
        archives::append_zip(file, folder, paths, base, password, cancel)
    }
}

pub fn supports_text_search(path: &Path) -> bool {
    is_zip_container(path) || is_brotli(path) || seven_zip::can_read(path)
}

pub fn is_brotli(path: &Path) -> bool {
    path.extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("br") || ext.eq_ignore_ascii_case("tbr"))
}

fn brotli_path(file: &Path) -> io::Result<PathBuf> {
    let stem = file
        .file_stem()
        .ok_or_else(|| io::Error::other("Dateiname im Brotli-Archiv fehlt."))?;
    files::leaf_name(stem)?;
    let mut name = stem.to_os_string();
    if file
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("tbr"))
    {
        name.push(".tar");
    }
    Ok(PathBuf::from(name))
}

fn decode_brotli(
    file: &Path,
    expected: &Metadata,
    output: impl Write,
    limit: u64,
    cancel: &AtomicBool,
) -> io::Result<u64> {
    files::check_cancel(cancel)?;
    let reader = brotli_decompressor::Decompressor::new(File::open(file)?, 64 * 1024);
    let size = tools::copy_cancelled(reader.take(limit + 1), output, cancel)?;
    if size > limit {
        return Err(io::Error::other(
            "Brotli-Daten überschreiten das Größenlimit.",
        ));
    }
    files::ensure_unchanged(file, expected)?;
    Ok(size)
}

#[derive(Clone, Debug)]
pub struct Item {
    pub path: PathBuf,
    pub directory: bool,
    pub size: u64,
    pub encrypted: bool,
    pub modified: Option<SystemTime>,
}

pub struct WriteConflict<'a> {
    pub source: &'a Path,
    pub source_metadata: &'a Metadata,
    pub target: &'a files::Entry,
}

pub struct Catalog {
    file: PathBuf,
    metadata: Metadata,
    volumes: Vec<(PathBuf, Metadata)>,
    folders: BTreeMap<PathBuf, Vec<files::Entry>>,
    encrypted: bool,
    header_encrypted: bool,
}

impl Catalog {
    pub fn read(file: &Path, cancel: &AtomicBool) -> io::Result<Self> {
        Self::read_with_password(file, None, cancel)
    }

    pub fn read_with_password(
        file: &Path,
        password: Option<&str>,
        cancel: &AtomicBool,
    ) -> io::Result<Self> {
        let metadata = files::regular_metadata(file)?;
        if !metadata.is_file() {
            return Err(io::Error::other(
                "Das Archiv muss eine reguläre Datei sein.",
            ));
        }
        let volumes = if archive_parts::kind(file).is_some() {
            archive_parts::following(file, cancel)?
        } else {
            crate::uc2_volumes::snapshot(file)?
        };
        let mut header_encrypted = false;
        let entries: Vec<_> = if is_zip_container(file) {
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
        } else if seven_zip::can_read(file) {
            let (items, protected) = seven_zip::list(file, password, cancel)?;
            header_encrypted = protected;
            items
        } else if let Some(format) = legacy_archives::Format::of(file) {
            format.list(file, cancel)?
        } else if is_brotli(file) {
            let path = brotli_path(file)?;
            vec![Item {
                path,
                directory: false,
                size: decode_brotli(file, &metadata, io::sink(), MAX_EXTRACT, cancel)?,
                encrypted: false,
                modified: None,
            }]
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
        let catalog = Self {
            file: file.to_path_buf(),
            metadata,
            volumes,
            folders,
            encrypted,
            header_encrypted,
        };
        catalog.check_unchanged()?;
        Ok(catalog)
    }

    pub fn unchanged(&self, file: &Path) -> bool {
        self.file == file && self.check_unchanged().is_ok()
    }

    pub fn storage_paths(&self) -> impl Iterator<Item = &Path> {
        std::iter::once(self.file.as_path())
            .chain(self.volumes.iter().map(|(path, _)| path.as_path()))
    }

    pub fn can_sync_write(&self) -> bool {
        supports_sync_write(&self.file)
            && !self.metadata.permissions().readonly()
            && self
                .volumes
                .iter()
                .all(|(_, metadata)| !metadata.permissions().readonly())
    }

    pub fn has_directory(&self, path: &Path) -> bool {
        self.folders.contains_key(path)
    }

    pub fn append_resolving(
        &self,
        folder: &Path,
        sources: &[PathBuf],
        base: &Path,
        password: Option<&str>,
        cancel: &AtomicBool,
        mut resolve: impl FnMut(WriteConflict<'_>) -> io::Result<files::ConflictDecision>,
    ) -> io::Result<files::TransferReport> {
        use files::{Conflict, ConflictDecision, Kind};
        self.check_unchanged()?;
        if !self.can_sync_write() || !self.has_directory(folder) {
            return Err(io::Error::other(
                "Der Archivordner ist für Änderungen gesperrt.",
            ));
        }
        let storage_paths: Vec<_> = self
            .storage_paths()
            .map(fs::canonicalize)
            .collect::<io::Result<_>>()?;
        for source in sources {
            let source = fs::canonicalize(source)?;
            if storage_paths.iter().any(|path| path.starts_with(&source)) {
                return Err(io::Error::other(
                    "Ein Archiv kann sich nicht selbst enthalten.",
                ));
            }
        }
        let plan = archives::plan_sources(sources, base, Path::new(""), cancel)?;
        let existing: BTreeMap<_, _> = self
            .folders
            .values()
            .flatten()
            .map(|entry| (fold(&entry.path), entry))
            .collect();
        let mut occupied: BTreeSet<_> = existing.keys().cloned().collect();
        occupied.extend(plan.iter().map(|(_, name, _)| {
            let relative: PathBuf = Path::new(name).components().collect();
            fold(&folder.join(relative))
        }));
        let staging = tempfile::tempdir()?;
        let mut mapped = BTreeMap::<PathBuf, Option<PathBuf>>::new();
        let mut additions = BTreeSet::new();
        let mut removals = Vec::new();
        let mut report = files::TransferReport::default();
        for (source, name, metadata) in plan {
            files::check_cancel(cancel)?;
            let relative: PathBuf = Path::new(&name).components().collect();
            let mut destination = folder.to_path_buf();
            let mut parent = relative.parent().expect("validated archive entry");
            let mut skipped = false;
            loop {
                if let Some(redirect) = mapped.get(parent) {
                    match redirect {
                        Some(redirect) => {
                            destination = redirect
                                .join(relative.strip_prefix(parent).map_err(io::Error::other)?)
                        }
                        None => skipped = true,
                    }
                    break;
                }
                let Some(ancestor) = parent.parent() else {
                    destination.push(&relative);
                    break;
                };
                parent = ancestor;
            }
            if skipped {
                report.skipped += 1;
                continue;
            }
            let mut append_to = None;
            if let Some(&target) = existing.get(&fold(&destination)) {
                if metadata.is_dir() && target.kind == Kind::Directory {
                    mapped.insert(relative, Some(target.path.clone()));
                    continue;
                }
                let decision = resolve(WriteConflict {
                    source: &source,
                    source_metadata: &metadata,
                    target,
                })?;
                files::check_cancel(cancel)?;
                match decision {
                    ConflictDecision::RenameTo(name) => {
                        let candidate = destination.with_file_name(files::leaf_name(&name)?);
                        if !occupied.insert(fold(&candidate)) {
                            return Err(io::Error::new(
                                io::ErrorKind::AlreadyExists,
                                "Der gewählte Archivname ist bereits vorhanden oder vorgemerkt.",
                            ));
                        }
                        destination = candidate;
                    }
                    ConflictDecision::Apply(Conflict::Rename) => {
                        let mut free = None;
                        for index in 2..=10_000 {
                            let candidate = files::numbered_copy_name(&destination, index)?;
                            if occupied.insert(fold(&candidate)) {
                                free = Some(candidate);
                                break;
                            }
                        }
                        destination = free
                            .ok_or_else(|| io::Error::other("Kein freier Archivname gefunden."))?;
                    }
                    ConflictDecision::Apply(
                        rule @ (Conflict::Replace
                        | Conflict::Newer
                        | Conflict::Larger
                        | Conflict::Append),
                    ) if metadata.is_file() && target.kind == Kind::File => {
                        if rule == Conflict::Newer
                            && !metadata
                                .modified()
                                .ok()
                                .zip(target.modified)
                                .is_some_and(|(from, to)| from > to)
                            || rule == Conflict::Larger
                                && target.size.is_some_and(|size| metadata.len() <= size)
                        {
                            report.skipped += 1;
                            continue;
                        }
                        destination = target.path.clone();
                        removals.push(target.path.clone());
                        if rule == Conflict::Append {
                            append_to = Some(target);
                        }
                    }
                    ConflictDecision::Apply(Conflict::Skip) => {
                        if metadata.is_dir() {
                            mapped.insert(relative, None);
                        }
                        report.skipped += 1;
                        continue;
                    }
                    _ => return Err(io::ErrorKind::AlreadyExists.into()),
                }
            }
            let output = staging.path().join(&destination);
            if metadata.is_dir() {
                fs::create_dir_all(&output)?;
                mapped.insert(relative, Some(destination));
            } else {
                let parent = output.parent().expect("validated archive path");
                fs::create_dir_all(parent)?;
                if let Some(previous) = append_to {
                    let selection = Selection::new(
                        previous
                            .path
                            .parent()
                            .expect("validated archive entry")
                            .to_path_buf(),
                        vec![previous.path.clone()],
                    )?;
                    extract(&self.file, parent, &selection, password, cancel)?;
                    self.check_unchanged()?;
                    let staged = files::regular_metadata(&output)?;
                    if previous.size.is_some_and(|size| size != staged.len()) {
                        return Err(io::Error::other(
                            "Die Archiveintragsgröße hat sich geändert.",
                        ));
                    }
                    files::append_file_checked(&source, &output, &metadata, &staged, cancel)?;
                } else {
                    files::copy_file_checked(
                        &source,
                        &output,
                        &metadata,
                        None,
                        cancel,
                        &mut |_| {},
                    )?;
                }
            }
            additions.insert(output);
            report.completed += 1;
        }
        let additions: Vec<_> = additions.into_iter().collect();
        self.apply_sync_changes(removals, &additions, staging.path(), password, cancel)?;
        Ok(report)
    }

    pub fn apply_sync_changes(
        &self,
        removals: Vec<PathBuf>,
        additions: &[PathBuf],
        base: &Path,
        password: Option<&str>,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        self.check_unchanged()?;
        if removals.is_empty() && additions.is_empty() {
            return Ok(());
        }
        if !self.can_sync_write() {
            return Err(io::Error::other(
                "Der Archivsatz ist für Änderungen gesperrt.",
            ));
        }
        if removals.iter().any(|path| self.has_directory(path)) {
            let selected: BTreeSet<_> = removals.iter().map(PathBuf::as_path).collect();
            for row in self.folders.values().flatten() {
                files::check_cancel(cancel)?;
                if !selected.contains(row.path.as_path())
                    && row
                        .path
                        .ancestors()
                        .skip(1)
                        .any(|parent| selected.contains(parent))
                {
                    return Err(io::Error::other(format!(
                        "Der Archivordner enthält einen Eintrag ohne Löschfreigabe: {}",
                        row.path.display()
                    )));
                }
            }
        }
        let modify = |archive: &Path| -> io::Result<usize> {
            self.check_unchanged()?;
            let changed = if seven_zip::is_7z(archive) {
                seven_zip::sync_changes(archive, removals, additions, base, password, cancel)?
            } else {
                archives::sync_zip_changes(archive, removals, additions, base, password, cancel)?
            };
            self.check_unchanged()?;
            Ok(changed)
        };
        if archive_parts::kind(&self.file).is_some() {
            archive_parts::rewrite(&self.file, password, additions, cancel, modify)?;
            return Ok(());
        }
        let target = fs::canonicalize(&self.file)?;
        for source in additions {
            files::check_cancel(cancel)?;
            if target.starts_with(fs::canonicalize(source)?) {
                return Err(io::Error::other(
                    "Ein Archiv kann sich nicht selbst enthalten.",
                ));
            }
        }
        let parent = self
            .file
            .parent()
            .ok_or_else(|| io::Error::other("Archivordner fehlt."))?;
        let working = tempfile::tempdir_in(parent)?;
        let staged = working.path().join(
            self.file
                .file_name()
                .ok_or_else(|| io::Error::other("Archivname fehlt."))?,
        );
        let mut output = File::create(&staged)?;
        let size = tools::copy_cancelled(
            File::open(&self.file)?.take(self.metadata.len().saturating_add(1)),
            &mut output,
            cancel,
        )?;
        if size != self.metadata.len() {
            return Err(io::Error::other("Die Archivgröße hat sich geändert."));
        }
        drop(output);
        test(&staged, password, cancel)?;
        modify(&staged)?;
        test(&staged, password, cancel)?;
        crate::archive_zone::ArchiveZone::read(&self.file, cancel)?.apply(&staged)?;
        fs::set_permissions(&staged, self.metadata.permissions())?;
        files::check_cancel(cancel)?;
        self.check_unchanged()?;
        tempfile::TempPath::try_from_path(staged)?
            .persist(&self.file)
            .map_err(|error| error.error)?;
        Ok(())
    }

    pub fn check_unchanged(&self) -> io::Result<()> {
        files::ensure_unchanged(&self.file, &self.metadata)?;
        for (path, metadata) in &self.volumes {
            files::ensure_unchanged(path, metadata)?;
        }
        if archive_parts::kind(&self.file).is_some()
            && archive_parts::part_path(&self.file, self.volumes.len() + 2).try_exists()?
        {
            return Err(io::Error::other(
                "Ein neues Archivsegment ist hinzugekommen.",
            ));
        }
        Ok(())
    }

    pub fn contains_encrypted_entries(&self) -> bool {
        self.encrypted || self.header_encrypted
    }

    pub fn header_encrypted(&self) -> bool {
        self.header_encrypted
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
        self.check_unchanged()?;
        if query.text.is_some() && !supports_text_search(&self.file) {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Textsuche ist für dieses Archivformat nicht verfügbar.",
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
            if is_brotli(&self.file) {
                if let Some(entry) = entries.first() {
                    if entry.size.is_some_and(|size| size > MAX_PREVIEW) {
                        errors.push(format!(
                            "{} überschreitet das Suchlimit von 64 MiB.",
                            entry.path.display()
                        ));
                        entries.clear();
                    } else {
                        let mut data = Vec::new();
                        decode_brotli(&self.file, &self.metadata, &mut data, MAX_PREVIEW, cancel)?;
                        if !pattern.is_match(&tools::decode_text(&data)) {
                            entries.clear();
                        }
                    }
                }
            } else if seven_zip::can_read(&self.file) {
                let (matched, skipped) =
                    seven_zip::search_text(&self.file, &entries, pattern, password, cancel)?;
                entries.retain(|entry| matched.contains(&entry.path));
                errors = skipped;
            } else {
                let (matched, skipped) =
                    archives::search_text(&self.file, &entries, pattern, password, cancel)?;
                entries.retain(|entry| matched.contains(&entry.path));
                errors = skipped;
            }
        }
        self.check_unchanged()?;
        Ok(files::Directory { entries, errors })
    }

    pub fn search_candidates(
        &self,
        roots: &[PathBuf],
        query: &crate::advanced_search::Query,
        show_hidden: bool,
        cancel: &AtomicBool,
        mut inspect: impl FnMut(&files::Entry) -> io::Result<bool>,
    ) -> io::Result<crate::advanced_search::Found> {
        self.check_unchanged()?;
        for root in roots {
            if !self.folders.contains_key(root)
                && !root
                    .parent()
                    .and_then(|parent| self.folders.get(parent))
                    .is_some_and(|rows| rows.iter().any(|row| row.path == *root))
            {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("Suchort fehlt im Archiv: {}", root.display()),
                ));
            }
        }
        let mut entries = Vec::new();
        let mut errors = Vec::new();
        let mut interrupted = false;
        'folders: for (parent, rows) in &self.folders {
            for row in rows {
                if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                    break 'folders;
                }
                let in_scope = roots.iter().any(|root| {
                    row.path == *root && row.kind == files::Kind::File
                        || parent.strip_prefix(root).ok().is_some_and(|relative| {
                            query
                                .max_depth
                                .is_none_or(|depth| relative.components().count() <= depth)
                        })
                });
                if !in_scope || !(show_hidden || !row.hidden) || !query.matches_metadata(row) {
                    continue;
                }
                match inspect(row) {
                    Ok(true) => entries.push(row.clone()),
                    Ok(false) => (),
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                        interrupted = true;
                        break 'folders;
                    }
                    Err(error) if errors.len() < 100 => {
                        errors.push(format!("{}: {error}", row.path.display()))
                    }
                    Err(_) => (),
                }
            }
        }
        self.check_unchanged()?;
        Ok(crate::advanced_search::Found {
            directory: files::Directory { entries, errors },
            cancelled: interrupted || cancel.load(std::sync::atomic::Ordering::Relaxed),
        })
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
        Self::preview_bounded(file, MAX_PREVIEW)
    }

    pub fn preview_bounded(file: &Path, limit: u64) -> io::Result<Self> {
        let parent = file
            .parent()
            .ok_or_else(|| io::Error::other("Archivdatei fehlt."))?;
        let mut selection = Self::new(parent.to_path_buf(), vec![file.to_path_buf()])?;
        selection.limit = limit.min(MAX_PREVIEW);
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
    extract_with_progress(file, destination, selection, password, cancel, |_| {})
}

pub fn extract_with_progress(
    file: &Path,
    destination: &Path,
    selection: &Selection,
    password: Option<&str>,
    cancel: &AtomicBool,
    progress: impl FnMut(files::Progress<'_>),
) -> io::Result<usize> {
    if is_zip_container(file) {
        archives::extract_zip_selection_with_progress(
            file,
            destination,
            password,
            Some(selection),
            cancel,
            progress,
        )
    } else if seven_zip::can_read(file) {
        seven_zip::extract(file, destination, Some(selection), password, cancel)
    } else if let Some(format) = legacy_archives::Format::of(file) {
        format.extract(file, destination, Some(selection), cancel)
    } else if is_brotli(file) {
        extract_brotli(file, destination, Some(selection), cancel)
    } else {
        ExternalArchiver::discover(cancel)?.extract_selection(
            file,
            destination,
            Some(selection),
            cancel,
        )
    }
}

pub fn test(file: &Path, password: Option<&str>, cancel: &AtomicBool) -> io::Result<usize> {
    if is_zip_container(file) {
        archives::test_zip(file, password, cancel)
    } else if seven_zip::can_read(file) {
        seven_zip::test(file, password, cancel)
    } else if let Some(format) = legacy_archives::Format::of(file) {
        format.test(file, cancel)
    } else if is_brotli(file) {
        let metadata = files::regular_metadata(file)?;
        decode_brotli(file, &metadata, io::sink(), MAX_EXTRACT, cancel)?;
        Ok(1)
    } else {
        ExternalArchiver::discover(cancel)?.test(file, cancel)
    }
}

pub fn extract_all(
    file: &Path,
    destination: &Path,
    password: Option<&str>,
    cancel: &AtomicBool,
) -> io::Result<usize> {
    extract_all_with_progress(file, destination, password, cancel, |_| {})
}

pub fn extract_all_with_progress(
    file: &Path,
    destination: &Path,
    password: Option<&str>,
    cancel: &AtomicBool,
    progress: impl FnMut(files::Progress<'_>),
) -> io::Result<usize> {
    if is_zip_container(file) {
        archives::extract_zip_selection_with_progress(
            file,
            destination,
            password,
            None,
            cancel,
            progress,
        )
    } else if seven_zip::can_read(file) {
        seven_zip::extract(file, destination, None, password, cancel)
    } else if let Some(format) = legacy_archives::Format::of(file) {
        format.extract(file, destination, None, cancel)
    } else if is_brotli(file) {
        extract_brotli(file, destination, None, cancel)
    } else {
        ExternalArchiver::discover(cancel)?.extract(file, destination, cancel)
    }
}

fn extract_brotli(
    file: &Path,
    destination: &Path,
    selection: Option<&Selection>,
    cancel: &AtomicBool,
) -> io::Result<usize> {
    let metadata = files::regular_metadata(file)?;
    let entries = project(
        vec![brotli_path(file)?],
        selection,
        |path| (path.as_path(), 0, false),
        cancel,
    )?;
    let base = check_destination(destination, &entries, |_| false, cancel)?;
    let zone = crate::archive_zone::ArchiveZone::read(file, cancel)?;
    let mut staged = tempfile::NamedTempFile::new_in(&base)?;
    decode_brotli(
        file,
        &metadata,
        &mut staged,
        selection.map_or(MAX_EXTRACT, |selection| selection.limit),
        cancel,
    )?;
    zone.apply(staged.path())?;
    staged.as_file().sync_all()?;
    files::check_cancel(cancel)?;
    staged
        .persist_noclobber(base.join(&entries[0].1))
        .map_err(|error| error.error)?;
    Ok(1)
}

#[cfg(test)]
mod tests {
    #[test]
    fn archive_write_conflicts_keep_skipped_members_and_publish_the_delta_once() {
        use super::*;
        let fixture = tempfile::tempdir().unwrap();
        let base = fixture.path().join("input");
        let docs = base.join("docs");
        fs::create_dir_all(&docs).unwrap();
        for name in [
            "append.txt",
            "auto.txt",
            "keep.txt",
            "rename.txt",
            "replace.txt",
        ] {
            fs::write(docs.join(name), b"original").unwrap();
        }
        let archive = fixture.path().join("sample.zip");
        let cancel = AtomicBool::new(false);
        archives::create_zip(std::slice::from_ref(&docs), &base, &archive, None, &cancel).unwrap();
        let catalog = Catalog::read(&archive, &cancel).unwrap();
        for name in [
            "append.txt",
            "auto.txt",
            "keep.txt",
            "rename.txt",
            "replace.txt",
            "new.txt",
        ] {
            fs::write(docs.join(name), b"changed contents").unwrap();
        }
        let initial = fs::read(&archive).unwrap();
        let sources = std::slice::from_ref(&docs);
        let aborted =
            catalog.append_resolving(Path::new(""), sources, &base, None, &cancel, |_| {
                Err(io::ErrorKind::Interrupted.into())
            });
        assert_eq!(aborted.unwrap_err().kind(), io::ErrorKind::Interrupted);
        assert_eq!(fs::read(&archive).unwrap(), initial);
        let occupied =
            catalog.append_resolving(Path::new(""), sources, &base, None, &cancel, |_| {
                Ok(files::ConflictDecision::RenameTo("new.txt".into()))
            });
        assert_eq!(occupied.unwrap_err().kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&archive).unwrap(), initial);
        let report = catalog
            .append_resolving(Path::new(""), sources, &base, None, &cancel, |conflict| {
                Ok(match conflict.target.name.to_str().unwrap() {
                    "append.txt" => files::Conflict::Append.into(),
                    "auto.txt" => files::Conflict::Rename.into(),
                    "rename.txt" => files::ConflictDecision::RenameTo("Kopie ä.txt".into()),
                    "replace.txt" => files::Conflict::Replace.into(),
                    _ => files::Conflict::Skip.into(),
                })
            })
            .unwrap();
        assert_eq!((report.completed, report.skipped), (5, 1));
        let output = fixture.path().join("output");
        fs::create_dir(&output).unwrap();
        extract_all(&archive, &output, None, &cancel).unwrap();
        for (name, expected) in [
            ("append.txt", "originalchanged contents"),
            ("auto.txt", "original"),
            ("auto (2).txt", "changed contents"),
            ("keep.txt", "original"),
            ("rename.txt", "original"),
            ("Kopie ä.txt", "changed contents"),
            ("replace.txt", "changed contents"),
            ("new.txt", "changed contents"),
        ] {
            assert_eq!(
                fs::read_to_string(output.join("docs").join(name)).unwrap(),
                expected
            );
        }
    }

    use super::*;
    use zip::HasZipMetadata;

    #[test]
    fn encrypted_segments_round_trip_without_external_archiver() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path();
        let file = root.join("Größe.txt");
        let data = b"Arco Mehrteiler\n".repeat(400);
        fs::write(&file, &data).unwrap();
        let password = "fixture secret";
        let cancel = AtomicBool::new(false);
        for format in ["zip", "7z"] {
            let archive = root.join(format!("probe.{format}"));
            let first = root.join(format!("probe.{format}.001"));
            let parts = pack_split(
                std::slice::from_ref(&file),
                root,
                &archive,
                61,
                Some(password),
                &cancel,
            )
            .unwrap();
            assert!(parts > 2);
            assert!(!archive.exists());
            assert!(supports_password(&first));
            assert!(is_editable(&first));
            assert!(test(&first, None, &cancel).is_err());
            assert!(test(&first, Some("incorrect"), &cancel).is_err());
            assert_eq!(test(&first, Some(password), &cancel).unwrap(), 1);
            if format == "7z" {
                let error = Catalog::read(&first, &cancel).err().unwrap();
                assert!(needs_header_password(&error));
            }
            let catalog = Catalog::read_with_password(&first, Some(password), &cancel).unwrap();
            assert_eq!(catalog.header_encrypted(), format == "7z");
            assert!(catalog.contains_encrypted_entries());
            let query = tools::Search {
                names: files::compile_masks("*.txt").unwrap(),
                text: Some(regex::Regex::new("Arco Mehrteiler").unwrap()),
                min_size: None,
                max_size: None,
                max_depth: None,
                show_hidden: false,
                ignore: None,
            };
            assert_eq!(
                catalog
                    .search(Path::new(""), &query, Some(password), &cancel)
                    .unwrap()
                    .entries
                    .len(),
                1
            );
            let output = tempfile::tempdir().unwrap();
            let selection = Selection::preview(Path::new("Größe.txt")).unwrap();
            assert!(
                extract(
                    &first,
                    output.path(),
                    &selection,
                    Some("incorrect"),
                    &cancel
                )
                .is_err()
            );
            let target = output.path().join("Größe.txt");
            assert!(!target.exists());
            assert_eq!(
                extract(&first, output.path(), &selection, Some(password), &cancel).unwrap(),
                1
            );
            assert_eq!(fs::read(&target).unwrap(), data);
            assert!(extract_all(&first, output.path(), Some(password), &cancel).is_err());
            assert!(
                pack_split(
                    std::slice::from_ref(&file),
                    root,
                    &archive,
                    61,
                    Some(password),
                    &cancel,
                )
                .is_err()
            );
            let original = fs::read(&first).unwrap();
            let rename = || archives::ZipEdit::Rename("Größe.txt".into(), "Neu.txt".into());
            assert!(edit(&first, rename(), Some("incorrect"), &cancel).is_err());
            assert_eq!(fs::read(&first).unwrap(), original);
            assert_eq!(edit(&first, rename(), Some(password), &cancel).unwrap(), 1);
            assert!(!catalog.unchanged(&first));
            assert_eq!(
                append(
                    &first,
                    Path::new(""),
                    std::slice::from_ref(&file),
                    root,
                    Some(password),
                    &cancel
                )
                .unwrap(),
                1
            );
            assert_eq!(
                edit(
                    &first,
                    archives::ZipEdit::Remove(vec!["Größe.txt".into()]),
                    Some(password),
                    &cancel
                )
                .unwrap(),
                1
            );
            let catalog = Catalog::read_with_password(&first, Some(password), &cancel).unwrap();
            assert!(catalog.contains_encrypted_entries());
            assert_eq!(catalog.header_encrypted(), format == "7z");
            assert_eq!(test(&first, Some(password), &cancel).unwrap(), 1);
            assert_eq!(
                extract_all(&first, output.path(), Some(password), &cancel).unwrap(),
                1
            );
            assert_eq!(fs::read(output.path().join("Neu.txt")).unwrap(), data);
            fs::remove_file(archive_parts::part_path(&first, 2)).unwrap();
            assert!(!catalog.unchanged(&first));
            assert!(test(&first, Some(password), &cancel).is_err());
        }
    }

    #[test]
    fn brotli_catalog_preview_and_integrity() {
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path().join("Grüße.txt.br");
        let compressed = [
            139, 9, 128, 65, 114, 99, 111, 32, 66, 114, 111, 116, 108, 105, 32, 102, 105, 120, 116,
            117, 114, 101, 10, 3,
        ];
        let data = b"Arco Brotli fixture\n";
        fs::write(&path, compressed).unwrap();
        let cancel = AtomicBool::new(false);
        let catalog = Catalog::read(&path, &cancel).unwrap();
        let rows = catalog.rows(Path::new("")).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].path, Path::new("Grüße.txt"));
        assert_eq!(rows[0].size, Some(data.len() as u64));
        assert_eq!(test(&path, None, &cancel).unwrap(), 1);
        let query = tools::Search {
            names: files::compile_masks("*.txt").unwrap(),
            text: Some(regex::Regex::new("Brotli").unwrap()),
            min_size: None,
            max_size: None,
            max_depth: None,
            show_hidden: false,
            ignore: None,
        };
        assert_eq!(
            catalog
                .search(Path::new(""), &query, None, &cancel)
                .unwrap()
                .entries
                .len(),
            1
        );
        let selection = Selection::preview(&rows[0].path).unwrap();
        assert_eq!(
            extract(&path, fixture.path(), &selection, None, &cancel).unwrap(),
            1
        );
        let target = fixture.path().join(&rows[0].path);
        assert_eq!(fs::read(&target).unwrap(), data);
        assert!(extract_all(&path, fixture.path(), None, &cancel).is_err());
        assert_eq!(fs::read(&target).unwrap(), data);
        fs::remove_file(&target).unwrap();
        fs::write(&path, &compressed[..compressed.len() - 1]).unwrap();
        assert!(test(&path, None, &cancel).is_err());
        assert!(extract_all(&path, fixture.path(), None, &cancel).is_err());
        assert!(!target.exists());
    }

    #[test]
    fn sync_preserves_zipcrypto_and_refuses_unapproved_or_stale_removals() {
        let fixture = tempfile::tempdir().unwrap();
        let source = fixture.path().join("source");
        let folder = source.join("docs");
        fs::create_dir_all(&folder).unwrap();
        let file = folder.join("only.txt");
        fs::write(&file, b"old").unwrap();
        let archive = fixture.path().join("archive.zip");
        let cancel = AtomicBool::new(false);
        let password = "test fixture password";
        archives::create_zip_zipcrypto(&[folder], &source, &archive, password, &cancel).unwrap();
        let catalog = Catalog::read(&archive, &cancel).unwrap();
        let before = fs::read(&archive).unwrap();
        assert!(
            catalog
                .apply_sync_changes(vec!["docs".into()], &[], &source, Some(password), &cancel)
                .is_err()
        );
        assert_eq!(fs::read(&archive).unwrap(), before);
        fs::write(&file, b"new").unwrap();
        let member = PathBuf::from("docs").join("only.txt");
        let apply = |password| {
            catalog.apply_sync_changes(
                vec![member.clone()],
                std::slice::from_ref(&file),
                &source,
                Some(password),
                &cancel,
            )
        };
        assert!(apply("wrong password").is_err());
        assert_eq!(fs::read(&archive).unwrap(), before);
        apply(password).unwrap();
        {
            let mut zip = zip::ZipArchive::new(File::open(&archive).unwrap()).unwrap();
            let item = zip.by_index_raw(1).unwrap();
            assert_eq!(item.name().unwrap(), "docs/only.txt");
            assert!(item.encrypted());
            assert!(item.get_metadata().aes_settings().is_none());
        }
        let after = fs::read(&archive).unwrap();
        assert_ne!(before, after);
        assert!(apply(password).is_err());
        assert_eq!(fs::read(&archive).unwrap(), after);
    }
}
