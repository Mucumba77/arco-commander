use crate::{archives::CrcWriter, files, tools};
use std::{
    fs::File,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};
use tempfile::NamedTempFile;

const MAX_PARTS: usize = 9_999;

pub struct Manifest {
    path: PathBuf,
    pub name: String,
    size: u64,
    crc: u32,
}

pub fn split(
    source: &Path,
    target: &Path,
    part_size: u64,
    cancel: &AtomicBool,
) -> io::Result<usize> {
    let meta = files::regular_metadata(source)?;
    if !meta.is_file() || part_size == 0 {
        return Err(io::Error::other(
            "Eine reguläre Datei und eine positive Teilgröße sind erforderlich.",
        ));
    }
    let name = source
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            io::Error::other("Der Dateiname kann nicht im CRC-Format gespeichert werden.")
        })?;
    files::leaf_name(name.as_ref())?;
    let count = meta.len().div_ceil(part_size).max(1);
    if count > MAX_PARTS as u64 {
        return Err(io::Error::other(
            "Der Satz würde mehr als 9999 Teile benötigen.",
        ));
    }
    let mut input = File::open(source)?;
    let working = tempfile::tempdir_in(target)?;
    let manifest = working.path().join(name).with_extension("crc");
    let mut parts = Vec::with_capacity(count as usize + 1);
    let mut remaining = meta.len();
    let mut crc = crc32fast::Hasher::new();
    for index in 1..=count {
        files::check_cancel(cancel)?;
        let path = manifest.with_extension(format!("{index:03}"));
        let mut output = CrcWriter {
            output: File::create(&path)?,
            crc,
        };
        let size = remaining.min(part_size);
        if tools::copy_cancelled((&mut input).take(size), &mut output, cancel)? != size {
            return Err(io::Error::other("Die Quelldatei wurde abgeschnitten."));
        }
        output.output.sync_all()?;
        crc = output.crc;
        remaining -= size;
        parts.push((
            path.clone(),
            target.join(path.file_name().expect("part name")),
        ));
    }
    files::ensure_unchanged(source, &meta)?;
    let mut output = File::create(&manifest)?;
    let (legacy, _, _) = encoding_rs::WINDOWS_1252.encode(name);
    output.write_all(b"filename=")?;
    output.write_all(&legacy)?;
    output.write_all(b"\r\n")?;
    if !name.is_ascii() {
        write!(output, "filenameutf8={name}\r\n")?;
    }
    write!(
        output,
        "size={}\r\ncrc32={:08X}\r\n",
        meta.len(),
        crc.finalize()
    )?;
    output.sync_all()?;
    drop(output);
    // The .crc is the entry point for consumers and must be published last.
    parts.insert(
        0,
        (
            manifest.clone(),
            target.join(manifest.file_name().expect("manifest name")),
        ),
    );
    tools::publish_parts(parts, cancel)?;
    Ok(count as usize)
}

pub fn selected_manifest(paths: &[PathBuf]) -> io::Result<Option<Manifest>> {
    let Some(first) = paths.first() else {
        return Ok(None);
    };
    let candidate = first.with_extension("crc");
    let belongs = |path: &Path| {
        path.parent() == first.parent()
            && path
                .file_stem()
                .zip(first.file_stem())
                .is_some_and(|(left, right)| left.eq_ignore_ascii_case(right))
            && path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| {
                    ext.eq_ignore_ascii_case("crc")
                        || (ext.len() >= 3 && ext.bytes().all(|byte| byte.is_ascii_digit()))
                })
    };
    if paths.iter().all(|path| belongs(path)) && candidate.try_exists()? {
        return Manifest::read(candidate).map(Some);
    }
    Ok(None)
}

impl Manifest {
    fn read(path: PathBuf) -> io::Result<Self> {
        let invalid = || io::Error::new(io::ErrorKind::InvalidData, "Ungültige CRC-Begleitdatei.");
        let meta = files::regular_metadata(&path)?;
        if !meta.is_file() || meta.len() > 8192 {
            return Err(invalid());
        }
        let mut bytes = Vec::new();
        File::open(&path)?.take(8193).read_to_end(&mut bytes)?;
        files::ensure_unchanged(&path, &meta)?;
        let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&bytes);
        let (mut legacy, mut utf8, mut size, mut crc) = (None, None, None, None);
        for line in bytes.split(|&byte| byte == b'\n') {
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            let Some(index) = line.iter().position(|&byte| byte == b'=') else {
                continue;
            };
            let (key, rest) = (&line[..index], &line[index + 1..]);
            if key.eq_ignore_ascii_case(b"filename") {
                let name = std::str::from_utf8(rest)
                    .map(str::to_owned)
                    .unwrap_or_else(|_| encoding_rs::WINDOWS_1252.decode(rest).0.into_owned());
                if legacy.replace(name).is_some() {
                    return Err(invalid());
                }
            } else if key.eq_ignore_ascii_case(b"filenameutf8") {
                let name = std::str::from_utf8(rest).map_err(|_| invalid())?.to_owned();
                if utf8.replace(name).is_some() {
                    return Err(invalid());
                }
            } else if key.eq_ignore_ascii_case(b"size") {
                let value = std::str::from_utf8(rest)
                    .map_err(|_| invalid())?
                    .trim()
                    .parse()
                    .map_err(|_| invalid())?;
                if size.replace(value).is_some() {
                    return Err(invalid());
                }
            } else if key.eq_ignore_ascii_case(b"crc32") {
                let value = std::str::from_utf8(rest).map_err(|_| invalid())?.trim();
                if value.len() != 8 {
                    return Err(invalid());
                }
                let value = u32::from_str_radix(value, 16).map_err(|_| invalid())?;
                if crc.replace(value).is_some() {
                    return Err(invalid());
                }
            }
        }
        let name = utf8.or(legacy).ok_or_else(invalid)?;
        files::leaf_name(name.as_ref())?;
        Ok(Self {
            path,
            name,
            size: size.ok_or_else(invalid)?,
            crc: crc.ok_or_else(invalid)?,
        })
    }

    pub fn join(&self, target: &Path, cancel: &AtomicBool) -> io::Result<()> {
        let parent = target
            .parent()
            .ok_or_else(|| io::Error::other("Zielordner fehlt."))?;
        let mut output = CrcWriter {
            output: NamedTempFile::new_in(parent)?,
            crc: crc32fast::Hasher::new(),
        };
        let mut total = 0;
        let start = usize::from(!self.path.with_extension("000").try_exists()?);
        let mut count = 0;
        for index in start..=MAX_PARTS {
            files::check_cancel(cancel)?;
            let path = self.path.with_extension(format!("{index:03}"));
            let meta = match files::regular_metadata(&path) {
                Ok(meta) if meta.is_file() => meta,
                Ok(_) => return Err(io::Error::other("Ein Dateiteil ist keine reguläre Datei.")),
                Err(error) if error.kind() == io::ErrorKind::NotFound => break,
                Err(error) => return Err(error),
            };
            if meta.len() > self.size - total {
                return Err(io::Error::other(
                    "Die Dateiteile überschreiten die erwartete Gesamtgröße.",
                ));
            }
            let size =
                tools::copy_cancelled(File::open(&path)?.take(meta.len()), &mut output, cancel)?;
            files::ensure_unchanged(&path, &meta)?;
            if size != meta.len() {
                return Err(io::Error::other("Ein Dateiteil wurde abgeschnitten."));
            }
            total += size;
            count += 1;
        }
        if count == 0 || total != self.size || output.crc.finalize() != self.crc {
            return Err(io::Error::other(
                "Dateiteile fehlen oder die CRC-Prüfsumme stimmt nicht.",
            ));
        }
        output.output.as_file().sync_all()?;
        files::check_cancel(cancel)?;
        output
            .output
            .persist_noclobber(target)
            .map_err(|error| error.error)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn writes_commander_manifest_and_checks_size_and_crc_before_publishing() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path();
        let input = root.join("Größe.bin");
        let parts = root.join("parts");
        fs::create_dir(&parts).unwrap();
        fs::write(&input, b"123456789").unwrap();
        let cancel = AtomicBool::new(false);
        assert_eq!(split(&input, &parts, 4, &cancel).unwrap(), 3);
        let first = parts.join("Größe.001");
        assert_eq!(fs::read(&first).unwrap(), b"1234");
        let file = parts.join("Größe.crc");
        let contents = fs::read(&file).unwrap();
        assert!(contents.ends_with(b"size=9\r\ncrc32=CBF43926\r\n"));
        let manifest = selected_manifest(&[first.clone()]).unwrap().unwrap();
        assert_eq!(manifest.name, "Größe.bin");
        let target = root.join("result.bin");
        manifest.join(&target, &cancel).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"123456789");
        fs::write(&first, b"123X").unwrap();
        let failed = root.join("failed.bin");
        assert!(manifest.join(&failed, &cancel).is_err());
        assert!(!failed.exists());
        fs::remove_file(parts.join("Größe.002")).unwrap();
        assert!(manifest.join(&failed, &cancel).is_err());
    }
}
