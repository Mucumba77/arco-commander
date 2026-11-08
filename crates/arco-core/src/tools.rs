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
use std::sync::Arc;
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
    pub ignore: Option<Arc<crate::ignore::IgnoreList>>,
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
        for item in
            walk.into_iter().filter_entry(|entry| {
                entry.depth() == 0
                    || (query.ignore.as_ref().is_none_or(|ignore| {
                        !ignore.local(entry.path(), entry.file_type().is_dir())
                    }) && entry.metadata().is_ok_and(|meta| !files::is_link(&meta)))
            })
        {
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PreviewEncoding {
    #[default]
    Auto,
    Utf8,
    Utf16Le,
    Utf16Be,
    Windows1252,
}

pub struct Preview {
    pub text: String,
    pub length: u64,
    pub offset: u64,
    pub next: u64,
}

pub fn preview(path: &Path, offset: u64, mode: PreviewMode) -> io::Result<Preview> {
    preview_with_encoding(path, offset, mode, PreviewEncoding::Auto)
}

pub fn preview_with_encoding(
    path: &Path,
    offset: u64,
    mode: PreviewMode,
    choice: PreviewEncoding,
) -> io::Result<Preview> {
    let mut file = File::open(path)?;
    let length = file.metadata()?.len();
    let (encoding, bom) = if mode == PreviewMode::Text {
        preview_encoding(&mut file, choice)?
    } else {
        (UTF_8, 0)
    };
    let mut offset = offset.min(length);
    if mode == PreviewMode::Text && offset < length {
        if encoding == UTF_16LE || encoding == UTF_16BE {
            offset -= offset % 2;
        } else if encoding == UTF_8 {
            for _ in 0..3 {
                if offset == 0 {
                    break;
                }
                file.seek(SeekFrom::Start(offset))?;
                let mut byte = [0];
                if file.read(&mut byte)? == 0 || byte[0] & 0xc0 != 0x80 {
                    break;
                }
                offset -= 1;
            }
        }
    }
    let skip = if offset == 0 { bom } else { 0 };
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

fn preview_encoding(
    file: &mut File,
    choice: PreviewEncoding,
) -> io::Result<(&'static Encoding, usize)> {
    file.seek(SeekFrom::Start(0))?;
    let mut sample = Vec::new();
    file.take(8192).read_to_end(&mut sample)?;
    let bom = Encoding::for_bom(&sample);
    let encoding = match choice {
        PreviewEncoding::Auto => bom.map_or_else(
            || {
                if std::str::from_utf8(&sample).is_err_and(|error| error.error_len().is_some()) {
                    WINDOWS_1252
                } else {
                    UTF_8
                }
            },
            |(encoding, _)| encoding,
        ),
        PreviewEncoding::Utf8 => UTF_8,
        PreviewEncoding::Utf16Le => UTF_16LE,
        PreviewEncoding::Utf16Be => UTF_16BE,
        PreviewEncoding::Windows1252 => WINDOWS_1252,
    };
    Ok((
        encoding,
        bom.filter(|(found, _)| *found == encoding)
            .map_or(0, |(_, size)| size),
    ))
}

pub fn find_preview(
    path: &Path,
    start: u64,
    mode: PreviewMode,
    choice: PreviewEncoding,
    query: &str,
    cancel: &AtomicBool,
) -> io::Result<Option<u64>> {
    if query.is_empty() || query.len() > 4096 {
        return Err(io::Error::other(
            "Der Suchwert benötigt 1 bis 4.096 UTF-8-Bytes.",
        ));
    }
    let metadata = files::regular_metadata(path)?;
    if !metadata.is_file() {
        return Err(io::Error::other(
            "Die Vorschau benötigt eine reguläre Datei.",
        ));
    }
    let mut file = File::open(path)?;
    let (bytes, start, utf16) = if mode == PreviewMode::Hex {
        let text: String = query
            .chars()
            .filter(|value| !value.is_ascii_whitespace())
            .collect();
        if text.is_empty()
            || !text.len().is_multiple_of(2)
            || !text.bytes().all(|value| value.is_ascii_hexdigit())
        {
            return Err(io::Error::other("Gib Hexbytes wie 4D 5A oder 4D5A ein."));
        }
        let bytes = (0..text.len())
            .step_by(2)
            .map(|index| u8::from_str_radix(&text[index..index + 2], 16).unwrap())
            .collect();
        (bytes, start, false)
    } else {
        let (encoding, bom) = preview_encoding(&mut file, choice)?;
        let utf16 = encoding == UTF_16LE || encoding == UTF_16BE;
        let bytes = if utf16 {
            query
                .encode_utf16()
                .flat_map(|unit| {
                    if encoding == UTF_16LE {
                        unit.to_le_bytes()
                    } else {
                        unit.to_be_bytes()
                    }
                })
                .collect()
        } else {
            let (bytes, _, errors) = encoding.encode(query);
            if errors {
                return Err(io::Error::other(
                    "Der Suchwert ist in der gewählten Kodierung nicht darstellbar.",
                ));
            }
            bytes.into_owned()
        };
        (bytes, start.max(bom as u64), utf16)
    };
    let result = find_bytes(&mut file, start.min(metadata.len()), &bytes, utf16, cancel)?;
    files::ensure_unchanged(path, &metadata)?;
    Ok(result)
}

fn find_bytes(
    file: &mut File,
    start: u64,
    pattern: &[u8],
    utf16: bool,
    cancel: &AtomicBool,
) -> io::Result<Option<u64>> {
    let mut prefixes = vec![0; pattern.len()];
    let mut matched = 0;
    for (index, &byte) in pattern.iter().enumerate().skip(1) {
        while matched > 0 && byte != pattern[matched] {
            matched = prefixes[matched - 1];
        }
        if byte == pattern[matched] {
            matched += 1;
        }
        prefixes[index] = matched;
    }
    let mut matched = 0;
    let mut buffer = vec![0; 64 * 1024];
    let mut offset = start;
    file.seek(SeekFrom::Start(start))?;
    loop {
        files::check_cancel(cancel)?;
        let count = file.read(&mut buffer)?;
        if count == 0 {
            return Ok(None);
        }
        for (index, &byte) in buffer[..count].iter().enumerate() {
            while matched > 0 && byte != pattern[matched] {
                matched = prefixes[matched - 1];
            }
            if byte == pattern[matched] {
                matched += 1;
                if matched == pattern.len() {
                    let position = offset + index as u64 + 1 - pattern.len() as u64;
                    if !utf16 || position.is_multiple_of(2) {
                        return Ok(Some(position));
                    }
                    matched = prefixes[matched - 1];
                }
            }
        }
        offset += count as u64;
    }
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

pub fn read_text_file(path: &Path, maximum: u64, cancel: &AtomicBool) -> io::Result<String> {
    let metadata = files::regular_metadata(path)?;
    if !metadata.is_file() || metadata.len() > maximum {
        return Err(io::Error::other(
            "Wähle eine Textdatei innerhalb der Größenbegrenzung.",
        ));
    }
    let mut buffer = Vec::new();
    copy_cancelled(
        File::open(path)?.take(maximum.saturating_add(1)),
        &mut buffer,
        cancel,
    )?;
    if buffer.len() as u64 != metadata.len() {
        return Err(io::Error::other("Die Dateigröße hat sich geändert."));
    }
    files::ensure_unchanged(path, &metadata)?;
    let text = decode_text(&buffer);
    if text.chars().any(|character| {
        character.is_ascii_control() && !matches!(character, '\r' | '\n' | '\t' | '\x0c')
    }) {
        return Err(io::Error::other("Die Datei enthält Binärdaten."));
    }
    Ok(text)
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

pub struct BinaryComparison {
    pub paths: [PathBuf; 2],
    metadata: [fs::Metadata; 2],
}

pub struct BinaryPage {
    pub differences: Vec<(u64, [Option<u8>; 2])>,
    pub has_previous: bool,
    pub has_next: bool,
}

impl BinaryComparison {
    pub fn open(paths: [PathBuf; 2]) -> io::Result<Self> {
        let metadata = [
            files::regular_metadata(&paths[0])?,
            files::regular_metadata(&paths[1])?,
        ];
        if metadata.iter().any(|item| !item.is_file()) {
            return Err(io::Error::other(
                "Für den Vergleich werden zwei Dateien benötigt.",
            ));
        }
        Ok(Self { paths, metadata })
    }

    pub fn page(&self, cursor: u64, backward: bool, cancel: &AtomicBool) -> io::Result<BinaryPage> {
        const PAGE: usize = 128;
        const BLOCK: u64 = 128 * 1024;
        self.verify()?;
        let mut files = [File::open(&self.paths[0])?, File::open(&self.paths[1])?];
        let mut buffers = [vec![0; BLOCK as usize], vec![0; BLOCK as usize]];
        let limit = self.metadata[0].len().max(self.metadata[1].len());
        let mut position = cursor.min(limit);
        let mut differences = Vec::with_capacity(PAGE + 1);
        while if backward {
            position > 0
        } else {
            position < limit
        } {
            crate::files::check_cancel(cancel)?;
            let (start, end) = if backward {
                (position.saturating_sub(BLOCK), position)
            } else {
                (position, position.saturating_add(BLOCK).min(limit))
            };
            let length = (end - start) as usize;
            let mut read = [0; 2];
            for side in 0..2 {
                files[side].seek(SeekFrom::Start(start))?;
                read[side] = read_block(&mut files[side], &mut buffers[side][..length])?;
            }
            let left = &buffers[0][..read[0]];
            let right = &buffers[1][..read[1]];
            if left != right {
                for index in 0..length {
                    let index = if backward { length - index - 1 } else { index };
                    let bytes = [left.get(index).copied(), right.get(index).copied()];
                    if bytes[0] != bytes[1] {
                        differences.push((start + index as u64, bytes));
                        if differences.len() > PAGE {
                            break;
                        }
                    }
                }
            }
            if differences.len() > PAGE {
                break;
            }
            position = if backward { start } else { end };
        }
        self.verify()?;
        let more = differences.len() > PAGE;
        differences.truncate(PAGE);
        if backward {
            differences.reverse();
        }
        Ok(BinaryPage {
            differences,
            // Nonzero cursors are boundaries of an already displayed page.
            has_previous: if backward { more } else { cursor > 0 },
            has_next: if backward { true } else { more },
        })
    }

    fn verify(&self) -> io::Result<()> {
        for (path, metadata) in self.paths.iter().zip(&self.metadata) {
            files::ensure_unchanged(path, metadata)?;
        }
        Ok(())
    }
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
    pub fn matches_text(self, value: &str) -> bool {
        let length = match self {
            Self::Crc32 => 8,
            Self::Md5 => 32,
            Self::Sha1 => 40,
            Self::Sha224 | Self::Sha3_224 => 56,
            Self::Sha256 | Self::Sha3_256 | Self::Blake3 => 64,
            Self::Sha384 | Self::Sha3_384 => 96,
            Self::Sha512 | Self::Sha3_512 => 128,
        };
        value.len() == length && value.bytes().all(|byte| byte.is_ascii_hexdigit())
    }

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

#[derive(Clone, Copy)]
pub enum DuplicateMode {
    Content,
    Name,
    Size,
    NameAndSize,
    NameAndContent,
}

pub fn duplicates(
    paths: &[PathBuf],
    mode: DuplicateMode,
    ignore: Option<&crate::ignore::IgnoreList>,
    cancel: &AtomicBool,
) -> io::Result<Vec<Vec<PathBuf>>> {
    let by_name = matches!(
        mode,
        DuplicateMode::Name | DuplicateMode::NameAndSize | DuplicateMode::NameAndContent
    );
    let by_size = !matches!(mode, DuplicateMode::Name);
    let by_content = matches!(mode, DuplicateMode::Content | DuplicateMode::NameAndContent);
    let mut candidates = HashMap::<_, Vec<_>>::new();
    let mut seen = std::collections::BTreeSet::new();
    for root in paths {
        for item in WalkDir::new(root)
            .follow_links(false)
            .into_iter()
            .filter_entry(|entry| {
                entry.depth() == 0
                    || (!ignore.is_some_and(|ignore| {
                        ignore.local(entry.path(), entry.file_type().is_dir())
                    }) && entry
                        .metadata()
                        .is_ok_and(|metadata| !files::is_link(&metadata)))
            })
        {
            files::check_cancel(cancel)?;
            let item = item?;
            let metadata = fs::symlink_metadata(item.path())?;
            if metadata.is_file() && !files::is_link(&metadata) {
                let path = fs::canonicalize(item.path())?;
                if seen.insert(path) {
                    let name = by_name.then(|| item.file_name().to_string_lossy().to_lowercase());
                    candidates
                        .entry((name, by_size.then_some(metadata.len())))
                        .or_default()
                        .push(item.into_path());
                }
            }
        }
    }
    let mut groups = Vec::new();
    for group in candidates.into_values().filter(|group| group.len() > 1) {
        if by_content {
            let mut hashes = HashMap::<String, Vec<PathBuf>>::new();
            for path in group {
                let before = files::regular_metadata(&path)?;
                let hash = checksum(&path, Hash::Blake3, cancel)?;
                files::ensure_unchanged(&path, &before)?;
                hashes.entry(hash).or_default().push(path);
            }
            groups.extend(hashes.into_values().filter(|group| group.len() > 1));
        } else {
            groups.push(group);
        }
    }
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

pub(crate) fn publish_parts(
    parts: Vec<(PathBuf, PathBuf)>,
    cancel: &AtomicBool,
) -> io::Result<usize> {
    let count = parts.len();
    for (_, target) in &parts {
        files::check_cancel(cancel)?;
        if target.try_exists()? {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("Archivteil existiert bereits: {}", target.display()),
            ));
        }
    }
    let mut published = Vec::with_capacity(count);
    let publish = (|| -> io::Result<()> {
        // The first part acts as the entry point and is published last.
        for (staged, target) in parts.into_iter().rev() {
            files::check_cancel(cancel)?;
            let metadata = files::regular_metadata(&staged)?;
            tempfile::TempPath::try_from_path(staged)?
                .persist_noclobber(&target)
                .map_err(|error| error.error)?;
            published.push((target, metadata));
        }
        Ok(())
    })();
    if let Err(error) = publish {
        let mut rollback_error = None;
        for (target, metadata) in published {
            if let Err(error) =
                files::ensure_unchanged(&target, &metadata).and_then(|()| fs::remove_file(&target))
            {
                rollback_error = Some(error);
            }
        }
        if let Some(rollback_error) = rollback_error {
            return Err(io::Error::other(format!(
                "{error}. Ein Archivteil konnte nicht zurückgenommen werden: {rollback_error}"
            )));
        }
        return Err(error);
    }
    Ok(count)
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
    #[test]
    fn binary_comparison_pages_across_blocks_and_extra_tail() {
        use super::*;
        let directory = tempfile::tempdir().unwrap();
        let paths = [directory.path().join("a"), directory.path().join("b")];
        let left = vec![0; 128 * 1024 + 200];
        let mut right = left.clone();
        right[128 * 1024 - 32..128 * 1024 + 128].fill(1);
        right.push(7);
        fs::write(&paths[0], left).unwrap();
        fs::write(&paths[1], right).unwrap();
        let comparison = BinaryComparison::open(paths.clone()).unwrap();
        let cancel = AtomicBool::new(false);
        let first = comparison.page(0, false, &cancel).unwrap();
        assert!(!first.has_previous && first.has_next);
        assert_eq!(first.differences.len(), 128);
        let second = comparison
            .page(first.differences.last().unwrap().0 + 1, false, &cancel)
            .unwrap();
        assert!(second.has_previous && !second.has_next);
        assert_eq!(second.differences.last().unwrap().1, [None, Some(7)]);
        let back = comparison
            .page(second.differences[0].0, true, &cancel)
            .unwrap();
        assert_eq!(back.differences, first.differences);
        fs::write(&paths[0], b"changed").unwrap();
        assert!(comparison.page(0, false, &cancel).is_err());
    }

    #[test]
    fn viewer_search_keeps_byte_positions_in_utf16_across_read_blocks() {
        use super::*;
        let fixture = tempfile::NamedTempFile::new().unwrap();
        let mut data = vec![0xff, 0xfe];
        for _ in 0..32767 {
            data.extend(b"a\0");
        }
        for unit in "Grüße".encode_utf16() {
            data.extend(unit.to_le_bytes());
        }
        fs::write(fixture.path(), data).unwrap();
        let cancel = AtomicBool::new(false);
        let offset = find_preview(
            fixture.path(),
            0,
            PreviewMode::Text,
            PreviewEncoding::Auto,
            "Grüße",
            &cancel,
        )
        .unwrap()
        .unwrap();
        assert_eq!(offset, 65536);
        let page = preview_with_encoding(
            fixture.path(),
            offset + 1,
            PreviewMode::Text,
            PreviewEncoding::Utf16Le,
        )
        .unwrap();
        assert_eq!(page.offset, offset);
        assert_eq!(page.text, "Grüße");
        assert_eq!(
            find_preview(
                fixture.path(),
                0,
                PreviewMode::Hex,
                PreviewEncoding::Auto,
                "47 00 72 00",
                &cancel
            )
            .unwrap(),
            Some(offset)
        );
        assert_eq!(
            find_preview(
                fixture.path(),
                offset + 1,
                PreviewMode::Text,
                PreviewEncoding::Auto,
                "Grüße",
                &cancel
            )
            .unwrap(),
            None
        );
    }

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
