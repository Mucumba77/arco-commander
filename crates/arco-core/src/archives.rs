use crate::{files, tools};
use chrono::{DateTime, Datelike, Local, NaiveDate, Timelike};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs::{self, File, Metadata};
use std::io::{self, BufReader, Read, Seek};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::SystemTime;
use tempfile::NamedTempFile;
use walkdir::WalkDir;
use zip::{AesMode, CompressionMethod, ZipArchive, ZipWriter, write::SimpleFileOptions};

const ENTRY_LIMIT: usize = 500_000;
const EXTRACT_LIMIT: u64 = 256 * 1024 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct Member {
    index: usize,
    pub path: PathBuf,
    pub directory: bool,
    pub encrypted: bool,
    pub size: u64,
    pub modified: Option<SystemTime>,
}

pub(crate) fn list_zip(path: &Path, cancel: &AtomicBool) -> io::Result<Vec<Member>> {
    let mut archive = ZipArchive::new(BufReader::new(File::open(path)?))?;
    members(&mut archive, cancel)
}

fn members(
    archive: &mut ZipArchive<impl Read + Seek>,
    cancel: &AtomicBool,
) -> io::Result<Vec<Member>> {
    if archive.len() > ENTRY_LIMIT {
        return Err(io::Error::other("too many archive entries"));
    }
    let mut entries = Vec::new();
    let mut names = BTreeMap::new();
    for index in 0..archive.len() {
        files::check_cancel(cancel)?;
        let file = archive.by_index_raw(index)?;
        let name = file.name()?.replace('\\', "/");
        let name = name.strip_suffix('/').unwrap_or(&name);
        if name.is_empty()
            || file
                .unix_mode()
                .is_some_and(|mode| mode & 0o170000 == 0o120000)
        {
            return Err(io::Error::other(
                "archive contains an empty name or a symbolic link",
            ));
        }
        let path = checked_archive_path(name)?;
        let key = name.to_lowercase();
        if names.insert(key, file.is_dir()).is_some() {
            return Err(io::Error::other(format!("ambiguous archive entry: {name}")));
        }
        entries.push(Member {
            index,
            path,
            directory: file.is_dir(),
            encrypted: file.encrypted(),
            size: file.size(),
            modified: file.last_modified().and_then(zip_timestamp),
        });
    }
    for key in names.keys() {
        files::check_cancel(cancel)?;
        for (split, _) in key.match_indices('/') {
            if names.get(&key[..split]) == Some(&false) {
                return Err(io::Error::other(format!(
                    "a file is also an archive directory: {key}"
                )));
            }
        }
    }
    Ok(entries)
}

fn zip_timestamp(stamp: zip::DateTime) -> Option<SystemTime> {
    let date = NaiveDate::from_ymd_opt(
        stamp.year().into(),
        stamp.month().into(),
        stamp.day().into(),
    )?;
    let time = date.and_hms_opt(
        stamp.hour().into(),
        stamp.minute().into(),
        stamp.second().into(),
    )?;
    crate::archive_view::local_timestamp(time)
}

fn zip_date(time: SystemTime) -> Option<zip::DateTime> {
    let time = DateTime::<Local>::from(time);
    zip::DateTime::from_date_and_time(
        time.year().try_into().ok()?,
        time.month() as u8,
        time.day() as u8,
        time.hour() as u8,
        time.minute() as u8,
        time.second() as u8,
    )
    .ok()
}

pub(crate) fn checked_archive_path(name: &str) -> io::Result<PathBuf> {
    let mut path = PathBuf::new();
    for component in name.split('/') {
        path.push(files::leaf_name(OsStr::new(component))?);
    }
    Ok(path)
}

pub fn create_zip(
    paths: &[PathBuf],
    base: &Path,
    target: &Path,
    password: Option<&str>,
    cancel: &AtomicBool,
) -> io::Result<()> {
    let plan = plan_sources(paths, base, Path::new(""), cancel)?;
    let parent = target
        .parent()
        .ok_or_else(|| io::Error::other("missing target directory"))?;
    let mut staging = NamedTempFile::new_in(parent)?;
    let mut archive = ZipWriter::new(staging.as_file_mut());
    write_plan(&mut archive, plan, password, cancel)?;
    archive.finish()?.sync_all()?;
    files::check_cancel(cancel)?;
    staging
        .persist_noclobber(target)
        .map_err(|error| error.error)?;
    Ok(())
}

pub fn append_zip(
    path: &Path,
    folder: &Path,
    paths: &[PathBuf],
    base: &Path,
    password: Option<&str>,
    cancel: &AtomicBool,
) -> io::Result<usize> {
    files::check_cancel(cancel)?;
    let original = writable_zip(path)?;
    let target = fs::canonicalize(path)?;
    for source in paths {
        if target.starts_with(fs::canonicalize(source)?) {
            return Err(io::Error::other(
                "Ein ZIP kann sich nicht selbst enthalten.",
            ));
        }
    }
    let input = File::open(path)?;
    let mut archive = ZipArchive::new(BufReader::new(input))?;
    let existing = members(&mut archive, cancel)?;
    if password.is_none() && existing.iter().any(|entry| entry.encrypted) {
        return Err(io::Error::other(
            "Für neue Einträge in diesem ZIP ist ein Passwort erforderlich.",
        ));
    }
    if !folder.as_os_str().is_empty()
        && !existing.iter().any(|entry| {
            entry.path.starts_with(folder) && (entry.path != folder || entry.directory)
        })
    {
        return Err(io::Error::other("Der Zielordner fehlt im ZIP."));
    }
    let plan = plan_sources(paths, base, folder, cancel)?;
    let mut occupied = BTreeMap::new();
    for entry in &existing {
        files::check_cancel(cancel)?;
        crate::archive_view::register_path(&mut occupied, &entry.path, entry.directory)?;
    }
    for (_, name, _) in &plan {
        let path = checked_archive_path(name)?;
        if occupied.contains_key(&path.to_string_lossy().to_lowercase()) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("ZIP-Eintrag existiert bereits: {name}"),
            ));
        }
    }
    for (_, name, metadata) in &plan {
        crate::archive_view::register_path(
            &mut occupied,
            &checked_archive_path(name)?,
            metadata.is_dir(),
        )?;
    }
    if plan.len() + existing.len() > ENTRY_LIMIT {
        return Err(io::Error::other("Das ZIP enthält zu viele Einträge."));
    }

    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("Zielordner fehlt."))?;
    let mut staging = NamedTempFile::new_in(parent)?;
    let mut input = archive.into_inner().into_inner();
    input.rewind()?;
    if tools::copy_cancelled(input, &mut staging, cancel)? != original.len() {
        return Err(io::Error::other("Das ZIP hat sich beim Kopieren geändert."));
    }
    let count = plan.iter().filter(|(_, _, meta)| meta.is_file()).count();
    let mut archive = ZipWriter::new_append(staging.as_file_mut())?;
    write_plan(&mut archive, plan, password, cancel)?;
    archive.finish()?.sync_all()?;
    replace_zip(staging, path, &original, cancel)?;
    Ok(count)
}

pub enum ZipEdit {
    Remove(Vec<PathBuf>),
    Rename(PathBuf, std::ffi::OsString),
}

pub fn edit_zip(path: &Path, edit: ZipEdit, cancel: &AtomicBool) -> io::Result<usize> {
    files::check_cancel(cancel)?;
    let original = writable_zip(path)?;
    let input = EditReader {
        input: BufReader::new(File::open(path)?),
        cancel,
    };
    let mut archive = ZipArchive::new(input)?;
    let entries = members(&mut archive, cancel)?;
    if archive.offset() != 0 || entries.iter().any(|entry| entry.encrypted) {
        return Err(io::Error::other(
            "Löschen und Umbenennen setzen ein unverschlüsseltes ZIP ohne vorangestellte Daten voraus.",
        ));
    }
    let (selection, rename) = match edit {
        ZipEdit::Remove(paths) => (
            crate::archive_view::Selection::new(PathBuf::new(), paths)?,
            None,
        ),
        ZipEdit::Rename(source, name) => {
            let name = files::leaf_name(&name)?;
            let target = source.with_file_name(name);
            if source == target {
                return Ok(0);
            }
            (
                crate::archive_view::Selection::new(PathBuf::new(), vec![source.clone()])?,
                Some((source, target)),
            )
        }
    };
    let affected: BTreeSet<_> = crate::archive_view::project(
        entries.clone(),
        Some(&selection),
        |entry| (&entry.path, 0, entry.directory),
        cancel,
    )?
    .into_iter()
    .map(|(entry, _)| entry.index)
    .collect();

    let mut namespace = BTreeMap::new();
    let mut plan = Vec::new();
    for entry in entries {
        files::check_cancel(cancel)?;
        let target = if affected.contains(&entry.index) {
            let Some((source, target)) = &rename else {
                continue;
            };
            let relative = entry.path.strip_prefix(source).map_err(io::Error::other)?;
            Some(if relative.as_os_str().is_empty() {
                target.clone()
            } else {
                target.join(relative)
            })
        } else {
            None
        };
        let output = target.as_deref().unwrap_or(&entry.path);
        crate::archive_view::register_path(&mut namespace, output, entry.directory)?;
        plan.push((entry, target));
    }
    if let Some((_, target)) = &rename {
        // A rename must not merge an implicit directory into a distinct existing directory.
        for (entry, replacement) in &plan {
            if replacement.is_none()
                && entry.path.ancestors().any(|part| {
                    part.to_string_lossy().to_lowercase() == target.to_string_lossy().to_lowercase()
                })
            {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "Der neue ZIP-Name existiert bereits.",
                ));
            }
        }
    }

    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("Zielordner fehlt."))?;
    let mut staging = NamedTempFile::new_in(parent)?;
    let mut writer = ZipWriter::new(staging.as_file_mut());
    writer.set_raw_comment(archive.comment().into())?;
    for (entry, target) in &plan {
        files::check_cancel(cancel)?;
        let member = archive.by_index_raw(entry.index)?;
        let result = if let Some(target) = target {
            let mut name = target.to_string_lossy().replace('\\', "/");
            if entry.directory {
                name.push('/');
            }
            writer.raw_copy_file_rename(member, name)
        } else {
            writer.raw_copy_file(member)
        };
        files::check_cancel(cancel)?;
        result?;
    }
    writer.finish()?.sync_all()?;
    drop(archive);
    let result = list_zip(staging.path(), cancel)?;
    if result.len() != plan.len()
        || result.iter().zip(&plan).any(|(written, (source, target))| {
            written.path != target.as_deref().unwrap_or(&source.path)
                || written.directory != source.directory
                || written.size != source.size
                || written.encrypted != source.encrypted
        })
    {
        return Err(io::Error::other(
            "Die Liste im neuen ZIP stimmt nicht mit der Bearbeitung überein.",
        ));
    }
    replace_zip(staging, path, &original, cancel)?;
    Ok(affected.len())
}

struct EditReader<'a> {
    input: BufReader<File>,
    cancel: &'a AtomicBool,
}

impl Read for EditReader<'_> {
    fn read(&mut self, data: &mut [u8]) -> io::Result<usize> {
        if self.cancel.load(std::sync::atomic::Ordering::Relaxed) {
            // io::copy retries Interrupted. The caller translates this after the copy returns.
            return Err(io::Error::other("ZIP-Bearbeitung abgebrochen."));
        }
        self.input.read(data)
    }
}

impl Seek for EditReader<'_> {
    fn seek(&mut self, position: io::SeekFrom) -> io::Result<u64> {
        self.input.seek(position)
    }
}

fn writable_zip(path: &Path) -> io::Result<Metadata> {
    let metadata = files::regular_metadata(path)?;
    if !path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("zip"))
        || !metadata.is_file()
        || metadata.permissions().readonly()
    {
        return Err(io::Error::other(
            "Das Ziel muss eine beschreibbare ZIP-Datei sein.",
        ));
    }
    Ok(metadata)
}

fn replace_zip(
    staging: NamedTempFile,
    path: &Path,
    original: &Metadata,
    cancel: &AtomicBool,
) -> io::Result<()> {
    files::check_cancel(cancel)?;
    files::ensure_unchanged(path, original)?;
    fs::set_permissions(staging.path(), original.permissions())?;
    staging.persist(path).map_err(|error| error.error)?;
    Ok(())
}

type PlannedSource = (PathBuf, String, Metadata);

fn plan_sources(
    paths: &[PathBuf],
    base: &Path,
    prefix: &Path,
    cancel: &AtomicBool,
) -> io::Result<Vec<PlannedSource>> {
    if paths.is_empty() {
        return Err(io::Error::other("no source entries"));
    }
    let mut plan = Vec::new();
    let mut names = BTreeMap::new();
    let mut namespace = BTreeMap::new();
    for path in paths {
        for item in WalkDir::new(path).follow_links(false) {
            files::check_cancel(cancel)?;
            let item = item?;
            let path = item.into_path();
            let meta = files::regular_metadata(&path)?;
            let relative = path.strip_prefix(base).map_err(io::Error::other)?;
            if relative.as_os_str().is_empty() {
                return Err(io::Error::other("cannot pack the base directory itself"));
            }
            let mut parts = Vec::new();
            for component in prefix.components().chain(relative.components()) {
                let Component::Normal(component) = component else {
                    return Err(io::Error::other("invalid source path"));
                };
                let component = files::leaf_name(component)?;
                parts.push(
                    component
                        .to_str()
                        .ok_or_else(|| io::Error::other("invalid Unicode name"))?,
                );
            }
            let name = parts.join("/");
            match names.entry(name.to_lowercase()) {
                std::collections::btree_map::Entry::Vacant(entry) => {
                    crate::archive_view::register_path(
                        &mut namespace,
                        &checked_archive_path(&name)?,
                        meta.is_dir(),
                    )?;
                    entry.insert(path.clone());
                    plan.push((path, name, meta));
                }
                std::collections::btree_map::Entry::Occupied(entry) if entry.get() == &path => (),
                std::collections::btree_map::Entry::Occupied(_) => {
                    return Err(io::Error::other(
                        "Quellen enthalten kollidierende Dateinamen.",
                    ));
                }
            }
            if plan.len() > ENTRY_LIMIT {
                return Err(io::Error::other("too many archive entries"));
            }
        }
    }
    Ok(plan)
}

fn write_plan(
    archive: &mut ZipWriter<&mut File>,
    plan: Vec<PlannedSource>,
    password: Option<&str>,
    cancel: &AtomicBool,
) -> io::Result<()> {
    for (path, name, meta) in plan {
        files::check_cancel(cancel)?;
        files::ensure_unchanged(&path, &meta)?;
        let mut options = SimpleFileOptions::default();
        if let Some(modified) = meta.modified().ok().and_then(zip_date) {
            options = options.last_modified_time(modified);
        }
        if meta.is_dir() {
            archive.add_directory(name, options)?;
        } else {
            let options = options
                .compression_method(CompressionMethod::Deflated)
                .large_file(true);
            let options = match password {
                Some(password) => options.with_aes_encryption(AesMode::Aes256, password),
                None => options,
            };
            archive.start_file(name, options)?;
            let actual = tools::copy_cancelled(File::open(&path)?, &mut *archive, cancel)?;
            if actual != meta.len() {
                return Err(io::Error::other("source size changed during packing"));
            }
            files::ensure_unchanged(&path, &meta)?;
        }
    }
    Ok(())
}

pub fn extract_zip(
    path: &Path,
    destination: &Path,
    password: Option<&str>,
    cancel: &AtomicBool,
) -> io::Result<usize> {
    extract_zip_selection(path, destination, password, None, cancel)
}

pub(crate) fn extract_zip_selection(
    path: &Path,
    destination: &Path,
    password: Option<&str>,
    selection: Option<&crate::archive_view::Selection>,
    cancel: &AtomicBool,
) -> io::Result<usize> {
    let mut archive = ZipArchive::new(BufReader::new(File::open(path)?))?;
    let entries = members(&mut archive, cancel)?;
    let entries = crate::archive_view::project(
        entries,
        selection,
        |member| (&member.path, member.size, member.directory),
        cancel,
    )?;
    if password.is_none() && entries.iter().any(|(member, _)| member.encrypted) {
        return Err(io::Error::other(
            "Für die Archivauswahl ist ein Passwort erforderlich.",
        ));
    }
    let base = crate::archive_view::check_destination(
        destination,
        &entries,
        |entry| entry.directory,
        cancel,
    )?;
    let mut count = 0;
    for (member, relative) in entries {
        files::check_cancel(cancel)?;
        let target = base.join(&relative);
        let mut current = base.clone();
        let parent = if member.directory {
            &relative
        } else {
            relative.parent().unwrap_or(Path::new(""))
        };
        for component in parent.components() {
            current.push(component);
            files::create_or_verify_directory(&current)?;
        }
        if member.directory {
            continue;
        }
        let mut staging = NamedTempFile::new_in(&current)?;
        let mut entry = if member.encrypted {
            let password = password.ok_or_else(|| io::Error::other("archive password required"))?;
            archive.by_index_decrypt(member.index, password.as_bytes())?
        } else {
            archive.by_index(member.index)?
        };
        let bytes =
            tools::copy_cancelled((&mut entry).take(member.size + 1), &mut staging, cancel)?;
        if bytes != member.size {
            return Err(io::Error::other("archive entry size mismatch"));
        }
        crate::archive_view::preserve_timestamp(staging.as_file(), member.modified)?;
        staging.as_file().sync_all()?;
        files::check_cancel(cancel)?;
        staging
            .persist_noclobber(target)
            .map_err(|error| error.error)?;
        count += 1;
    }
    Ok(count)
}

pub fn test_zip(path: &Path, password: Option<&str>, cancel: &AtomicBool) -> io::Result<usize> {
    let mut archive = ZipArchive::new(BufReader::new(File::open(path)?))?;
    let entries = members(&mut archive, cancel)?;
    validate_size(&entries)?;
    for member in &entries {
        files::check_cancel(cancel)?;
        if member.directory {
            continue;
        }
        let mut entry = if member.encrypted {
            let password = password.ok_or_else(|| io::Error::other("archive password required"))?;
            archive.by_index_decrypt(member.index, password.as_bytes())?
        } else {
            archive.by_index(member.index)?
        };
        let bytes = tools::copy_cancelled((&mut entry).take(member.size + 1), io::sink(), cancel)?;
        if bytes != member.size {
            return Err(io::Error::other("archive entry size mismatch"));
        }
    }
    Ok(entries.len())
}

pub(crate) fn search_text(
    path: &Path,
    candidates: &[files::Entry],
    pattern: &regex::Regex,
    password: Option<&str>,
    cancel: &AtomicBool,
) -> io::Result<(BTreeSet<PathBuf>, Vec<String>)> {
    let mut archive = ZipArchive::new(BufReader::new(File::open(path)?))?;
    let wanted: BTreeSet<_> = candidates
        .iter()
        .filter(|row| row.kind == files::Kind::File)
        .map(|row| &row.path)
        .collect();
    let mut matches = BTreeSet::new();
    let mut errors = Vec::new();
    for member in members(&mut archive, cancel)? {
        files::check_cancel(cancel)?;
        if !wanted.contains(&member.path) {
            continue;
        }
        let result = (|| -> io::Result<bool> {
            if member.size > crate::archive_view::MAX_PREVIEW {
                return Err(io::Error::other(
                    "Datei überschreitet das Suchlimit von 64 MiB.",
                ));
            }
            let entry = if member.encrypted {
                let password = password.ok_or_else(|| io::Error::other("Passwort fehlt."))?;
                archive.by_index_decrypt(member.index, password.as_bytes())?
            } else {
                archive.by_index(member.index)?
            };
            let mut data = Vec::new();
            let actual = tools::copy_cancelled(entry.take(member.size + 1), &mut data, cancel)?;
            if actual != member.size {
                return Err(io::Error::other(
                    "Dateigröße stimmt nicht mit dem Verzeichnis überein.",
                ));
            }
            Ok(pattern.is_match(&tools::decode_text(&data)))
        })();
        match result {
            Ok(true) => {
                matches.insert(member.path);
            }
            Ok(false) => (),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => return Err(error),
            Err(error) => errors.push(format!("{}: {error}", member.path.display())),
        }
    }
    Ok((matches, errors))
}

fn validate_size(entries: &[Member]) -> io::Result<()> {
    let total = entries
        .iter()
        .try_fold(0u64, |total, entry| total.checked_add(entry.size));
    if total.is_none_or(|total| total > EXTRACT_LIMIT) {
        Err(io::Error::other(
            "archive exceeds the extraction size limit",
        ))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive_view::{self, Catalog, Selection};
    use std::io::Write;

    #[test]
    fn zip_rename_and_delete_keep_unrelated_entries() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("edit.zip");
        let mut writer = ZipWriter::new(File::create(&path).unwrap());
        writer.set_comment("Archivkommentar").unwrap();
        for (name, contents) in [
            ("left/a.txt", "a"),
            ("right/b.txt", "b"),
            ("keep.txt", "Arco"),
        ] {
            writer
                .start_file(name, SimpleFileOptions::default())
                .unwrap();
            writer.write_all(contents.as_bytes()).unwrap();
        }
        writer.finish().unwrap();
        let cancel = AtomicBool::new(false);
        let rename = |source: &str, target: &str| ZipEdit::Rename(source.into(), target.into());
        let before = fs::read(&path).unwrap();
        assert_eq!(
            edit_zip(&path, rename("left", "RIGHT"), &cancel)
                .unwrap_err()
                .kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(
            edit_zip(&path, rename("left", "docs"), &AtomicBool::new(true))
                .unwrap_err()
                .kind(),
            io::ErrorKind::Interrupted
        );
        assert_eq!(fs::read(&path).unwrap(), before);
        assert_eq!(edit_zip(&path, rename("left", "docs"), &cancel).unwrap(), 1);
        assert_eq!(
            edit_zip(&path, ZipEdit::Remove(vec!["right".into()]), &cancel).unwrap(),
            1
        );
        assert_eq!(test_zip(&path, None, &cancel).unwrap(), 2);
        let mut archive = ZipArchive::new(File::open(&path).unwrap()).unwrap();
        assert_eq!(archive.comment(), b"Archivkommentar");
        let mut contents = String::new();
        archive
            .by_name("docs/a.txt")
            .unwrap()
            .read_to_string(&mut contents)
            .unwrap();
        assert_eq!(contents, "a");
        contents.clear();
        archive
            .by_name("keep.txt")
            .unwrap()
            .read_to_string(&mut contents)
            .unwrap();
        assert_eq!(contents, "Arco");
        assert!(archive.by_name("right/b.txt").is_err());
    }

    #[test]
    fn archive_search_filters_the_current_subtree_and_handles_aes() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("search.zip");
        let mut writer = ZipWriter::new(File::create(&path).unwrap());
        let options = SimpleFileOptions::default().with_aes_encryption(AesMode::Aes256, "secret");
        for (name, contents) in [
            ("docs/a.txt", "Treffer"),
            ("docs/nested/b.txt", "kein Suchwort"),
            ("docs-other/c.txt", "Treffer"),
        ] {
            writer.start_file(name, options).unwrap();
            writer.write_all(contents.as_bytes()).unwrap();
        }
        writer.finish().unwrap();
        let cancel = AtomicBool::new(false);
        let catalog = Catalog::read(&path, &cancel).unwrap();
        let mut query = tools::Search {
            names: files::compile_masks("*.txt").unwrap(),
            text: None,
            min_size: None,
            max_size: None,
            max_depth: None,
            show_hidden: false,
        };
        let folder = Path::new("docs");
        assert_eq!(
            catalog
                .search(folder, &query, None, &cancel)
                .unwrap()
                .entries
                .len(),
            2
        );
        query.text = Some(regex::Regex::new("Treffer").unwrap());
        let locked = catalog.search(folder, &query, None, &cancel).unwrap();
        assert!(locked.entries.is_empty());
        assert_eq!(locked.errors.len(), 2);
        let matches = catalog
            .search(folder, &query, Some("secret"), &cancel)
            .unwrap();
        assert!(matches.errors.is_empty());
        assert_eq!(matches.entries.len(), 1);
        assert_eq!(matches.entries[0].path, folder.join("a.txt"));
    }

    #[test]
    fn unsafe_windows_paths_are_rejected() {
        for name in [
            "../out.txt",
            "folder/../out.txt",
            "/out",
            "C:/out",
            "CON.txt",
            "out.txt:stream",
            "out.",
            "a//b",
        ] {
            assert!(checked_archive_path(name).is_err(), "{name}");
        }
        assert_eq!(
            checked_archive_path("ordner/ä.txt").unwrap(),
            Path::new("ordner").join("ä.txt")
        );
    }

    #[test]
    fn zip_encrypted_roundtrip() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("ä.txt");
        let archive = root.path().join("example.zip");
        let destination = root.path().join("output");
        let cancel = AtomicBool::new(false);
        fs::write(&path, b"Arco").unwrap();
        fs::create_dir(&destination).unwrap();
        create_zip(&[path], root.path(), &archive, Some("example"), &cancel).unwrap();
        assert!(test_zip(&archive, Some("wrong"), &cancel).is_err());
        assert_eq!(test_zip(&archive, Some("example"), &cancel).unwrap(), 1);
        assert_eq!(
            extract_zip(&archive, &destination, Some("example"), &cancel).unwrap(),
            1
        );
        assert_eq!(fs::read(destination.join("ä.txt")).unwrap(), b"Arco");
    }

    #[test]
    fn catalog_and_extraction_support_implicit_directories() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("implicit.zip");
        let mut archive = ZipWriter::new(File::create(&path).unwrap());
        for (name, contents) in [
            ("nested/docs/ä.txt", "selected"),
            ("nested/other.txt", "skip"),
            ("elsewhere.txt", "skip"),
        ] {
            archive
                .start_file(name, SimpleFileOptions::default())
                .unwrap();
            archive.write_all(contents.as_bytes()).unwrap();
        }
        archive.finish().unwrap();
        let cancel = AtomicBool::new(false);
        let catalog = Catalog::read(&path, &cancel).unwrap();
        assert_eq!(catalog.rows(Path::new("")).unwrap().len(), 2);
        let nested = Path::new("nested");
        let docs = nested.join("docs");
        let rows = catalog.rows(nested).unwrap();
        assert!(
            rows.iter()
                .any(|row| row.path == docs && row.kind == files::Kind::Directory)
        );
        let selection = Selection::new(nested.to_path_buf(), vec![docs]).unwrap();
        let output = root.path().join("output");
        fs::create_dir(&output).unwrap();
        assert_eq!(
            archive_view::extract(&path, &output, &selection, None, &cancel).unwrap(),
            1
        );
        assert_eq!(
            fs::read(output.join("docs").join("ä.txt")).unwrap(),
            b"selected"
        );
        assert_eq!(fs::read_dir(&output).unwrap().count(), 1);
        let preview = Selection::preview(&nested.join("other.txt")).unwrap();
        archive_view::extract(&path, &output, &preview, None, &cancel).unwrap();
        assert_eq!(fs::read(output.join("other.txt")).unwrap(), b"skip");
        let missing =
            Selection::new(nested.to_path_buf(), vec![nested.join("missing.txt")]).unwrap();
        assert_eq!(
            archive_view::extract(&path, &output, &missing, None, &cancel)
                .unwrap_err()
                .kind(),
            io::ErrorKind::NotFound
        );
    }

    #[test]
    fn zip_append_preserves_entries_and_leaves_conflicting_archive_intact() {
        let root = tempfile::tempdir().unwrap();
        let base = root.path();
        let archive = base.join("example.zip");
        let folder = base.join("docs");
        let source = base.join("neu.txt");
        let cancel = AtomicBool::new(false);
        fs::create_dir(&folder).unwrap();
        fs::write(folder.join("alt.txt"), b"Original").unwrap();
        fs::write(&source, b"Neu").unwrap();
        create_zip(&[folder], base, &archive, Some("password"), &cancel).unwrap();
        let append = |cancel: &AtomicBool, password| {
            append_zip(
                &archive,
                Path::new("docs"),
                std::slice::from_ref(&source),
                base,
                password,
                cancel,
            )
        };
        assert!(append(&cancel, None).is_err());
        assert_eq!(append(&cancel, Some("password")).unwrap(), 1);
        assert_eq!(test_zip(&archive, Some("password"), &cancel).unwrap(), 3);
        let before = fs::read(&archive).unwrap();
        assert_eq!(
            append(&cancel, Some("password")).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(
            append(&AtomicBool::new(true), Some("password"))
                .unwrap_err()
                .kind(),
            io::ErrorKind::Interrupted
        );
        assert_eq!(fs::read(&archive).unwrap(), before);
        let output = base.join("output");
        fs::create_dir_all(output.join("docs")).unwrap();
        fs::write(output.join("docs").join("neu.txt"), b"Keep").unwrap();
        assert_eq!(
            extract_zip(&archive, &output, Some("password"), &cancel)
                .unwrap_err()
                .kind(),
            io::ErrorKind::AlreadyExists
        );
        assert!(!output.join("docs").join("alt.txt").exists());
        assert_eq!(
            fs::read(output.join("docs").join("neu.txt")).unwrap(),
            b"Keep"
        );
    }
}
