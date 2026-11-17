pub use crate::archive_view::ZonePolicy;
use serde::{Deserialize, Serialize};
use std::io;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub zone: ZonePolicy,
    pub zip_level: u8,
    pub zip_method: ZipMethod,
    pub zip_cipher: ZipCipher,
    pub seven_zip_level: u8,
    pub seven_zip_method: SevenZipMethod,
    pub threads: u16,
    pub dictionary_kib: u32,
    pub word_size: u16,
    pub solid_block_mib: u32,
    pub solid: bool,
    pub gzip_level: u8,
    pub tar_dialect: TarDialect,
    pub encrypt_headers: bool,
    pub newest_timestamp: bool,
    pub verify: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ZipMethod {
    Store,
    #[default]
    Deflate,
    Bzip2,
    Lzma,
    Ppmd,
    Xz,
    Zstd,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ZipCipher {
    #[default]
    Aes256,
    Aes192,
    Aes128,
    ZipCrypto,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SevenZipMethod {
    #[default]
    Lzma2,
    Lzma,
    Ppmd,
    Bzip2,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TarDialect {
    #[default]
    Gnu,
    Posix,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            zone: ZonePolicy::Preserve,
            zip_level: 6,
            zip_method: ZipMethod::Deflate,
            zip_cipher: ZipCipher::Aes256,
            seven_zip_level: 3,
            seven_zip_method: SevenZipMethod::Lzma2,
            threads: 0,
            dictionary_kib: 0,
            word_size: 0,
            solid_block_mib: 0,
            solid: false,
            gzip_level: 6,
            tar_dialect: TarDialect::Gnu,
            encrypt_headers: true,
            newest_timestamp: false,
            verify: true,
        }
    }
}

impl Settings {
    pub fn validate(self) -> io::Result<()> {
        if self.zip_level > 9 || self.seven_zip_level > 9 || self.gzip_level > 9 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Die Kompressionsraten müssen zwischen 0 und 9 liegen.",
            ));
        }
        if self.threads > 64 || self.dictionary_kib > 128 * 1024 || self.solid_block_mib > 65_536 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "7z: höchstens 64 Threads, 128 MiB Wörterbuch und 65536 MiB pro Solid-Block.",
            ));
        }
        let valid = match self.seven_zip_method {
            SevenZipMethod::Lzma | SevenZipMethod::Lzma2 => {
                (self.dictionary_kib == 0 || self.dictionary_kib >= 4)
                    && (self.word_size == 0 || (5..=273).contains(&self.word_size))
            }
            SevenZipMethod::Ppmd => {
                self.dictionary_kib <= 64 * 1024
                    && (self.word_size == 0 || (2..=32).contains(&self.word_size))
            }
            SevenZipMethod::Bzip2 => {
                (self.dictionary_kib == 0 || (100..=900).contains(&self.dictionary_kib))
                    && self.word_size == 0
            }
        };
        if !valid {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Wörterbuch- oder Wortgröße passt nicht zur gewählten 7z-Methode.",
            ));
        }
        Ok(())
    }

    pub(crate) fn finish(
        self,
        target: &std::path::Path,
        newest: Option<std::time::SystemTime>,
    ) -> io::Result<()> {
        if self.newest_timestamp {
            if let Some(time) = newest {
                filetime::set_file_mtime(target, filetime::FileTime::from_system_time(time))?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn old_profiles_receive_defaults_and_archive_settings_roundtrip() {
        let mut document = serde_json::to_value(crate::profile::Profile::default()).unwrap();
        document.as_object_mut().unwrap().remove("archives");
        let old: crate::profile::Profile = serde_json::from_value(document).unwrap();
        assert_eq!(old.archives, Settings::default());
        let configured = Settings {
            zone: ZonePolicy::Omit,
            zip_level: 9,
            zip_method: ZipMethod::Xz,
            zip_cipher: ZipCipher::Aes128,
            seven_zip_level: 1,
            seven_zip_method: SevenZipMethod::Ppmd,
            solid: true,
            gzip_level: 2,
            tar_dialect: TarDialect::Posix,
            encrypt_headers: false,
            newest_timestamp: true,
            verify: false,
            threads: 2,
            dictionary_kib: 1024,
            word_size: 8,
            solid_block_mib: 64,
        };
        configured.validate().unwrap();
        let mut profile = old;
        profile.archives = configured;
        let json = serde_json::to_vec(&profile).unwrap();
        assert_eq!(
            serde_json::from_slice::<crate::profile::Profile>(&json)
                .unwrap()
                .archives,
            configured
        );
        assert!(
            Settings {
                gzip_level: 10,
                ..configured
            }
            .validate()
            .is_err()
        );
    }
}
