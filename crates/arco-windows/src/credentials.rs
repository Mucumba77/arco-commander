use std::{io, ptr, slice};
use windows_sys::Win32::{Foundation::ERROR_NOT_FOUND, Security::Credentials::*};
use zeroize::{Zeroize, Zeroizing};

pub fn write(target: &str, password: &str) -> io::Result<()> {
    if password.len() > CRED_MAX_CREDENTIAL_BLOB_SIZE as usize {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Das Passwort kann in dieser Länge nicht gespeichert werden.",
        ));
    }
    let mut target = target_name(target)?;
    let mut blob = Zeroizing::new(password.as_bytes().to_vec());
    let record = CREDENTIALW {
        Type: CRED_TYPE_GENERIC,
        TargetName: target.as_mut_ptr(),
        CredentialBlobSize: blob.len() as u32,
        CredentialBlob: blob.as_mut_ptr(),
        Persist: CRED_PERSIST_LOCAL_MACHINE,
        ..CREDENTIALW::default()
    };
    if unsafe { CredWriteW(&record, 0) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub fn read(target: &str) -> io::Result<Option<Zeroizing<String>>> {
    let target = target_name(target)?;
    let mut record = ptr::null_mut();
    if unsafe { CredReadW(target.as_ptr(), CRED_TYPE_GENERIC, 0, &mut record) } == 0 {
        missing_or_error()?;
        return Ok(None);
    }
    // CredReadW owns the record and blob as a single allocation.
    let bytes = unsafe {
        let mut bytes = Vec::new();
        if (*record).CredentialBlobSize != 0 {
            let blob = slice::from_raw_parts_mut(
                (*record).CredentialBlob,
                (*record).CredentialBlobSize as usize,
            );
            bytes.extend_from_slice(blob);
            blob.zeroize();
        }
        CredFree(record.cast());
        bytes
    };
    match String::from_utf8(bytes) {
        Ok(password) => Ok(Some(Zeroizing::new(password))),
        Err(error) => {
            Zeroizing::new(error.into_bytes());
            Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Ungültiger Eintrag in der Windows-Anmeldeinformationsverwaltung.",
            ))
        }
    }
}

pub fn delete(target: &str) -> io::Result<()> {
    let target = target_name(target)?;
    if unsafe { CredDeleteW(target.as_ptr(), CRED_TYPE_GENERIC, 0) } == 0 {
        return missing_or_error();
    }
    Ok(())
}

fn target_name(target: &str) -> io::Result<Vec<u16>> {
    if target.is_empty() || target.contains('\0') {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    Ok(target.encode_utf16().chain([0]).collect())
}

fn missing_or_error() -> io::Result<()> {
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(ERROR_NOT_FOUND as i32) {
        Ok(())
    } else {
        Err(error)
    }
}
