use super::{Attrs, Wire, path};
use crate::ftp::{ChecksumAlgorithm, DownloadPhase, DownloadProgress, Session, Url, check_cancel};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::ops::Range;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::time::Duration;

const OVERLAP: usize = 4096;

mod resume;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct Source {
    path: String,
    peer: String,
    size: u64,
    modified: u32,
}

impl Source {
    fn verify(&self, attrs: Attrs) -> io::Result<()> {
        if attrs.directory() == Some(false)
            && attrs.size == Some(self.size)
            && attrs.modified == Some(self.modified)
        {
            Ok(())
        } else {
            Err(io::Error::other("Die SFTP-Quelldatei hat sich geändert."))
        }
    }

    fn verify_handle(&self, wire: &mut Wire, handle: &[u8]) -> io::Result<()> {
        match wire.fstat(handle) {
            Ok(attrs) => self.verify(attrs),
            Err(error) if error.kind() == io::ErrorKind::Unsupported => Ok(()),
            Err(error) => Err(error),
        }
    }
}

struct Output<'a> {
    state: Mutex<OutputState<'a>>,
    received: AtomicU64,
    stop: AtomicBool,
}

struct OutputState<'a> {
    file: &'a File,
    receipts: Vec<Receipt>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct Receipt {
    bytes: u64,
    hash: [u8; 32],
}

struct Segment {
    range: Range<u64>,
    saved: u64,
    digest: Option<blake3::Hasher>,
}

fn split_ranges(size: u64, count: usize) -> Vec<Range<u64>> {
    let mut offset = 0;
    (0..count)
        .map(|index| {
            let width = size / count as u64 + u64::from((index as u64) < size % count as u64);
            let range = offset..offset + width;
            offset = range.end;
            range
        })
        .collect()
}

impl<'file> Output<'file> {
    fn lock(&self) -> io::Result<std::sync::MutexGuard<'_, OutputState<'file>>> {
        self.state
            .lock()
            .map_err(|_| io::Error::other("SFTP-Zieldatei nicht verfügbar."))
    }

    fn write(
        &self,
        index: usize,
        offset: u64,
        bytes: &[u8],
        receipt: Option<Receipt>,
    ) -> io::Result<()> {
        let mut state = self.lock()?;
        let mut file = state.file;
        file.seek(SeekFrom::Start(offset))?;
        file.write_all(bytes)?;
        if let Some(receipt) = receipt {
            state.receipts[index] = receipt;
        }
        self.received.fetch_add(bytes.len() as u64, Relaxed);
        Ok(())
    }

    fn read(&self, offset: u64, size: usize) -> io::Result<Vec<u8>> {
        let state = self.lock()?;
        let mut file = state.file;
        file.seek(SeekFrom::Start(offset))?;
        let mut bytes = vec![0; size];
        file.read_exact(&mut bytes)?;
        Ok(bytes)
    }
}

struct Boundary {
    start: Vec<u8>,
    next: Vec<u8>,
}

impl Session {
    pub(in crate::ftp) fn receive_parallel_sftp(
        &self,
        url: &Url,
        file: &File,
        limit: Option<u64>,
        cancel: &AtomicBool,
        mut progress: impl FnMut(DownloadProgress) -> io::Result<()>,
    ) -> io::Result<bool> {
        let Some(options) = &self.parallel_downloads else {
            return Ok(false);
        };
        if limit.is_some_and(|size| size < options.minimum) || file.metadata()?.len() != 0 {
            return Ok(false);
        }
        let Some(source) = self.parallel_source(url, cancel)? else {
            return Ok(false);
        };
        let size = source.size;
        if size < options.minimum {
            return Ok(false);
        }
        if limit.is_some_and(|limit| size > limit) {
            return Err(io::Error::other(
                "Die SFTP-Datei überschreitet das Größenlimit.",
            ));
        }
        let Some(slots) = options.connections.try_claim_up_to(7) else {
            return Ok(false);
        };
        let workers = slots.count() + 1;
        let output = Output {
            state: Mutex::new(OutputState {
                file,
                receipts: Vec::new(),
            }),
            received: AtomicU64::new(0),
            stop: AtomicBool::new(false),
        };
        let segments = split_ranges(size, workers)
            .into_iter()
            .map(|range| Segment {
                range,
                saved: 0,
                digest: None,
            })
            .collect();
        self.receive_segments(
            &source,
            &output,
            segments,
            workers,
            cancel,
            &mut progress,
            |_| Ok(()),
        )?;
        Ok(true)
    }

    fn parallel_source(&self, url: &Url, cancel: &AtomicBool) -> io::Result<Option<Source>> {
        self.validate(url)?;
        let remote = path(url)?;
        let attrs = self.sftp_operation(cancel, |wire| wire.lstat(&remote))?;
        let (Some(size), Some(modified), Some(false)) =
            (attrs.size, attrs.modified, attrs.directory())
        else {
            return Ok(None);
        };
        Ok(Some(Source {
            path: remote,
            peer: self.connected_sftp_fingerprint(cancel)?,
            size,
            modified,
        }))
    }

    fn receive_segments(
        &self,
        source: &Source,
        output: &Output<'_>,
        segments: Vec<Segment>,
        workers: usize,
        cancel: &AtomicBool,
        mut progress: impl FnMut(DownloadProgress) -> io::Result<()>,
        mut checkpoint: impl FnMut(&Output<'_>) -> io::Result<()>,
    ) -> io::Result<()> {
        let size = source.size;
        let count = segments.len();
        let pending = Mutex::new(segments.into_iter().enumerate());
        let mut notify = |received, phase| {
            progress(DownloadProgress {
                received,
                total: Some(size),
                phase,
            })
        };
        let mut reported = output.received.load(Relaxed);
        notify(reported, DownloadPhase::Receiving)?;
        let boundaries = std::thread::scope(|scope| {
            let mut handles = Vec::with_capacity(workers);
            for index in 0..workers {
                let session = if index == 0 {
                    self.clone()
                } else {
                    self.with_transfer_connection()
                };
                let pending = &pending;
                handles.push(scope.spawn(move || {
                    let mut completed = Vec::new();
                    while !output.stop.load(Relaxed) {
                        let next = pending
                            .lock()
                            .unwrap_or_else(|error| error.into_inner())
                            .next();
                        let Some((index, segment)) = next else { break };
                        let result =
                            session.receive_segment(source, index, segment, workers, output);
                        let failed = result.is_err();
                        completed.push((index, result));
                        if failed {
                            output.stop.store(true, Relaxed);
                            break;
                        }
                    }
                    completed
                }));
            }
            let mut failure = None;
            while handles.iter().any(|handle| !handle.is_finished()) {
                if let Err(error) = check_cancel(cancel) {
                    output.stop.store(true, Relaxed);
                    failure = Some(error);
                }
                let actual = output.received.load(Relaxed);
                if actual != reported && failure.is_none() {
                    if let Err(error) =
                        checkpoint(output).and_then(|()| notify(actual, DownloadPhase::Receiving))
                    {
                        output.stop.store(true, Relaxed);
                        failure = Some(error);
                    }
                    reported = actual;
                }
                std::thread::sleep(Duration::from_millis(40));
            }
            let mut stopped = None;
            let mut results = Vec::with_capacity(count);
            for handle in handles {
                match handle.join() {
                    Err(_) => {
                        failure.get_or_insert_with(|| {
                            io::Error::other("Ein SFTP-Teiltransfer wurde unerwartet beendet.")
                        });
                    }
                    Ok(completed) => {
                        for (index, result) in completed {
                            match result {
                                Ok(boundary) => results.push((index, boundary)),
                                Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                                    stopped = Some(error)
                                }
                                Err(error) => {
                                    failure.get_or_insert(error);
                                }
                            }
                        }
                    }
                }
            }
            if let Some(error) = failure.or(stopped) {
                return Err(error);
            }
            results.sort_unstable_by_key(|&(index, _)| index);
            Ok(results
                .into_iter()
                .map(|(_, boundary)| boundary)
                .collect::<Vec<_>>())
        })?;
        check_cancel(cancel)?;
        let file = output.lock()?.file;
        if boundaries.len() != count
            || output.received.load(Relaxed) != size
            || file.metadata()?.len() != size
        {
            return Err(io::Error::other(
                "Der parallele SFTP-Download ist unvollständig.",
            ));
        }
        notify(size, DownloadPhase::Verifying)?;
        check_cancel(cancel)?;
        for pair in boundaries.windows(2) {
            if !pair[0].next.starts_with(&pair[1].start) || pair[1].start.is_empty() {
                return Err(io::Error::other(
                    "Die überlappenden SFTP-Teilbereiche stimmen nicht überein.",
                ));
            }
        }
        let digest = self.sftp_operation(cancel, |wire| {
            source.verify(wire.lstat(&source.path)?)?;
            let digest = wire.checksum(&source.path, ChecksumAlgorithm::Sha256)?;
            source.verify(wire.lstat(&source.path)?)?;
            Ok(digest)
        })?;
        if let Some(digest) = digest {
            verify_digest(file, &digest, cancel)?;
        }
        (&*file).seek(SeekFrom::End(0))?;
        Ok(())
    }

    fn receive_segment(
        &self,
        source: &Source,
        index: usize,
        segment: Segment,
        workers: usize,
        output: &Output<'_>,
    ) -> io::Result<Boundary> {
        if self.connected_sftp_fingerprint(&output.stop)? != source.peer {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Die SFTP-Teiltransfers melden unterschiedliche Serveridentitäten.",
            ));
        }
        self.sftp_operation(&output.stop, |wire| {
            source.verify(wire.lstat(&source.path)?)?;
            let handle = wire.open(&source.path, false)?;
            let result = (|| {
                source.verify_handle(wire, &handle)?;
                let Segment { range, saved, mut digest } = segment;
                let end = range.end.saturating_add(OVERLAP as u64).min(source.size);
                let mut offset = range.start + saved;
                let mut next = Vec::new();
                let mut rate = self.transfer_rate().for_connection(workers);
                let mut charged = 0;
                let mut read = |offset: u64, limit: u64| -> io::Result<Vec<u8>> {
                    check_cancel(&output.stop)?;
                    let count = rate.chunk((limit - offset).min(32 * 1024) as usize) as u32;
                    let data = wire.read(&handle, offset, count)?.ok_or_else(|| {
                        io::Error::new(io::ErrorKind::UnexpectedEof, "Ein SFTP-Teilbereich endet zu früh.")
                    })?;
                    charged += data.len() as u64;
                    rate.wait(charged, &output.stop)?;
                    Ok(data)
                };
                let mut start = if saved == 0 {
                    Vec::new()
                } else {
                    let overlap = (saved.min(OVERLAP as u64)) as usize;
                    let begin = offset - overlap as u64;
                    let local = output.read(begin, overlap)?;
                    let mut checked = 0;
                    while checked < overlap {
                        let actual = read(begin + checked as u64, offset)?;
                        if !local[checked..].starts_with(&actual) {
                            return Err(io::Error::other("Der gespeicherte SFTP-Teilbereich stimmt nicht mit dem Server überein."));
                        }
                        checked += actual.len();
                    }
                    output.read(range.start, saved.min(OVERLAP as u64) as usize)?
                };
                while offset < end {
                    let in_range = offset < range.end;
                    let limit = if in_range { range.end } else { end };
                    let data = read(offset, limit)?;
                    if in_range {
                        start.extend_from_slice(&data[..data.len().min(OVERLAP - start.len())]);
                        let receipt = digest.as_mut().map(|digest| {
                            digest.update(&data);
                            Receipt { bytes: offset - range.start + data.len() as u64, hash: *digest.finalize().as_bytes() }
                        });
                        output.write(index, offset, &data, receipt)?;
                    } else {
                        next.extend_from_slice(&data);
                    }
                    offset += data.len() as u64;
                }
                source.verify_handle(wire, &handle)?;
                source.verify(wire.lstat(&source.path)?)?;
                Ok(Boundary { start, next })
            })();
            let closed = wire.close(&handle);
            result.and_then(|boundary| closed.map(|()| boundary))
        })
    }
}

fn verify_digest(mut file: &File, expected: &[u8], cancel: &AtomicBool) -> io::Result<()> {
    file.seek(SeekFrom::Start(0))?;
    let mut digest = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        check_cancel(cancel)?;
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    if &digest.finalize()[..] != expected {
        return Err(io::Error::other(
            "Die SFTP-Prüfsumme der Zieldatei stimmt nicht überein.",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ftp::{Connection, Security, TransferConnections, TransferLimit};
    use std::time::Instant;

    #[test]
    #[ignore = "requires scripts/test-sftp-reference.py"]
    fn reference_sftp_segmented_downloads_publish_only_a_verified_file() {
        let session = Connection::new(
            &std::env::var("ARCO_SFTP_TEST_URL").unwrap(),
            "arco",
            Security::Sftp,
        )
        .unwrap()
        .with_ssh(&std::env::var("ARCO_SFTP_TEST_FINGERPRINT").unwrap(), None)
        .unwrap()
        .authenticate("fixture-only")
        .unwrap();
        let pool = TransferConnections::default();
        pool.set_limit(2);
        let _job = pool.try_claim().unwrap();
        let speed = TransferLimit::default();
        let session = session
            .with_parallel_sftp_downloads(1, pool.clone())
            .with_transfer_limit(speed.clone());
        let directory = session.initial_directory();
        let local = tempfile::tempdir().unwrap();
        let cancel = AtomicBool::new(false);
        for (name, size, expected_error) in [
            ("sftp-segments.bin", 512 * 1024, ""),
            ("sftp-segments-legacy.bin", 2049, ""),
            ("sftp-segments-stale.bin", 512 * 1024, "überlappenden"),
            ("sftp-segments-hash.bin", 512 * 1024, "Prüfsumme"),
            ("sftp-segments-cancel.bin", 512 * 1024, ""),
            ("sftp-segments-serial.bin", 512 * 1024, ""),
        ] {
            let source = local.path().join("source");
            let target = local.path().join(name);
            let content: Vec<_> = (0..size).map(|offset| (offset % 251) as u8).collect();
            std::fs::write(&source, &content).unwrap();
            session
                .upload(&directory, &source, name, false, &cancel)
                .unwrap();
            speed
                .set_override_kib((name == "sftp-segments.bin").then_some(256))
                .unwrap();
            let started = Instant::now();
            let serial = name == "sftp-segments-serial.bin";
            let occupied = serial.then(|| pool.try_claim().unwrap());
            let mut previous = 0;
            let mut intermediate = false;
            let result =
                session.download_with_progress(&directory, name, &target, &cancel, |progress| {
                    assert_eq!(progress.total, (!serial).then_some(size as u64));
                    assert!(progress.received >= previous && progress.received <= size as u64);
                    assert!(!target.exists());
                    previous = progress.received;
                    if progress.phase == DownloadPhase::Receiving
                        && (1..size as u64).contains(&progress.received)
                    {
                        intermediate = true;
                        if name == "sftp-segments-cancel.bin" {
                            cancel.store(true, Relaxed);
                        }
                    }
                });
            drop(occupied);
            assert!(
                pool.try_claim().is_some(),
                "segment slot should be released"
            );
            if name == "sftp-segments-cancel.bin" {
                assert!(intermediate);
                assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Interrupted);
                assert!(!target.exists());
                cancel.store(false, Relaxed);
            } else if expected_error.is_empty() {
                result.unwrap_or_else(|error| panic!("{name}: {error}"));
                assert_eq!(std::fs::read(&target).unwrap(), content);
                if name == "sftp-segments.bin" {
                    assert!(
                        started.elapsed() >= Duration::from_millis(1700),
                        "the ranges exceeded the joint rate limit"
                    );
                    assert!(
                        intermediate,
                        "progress must count bytes written in either range"
                    );
                }
            } else {
                let error = result.unwrap_err();
                assert!(
                    error.to_string().contains(expected_error),
                    "{name}: {error}"
                );
                assert!(!target.exists());
            }
            let listing = session.list_sync(&directory, &cancel).unwrap();
            session
                .delete_sync_entry(&directory, listing.regular_file(name).unwrap(), &cancel)
                .unwrap();
        }
    }
}
