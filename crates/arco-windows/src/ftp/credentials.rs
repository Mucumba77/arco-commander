use super::{Connection, check_password};
use crate::credentials;
use std::io;
use windows::Win32::System::Com::CoCreateGuid;
use zeroize::Zeroizing;

const S3_TOKEN_CHUNK: usize = 2048;
const S3_TOKEN_CHUNKS: usize = super::s3_credentials::MAX_SESSION_TOKEN.div_ceil(S3_TOKEN_CHUNK);

impl Connection {
    pub fn store_s3_session_token(&self, token: &str) -> io::Result<String> {
        self.check_s3_session_token(token)?;
        let guid = unsafe { CoCreateGuid() }.map_err(io::Error::other)?;
        let id = format!("{:032x}", guid.to_u128());
        let target = self.s3_token_target(&id)?;
        let result = (|| -> io::Result<()> {
            for (index, offset) in (0..token.len()).step_by(S3_TOKEN_CHUNK).enumerate() {
                let end = (offset + S3_TOKEN_CHUNK).min(token.len());
                credentials::write(&format!("{target}/{index}"), &token[offset..end])?;
            }
            credentials::write(&target, &token.len().div_ceil(S3_TOKEN_CHUNK).to_string())
        })();
        if let Err(error) = result {
            self.delete_s3_session_token(&id).map_err(|cleanup| {
                io::Error::other(format!("{error}. Der unvollständige Windows-Tokeneintrag konnte nicht bereinigt werden. {cleanup}"))
            })?;
            return Err(error);
        }
        Ok(id)
    }

    pub fn use_s3_session_token(self, id: &str) -> io::Result<Self> {
        let target = self.s3_token_target(id)?;
        let Some(index) = credentials::read(&target)? else {
            return Ok(self);
        };
        let count = index
            .parse::<usize>()
            .ok()
            .filter(|count| (1..=S3_TOKEN_CHUNKS).contains(count))
            .ok_or_else(invalid_s3_token_entry)?;
        let mut token = Zeroizing::new(String::new());
        for part in 0..count {
            let chunk = credentials::read(&format!("{target}/{part}"))?
                .ok_or_else(invalid_s3_token_entry)?;
            token.push_str(&chunk);
        }
        self.with_s3_session_token(token)
    }

    pub fn delete_s3_session_token(&self, id: &str) -> io::Result<()> {
        let target = self.s3_token_target(id)?;
        for index in 0..S3_TOKEN_CHUNKS {
            credentials::delete(&format!("{target}/{index}"))?;
        }
        credentials::delete(&target)
    }

    pub fn same_s3_token_scope(&self, other: &Self) -> bool {
        self.same_credential_scope(other) && self.url.scheme() == other.url.scheme()
    }

    fn s3_token_target(&self, id: &str) -> io::Result<String> {
        if self.security != super::Security::S3
            || id.len() != 32
            || !id.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(invalid_s3_token_entry());
        }
        let scope = format!("{}\n{}", self.url.scheme(), self.credential_scope());
        Ok(format!(
            "Arco Commander S3 Token/{id}-{}",
            blake3::hash(scope.as_bytes())
        ))
    }

    pub fn store_proxy_password(&self, password: &str) -> io::Result<String> {
        self.check_proxy_password(password)?;
        let guid = unsafe { CoCreateGuid() }.map_err(io::Error::other)?;
        let id = format!("{:032x}", guid.to_u128());
        credentials::write(&self.proxy_credential_target(&id)?, password)?;
        Ok(id)
    }

    pub fn use_proxy_password(self, id: &str) -> io::Result<Self> {
        let password = credentials::read(&self.proxy_credential_target(id)?)?.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "Speichere das Proxy-Passwort erneut in den Windows-Anmeldeinformationen.",
            )
        })?;
        self.with_proxy_password(&password)
    }

    pub fn delete_proxy_password(&self, id: &str) -> io::Result<()> {
        credentials::delete(&self.proxy_credential_target(id)?)
    }

    fn proxy_credential_target(&self, id: &str) -> io::Result<String> {
        let proxy = self
            .proxy
            .as_ref()
            .filter(|_| self.proxy_has_credential_identity())
            .ok_or_else(|| super::invalid_input("Die Proxy-Anmeldung ist unvollständig."))?;
        if id.len() != 32 || !id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(super::invalid_input(
                "Ungültiger Verweis auf das Proxy-Passwort.",
            ));
        }
        let mut scope = format!("{proxy}\n{}", self.proxy_user);
        if let Some(command) = &self.local_proxy {
            use std::fmt::Write as _;
            write!(scope, "\n{}", command.scope).expect("format local proxy scope");
        }
        let scope = blake3::hash(scope.as_bytes());
        Ok(format!("Arco Commander Proxy/{id}-{scope}"))
    }

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
        let mut scope = format!(
            "{:?}\n{}\n{:?}\n{}\n{:?}",
            self.security,
            self.url.host_str().unwrap_or_default(),
            self.url.port_or_known_default(),
            self.user,
            self.proxy,
        );
        // Preserve the scope of existing ordinary FTP credentials. OTP
        // passphrases require their own reference and must never become PASS.
        if self.options.otp {
            scope.push_str("\notp-rfc2289");
        }
        if let Some(ssh) = &self.ssh {
            scope.push('\n');
            for (index, digest) in ssh.fingerprints.encoded().enumerate() {
                if index > 0 {
                    scope.push(';');
                }
                scope.push_str(&digest);
            }
            if ssh.kerberos_kex {
                scope.push_str("\nkerberos-kex:");
                scope.push_str(ssh.kerberos_spn.as_deref().unwrap_or_default());
            }
            if ssh.uses_key() {
                scope.push_str("\nprivate-key-passphrase");
            }
        }
        blake3::hash(scope.as_bytes())
    }
}

fn invalid_s3_token_entry() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "Ungültiger Windows-Eintrag für das S3-Sitzungstoken.",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ftp::Security;

    #[test]
    #[ignore = "requires Windows Credential Manager in an interactive logon session"]
    fn s3_token_uses_credential_manager_and_is_bound_to_endpoint() {
        let original =
            Connection::new("https://s3-reference.invalid/bucket/", "ARCO", Security::S3).unwrap();
        let same =
            Connection::new("https://s3-reference.invalid/other/", "ARCO", Security::S3).unwrap();
        let other = Connection::new("https://s3-other.invalid/", "ARCO", Security::S3).unwrap();
        let token = Zeroizing::new("arco-test-token".repeat(310));
        let id = original.store_s3_session_token(&token).unwrap();
        let read = same.use_s3_session_token(&id);
        let different_scope = other.use_s3_session_token(&id);
        original.delete_s3_session_token(&id).unwrap();
        assert!(
            read.unwrap()
                .s3_session_token
                .as_ref()
                .is_some_and(|value| value.as_str() == token.as_str())
        );
        assert!(!different_scope.unwrap().has_s3_session_token());
        assert!(
            !original
                .use_s3_session_token(&id)
                .unwrap()
                .has_s3_session_token()
        );
    }

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
