use crate::{archive_view, archives, external_archives, files, tools};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    ffi::{OsStr, OsString},
    fs::{self, File},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::AtomicBool,
};
use tempfile::TempPath;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackerProfile {
    pub executable: PathBuf,
    pub extension: String,
    pub arguments: Vec<String>,
}

impl PackerProfile {
    pub fn validate(&self) -> io::Result<()> {
        if !self.executable.is_absolute()
            || (cfg!(windows)
                && !self
                    .executable
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("exe")))
        {
            return Err(io::Error::other(
                "Gib einen absoluten Pfad zu einer ausführbaren Datei an.",
            ));
        }
        if !matches!(
            self.extension.as_str(),
            "zip" | "7z" | "tar" | "gz" | "rar" | "arj" | "lha" | "lzh" | "ace" | "uc2"
        ) {
            return Err(io::Error::other(
                "Diese Archivendung ist für Packerprofile nicht verfügbar.",
            ));
        }
        let count = |value: &str| {
            self.arguments
                .iter()
                .filter(|argument| *argument == value)
                .count()
        };
        if self.arguments.len() > 64
            || self
                .arguments
                .iter()
                .any(|arg| arg.is_empty() || arg.len() > 4096 || arg.contains(['\0', '\n', '\r']))
            || count("{archive}") != 1
            || count("{sources}") + count("@{list}") != 1
        {
            return Err(io::Error::other(
                "Gib {archive} und entweder {sources} oder @{list} je einmal als eigenes Argument an.",
            ));
        }
        Ok(())
    }

    pub fn pack(
        &self,
        sources: &[PathBuf],
        base: &Path,
        target: &Path,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        self.validate()?;
        files::check_cancel(cancel)?;
        if !files::regular_metadata(&self.executable)?.is_file() {
            return Err(io::Error::other("Die Packer-EXE fehlt."));
        }
        if !target
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case(&self.extension))
        {
            return Err(io::Error::other(
                "Die Zielendung passt nicht zum Packerprofil.",
            ));
        }
        files::leaf_name(target.file_name().unwrap_or_default())?;
        let parent = fs::canonicalize(
            target
                .parent()
                .ok_or_else(|| io::Error::other("Zielordner fehlt."))?,
        )?;
        if target.try_exists()? {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "Das Zielarchiv existiert bereits.",
            ));
        }
        for source in sources {
            if parent.starts_with(fs::canonicalize(source)?) {
                return Err(io::Error::other("Das Zielarchiv liegt im Quellordner."));
            }
        }
        let plan = archives::plan_sources(sources, base, Path::new(""), cancel)?;
        let working = tempfile::tempdir_in(parent)?;
        let data = working.path().join("files");
        fs::create_dir(&data)?;
        let mut expected = BTreeSet::new();
        for (source, name, metadata) in plan {
            files::check_cancel(cancel)?;
            let path = archives::checked_archive_path(&name)?;
            let destination = data.join(&path);
            files::ensure_unchanged(&source, &metadata)?;
            if metadata.is_dir() {
                fs::create_dir_all(&destination)?;
            } else {
                fs::create_dir_all(destination.parent().expect("staging parent"))?;
                let mut output = File::options()
                    .create_new(true)
                    .write(true)
                    .open(&destination)?;
                let copied = tools::copy_cancelled(
                    File::open(&source)?.take(metadata.len().saturating_add(1)),
                    &mut output,
                    cancel,
                )?;
                if copied != metadata.len() {
                    return Err(io::Error::other(
                        "Eine Quelldatei hat sich beim Kopieren geändert.",
                    ));
                }
                archive_view::preserve_timestamp(&output, metadata.modified().ok())?;
            }
            files::ensure_unchanged(&source, &metadata)?;
            expected.insert((
                path.to_string_lossy().to_lowercase(),
                (!metadata.is_dir()).then_some(metadata.len()),
            ));
            for ancestor in path
                .ancestors()
                .skip(1)
                .filter(|path| !path.as_os_str().is_empty())
            {
                expected.insert((ancestor.to_string_lossy().to_lowercase(), None));
            }
        }
        let selected = sources
            .iter()
            .map(|path| {
                path.strip_prefix(base)
                    .map(|relative| Path::new(".").join(relative))
            })
            .collect::<Result<BTreeSet<_>, _>>()
            .map_err(io::Error::other)?;
        let output_name = format!("output.{}", self.extension);
        let output_arg = Path::new("..").join(&output_name);
        let mut command = Command::new(&self.executable);
        command.current_dir(&data).stdin(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000);
        }
        for argument in &self.arguments {
            match argument.as_str() {
                "{archive}" => {
                    command.arg(&output_arg);
                }
                "{sources}" => {
                    command.args(&selected);
                }
                "@{list}" => {
                    let mut list = File::create(working.path().join("sources.txt"))?;
                    for path in &selected {
                        writeln!(list, "{}", path.display())?;
                    }
                    let mut arg = OsString::from("@");
                    arg.push(Path::new("..").join("sources.txt"));
                    command.arg(arg);
                }
                argument => {
                    command.arg(OsStr::new(argument));
                }
            }
        }
        external_archives::capture_output("Externer Packer", command, 1024 * 1024, cancel)?;
        let archive = working.path().join(output_name);
        let catalog = archive_view::Catalog::read(&archive, cancel)?;
        let query = tools::Search {
            names: files::compile_masks("")?,
            text: None,
            min_size: None,
            max_size: None,
            max_depth: None,
            show_hidden: true,
            ignore: None,
        };
        let contents: BTreeSet<_> = catalog
            .search(Path::new(""), &query, None, cancel)?
            .entries
            .into_iter()
            .map(|entry| (entry.path.to_string_lossy().to_lowercase(), entry.size))
            .collect();
        if contents != expected {
            return Err(io::Error::other(
                "Einträge oder Dateigrößen des Archivs weichen von der Auswahl ab.",
            ));
        }
        archive_view::test(&archive, None, cancel)?;
        File::options().write(true).open(&archive)?.sync_all()?;
        files::check_cancel(cancel)?;
        TempPath::try_from_path(archive)?
            .persist_noclobber(target)
            .map_err(|error| error.error)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires ARCO_7ZIP_PATH to point to 7-Zip"]
    fn packs_with_separate_arguments_and_utf8_list() {
        let Some(executable) = std::env::var_os("ARCO_7ZIP_PATH") else {
            return;
        };
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path();
        let source = root.join("source");
        let selected = source.join("-Größe [Probe]");
        fs::create_dir_all(&selected).unwrap();
        fs::create_dir(selected.join("Leer")).unwrap();
        fs::write(selected.join("@Daten.txt"), b"external profile data").unwrap();
        let cancel = AtomicBool::new(false);
        for (index, placeholder) in ["{sources}", "@{list}"].into_iter().enumerate() {
            let profile = PackerProfile {
                executable: executable.clone().into(),
                extension: "zip".into(),
                arguments: ["a", "-tzip", "-spd", "-scsUTF-8", "{archive}", placeholder]
                    .into_iter()
                    .map(str::to_owned)
                    .collect(),
            };
            let archive = root.join(format!("profile{index}.zip"));
            let paths = std::slice::from_ref(&selected);
            profile.pack(paths, &source, &archive, &cancel).unwrap();
            let output = tempfile::tempdir().unwrap();
            archive_view::extract_all(&archive, output.path(), None, &cancel).unwrap();
            assert_eq!(
                fs::read(output.path().join("-Größe [Probe]/@Daten.txt")).unwrap(),
                b"external profile data"
            );
            assert!(output.path().join("-Größe [Probe]/Leer").is_dir());
            assert!(profile.pack(paths, &source, &archive, &cancel).is_err());
        }
    }
}
