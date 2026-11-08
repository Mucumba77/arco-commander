use super::{Session, Url, check_cancel, run};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use tempfile::{NamedTempFile, TempPath};

const OVERLAP: u64 = 64 * 1024;

#[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
struct Stamp {
    size: u64,
    modified: String,
}

impl Session {
    pub fn download_resuming(
        &self,
        directory: &Url,
        remote: &str,
        destination: &Path,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        let url = self.file(directory, remote)?;
        if destination.try_exists()? {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                destination.display().to_string(),
            ));
        }
        let expected = self.remote_stamp(directory, remote, cancel)?;
        let (partial_path, stamp_path) = self.checkpoint_paths(&url, destination)?;
        let mut partial = open_checkpoint(&partial_path, &stamp_path, &expected)?;
        let offset = partial.metadata()?.len();
        if offset > expected.size {
            return Err(io::Error::other(
                "lokale Teildatei ist größer als die FTP-Datei",
            ));
        }
        if offset > 0 {
            let start = offset.saturating_sub(OVERLAP);
            let overlap = format!("{start}-{}", offset - 1);
            let remote_bytes = run(self.command(&url)?, &["--range", &overlap], None, cancel)?;
            let mut local_bytes = vec![0; (offset - start) as usize];
            partial.seek(SeekFrom::Start(start))?;
            partial.read_exact(&mut local_bytes)?;
            if remote_bytes != local_bytes {
                return Err(io::Error::other(
                    "Teildatei stimmt im überlappenden Bereich nicht mit dem Server überein",
                ));
            }
        }
        if offset < expected.size {
            partial.seek(SeekFrom::Start(offset))?;
            let offset = offset.to_string();
            run(
                self.command(&url)?,
                &["--continue-at", &offset],
                Some(partial.try_clone()?),
                cancel,
            )?;
        }
        if partial.metadata()?.len() != expected.size
            || self.remote_stamp(directory, remote, cancel)? != expected
        {
            return Err(io::Error::other(
                "FTP-Datei oder Größe hat sich während des Downloads geändert",
            ));
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

    fn remote_stamp(
        &self,
        directory: &Url,
        remote: &str,
        cancel: &AtomicBool,
    ) -> io::Result<Stamp> {
        let entry = self
            .list(directory, cancel)?
            .entries
            .into_iter()
            .find(|entry| entry.name == remote && entry.directory == Some(false))
            .ok_or_else(|| {
                io::Error::other("der Server liefert keinen eindeutigen MLSD-Dateieintrag")
            })?;
        let (Some(size), Some(modified)) = (entry.size, entry.modified) else {
            return Err(io::Error::other(
                "FTP-Resume erfordert Größe und Änderungszeit vom Server",
            ));
        };
        Ok(Stamp { size, modified })
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
        let hex = identity.finalize().to_hex();
        let basename = format!(".arco-ftp-{}", &hex[..24]);
        Ok((
            parent.join(format!("{basename}.part")),
            parent.join(format!("{basename}.json")),
        ))
    }
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
                return Err(io::Error::other("ungültige FTP-Fortsetzungsdatei"));
            }
            let mut bytes = Vec::new();
            File::open(stamp)?.take(1025).read_to_end(&mut bytes)?;
            let actual: Stamp = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
            if &actual != expected {
                return Err(io::Error::other(
                    "FTP-Datei stimmt nicht mehr mit der gespeicherten Teildatei überein",
                ));
            }
            options.open(partial)
        }
        Ok(_) => Err(io::Error::other("FTP-Teildatei ist keine reguläre Datei")),
        Err(error) => Err(error),
    }
}

fn is_regular(metadata: &fs::Metadata) -> bool {
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
