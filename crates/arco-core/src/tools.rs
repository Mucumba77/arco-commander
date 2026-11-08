use crate::files::{self, Directory};
use encoding_rs::{Encoding, UTF_8, UTF_16BE, UTF_16LE, WINDOWS_1252};
use globset::GlobSet;
use regex::Regex;
use sha2::Digest;
use similar::{ChangeTag, TextDiff};
use std::collections::HashMap;
use std::ffi::OsStr;
use std::fmt::Write as _;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::AtomicBool;
use tempfile::NamedTempFile;
use walkdir::WalkDir;

const SEARCH_CONTENT_LIMIT: u64 = 64 * 1024 * 1024;
const DIFF_LIMIT: u64 = 4 * 1024 * 1024;

pub struct Search {
    pub names: GlobSet,
    pub text: Option<Regex>,
    pub min_size: Option<u64>,
    pub max_size: Option<u64>,
    pub max_depth: Option<usize>,
    pub show_hidden: bool,
}

pub fn search(roots: &[PathBuf], query: &Search, cancel: &AtomicBool) -> io::Result<Directory> {
    let mut directory = Directory {
        entries: Vec::new(),
        errors: Vec::new(),
    };
    for root in roots {
        let mut walk = WalkDir::new(root).min_depth(1).follow_links(false);
        if let Some(depth) = query.max_depth {
            walk = walk.max_depth(depth);
        }
        for item in walk.into_iter().filter_entry(|entry| {
            entry.depth() == 0 || entry.metadata().is_ok_and(|meta| !files::is_link(&meta))
        }) {
            files::check_cancel(cancel)?;
            let result = item.map_err(io::Error::other).and_then(|item| {
                let path = item.path();
                if !query.names.is_empty() && !query.names.is_match(item.file_name()) {
                    return Ok(None);
                }
                let entry =
                    files::Entry::read(path.to_path_buf(), item.file_name().to_os_string())?;
                if !query.show_hidden && entry.hidden {
                    return Ok(None);
                }
                if query
                    .min_size
                    .is_some_and(|size| entry.size.is_none_or(|actual| actual < size))
                    || query
                        .max_size
                        .is_some_and(|size| entry.size.is_none_or(|actual| actual > size))
                {
                    return Ok(None);
                }
                if let Some(pattern) = &query.text {
                    let Some(size) = entry.size else {
                        return Ok(None);
                    };
                    if size > SEARCH_CONTENT_LIMIT {
                        return Err(io::Error::other(format!(
                            "text search limit exceeded: {}",
                            path.display()
                        )));
                    }
                    let data = read_bounded(path, SEARCH_CONTENT_LIMIT)?;
                    if !pattern.is_match(&decode_text(&data)) {
                        return Ok(None);
                    }
                }
                Ok(Some(entry))
            });
            match result {
                Ok(Some(entry)) => directory.entries.push(entry),
                Ok(None) => (),
                Err(error) => directory.errors.push(error.to_string()),
            }
        }
    }
    Ok(directory)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreviewMode {
    Text,
    Hex,
}

pub struct Preview {
    pub text: String,
    pub length: u64,
    pub offset: u64,
    pub next: u64,
}

pub fn preview(path: &Path, offset: u64, mode: PreviewMode) -> io::Result<Preview> {
    let mut file = File::open(path)?;
    let length = file.metadata()?.len();
    let offset = offset.min(length);
    let mut encoding = UTF_8;
    let mut skip = 0;
    if mode == PreviewMode::Text {
        let mut sample = Vec::new();
        (&mut file).take(8192).read_to_end(&mut sample)?;
        if let Some((detected, bom)) = Encoding::for_bom(&sample) {
            encoding = detected;
            if offset == 0 {
                skip = bom;
            }
        } else if std::str::from_utf8(&sample).is_err_and(|error| error.error_len().is_some()) {
            encoding = WINDOWS_1252;
        }
    }
    file.seek(SeekFrom::Start(offset))?;
    let limit = if mode == PreviewMode::Hex {
        4096
    } else {
        32 * 1024
    };
    let mut data = Vec::with_capacity(limit);
    (&mut file).take(limit as u64).read_to_end(&mut data)?;
    let text = if mode == PreviewMode::Hex {
        let mut text = String::new();
        for (row, bytes) in data.chunks(16).enumerate() {
            write!(text, "{:016x}  ", offset + (row * 16) as u64).unwrap();
            for index in 0..16 {
                if let Some(byte) = bytes.get(index) {
                    write!(text, "{byte:02x} ").unwrap();
                } else {
                    text.push_str("   ");
                }
            }
            text.push(' ');
            text.extend(bytes.iter().map(|&byte| {
                if byte.is_ascii_graphic() || byte == b' ' {
                    char::from(byte)
                } else {
                    '.'
                }
            }));
            text.push('\n');
        }
        text
    } else {
        complete_character(&mut file, &mut data, encoding, skip)?;
        encoding
            .decode_without_bom_handling(&data[skip.min(data.len())..])
            .0
            .into_owned()
    };
    Ok(Preview {
        text,
        length,
        offset,
        next: offset + data.len() as u64,
    })
}

fn complete_character(
    file: &mut File,
    data: &mut Vec<u8>,
    encoding: &'static Encoding,
    skip: usize,
) -> io::Result<()> {
    if encoding == UTF_8 {
        for _ in 0..3 {
            let Some(incomplete) = std::str::from_utf8(&data[skip.min(data.len())..]).err() else {
                break;
            };
            if incomplete.error_len().is_some() {
                break;
            }
            let mut byte = [0];
            if file.read(&mut byte)? == 0 {
                break;
            }
            data.push(byte[0]);
        }
    } else if (encoding == UTF_16LE || encoding == UTF_16BE) && data.len() >= skip + 2 {
        let end = [data[data.len() - 2], data[data.len() - 1]];
        let unit = if encoding == UTF_16LE {
            u16::from_le_bytes(end)
        } else {
            u16::from_be_bytes(end)
        };
        if (0xd800..=0xdbff).contains(&unit) {
            file.take(2).read_to_end(data)?;
        }
    }
    Ok(())
}

pub(crate) fn decode_text(bytes: &[u8]) -> String {
    if let Some((encoding, skip)) = encoding_rs::Encoding::for_bom(bytes) {
        return encoding
            .decode_without_bom_handling(&bytes[skip..])
            .0
            .into_owned();
    }
    match std::str::from_utf8(bytes) {
        Ok(text) => text.to_owned(),
        Err(_) => WINDOWS_1252
            .decode_without_bom_handling(bytes)
            .0
            .into_owned(),
    }
}

fn read_bounded(path: &Path, maximum: u64) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take(maximum + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > maximum {
        return Err(io::Error::other(format!(
            "file too large: {}",
            path.display()
        )));
    }
    Ok(bytes)
}

pub fn compare_text(left: &Path, right: &Path) -> io::Result<String> {
    let a = read_bounded(left, DIFF_LIMIT)?;
    let b = read_bounded(right, DIFF_LIMIT)?;
    if a == b {
        return Ok("Dateien sind inhaltsgleich.".into());
    }
    let left = decode_text(&a);
    let right = decode_text(&b);
    let diff = TextDiff::configure()
        .timeout(std::time::Duration::from_secs(2))
        .diff_lines(&left, &right);
    let mut output = String::new();
    for change in diff.iter_all_changes() {
        if output.len() > 1_000_000 {
            output.push_str("\n[Weitere Zeilen ausgeblendet]\n");
            break;
        }
        output.push(match change.tag() {
            ChangeTag::Delete => '-',
            ChangeTag::Insert => '+',
            ChangeTag::Equal => ' ',
        });
        output.push(' ');
        output.push_str(change.value());
        if change.missing_newline() {
            output.push('\n');
        }
    }
    Ok(output)
}

pub fn compare_binary(left: &Path, right: &Path, cancel: &AtomicBool) -> io::Result<String> {
    let mut left = BufReader::new(File::open(left)?);
    let mut right = BufReader::new(File::open(right)?);
    let mut a = vec![0; 128 * 1024];
    let mut b = vec![0; a.len()];
    let mut offset = 0u64;
    let mut changed = 0u64;
    let mut lines = String::new();
    loop {
        files::check_cancel(cancel)?;
        let a_len = read_block(&mut left, &mut a)?;
        let b_len = read_block(&mut right, &mut b)?;
        let length = a_len.max(b_len);
        if length == 0 {
            break;
        }
        let a = &a[..a_len];
        let b = &b[..b_len];
        if a != b {
            for index in 0..length {
                let left = a.get(index);
                let right = b.get(index);
                if left == right {
                    continue;
                }
                if changed < 512 {
                    let label =
                        |byte: Option<&u8>| byte.map_or("--".into(), |byte| format!("{byte:02X}"));
                    writeln!(
                        lines,
                        "{:016X}  {}  {}",
                        offset + index as u64,
                        label(left),
                        label(right)
                    )
                    .unwrap();
                }
                changed += 1;
            }
        }
        offset += length as u64;
    }
    if changed == 0 {
        return Ok("Dateien sind bytegleich.".into());
    }
    Ok(format!(
        "{changed} unterschiedliche Bytepositionen\n\nOffset (hex)      Links  Rechts\n{lines}"
    ))
}

pub fn contents_match(left: &Path, right: &Path, cancelled: impl Fn() -> bool) -> io::Result<bool> {
    let mut left = File::open(left)?;
    let mut right = File::open(right)?;
    if left.metadata()?.len() != right.metadata()?.len() {
        return Ok(false);
    }
    let mut a = vec![0; 128 * 1024];
    let mut b = vec![0; a.len()];
    loop {
        if cancelled() {
            return Err(io::ErrorKind::Interrupted.into());
        }
        let a_len = read_block(&mut left, &mut a)?;
        let b_len = read_block(&mut right, &mut b)?;
        if a_len != b_len || a[..a_len] != b[..b_len] {
            return Ok(false);
        }
        if a_len == 0 {
            return Ok(true);
        }
    }
}

fn read_block(reader: &mut impl Read, buffer: &mut [u8]) -> io::Result<usize> {
    let mut total = 0;
    while total < buffer.len() {
        match reader.read(&mut buffer[total..]) {
            Ok(0) => break,
            Ok(count) => total += count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    Ok(total)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Hash {
    Crc32,
    Md5,
    Sha1,
    Sha224,
    Sha256,
    Sha384,
    Sha512,
    Sha3_224,
    Sha3_256,
    Sha3_384,
    Sha3_512,
    Blake3,
}

impl Hash {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().replace('-', "").as_str() {
            "crc32" | "sfv" => Some(Self::Crc32),
            "md5" => Some(Self::Md5),
            "sha1" => Some(Self::Sha1),
            "sha224" => Some(Self::Sha224),
            "sha256" => Some(Self::Sha256),
            "sha384" => Some(Self::Sha384),
            "sha512" => Some(Self::Sha512),
            "sha3_224" => Some(Self::Sha3_224),
            "sha3_256" => Some(Self::Sha3_256),
            "sha3_384" => Some(Self::Sha3_384),
            "sha3_512" => Some(Self::Sha3_512),
            "blake3" | "bk3" => Some(Self::Blake3),
            _ => None,
        }
    }
}

pub fn checksum(path: &Path, method: Hash, cancel: &AtomicBool) -> io::Result<String> {
    checksum_file(File::open(path)?, method, cancel)
}

pub fn checksum_file(input: File, method: Hash, cancel: &AtomicBool) -> io::Result<String> {
    fn digest<D: Digest>(input: File, cancel: &AtomicBool) -> io::Result<String>
    where
        sha2::digest::Output<D>: std::fmt::LowerHex,
    {
        let mut hash = D::new();
        read_chunks(input, cancel, |chunk| hash.update(chunk))?;
        Ok(format!("{:x}", hash.finalize()))
    }
    match method {
        Hash::Crc32 => {
            let mut hash = crc32fast::Hasher::new();
            read_chunks(input, cancel, |chunk| hash.update(chunk))?;
            Ok(format!("{:08x}", hash.finalize()))
        }
        Hash::Md5 => digest::<md5::Md5>(input, cancel),
        Hash::Sha1 => digest::<sha1::Sha1>(input, cancel),
        Hash::Sha224 => digest::<sha2::Sha224>(input, cancel),
        Hash::Sha256 => digest::<sha2::Sha256>(input, cancel),
        Hash::Sha384 => digest::<sha2::Sha384>(input, cancel),
        Hash::Sha512 => digest::<sha2::Sha512>(input, cancel),
        Hash::Sha3_224 => digest::<sha3::Sha3_224>(input, cancel),
        Hash::Sha3_256 => digest::<sha3::Sha3_256>(input, cancel),
        Hash::Sha3_384 => digest::<sha3::Sha3_384>(input, cancel),
        Hash::Sha3_512 => digest::<sha3::Sha3_512>(input, cancel),
        Hash::Blake3 => {
            let mut hash = blake3::Hasher::new();
            read_chunks(input, cancel, |chunk| {
                hash.update(chunk);
            })?;
            Ok(hash.finalize().to_hex().to_string())
        }
    }
}

fn read_chunks(
    mut input: File,
    cancel: &AtomicBool,
    mut read: impl FnMut(&[u8]),
) -> io::Result<()> {
    let mut buffer = vec![0; 128 * 1024];
    loop {
        files::check_cancel(cancel)?;
        let count = input.read(&mut buffer)?;
        if count == 0 {
            return Ok(());
        }
        read(&buffer[..count]);
    }
}

pub fn checksum_list(
    paths: &[PathBuf],
    base: &Path,
    method: Hash,
    cancel: &AtomicBool,
) -> io::Result<String> {
    let mut result = String::new();
    for path in paths {
        let name = path
            .strip_prefix(base)
            .map_err(io::Error::other)?
            .to_string_lossy();
        if name.contains(['\n', '\r']) {
            return Err(io::Error::other(
                "checksum filenames cannot contain newlines",
            ));
        }
        let hash = checksum(path, method, cancel)?;
        if method == Hash::Crc32 {
            writeln!(result, "{name} {hash}").unwrap();
        } else {
            writeln!(result, "{hash} *{name}").unwrap();
        }
    }
    Ok(result)
}

pub fn verify_checksums(path: &Path, method: Hash, cancel: &AtomicBool) -> io::Result<String> {
    let base = fs::canonicalize(
        path.parent()
            .ok_or_else(|| io::Error::other("manifest has no parent"))?,
    )?;
    let mut output = String::new();
    for line in BufReader::new(File::open(path)?).lines() {
        files::check_cancel(cancel)?;
        let line = line?;
        let line = line.trim_start_matches('\u{feff}');
        if line.trim().is_empty() || line.starts_with(';') || line.starts_with('#') {
            continue;
        }
        let parsed = if method == Hash::Crc32 {
            line.rsplit_once(' ')
        } else {
            line.split_once(' ')
                .map(|(hash, file)| (file.strip_prefix(['*', ' ']).unwrap_or(file), hash))
        };
        let Some((name, expected)) = parsed else {
            writeln!(output, "Ungültige Zeile: {line}").unwrap();
            continue;
        };
        let relative = Path::new(name);
        let target = (|| {
            if relative
                .components()
                .any(|part| !matches!(part, Component::Normal(_)))
            {
                return Err(io::Error::other("invalid manifest path"));
            }
            let target = fs::canonicalize(base.join(relative))?;
            if !target.starts_with(&base) {
                return Err(io::Error::other("path is outside the manifest directory"));
            }
            checksum(&target, method, cancel)
        })();
        match target {
            Ok(actual) if expected.eq_ignore_ascii_case(&actual) => {
                writeln!(output, "OK       {name}").unwrap()
            }
            Ok(_) => writeln!(output, "ABWEICHUNG {name}").unwrap(),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => return Err(error),
            Err(error) => writeln!(output, "FEHLER   {name}: {error}").unwrap(),
        }
    }
    Ok(output)
}

pub fn duplicates(paths: &[PathBuf], cancel: &AtomicBool) -> io::Result<Vec<Vec<PathBuf>>> {
    let mut sizes = HashMap::<u64, Vec<PathBuf>>::new();
    for root in paths {
        for item in WalkDir::new(root).follow_links(false) {
            files::check_cancel(cancel)?;
            let item = item?;
            let metadata = fs::symlink_metadata(item.path())?;
            if metadata.is_file() && !files::is_link(&metadata) {
                sizes
                    .entry(metadata.len())
                    .or_default()
                    .push(item.into_path());
            }
        }
    }
    let mut hashes = HashMap::<String, Vec<PathBuf>>::new();
    for group in sizes.into_values().filter(|group| group.len() > 1) {
        for path in group {
            let hash = checksum(&path, Hash::Blake3, cancel)?;
            hashes.entry(hash).or_default().push(path);
        }
    }
    let mut groups: Vec<_> = hashes
        .into_values()
        .filter(|group| group.len() > 1)
        .collect();
    groups.iter_mut().for_each(|group| group.sort());
    groups.sort();
    Ok(groups)
}

pub fn split(
    path: &Path,
    target: &Path,
    part_size: u64,
    cancel: &AtomicBool,
) -> io::Result<Vec<PathBuf>> {
    if part_size == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "part size must be positive",
        ));
    }
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::other("file name is missing"))?;
    let mut input = File::open(path)?;
    let mut remaining = input.metadata()?.len();
    let part_count = remaining.div_ceil(part_size).max(1);
    if part_count > 100_000 {
        return Err(io::Error::other("too many parts"));
    }
    let mut paths = Vec::new();
    for part in 1..=part_count {
        files::check_cancel(cancel)?;
        let mut name = name.to_os_string();
        name.push(format!(".{part:03}"));
        let path = target.join(name);
        let mut temp = NamedTempFile::new_in(target)?;
        let length = remaining.min(part_size);
        let actual = copy_cancelled((&mut input).take(length), &mut temp, cancel)?;
        if actual != length {
            return Err(io::Error::other("source size changed"));
        }
        temp.as_file().sync_all()?;
        temp.persist_noclobber(&path).map_err(|error| error.error)?;
        remaining -= actual;
        paths.push(path);
    }
    Ok(paths)
}

pub fn join(parts: &[PathBuf], target: &Path, cancel: &AtomicBool) -> io::Result<()> {
    if parts.is_empty() {
        return Err(io::Error::other("no parts selected"));
    }
    let parent = target
        .parent()
        .ok_or_else(|| io::Error::other("missing target directory"))?;
    let mut temp = NamedTempFile::new_in(parent)?;
    for path in parts {
        copy_cancelled(File::open(path)?, &mut temp, cancel)?;
    }
    temp.as_file().sync_all()?;
    files::check_cancel(cancel)?;
    temp.persist_noclobber(target)
        .map_err(|error| error.error)?;
    Ok(())
}

pub(crate) fn copy_cancelled(
    mut reader: impl Read,
    mut writer: impl Write,
    cancel: &AtomicBool,
) -> io::Result<u64> {
    let mut total = 0;
    let mut buffer = vec![0; 128 * 1024];
    loop {
        files::check_cancel(cancel)?;
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            return Ok(total);
        }
        writer.write_all(&buffer[..count])?;
        total += count as u64;
    }
}

pub fn output_name(value: &str) -> io::Result<&OsStr> {
    files::leaf_name(OsStr::new(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_view_keeps_utf16_encoding_and_surrogate_pairs_across_pages() {
        let path = NamedTempFile::new().unwrap();
        let value = format!("{}🦀Ende", "a".repeat(16_382));
        let encoded: Vec<u8> = [0xff, 0xfe]
            .into_iter()
            .chain(value.encode_utf16().flat_map(u16::to_le_bytes))
            .collect();
        fs::write(path.path(), encoded).unwrap();
        let first = preview(path.path(), 0, PreviewMode::Text).unwrap();
        let second = preview(path.path(), first.next, PreviewMode::Text).unwrap();
        assert!(first.text.ends_with('🦀'));
        assert_eq!(second.text, "Ende");
    }
}
