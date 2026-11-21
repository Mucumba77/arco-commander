use super::{Attrs, Wire, path};
use crate::ftp::{
    DownloadPhase, DownloadProgress, Session, Url, check_cancel, invalid_input,
    resume::retry_transfer,
};
use std::fs::OpenOptions;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::atomic::AtomicBool;

const CHUNK: usize = 32 * 1024;

impl Session {
    pub fn upload_sftp_resuming(
        &self,
        directory: &Url,
        source: &Path,
        name: &str,
        cancel: &AtomicBool,
        mut progress: impl FnMut(DownloadProgress),
    ) -> io::Result<()> {
        if !self.is_sftp() {
            return Err(invalid_input("Dieser fortsetzbare Upload benötigt SFTP."));
        }
        let destination = self.file(directory, name)?;
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.share_mode(1); // Keep the source immutable while the upload uses it.
        }
        let mut source = options.open(source)?;
        let original = source.metadata()?;
        if !original.is_file() {
            return Err(invalid_input("Wähle eine reguläre lokale Datei."));
        }
        let size = original.len();
        let modified = original.modified()?;
        let digest = local_digest(&mut source, cancel)?;
        let mut identity = blake3::Hasher::new();
        identity.update(destination.as_str().as_bytes());
        identity.update(&[0]);
        identity.update(self.connection.user.as_bytes());
        identity.update(self.sftp_fingerprint()?.as_bytes());
        identity.update(digest.as_bytes());
        let hex = identity.finalize().to_hex();
        let partial_name = format!(".arco-{}.filepart", &hex[..32]);
        let partial = path(&self.file(directory, &partial_name)?)?;
        let destination = path(&destination)?;
        retry_transfer(cancel, &mut progress, |progress| {
            self.sftp_operation(cancel, |wire| {
                require_new(wire, &destination)?;
                let existing = match wire.lstat(&partial) {
                    Ok(attrs) if attrs.directory() == Some(false) => Some(attrs.size
                        .filter(|offset| *offset <= size)
                        .ok_or_else(|| invalid_input("Ungültige Größe der SFTP-Teildatei."))?),
                    Ok(_) => return Err(invalid_input("Die SFTP-Teildatei ist keine reguläre Datei.")),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => None,
                    Err(error) => return Err(error),
                };
                let handle = if let Some(offset) = existing {
                    source.seek(SeekFrom::Start(0))?;
                    let prefix = local_digest(&mut source.by_ref().take(offset), cancel)?;
                    if compare_remote(wire, &partial, offset, cancel)? != prefix {
                        return Err(invalid_input("Die SFTP-Teildatei stimmt nicht mit dem Anfang der lokalen Datei überein."));
                    }
                    wire.reopen_for_write(&partial)?
                } else {
                    wire.open(&partial, true)?
                };
                let result = (|| {
                    let start = existing.unwrap_or_default();
                    let mut offset = start;
                    let mut rate = self.transfer_rate();
                    source.seek(SeekFrom::Start(offset))?;
                    let mut buffer = [0; CHUNK];
                    while offset < size {
                        progress(DownloadProgress { received: offset, total: Some(size), phase: DownloadPhase::Receiving });
                        check_cancel(cancel)?;
                        let len = (size - offset).min(rate.chunk(CHUNK) as u64) as usize;
                        source.read_exact(&mut buffer[..len])?;
                        rate.wait(offset - start + len as u64, cancel)?;
                        wire.write(&handle, offset, &buffer[..len])?;
                        offset += len as u64;
                    }
                    progress(DownloadProgress { received: offset, total: Some(size), phase: DownloadPhase::Receiving });
                    Ok(())
                })();
                let closed = wire.close(&handle);
                result.and(closed)?;
                progress(DownloadProgress { received: size, total: Some(size), phase: DownloadPhase::Verifying });
                if compare_remote(wire, &partial, size, cancel)? != digest
                    || source.metadata()?.len() != size || source.metadata()?.modified()? != modified {
                    return Err(invalid_input("Die lokale Quelle oder die SFTP-Teildatei hat sich geändert."));
                }
                if self.preserve_transfer_time(false) {
                    let modified = u32::try_from(chrono::DateTime::<chrono::Utc>::from(modified).timestamp())
                        .map_err(io::Error::other)?;
                    let before = wire.lstat(&partial)?;
                    wire.set_attrs(&partial, Attrs {
                        access: Some(before.access.unwrap_or(modified)),
                        modified: Some(modified),
                        ..Attrs::default()
                    })?;
                }
                check_cancel(cancel)?;
                require_new(wire, &destination)?;
                wire.rename(&partial, &destination).map_err(|error| {
                    io::Error::other(format!("SFTP-Datei konnte nicht sicher veröffentlicht werden. Teildatei oder Ziel prüfen. {error}"))
                })
            })
        })
    }
}

fn local_digest(source: &mut impl Read, cancel: &AtomicBool) -> io::Result<blake3::Hash> {
    let mut hash = blake3::Hasher::new();
    let mut buffer = [0; CHUNK];
    loop {
        check_cancel(cancel)?;
        let count = source.read(&mut buffer)?;
        if count == 0 {
            return Ok(hash.finalize());
        }
        hash.update(&buffer[..count]);
    }
}

fn require_new(wire: &mut Wire, destination: &str) -> io::Result<()> {
    match wire.lstat(destination) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "Das SFTP-Ziel ist bereits vorhanden.",
        )),
    }
}

fn compare_remote(
    wire: &mut Wire,
    partial: &str,
    size: u64,
    cancel: &AtomicBool,
) -> io::Result<blake3::Hash> {
    let old = wire.lstat(partial)?;
    if old.directory() != Some(false) || old.size != Some(size) {
        return Err(invalid_input("Die Größe der SFTP-Teildatei stimmt nicht."));
    }
    let handle = wire.open(partial, false)?;
    let result = (|| {
        let mut hash = blake3::Hasher::new();
        let mut offset = 0;
        while offset < size {
            check_cancel(cancel)?;
            let count = (size - offset).min(CHUNK as u64) as u32;
            let chunk = wire
                .read(&handle, offset, count)?
                .ok_or_else(|| invalid_input("Die SFTP-Teildatei ist zu kurz."))?;
            offset += chunk.len() as u64;
            hash.update(&chunk);
        }
        let current = wire.lstat(partial)?;
        if current.size != old.size
            || current.modified != old.modified
            || current.directory() != Some(false)
        {
            return Err(invalid_input(
                "Die SFTP-Teildatei hat sich während der Prüfung geändert.",
            ));
        }
        Ok(hash.finalize())
    })();
    let closed = wire.close(&handle);
    result.and_then(|value| closed.map(|()| value))
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use crate::ftp::{Connection, Security};
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::Ordering;

    #[test]
    #[ignore = "requires scripts/test-sftp-reference.py"]
    fn reference_sftp_upload_resume() {
        let cancel = AtomicBool::new(false);
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
        let local = tempfile::tempdir().unwrap();
        let source = local.path().join("source");
        let data: Vec<_> = (0..260_000).map(|i| (i % 251) as u8).collect();
        fs::write(&source, &data).unwrap();
        let name = "resume-upload.bin";
        let directory = session.initial_directory();
        let root = PathBuf::from(std::env::var_os("ARCO_SFTP_TEST_ROOT").unwrap());
        let target = root.join(name);
        let error = session
            .upload_sftp_resuming(&directory, &source, name, &cancel, |progress| {
                if progress.phase == DownloadPhase::Receiving && progress.received >= 64 * 1024 {
                    cancel.store(true, Ordering::Relaxed);
                }
            })
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        cancel.store(false, Ordering::Relaxed);
        assert!(!target.exists());
        let partial = fs::read_dir(&root)
            .unwrap()
            .filter_map(Result::ok)
            .find(|entry| entry.file_name().to_string_lossy().ends_with(".filepart"))
            .unwrap()
            .path();
        assert_eq!(fs::read(&partial).unwrap(), &data[..64 * 1024]);
        fs::write(&partial, [0; 64 * 1024]).unwrap();
        assert!(
            session
                .upload_sftp_resuming(&directory, &source, name, &cancel, |_| {})
                .is_err()
        );
        assert!(!target.exists());
        fs::write(&partial, &data[..64 * 1024]).unwrap();
        let mut retried = false;
        let mut next_offset = None;
        session
            .upload_sftp_resuming(&directory, &source, name, &cancel, |progress| {
                if progress.phase == DownloadPhase::Reconnecting {
                    retried = true;
                }
                if retried && progress.phase == DownloadPhase::Receiving {
                    next_offset.get_or_insert(progress.received);
                }
            })
            .unwrap();
        assert_eq!(next_offset, Some(128 * 1024));
        assert_eq!(fs::read(&target).unwrap(), data);
        assert!(!partial.exists());
        assert_eq!(
            session
                .upload_sftp_resuming(&directory, &source, name, &cancel, |_| {})
                .unwrap_err()
                .kind(),
            io::ErrorKind::AlreadyExists
        );
        fs::remove_file(target).unwrap();
        let speed = crate::ftp::TransferLimit::default();
        speed.set_override_kib(Some(8)).unwrap();
        let adjustable = session.with_transfer_limit(speed.clone());
        let name = "adjustable-upload.bin";
        let started = std::time::Instant::now();
        let mut changed = false;
        adjustable
            .upload_sftp_resuming(&directory, &source, name, &cancel, |progress| {
                if !changed
                    && progress.phase == DownloadPhase::Receiving
                    && progress.received > 0
                    && progress.received < data.len() as u64
                {
                    speed.set_override_kib(Some(0)).unwrap();
                    changed = true;
                }
            })
            .unwrap();
        assert!(changed && started.elapsed() < std::time::Duration::from_secs(8));
        assert_eq!(fs::read(root.join(name)).unwrap(), data);
        fs::remove_file(root.join(name)).unwrap();
    }
}
