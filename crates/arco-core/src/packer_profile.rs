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
mod tc;
pub use tc::{TcPackers, read_tc_packers};

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
            "zip"
                | "7z"
                | "tar"
                | "tgz"
                | "gz"
                | "rar"
                | "arj"
                | "lha"
                | "lzh"
                | "ace"
                | "uc2"
                | "exe"
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
        self.pack_with_password(sources, base, target, None, cancel)
    }

    pub fn pack_with_password(
        &self,
        sources: &[PathBuf],
        base: &Path,
        target: &Path,
        password: Option<&str>,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        self.pack_set(sources, base, target, password, cancel)
            .map(|_| ())
    }

    /// Returns every published part, with the actual entry point first.
    pub fn pack_set(
        &self,
        sources: &[PathBuf],
        base: &Path,
        target: &Path,
        password: Option<&str>,
        cancel: &AtomicBool,
    ) -> io::Result<Vec<PathBuf>> {
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
        let plan = if sources.is_empty() {
            Vec::new()
        } else {
            archives::plan_sources(sources, base, Path::new(""), cancel)?
        };
        let working = tempfile::tempdir_in(&parent)?;
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
        let lha32 = self
            .executable
            .file_stem()
            .is_some_and(|name| name.eq_ignore_ascii_case("lha32"));
        let selected = sources
            .iter()
            .map(|path| {
                path.strip_prefix(base).map(|relative| {
                    let relative = Path::new(".").join(relative);
                    if lha32 && path.is_dir() {
                        relative.join("*.*")
                    } else {
                        relative
                    }
                })
            })
            .collect::<Result<BTreeSet<_>, _>>()
            .map_err(io::Error::other)?;
        let output_name = format!("output.{}", self.extension);
        let output_arg = Path::new("..").join(&output_name);
        let mut command = Command::new(&self.executable);
        if password.is_some()
            && self.executable.file_stem().is_some_and(|name| {
                ["7z", "7zg", "rar", "winrar", "ace", "winace"]
                    .iter()
                    .any(|tool| name.eq_ignore_ascii_case(tool))
            })
            && !self
                .arguments
                .iter()
                .any(|argument| argument.starts_with("-p"))
        {
            command.arg("-p");
        }
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
        if sources.is_empty() {
            let empty = working.path().join(&output_name);
            match self.extension.as_str() {
                "rar" => {
                    let mut data = b"Rar!\x1a\x07\x01\0".to_vec();
                    for body in [[1u8, 0, 0], [5, 0, 0]] {
                        let mut header = vec![3];
                        header.extend_from_slice(&body);
                        data.extend_from_slice(&crc32fast::hash(&header).to_le_bytes());
                        data.extend(header);
                    }
                    fs::write(empty, data)?;
                }
                "arj" => {
                    let mut main = vec![0u8; 32];
                    main[..8].copy_from_slice(&[30, 3, 1, 0, 0, 0, 2, 0]);
                    let mut data = vec![0x60, 0xea, 32, 0];
                    data.extend_from_slice(&main);
                    data.extend_from_slice(&crc32fast::hash(&main).to_le_bytes());
                    data.extend_from_slice(&[0, 0, 0x60, 0xea, 0, 0]);
                    fs::write(empty, data)?;
                }
                "lha" | "lzh" => fs::write(empty, [0])?,
                _ => {
                    external_archives::capture_output_input(
                        "Externer Packer",
                        command,
                        1024 * 1024,
                        cancel,
                        external_archives::password_input(password, true)?,
                    )?;
                }
            }
        } else {
            external_archives::capture_output_input(
                "Externer Packer",
                command,
                1024 * 1024,
                cancel,
                external_archives::password_input(password, true)?,
            )?;
        }
        let mut archive = working.path().join(&output_name);
        if !archive.try_exists()? {
            let first = regex::Regex::new(
                r"(?i)^output(?:\.part0*1\.(?:rar|exe)|\.(?:zip|7z)\.001|(?:\.uc2)?\.p01)$",
            )
            .expect("fixed first-volume pattern");
            let candidates = fs::read_dir(working.path())?
                .filter_map(|entry| entry.ok())
                .filter(|entry| {
                    entry
                        .file_name()
                        .to_str()
                        .is_some_and(|name| first.is_match(name))
                })
                .map(|entry| entry.path())
                .collect::<Vec<_>>();
            if candidates.len() != 1 {
                return Err(io::Error::other(
                    "Der Packer hat keinen eindeutigen Archiveinstieg erzeugt.",
                ));
            }
            archive = candidates[0].clone();
        }
        let catalog = archive_view::Catalog::read_with_password(&archive, password, cancel)?;
        if password.is_some()
            && expected.iter().any(|(_, size)| size.is_some())
            && !catalog.contains_encrypted_entries()
        {
            return Err(io::Error::other(
                "Der Packer hat trotz gesetztem Passwort kein verschlüsseltes Archiv erzeugt. Prüfe seine Profilargumente.",
            ));
        }
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
            .search(Path::new(""), &query, password, cancel)?
            .entries
            .into_iter()
            .map(|entry| (entry.path.to_string_lossy().to_lowercase(), entry.size))
            .collect();
        if contents != expected {
            return Err(io::Error::other(
                "Einträge oder Dateigrößen des Archivs weichen von der Auswahl ab.",
            ));
        }
        archive_view::test(&archive, password, cancel)?;
        let parts = catalog
            .storage_paths()
            .map(Path::to_path_buf)
            .collect::<Vec<_>>();
        let target_stem = target
            .file_stem()
            .and_then(|stem| stem.to_str())
            .ok_or_else(|| io::Error::other("Archivname benötigt Unicode."))?;
        let rar_stem = regex::Regex::new(r"(?i)\.part[0-9]+$").expect("fixed RAR stem pattern");
        let target_stem = if self.extension == "rar" {
            rar_stem.replace(target_stem, "").into_owned()
        } else {
            target_stem.to_owned()
        };
        let mut replacements = Vec::new();
        for part in parts {
            files::check_cancel(cancel)?;
            if part.parent() != Some(working.path()) {
                return Err(io::Error::other(
                    "Ein Packerteil liegt außerhalb des Arbeitsordners.",
                ));
            }
            File::options().write(true).open(&part)?.sync_all()?;
            let name = part
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(|name| name.strip_prefix("output"))
                .ok_or_else(|| io::Error::other("Unerwarteter Name eines Packerteils."))?;
            let target = if part == archive && archive.file_name() == Some(OsStr::new(&output_name))
            {
                parent.join(target.file_name().expect("validated target name"))
            } else {
                parent.join(tools::output_name(&format!("{target_stem}{name}"))?)
            };
            replacements.push((part, target));
        }
        catalog.check_unchanged()?;
        let outputs = replacements
            .iter()
            .map(|(_, target)| target.clone())
            .collect();
        tools::publish_parts(replacements, cancel)?;
        Ok(outputs)
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
