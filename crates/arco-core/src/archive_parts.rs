use crate::{archive_view, files, tools};
use std::{
    fs::{self, File, Metadata},
    io::{self, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};
use tempfile::{Builder, TempPath};

pub(crate) fn kind(path: &Path) -> Option<&std::ffi::OsStr> {
    let extension = Path::new(path.file_stem()?).extension()?;
    (path.extension()? == "001"
        && (extension.eq_ignore_ascii_case("zip") || extension.eq_ignore_ascii_case("7z")))
    .then_some(extension)
}

pub(crate) fn part_path(first: &Path, index: usize) -> PathBuf {
    let mut name = first.file_stem().unwrap_or_default().to_os_string();
    name.push(format!(".{index:03}"));
    first.with_file_name(name)
}

pub(crate) fn following(first: &Path, cancel: &AtomicBool) -> io::Result<Vec<(PathBuf, Metadata)>> {
    let mut parts = Vec::new();
    for index in 2..=10_000 {
        files::check_cancel(cancel)?;
        let path = part_path(first, index);
        match files::regular_metadata(&path) {
            Ok(metadata) if metadata.is_file() && metadata.len() != 0 => {
                parts.push((path, metadata));
            }
            Ok(_) => {
                return Err(io::Error::other(
                    "Ein Archivsegment ist leer oder keine Datei.",
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(parts),
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::other("Das Archiv enthält zu viele Segmente."))
}

pub(crate) fn rewrite(
    first: &Path,
    password: Option<&str>,
    sources: &[PathBuf],
    cancel: &AtomicBool,
    change: impl FnOnce(&Path) -> io::Result<usize>,
) -> io::Result<usize> {
    files::regular_metadata(first)?;
    let first = fs::canonicalize(first)?;
    let parent = first
        .parent()
        .ok_or_else(|| io::Error::other("Zielordner fehlt."))?;
    let mut originals = vec![(first.clone(), files::regular_metadata(&first)?)];
    originals.extend(following(&first, cancel)?);
    let part_size = originals[0].1.len();
    if part_size == 0
        || originals
            .iter()
            .any(|(_, meta)| meta.permissions().readonly())
    {
        return Err(io::Error::other("Der Archivsatz muss beschreibbar sein."));
    }
    for source in sources {
        let source = fs::canonicalize(source)?;
        if originals.iter().any(|(part, _)| part.starts_with(&source)) {
            return Err(io::Error::other(
                "Ein Mehrteiler kann seine eigenen Teile nicht enthalten.",
            ));
        }
    }
    check_siblings(&first, originals.len(), cancel)?;
    let staging = tempfile::tempdir_in(parent)?;
    let joined = staging
        .path()
        .join(first.file_stem().expect("multipart name"));
    let mut output = File::create(&joined)?;
    let size = tools::copy_cancelled(Input::open(&first, cancel)?, &mut output, cancel)?;
    if originals
        .iter()
        .try_fold(0u64, |total, (_, meta)| total.checked_add(meta.len()))
        != Some(size)
    {
        return Err(io::Error::other(
            "Die Größe des Archivsatzes hat sich geändert.",
        ));
    }
    drop(output);
    archive_view::test(&joined, password, cancel)?;
    let changed = change(&joined)?;
    let parts = stage_parts(&joined, &first, &originals, cancel)?;
    archive_view::test(&parts[0], password, cancel)?;
    let zone = crate::archive_zone::ArchiveZone::read(&first, cancel)?;
    for part in &parts {
        files::check_cancel(cancel)?;
        zone.apply(part)?;
    }
    check_siblings(&first, originals.len(), cancel)?;
    for (part, metadata) in &originals {
        files::ensure_unchanged(part, metadata)?;
    }
    let replacements = parts
        .into_iter()
        .enumerate()
        .map(|(index, part)| (part, part_path(&first, index + 1)))
        .collect();
    replace_parts(parent, &originals, replacements, cancel)?;
    Ok(changed)
}

fn stage_parts(
    joined: &Path,
    first: &Path,
    originals: &[(PathBuf, Metadata)],
    cancel: &AtomicBool,
) -> io::Result<Vec<PathBuf>> {
    let mut input = File::open(joined)?;
    let mut remaining = input.metadata()?.len();
    let fixed = originals.len() - 1;
    // The last segment may have been short. Reuse the final full segment's
    // capacity when the new archive reaches the end of the original sequence.
    let mut capacities = originals.iter().map(|(_, meta)| meta.len()).rev();
    let final_size = capacities.next().unwrap_or(1);
    let tail_capacity = capacities.next().unwrap_or(final_size).max(final_size);
    let mut paths = Vec::new();
    let staged_first = joined.with_file_name(first.file_name().expect("archive part name"));
    for index in 0..9_999 {
        files::check_cancel(cancel)?;
        let capacity = if index < fixed {
            originals[index].1.len()
        } else {
            tail_capacity
        };
        let path = part_path(&staged_first, index + 1);
        let length = remaining.min(capacity);
        let mut output = File::options().write(true).create_new(true).open(&path)?;
        if tools::copy_cancelled((&mut input).take(length), &mut output, cancel)? != length {
            return Err(io::Error::other("Die Archivdatei wurde abgeschnitten."));
        }
        output.sync_all()?;
        paths.push(path);
        remaining -= length;
        if remaining == 0 {
            return Ok(paths);
        }
    }
    Err(io::Error::other(
        "Das Archiv würde mehr als 9999 Teile benötigen.",
    ))
}

fn check_siblings(first: &Path, count: usize, cancel: &AtomicBool) -> io::Result<()> {
    let stem = first.file_stem().expect("multipart name").to_string_lossy();
    for entry in fs::read_dir(first.parent().expect("multipart directory"))? {
        files::check_cancel(cancel)?;
        let entry = entry?;
        let path = entry.path();
        if path
            .file_stem()
            .is_some_and(|name| name.eq_ignore_ascii_case(stem.as_ref()))
            && path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| {
                    ext.len() >= 3
                        && ext.bytes().all(|byte| byte.is_ascii_digit())
                        && ext.parse::<usize>().ok().is_none_or(|index| {
                            !(1..=count).contains(&index) || ext != format!("{index:03}")
                        })
                })
        {
            return Err(io::Error::other(format!(
                "Unerwarteter Teil im Archivsatz: {}",
                path.display()
            )));
        }
    }
    Ok(())
}

fn replace_parts(
    parent: &Path,
    originals: &[(PathBuf, Metadata)],
    replacements: Vec<(PathBuf, PathBuf)>,
    cancel: &AtomicBool,
) -> io::Result<()> {
    let mut backup = Builder::new()
        .prefix(".arco-archiv-sicherung-")
        .tempdir_in(parent)?;
    backup.disable_cleanup(true);
    let mut moved = Vec::new();
    let replace = (|| -> io::Result<()> {
        // Remove the entry point first. Publishing and rollback restore it last.
        for (original, metadata) in originals {
            files::check_cancel(cancel)?;
            files::ensure_unchanged(original, metadata)?;
            let copy = backup
                .path()
                .join(original.file_name().expect("archive part name"));
            fs::rename(original, &copy)?;
            moved.push((copy, original));
        }
        tools::publish_parts(replacements, cancel)?;
        Ok(())
    })();
    if let Err(error) = replace {
        for (copy, original) in moved.into_iter().rev() {
            let restore = TempPath::try_from_path(copy).and_then(|mut copy| {
                copy.disable_cleanup(true);
                copy.persist_noclobber(original)
                    .map_err(|error| error.error)
            });
            if let Err(restore) = restore {
                return Err(io::Error::other(format!(
                    "{error}. Sicherung konnte nicht vollständig zurückgespielt werden ({restore}): {}",
                    backup.path().display()
                )));
            }
        }
        backup.disable_cleanup(false);
        return Err(error);
    }
    backup.disable_cleanup(false);
    Ok(())
}

pub(crate) enum Input<'a> {
    Single(File),
    Segmented(SegmentReader<'a>),
}

impl<'a> Input<'a> {
    pub(crate) fn open(path: &Path, cancel: &'a AtomicBool) -> io::Result<Self> {
        if kind(path).is_some() {
            SegmentReader::open(path, 0, following(path, cancel)?, cancel).map(Self::Segmented)
        } else {
            File::open(path).map(Self::Single)
        }
    }

    pub(crate) fn embedded(path: &Path, offset: u64, cancel: &'a AtomicBool) -> io::Result<Self> {
        SegmentReader::open(path, offset, Vec::new(), cancel).map(Self::Segmented)
    }
}

impl Read for Input<'_> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Single(file) => file.read(output),
            Self::Segmented(reader) => reader.read(output),
        }
    }
}

impl Seek for Input<'_> {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        match self {
            Self::Single(file) => file.seek(from),
            Self::Segmented(reader) => reader.seek(from),
        }
    }
}

struct Part {
    path: PathBuf,
    metadata: Metadata,
    end: u64,
}

struct OpenPart {
    index: usize,
    position: u64,
    file: File,
}

pub(crate) struct SegmentReader<'a> {
    parts: Vec<Part>,
    position: u64,
    length: u64,
    active: OpenPart,
    cancel: &'a AtomicBool,
}

impl<'a> SegmentReader<'a> {
    fn open(
        first: &Path,
        offset: u64,
        following: Vec<(PathBuf, Metadata)>,
        cancel: &'a AtomicBool,
    ) -> io::Result<Self> {
        let metadata = files::regular_metadata(first)?;
        if !metadata.is_file() || metadata.len() <= offset {
            return Err(io::Error::other(
                "Das erste Archivsegment ist leer oder keine Datei.",
            ));
        }
        let mut length = 0u64;
        let parts = std::iter::once((first.to_owned(), metadata))
            .chain(following)
            .enumerate()
            .map(|(index, (path, metadata))| {
                let offset = if index == 0 { offset } else { 0 };
                length = length.checked_add(metadata.len() - offset).ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "Das Archiv ist zu groß.")
                })?;
                Ok(Part {
                    path,
                    metadata,
                    end: length,
                })
            })
            .collect::<io::Result<Vec<_>>>()?;
        let active = OpenPart {
            index: 0,
            position: 0,
            file: open_part(&parts[0])?,
        };
        Ok(Self {
            parts,
            position: 0,
            length,
            active,
            cancel,
        })
    }
}

fn open_part(part: &Part) -> io::Result<File> {
    let mut options = File::options();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(1); // FILE_SHARE_READ
    }
    files::ensure_unchanged(&part.path, &part.metadata)?;
    let file = options.open(&part.path)?;
    files::ensure_unchanged(&part.path, &part.metadata)?;
    Ok(file)
}

impl Read for SegmentReader<'_> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if self.cancel.load(Ordering::Relaxed) {
            // read_exact and copy retry Interrupted.
            return Err(io::Error::other("Archivauftrag abgebrochen."));
        }
        if output.is_empty() || self.position >= self.length {
            return Ok(0);
        }
        let index = self.parts.partition_point(|part| part.end <= self.position);
        let part = &self.parts[index];
        let local = part.metadata.len() - (part.end - self.position);
        if self.active.index != index {
            self.active = OpenPart {
                index,
                position: 0,
                file: open_part(part)?,
            };
        }
        if self.active.position != local {
            self.active.file.seek(SeekFrom::Start(local))?;
        }
        let limit = output
            .len()
            .min((part.end - self.position).try_into().unwrap_or(usize::MAX));
        let count = self.active.file.read(&mut output[..limit])?;
        if count == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "Ein Archivsegment wurde abgeschnitten.",
            ));
        }
        self.active.position = local + count as u64;
        self.position += count as u64;
        Ok(count)
    }
}

impl Seek for SegmentReader<'_> {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        files::check_cancel(self.cancel)?;
        let position = match from {
            SeekFrom::Start(position) => Some(position),
            SeekFrom::Current(offset) => self.position.checked_add_signed(offset),
            SeekFrom::End(offset) => self.length.checked_add_signed(offset),
        };
        self.position = position.ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "Ungültige Position im Archiv.")
        })?;
        Ok(self.position)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_publication_restores_all_original_parts() {
        let fixture = tempfile::tempdir().unwrap();
        let directory = fixture.path();
        let first = directory.join("archive.zip.001");
        let second = part_path(&first, 2);
        let collision = part_path(&first, 3);
        fs::write(&first, b"first").unwrap();
        fs::write(&second, b"second").unwrap();
        fs::write(&collision, b"other file").unwrap();
        let original =
            [&first, &second].map(|path| (path.clone(), files::regular_metadata(path).unwrap()));
        let staged = directory.join("new");
        fs::write(&staged, b"staged data").unwrap();
        assert!(
            replace_parts(
                directory,
                &original,
                vec![(staged, collision.clone())],
                &AtomicBool::new(false)
            )
            .is_err()
        );
        assert_eq!(fs::read(&first).unwrap(), b"first");
        assert_eq!(fs::read(&second).unwrap(), b"second");
        assert_eq!(fs::read(&collision).unwrap(), b"other file");
        assert_eq!(fs::read_dir(directory).unwrap().count(), 4);
    }
}
