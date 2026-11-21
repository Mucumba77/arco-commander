use super::{Output, OutputState, Receipt, Segment, Source, split_ranges};
use crate::ftp::{DownloadProgress, Session, Url, check_cancel};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::time::{Duration, UNIX_EPOCH};
use tempfile::{NamedTempFile, TempPath};

const CHECKPOINT_INTERVAL: u64 = 4 * 1024 * 1024;
// Allow the longest paths the SFTP wire format accepts, including JSON escaping.
const MANIFEST_LIMIT: u64 = 1024 * 1024;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    version: u8,
    source: Source,
    receipts: Vec<Receipt>,
}

impl Manifest {
    fn load(path: &Path) -> io::Result<Self> {
        let file = checkpoint_file(path, false, false)?;
        let mut data = Vec::new();
        file.take(MANIFEST_LIMIT + 1).read_to_end(&mut data)?;
        if data.len() as u64 > MANIFEST_LIMIT {
            return Err(invalid_checkpoint());
        }
        serde_json::from_slice(&data).map_err(|_| invalid_checkpoint())
    }

    fn save(&self, path: &Path, create: bool) -> io::Result<()> {
        let parent = path.parent().ok_or_else(invalid_checkpoint)?;
        let data = serde_json::to_vec(self).map_err(io::Error::other)?;
        if data.len() as u64 > MANIFEST_LIMIT {
            return Err(invalid_checkpoint());
        }
        let temporary = NamedTempFile::new_in(parent)?;
        temporary.as_file().write_all(&data)?;
        temporary.as_file().sync_all()?;
        if create {
            temporary.persist_noclobber(path)
        } else {
            temporary.persist(path)
        }
        .map_err(|error| error.error)?;
        Ok(())
    }

    fn snapshot(&mut self, output: &Output<'_>, path: &Path) -> io::Result<()> {
        let state = output.lock()?;
        // Publish offsets only after their file data is durable.
        state.file.sync_all()?;
        self.receipts.clone_from(&state.receipts);
        self.save(path, false)
    }

    fn prepare_ranges(&self, mut file: &File, cancel: &AtomicBool) -> io::Result<Vec<Segment>> {
        let length = file.metadata()?.len();
        if length > self.source.size || self.source.size < self.receipts.len() as u64 {
            return Err(invalid_checkpoint());
        }
        let mut segments = Vec::with_capacity(self.receipts.len());
        let mut buffer = [0; 64 * 1024];
        for (range, receipt) in split_ranges(self.source.size, self.receipts.len())
            .into_iter()
            .zip(&self.receipts)
        {
            if receipt.bytes > range.end - range.start
                || receipt.bytes > 0 && range.start + receipt.bytes > length
            {
                return Err(invalid_checkpoint());
            }
            let mut digest = blake3::Hasher::new();
            file.seek(SeekFrom::Start(range.start))?;
            let mut remaining = receipt.bytes;
            while remaining > 0 {
                check_cancel(cancel)?;
                let width = remaining.min(buffer.len() as u64) as usize;
                file.read_exact(&mut buffer[..width])?;
                digest.update(&buffer[..width]);
                remaining -= width as u64;
            }
            if digest.finalize().as_bytes() != &receipt.hash {
                return Err(io::Error::other(
                    "Die gespeicherten SFTP-Teilbereiche wurden lokal geändert.",
                ));
            }
            segments.push(Segment {
                range,
                saved: receipt.bytes,
                digest: Some(digest),
            });
        }
        Ok(segments)
    }
}

impl Session {
    pub(in crate::ftp) fn resume_sftp_segments(
        &self,
        url: &Url,
        destination: &Path,
        legacy: &Path,
        cancel: &AtomicBool,
        progress: &mut dyn FnMut(DownloadProgress),
    ) -> io::Result<bool> {
        let part = legacy.with_extension("segments.part");
        let journal = legacy.with_extension("segments.json");
        let has_part = exists(&part)?;
        let has_journal = exists(&journal)?;
        let settings = self.parallel_downloads.as_ref();
        if !has_journal && !has_part && (settings.is_none() || exists(legacy)?) {
            return Ok(false);
        }
        if has_part && !has_journal {
            return Err(invalid_checkpoint());
        }
        let Some(source) = self.parallel_source(url, cancel)? else {
            return if has_journal {
                Err(invalid_checkpoint())
            } else {
                Ok(false)
            };
        };

        let (file, mut manifest, slots) = if has_journal {
            let existing = has_part
                .then(|| checkpoint_file(&part, true, false))
                .transpose()?;
            let manifest = Manifest::load(&journal)?;
            if manifest.version != 1 || !(2..=8).contains(&manifest.receipts.len()) {
                return Err(invalid_checkpoint());
            }
            if manifest.source != source {
                return Err(io::Error::other(
                    "Die Serverdatei oder SFTP-Serveridentität stimmt nicht mehr mit den gespeicherten Teilbereichen überein.",
                ));
            }
            let file = match existing {
                Some(file) => file,
                None if manifest.receipts.iter().all(|receipt| receipt.bytes == 0) => {
                    checkpoint_file(&part, true, true)?
                }
                None => return Err(invalid_checkpoint()),
            };
            let slots = settings
                .filter(|settings| source.size >= settings.minimum)
                .and_then(|settings| {
                    settings
                        .connections
                        .try_claim_up_to(manifest.receipts.len() - 1)
                });
            (file, manifest, slots)
        } else {
            let Some(settings) = settings.filter(|settings| source.size >= settings.minimum) else {
                return Ok(false);
            };
            let Some(slots) = settings.connections.try_claim_up_to(7) else {
                return Ok(false);
            };
            let manifest = Manifest {
                version: 1,
                source: source.clone(),
                receipts: vec![
                    Receipt {
                        bytes: 0,
                        hash: *blake3::hash(b"").as_bytes()
                    };
                    slots.count() + 1
                ],
            };
            manifest.save(&journal, true)?;
            (checkpoint_file(&part, true, true)?, manifest, Some(slots))
        };

        let segments = manifest.prepare_ranges(&file, cancel)?;
        let mut committed = manifest
            .receipts
            .iter()
            .map(|receipt| receipt.bytes)
            .sum::<u64>();
        let output = Output {
            state: Mutex::new(OutputState {
                file: &file,
                receipts: manifest.receipts.clone(),
            }),
            received: AtomicU64::new(committed),
            stop: AtomicBool::new(false),
        };
        let result = self.receive_segments(
            &source,
            &output,
            segments,
            1 + slots.as_ref().map_or(0, |slots| slots.count()),
            cancel,
            |update| {
                progress(update);
                Ok(())
            },
            |output| {
                let received = output.received.load(Relaxed);
                if received.saturating_sub(committed) >= CHECKPOINT_INTERVAL {
                    manifest.snapshot(output, &journal)?;
                    committed = received;
                }
                Ok(())
            },
        );
        drop(slots);
        if let Err(error) = result {
            if error.kind() == io::ErrorKind::Interrupted
                || crate::ftp::resume::is_retryable_transport(&error)
            {
                manifest.snapshot(&output, &journal)?;
            }
            return Err(error);
        }
        if self.preserve_transfer_time(false) {
            file.set_modified(UNIX_EPOCH + Duration::from_secs(u64::from(source.modified)))?;
        }
        manifest.snapshot(&output, &journal)?;
        drop(output);
        drop(file);
        check_cancel(cancel)?;
        if let Err(error) = TempPath::try_from_path(&part)?.persist_noclobber(destination) {
            let _ = error.path.keep();
            return Err(error.error);
        }
        let _ = fs::remove_file(journal);
        Ok(true)
    }
}

fn exists(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn checkpoint_file(path: &Path, writable: bool, create: bool) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(writable).create_new(create);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ,
        };
        options
            .share_mode(FILE_SHARE_READ)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    #[cfg(not(windows))]
    if !create && !crate::ftp::resume::is_regular(&fs::symlink_metadata(path)?) {
        return Err(invalid_checkpoint());
    }
    let file = options.open(path)?;
    if !crate::ftp::resume::is_regular(&file.metadata()?) {
        return Err(invalid_checkpoint());
    }
    #[cfg(not(windows))]
    if writable {
        file.try_lock().map_err(io::Error::from)?;
    }
    Ok(file)
}

fn invalid_checkpoint() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "Ungültiger SFTP-Fortsetzungsstand für Teilbereiche.",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ftp::{Connection, DownloadPhase, Security, TransferConnections};
    use std::path::PathBuf;

    fn reference_session() -> Session {
        Connection::new(
            &std::env::var("ARCO_SFTP_TEST_URL").unwrap(),
            "arco",
            Security::Sftp,
        )
        .unwrap()
        .with_ssh(&std::env::var("ARCO_SFTP_TEST_FINGERPRINT").unwrap(), None)
        .unwrap()
        .authenticate("fixture-only")
        .unwrap()
    }

    fn flip_byte(path: &Path, offset: u64) {
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .unwrap();
        let modified = file.metadata().unwrap().modified().unwrap();
        let mut value = [0];
        file.seek(SeekFrom::Start(offset)).unwrap();
        file.read_exact(&mut value).unwrap();
        file.seek(SeekFrom::Start(offset)).unwrap();
        file.write_all(&[value[0] ^ 0xff]).unwrap();
        file.set_modified(modified).unwrap();
        file.sync_all().unwrap();
    }

    #[test]
    #[ignore = "requires scripts/test-sftp-reference.py"]
    fn reference_sftp_resumes_saved_ranges_after_restart_and_reconnect() {
        let pool = TransferConnections::default();
        pool.set_limit(2);
        let _job = pool.try_claim().unwrap();
        let session = reference_session().with_parallel_sftp_downloads(1, pool.clone());
        let directory = session.initial_directory();
        let root = PathBuf::from(std::env::var_os("ARCO_SFTP_TEST_ROOT").unwrap());
        let trace = PathBuf::from(std::env::var_os("ARCO_SFTP_RANGE_TRACE").unwrap());
        let work = tempfile::tempdir().unwrap();
        let cancel = AtomicBool::new(false);
        let data: Vec<_> = (0..3 * 1024 * 1024)
            .map(|index| (index % 251) as u8)
            .collect();
        let source = work.path().join("source");
        fs::write(&source, &data).unwrap();
        let name = "sftp-ranges-restart.bin";
        session
            .upload(&directory, &source, name, false, &cancel)
            .unwrap();
        let destination = work.path().join("destination");
        let error = session
            .download_resuming_with_progress(&directory, name, &destination, &cancel, |progress| {
                if progress.phase == DownloadPhase::Receiving && progress.received >= 256 * 1024 {
                    cancel.store(true, Relaxed);
                }
            })
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        assert!(!destination.exists());
        cancel.store(false, Relaxed);
        assert!(pool.try_claim().is_some());

        let get_checkpoint = |suffix: &str| {
            fs::read_dir(work.path())
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .find(|path| {
                    path.file_name()
                        .unwrap()
                        .to_string_lossy()
                        .ends_with(suffix)
                })
                .unwrap()
        };
        let part = get_checkpoint(".segments.part");
        let journal = get_checkpoint(".segments.json");
        let manifest = Manifest::load(&journal).unwrap();
        let ranges = split_ranges(data.len() as u64, manifest.receipts.len());
        let saved: u64 = manifest.receipts.iter().map(|receipt| receipt.bytes).sum();
        assert!((256 * 1024..data.len() as u64).contains(&saved));
        assert!(manifest.receipts.iter().all(|receipt| receipt.bytes > 0));
        let (index, receipt) = manifest
            .receipts
            .iter()
            .enumerate()
            .max_by_key(|(_, receipt)| receipt.bytes)
            .unwrap();
        assert!(receipt.bytes > 3 * super::super::OVERLAP as u64);
        let saved_byte = ranges[index].start + receipt.bytes - 1;

        let restarted = reference_session();
        flip_byte(&part, saved_byte);
        let error = restarted
            .download_resuming(&directory, name, &destination, &cancel)
            .unwrap_err();
        assert!(error.to_string().contains("lokal geändert"), "{error}");
        assert!(!destination.exists());
        flip_byte(&part, saved_byte);

        let remote = root.join(name);
        flip_byte(&remote, saved_byte);
        let error = restarted
            .download_resuming(&directory, name, &destination, &cancel)
            .unwrap_err();
        assert!(
            error.to_string().contains("stimmt nicht mit dem Server"),
            "{error}"
        );
        assert!(!destination.exists());
        flip_byte(&remote, saved_byte);

        let trace_start = fs::read_to_string(&trace).unwrap().len();
        let mut first = None;
        restarted
            .download_resuming_with_progress(&directory, name, &destination, &cancel, |progress| {
                if progress.phase == DownloadPhase::Receiving {
                    first.get_or_insert(progress.received);
                }
            })
            .unwrap();
        assert_eq!(first, Some(saved));
        assert_eq!(fs::read(&destination).unwrap(), data);
        assert!(!part.exists());
        assert!(!journal.exists());
        let overlap = super::super::OVERLAP as u64;
        for line in fs::read_to_string(&trace).unwrap()[trace_start..].lines() {
            let mut columns = line.split_ascii_whitespace();
            assert_eq!(columns.next(), Some(name));
            let start: u64 = columns.next().unwrap().parse().unwrap();
            let length: u64 = columns.next().unwrap().parse().unwrap();
            for (range, receipt) in ranges.iter().zip(&manifest.receipts) {
                let early = range.start + overlap;
                let late = range.start + receipt.bytes.saturating_sub(overlap);
                if early < late {
                    assert!(
                        start + length <= early || start >= late,
                        "already saved payload requested again: {line}"
                    );
                }
            }
        }

        let retry = "sftp-ranges-retry.bin";
        session
            .upload(&directory, &source, retry, false, &cancel)
            .unwrap();
        let after_loss = work.path().join("after-loss");
        let mut reconnects = 0;
        let mut reused = None;
        session
            .download_resuming_with_progress(&directory, retry, &after_loss, &cancel, |progress| {
                if progress.phase == DownloadPhase::Reconnecting {
                    reconnects += 1;
                } else if reconnects > 0 && progress.phase == DownloadPhase::Receiving {
                    reused.get_or_insert(progress.received);
                }
            })
            .unwrap();
        assert_eq!(reconnects, 1);
        assert!((96 * 1024..data.len() as u64).contains(&reused.unwrap()));
        assert_eq!(fs::read(after_loss).unwrap(), data);
        let listing = session.list_sync(&directory, &cancel).unwrap();
        for name in [name, retry] {
            session
                .delete_sync_entry(&directory, listing.regular_file(name).unwrap(), &cancel)
                .unwrap();
        }
    }
}
