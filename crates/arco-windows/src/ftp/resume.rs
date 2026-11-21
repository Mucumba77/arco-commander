use super::{DownloadPhase, DownloadProgress, Session, Url, check_cancel, run, run_observed};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::Duration;
use tempfile::{NamedTempFile, TempPath};

const OVERLAP: u64 = 64 * 1024;

#[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
struct Stamp {
    size: u64,
    modified: String,
    #[serde(default)]
    unique: Option<String>,
}

impl Session {
    pub fn upload_resuming(
        &self,
        directory: &Url,
        source: &Path,
        remote: &str,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        self.upload_resuming_with_progress(directory, source, remote, cancel, |_| {})
    }

    pub fn upload_resuming_with_progress(
        &self,
        directory: &Url,
        source: &Path,
        remote: &str,
        cancel: &AtomicBool,
        mut progress: impl FnMut(DownloadProgress),
    ) -> io::Result<()> {
        if !self.security().is_ftp() || self.is_http_forward_ftp() {
            return Err(super::invalid_input(
                "Dieser fortsetzbare Upload benötigt FTP oder FTPS.",
            ));
        }
        let url = self.file(directory, remote)?;
        let source_name = source
            .to_str()
            .ok_or_else(|| io::Error::other("curl benötigt einen Unicode-Dateipfad."))?;
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.share_mode(1); // Allow readers and keep the upload source fixed.
        }
        let source_lock = options.open(source)?;
        let original = source_lock.metadata()?;
        if !original.is_file() {
            return Err(io::Error::other(
                "Für FTP-Resume wird eine reguläre lokale Datei benötigt.",
            ));
        }
        let size = original.len();
        let modified = original.modified()?;
        let verify_source = || -> io::Result<()> {
            let current = fs::metadata(source)?;
            if current.len() != size || current.modified()? != modified {
                return Err(io::Error::other("Die lokale FTP-Quelle hat sich geändert."));
            }
            Ok(())
        };
        let mut appended_from: Option<Stamp> = None;
        retry_transfer(cancel, &mut progress, |progress| {
            let expected = self.remote_stamp(directory, remote, cancel)?;
            let offset = expected.size;
            if offset == 0 || offset > size || (offset == size && appended_from.is_none()) {
                return Err(io::Error::other(
                    "Die entfernte Teildatei muss kleiner als die lokale Quelle und darf nicht leer sein.",
                ));
            }
            if appended_from.as_ref().is_some_and(|previous| {
                offset < previous.size
                    || previous.unique.is_some() && expected.unique != previous.unique
            }) {
                return Err(io::Error::other(
                    "Die FTP-Teildatei wurde nach dem Verbindungsabbruch gekürzt oder ersetzt.",
                ));
            }
            let staged = tempfile::tempdir()?;
            let verify_remote = |stamp: &Stamp, path: &Path| -> io::Result<()> {
                self.download_bounded(directory, remote, path, stamp.size, cancel)?;
                compare_prefix(source, path, stamp.size, cancel)?;
                if self.remote_stamp(directory, remote, cancel)? != *stamp {
                    return Err(io::Error::other(
                        "Die FTP-Datei hat sich während der Prüfung geändert.",
                    ));
                }
                verify_source()
            };
            progress(DownloadProgress {
                received: offset,
                total: Some(size),
                phase: DownloadPhase::Verifying,
            });
            verify_remote(&expected, &staged.path().join("prefix"))?;
            if offset == size {
                // The server kept the complete data even though the last attempt lost its reply.
                return Ok(());
            }
            progress(DownloadProgress {
                received: offset,
                total: Some(size),
                phase: DownloadPhase::Receiving,
            });
            let request = self.transfer_command(&url)?;
            appended_from = Some(expected);
            run(
                request,
                &[
                    "--continue-at",
                    &offset.to_string(),
                    "--upload-file",
                    source_name,
                ],
                None,
                cancel,
            )?;
            let actual = self.remote_stamp(directory, remote, cancel)?;
            if actual.size != size {
                return Err(io::Error::other(
                    "FTP-Resume: Die entfernte Datei hat nach dem Anhängen nicht die erwartete Größe.",
                ));
            }
            progress(DownloadProgress {
                received: size,
                total: Some(size),
                phase: DownloadPhase::Verifying,
            });
            verify_remote(&actual, &staged.path().join("complete"))
        })?;
        if self.preserve_transfer_time(false) {
            let time = chrono::DateTime::<chrono::Utc>::from(modified);
            self.set_ftp_upload_modified(
                directory,
                remote,
                &time.format("%Y%m%d%H%M%S").to_string(),
                cancel,
            )?;
        }
        Ok(())
    }

    pub fn download_resuming(
        &self,
        directory: &Url,
        remote: &str,
        destination: &Path,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        self.download_resuming_with_progress(directory, remote, destination, cancel, |_| {})
    }

    pub fn download_resuming_with_progress(
        &self,
        directory: &Url,
        remote: &str,
        destination: &Path,
        cancel: &AtomicBool,
        mut progress: impl FnMut(DownloadProgress),
    ) -> io::Result<()> {
        if !(self.is_sftp() || self.security().is_ftp() && !self.is_http_forward_ftp()) {
            return Err(super::invalid_input(
                "Dieser fortsetzbare Download benötigt FTP, FTPS oder SFTP.",
            ));
        }
        retry_transfer(cancel, &mut progress, |progress| {
            self.resume_download(directory, remote, destination, cancel, progress)
        })
    }

    fn resume_download(
        &self,
        directory: &Url,
        remote: &str,
        destination: &Path,
        cancel: &AtomicBool,
        progress: &mut dyn FnMut(DownloadProgress),
    ) -> io::Result<()> {
        let url = self.file(directory, remote)?;
        if destination.try_exists()? {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                destination.display().to_string(),
            ));
        }
        let (partial_path, stamp_path) = self.checkpoint_paths(&url, destination)?;
        if self.is_sftp()
            && self.resume_sftp_segments(&url, destination, &partial_path, cancel, progress)?
        {
            return Ok(());
        }
        let expected = self.remote_stamp(directory, remote, cancel)?;
        let mut partial = open_checkpoint(&partial_path, &stamp_path, &expected)?;
        let offset = partial.metadata()?.len();
        if offset > expected.size {
            return Err(io::Error::other(
                "Die lokale Teildatei ist größer als die Serverdatei.",
            ));
        }
        if offset > 0 {
            let start = offset.saturating_sub(OVERLAP);
            let remote_bytes = if self.is_sftp() {
                self.read_sftp_range(&url, start..offset, cancel)?
            } else {
                self.read_ftp_overlap(&url, start..offset, cancel)?
            };
            let mut local_bytes = vec![0; (offset - start) as usize];
            partial.seek(SeekFrom::Start(start))?;
            partial.read_exact(&mut local_bytes)?;
            if remote_bytes != local_bytes {
                return Err(io::Error::other(
                    "Teildatei stimmt im überlappenden Bereich nicht mit dem Server überein",
                ));
            }
        }
        let mut observe = || {
            let received = partial.metadata()?.len();
            if received > expected.size {
                return Err(io::Error::other(
                    "Die empfangenen Daten überschreiten die gemeldete Dateigröße.",
                ));
            }
            progress(DownloadProgress {
                received,
                total: Some(expected.size),
                phase: DownloadPhase::Receiving,
            });
            Ok(())
        };
        observe()?;
        if offset < expected.size {
            (&partial).seek(SeekFrom::Start(offset))?;
            if self.is_sftp() {
                self.receive_sftp_file(
                    &url,
                    &partial,
                    offset,
                    Some(expected.size),
                    cancel,
                    |_| observe(),
                )?;
            } else {
                let offset = offset.to_string();
                run_observed(
                    self.transfer_command(&url)?,
                    &["--continue-at", &offset],
                    Some(partial.try_clone()?),
                    cancel,
                    observe,
                )?;
            }
        }
        progress(DownloadProgress {
            received: partial.metadata()?.len(),
            total: Some(expected.size),
            phase: DownloadPhase::Verifying,
        });
        if partial.metadata()?.len() != expected.size
            || self.remote_stamp(directory, remote, cancel)? != expected
        {
            return Err(io::Error::other(
                "Die Serverdatei hat sich während des Downloads geändert.",
            ));
        }
        if self.preserve_transfer_time(false) {
            let modified =
                chrono::NaiveDateTime::parse_from_str(&expected.modified, "%Y%m%d%H%M%S%.f")
                    .map_err(io::Error::other)?
                    .and_utc();
            partial.set_modified(modified.into())?;
        }
        partial.sync_all()?;
        drop(partial);
        check_cancel(cancel)?;
        if let Err(error) = TempPath::try_from_path(&partial_path)?.persist_noclobber(destination) {
            let _ = error.path.keep();
            return Err(error.error);
        }
        let _ = fs::remove_file(stamp_path);
        Ok(())
    }

    fn read_ftp_overlap(
        &self,
        url: &Url,
        range: std::ops::Range<u64>,
        cancel: &AtomicBool,
    ) -> io::Result<Vec<u8>> {
        let length = range.end - range.start;
        let overlap = format!("{}-{}", range.start, range.end - 1);
        let mut data = tempfile::tempfile()?;
        let result = run(
            self.transfer_command(url)?,
            &["--range", &overlap],
            Some(data.try_clone()?),
            cancel,
        );
        // FTPS can close its data socket while ending a bounded range. The bytes
        // already received are sufficient for the overlap comparison.
        if let Err(error) = result
            && (!is_retryable_transport(&error) || data.metadata()?.len() != length)
        {
            return Err(error);
        }
        check_cancel(cancel)?;
        data.seek(SeekFrom::Start(0))?;
        let mut bytes = Vec::new();
        data.take(length + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 != length {
            return Err(io::Error::other(
                "FTP lieferte einen ungültigen Prüfbereich.",
            ));
        }
        Ok(bytes)
    }

    fn remote_stamp(
        &self,
        directory: &Url,
        remote: &str,
        cancel: &AtomicBool,
    ) -> io::Result<Stamp> {
        let listing = self.list_sync(directory, cancel)?;
        let entry = listing.regular_file(remote)?;
        let (Some(size), Some(modified)) = (entry.size, &entry.modified) else {
            return Err(io::Error::other(
                "Fortsetzbare Downloads benötigen Größe und Änderungszeit vom Server.",
            ));
        };
        Ok(Stamp {
            size,
            modified: modified.clone(),
            unique: entry.unique.clone(),
        })
    }

    fn checkpoint_paths(&self, url: &Url, destination: &Path) -> io::Result<(PathBuf, PathBuf)> {
        let parent = destination
            .parent()
            .ok_or_else(|| io::Error::other("missing destination directory"))?;
        let filename = destination
            .file_name()
            .ok_or_else(|| io::Error::other("missing destination name"))?;
        let mut identity = blake3::Hasher::new();
        identity.update(url.as_str().as_bytes());
        identity.update(&[0]);
        identity.update(self.connection.user.as_bytes());
        identity.update(&[0]);
        identity.update(filename.as_encoded_bytes());
        if self.is_sftp() {
            identity.update(&[0]);
            identity.update(self.sftp_fingerprint()?.as_bytes());
        }
        let hex = identity.finalize().to_hex();
        let basename = format!(".arco-ftp-{}", &hex[..24]);
        Ok((
            parent.join(format!("{basename}.part")),
            parent.join(format!("{basename}.json")),
        ))
    }
}

pub(super) fn retry_transfer(
    cancel: &AtomicBool,
    progress: &mut impl FnMut(DownloadProgress),
    mut work: impl FnMut(&mut dyn FnMut(DownloadProgress)) -> io::Result<()>,
) -> io::Result<()> {
    let mut retries = 2;
    loop {
        check_cancel(cancel)?;
        match work(progress) {
            Err(error) if retries > 0 && is_retryable_transport(&error) => {
                retries -= 1;
                progress(DownloadProgress {
                    received: 0,
                    total: None,
                    phase: DownloadPhase::Reconnecting,
                });
                for _ in 0..4 {
                    check_cancel(cancel)?;
                    std::thread::sleep(Duration::from_millis(125));
                }
            }
            result => return result,
        }
    }
}

pub(super) fn is_retryable_transport(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::ConnectionAborted
            | io::ErrorKind::ConnectionRefused
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::BrokenPipe
            | io::ErrorKind::TimedOut
            | io::ErrorKind::UnexpectedEof
    )
}

fn compare_prefix(
    source: &Path,
    downloaded: &Path,
    size: u64,
    cancel: &AtomicBool,
) -> io::Result<()> {
    let mut source = File::open(source)?;
    let mut downloaded = File::open(downloaded)?;
    if downloaded.metadata()?.len() != size {
        return Err(io::Error::other(
            "FTP-Resume: Die heruntergeladene Dateigröße stimmt nicht.",
        ));
    }
    let mut left = [0; 64 * 1024];
    let mut right = [0; 64 * 1024];
    let mut remaining = size;
    while remaining > 0 {
        check_cancel(cancel)?;
        let width = remaining.min(left.len() as u64) as usize;
        source.read_exact(&mut left[..width])?;
        downloaded.read_exact(&mut right[..width])?;
        if left[..width] != right[..width] {
            return Err(io::Error::other(
                "FTP-Resume: Der Inhalt der entfernten Datei weicht von der lokalen Quelle ab.",
            ));
        }
        remaining -= width as u64;
    }
    Ok(())
}

fn open_checkpoint(partial: &Path, stamp: &Path, expected: &Stamp) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(1); // FILE_SHARE_READ
    }
    match fs::symlink_metadata(partial) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let file = options.create_new(true).open(partial)?;
            let saved = (|| {
                let temporary = NamedTempFile::new_in(
                    stamp
                        .parent()
                        .ok_or_else(|| io::Error::other("missing stamp parent"))?,
                )?;
                serde_json::to_writer(temporary.as_file(), expected).map_err(io::Error::other)?;
                temporary.as_file().sync_all()?;
                temporary
                    .persist_noclobber(stamp)
                    .map_err(|error| error.error)?;
                Ok(())
            })();
            if let Err(error) = saved {
                drop(file);
                let _ = fs::remove_file(partial);
                return Err(error);
            }
            Ok(file)
        }
        Ok(meta) if is_regular(&meta) => {
            if !is_regular(&fs::symlink_metadata(stamp)?) {
                return Err(io::Error::other("Ungültige Fortsetzungsdatei."));
            }
            let mut bytes = Vec::new();
            File::open(stamp)?.take(1025).read_to_end(&mut bytes)?;
            let actual: Stamp = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
            if &actual != expected {
                return Err(io::Error::other(
                    "Die Serverdatei stimmt nicht mehr mit der gespeicherten Teildatei überein.",
                ));
            }
            options.open(partial)
        }
        Ok(_) => Err(io::Error::other("Die Teildatei ist keine reguläre Datei.")),
        Err(error) => Err(error),
    }
}

pub(super) fn is_regular(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.is_file() && metadata.file_attributes() & 0x400 == 0
    }
    #[cfg(not(windows))]
    {
        metadata.is_file() && !metadata.is_symlink()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ftp::{Connection, Security};
    use std::sync::atomic::Ordering;

    #[test]
    #[cfg(windows)]
    #[ignore = "requires scripts/test-sftp-reference.py"]
    fn reference_sftp_resume() {
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
        let directory = session.initial_directory();
        let local = tempfile::tempdir().unwrap();
        let source = local.path().join("input");
        let target = local.path().join("output");
        let data: Vec<_> = (0..270_000).map(|index| (index % 251) as u8).collect();
        fs::write(&source, &data).unwrap();
        let name = "sftp-resume.bin";
        let cancel = AtomicBool::new(false);
        session
            .upload(&directory, &source, name, false, &cancel)
            .unwrap();
        let interrupted = session
            .download_resuming_with_progress(&directory, name, &target, &cancel, |progress| {
                if progress.received >= 96 * 1024 {
                    cancel.store(true, Ordering::Relaxed);
                }
            })
            .unwrap_err();
        assert_eq!(interrupted.kind(), io::ErrorKind::Interrupted);
        assert!(!target.exists());
        cancel.store(false, Ordering::Relaxed);

        let url = session.file(&directory, name).unwrap();
        let (partial, stamp) = session.checkpoint_paths(&url, &target).unwrap();
        let saved = fs::read(&partial).unwrap();
        assert_eq!(saved, data[..96 * 1024]);
        fs::write(&partial, vec![0; saved.len()]).unwrap();
        assert!(
            session
                .download_resuming(&directory, name, &target, &cancel)
                .is_err()
        );
        assert!(!target.exists());
        fs::write(&partial, &saved).unwrap();
        let mut started_at = None;
        session
            .download_resuming_with_progress(&directory, name, &target, &cancel, |progress| {
                started_at.get_or_insert(progress.received);
            })
            .unwrap();
        assert_eq!(started_at, Some(saved.len() as u64));
        assert_eq!(fs::read(&target).unwrap(), data);
        assert!(!partial.exists());
        assert!(!stamp.exists());

        let reconnect_name = "sftp-reconnect.bin";
        session
            .upload(&directory, &source, reconnect_name, false, &cancel)
            .unwrap();
        let destination = local.path().join("after-reconnect");
        let mut reconnects = 0;
        let mut restarted_at = None;
        session
            .download_resuming_with_progress(
                &directory,
                reconnect_name,
                &destination,
                &cancel,
                |progress| {
                    if progress.phase == DownloadPhase::Reconnecting {
                        reconnects += 1;
                    } else if reconnects == 1 && progress.phase == DownloadPhase::Receiving {
                        restarted_at.get_or_insert(progress.received);
                    }
                },
            )
            .unwrap();
        assert_eq!(reconnects, 1);
        assert_eq!(restarted_at, Some(96 * 1024));
        assert_eq!(fs::read(&destination).unwrap(), data);

        for entry in session.list_sync(&directory, &cancel).unwrap().entries {
            if entry.name == name || entry.name == reconnect_name {
                session
                    .delete_sync_entry(&directory, &entry, &cancel)
                    .unwrap();
            }
        }
    }

    #[test]
    #[cfg(windows)]
    #[ignore = "requires a disposable writable local FTP server"]
    fn resumes_only_matching_overlap() {
        let address = std::env::var("ARCO_TEST_FTP").unwrap();
        let session = Connection::new(&address, "arco", Security::Plain)
            .unwrap()
            .authenticate("test-only")
            .unwrap();
        let directory = session.initial_directory();
        let local = tempfile::tempdir().unwrap();
        let source = local.path().join("source.txt");
        let target = local.path().join("target.txt");
        let name = "resume-fixture.txt";
        let data: Vec<_> = (0..100_000).map(|index| (index % 251) as u8).collect();
        fs::write(&source, &data).unwrap();
        let cancel = AtomicBool::new(false);
        session
            .upload(&directory, &source, name, false, &cancel)
            .unwrap();
        let url = session.file(&directory, name).unwrap();
        let (partial, stamp) = session.checkpoint_paths(&url, &target).unwrap();
        let expected = session.remote_stamp(&directory, name, &cancel).unwrap();
        fs::write(&partial, vec![0; 70_000]).unwrap();
        fs::write(&stamp, serde_json::to_vec(&expected).unwrap()).unwrap();
        assert!(
            session
                .download_resuming(&directory, name, &target, &cancel)
                .is_err()
        );
        assert!(!target.exists());
        fs::write(&partial, &data[..70_000]).unwrap();
        session
            .download_resuming(&directory, name, &target, &cancel)
            .unwrap();
        assert_eq!(fs::read(&target).unwrap(), data);
        assert!(!partial.exists());
        assert!(!stamp.exists());
    }
}
