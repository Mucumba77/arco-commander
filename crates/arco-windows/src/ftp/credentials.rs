use super::{Connection, check_password};
use std::{io, ptr, slice};
use windows::Win32::System::Com::CoCreateGuid;
use windows_sys::Win32::{Foundation::ERROR_NOT_FOUND, Security::Credentials::*};
use zeroize::{Zeroize, Zeroizing};

impl Connection {
    pub fn store_password(&self, password: &str) -> io::Result<String> {
        check_password(password)?;
        if password.is_empty() || password.len() > CRED_MAX_CREDENTIAL_BLOB_SIZE as usize {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Das Passwort kann in dieser Länge nicht gespeichert werden.",
            ));
        }
        let guid = unsafe { CoCreateGuid() }.map_err(io::Error::other)?;
        let id = format!("{:032x}", guid.to_u128());
        let mut target = self.credential_target(&id)?;
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
        Ok(id)
    }

    pub fn read_password(&self, id: &str) -> io::Result<Option<Zeroizing<String>>> {
        let target = self.credential_target(id)?;
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
                    "Ungültiger FTP-Eintrag in der Anmeldeinformationsverwaltung.",
                ))
            }
        }
    }

    pub fn delete_password(&self, id: &str) -> io::Result<()> {
        let target = self.credential_target(id)?;
        if unsafe { CredDeleteW(target.as_ptr(), CRED_TYPE_GENERIC, 0) } == 0 {
            return missing_or_error();
        }
        Ok(())
    }

    pub fn same_credential_scope(&self, other: &Self) -> bool {
        self.credential_scope() == other.credential_scope()
    }

    fn credential_target(&self, id: &str) -> io::Result<Vec<u16>> {
        if id.len() != 32 || !id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Ungültige FTP-Zugangsdatenreferenz.",
            ));
        }
        let hash = self.credential_scope();
        Ok(format!("Arco Commander FTP/{id}-{hash}")
            .encode_utf16()
            .chain([0])
            .collect())
    }

    fn credential_scope(&self) -> blake3::Hash {
        let scope = format!(
            "{:?}\n{}\n{:?}\n{}\n{:?}",
            self.security,
            self.url.host_str().unwrap_or_default(),
            self.url.port_or_known_default(),
            self.user,
            self.proxy,
        );
        blake3::hash(scope.as_bytes())
    }
}

fn missing_or_error() -> io::Result<()> {
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(ERROR_NOT_FOUND as i32) {
        return Ok(());
    }
    Err(error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ftp::Security;

    #[test]
    #[ignore = "requires Windows Credential Manager in an interactive logon session"]
    fn password_round_trip_is_bound_to_server_and_login() {
        let original = Connection::new(
            "ftp://example.invalid/start/",
            "arco",
            Security::ExplicitTls,
        )
        .unwrap();
        let changed_dir = Connection::new(
            "ftp://example.invalid/other/",
            "arco",
            Security::ExplicitTls,
        )
        .unwrap();
        let changed_user =
            Connection::new("ftp://example.invalid/", "other", Security::ExplicitTls).unwrap();
        let changed_tls =
            Connection::new("ftp://example.invalid/", "arco", Security::Plain).unwrap();
        let id = original.store_password("test-only-ä").unwrap();
        let read = changed_dir.read_password(&id);
        let other_user = changed_user.read_password(&id);
        let other_tls = changed_tls.read_password(&id);
        original.delete_password(&id).unwrap();
        assert_eq!(read.unwrap().unwrap().as_str(), "test-only-ä");
        assert!(other_user.unwrap().is_none());
        assert!(other_tls.unwrap().is_none());
        assert!(original.read_password(&id).unwrap().is_none());
    }
}
