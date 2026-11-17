//! Bound, read-only volume discovery. Continuations stay beside their entry point.
use crate::files;
use std::{
    fs::{self, File, Metadata},
    io::{self, Read},
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};

fn prefix(path: &Path, limit: u64) -> io::Result<Vec<u8>> {
    files::regular_metadata(path)?;
    let mut bytes = Vec::new();
    File::open(path)?.take(limit).read_to_end(&mut bytes)?;
    Ok(bytes)
}
fn short(bytes: &[u8]) -> u16 {
    u16::from_le_bytes(bytes.try_into().expect("two bytes"))
}
fn vint(bytes: &[u8], at: &mut usize) -> Option<u64> {
    let mut value = 0;
    for shift in (0..70).step_by(7) {
        let byte = *bytes.get(*at)?;
        *at += 1;
        if shift == 63 && byte > 1 {
            return None;
        }
        value |= u64::from(byte & 127) << shift;
        if byte & 128 == 0 {
            return Some(value);
        }
    }
    None
}

pub(crate) fn rar_headers_encrypted(path: &Path) -> bool {
    let bytes = match prefix(
        path,
        if path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("exe"))
        {
            2 * 1024 * 1024
        } else {
            4096
        },
    ) {
        Ok(bytes) => bytes,
        Err(_) => return false,
    };
    for (offset, magic) in bytes.windows(7).enumerate() {
        if magic != b"Rar!\x1a\x07\x00" && magic != b"Rar!\x1a\x07\x01" {
            continue;
        }
        if magic[6] == 0 {
            return bytes.get(offset + 9) == Some(&0x73)
                && bytes
                    .get(offset + 10)
                    .is_some_and(|flags| flags & 0x80 != 0);
        }
        let mut at = offset + 8 + 4;
        return vint(&bytes, &mut at).is_some() && vint(&bytes, &mut at) == Some(4);
    }
    false
}

pub(crate) fn is_sfx(path: &Path) -> bool {
    if !path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("exe"))
    {
        return false;
    }
    let Ok(bytes) = prefix(path, 2 * 1024 * 1024) else {
        return false;
    };
    if !bytes.starts_with(b"MZ") {
        return false;
    }
    bytes
        .windows(7)
        .any(|bytes| bytes == b"Rar!\x1a\x07\x00" || bytes == b"Rar!\x1a\x07\x01")
        || bytes.windows(4).enumerate().any(|(at, bytes4)| {
            if bytes4[..2] != [0x60, 0xea] {
                return false;
            }
            let length = usize::from(short(&bytes4[2..]));
            (30..=2600).contains(&length)
                && bytes.get(at + 4..at + 8 + length).is_some_and(|body| {
                    crc32fast::hash(&body[..length])
                        == u32::from_le_bytes(body[length..length + 4].try_into().unwrap())
                })
        })
}

fn arj_next(path: &Path) -> io::Result<bool> {
    let bytes = prefix(path, 2 * 1024 * 1024 + 2612)?;
    let offset = arj_offset(&bytes).ok_or_else(|| io::Error::other("ARJ-Hauptheader fehlt."))?;
    let bytes = &bytes[offset..];
    let length = usize::from(short(&bytes[2..4]));
    if length < 30
        || length > 2600
        || bytes.len() < 8 + length
        || crc32fast::hash(&bytes[4..4 + length])
            != u32::from_le_bytes(bytes[4 + length..8 + length].try_into().unwrap())
    {
        return Err(io::Error::other("Ungültiger ARJ-Hauptheader."));
    }
    Ok(bytes[8] & 4 != 0)
}

fn arj_offset(bytes: &[u8]) -> Option<usize> {
    let candidates = if bytes.starts_with(b"MZ") {
        bytes.len().min(2 * 1024 * 1024)
    } else {
        1
    };
    (0..candidates).find(|&at| {
        let Some(head) = bytes.get(at..at + 4) else {
            return false;
        };
        if head[..2] != [0x60, 0xea] {
            return false;
        }
        let length = usize::from(short(&head[2..]));
        (30..=2600).contains(&length)
            && bytes.get(at + 4..at + 8 + length).is_some_and(|body| {
                body[0] >= 30
                    && body[6] == 2
                    && crc32fast::hash(&body[..length])
                        == u32::from_le_bytes(body[length..length + 4].try_into().unwrap())
            })
    })
}

fn cab_next(path: &Path) -> io::Result<(u16, u16, Option<String>)> {
    let bytes = prefix(path, 65_536)?;
    if bytes.len() < 36 || &bytes[..4] != b"MSCF" {
        return Err(io::Error::other("CAB-Hauptheader fehlt."));
    }
    let flags = short(&bytes[30..32]);
    let mut at = 36;
    if flags & 4 != 0 {
        let reserve = bytes
            .get(at..at + 4)
            .ok_or_else(|| io::Error::other("CAB-Reserveheader fehlt."))?;
        at += 4 + usize::from(short(&reserve[..2]));
    }
    let mut text = || -> io::Result<String> {
        let value = bytes
            .get(at..)
            .and_then(|tail| {
                tail.iter()
                    .position(|&byte| byte == 0)
                    .map(|length| &tail[..length])
            })
            .filter(|value| value.len() <= 255)
            .ok_or_else(|| io::Error::other("Ungültiger CAB-Folgename."))?;
        at += value.len() + 1;
        Ok(encoding_rs::WINDOWS_1252.decode(value).0.into_owned())
    };
    if flags & 1 != 0 {
        text()?;
        text()?;
    }
    let next = if flags & 2 != 0 {
        let next = text()?;
        text()?;
        files::leaf_name(std::ffi::OsStr::new(&next))?;
        Some(next)
    } else {
        None
    };
    Ok((short(&bytes[32..34]), short(&bytes[34..36]), next))
}

pub(crate) fn snapshot(path: &Path, cancel: &AtomicBool) -> io::Result<Vec<(PathBuf, Metadata)>> {
    let extension = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default();
    if let Some(count) = crate::archives::spanned_disks(path) {
        if count > 10_000 {
            return Err(io::Error::other("Zu viele ZIP-Datenträger."));
        }
        let mut parts = vec![(path.to_path_buf(), files::regular_metadata(path)?)];
        for index in 1..count {
            files::check_cancel(cancel)?;
            let part = path.with_extension(format!("z{index:02}"));
            parts.push((part.clone(), files::regular_metadata(&part)?));
        }
        return Ok(parts);
    }
    if extension.eq_ignore_ascii_case("cab") {
        let (set, index, next) = cab_next(path)?;
        if index != 0 {
            return Err(io::Error::other("Öffne den ersten CAB-Teil des Satzes."));
        }
        if next.is_none() {
            return Ok(Vec::new());
        }
        let mut current = path.to_path_buf();
        let mut result = Vec::new();
        loop {
            files::check_cancel(cancel)?;
            let (id, index, next) = cab_next(&current)?;
            if id != set || usize::from(index) != result.len() || result.len() >= 10_000 {
                return Err(io::Error::other("CAB-Teile gehören nicht zum selben Satz."));
            }
            result.push((current.clone(), files::regular_metadata(&current)?));
            let Some(next) = next else {
                return Ok(result);
            };
            current = path
                .parent()
                .ok_or_else(|| io::Error::other("CAB-Ordner fehlt."))?
                .join(next);
        }
    }
    if extension.eq_ignore_ascii_case("arj")
        || extension.eq_ignore_ascii_case("exe")
            && arj_offset(&prefix(path, 2 * 1024 * 1024 + 2612)?).is_some()
    {
        if !arj_next(path)? {
            return Ok(Vec::new());
        }
        let mut result = vec![(path.to_path_buf(), files::regular_metadata(path)?)];
        for index in 1..=9999 {
            files::check_cancel(cancel)?;
            let next = path.with_extension(format!("a{index:02}"));
            let more = arj_next(&next)?;
            result.push((next.clone(), files::regular_metadata(&next)?));
            if !more {
                return Ok(result);
            }
        }
        return Err(io::Error::other("Zu viele ARJ-Teile."));
    }
    if !extension.eq_ignore_ascii_case("rar") && !extension.eq_ignore_ascii_case("exe") {
        return Ok(Vec::new());
    }
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    let pattern =
        regex::Regex::new(r"(?i)^(.+\.part)([0-9]+)\.(rar|exe)$").expect("fixed volume pattern");
    if let Some(parts) = pattern.captures(name) {
        if parts[2].parse::<usize>().ok() != Some(1) {
            return Err(io::Error::other("Öffne den ersten RAR-Teil (.part1.rar)."));
        }
        let parent = path
            .parent()
            .ok_or_else(|| io::Error::other("RAR-Ordner fehlt."))?;
        let width = parts[2].len();
        let mut result = Vec::new();
        for index in 1..=9999 {
            files::check_cancel(cancel)?;
            let part = if index == 1 {
                path.to_path_buf()
            } else {
                parent.join(format!("{}{:0width$}.rar", &parts[1], index))
            };
            match fs::symlink_metadata(&part) {
                Ok(_) => result.push((part.clone(), files::regular_metadata(&part)?)),
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(result),
                Err(error) => return Err(error),
            }
        }
        return Err(io::Error::other("Zu viele RAR-Teile."));
    }
    let bytes = prefix(
        path,
        if extension.eq_ignore_ascii_case("exe") {
            2 * 1024 * 1024
        } else {
            4096
        },
    )?;
    let Some(offset) = bytes
        .windows(7)
        .position(|magic| magic == b"Rar!\x1a\x07\x00")
    else {
        return Ok(Vec::new());
    };
    let bytes = &bytes[offset..];
    if bytes.len() < 14 || bytes[9] != 0x73 || bytes[10] & 1 == 0 {
        return Ok(Vec::new());
    }
    let mut result = vec![(path.to_path_buf(), files::regular_metadata(path)?)];
    for letter in b'r'..=b'z' {
        for number in 0..100 {
            files::check_cancel(cancel)?;
            let part = path.with_extension(format!("{}{number:02}", char::from(letter)));
            match fs::symlink_metadata(&part) {
                Ok(_) => result.push((part.clone(), files::regular_metadata(&part)?)),
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(result),
                Err(error) => return Err(error),
            }
        }
    }
    Ok(result)
}
