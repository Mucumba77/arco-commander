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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Format {
    SevenZip,
    Tar,
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
    path.extension()
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
            )
        })
}

impl ExternalArchiver {
    pub fn discover(cancel: &AtomicBool) -> io::Result<Self> {
        let executable = std::env::var_os("ARCO_7ZIP_PATH")
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
        let mut command = self.command();
        command
            .args(["l", "-slt", "-ba", "-sccUTF-8", "--"])
            .arg(archive);
        parse_list(&capture(command, LIST_LIMIT, cancel)?)
    }

    pub fn test(&self, archive: &Path, cancel: &AtomicBool) -> io::Result<usize> {
        let entries = self.list(archive, cancel)?;
        let mut command = self.command();
        command.args(["t", "-bso0", "--"]).arg(archive);
        capture(command, 64 * 1024, cancel)?;
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
        let entries = self.list(archive, cancel)?;
        let entries = crate::archive_view::project(
            entries,
            selection,
            |entry| (&entry.path, entry.size, entry.directory),
            cancel,
        )?;
        if entries.iter().any(|(entry, _)| entry.encrypted) {
            return Err(io::Error::other(
                "Passwortgeschützte externe Archive werden derzeit nicht entpackt.",
            ));
        }
        let base = crate::archive_view::check_destination(
            destination,
            &entries,
            |entry| entry.directory,
            cancel,
        )?;
        let mut completed = 0;
        for (entry, relative) in entries {
            files::check_cancel(cancel)?;
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
            command
                .args(["x", "-so", "-spd", "--"])
                .arg(archive)
                .arg(&entry.raw_path);
            stream_to_file(command, output.reopen()?, entry.size, cancel)?;
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
                files::regular_metadata(item?.path())?;
            }
            items.push(relative.to_string());
        }
        let working = tempfile::tempdir_in(&parent)?;
        let list = working.path().join("source-list.txt");
        fs::write(&list, items.join("\n"))?;
        let (format_flag, staging_name) = match format {
            Format::SevenZip => ("-t7z", "archive-stage.7z"),
            Format::Tar => ("-ttar", "archive-stage.tar"),
            Format::Gzip => ("-tgzip", "archive-stage.gz"),
        };
        let stage = working.path().join(staging_name);
        let mut list_argument = OsString::from("@");
        list_argument.push(&list);
        let mut command = self.command();
        command
            .current_dir(base)
            .args(["a", format_flag, "-spd", "-scsUTF-8", "-bso0"])
            .arg(&stage)
            .arg(list_argument);
        capture(command, 64 * 1024, cancel)?;
        self.test(&stage, cancel)?;
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

fn parse_list(bytes: &[u8]) -> io::Result<Vec<Entry>> {
    let text = std::str::from_utf8(bytes).map_err(io::Error::other)?;
    let mut entries = Vec::new();
    let mut fields = BTreeMap::new();
    let mut names = BTreeMap::new();
    let mut total = 0u64;
    for line in text.lines().chain(Some("")) {
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
        let name = fields
            .get("Path")
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
            .and_then(|value| value.parse::<u64>().ok())
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
            raw_path: (*name).to_string(),
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

fn capture(mut command: Command, limit: u64, cancel: &AtomicBool) -> io::Result<Vec<u8>> {
    files::check_cancel(cancel)?;
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let stdout = child.stdout.take().expect("stdout piped");
    let reader = std::thread::spawn(move || {
        let mut data = Vec::new();
        stdout.take(limit + 1).read_to_end(&mut data)?;
        if data.len() as u64 > limit {
            return Err(io::Error::other("7-Zip output exceeds the listing limit"));
        }
        Ok(data)
    });
    let status = wait(&mut child, cancel);
    let bytes = reader
        .join()
        .map_err(|_| io::Error::other("archive reader failed"))?;
    status?;
    bytes
}

fn stream_to_file(
    mut command: Command,
    mut output: File,
    expected: u64,
    cancel: &AtomicBool,
) -> io::Result<()> {
    files::check_cancel(cancel)?;
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let stdout = child.stdout.take().expect("stdout piped");
    let reader = std::thread::spawn(move || {
        let length = io::copy(&mut stdout.take(expected + 1), &mut output)?;
        output.flush()?;
        Ok::<_, io::Error>(length)
    });
    let status = wait(&mut child, cancel);
    let length = reader
        .join()
        .map_err(|_| io::Error::other("archive stream failed"))?;
    status?;
    if length? != expected {
        return Err(io::Error::other("archive entry size mismatch"));
    }
    Ok(())
}

fn wait(child: &mut Child, cancel: &AtomicBool) -> io::Result<()> {
    loop {
        let state = files::check_cancel(cancel).and_then(|()| child.try_wait());
        match state {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => {
                return Err(io::Error::other(format!(
                    "7-Zip beendet mit Status {}. Prüfe Archiv, Format und eventuellen Passwortschutz.",
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
