use crate::{archives, files};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, SystemTime};
use tempfile::{NamedTempFile, TempPath};
use walkdir::WalkDir;

const MINIMUM_7ZIP: (u32, u32) = (26, 5);
const LIST_LIMIT: u64 = 24 * 1024 * 1024;
const ENTRY_LIMIT: usize = 500_000;
const EXTRACT_LIMIT: u64 = 256 * 1024 * 1024 * 1024;

pub struct ExternalArchiver {
    executable: PathBuf,
}

#[derive(Debug)]
pub(crate) struct HeaderPassword;
impl std::fmt::Display for HeaderPassword {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Das Archiv konnte nicht ohne Passwort gelesen werden. Prüfe Passwort, Dateinamenverschlüsselung und Archivdaten.")
    }
}
impl std::error::Error for HeaderPassword {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Format {
    SevenZip,
    SevenZipSfx,
    Tar,
    TarGzip,
    Gzip,
}

pub struct Entry {
    raw_path: String,
    pub path: PathBuf,
    pub directory: bool,
    pub size: u64,
    pub encrypted: bool,
    pub modified: Option<SystemTime>,
}

pub fn is_supported(path: &Path) -> bool {
    is_first_volume(path)
        || path
            .extension()
            .and_then(|value| value.to_str())
            .is_some_and(|value| {
                matches!(
                    value.to_ascii_lowercase().as_str(),
                    "7z" | "arj"
                        | "lzh"
                        | "lha"
                        | "rar"
                        | "cab"
                        | "tar"
                        | "gz"
                        | "tgz"
                        | "xz"
                        | "bz2"
                        | "zst"
                        | "tzst"
                )
            })
}

pub(crate) fn is_empty_lha(path: &Path) -> bool {
    path.extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("lha") || ext.eq_ignore_ascii_case("lzh"))
        && files::regular_metadata(path).is_ok_and(|metadata| metadata.len() == 1)
        && fs::read(path).is_ok_and(|bytes| bytes == [0])
}

pub(crate) fn is_first_volume(path: &Path) -> bool {
    crate::archive_parts::kind(path).is_some()
}

pub fn is_sfx(path: &Path) -> bool {
    sfx_offset(path).is_some()
}

pub(crate) fn sfx_offset(path: &Path) -> Option<u64> {
    if !path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("exe"))
    {
        return None;
    }
    let Ok(file) = File::open(path) else {
        return None;
    };
    let Ok(metadata) = file.metadata() else {
        return None;
    };
    let mut bytes = Vec::new();
    if file.take(2 * 1024 * 1024).read_to_end(&mut bytes).is_err() || !bytes.starts_with(b"MZ") {
        return None;
    }
    bytes.windows(32).enumerate().find_map(|(offset, header)| {
        if !header.starts_with(b"7z\xbc\xaf\x27\x1c") {
            return None;
        }
        let word =
            |range: std::ops::Range<usize>| u64::from_le_bytes(header[range].try_into().unwrap());
        let start_crc = u32::from_le_bytes(header[8..12].try_into().unwrap());
        (crc32fast::hash(&header[12..32]) == start_crc
            && (offset as u64 + 32)
                .checked_add(word(12..20))
                .and_then(|next| next.checked_add(word(20..28)))
                .is_some_and(|end| end <= metadata.len()))
        .then_some(offset as u64)
    })
}

fn configured_executable() -> Option<PathBuf> {
    std::env::var_os("ARCO_7ZIP_PATH")
        .map(PathBuf::from)
        .or_else(|| {
            let program = std::env::current_exe().ok()?;
            let adjacent = program.parent()?.join("7zip").join("7z.exe");
            adjacent.is_file().then_some(adjacent)
        })
        .or_else(|| {
            std::env::var_os("ProgramFiles")
                .map(|folder| PathBuf::from(folder).join("7-Zip").join("7z.exe"))
        })
}

pub(crate) fn sfx_module() -> io::Result<PathBuf> {
    std::env::var_os("ARCO_7ZIP_SFX")
        .map(PathBuf::from)
        .or_else(|| configured_executable().map(|path| path.with_file_name("7z.sfx")))
        .filter(|module| module.is_absolute() && module.is_file())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "Das Modul 7z.sfx fehlt neben 7z.exe. ARCO_7ZIP_SFX kann auf das Modul zeigen.",
            )
        })
}

impl ExternalArchiver {
    pub(crate) fn pack_zip_options(
        &self,
        paths: &[PathBuf],
        base: &Path,
        target: &Path,
        password: Option<&str>,
        settings: crate::archive_settings::Settings,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        settings.validate()?;
        let mut arguments: Vec<String> = ["a", "-tzip", "-mm=LZMA", "-spd", "-y"]
            .into_iter()
            .map(str::to_owned)
            .collect();
        arguments.push(format!("-mx={}", settings.zip_level));
        if password.is_some() {
            use crate::archive_settings::ZipCipher;
            let method = match settings.zip_cipher {
                ZipCipher::Aes256 => "AES256",
                ZipCipher::Aes192 => "AES192",
                ZipCipher::Aes128 => "AES128",
                ZipCipher::ZipCrypto => "ZipCrypto",
            };
            arguments.extend([format!("-mem={method}"), "-p".into()]);
        }
        if settings.newest_timestamp {
            arguments.push("-stl".into());
        }
        arguments.extend(["{archive}".into(), "{sources}".into()]);
        crate::packer_profile::PackerProfile {
            executable: self.executable.clone(),
            extension: "zip".into(),
            arguments,
        }
        .pack_with_password(paths, base, target, password, cancel)
    }
    pub fn discover(cancel: &AtomicBool) -> io::Result<Self> {
        let executable = configured_executable()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "7-Zip wurde nicht gefunden"))?;
        Self::open(executable, cancel)
    }

    pub fn open(executable: PathBuf, cancel: &AtomicBool) -> io::Result<Self> {
        if !executable.is_absolute() || !executable.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "7-Zip fehlt. Installiere 7-Zip oder setze ARCO_7ZIP_PATH auf die EXE.",
            ));
        }
        let archiver = Self { executable };
        let mut command = archiver.command();
        command.arg("i");
        let output = capture(command, 2 * 1024 * 1024, cancel)?;
        let version = String::from_utf8_lossy(&output)
            .lines()
            .find_map(parse_version);
        if version.is_none_or(|version| version < MINIMUM_7ZIP) {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Der Archiv-Adapter benötigt 7-Zip 26.05 oder neuer.",
            ));
        }
        Ok(archiver)
    }

    pub fn list(&self, archive: &Path, cancel: &AtomicBool) -> io::Result<Vec<Entry>> {
        self.list_with_password(archive, None, cancel)
    }

    pub(crate) fn pack_seven_options(
        &self,
        paths: &[PathBuf],
        base: &Path,
        target: &Path,
        password: Option<&str>,
        settings: crate::archive_settings::Settings,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        use crate::archive_settings::SevenZipMethod;
        settings.validate()?;
        let method = match settings.seven_zip_method {
            SevenZipMethod::Lzma2 => "LZMA2",
            SevenZipMethod::Lzma => "LZMA",
            SevenZipMethod::Ppmd => "PPMd",
            SevenZipMethod::Bzip2 => "BZip2",
        };
        let mut arguments = vec![
            "a".into(),
            "-t7z".into(),
            "-spd".into(),
            "-y".into(),
            format!("-mx={}", settings.seven_zip_level),
            format!("-m0={method}"),
            format!(
                "-mmt={}",
                if settings.threads == 0 {
                    "on".into()
                } else {
                    settings.threads.to_string()
                }
            ),
        ];
        arguments.push(if !settings.solid {
            "-ms=off".into()
        } else if settings.solid_block_mib == 0 {
            "-ms=on".into()
        } else {
            format!("-ms={}m", settings.solid_block_mib)
        });
        if settings.dictionary_kib != 0 {
            arguments.push(format!(
                "-m{}={}k",
                if settings.seven_zip_method == SevenZipMethod::Ppmd {
                    "mem"
                } else {
                    "d"
                },
                settings.dictionary_kib
            ));
        }
        if settings.word_size != 0 {
            arguments.push(format!(
                "-m{}={}",
                if settings.seven_zip_method == SevenZipMethod::Ppmd {
                    "o"
                } else {
                    "fb"
                },
                settings.word_size
            ));
        }
        if password.is_some() {
            arguments.push(format!(
                "-mhe={}",
                if settings.encrypt_headers {
                    "on"
                } else {
                    "off"
                }
            ));
        }
        if settings.newest_timestamp {
            arguments.push("-stl".into());
        }
        arguments.extend(["{archive}".into(), "{sources}".into()]);
        crate::packer_profile::PackerProfile {
            executable: self.executable.clone(),
            extension: "7z".into(),
            arguments,
        }
        .pack_with_password(paths, base, target, password, cancel)
    }

    pub(crate) fn list_with_password(
        &self,
        archive: &Path,
        password: Option<&str>,
        cancel: &AtomicBool,
    ) -> io::Result<Vec<Entry>> {
        let mut command = self.command();
        command
            .args(["l", "-slt", "-ba", "-sccUTF-8", "--"])
            .arg(archive);
        let listing = capture_output_input(
            "7-Zip",
            command,
            LIST_LIMIT,
            cancel,
            password_input(password, false)?,
        )
        .map_err(|error| {
            if password.is_none()
                && error.kind() != io::ErrorKind::Interrupted
                && crate::external_volumes::rar_headers_encrypted(archive)
            {
                io::Error::other(HeaderPassword)
            } else {
                error
            }
        })?;
        if let Some(name) = stream_name(archive)? {
            let text = std::str::from_utf8(&listing).map_err(io::Error::other)?;
            let sizes = text
                .lines()
                .filter_map(|line| line.strip_prefix("Size = "))
                .collect::<Vec<_>>();
            let gzip = archive.extension().is_some_and(|ext| {
                ext.eq_ignore_ascii_case("gz") || ext.eq_ignore_ascii_case("tgz")
            });
            let size = if gzip {
                let mut command = self.command();
                command.args(["x", "-so", "-spd", "--"]).arg(archive);
                measure_stream(command, cancel)?
            } else {
                match sizes.as_slice() {
                    [""] | [] => {
                        let mut command = self.command();
                        command.args(["x", "-so", "-spd", "--"]).arg(archive);
                        measure_stream(command, cancel)?
                    }
                    [value] => value
                        .parse::<u64>()
                        .map_err(|_| io::Error::other("archive stream has no verified size"))?,
                    _ => {
                        return Err(io::Error::other(
                            "a compressed stream must have one payload",
                        ));
                    }
                }
            };
            let entries = parse_list_fallback(&listing, Some((&name, size)))?;
            if entries.len() != 1 || entries[0].directory || entries[0].encrypted {
                return Err(io::Error::other("invalid compressed-stream catalog"));
            }
            Ok(entries)
        } else {
            parse_list(&listing)
        }
    }

    pub fn test(&self, archive: &Path, cancel: &AtomicBool) -> io::Result<usize> {
        self.test_with_password(archive, None, cancel)
    }

    pub(crate) fn test_with_password(
        &self,
        archive: &Path,
        password: Option<&str>,
        cancel: &AtomicBool,
    ) -> io::Result<usize> {
        let original = files::regular_metadata(archive)?;
        let volumes = crate::external_volumes::snapshot(archive, cancel)?;
        let entries = self.list_with_password(archive, password, cancel)?;
        let mut command = self.command();
        command.args(["t", "-bso0", "--"]).arg(archive);
        capture_output_input(
            "7-Zip",
            command,
            64 * 1024,
            cancel,
            password_input(password, false)?,
        )?;
        files::ensure_unchanged(archive, &original)?;
        crate::uc2_volumes::unchanged(&volumes)?;
        Ok(entries.len())
    }

    pub fn extract(
        &self,
        archive: &Path,
        destination: &Path,
        cancel: &AtomicBool,
    ) -> io::Result<usize> {
        self.extract_selection(archive, destination, None, cancel)
    }

    pub(crate) fn extract_selection(
        &self,
        archive: &Path,
        destination: &Path,
        selection: Option<&crate::archive_view::Selection>,
        cancel: &AtomicBool,
    ) -> io::Result<usize> {
        self.extract_with_zone(
            archive,
            destination,
            selection,
            None,
            cancel,
            Default::default(),
        )
    }

    pub(crate) fn extract_with_zone(
        &self,
        archive: &Path,
        destination: &Path,
        selection: Option<&crate::archive_view::Selection>,
        password: Option<&str>,
        cancel: &AtomicBool,
        policy: crate::archive_zone::ZonePolicy,
    ) -> io::Result<usize> {
        let original = files::regular_metadata(archive)?;
        let volumes = crate::external_volumes::snapshot(archive, cancel)?;
        let entries = self.list_with_password(archive, password, cancel)?;
        let entries = crate::archive_view::project(
            entries,
            selection,
            |entry| (&entry.path, entry.size, entry.directory),
            cancel,
        )?;
        if password.is_none() && entries.iter().any(|(entry, _)| entry.encrypted) {
            return Err(io::Error::other(
                "Für die verschlüsselten Archiveinträge ist ein Passwort erforderlich.",
            ));
        }
        let base = crate::archive_view::check_destination(
            destination,
            &entries,
            |entry| entry.directory,
            cancel,
        )?;
        let zone = crate::archive_zone::ArchiveZone::read_with_policy(archive, cancel, policy)?;
        let mut completed = 0;
        for (entry, relative) in entries {
            files::check_cancel(cancel)?;
            files::ensure_unchanged(archive, &original)?;
            crate::uc2_volumes::unchanged(&volumes)?;
            let target = base.join(&relative);
            let parent = if entry.directory {
                &relative
            } else {
                relative.parent().unwrap_or(Path::new(""))
            };
            let mut current = base.clone();
            for component in parent.components() {
                current.push(component);
                files::create_or_verify_directory(&current)?;
            }
            if entry.directory {
                continue;
            }
            let output = NamedTempFile::new_in(current)?;
            let mut command = self.command();
            command.args(["x", "-so", "-spd", "--"]).arg(archive);
            if !entry.raw_path.is_empty() {
                command.arg(&entry.raw_path);
            }
            stream_output_input(
                "7-Zip",
                command,
                output.reopen()?,
                entry.size,
                cancel,
                password_input(password, false)?,
            )?;
            files::ensure_unchanged(archive, &original)?;
            crate::uc2_volumes::unchanged(&volumes)?;
            zone.apply(output.path())?;
            crate::archive_view::preserve_timestamp(output.as_file(), entry.modified)?;
            output.as_file().sync_all()?;
            files::check_cancel(cancel)?;
            output
                .persist_noclobber(&target)
                .map_err(|error| error.error)?;
            completed += 1;
        }
        Ok(completed)
    }

    pub fn pack(
        &self,
        paths: &[PathBuf],
        base: &Path,
        target: &Path,
        format: Format,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        self.pack_with_options(paths, base, target, format, Default::default(), cancel)
    }

    pub fn pack_with_options(
        &self,
        paths: &[PathBuf],
        base: &Path,
        target: &Path,
        format: Format,
        settings: crate::archive_settings::Settings,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        settings.validate()?;
        if format == Format::TarGzip {
            let parent = target
                .parent()
                .ok_or_else(|| io::Error::other("Zielordner fehlt."))?;
            let working = tempfile::tempdir_in(parent)?;
            let tar = working.path().join("archive.tar");
            self.pack_with_options(paths, base, &tar, Format::Tar, settings, cancel)?;
            let gzip = working.path().join("archive.tgz");
            self.pack_with_options(
                &[tar],
                working.path(),
                &gzip,
                Format::Gzip,
                settings,
                cancel,
            )?;
            crate::archive_view::test(&gzip, None, cancel)?;
            files::check_cancel(cancel)?;
            return TempPath::try_from_path(gzip)?
                .persist_noclobber(target)
                .map(|_| ())
                .map_err(|error| error.error);
        }
        if paths.is_empty() && format == Format::Tar {
            let parent = target
                .parent()
                .ok_or_else(|| io::Error::other("Zielordner fehlt."))?;
            let mut empty = NamedTempFile::new_in(parent)?;
            empty.write_all(&[0; 1024])?;
            empty.as_file().sync_all()?;
            files::check_cancel(cancel)?;
            return empty
                .persist_noclobber(target)
                .map(|_| ())
                .map_err(|error| error.error);
        }
        if paths.is_empty() || (format == Format::Gzip && (paths.len() != 1 || !paths[0].is_file()))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "GZIP benötigt genau eine Datei. 7z und TAR benötigen mindestens einen Eintrag.",
            ));
        }
        let parent = fs::canonicalize(
            target
                .parent()
                .ok_or_else(|| io::Error::other("missing output directory"))?,
        )?;
        let target = parent.join(
            target
                .file_name()
                .ok_or_else(|| io::Error::other("missing archive name"))?,
        );
        if target.try_exists()? {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                target.display().to_string(),
            ));
        }
        let base = fs::canonicalize(base)?;
        let mut items = Vec::new();
        let mut snapshots = Vec::new();
        for path in paths {
            files::check_cancel(cancel)?;
            let metadata = files::regular_metadata(path)?;
            let path = fs::canonicalize(path)?;
            if metadata.is_dir() && parent.starts_with(&path) {
                return Err(io::Error::other("das Zielarchiv liegt im Quellordner"));
            }
            let relative = path.strip_prefix(&base).map_err(io::Error::other)?;
            let relative = relative
                .to_str()
                .ok_or_else(|| io::Error::other("7-Zip list files require Unicode filenames"))?;
            if relative.is_empty() || relative.contains(['\n', '\r']) {
                return Err(io::Error::other("invalid archive source"));
            }
            for item in WalkDir::new(&path).follow_links(false) {
                files::check_cancel(cancel)?;
                let item = item?;
                snapshots.push((
                    item.path().to_path_buf(),
                    files::regular_metadata(item.path())?,
                ));
                if snapshots.len() > ENTRY_LIMIT {
                    return Err(io::Error::other("Zu viele Archivquellen."));
                }
            }
            items.push(relative.to_string());
        }
        let working = tempfile::tempdir_in(&parent)?;
        let list = working.path().join("source-list.txt");
        fs::write(&list, items.join("\n"))?;
        let (format_flag, staging_name) = match format {
            Format::SevenZip => ("-t7z", "archive-stage.7z"),
            Format::SevenZipSfx => ("-t7z", "archive-stage.exe"),
            Format::Tar => ("-ttar", "archive-stage.tar"),
            Format::TarGzip => unreachable!("compressed TAR handled before source planning"),
            Format::Gzip => ("-tgzip", "archive-stage.gz"),
        };
        let stage = working.path().join(staging_name);
        let mut list_argument = OsString::from("@");
        list_argument.push(&list);
        let mut command = self.command();
        command
            .current_dir(base)
            .args(["a", format_flag, "-spd", "-scsUTF-8", "-bso0"]);
        if format == Format::Gzip {
            command.arg(format!("-mx={}", settings.gzip_level));
        }
        if format == Format::Tar {
            command.arg(
                if settings.tar_dialect == crate::archive_settings::TarDialect::Gnu {
                    "-mm=gnu"
                } else {
                    "-mm=posix"
                },
            );
        }
        if format == Format::SevenZipSfx {
            let module = std::env::var_os("ARCO_7ZIP_SFX")
                .map(PathBuf::from)
                .unwrap_or_else(|| self.executable.with_file_name("7z.sfx"));
            if !module.is_absolute() || !module.is_file() {
                return Err(io::Error::other(
                    "Das 7-Zip-SFX-Modul 7z.sfx fehlt neben 7z.exe. Alternativ kann ARCO_7ZIP_SFX auf das Modul zeigen.",
                ));
            }
            let mut argument = OsString::from("-sfx");
            argument.push(module);
            command.arg(argument);
        }
        command.arg(&stage).arg(list_argument);
        capture(command, 64 * 1024, cancel)?;
        if format == Format::SevenZipSfx && !is_sfx(&stage) {
            return Err(io::Error::other(
                "7-Zip hat kein gültiges Windows-SFX-Archiv erzeugt.",
            ));
        }
        self.test(&stage, cancel)?;
        for (path, metadata) in &snapshots {
            files::check_cancel(cancel)?;
            files::ensure_unchanged(path, metadata)?;
        }
        let newest = snapshots
            .iter()
            .filter(|(_, metadata)| metadata.is_file())
            .filter_map(|(_, metadata)| metadata.modified().ok())
            .max();
        settings.finish(&stage, newest)?;
        fs::OpenOptions::new()
            .write(true)
            .open(&stage)?
            .sync_all()?;
        files::check_cancel(cancel)?;
        TempPath::try_from_path(stage)?
            .persist_noclobber(target)
            .map_err(|error| error.error)?;
        Ok(())
    }

    fn command(&self) -> Command {
        let mut command = Command::new(&self.executable);
        command.args(["-bsp0", "-bd"]).stdin(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        command
    }
}

fn parse_version(line: &str) -> Option<(u32, u32)> {
    let version = line.strip_prefix("7-Zip ")?.split_whitespace().next()?;
    let (major, minor) = version.split_once('.')?;
    Some((major.parse().ok()?, minor.parse().ok()?))
}

fn stream_name(path: &Path) -> io::Result<Option<String>> {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if !matches!(
        extension.as_str(),
        "bz2" | "xz" | "gz" | "tgz" | "zst" | "tzst"
    ) {
        return Ok(None);
    }
    let mut magic = Vec::new();
    File::open(path)?.take(6).read_to_end(&mut magic)?;
    let recognized = match extension.as_str() {
        "bz2" => {
            magic.starts_with(b"BZh")
                && magic
                    .get(3)
                    .is_some_and(|byte| (b'1'..=b'9').contains(byte))
        }
        "xz" => magic.starts_with(b"\xfd7zXZ\0"),
        "gz" | "tgz" => magic.starts_with(&[0x1f, 0x8b, 8]),
        "zst" | "tzst" => magic.starts_with(&[0x28, 0xb5, 0x2f, 0xfd]),
        _ => false,
    };
    if !recognized {
        return Ok(None);
    }
    let mut name = path
        .file_stem()
        .and_then(|name| name.to_str())
        .ok_or_else(|| io::Error::other("Der Stream benötigt einen gültigen Unicode-Dateinamen."))?
        .to_owned();
    if matches!(extension.as_str(), "tgz" | "tzst") {
        name.push_str(".tar");
    }
    archives::checked_archive_path(&name)?;
    Ok(Some(name))
}

fn measure_stream(mut command: Command, cancel: &AtomicBool) -> io::Result<u64> {
    files::check_cancel(cancel)?;
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let output = child.stdout.take().expect("stdout piped");
    let reader = std::thread::spawn(move || {
        let size = io::copy(&mut output.take(EXTRACT_LIMIT + 1), &mut io::sink())?;
        if size > EXTRACT_LIMIT {
            return Err(io::Error::other(
                "Der komprimierte Stream überschreitet das Größenlimit.",
            ));
        }
        Ok(size)
    });
    let status = wait("7-Zip", &mut child, cancel);
    let size = reader
        .join()
        .map_err(|_| io::Error::other("Stream-Größenprüfung fehlgeschlagen"))??;
    status?;
    Ok(size)
}

fn parse_list(bytes: &[u8]) -> io::Result<Vec<Entry>> {
    parse_list_fallback(bytes, None)
}

fn parse_list_fallback(bytes: &[u8], fallback: Option<(&str, u64)>) -> io::Result<Vec<Entry>> {
    let text = std::str::from_utf8(bytes).map_err(io::Error::other)?;
    let mut entries = Vec::new();
    let mut fields = BTreeMap::new();
    let mut names = BTreeMap::new();
    let mut total = 0u64;
    for line in text.lines().chain(Some("")) {
        // The console reader prints this prompt on stdout even with stdin
        // redirected. It is protocol framing, never an archive field.
        if line.trim() == "Enter password (will not be echoed):" {
            continue;
        }
        if !line.is_empty() {
            let (key, value) = line
                .split_once(" = ")
                .ok_or_else(|| io::Error::other("invalid 7-Zip listing"))?;
            if fields.insert(key, value).is_some() {
                return Err(io::Error::other("duplicate field in 7-Zip listing"));
            }
            continue;
        }
        if fields.is_empty() {
            continue;
        }
        let original = fields.get("Path").copied().filter(|path| !path.is_empty());
        let name = original
            .or_else(|| fallback.map(|(name, _)| name))
            .ok_or_else(|| io::Error::other("archive entry has no path"))?;
        let attributes = fields.get("Attributes").copied().unwrap_or_default();
        let symbolic = ["Symbolic Link", "Hard Link", "Alternate Stream"]
            .iter()
            .any(|field| {
                fields
                    .get(field)
                    .is_some_and(|value| !matches!(*value, "" | "-"))
            });
        if symbolic
            || attributes
                .split_whitespace()
                .any(|value| value.starts_with('l') && value.len() >= 10)
        {
            return Err(io::Error::other(
                "archive contains a link or alternate data stream",
            ));
        }
        let directory = fields.get("Folder") == Some(&"+")
            || attributes.starts_with('D')
            || fields.get("Mode").is_some_and(|mode| mode.starts_with('d'));
        let size = fields
            .get("Size")
            .filter(|value| !value.is_empty())
            .and_then(|value| value.parse::<u64>().ok())
            .or_else(|| {
                fields
                    .get("Size")
                    .is_none_or(|value| value.is_empty())
                    .then(|| fallback.map(|(_, size)| size))
                    .flatten()
            })
            .ok_or_else(|| io::Error::other("archive entry has no verified size"))?;
        let normalized = name.replace('\\', "/");
        let normalized = if directory {
            normalized.trim_end_matches('/')
        } else {
            &normalized
        };
        let path = archives::checked_archive_path(normalized)?;
        if names.insert(normalized.to_lowercase(), directory).is_some() {
            return Err(io::Error::other("archive contains colliding names"));
        }
        total = total
            .checked_add(size)
            .ok_or_else(|| io::Error::other("archive size overflow"))?;
        if total > EXTRACT_LIMIT || entries.len() == ENTRY_LIMIT {
            return Err(io::Error::other(
                "archive exceeds the extraction size limit",
            ));
        }
        entries.push(Entry {
            raw_path: original.unwrap_or_default().to_string(),
            path,
            directory,
            size,
            encrypted: fields.get("Encrypted") == Some(&"+"),
            modified: fields
                .get("Modified")
                .and_then(|text| {
                    chrono::NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M:%S%.f").ok()
                })
                .and_then(crate::archive_view::local_timestamp),
        });
        fields.clear();
    }
    for key in names.keys() {
        if key
            .match_indices('/')
            .any(|(split, _)| names.get(&key[..split]) == Some(&false))
        {
            return Err(io::Error::other(
                "an archive path is both a file and a directory",
            ));
        }
    }
    Ok(entries)
}

fn capture(command: Command, limit: u64, cancel: &AtomicBool) -> io::Result<Vec<u8>> {
    capture_output("7-Zip", command, limit, cancel)
}

pub(crate) fn capture_output(
    tool: &str,
    command: Command,
    limit: u64,
    cancel: &AtomicBool,
) -> io::Result<Vec<u8>> {
    capture_output_input(tool, command, limit, cancel, None)
}

pub(crate) fn capture_output_input(
    tool: &str,
    mut command: Command,
    limit: u64,
    cancel: &AtomicBool,
    input: Option<zeroize::Zeroizing<Vec<u8>>>,
) -> io::Result<Vec<u8>> {
    files::check_cancel(cancel)?;
    if input.is_some() {
        command.stdin(Stdio::piped());
    }
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    send_input(&mut child, input)?;
    let stdout = child.stdout.take().expect("stdout piped");
    let reader = std::thread::spawn(move || {
        let mut data = Vec::new();
        stdout.take(limit + 1).read_to_end(&mut data)?;
        if data.len() as u64 > limit {
            return Err(io::Error::other(
                "Die Ausgabe des Archiv-Adapters überschreitet das Limit.",
            ));
        }
        Ok(data)
    });
    let status = wait(tool, &mut child, cancel);
    let bytes = reader
        .join()
        .map_err(|_| io::Error::other("archive reader failed"))?;
    status?;
    bytes
}

pub(crate) fn stream_output_input(
    tool: &str,
    mut command: Command,
    mut output: File,
    expected: u64,
    cancel: &AtomicBool,
    input: Option<zeroize::Zeroizing<Vec<u8>>>,
) -> io::Result<()> {
    files::check_cancel(cancel)?;
    if input.is_some() {
        command.stdin(Stdio::piped());
    }
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    send_input(&mut child, input)?;
    let stdout = child.stdout.take().expect("stdout piped");
    let reader = std::thread::spawn(move || {
        let length = io::copy(&mut stdout.take(expected + 1), &mut output)?;
        output.flush()?;
        Ok::<_, io::Error>(length)
    });
    let status = wait(tool, &mut child, cancel);
    let length = reader
        .join()
        .map_err(|_| io::Error::other("archive stream failed"))?;
    status?;
    if length? != expected {
        return Err(io::Error::other("archive entry size mismatch"));
    }
    Ok(())
}

fn send_input(child: &mut Child, input: Option<zeroize::Zeroizing<Vec<u8>>>) -> io::Result<()> {
    if let Some(input) = input {
        // Bound the pipe write; passwords never enter command lines, logs or files.
        let result = if input.len() > 4096 {
            Err(io::Error::other("Die Adaptereingabe überschreitet 4 KiB."))
        } else {
            match child.stdin.take() {
                Some(mut pipe) => pipe.write_all(&input),
                None => Err(io::Error::other("Adaptereingabe fehlt.")),
            }
        };
        if let Err(error) = result {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
    }
    Ok(())
}

pub(crate) fn password_input(
    password: Option<&str>,
    confirm: bool,
) -> io::Result<Option<zeroize::Zeroizing<Vec<u8>>>> {
    password
        .map(|password| {
            if password.is_empty() || password.len() > 1024 || password.contains(['\0', '\n', '\r'])
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "Das Packerpasswort benötigt 1–1024 UTF-8-Bytes ohne Zeilenumbrüche oder NUL.",
                ));
            }
            let mut input = zeroize::Zeroizing::new(Vec::new());
            input.extend_from_slice(password.as_bytes());
            input.push(b'\n');
            if confirm {
                input.extend_from_slice(password.as_bytes());
                input.push(b'\n');
            }
            Ok(input)
        })
        .transpose()
}

fn wait(tool: &str, child: &mut Child, cancel: &AtomicBool) -> io::Result<()> {
    loop {
        let state = files::check_cancel(cancel).and_then(|()| child.try_wait());
        match state {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => {
                return Err(io::Error::other(format!(
                    "{tool} beendet mit Status {}. Prüfe Archiv, Format und eventuellen Passwortschutz.",
                    status.code().unwrap_or(-1)
                )));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(30)),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn password_prompt_is_framing_and_other_unexpected_lines_are_rejected() {
        let member = "Path = data.txt\nFolder = -\nSize = 3\nEncrypted = +\n\n";
        let listed = super::parse_list(
            format!("\nEnter password (will not be echoed):\n{member}").as_bytes(),
        )
        .unwrap();
        assert_eq!(listed.len(), 1);
        assert!(listed[0].encrypted);
        assert!(super::parse_list(format!("unexpected output\n{member}").as_bytes()).is_err());
    }
    #[test]
    #[ignore = "requires 7-Zip 26.05+ via ARCO_7ZIP_PATH"]
    fn independent_nameless_bzip2_and_xz_streams_list_and_extract_with_verified_sizes() {
        let fixture = tempfile::tempdir().unwrap();
        let input = fixture.path().join("input");
        fs::create_dir(&input).unwrap();
        let name = "Grüße.txt";
        let cancel = AtomicBool::new(false);
        let archiver = ExternalArchiver::discover(&cancel).unwrap();
        for (format, suffix) in [("bzip2", "bz2"), ("xz", "xz")] {
            for (label, body) in [
                ("text", b"independent stream payload".as_slice()),
                ("empty", b"".as_slice()),
            ] {
                fs::write(input.join(name), body).unwrap();
                let archive = fixture.path().join(format!("{label}-{name}.{suffix}"));
                let mut command = archiver.command();
                command
                    .current_dir(&input)
                    .args(["a", &format!("-t{format}"), "-y"])
                    .arg(&archive)
                    .arg(name);
                capture(command, 64 * 1024, &cancel).unwrap();
                let entries = archiver.list(&archive, &cancel).unwrap();
                assert_eq!(entries.len(), 1);
                assert_eq!(entries[0].size, body.len() as u64);
                assert!(entries[0].raw_path.is_empty());
                let target = fixture.path().join(format!("{format}-{label}"));
                fs::create_dir(&target).unwrap();
                assert_eq!(archiver.extract(&archive, &target, &cancel).unwrap(), 1);
                assert_eq!(
                    fs::read(target.join(format!("{label}-{name}"))).unwrap(),
                    body
                );
            }
        }
    }

    #[test]
    fn unnamed_streams_require_a_safe_name_and_verified_size_and_never_become_archive_members() {
        use super::*;
        let listing = b"Size = \nPacked Size = \n\n";
        assert!(parse_list(listing).is_err());
        let entries = parse_list_fallback(listing, Some(("payload.txt", 12))).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, Path::new("payload.txt"));
        assert_eq!(entries[0].size, 12);
        assert!(entries[0].raw_path.is_empty());
        assert!(parse_list_fallback(listing, Some(("../escape", 12))).is_err());
        assert!(parse_list_fallback(b"Size = invalid\n\n", Some(("payload.txt", 12))).is_err());
        assert!(parse_list_fallback(listing, Some(("payload.txt", EXTRACT_LIMIT + 1))).is_err());
    }

    use super::*;

    #[test]
    fn rejects_unsafe_or_colliding_listings() {
        for list in [
            "Path = ..\\outside\nSize = 1\n",
            "Path = a\nSize = 0\nSymbolic Link = target\n",
            "Path = a\nSize = 1\n\nPath = A\nSize = 1\n",
            "Path = a\nSize = 1\n\nPath = a/b\nSize = 1\n",
        ] {
            assert!(parse_list(list.as_bytes()).is_err(), "{list}");
        }
    }

    #[test]
    #[cfg(windows)]
    #[ignore = "requires 7-Zip 26.05+ via ARCO_7ZIP_PATH"]
    fn external_archive_round_trip() {
        let cancel = AtomicBool::new(false);
        let engine = ExternalArchiver::discover(&cancel).unwrap();
        let fixture = tempfile::tempdir().unwrap();
        let source = fixture.path().join("source");
        let nested = source.join("Unterordner");
        let destination = fixture.path().join("target");
        fs::create_dir_all(&nested).unwrap();
        let file = nested.join("Probe [ä].txt");
        fs::write(&file, b"archive bytes").unwrap();
        fs::create_dir(&destination).unwrap();
        for (format, name) in [(Format::SevenZip, "test.7z"), (Format::Tar, "test.tar")] {
            let archive = fixture.path().join(name);
            let output = destination.join(name);
            fs::create_dir(&output).unwrap();
            engine
                .pack(
                    std::slice::from_ref(&nested),
                    &source,
                    &archive,
                    format,
                    &cancel,
                )
                .unwrap();
            assert_eq!(engine.test(&archive, &cancel).unwrap(), 2);
            assert_eq!(engine.extract(&archive, &output, &cancel).unwrap(), 1);
            assert_eq!(
                fs::read(output.join("Unterordner").join("Probe [ä].txt")).unwrap(),
                b"archive bytes"
            );
            let folder = Path::new("Unterordner");
            let member = folder.join("Probe [ä].txt");
            let catalog = crate::archive_view::Catalog::read(&archive, &cancel).unwrap();
            assert_eq!(catalog.rows(folder).unwrap()[0].path, member);
            let selection = crate::archive_view::Selection::preview(&member).unwrap();
            let selected = tempfile::tempdir().unwrap();
            assert_eq!(
                engine
                    .extract_selection(&archive, selected.path(), Some(&selection), &cancel)
                    .unwrap(),
                1
            );
            assert_eq!(
                fs::read(selected.path().join("Probe [ä].txt")).unwrap(),
                b"archive bytes"
            );
        }
        let gzip = fixture.path().join("test.gz");
        let native = fixture.path().join("native.7z");
        crate::archive_view::pack_7z(
            std::slice::from_ref(&nested),
            &source,
            &native,
            None,
            &cancel,
        )
        .unwrap();
        assert_eq!(engine.test(&native, &cancel).unwrap(), 2);
        let output = destination.join("gzip");
        fs::create_dir(&output).unwrap();
        engine
            .pack(
                std::slice::from_ref(&file),
                &nested,
                &gzip,
                Format::Gzip,
                &cancel,
            )
            .unwrap();
        assert_eq!(engine.test(&gzip, &cancel).unwrap(), 1);
        assert_eq!(engine.extract(&gzip, &output, &cancel).unwrap(), 1);
        assert_eq!(
            fs::read(output.join("Probe [ä].txt")).unwrap(),
            b"archive bytes"
        );

        for (format, name) in [("-t7z", "multi.7z"), ("-tzip", "multi.zip")] {
            let payload = "volume.txt";
            let bytes = b"split archive fixture\n".repeat(800);
            fs::write(fixture.path().join(payload), &bytes).unwrap();
            let mut command = engine.command();
            command
                .current_dir(fixture.path())
                .args(["a", format, "-v2k", "-mx0", name, payload]);
            capture(command, LIST_LIMIT, &cancel).unwrap();
            let first = fixture.path().join(format!("{name}.001"));
            assert!(crate::archive_view::is_supported(&first));
            assert_eq!(crate::archive_view::test(&first, None, &cancel).unwrap(), 1);
            let catalog = crate::archive_view::Catalog::read(&first, &cancel).unwrap();
            assert!(catalog.unchanged(&first));
            let entries = catalog.rows(Path::new("")).unwrap();
            assert_eq!(entries[0].path, Path::new(payload));
            let output = tempfile::tempdir().unwrap();
            let selection = crate::archive_view::Selection::preview(Path::new(payload)).unwrap();
            assert_eq!(
                crate::archive_view::extract(&first, output.path(), &selection, None, &cancel)
                    .unwrap(),
                1
            );
            assert_eq!(fs::read(output.path().join(payload)).unwrap(), bytes);
            fs::remove_file(fixture.path().join(format!("{name}.002"))).unwrap();
            assert!(!catalog.unchanged(&first));
            assert!(crate::archive_view::test(&first, None, &cancel).is_err());
        }

        let encrypted_payload = "protected.txt";
        let data = b"encrypted split fixture\n".repeat(220);
        fs::write(fixture.path().join(encrypted_payload), &data).unwrap();
        for (format, encryption, name, protected_header) in [
            ("-t7z", "-mhe=on", "hidden.7z", true),
            ("-t7z", "-mhe=off", "visible.7z", false),
            ("-tzip", "-mem=AES256", "external.zip", false),
        ] {
            let mut command = engine.command();
            command.current_dir(fixture.path()).args([
                "a",
                format,
                "-v2k",
                "-mx0",
                "-pfixture",
                encryption,
                name,
                encrypted_payload,
            ]);
            capture(command, LIST_LIMIT, &cancel).unwrap();
            let first = fixture.path().join(format!("{name}.001"));
            let catalog =
                crate::archive_view::Catalog::read_with_password(&first, Some("fixture"), &cancel)
                    .unwrap();
            assert_eq!(catalog.header_encrypted(), protected_header);
            assert!(catalog.contains_encrypted_entries());
            let output = tempfile::tempdir().unwrap();
            assert_eq!(
                crate::archive_view::extract_all(&first, output.path(), Some("fixture"), &cancel)
                    .unwrap(),
                1
            );
            assert_eq!(
                fs::read(output.path().join(encrypted_payload)).unwrap(),
                data
            );
        }

        for name in ["arco.7z", "arco.zip"] {
            let archive = fixture.path().join(name);
            let count = crate::archive_view::pack_split(
                std::slice::from_ref(&nested),
                &source,
                &archive,
                80,
                None,
                &cancel,
            )
            .unwrap();
            assert!(count > 1);
            let first = fixture.path().join(format!("{name}.001"));
            assert_eq!(engine.test(&first, &cancel).unwrap(), 2);
            let output = tempfile::tempdir().unwrap();
            assert_eq!(engine.extract(&first, output.path(), &cancel).unwrap(), 1);
            assert_eq!(
                fs::read(output.path().join("Unterordner/Probe [ä].txt")).unwrap(),
                b"archive bytes"
            );
            assert!(
                crate::archive_view::pack_split(
                    std::slice::from_ref(&nested),
                    &source,
                    &archive,
                    80,
                    None,
                    &cancel,
                )
                .is_err()
            );
        }

        for name in ["native-aes.7z", "native-aes.zip"] {
            let archive = fixture.path().join(name);
            let count = crate::archive_view::pack_split(
                std::slice::from_ref(&nested),
                &source,
                &archive,
                80,
                Some("fixture"),
                &cancel,
            )
            .unwrap();
            assert!(count > 1);
            let first = fixture.path().join(format!("{name}.001"));
            let mut command = engine.command();
            command.args(["t", "-pfixture", "--"]).arg(&first);
            capture(command, LIST_LIMIT, &cancel).unwrap();
        }

        let mut command = engine.command();
        command.current_dir(fixture.path()).args([
            "a",
            "-tzip",
            "-mem=ZipCrypto",
            "-pfixture",
            "-v2k",
            "-mx0",
            "external-legacy.zip",
            encrypted_payload,
        ]);
        capture(command, LIST_LIMIT, &cancel).unwrap();
        let first = fixture.path().join("external-legacy.zip.001");
        crate::archive_view::edit(
            &first,
            archives::ZipEdit::Rename(encrypted_payload.into(), "legacy-renamed.txt".into()),
            Some("fixture"),
            &cancel,
        )
        .unwrap();
        let mut command = engine.command();
        command.args(["t", "-pfixture", "--"]).arg(first);
        capture(command, LIST_LIMIT, &cancel).unwrap();

        if sfx_module().is_ok() {
            let plain_sfx = fixture.path().join("plain.exe");
            engine
                .pack(
                    std::slice::from_ref(&nested),
                    &source,
                    &plain_sfx,
                    Format::SevenZipSfx,
                    &cancel,
                )
                .unwrap();
            assert_eq!(
                crate::archive_view::test(&plain_sfx, None, &cancel).unwrap(),
                2
            );
            let sfx = fixture.path().join("protected.exe");
            crate::archive_view::pack_encrypted_7z_sfx(
                std::slice::from_ref(&nested),
                &source,
                &sfx,
                "fixture",
                &cancel,
            )
            .unwrap();
            assert!(is_sfx(&sfx));
            assert!(crate::archive_view::is_openable(&sfx));
            assert!(!crate::archive_view::is_supported(&sfx));
            let error = crate::archive_view::Catalog::read(&sfx, &cancel)
                .err()
                .unwrap();
            assert!(crate::archive_view::needs_header_password(&error));
            let output = tempfile::tempdir().unwrap();
            assert_eq!(
                crate::archive_view::extract_all(&sfx, output.path(), Some("fixture"), &cancel)
                    .unwrap(),
                1
            );
            assert_eq!(
                fs::read(output.path().join("Unterordner/Probe [ä].txt")).unwrap(),
                b"archive bytes"
            );
            let mut command = engine.command();
            command.args(["t", "-pfixture", "--"]).arg(&sfx);
            capture(command, LIST_LIMIT, &cancel).unwrap();
        }
    }

    #[test]
    #[cfg(windows)]
    #[ignore = "requires 7-Zip 26.05+ via ARCO_7ZIP_PATH"]
    fn zip_decoder_compatibility() {
        use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

        let cancel = AtomicBool::new(false);
        let engine = ExternalArchiver::discover(&cancel).unwrap();
        let fixture = tempfile::tempdir().unwrap();
        let source = "Probe [ä].txt";
        let content = b"Arco archive compatibility test\n".repeat(4096);
        fs::write(fixture.path().join(source), &content).unwrap();
        let methods = [
            ("Deflate64", CompressionMethod::Deflate64),
            ("BZip2", CompressionMethod::Bzip2),
            ("LZMA", CompressionMethod::Lzma),
            ("PPMd", CompressionMethod::Ppmd),
            ("XZ", CompressionMethod::Xz),
            ("Zstd", CompressionMethod::Zstd),
        ];
        for (name, method) in methods {
            let archive = fixture.path().join(format!("{name}.zip"));
            if method == CompressionMethod::Zstd {
                let mut writer = ZipWriter::new(File::create(&archive).unwrap());
                writer
                    .start_file(
                        source,
                        SimpleFileOptions::default().compression_method(method),
                    )
                    .unwrap();
                writer.write_all(&content).unwrap();
                writer.finish().unwrap();
            } else {
                let mut command = engine.command();
                command.current_dir(fixture.path());
                command
                    .args(["a", "-tzip", "-spd", &format!("-mm={name}"), "--"])
                    .arg(&archive)
                    .arg(source);
                capture(command, 64 * 1024, &cancel).unwrap();
            }
            let mut zip = zip::ZipArchive::new(File::open(&archive).unwrap()).unwrap();
            assert_eq!(zip.by_index_raw(0).unwrap().compression(), method, "{name}");
            let output = fixture.path().join(name);
            fs::create_dir(&output).unwrap();
            assert_eq!(archives::test_zip(&archive, None, &cancel).unwrap(), 1);
            assert_eq!(
                archives::extract_zip(&archive, &output, None, &cancel).unwrap(),
                1
            );
            assert_eq!(fs::read(output.join(source)).unwrap(), content, "{name}");
        }
    }
}
