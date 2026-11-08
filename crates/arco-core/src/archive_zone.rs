use std::{io, path::Path, sync::atomic::AtomicBool};

#[cfg(windows)]
use std::{
    fs::File,
    io::{Read, Write},
    path::PathBuf,
};

#[derive(Default)]
pub(crate) struct ArchiveZone {
    #[cfg(windows)]
    contents: Vec<u8>,
}

impl ArchiveZone {
    #[cfg(windows)]
    pub(crate) fn read(source: &Path, cancel: &AtomicBool) -> io::Result<Self> {
        use crate::{archive_parts, files, tools, uc2_volumes};
        let mut zone = Self::default();
        let mut rank = None;
        let parts = if archive_parts::kind(source).is_some() {
            archive_parts::following(source, cancel)?
        } else {
            uc2_volumes::snapshot(source)?
        };
        for path in std::iter::once(source).chain(
            parts
                .iter()
                .map(|(path, _)| path.as_path())
                .filter(|path| *path != source),
        ) {
            files::check_cancel(cancel)?;
            let stream = match File::open(stream_path(path)) {
                Ok(file) => file,
                Err(error)
                    if error.kind() == io::ErrorKind::NotFound || streams_unsupported(&error) =>
                {
                    continue;
                }
                Err(error) => return Err(error),
            };
            let mut contents = Vec::new();
            stream.take(65_537).read_to_end(&mut contents)?;
            if contents.len() > 65_536 {
                return Err(io::Error::other("Zone.Identifier überschreitet 64 KiB."));
            }
            if contents.is_empty() {
                continue;
            }
            let severity = tools::decode_text(&contents)
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once('=')?;
                    if name.trim().eq_ignore_ascii_case("ZoneId") {
                        value.trim().parse::<u8>().ok().filter(|&value| value <= 4)
                    } else {
                        None
                    }
                })
                .unwrap_or_default();
            if rank.is_none_or(|old| severity > old) {
                rank = Some(severity);
                zone.contents = contents;
            }
        }
        Ok(zone)
    }

    #[cfg(not(windows))]
    pub(crate) fn read(_: &Path, _: &AtomicBool) -> io::Result<Self> {
        Ok(Self::default())
    }

    #[cfg(windows)]
    pub(crate) fn apply(&self, target: &Path) -> io::Result<()> {
        if self.contents.is_empty() {
            return Ok(());
        }
        let mut stream = match File::create(stream_path(target)) {
            Ok(file) => file,
            Err(error) if streams_unsupported(&error) => return Ok(()),
            Err(error) => return Err(error),
        };
        stream.write_all(&self.contents)?;
        stream.sync_all()
    }

    #[cfg(not(windows))]
    pub(crate) fn apply(&self, _: &Path) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(windows)]
fn stream_path(path: &Path) -> PathBuf {
    let mut stream = path.as_os_str().to_os_string();
    stream.push(":Zone.Identifier:$DATA");
    PathBuf::from(stream)
}

#[cfg(windows)]
fn streams_unsupported(error: &io::Error) -> bool {
    // ERROR_INVALID_FUNCTION, ERROR_NOT_SUPPORTED, ERROR_INVALID_NAME.
    matches!(error.raw_os_error(), Some(1 | 50 | 123))
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use crate::archive_view;
    use std::fs;

    #[test]
    fn extraction_keeps_zone_from_archive_and_continuation() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path();
        let source = root.join("probe.txt");
        fs::write(&source, b"fixture payload").unwrap();
        let paths = std::slice::from_ref(&source);
        let cancel = AtomicBool::new(false);
        let zip = root.join("marked.zip");
        crate::archives::create_zip(paths, root, &zip, None, &cancel).unwrap();
        let lower = b"[ZoneTransfer]\r\nZoneId=3\r\n";
        match fs::write(stream_path(&zip), lower) {
            Ok(()) => (),
            Err(error) if streams_unsupported(&error) => return,
            Err(error) => panic!("{error}"),
        }
        let output = tempfile::tempdir().unwrap();
        archive_view::extract_all(&zip, output.path(), None, &cancel).unwrap();
        assert_eq!(
            fs::read(stream_path(&output.path().join("probe.txt"))).unwrap(),
            lower
        );
        let seven = root.join("marked.7z");
        assert!(archive_view::pack_split(paths, root, &seven, 61, None, &cancel).unwrap() > 1);
        let first = root.join("marked.7z.001");
        fs::write(stream_path(&first), lower).unwrap();
        let higher = b"[ZoneTransfer]\r\nZoneId=4\r\nHostUrl=https://example.invalid/archive\r\n";
        fs::write(stream_path(&root.join("marked.7z.002")), higher).unwrap();
        let output = tempfile::tempdir().unwrap();
        archive_view::extract_all(&first, output.path(), None, &cancel).unwrap();
        assert_eq!(
            fs::read(stream_path(&output.path().join("probe.txt"))).unwrap(),
            higher
        );
    }
}
