//! Import executable paths and documented packer defaults without running tools.
use super::PackerProfile;
use crate::{
    archive_settings::{Settings, SevenZipMethod, TarDialect, ZipCipher, ZipMethod},
    profile,
};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    io,
    path::{Path, PathBuf},
};

#[derive(Default)]
pub struct TcPackers {
    pub profiles: BTreeMap<String, PackerProfile>,
    pub options: Vec<(String, String)>,
    pub skipped: usize,
}

impl TcPackers {
    pub fn apply_options(&self, settings: &mut Settings) {
        for (key, value) in &self.options {
            match key.as_str() {
                "internalziprate" => {
                    if let Ok(level @ 0..=9) = value.parse() {
                        settings.zip_level = level;
                    } else if value == "10" {
                        settings.zip_method = ZipMethod::Lzma;
                    }
                }
                "7zrate" => {
                    if let Ok(level @ 0..=9) = value.parse() {
                        settings.seven_zip_level = level;
                    }
                }
                "verifyzip" => settings.verify = value == "1",
                "zipsetdatetonewest" => settings.newest_timestamp = value == "1",
                "7zencryptnames" => settings.encrypt_headers = value == "1",
                "7zsolid" => settings.solid = value == "1",
                "linuxcompatible" => {
                    settings.tar_dialect = if value == "1" {
                        TarDialect::Gnu
                    } else {
                        TarDialect::Posix
                    }
                }
                "zipaes" => {
                    settings.zip_cipher = match value.as_str() {
                        "0" => ZipCipher::ZipCrypto,
                        "1" => ZipCipher::Aes128,
                        "2" => ZipCipher::Aes192,
                        _ => ZipCipher::Aes256,
                    }
                }
                "7zmethod" => {
                    settings.seven_zip_method = match value.to_ascii_uppercase().as_str() {
                        "LZMA" => SevenZipMethod::Lzma,
                        "PPMD" => SevenZipMethod::Ppmd,
                        "BZIP2" => SevenZipMethod::Bzip2,
                        _ => SevenZipMethod::Lzma2,
                    }
                }
                "7zthreads" => {
                    if let Ok(value) = value.parse() {
                        settings.threads = value;
                    }
                }
                _ => (),
            }
        }
        let method = match settings.seven_zip_method {
            SevenZipMethod::Lzma2 => "lzma2",
            SevenZipMethod::Lzma => "lzma",
            SevenZipMethod::Ppmd => "ppmd",
            SevenZipMethod::Bzip2 => "bzip2",
        };
        let suffix = format!("{method}_{}", settings.seven_zip_level);
        for (key, value) in &self.options {
            let mut candidate = *settings;
            if key == &format!("7zdictsize{suffix}") {
                if let Ok(size) = value.parse::<i32>() {
                    candidate.dictionary_kib = if size < 0 {
                        size.unsigned_abs()
                    } else {
                        (size as u32).saturating_mul(1024)
                    };
                }
            } else if key == &format!("7zwordsize{suffix}") {
                if let Ok(size) = value.parse() {
                    candidate.word_size = size;
                }
            } else if key == &format!("7zsolidsize{suffix}") {
                if let Ok(size) = value.parse() {
                    candidate.solid_block_mib = size;
                }
            }
            if candidate.validate().is_ok() {
                *settings = candidate;
            }
        }
    }
}

pub fn read_tc_packers(
    path: &Path,
    resolve: impl Fn(&str) -> Option<OsString>,
) -> io::Result<TcPackers> {
    let text = profile::read_tc_text(path)?;
    let mut result = TcPackers::default();
    for (section, key, value) in profile::ini_entries(&text) {
        if section != "packer" {
            continue;
        }
        let (extension, switches): (&str, &[&str]) = match key.as_str() {
            "arj" => ("arj", &["a", "-r", "-y"]),
            "lha" => ("lzh", &["a", "-r", "-x"]),
            "rar" => ("rar", &["a", "-r", "-y"]),
            "uc2" => ("uc2", &["a"]),
            "ace" => ("ace", &["a", "-r", "-y"]),
            "7z" | "7zip" => ("7z", &["a", "-t7z", "-spd", "-y"]),
            "zip" => ("zip", &[]),
            "internalziprate" | "7zrate" => {
                match value.parse::<u8>() {
                    Ok(level) if level <= 9 || level == 10 && key == "internalziprate" => {
                        result.options.push((key, value.into()))
                    }
                    _ => result.skipped += 1,
                }
                continue;
            }
            "verifyzip" | "zipsetdatetonewest" | "7zencryptnames" | "7zsolid"
            | "linuxcompatible" => {
                if value == "0" || value == "1" {
                    result.options.push((key, value.into()));
                } else {
                    result.skipped += 1;
                }
                continue;
            }
            "zipaes" => {
                if ["0", "1", "2", "3"].contains(&value) {
                    result.options.push((key, value.into()));
                } else {
                    result.skipped += 1;
                }
                continue;
            }
            "7zthreads" => {
                if value.parse::<u16>().is_ok_and(|value| value <= 64) {
                    result.options.push((key, value.into()));
                } else {
                    result.skipped += 1;
                }
                continue;
            }
            "7zmethod" => {
                if ["LZMA2", "LZMA", "PPMD", "BZIP2"].contains(&value.to_ascii_uppercase().as_str())
                {
                    result.options.push((key, value.into()));
                } else {
                    result.skipped += 1;
                }
                continue;
            }
            _ if regex::Regex::new(
                r"^7z(dictsize|wordsize|solidsize)(lzma2|lzma|ppmd|bzip2)_[0-9]$",
            )
            .expect("fixed option pattern")
            .is_match(&key) =>
            {
                if value
                    .parse::<i32>()
                    .is_ok_and(|number| (-131_072..=65_536).contains(&number))
                {
                    result.options.push((key, value.into()));
                } else {
                    result.skipped += 1;
                }
                continue;
            }
            _ => continue,
        };
        if value.trim().is_empty() {
            continue;
        }
        let Some(executable) = profile::expand_tc_path(value, &resolve).map(PathBuf::from) else {
            result.skipped += 1;
            continue;
        };
        let mut switches = switches.to_vec();
        if extension == "zip" {
            let seven = executable.file_stem().is_some_and(|name| {
                name.eq_ignore_ascii_case("7z") || name.eq_ignore_ascii_case("7zg")
            });
            switches = if seven {
                vec!["a", "-tzip", "-spd", "-y"]
            } else {
                vec!["-r", "-q"]
            };
        }
        let packer = PackerProfile {
            executable,
            extension: extension.into(),
            arguments: switches
                .into_iter()
                .chain(["{archive}", "{sources}"])
                .map(str::to_owned)
                .collect(),
        };
        if packer.validate().is_err() {
            result.skipped += 1;
            continue;
        }
        result
            .profiles
            .insert(format!("TC {}", key.to_uppercase()), packer);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn imports_active_tc_method_preset_and_converts_dictionary_units() {
        let folder = tempfile::tempdir().unwrap();
        let ini = folder.path().join("wincmd.ini");
        std::fs::write(&ini, "[Packer]\n7zdictSizeLZMA2_6=-512\n7zwordSizeLZMA2_6=32\n7zSolidSizeLZMA2_6=64\n7zThreads=2\n7zRate=6\n7zMethod=LZMA2\n").unwrap();
        let imported = read_tc_packers(&ini, |_| None).unwrap();
        let mut settings = Settings::default();
        imported.apply_options(&mut settings);
        assert_eq!(
            (
                settings.dictionary_kib,
                settings.word_size,
                settings.solid_block_mib,
                settings.threads
            ),
            (512, 32, 64, 2)
        );
        settings.validate().unwrap();
    }
    #[test]
    fn imports_paths_options_and_literal_arguments_without_executing_tools() {
        let folder = tempfile::tempdir().unwrap();
        let ini = folder.path().join("wincmd.ini");
        std::fs::write(&ini, "[Packer]\nARJ=%COMMANDER_PATH%/arj32.exe\nZIP=\"%COMMANDER_PATH%/7z.exe\"\nACE=relative.exe\nRAR=%MISSING%/rar.exe\nInternalZipRate=9\n7zRate=42\nVerifyZip=0\nZipSetDateToNewest=1\n7ZEncryptNames=0\n").unwrap();
        let imported = read_tc_packers(&ini, |key| {
            (key == "COMMANDER_PATH").then(|| folder.path().as_os_str().into())
        })
        .unwrap();
        assert_eq!(imported.profiles.len(), 2);
        assert_eq!(imported.skipped, 3);
        assert!(
            imported.profiles["TC ZIP"]
                .arguments
                .contains(&"-tzip".into())
        );
        let mut settings = Settings::default();
        imported.apply_options(&mut settings);
        assert_eq!(settings.zip_level, 9);
        assert_eq!(settings.seven_zip_level, 3);
        assert!(!settings.verify);
        assert!(settings.newest_timestamp);
        assert!(!settings.encrypt_headers);
    }
}
