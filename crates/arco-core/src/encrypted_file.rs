use crate::{files, sync::LocalRoot};
use age::secrecy::SecretString;
use std::ffi::OsStr;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

pub fn validate_passphrase(passphrase: &str, creating: bool) -> io::Result<()> {
    if passphrase.is_empty()
        || passphrase.len() > 1024
        || passphrase.chars().any(char::is_control)
        || (creating && passphrase.chars().count() < 12)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            if creating {
                "Verwende mindestens zwölf Zeichen und keine Steuerzeichen (maximal 1024 Bytes)."
            } else {
                "Gib eine gültige Passphrase ein."
            },
        ));
    }
    Ok(())
}

struct Cancellable<'a, R> {
    reader: R,
    cancel: &'a AtomicBool,
}

impl<R: Read> Read for Cancellable<'_, R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        files::check_cancel(self.cancel)?;
        self.reader.read(buffer)
    }
}

pub fn seal(
    source: &Path,
    passphrase: &str,
    output: impl Write,
    cancel: &AtomicBool,
) -> io::Result<()> {
    // A fixed work factor keeps packages portable across supported machines.
    let work_factor = if cfg!(test) { 10 } else { 17 };
    validate_passphrase(passphrase, true)?;
    files::check_cancel(cancel)?;
    let name = files::leaf_name(source.file_name().unwrap_or_default())?;
    let before = files::regular_metadata(source)?;
    if !before.is_file() {
        return Err(invalid_data("Wähle eine einzelne reguläre Datei."));
    }
    let input = files::open_for_read(source)?;
    files::ensure_unchanged(source, &before)?;
    let mut recipient = age::scrypt::Recipient::new(SecretString::from(passphrase.to_owned()));
    recipient.set_work_factor(work_factor);
    let encryptor =
        age::Encryptor::with_recipients(std::iter::once(&recipient as &dyn age::Recipient))
            .map_err(io::Error::other)?;
    let mut archive = tar::Builder::new(encryptor.wrap_output(output)?);
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Regular);
    header.set_size(before.len());
    header.set_mode(0o600);
    let mut limited = Cancellable {
        reader: input,
        cancel,
    }
    .take(before.len());
    archive.append_data(&mut header, Path::new(name), &mut limited)?;
    if limited.limit() != 0 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "Die Quelldatei wurde verkürzt.",
        ));
    }
    files::ensure_unchanged(source, &before)?;
    files::check_cancel(cancel)?;
    archive.into_inner()?.finish()?;
    Ok(())
}

pub fn unseal(
    input: impl Read,
    folder: &Path,
    passphrase: &str,
    cancel: &AtomicBool,
    allow_name: impl FnOnce(&str) -> io::Result<()>,
) -> io::Result<PathBuf> {
    validate_passphrase(passphrase, false)?;
    files::check_cancel(cancel)?;
    let root = LocalRoot::open(folder)?;
    let mut identity = age::scrypt::Identity::new(SecretString::from(passphrase.to_owned()));
    identity.set_max_work_factor(19);
    let input = Cancellable {
        reader: input,
        cancel,
    };
    let decryptor = age::Decryptor::new(input).map_err(io::Error::other)?;
    if !decryptor.is_scrypt() {
        return Err(invalid_data("Das age-Paket verwendet keine Passphrase."));
    }
    let reader = decryptor
        .decrypt(std::iter::once(&identity as &dyn age::Identity))
        .map_err(|_| invalid_data("Passphrase oder verschlüsseltes Dateipaket ungültig."))?;
    let mut archive = tar::Archive::new(reader);
    let mut entries = archive.entries()?;
    let mut entry = entries.next().transpose()?.ok_or_else(invalid_payload)?;
    if !entry.header().entry_type().is_file() {
        return Err(invalid_payload());
    }
    let name = checked_name(entry.path_bytes().as_ref())?;
    allow_name(&name)?;
    let target = root.checked_path(Path::new(&name))?;
    let mut output = tempfile::NamedTempFile::new_in(root.path())?;
    if io::copy(
        &mut Cancellable {
            reader: &mut entry,
            cancel,
        },
        &mut output,
    )? != entry.size()
    {
        return Err(invalid_payload());
    }
    drop(entry);
    if entries.next().transpose()?.is_some() {
        return Err(invalid_payload());
    }
    // tar stops at its first zero block. Read through age EOF to authenticate
    // the final chunk and verify the remainder of the TAR terminator.
    let mut reader = archive.into_inner();
    let mut block = [0u8; 512];
    reader.read_exact(&mut block)?;
    loop {
        if block.iter().any(|byte| *byte != 0) {
            return Err(invalid_payload());
        }
        let length = reader.read(&mut block)?;
        if length == 0 {
            break;
        }
        block[length..].fill(0);
    }
    root.verify()?;
    files::publish_checked(output, &target, None, cancel)?;
    Ok(folder.join(name))
}

fn checked_name(bytes: &[u8]) -> io::Result<String> {
    let name = std::str::from_utf8(bytes).map_err(|_| invalid_payload())?;
    files::leaf_name(OsStr::new(name))?;
    Ok(name.to_owned())
}

fn invalid_data(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn invalid_payload() -> io::Error {
    invalid_data("Das entschlüsselte Paket muss genau eine reguläre Datei enthalten.")
}

#[cfg(test)]
mod tests {
    use super::{seal, unseal};
    use std::fs;
    use std::sync::atomic::AtomicBool;

    #[test]
    fn authenticates_content_and_name_before_publication() {
        let source = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        let name = "Überblick – vertraulich.txt";
        let data = vec![42; 150_000];
        fs::write(source.path().join(name), &data).unwrap();
        let cancel = AtomicBool::new(false);
        let password = "synthetische-Testpassphrase";
        let mut package = Vec::new();
        seal(&source.path().join(name), password, &mut package, &cancel).unwrap();
        assert!(
            !package
                .windows(name.len())
                .any(|part| part == name.as_bytes())
        );
        assert!(unseal(&package[..], target.path(), "falsch", &cancel, |_| Ok(())).is_err());
        assert!(
            unseal(
                &package[..package.len() - 1],
                target.path(),
                password,
                &cancel,
                |_| Ok(())
            )
            .is_err()
        );
        let mut tampered = package.clone();
        *tampered.last_mut().unwrap() ^= 1;
        assert!(unseal(&tampered[..], target.path(), password, &cancel, |_| Ok(())).is_err());
        assert_eq!(target.path().read_dir().unwrap().count(), 0);
        let path = unseal(&package[..], target.path(), password, &cancel, |_| Ok(())).unwrap();
        assert_eq!(path, target.path().join(name));
        assert_eq!(fs::read(&path).unwrap(), data);
        fs::write(&path, b"existing").unwrap();
        assert!(unseal(&package[..], target.path(), password, &cancel, |_| Ok(())).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"existing");
        assert_eq!(target.path().read_dir().unwrap().count(), 1);
    }
}
