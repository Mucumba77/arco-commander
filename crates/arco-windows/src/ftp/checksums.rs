use super::{DownloadProgress, Entry, Security, Session, Url, sync::SyncChecksum};
use std::io;
use std::sync::atomic::AtomicBool;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChecksumAlgorithm {
    Sha512,
    Sha384,
    Sha256,
    Sha224,
    Sha1,
    Md5,
    Crc32,
}

impl ChecksumAlgorithm {
    pub const ALL: [Self; 7] = [
        Self::Sha512,
        Self::Sha384,
        Self::Sha256,
        Self::Sha224,
        Self::Sha1,
        Self::Md5,
        Self::Crc32,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Self::Sha512 => "SHA-512",
            Self::Sha384 => "SHA-384",
            Self::Sha256 => "SHA-256",
            Self::Sha224 => "SHA-224",
            Self::Sha1 => "SHA-1",
            Self::Md5 => "MD5",
            Self::Crc32 => "CRC32",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        let name = name.replace('-', "");
        Self::ALL.into_iter().find(|algorithm| {
            algorithm
                .name()
                .replace('-', "")
                .eq_ignore_ascii_case(&name)
        })
    }

    fn scp_program(self) -> Option<(&'static str, usize)> {
        Some(match self {
            Self::Sha512 => ("sha512sum", 128),
            Self::Sha384 => ("sha384sum", 96),
            Self::Sha256 => ("sha256sum", 64),
            Self::Sha224 => ("sha224sum", 56),
            Self::Sha1 => ("sha1sum", 40),
            Self::Md5 => ("md5sum", 32),
            Self::Crc32 => return None,
        })
    }
}

#[derive(Clone, Copy)]
pub struct RemoteChecksum(ChecksumMethod);

#[derive(Clone, Copy)]
enum ChecksumMethod {
    Sftp(ChecksumAlgorithm),
    Scp(ChecksumAlgorithm, &'static str, usize),
    Ftp(SyncChecksum),
}

impl Session {
    pub fn prepare_checksum(
        &self,
        directory: &Url,
        algorithm: Option<ChecksumAlgorithm>,
        cancel: &AtomicBool,
    ) -> io::Result<RemoteChecksum> {
        if self.is_sftp() {
            return Ok(RemoteChecksum(ChecksumMethod::Sftp(
                algorithm.unwrap_or(ChecksumAlgorithm::Sha256),
            )));
        }
        if self.security() == Security::Scp {
            let algorithm = algorithm.unwrap_or(ChecksumAlgorithm::Sha256);
            let (program, width) = algorithm.scp_program().ok_or_else(unsupported_checksum)?;
            return Ok(RemoteChecksum(ChecksumMethod::Scp(
                algorithm, program, width,
            )));
        }
        self.sync_checksum_method_for(directory, algorithm.map(ChecksumAlgorithm::name), cancel)?
            .map(|method| RemoteChecksum(ChecksumMethod::Ftp(method)))
            .ok_or_else(unsupported_checksum)
    }

    pub fn remote_checksum(
        &self,
        directory: &Url,
        entry: &Entry,
        method: RemoteChecksum,
        cancel: &AtomicBool,
        progress: impl FnMut(DownloadProgress),
    ) -> io::Result<(&'static str, String)> {
        match method.0 {
            ChecksumMethod::Sftp(algorithm) => self
                .sftp_checksum(directory, entry, algorithm, cancel, progress)
                .map(|digest| (algorithm.name(), digest)),
            ChecksumMethod::Scp(algorithm, program, width) => self
                .scp_checksum(directory, entry, program, width, cancel)
                .map(|digest| (algorithm.name(), digest)),
            ChecksumMethod::Ftp(method) => self
                .sync_checksum(directory, entry, method, cancel)?
                .ok_or_else(unsupported_checksum),
        }
    }
}

fn unsupported_checksum() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "Die gewählte Prüfsumme ist für diese Serververbindung nicht verfügbar.",
    )
}
