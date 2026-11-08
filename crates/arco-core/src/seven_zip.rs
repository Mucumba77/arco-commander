use crate::{
    archive_parts,
    archive_view::{self, Item, Selection},
    archives::{self, ZipEdit},
    files, tools,
};
use sevenz_rust2::{
    Archive, ArchiveEntry, ArchiveReader, ArchiveWriter, BlockDecoder, EncoderMethod, Error,
    Password,
    encoder_options::{AesEncoderOptions, Lzma2Options},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{self, BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};
use tempfile::{NamedTempFile, TempPath};

const MEMORY_KIB: usize = 128 * 1024;
const MAGIC: &[u8] = b"7z\xbc\xaf\x27\x1c";

#[derive(Debug)]
pub(crate) struct HeaderPassword;

impl std::fmt::Display for HeaderPassword {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Das 7z-Archiv benötigt ein gültiges Passwort für seine Dateinamen.")
    }
}

impl std::error::Error for HeaderPassword {}

pub(crate) fn is_7z(path: &Path) -> bool {
    path.extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("7z"))
}

pub(crate) fn can_read(path: &Path) -> bool {
    is_7z(path)
        || archive_parts::kind(path).is_some_and(|ext| ext.eq_ignore_ascii_case("7z"))
        || crate::external_archives::is_sfx(path)
}

fn open_read<'a>(
    path: &Path,
    cancel: &'a AtomicBool,
) -> io::Result<Cancellable<'a, BufReader<archive_parts::Input<'a>>>> {
    let input = match crate::external_archives::sfx_offset(path) {
        Some(offset) => archive_parts::Input::embedded(path, offset, cancel)?,
        None => archive_parts::Input::open(path, cancel)?,
    };
    Ok(Cancellable::new(BufReader::new(input), cancel))
}

fn result<T>(result: Result<T, Error>, cancel: &AtomicBool) -> io::Result<T> {
    files::check_cancel(cancel)?;
    result.map_err(io::Error::other)
}

fn entry_path(entry: &ArchiveEntry) -> io::Result<PathBuf> {
    let name = entry.name.replace('\\', "/");
    archives::checked_archive_path(name.trim_end_matches('/'))
}

fn encrypted_blocks(archive: &Archive) -> Vec<bool> {
    archive
        .blocks
        .iter()
        .map(|block| {
            block
                .coders
                .iter()
                .any(|coder| coder.encoder_method_id() == EncoderMethod::AES256_SHA256.id())
        })
        .collect()
}

fn items(archive: &Archive, cancel: &AtomicBool) -> io::Result<Vec<Item>> {
    if archive.files.len() > 500_000
        || archive.stream_map.file_block_index.len() != archive.files.len()
    {
        return Err(io::Error::other("Ungültige 7z-Eintragsliste."));
    }
    let encrypted = encrypted_blocks(archive);
    let mut seen = BTreeSet::new();
    archive
        .files
        .iter()
        .zip(&archive.stream_map.file_block_index)
        .map(|(entry, block)| {
            files::check_cancel(cancel)?;
            let attrs = entry.windows_attributes;
            if entry.is_anti_item
                || (entry.has_windows_attributes
                    && (attrs & 0x400 != 0 || (attrs >> 16) & 0xf000 == 0xa000))
            {
                return Err(io::Error::other(
                    "7z enthält eine Verknüpfung oder einen Löschmarker.",
                ));
            }
            let path = entry_path(entry)?;
            if !seen.insert(path.to_string_lossy().to_lowercase()) {
                return Err(io::Error::other("Mehrdeutiger Eintragsname im 7z-Archiv."));
            }
            Ok(Item {
                path,
                directory: entry.is_directory,
                size: entry.size,
                encrypted: block
                    .and_then(|index| encrypted.get(index))
                    .copied()
                    .unwrap_or_default(),
                modified: entry
                    .has_last_modified_date
                    .then(|| entry.last_modified_date.into()),
            })
        })
        .collect()
}

fn read_archive(
    input: &mut (impl Read + Seek),
    password: &Password,
    cancel: &AtomicBool,
) -> io::Result<(Archive, bool)> {
    let first = Archive::read_with_memory_limit(input, &Password::empty(), MEMORY_KIB);
    files::check_cancel(cancel)?;
    match first {
        Ok(archive) => Ok((archive, false)),
        Err(Error::PasswordRequired) => {
            let invalid = || io::Error::new(io::ErrorKind::PermissionDenied, HeaderPassword);
            if password.is_empty() {
                return Err(invalid());
            }
            input.rewind()?;
            let archive = Archive::read_with_memory_limit(input, password, MEMORY_KIB);
            files::check_cancel(cancel)?;
            Ok((archive.map_err(|_| invalid())?, true))
        }
        Err(error) => Err(io::Error::other(error)),
    }
}

pub(crate) fn list(
    path: &Path,
    password: Option<&str>,
    cancel: &AtomicBool,
) -> io::Result<(Vec<Item>, bool)> {
    let mut input = open_read(path, cancel)?;
    let (archive, protected) = read_archive(
        &mut input,
        &Password::new(password.unwrap_or_default()),
        cancel,
    )?;
    Ok((items(&archive, cancel)?, protected))
}

pub(crate) fn test(path: &Path, password: Option<&str>, cancel: &AtomicBool) -> io::Result<usize> {
    let mut reader = result(
        ArchiveReader::new_with_memory_limit(
            open_read(path, cancel)?,
            Password::new(password.unwrap_or_default()),
            MEMORY_KIB,
        ),
        cancel,
    )?;
    reader.set_thread_count(1);
    let items = items(reader.archive(), cancel)?;
    let count = items.len();
    archive_view::project(
        items,
        None,
        |item| (&item.path, item.size, item.directory),
        cancel,
    )?;
    result(
        reader.for_each_entries(|entry, input| {
            let actual = tools::copy_cancelled(
                input.take(entry.size.saturating_add(1)),
                io::sink(),
                cancel,
            )?;
            if actual != entry.size {
                return Err(io::Error::other("7z-Eintragsgröße stimmt nicht.").into());
            }
            Ok(true)
        }),
        cancel,
    )?;
    Ok(count)
}

pub(crate) fn extract(
    path: &Path,
    destination: &Path,
    selection: Option<&Selection>,
    password: Option<&str>,
    cancel: &AtomicBool,
) -> io::Result<usize> {
    let original = files::regular_metadata(path)?;
    let mut source = open_read(path, cancel)?;
    let password = Password::new(password.unwrap_or_default());
    let archive = result(
        Archive::read_with_memory_limit(&mut source, &password, MEMORY_KIB),
        cancel,
    )?;
    let items = items(&archive, cancel)?;
    let projected = archive_view::project(
        items,
        selection,
        |item| (&item.path, item.size, item.directory),
        cancel,
    )?;
    let base =
        archive_view::check_destination(destination, &projected, |item| item.directory, cancel)?;
    let zone = crate::archive_zone::ArchiveZone::read(path, cancel)?;
    let mut requested = BTreeMap::new();
    for (item, relative) in projected {
        let target = base.join(relative);
        let mut parent = base.clone();
        let through = if item.directory {
            target.as_path()
        } else {
            target.parent().expect("destination parent")
        };
        for component in through
            .strip_prefix(&base)
            .map_err(io::Error::other)?
            .components()
        {
            parent.push(component);
            files::create_or_verify_directory(&parent)?;
        }
        if !item.directory {
            requested.insert(item.path, (target, item.modified));
        }
    }
    let mut blocks = BTreeSet::new();
    let mut count = 0;
    let stage = |entry: &ArchiveEntry,
                 reader: &mut dyn Read|
     -> io::Result<Option<(TempPath, &Path)>> {
        files::check_cancel(cancel)?;
        if let Some((target, modified)) = requested.get(&entry_path(entry)?) {
            let mut output = NamedTempFile::new_in(target.parent().expect("destination parent"))?;
            let actual = tools::copy_cancelled(
                reader.take(entry.size.saturating_add(1)),
                &mut output,
                cancel,
            )?;
            if actual != entry.size {
                return Err(io::Error::other("7z-Eintragsgröße stimmt nicht."));
            }
            zone.apply(output.path())?;
            archive_view::preserve_timestamp(output.as_file(), *modified)?;
            output.as_file().sync_all()?;
            return Ok(Some((output.into_temp_path(), target)));
        }
        Ok(None)
    };
    let mut publish = |(output, target): (TempPath, &Path)| -> io::Result<()> {
        files::check_cancel(cancel)?;
        files::ensure_unchanged(path, &original)?;
        output
            .persist_noclobber(target)
            .map_err(|error| error.error)?;
        count += 1;
        Ok(())
    };
    for (entry, block) in archive
        .files
        .iter()
        .zip(&archive.stream_map.file_block_index)
    {
        if requested.contains_key(&entry_path(entry)?) {
            if let Some(index) = block.filter(|_| entry.has_stream) {
                blocks.insert(index);
            } else if let Some(output) = stage(entry, &mut io::empty())? {
                publish(output)?;
            }
        }
    }
    for index in blocks {
        let mut staged = Vec::new();
        let decoder = BlockDecoder::new_with_memory_limit(
            1,
            MEMORY_KIB,
            index,
            &archive,
            &password,
            &mut source,
        );
        result(
            decoder.for_each_entries(&mut |entry, reader| {
                if entry.has_stream
                    && let Some(output) = stage(entry, reader)?
                {
                    staged.push(output);
                }
                Ok(true)
            }),
            cancel,
        )?;
        // Some archives only store a CRC for the complete Solid block.
        for output in staged {
            publish(output)?;
        }
    }
    if count != requested.len() {
        return Err(io::Error::other(
            "Ein ausgewählter 7z-Eintrag konnte nicht gelesen werden.",
        ));
    }
    Ok(count)
}

pub(crate) fn search_text(
    path: &Path,
    candidates: &[files::Entry],
    pattern: &regex::Regex,
    password: Option<&str>,
    cancel: &AtomicBool,
) -> io::Result<(BTreeSet<PathBuf>, Vec<String>)> {
    let wanted: BTreeSet<_> = candidates
        .iter()
        .filter(|entry| entry.kind == files::Kind::File)
        .map(|entry| &entry.path)
        .collect();
    let mut matches = BTreeSet::new();
    let mut errors = Vec::new();
    if wanted.is_empty() {
        return Ok((matches, errors));
    }
    let mut source = open_read(path, cancel)?;
    let password = Password::new(password.unwrap_or_default());
    let (archive, _) = read_archive(&mut source, &password, cancel)?;
    let encrypted = encrypted_blocks(&archive);
    let mut selected = BTreeSet::new();
    let mut blocks = BTreeSet::new();
    for (entry, block) in archive
        .files
        .iter()
        .zip(&archive.stream_map.file_block_index)
    {
        files::check_cancel(cancel)?;
        let name = entry_path(entry)?;
        if !wanted.contains(&name) {
            continue;
        }
        if entry.size > archive_view::MAX_PREVIEW {
            errors.push(format!(
                "{} überschreitet das Suchlimit von 64 MiB.",
                name.display()
            ));
        } else if let Some(index) = block.filter(|_| entry.has_stream) {
            if encrypted[index] && password.is_empty() {
                errors.push(format!("{} benötigt ein Archivpasswort.", name.display()));
            } else {
                blocks.insert(index);
                selected.insert(name);
            }
        } else if pattern.is_match("") {
            matches.insert(name);
        }
    }
    for index in blocks {
        if archive.blocks[index].get_unpack_size() > archive_view::MAX_EXTRACT {
            errors.push("Ein 7z-Datenblock überschreitet das Entpacklimit.".into());
            continue;
        }
        let decoder = BlockDecoder::new_with_memory_limit(
            1,
            MEMORY_KIB,
            index,
            &archive,
            &password,
            &mut source,
        );
        let mut found = Vec::new();
        let decoded = result(
            decoder.for_each_entries(&mut |entry, reader| {
                files::check_cancel(cancel)?;
                let name = entry_path(entry)?;
                if selected.contains(&name) {
                    let mut data = Vec::new();
                    let actual =
                        tools::copy_cancelled(reader.take(entry.size + 1), &mut data, cancel)?;
                    if actual != entry.size {
                        return Err(io::Error::other("7z-Eintragsgröße stimmt nicht.").into());
                    }
                    if pattern.is_match(&tools::decode_text(&data)) {
                        found.push(name);
                    }
                }
                Ok(true)
            }),
            cancel,
        );
        match decoded {
            Ok(_) => matches.extend(found),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => return Err(error),
            Err(error) => errors.push(format!("7z-Datenblock {}: {error}", index + 1)),
        }
    }
    Ok((matches, errors))
}

pub(crate) fn append(
    path: &Path,
    folder: &Path,
    sources: &[PathBuf],
    base: &Path,
    password: Option<&str>,
    cancel: &AtomicBool,
) -> io::Result<usize> {
    let additions = archives::plan_sources(sources, base, folder, cancel)?;
    rewrite(path, None, folder, additions, password, cancel)
}

pub(crate) fn edit(
    path: &Path,
    edit: ZipEdit,
    password: Option<&str>,
    cancel: &AtomicBool,
) -> io::Result<usize> {
    rewrite(
        path,
        Some(edit),
        Path::new(""),
        Vec::new(),
        password,
        cancel,
    )
}

pub(crate) fn pack(
    paths: &[PathBuf],
    base: &Path,
    target: &Path,
    password: Option<&str>,
    cancel: &AtomicBool,
) -> io::Result<()> {
    let plan = archives::plan_sources(paths, base, Path::new(""), cancel)?;
    let parent = fs::canonicalize(
        target
            .parent()
            .ok_or_else(|| io::Error::other("Zielordner fehlt."))?,
    )?;
    for path in paths {
        if parent.starts_with(fs::canonicalize(path)?) {
            return Err(io::Error::other("Das Zielarchiv liegt im Quellordner."));
        }
    }
    let mut staging = NamedTempFile::new_in(parent)?;
    let mut writer = result(ArchiveWriter::new(staging.as_file_mut()), cancel)?;
    let protected = password
        .filter(|value| !value.is_empty())
        .map(Password::new);
    writer.set_encrypt_header(protected.is_some());
    push_sources(&mut writer, plan, protected.as_ref(), cancel)?;
    set_methods(&mut writer, protected.as_ref())?;
    writer.finish()?.sync_all()?;
    test(staging.path(), password, cancel)?;
    if protected.is_some() {
        verify_hidden_header(staging.path(), cancel)?;
    }
    files::check_cancel(cancel)?;
    staging
        .persist_noclobber(target)
        .map_err(|error| error.error)?;
    Ok(())
}

pub(crate) fn pack_sfx(
    paths: &[PathBuf],
    base: &Path,
    target: &Path,
    password: &str,
    cancel: &AtomicBool,
) -> io::Result<()> {
    if password.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Das Passwort für das 7z-SFX-Archiv fehlt.",
        ));
    }
    let module = crate::external_archives::sfx_module()?;
    let metadata = files::regular_metadata(&module)?;
    let mut stub = File::open(&module)?;
    let mut header = [0; 64];
    stub.read_exact(&mut header)?;
    let pe_offset = u32::from_le_bytes(header[60..64].try_into().unwrap());
    let mut signature = [0; 4];
    if &header[..2] != b"MZ" || metadata.len() > 2 * 1024 * 1024 - 32 {
        return Err(io::Error::other(
            "7z.sfx ist kein unterstütztes Windows-SFX-Modul.",
        ));
    }
    stub.seek(SeekFrom::Start(u64::from(pe_offset)))?;
    stub.read_exact(&mut signature)?;
    if &signature != b"PE\0\0" {
        return Err(io::Error::other("7z.sfx ist kein Windows-Programm."));
    }
    stub.rewind()?;
    let parent = target
        .parent()
        .ok_or_else(|| io::Error::other("Zielordner fehlt."))?;
    let working = tempfile::tempdir_in(parent)?;
    let payload = working.path().join("payload.7z");
    pack(paths, base, &payload, Some(password), cancel)?;
    let mut staging = tempfile::Builder::new()
        .suffix(".exe")
        .tempfile_in(parent)?;
    let copied = tools::copy_cancelled(stub.take(metadata.len() + 1), &mut staging, cancel)?;
    if copied != metadata.len() {
        return Err(io::Error::other("Das 7z-SFX-Modul hat sich geändert."));
    }
    files::ensure_unchanged(&module, &metadata)?;
    tools::copy_cancelled(File::open(payload)?, &mut staging, cancel)?;
    staging.as_file().sync_all()?;
    let staging = staging.into_temp_path();
    if crate::external_archives::sfx_offset(&staging) != Some(copied) {
        return Err(io::Error::other("Der 7z-Datenblock fehlt im SFX-Archiv."));
    }
    test(&staging, Some(password), cancel)?;
    verify_hidden_header(&staging, cancel)?;
    files::check_cancel(cancel)?;
    staging
        .persist_noclobber(target)
        .map_err(|error| error.error)?;
    Ok(())
}

pub(crate) fn sync_changes(
    path: &Path,
    removals: Vec<PathBuf>,
    sources: &[PathBuf],
    base: &Path,
    password: Option<&str>,
    cancel: &AtomicBool,
) -> io::Result<usize> {
    let root = Path::new("");
    let additions = if sources.is_empty() {
        Vec::new()
    } else {
        archives::plan_sources(sources, base, root, cancel)?
    };
    let edit = (!removals.is_empty()).then_some(ZipEdit::Remove(removals));
    rewrite(path, edit, root, additions, password, cancel)
}

fn rewrite(
    path: &Path,
    edit: Option<ZipEdit>,
    folder: &Path,
    additions: Vec<archives::PlannedSource>,
    supplied_password: Option<&str>,
    cancel: &AtomicBool,
) -> io::Result<usize> {
    let original = files::regular_metadata(path)?;
    if !original.is_file() || original.permissions().readonly() {
        return Err(io::Error::other(
            "Das Ziel muss ein beschreibbares 7z-Archiv sein.",
        ));
    }
    let canonical = fs::canonicalize(path)?;
    for (source, _, _) in &additions {
        if fs::canonicalize(source)? == canonical {
            return Err(io::Error::other(
                "Ein 7z-Archiv kann sich nicht selbst enthalten.",
            ));
        }
    }
    let mut source = Cancellable::new(File::open(path)?, cancel);
    let mut magic = [0; 6];
    source.read_exact(&mut magic)?;
    if magic != MAGIC {
        return Err(io::Error::other(
            "Bearbeitung erfordert ein eigenständiges 7z-Archiv.",
        ));
    }
    source.rewind()?;
    let password = Password::new(supplied_password.unwrap_or_default());
    let (archive, encrypt_header) = read_archive(&mut source, &password, cancel)?;
    let items = items(&archive, cancel)?;
    let protected_blocks = encrypted_blocks(&archive);
    let encrypt_additions = encrypt_header || protected_blocks.contains(&true);
    if encrypt_additions {
        if password.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Das 7z-Archiv benötigt ein Passwort.",
            ));
        }
        // Validate the password even if the operation removes every encrypted block.
        test(path, supplied_password, cancel)?;
        files::ensure_unchanged(path, &original)?;
    }
    if !folder.as_os_str().is_empty()
        && !items
            .iter()
            .any(|item| item.path.starts_with(folder) && (item.path != folder || item.directory))
    {
        return Err(io::Error::other("Der Zielordner fehlt im 7z-Archiv."));
    }
    let (selection, rename) = match edit {
        Some(ZipEdit::Rename(path, name)) => {
            let target = path.with_file_name(files::leaf_name(&name)?);
            if path == target {
                return Ok(0);
            }
            (
                Some(Selection::new(PathBuf::new(), vec![path.clone()])?),
                Some((path, target)),
            )
        }
        Some(ZipEdit::Remove(paths)) => (Some(Selection::new(PathBuf::new(), paths)?), None),
        None => (None, None),
    };
    let affected: BTreeSet<_> = match selection {
        Some(selection) => archive_view::project(
            items.iter().collect(),
            Some(&selection),
            |item| (&item.path, 0, item.directory),
            cancel,
        )?
        .into_iter()
        .map(|(item, _)| &item.path)
        .collect(),
        None => BTreeSet::new(),
    };
    let mut names = BTreeMap::new();
    let mut explicit_names = BTreeSet::new();
    let mut planned = BTreeMap::new();
    for (entry, item) in archive.files.iter().zip(&items) {
        files::check_cancel(cancel)?;
        let target = if affected.contains(&item.path) {
            let Some((from, to)) = &rename else {
                planned.insert(entry.name.as_str(), None);
                continue;
            };
            let relative = item.path.strip_prefix(from).map_err(io::Error::other)?;
            if relative.as_os_str().is_empty() {
                to.clone()
            } else {
                to.join(relative)
            }
        } else {
            item.path.clone()
        };
        if !explicit_names.insert(target.to_string_lossy().to_lowercase()) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("7z-Eintrag existiert bereits: {}", target.display()),
            ));
        }
        archive_view::register_path(&mut names, &target, item.directory)?;
        planned.insert(
            entry.name.as_str(),
            Some(target.to_string_lossy().replace('\\', "/")),
        );
    }
    for (_, name, metadata) in &additions {
        let entry = archives::checked_archive_path(name)?;
        if names.contains_key(&entry.to_string_lossy().to_lowercase()) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("7z-Eintrag existiert bereits: {name}"),
            ));
        }
        archive_view::register_path(&mut names, &entry, metadata.is_dir())?;
    }
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("Zielordner fehlt."))?;
    let mut staging = NamedTempFile::new_in(parent)?;
    let mut writer = result(ArchiveWriter::new(staging.as_file_mut()), cancel)?;
    writer.set_encrypt_header(encrypt_header);
    let mut blocks = vec![Vec::new(); archive.blocks.len()];
    for (entry, index) in archive
        .files
        .iter()
        .zip(&archive.stream_map.file_block_index)
    {
        if entry.has_stream {
            let block = index
                .and_then(|index| blocks.get_mut(index))
                .ok_or_else(|| io::Error::other("7z-Datenblock fehlt."))?;
            block.push(entry);
        } else if let Some(name) = &planned[entry.name.as_str()] {
            result(
                writer.push_archive_entry::<io::Empty>(
                    ArchiveEntry {
                        name: name.clone(),
                        ..entry.clone()
                    },
                    None,
                ),
                cancel,
            )?;
        }
    }
    for (index, entries) in blocks.into_iter().enumerate() {
        files::check_cancel(cancel)?;
        let keep = |entry: &ArchiveEntry| planned[entry.name.as_str()].is_some();
        if !entries.iter().any(|entry| keep(entry)) {
            continue;
        }
        // The library scans the full file list when copying a raw block.
        let raw_budget = archive.files.len().saturating_mul(archive.blocks.len()) <= 16_000_000;
        if raw_budget && entries.iter().all(|entry| keep(entry) && entry.has_crc) {
            result(
                writer.push_raw_block(&archive, index, &mut source, |entry| {
                    planned[entry.name.as_str()]
                        .clone()
                        .expect("retained block")
                }),
                cancel,
            )?;
            continue;
        }
        let decoder = BlockDecoder::new_with_memory_limit(
            1,
            MEMORY_KIB,
            index,
            &archive,
            &password,
            &mut source,
        );
        result(
            decoder.for_each_entries(&mut |entry, reader| {
                files::check_cancel(cancel)?;
                if let Some(name) = &planned[entry.name.as_str()]
                    && entry.has_stream
                {
                    set_methods(&mut writer, protected_blocks[index].then_some(&password))?;
                    let output = writer.push_archive_entry(
                        ArchiveEntry {
                            name: name.clone(),
                            ..entry.clone()
                        },
                        Some(Cancellable::new(
                            reader.take(entry.size.saturating_add(1)),
                            cancel,
                        )),
                    )?;
                    if output.size != entry.size {
                        return Err(io::Error::other("7z-Eintragsgröße stimmt nicht.").into());
                    }
                }
                Ok(true)
            }),
            cancel,
        )?;
    }
    let count = affected.len()
        + push_sources(
            &mut writer,
            additions,
            encrypt_additions.then_some(&password),
            cancel,
        )?;
    // The header gets a separate random salt and IV from every data block.
    set_methods(&mut writer, encrypt_header.then_some(&password))?;
    writer.finish()?.sync_all()?;
    test(staging.path(), supplied_password, cancel)?;
    if encrypt_header {
        verify_hidden_header(staging.path(), cancel)?;
    }
    drop(source);
    archives::replace_zip(staging, path, &original, cancel)?;
    Ok(count)
}

fn verify_hidden_header(path: &Path, cancel: &AtomicBool) -> io::Result<()> {
    match list(path, None, cancel) {
        Err(error) if archive_view::needs_header_password(&error) => Ok(()),
        Err(error) => Err(error),
        Ok(_) => Err(io::Error::other(
            "Das temporäre 7z-Archiv hat keinen verschlüsselten Header.",
        )),
    }
}

fn set_methods(
    writer: &mut ArchiveWriter<&mut File>,
    password: Option<&Password>,
) -> io::Result<()> {
    let mut methods = Vec::with_capacity(2);
    if let Some(password) = password {
        let mut iv = [0; 16];
        let mut salt = [0; 16];
        getrandom::fill(&mut iv).map_err(|error| io::Error::other(error.to_string()))?;
        getrandom::fill(&mut salt).map_err(|error| io::Error::other(error.to_string()))?;
        methods.push(
            AesEncoderOptions {
                password: password.clone(),
                iv,
                salt,
                num_cycles_power: 19,
            }
            .into(),
        );
    }
    methods.push(Lzma2Options::from_level(3).into());
    writer.set_content_methods(methods);
    Ok(())
}

fn push_sources(
    writer: &mut ArchiveWriter<&mut File>,
    sources: Vec<archives::PlannedSource>,
    password: Option<&Password>,
    cancel: &AtomicBool,
) -> io::Result<usize> {
    let mut count = 0;
    for (source, name, metadata) in sources {
        files::check_cancel(cancel)?;
        files::ensure_unchanged(&source, &metadata)?;
        let mut entry = ArchiveEntry::from_path(&source, name);
        entry.has_windows_attributes = true;
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            entry.windows_attributes = metadata.file_attributes() & 0x37;
        }
        #[cfg(not(windows))]
        {
            entry.windows_attributes = if metadata.is_dir() { 0x10 } else { 0x20 };
        }
        let reader = if metadata.is_file() {
            set_methods(writer, password)?;
            count += 1;
            Some(Cancellable::new(File::open(&source)?, cancel))
        } else {
            None
        };
        result(writer.push_archive_entry(entry, reader), cancel)?;
        files::ensure_unchanged(&source, &metadata)?;
    }
    Ok(count)
}

struct Cancellable<'a, R> {
    inner: R,
    cancel: &'a AtomicBool,
}

impl<'a, R> Cancellable<'a, R> {
    fn new(inner: R, cancel: &'a AtomicBool) -> Self {
        Self { inner, cancel }
    }
}

impl<R: Read> Read for Cancellable<'_, R> {
    fn read(&mut self, data: &mut [u8]) -> io::Result<usize> {
        if self.cancel.load(Ordering::Relaxed) {
            // std::io::copy retries Interrupted. The caller translates cancellation.
            return Err(io::Error::other("7z-Auftrag abgebrochen."));
        }
        self.inner.read(data)
    }
}

impl<R: Seek> Seek for Cancellable<'_, R> {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.inner.seek(position)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sevenz_rust2::SourceReader;

    #[test]
    fn encrypted_solid_edit_keeps_header_private_and_rejects_wrong_password() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path();
        let path = root.join("protected.7z");
        let password = "7z fixture password";
        let mut file = File::create(&path).unwrap();
        let mut writer = ArchiveWriter::new(&mut file).unwrap();
        // A cheap input fixture. Arco's rewrites use fresh salts and IVs.
        writer.set_content_methods(vec![
            AesEncoderOptions::new(Password::new(password)).into(),
            Lzma2Options::from_level(1).into(),
        ]);
        writer
            .push_archive_entries(
                vec![
                    ArchiveEntry::new_file("private/first.txt"),
                    ArchiveEntry::new_file("private/second.txt"),
                ],
                vec![
                    SourceReader::new(&b"first"[..]),
                    SourceReader::new(&b"second"[..]),
                ],
            )
            .unwrap();
        writer.finish().unwrap();
        drop(file);
        let cancel = AtomicBool::new(false);
        let before = fs::read(&path).unwrap();
        assert!(
            edit(
                &path,
                ZipEdit::Remove(vec!["private".into()]),
                Some("wrong"),
                &cancel
            )
            .is_err()
        );
        assert_eq!(fs::read(&path).unwrap(), before);
        assert_eq!(
            edit(
                &path,
                ZipEdit::Rename("private".into(), "renamed".into()),
                Some(password),
                &cancel
            )
            .unwrap(),
            2
        );
        assert!(archive_view::needs_header_password(
            &list(&path, None, &cancel).unwrap_err()
        ));
        assert_eq!(
            edit(
                &path,
                ZipEdit::Remove(vec!["renamed/first.txt".into()]),
                Some(password),
                &cancel
            )
            .unwrap(),
            1
        );
        let addition = root.join("extra.txt");
        fs::write(&addition, b"added").unwrap();
        assert_eq!(
            append(
                &path,
                Path::new("renamed"),
                &[addition],
                root,
                Some(password),
                &cancel
            )
            .unwrap(),
            1
        );
        let (entries, header) = list(&path, Some(password), &cancel).unwrap();
        assert!(header);
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().all(|entry| entry.encrypted));
        assert!(test(&path, Some("wrong"), &cancel).is_err());
        let catalog =
            archive_view::Catalog::read_with_password(&path, Some(password), &cancel).unwrap();
        let search = tools::Search {
            names: files::compile_masks("*.txt").unwrap(),
            text: Some(regex::Regex::new("second").unwrap()),
            min_size: None,
            max_size: None,
            max_depth: None,
            show_hidden: false,
            ignore: None,
        };
        let hits = catalog
            .search(Path::new(""), &search, Some(password), &cancel)
            .unwrap();
        assert!(hits.errors.is_empty());
        assert_eq!(hits.entries.len(), 1);
        assert_eq!(hits.entries[0].path, Path::new("renamed/second.txt"));
        let out = root.join("out");
        fs::create_dir(&out).unwrap();
        assert_eq!(
            extract(&path, &out, None, Some(password), &cancel).unwrap(),
            2
        );
        assert_eq!(fs::read(out.join("renamed/second.txt")).unwrap(), b"second");
        assert_eq!(fs::read(out.join("renamed/extra.txt")).unwrap(), b"added");
    }

    #[test]
    fn edits_a_solid_archive_and_preserves_the_original_on_conflicts() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path();
        let path = root.join("solid.7z");
        let mut writer = ArchiveWriter::create(&path).unwrap();
        writer
            .push_archive_entries(
                vec![
                    ArchiveEntry::new_file("old/one.txt"),
                    ArchiveEntry::new_file("old/two.txt"),
                ],
                vec![
                    SourceReader::new(&b"one"[..]),
                    SourceReader::new(&b"two"[..]),
                ],
            )
            .unwrap();
        writer.finish().unwrap();
        let cancel = AtomicBool::new(false);
        assert_eq!(
            edit(
                &path,
                ZipEdit::Rename("old".into(), "new".into()),
                None,
                &cancel
            )
            .unwrap(),
            2
        );
        let archive = Archive::open(&path).unwrap();
        assert_eq!(archive.blocks.len(), 1);
        let selection = Selection::preview(Path::new("new/two.txt")).unwrap();
        let preview = root.join("preview");
        fs::create_dir(&preview).unwrap();
        assert_eq!(
            extract(&path, &preview, Some(&selection), None, &cancel).unwrap(),
            1
        );
        assert_eq!(fs::read(preview.join("two.txt")).unwrap(), b"two");
        assert_eq!(
            edit(
                &path,
                ZipEdit::Remove(vec!["new/one.txt".into()]),
                None,
                &cancel
            )
            .unwrap(),
            1
        );
        assert_eq!(test(&path, None, &cancel).unwrap(), 1);
        let add = root.join("third.txt");
        fs::write(&add, b"three").unwrap();
        assert_eq!(
            append(
                &path,
                Path::new("new"),
                std::slice::from_ref(&add),
                root,
                None,
                &cancel
            )
            .unwrap(),
            1
        );
        let before = fs::read(&path).unwrap();
        assert!(append(&path, Path::new("new"), &[add], root, None, &cancel).is_err());
        assert!(
            edit(
                &path,
                ZipEdit::Rename("new/two.txt".into(), "third.txt".into()),
                None,
                &cancel
            )
            .is_err()
        );
        assert!(
            edit(
                &path,
                ZipEdit::Remove(vec!["new".into()]),
                None,
                &AtomicBool::new(true)
            )
            .is_err()
        );
        assert_eq!(fs::read(&path).unwrap(), before);
        let target = root.join("out");
        fs::create_dir(&target).unwrap();
        assert_eq!(extract(&path, &target, None, None, &cancel).unwrap(), 2);
        assert_eq!(fs::read(target.join("new/two.txt")).unwrap(), b"two");
        assert_eq!(fs::read(target.join("new/third.txt")).unwrap(), b"three");
        assert!(!target.join("new/one.txt").exists());
    }
}
