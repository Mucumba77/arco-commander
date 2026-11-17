//! Transactional full rewrites for TAR, UC2, protected ACE and external packers.
use crate::{
    archive_view::{self, Catalog},
    archives::{self, ZipEdit},
    external_archives::{ExternalArchiver, Format},
    files,
    packer_profile::PackerProfile,
    tools,
};
use std::{
    fs::{self, File},
    io,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};

pub(crate) fn supports(path: &Path) -> bool {
    crate::compressed_tar::recognizes(path)
        || crate::uc2_volumes::is_entry_point(path)
        || path.extension().is_some_and(|ext| {
            ["tar", "uc2", "rar", "arj", "lha", "lzh"]
                .iter()
                .any(|kind| ext.eq_ignore_ascii_case(kind))
        })
}

fn member(path: &Path) -> io::Result<PathBuf> {
    let name = path
        .to_str()
        .ok_or_else(|| io::Error::other("Archivpfad benötigt Unicode."))?
        .replace('\\', "/");
    archives::checked_archive_path(&name)
}

pub(crate) fn edit(
    file: &Path,
    change: ZipEdit,
    password: Option<&str>,
    packer: Option<&PackerProfile>,
    cancel: &AtomicBool,
) -> io::Result<usize> {
    rewrite(
        file,
        Some(change),
        Path::new(""),
        &[],
        Path::new(""),
        password,
        packer,
        cancel,
    )
}

pub(crate) fn append(
    file: &Path,
    folder: &Path,
    sources: &[PathBuf],
    base: &Path,
    password: Option<&str>,
    packer: Option<&PackerProfile>,
    cancel: &AtomicBool,
) -> io::Result<usize> {
    rewrite(file, None, folder, sources, base, password, packer, cancel)
}

pub(crate) fn sync(
    file: &Path,
    removals: Vec<PathBuf>,
    additions: &[PathBuf],
    base: &Path,
    password: Option<&str>,
    packer: Option<&PackerProfile>,
    cancel: &AtomicBool,
) -> io::Result<usize> {
    let change = (!removals.is_empty()).then_some(ZipEdit::Remove(removals));
    rewrite(
        file,
        change,
        Path::new(""),
        additions,
        base,
        password,
        packer,
        cancel,
    )
}

fn rewrite(
    file: &Path,
    change: Option<ZipEdit>,
    folder: &Path,
    sources: &[PathBuf],
    base: &Path,
    password: Option<&str>,
    packer: Option<&PackerProfile>,
    cancel: &AtomicBool,
) -> io::Result<usize> {
    if matches!(
        crate::legacy_archives::Format::of(file),
        Some(crate::legacy_archives::Format::Ace)
    ) {
        crate::ace_volumes::ensure_writable(file)?;
    }
    let catalog = Catalog::read_with_password(file, password, cancel)?;
    let volumes = crate::legacy_archives::storage_parts(file, cancel)?;
    let original = files::regular_metadata(file)?;
    if original.permissions().readonly() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Das Archiv ist schreibgeschützt.",
        ));
    }
    let target = fs::canonicalize(file)?;
    for source in sources {
        files::check_cancel(cancel)?;
        if target.starts_with(fs::canonicalize(source)?) {
            return Err(io::Error::other(
                "Ein Archiv kann sich nicht selbst enthalten.",
            ));
        }
    }
    let parent = file
        .parent()
        .ok_or_else(|| io::Error::other("Archivordner fehlt."))?;
    let workspace = tempfile::tempdir_in(parent)?;
    let tree = workspace.path().join("files");
    fs::create_dir(&tree)?;
    archive_view::extract_with_options(
        file,
        &tree,
        None,
        password,
        cancel,
        |_| {},
        archive_view::ZonePolicy::Omit,
    )?;
    catalog.check_unchanged()?;
    let mut changed = 0;
    if let Some(change) = change {
        match change {
            ZipEdit::Remove(roots) => {
                if roots.is_empty() {
                    return Err(io::Error::other("Wähle Archiveinträge zum Entfernen."));
                }
                let mut removed: Vec<PathBuf> = Vec::new();
                for root in roots {
                    let root = member(&root)?;
                    if removed.iter().any(|previous| root.starts_with(previous)) {
                        continue;
                    }
                    let path = tree.join(&root);
                    files::regular_metadata(&path)?;
                    for entry in walkdir::WalkDir::new(&path)
                        .follow_links(false)
                        .contents_first(true)
                    {
                        files::check_cancel(cancel)?;
                        let entry = entry?;
                        let metadata = files::regular_metadata(entry.path())?;
                        if metadata.is_dir() {
                            fs::remove_dir(entry.path())?;
                        } else {
                            fs::remove_file(entry.path())?;
                        }
                        changed += 1;
                    }
                    removed.push(root);
                }
            }
            ZipEdit::Rename(from, leaf) => {
                let from = member(&from)?;
                let target = from
                    .parent()
                    .unwrap_or(Path::new(""))
                    .join(files::leaf_name(&leaf)?);
                let source = tree.join(&from);
                let destination = tree.join(&target);
                files::regular_metadata(&source)?;
                if from != target {
                    if destination.try_exists()?
                        && !(cfg!(windows)
                            && from
                                .to_string_lossy()
                                .eq_ignore_ascii_case(&target.to_string_lossy()))
                    {
                        return Err(io::ErrorKind::AlreadyExists.into());
                    }
                    fs::rename(source, destination)?;
                    changed += 1;
                }
            }
        }
    }
    let additions = if sources.is_empty() {
        Vec::new()
    } else {
        archives::plan_sources(sources, base, folder, cancel)?
    };
    for (source, name, metadata) in additions {
        files::check_cancel(cancel)?;
        let destination = tree.join(member(Path::new(&name))?);
        if metadata.is_dir() {
            files::create_or_verify_directory(&destination)?;
        } else {
            fs::create_dir_all(
                destination
                    .parent()
                    .ok_or_else(|| io::Error::other("Archivunterordner fehlt."))?,
            )?;
            let mut output = File::options()
                .create_new(true)
                .write(true)
                .open(&destination)?;
            files::ensure_unchanged(&source, &metadata)?;
            let bytes = tools::copy_cancelled(File::open(&source)?, &mut output, cancel)?;
            files::ensure_unchanged(&source, &metadata)?;
            if bytes != metadata.len() {
                return Err(io::Error::other(
                    "Eine Archivquelle hat ihre Größe geändert.",
                ));
            }
            archive_view::preserve_timestamp(&output, metadata.modified().ok())?;
            output.sync_all()?;
        }
        changed += 1;
    }
    let paths = fs::read_dir(&tree)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<io::Result<Vec<_>>>()?;
    let uc2 = matches!(
        crate::legacy_archives::Format::of(file),
        Some(crate::legacy_archives::Format::Uc2)
    );
    let output = if uc2 {
        workspace.path().join("updated.uc2")
    } else {
        workspace.path().join(
            file.file_name()
                .ok_or_else(|| io::Error::other("Archivname fehlt."))?,
        )
    };
    let mut external_parts = None;
    if let Some(packer) = packer {
        let parts = packer.pack_set(&paths, &tree, &output, password, cancel)?;
        if parts.first().and_then(|path| fs::canonicalize(path).ok())
            != fs::canonicalize(&output).ok()
            || !output.is_file()
        {
            return Err(io::Error::other(
                "Der Packer würde den Einstieg des Archivsatzes umbenennen. Verwende ein Profil mit passender Teilbenennung; der bisherige Satz bleibt erhalten.",
            ));
        }
        external_parts = Some(parts);
    } else if file
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("ace"))
    {
        if let Some(password) = password {
            crate::legacy_archives::pack_ace_encrypted(
                &paths, &tree, &output, 2, password, cancel,
            )?;
        } else {
            crate::legacy_archives::pack_ace_blocked(&paths, &tree, &output, cancel)?;
        }
    } else if uc2 {
        crate::legacy_archives::pack_uc2(&paths, &tree, &output, cancel)?;
    } else if file
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("tar"))
        || crate::compressed_tar::recognizes(file)
    {
        ExternalArchiver::discover(cancel)?.pack(
            &paths,
            &tree,
            &output,
            if crate::compressed_tar::recognizes(file) {
                Format::TarGzip
            } else {
                Format::Tar
            },
            cancel,
        )?;
    } else {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "Für die Bearbeitung dieses Formats wird ein passendes externes Packerprofil benötigt.",
        ));
    }
    archive_view::test(&output, password, cancel)?;
    if let Some(parts) = external_parts
        && (parts.len() > 1 || !volumes.is_empty())
    {
        let zone = crate::archive_zone::ArchiveZone::read(file, cancel)?;
        for part in &parts {
            zone.apply(part)?;
            fs::set_permissions(part, original.permissions())?;
        }
        catalog.check_unchanged()?;
        let originals = if volumes.is_empty() {
            vec![(file.to_path_buf(), original)]
        } else {
            volumes
        };
        let replacements = parts
            .into_iter()
            .map(|part| {
                let target = parent.join(part.file_name().expect("external part name"));
                (part, target)
            })
            .collect();
        crate::archive_parts::replace_parts(parent, &originals, replacements, cancel)?;
        return Ok(changed);
    }
    if uc2 && !volumes.is_empty() {
        let directory = workspace.path().join("parts");
        fs::create_dir(&directory)?;
        let parts = crate::uc2_volumes::repack(file, &paths, &tree, &directory, cancel)?;
        let zone = crate::archive_zone::ArchiveZone::read(file, cancel)?;
        for part in &parts {
            zone.apply(part)?;
            fs::set_permissions(part, original.permissions())?;
        }
        catalog.check_unchanged()?;
        let replacements = parts
            .into_iter()
            .map(|part| {
                let target = parent.join(part.file_name().expect("UC2 part name"));
                (part, target)
            })
            .collect();
        crate::archive_parts::replace_parts(parent, &volumes, replacements, cancel)?;
        return Ok(changed);
    }
    if file
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("ace"))
        && !volumes.is_empty()
    {
        let directory = workspace.path().join("parts");
        fs::create_dir(&directory)?;
        let first = directory.join(
            file.file_name()
                .ok_or_else(|| io::Error::other("Archivname fehlt."))?,
        );
        let size = volumes
            .iter()
            .map(|(_, metadata)| metadata.len())
            .max()
            .ok_or_else(|| io::Error::other("ACE-Teilgröße fehlt."))?;
        let parts = crate::ace_volumes::split(&output, &first, size, password, cancel)?;
        let zone = crate::archive_zone::ArchiveZone::read(file, cancel)?;
        for part in &parts {
            zone.apply(part)?;
            fs::set_permissions(part, original.permissions())?;
        }
        catalog.check_unchanged()?;
        let parts = parts
            .into_iter()
            .enumerate()
            .map(|(index, part)| (part, crate::ace_volumes::part_path(file, index)))
            .collect();
        crate::archive_parts::replace_parts(parent, &volumes, parts, cancel)?;
        return Ok(changed);
    }
    crate::archive_zone::ArchiveZone::read(file, cancel)?.apply(&output)?;
    fs::set_permissions(&output, original.permissions())?;
    files::check_cancel(cancel)?;
    catalog.check_unchanged()?;
    tempfile::TempPath::try_from_path(output)?
        .persist(file)
        .map_err(|error| error.error)?;
    Ok(changed)
}
