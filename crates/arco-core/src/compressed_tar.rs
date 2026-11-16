use crate::{external_archives::ExternalArchiver, files};
use std::{fs, io, path::Path, sync::atomic::AtomicBool};

pub(crate) fn recognizes(path: &Path) -> bool {
    path.file_name().is_some_and(|name| {
        let name = name.to_string_lossy().to_ascii_lowercase();
        name.ends_with(".tgz") || name.ends_with(".tar.gz")
    })
}

pub(crate) fn with_inner<T>(
    file: &Path,
    cancel: &AtomicBool,
    work: impl FnOnce(&Path, &ExternalArchiver) -> io::Result<T>,
) -> io::Result<T> {
    let original = files::regular_metadata(file)?;
    let temporary = tempfile::tempdir()?;
    let adapter = ExternalArchiver::discover(cancel)?;
    adapter.extract_with_zone(
        file,
        temporary.path(),
        None,
        None,
        cancel,
        crate::archive_view::ZonePolicy::Omit,
    )?;
    let mut children = fs::read_dir(temporary.path())?;
    let inner = children
        .next()
        .ok_or_else(|| io::Error::other("Das komprimierte TAR enthält keine Nutzdatei."))??
        .path();
    if children.next().is_some() || !files::regular_metadata(&inner)?.is_file() {
        return Err(io::Error::other(
            "Das komprimierte TAR muss genau eine TAR-Datei enthalten.",
        ));
    }
    let result = work(&inner, &adapter)?;
    files::ensure_unchanged(file, &original)?;
    Ok(result)
}
