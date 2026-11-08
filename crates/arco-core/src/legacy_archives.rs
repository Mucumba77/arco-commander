pub use crate::ace::pack as pack_ace;
pub use crate::ace::pack_blocked as pack_ace_blocked;
pub use crate::ace::pack_compressed as pack_ace_compressed;
pub use crate::uc2_volumes::pack_sas as pack_uc2_sas;
pub use crate::uc2_volumes::pack_sea as pack_uc2_sea;
use crate::{
    archive_view::{self, Item, Selection},
    archives,
    external_archives::{capture_output, stream_output},
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
        if ext.eq_ignore_ascii_case("ace") {
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
        let original = files::regular_metadata(path)?;
        let volumes = crate::uc2_volumes::snapshot(path)?;
        let entries = self.list(path, cancel)?;
        let entries = archive_view::project(
            entries,
            None,
            |entry| (&entry.path, entry.size, entry.directory),
            cancel,
        )?;
        if entries.iter().any(|(entry, _)| entry.encrypted) {
            return Err(io::Error::other(
                "Verschlüsselte ACE-Einträge werden derzeit nicht geprüft.",
            ));
        }
        capture_output(self.label(), self.command("test", path)?, 1024, cancel)?;
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

    pub(crate) fn extract(
        self,
        path: &Path,
        destination: &Path,
        selection: Option<&Selection>,
        cancel: &AtomicBool,
    ) -> io::Result<usize> {
        let original = files::regular_metadata(path)?;
        let volumes = crate::uc2_volumes::snapshot(path)?;
        let entries = self.list(path, cancel)?.into_iter().enumerate().collect();
        let entries = archive_view::project(
            entries,
            selection,
            |(_, item)| (&item.path, item.size, item.directory),
            cancel,
        )?;
        if entries.iter().any(|((_, item), _)| item.encrypted) {
            return Err(io::Error::other(
                "Verschlüsselte ACE-Einträge werden derzeit nicht entpackt.",
            ));
        }
        let base = archive_view::check_destination(
            destination,
            &entries,
            |(_, item)| item.directory,
            cancel,
        )?;
        let zone = crate::archive_zone::ArchiveZone::read(path, cancel)?;
        let mut count = 0;
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
            files::ensure_unchanged(path, &original)?;
            crate::uc2_volumes::unchanged(&volumes)?;
            let staged = NamedTempFile::new_in(&parent)?;
            let mut command = self.command("extract", path)?;
            command.arg(index.to_string());
            stream_output(self.label(), command, staged.reopen()?, item.size, cancel)?;
            files::ensure_unchanged(path, &original)?;
            crate::uc2_volumes::unchanged(&volumes)?;
            zone.apply(staged.path())?;
            archive_view::preserve_timestamp(staged.as_file(), item.modified)?;
            staged.as_file().sync_all()?;
            files::check_cancel(cancel)?;
            staged
                .persist_noclobber(base.join(relative))
                .map_err(|error| error.error)?;
            count += 1;
        }
        Ok(count)
    }
}

pub fn pack_uc2(
    paths: &[PathBuf],
    base: &Path,
    target: &Path,
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
