use super::{Connection, check_password};
use crate::credentials;
use std::io;
use windows::Win32::System::Com::CoCreateGuid;
use zeroize::Zeroizing;

impl Connection {
    pub fn store_password(&self, password: &str) -> io::Result<String> {
        check_password(password)?;
        if password.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Das Passwort kann in dieser Länge nicht gespeichert werden.",
            ));
        }
        let guid = unsafe { CoCreateGuid() }.map_err(io::Error::other)?;
        let id = format!("{:032x}", guid.to_u128());
        credentials::write(&self.credential_target(&id)?, password)?;
        Ok(id)
    }

    pub fn read_password(&self, id: &str) -> io::Result<Option<Zeroizing<String>>> {
        credentials::read(&self.credential_target(id)?)
    }

    pub fn delete_password(&self, id: &str) -> io::Result<()> {
        credentials::delete(&self.credential_target(id)?)
    }

    pub fn same_credential_scope(&self, other: &Self) -> bool {
        self.credential_scope() == other.credential_scope()
    }

    fn credential_target(&self, id: &str) -> io::Result<String> {
        if id.len() != 32 || !id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Ungültige FTP-Zugangsdatenreferenz.",
            ));
        }
        let hash = self.credential_scope();
        Ok(format!("Arco Commander FTP/{id}-{hash}"))
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
