pub use crate::ace::pack as pack_ace;
pub use crate::ace::pack_blocked as pack_ace_blocked;
pub use crate::ace::pack_compressed as pack_ace_compressed;
pub use crate::ace_volumes::pack as pack_ace_volumes;
pub use crate::uc2_volumes::pack_sas as pack_uc2_sas;
pub use crate::uc2_volumes::pack_sea as pack_uc2_sea;
use crate::{
    archive_view::{self, Item, Selection},
    archives,
    external_archives::{capture_output, capture_output_input},
    files,
};
use chrono::{DateTime, Local, NaiveDateTime};
use std::{
    collections::BTreeSet,
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::AtomicBool,
};
use tempfile::NamedTempFile;

#[derive(Clone, Copy)]
pub(crate) enum Format {
    Ace,
    Uc2,
}

impl Format {
    pub(crate) fn of(path: &Path) -> Option<Self> {
        let ext = path.extension()?;
        if ext.eq_ignore_ascii_case("ace") || crate::ace_volumes::recognizes(path) {
            Some(Self::Ace)
        } else if ext.eq_ignore_ascii_case("uc2")
            || (!crate::external_archives::is_first_volume(path)
                && crate::uc2_volumes::is_volume(path))
        {
            Some(Self::Uc2)
        } else {
            None
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Ace => "ace",
            Self::Uc2 => "uc2",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Ace => "ACE-Adapter (Python mit acefile)",
            Self::Uc2 => "UC2-Adapter (Python mit uc2)",
        }
    }

    fn command(self, operation: &str, path: &Path) -> io::Result<Command> {
        let python = std::env::var_os("ARCO_ARCHIVE_PYTHON")
            .map(PathBuf::from)
            .or_else(|| {
                let program = std::env::current_exe().ok()?;
                Some(program.parent()?.join("archive-python").join("Scripts").join("python.exe"))
            })
            .filter(|path| path.is_absolute() && path.is_file())
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    format!(
                        "{} benötigt archive-python neben Arco oder den absoluten Pfad zur Python-EXE in ARCO_ARCHIVE_PYTHON.",
                        self.label()
                    ),
                )
            })?;
        let mut command = Command::new(python);
        command
            .args([
                "-I",
                "-c",
                include_str!("legacy_archives.py"),
                self.name(),
                operation,
            ])
            .arg(path)
            .stdin(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000);
        }
        Ok(command)
    }

    pub(crate) fn list(self, path: &Path, cancel: &AtomicBool) -> io::Result<Vec<Item>> {
        let output = capture_output(
            self.label(),
            self.command("list", path)?,
            24 * 1024 * 1024,
            cancel,
        )?;
        type Record = (String, u64, bool, bool, Option<String>);
        let rows: Vec<Record> = serde_json::from_slice(&output).map_err(io::Error::other)?;
        if rows.len() > 100_000 {
            return Err(io::Error::other("Das Archiv enthält zu viele Einträge."));
        }
        let mut seen = BTreeSet::new();
        rows.into_iter()
            .map(|(name, size, directory, encrypted, modified)| {
                files::check_cancel(cancel)?;
                let normalized = name.replace('\\', "/");
                let path = archives::checked_archive_path(normalized.trim_end_matches('/'))?;
                if !seen.insert(normalized.trim_end_matches('/').to_lowercase()) {
                    return Err(io::Error::other(
                        "Das Archiv enthält mehrdeutige Dateinamen.",
                    ));
                }
                let modified = modified
                    .and_then(|value| value.parse::<NaiveDateTime>().ok())
                    .and_then(archive_view::local_timestamp);
                Ok(Item {
                    path,
                    size,
                    directory,
                    encrypted,
                    modified,
                })
            })
            .collect()
    }

    pub(crate) fn test(self, path: &Path, cancel: &AtomicBool) -> io::Result<usize> {
        self.test_with_password(path, None, cancel)
    }

    pub(crate) fn test_with_password(
        self,
        path: &Path,
        password: Option<&str>,
        cancel: &AtomicBool,
    ) -> io::Result<usize> {
        let original = files::regular_metadata(path)?;
        let volumes = storage_parts(path, cancel)?;
        let entries = self.list(path, cancel)?;
        let entries = archive_view::project(
            entries,
            None,
            |entry| (&entry.path, entry.size, entry.directory),
            cancel,
        )?;
        if password.is_none() && entries.iter().any(|(entry, _)| entry.encrypted) {
            return Err(io::Error::other(
                "Für verschlüsselte ACE-Einträge ist ein Passwort erforderlich.",
            ));
        }
        capture_output_input(
            self.label(),
            self.command("test", path)?,
            1024,
            cancel,
            password_input(password)?,
        )?;
        files::ensure_unchanged(path, &original)?;
        crate::uc2_volumes::unchanged(&volumes)?;
        Ok(entries.len())
    }

    pub(crate) fn compare_sources(
        self,
        path: &Path,
        manifest: &Path,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        let mut command = self.command("compare-sources", path)?;
        command.arg(manifest);
        let output = capture_output(self.label(), command, 4096, cancel)?;
        if !output.is_empty() {
            return Err(io::Error::other(
                String::from_utf8_lossy(&output).into_owned(),
            ));
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn extract(
        self,
        path: &Path,
        destination: &Path,
        selection: Option<&Selection>,
        cancel: &AtomicBool,
    ) -> io::Result<usize> {
        self.extract_with_zone(
            path,
            destination,
            selection,
            None,
            cancel,
            Default::default(),
        )
    }

    pub(crate) fn extract_with_zone(
        self,
        path: &Path,
        destination: &Path,
        selection: Option<&Selection>,
        password: Option<&str>,
        cancel: &AtomicBool,
        policy: crate::archive_zone::ZonePolicy,
    ) -> io::Result<usize> {
        let original = files::regular_metadata(path)?;
        let volumes = storage_parts(path, cancel)?;
        let entries = self.list(path, cancel)?.into_iter().enumerate().collect();
        let entries = archive_view::project(
            entries,
            selection,
            |(_, item)| (&item.path, item.size, item.directory),
            cancel,
        )?;
        if password.is_none() && entries.iter().any(|((_, item), _)| item.encrypted) {
            return Err(io::Error::other(
                "Für verschlüsselte ACE-Einträge ist ein Passwort erforderlich.",
            ));
        }
        let base = archive_view::check_destination(
            destination,
            &entries,
            |(_, item)| item.directory,
            cancel,
        )?;
        let zone = crate::archive_zone::ArchiveZone::read_with_policy(path, cancel, policy)?;
        let working = tempfile::tempdir_in(&base)?;
        let mut staged = Vec::new();
        let mut requested = Vec::new();
        for ((index, item), relative) in entries {
            files::check_cancel(cancel)?;
            let folder = if item.directory {
                &relative
            } else {
                relative.parent().unwrap_or(Path::new(""))
            };
            let mut parent = base.clone();
            for part in folder.components() {
                parent.push(part);
                files::create_or_verify_directory(&parent)?;
            }
            if item.directory {
                continue;
            }
            let temporary = NamedTempFile::new_in(working.path())?.into_temp_path();
            requested.push((index, temporary.to_path_buf(), item.size));
            staged.push((temporary, item, relative));
        }
        if !requested.is_empty() {
            let mut manifest = NamedTempFile::new_in(working.path())?;
            serde_json::to_writer(&mut manifest, &requested).map_err(io::Error::other)?;
            manifest.as_file().sync_all()?;
            let mut command = self.command("extract-manifest", path)?;
            command.arg(manifest.path());
            capture_output_input(
                self.label(),
                command,
                1024,
                cancel,
                password_input(password)?,
            )?;
        }
        files::ensure_unchanged(path, &original)?;
        crate::uc2_volumes::unchanged(&volumes)?;
        let count = staged.len();
        for (temporary, item, relative) in staged {
            files::check_cancel(cancel)?;
            if files::regular_metadata(&temporary)?.len() != item.size {
                return Err(io::Error::other(
                    "Die entpackte Dateigröße weicht vom Archiv ab.",
                ));
            }
            files::create_or_verify_directory(&base)?;
            let mut parent = base.clone();
            for part in relative.parent().unwrap_or(Path::new("")).components() {
                parent.push(part);
                files::create_or_verify_directory(&parent)?;
            }
            zone.apply(&temporary)?;
            let file = fs::OpenOptions::new().write(true).open(&temporary)?;
            archive_view::preserve_timestamp(&file, item.modified)?;
            file.sync_all()?;
            drop(file);
            temporary
                .persist_noclobber(base.join(relative))
                .map_err(|error| error.error)?;
        }
        Ok(count)
    }
}

fn password_input(password: Option<&str>) -> io::Result<Option<zeroize::Zeroizing<Vec<u8>>>> {
    password
        .map(|password| {
            if password.len() > 1024 || password.contains('\0') {
                return Err(io::Error::other(
                    "Das Archivpasswort muss ohne NUL in höchstens 1024 UTF-8-Bytes passen.",
                ));
            }
            let data =
                zeroize::Zeroizing::new(serde_json::to_vec(password).map_err(io::Error::other)?);
            if data.len() > 4096 {
                return Err(io::Error::other(
                    "Die kodierte Passworteingabe überschreitet 4 KiB.",
                ));
            }
            Ok(data)
        })
        .transpose()
}

pub(crate) fn storage_parts(
    path: &Path,
    cancel: &AtomicBool,
) -> io::Result<Vec<(PathBuf, fs::Metadata)>> {
    if matches!(Format::of(path), Some(Format::Ace)) {
        crate::ace_volumes::snapshot(path, cancel)
    } else {
        let parts = crate::uc2_volumes::snapshot(path)?;
        if parts.is_empty() {
            crate::external_volumes::snapshot(path, cancel)
        } else {
            Ok(parts)
        }
    }
}

/// ACE compression is native; the pinned acefile adapter supplies its legacy
/// Blowfish primitive. The password travels only through an anonymous pipe.
pub fn pack_ace_encrypted(
    paths: &[PathBuf],
    base: &Path,
    target: &Path,
    mode: u8,
    password: &str,
    cancel: &AtomicBool,
) -> io::Result<()> {
    if password.is_empty() || password.len() > 50 || password.contains('\0') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "ACE-Passwörter benötigen 1–50 UTF-8-Bytes ohne NUL.",
        ));
    }
    let parent = target
        .parent()
        .ok_or_else(|| io::Error::other("Zielordner fehlt."))?;
    let workspace = tempfile::tempdir_in(parent)?;
    let plain = workspace.path().join("plain.ace");
    match mode {
        0 => pack_ace(paths, base, &plain, cancel)?,
        1 => pack_ace_compressed(paths, base, &plain, cancel)?,
        2 => pack_ace_blocked(paths, base, &plain, cancel)?,
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Unbekannte ACE-Kompressionsmethode.",
            ));
        }
    }
    let encrypted = workspace.path().join("encrypted.ace");
    let mut command = Format::Ace.command("encrypt", &plain)?;
    command.arg(&encrypted);
    capture_output_input(
        Format::Ace.label(),
        command,
        1024,
        cancel,
        password_input(Some(password))?,
    )?;
    Format::Ace.test_with_password(&encrypted, Some(password), cancel)?;
    files::check_cancel(cancel)?;
    tempfile::TempPath::try_from_path(encrypted)?
        .persist_noclobber(target)
        .map_err(|error| error.error)?;
    Ok(())
}

pub fn pack_uc2(
    paths: &[PathBuf],
    base: &Path,
    target: &Path,
    cancel: &AtomicBool,
) -> io::Result<()> {
    let plan = if paths.is_empty() {
        Vec::new()
    } else {
        archives::plan_sources(paths, base, Path::new(""), cancel)?
    };
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
    let working = tempfile::tempdir_in(parent)?;
    let staged = working.path().join("archive.uc2");
    let mut manifest = NamedTempFile::new_in(working.path())?;
    let entries: Vec<_> = plan
        .iter()
        .map(|(path, name, metadata)| {
            let modified = metadata
                .modified()
                .ok()
                .map(DateTime::<Local>::from)
                .map(|date| date.format("%Y-%m-%dT%H:%M:%S").to_string());
            (path, name, metadata.is_dir(), metadata.len(), modified)
        })
        .collect();
    serde_json::to_writer(&mut manifest, &entries).map_err(io::Error::other)?;
    manifest.flush()?;
    let mut command = Format::Uc2.command("pack", &staged)?;
    command.arg(manifest.path());
    capture_output(Format::Uc2.label(), command, 1024, cancel)?;
    for (path, _, metadata) in &plan {
        files::ensure_unchanged(path, metadata)?;
    }
    Format::Uc2.test(&staged, cancel)?;
    fs::OpenOptions::new()
        .write(true)
        .open(&staged)?
        .sync_all()?;
    files::check_cancel(cancel)?;
    tempfile::TempPath::try_from_path(staged)?
        .persist_noclobber(target)
        .map_err(|error| error.error)?;
    Ok(())
}
