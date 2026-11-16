use crate::files;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::AtomicBool;

pub(super) fn number_collisions(
    entries: &mut [(PathBuf, PathBuf)],
    cancel: Option<&AtomicBool>,
) -> io::Result<()> {
    let key = |path: &Path| path.to_string_lossy().to_lowercase();
    let sources = entries
        .iter()
        .map(|(source, _)| key(source))
        .collect::<std::collections::HashSet<_>>();
    let mut targets = std::collections::HashSet::new();
    for (source, target) in entries {
        let original = target.clone();
        let directory = files::regular_metadata(source)?.is_dir();
        let name = original.file_name().unwrap_or_default().to_string_lossy();
        let (stem, extension) = name
            .rsplit_once('.')
            .filter(|(stem, _)| !directory && !stem.is_empty())
            .map_or((name.as_ref(), ""), |(stem, extension)| (stem, extension));
        for number in 1..=100000 {
            if let Some(cancel) = cancel {
                files::check_cancel(cancel)?;
            }
            let candidate = key(target);
            let exists = match fs::symlink_metadata(&*target) {
                Ok(_) => !sources.contains(&candidate),
                Err(error) if error.kind() == io::ErrorKind::NotFound => false,
                Err(error) => return Err(error),
            };
            if !exists && targets.insert(candidate) {
                break;
            }
            if number == 100000 {
                return Err(io::Error::other(
                    "Keine freie Namensnummer bis 100.000 gefunden.",
                ));
            }
            let next = if extension.is_empty() {
                format!("{stem} ({})", number + 1)
            } else {
                format!("{stem} ({}).{extension}", number + 1)
            };
            *target = original.with_file_name(next);
        }
    }
    Ok(())
}

pub(super) fn target(source: &Path, name: &str) -> io::Result<PathBuf> {
    if name.is_empty() || name.starts_with(['\\', '/']) || name.ends_with(['\\', '/']) {
        return Err(io::Error::other("Gib einen relativen Zielnamen an."));
    }
    let mut target = source
        .parent()
        .ok_or_else(|| io::Error::other("Der Quellordner fehlt."))?
        .to_path_buf();
    for part in name.split(['\\', '/']).filter(|part| !part.is_empty()) {
        target.push(files::leaf_name(std::ffi::OsStr::new(part))?);
    }
    Ok(target)
}

pub(super) fn parents(root: &Path, path: &Path) -> io::Result<Vec<PathBuf>> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| io::Error::other("Das Umbenennziel liegt außerhalb des Quellbaums."))?;
    let components = relative.components().collect::<Vec<_>>();
    if components.is_empty() {
        return Err(io::Error::other(
            "Ein Ordner kann nicht sein eigenes Umbenennziel sein.",
        ));
    }
    let mut parent = root.to_path_buf();
    let mut parents = Vec::new();
    for (index, component) in components.iter().enumerate() {
        let Component::Normal(part) = component else {
            return Err(io::Error::other("Ungültiger relativer Umbenennpfad."));
        };
        files::leaf_name(part)?;
        parent.push(part);
        if index + 1 < components.len() {
            match fs::symlink_metadata(&parent) {
                Ok(metadata) if metadata.is_dir() && !files::is_link(&metadata) => (),
                Ok(_) => {
                    return Err(io::Error::other(format!(
                        "Kein regulärer Zielordner: {}",
                        parent.display()
                    )));
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => (),
                Err(error) => return Err(error),
            }
            parents.push(parent.clone());
        }
    }
    Ok(parents)
}

pub(super) fn create_parents(
    root: &Path,
    target: &Path,
    created: &mut Vec<PathBuf>,
) -> io::Result<()> {
    for path in parents(root, target)? {
        match fs::create_dir(&path) {
            Ok(()) => created.push(path),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                let metadata = fs::symlink_metadata(&path)?;
                if !metadata.is_dir() || files::is_link(&metadata) {
                    return Err(io::Error::other(format!(
                        "Der Zielordner wurde ersetzt: {}",
                        path.display()
                    )));
                }
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

pub(super) fn remove_empty(created: &[PathBuf]) {
    for path in created.iter().rev() {
        if fs::symlink_metadata(path)
            .is_ok_and(|metadata| metadata.is_dir() && !files::is_link(&metadata))
        {
            // Only empty directories can be removed; never recursively remove
            // data that another program may have added while we were running.
            let _ = fs::remove_dir(path);
        }
    }
}
