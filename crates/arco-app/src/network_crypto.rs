use crate::network_sync::{FtpEndpoint, check_transfer_mask};
use arco_core::{encrypted_file, files};
use arco_windows::ftp::{Entry, SyncUpload};
use std::io::{self, Seek};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

pub const SUFFIX: &str = ".arco.age";

pub fn upload(
    (session, directory): &FtpEndpoint,
    source: &Path,
    passphrase: &str,
    cancel: &AtomicBool,
    mut phase: impl FnMut(&str),
) -> io::Result<Entry> {
    let name = source
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "Für die Quelldatei fehlt ein gültiger Name.",
            )
        })?;
    check_transfer_mask(session, name)?;
    phase("Datei lokal verschlüsseln");
    let mut package = tempfile::Builder::new()
        .prefix("arco-")
        .rand_bytes(24)
        .suffix(SUFFIX)
        .tempfile()?;
    encrypted_file::seal(source, passphrase, &mut package, cancel)?;
    package.as_file().sync_all()?;
    let metadata = package.as_file().metadata()?;
    let path = package.path();
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .expect("ASCII temporary name");
    phase("Verschlüsseltes Paket hochladen");
    session.upload_sync(
        directory,
        SyncUpload {
            source: path,
            name,
            expected: None,
            size: metadata.len(),
            modified_utc: None,
        },
        || files::ensure_unchanged(path, &metadata),
        cancel,
    )
}

pub fn download(
    (session, directory): &FtpEndpoint,
    entry: &Entry,
    folder: &Path,
    passphrase: &str,
    cancel: &AtomicBool,
    mut phase: impl FnMut(&str),
) -> io::Result<PathBuf> {
    encrypted_file::validate_passphrase(passphrase, false)?;
    phase("Verschlüsseltes Paket herunterladen");
    let mut package = tempfile::tempfile()?;
    session.fetch_sync(directory, entry, &package, cancel)?;
    package.rewind()?;
    phase("Datei lokal entschlüsseln und prüfen");
    encrypted_file::unseal(package, folder, passphrase, cancel, |name| {
        check_transfer_mask(session, name)
    })
}
