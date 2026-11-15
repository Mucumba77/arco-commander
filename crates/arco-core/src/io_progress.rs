//! Scoped source-read observations; observers never own or publish output files.
use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Reading,
    Processing,
    Publishing,
}

pub struct Progress<'a> {
    pub source: &'a Path,
    pub completed: u64,
    pub total: Option<u64>,
    pub phase: Phase,
}

pub type Observer<'a> = &'a dyn Fn(Progress<'_>);

pub fn phase(source: &Path, phase: Phase, observer: Observer<'_>) {
    observer(Progress {
        source,
        completed: 0,
        total: None,
        phase,
    });
}

pub(crate) struct ObservedFile<'a> {
    file: File,
    path: PathBuf,
    completed: u64,
    total: u64,
    cancel: &'a AtomicBool,
    observer: Observer<'a>,
}

impl<'a> ObservedFile<'a> {
    pub fn open(path: &Path, cancel: &'a AtomicBool, observer: Observer<'a>) -> io::Result<Self> {
        Self::from_file(File::open(path)?, path, cancel, observer)
    }

    pub fn from_file(
        file: File,
        path: &Path,
        cancel: &'a AtomicBool,
        observer: Observer<'a>,
    ) -> io::Result<Self> {
        crate::files::check_cancel(cancel)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            return Err(io::Error::other("Wähle eine reguläre Datei."));
        }
        let result = Self {
            file,
            path: path.to_path_buf(),
            completed: 0,
            total: metadata.len(),
            cancel,
            observer,
        };
        result.report();
        Ok(result)
    }

    fn report(&self) {
        (self.observer)(Progress {
            source: &self.path,
            completed: self.completed,
            total: Some(self.total),
            phase: Phase::Reading,
        });
    }
}

impl Read for ObservedFile<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        crate::files::check_cancel(self.cancel)?;
        let count = self.file.read(buffer)?;
        if count > 0 {
            self.completed = self.completed.saturating_add(count as u64);
            self.report();
        }
        Ok(count)
    }
}

impl Seek for ObservedFile<'_> {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        crate::files::check_cancel(self.cancel)?;
        self.completed = self.file.seek(position)?;
        self.report();
        Ok(self.completed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{tc_split, tools};
    use std::{cell::Cell, fs, sync::atomic::Ordering};

    #[test]
    fn hashes_report_real_bytes_and_detect_changed_sources() {
        let fixture = tempfile::tempdir().unwrap();
        let source = fixture.path().join("file");
        fs::write(&source, b"abc").unwrap();
        let bytes = Cell::new(0);
        let hash = tools::checksum_file_observed(
            File::open(&source).unwrap(),
            tools::Hash::Sha256,
            &AtomicBool::new(false),
            &|p| bytes.set(p.completed),
        )
        .unwrap();
        assert_eq!(
            hash,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(bytes.get(), 3);
        let cancel = AtomicBool::new(false);
        let result = tools::checksum_file_observed(
            File::open(&source).unwrap(),
            tools::Hash::Sha256,
            &cancel,
            &|_| cancel.store(true, Ordering::Relaxed),
        );
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Interrupted);
        let changed = Cell::new(false);
        let result = tools::checksum_file_observed(
            File::open(&source).unwrap(),
            tools::Hash::Sha256,
            &AtomicBool::new(false),
            &|p| {
                if p.completed > 0 && !changed.replace(true) {
                    fs::write(&source, b"changed-size").unwrap();
                }
            },
        );
        assert!(result.is_err());
    }

    #[test]
    fn checksum_manifest_reports_mismatches_and_rejects_escaping_paths() {
        let fixture = tempfile::tempdir().unwrap();
        let base = fixture.path().join("checks");
        fs::create_dir(&base).unwrap();
        fs::write(base.join("file.txt"), b"abc").unwrap();
        fs::write(fixture.path().join("outside"), b"abc").unwrap();
        let expected = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        let list = base.join("list.sha256");
        fs::write(
            &list,
            format!(
                "{expected} *file.txt\n{} *file.txt\n{expected} *../outside\nbad *file.txt\n",
                "0".repeat(64)
            ),
        )
        .unwrap();
        let bytes = Cell::new(0);
        let report = tools::verify_checksums_observed(
            &list,
            tools::Hash::Sha256,
            &AtomicBool::new(false),
            &|p| bytes.set(p.completed),
        )
        .unwrap();
        assert!(report.contains("OK       file.txt"));
        assert!(report.contains("ABWEICHUNG file.txt"));
        assert!(report.contains("FEHLER   ../outside"));
        assert!(report.contains("Ungültige Zeile"));
        assert_eq!(bytes.get(), 3);
    }

    #[test]
    fn splitting_and_joining_cancel_without_publishing_incomplete_outputs() {
        let fixture = tempfile::tempdir().unwrap();
        let source = fixture.path().join("data.bin");
        fs::write(&source, vec![7; 300_000]).unwrap();
        for tc in [false, true] {
            let target = fixture.path().join(if tc { "tc" } else { "raw" });
            fs::create_dir(&target).unwrap();
            let cancel = AtomicBool::new(false);
            let observe = |p: Progress<'_>| {
                if p.phase == Phase::Publishing {
                    cancel.store(true, Ordering::Relaxed);
                }
            };
            let result = if tc {
                tc_split::split_observed(&source, &target, 100_000, &cancel, &observe)
            } else {
                tools::split_observed(&source, &target, 100_000, &cancel, &observe).map(|p| p.len())
            };
            assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Interrupted);
            assert_eq!(fs::read_dir(&target).unwrap().count(), 0);
        }
        let raw = fixture.path().join("parts");
        fs::create_dir(&raw).unwrap();
        let collision = raw.join("data.bin.002");
        fs::write(&collision, b"existing").unwrap();
        assert!(tools::split(&source, &raw, 100_000, &AtomicBool::new(false)).is_err());
        assert_eq!(fs::read_dir(&raw).unwrap().count(), 1);
        assert_eq!(fs::read(collision).unwrap(), b"existing");
        let joined = fixture.path().join("joined");
        let cancel = AtomicBool::new(false);
        let result = tools::join_observed(&[source], &joined, &cancel, &|p| {
            if p.completed > 0 {
                cancel.store(true, Ordering::Relaxed);
            }
        });
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Interrupted);
        assert!(!joined.exists());
    }
}
