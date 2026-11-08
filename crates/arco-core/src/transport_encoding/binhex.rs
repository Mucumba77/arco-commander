use super::{invalid, publish_group, source_file};
use crate::files;
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use tempfile::NamedTempFile;

const ALPHABET: &[u8; 64] = b"!\"#$%&'()*+,-012345689@ABCDEFGHIJKLMNPQRSTUVXYZ[`abcdefhijklmpqr";
const MARKER: u8 = 0x90;
const SYMBOLS: [u8; 256] = {
    let mut table = [u8::MAX; 256];
    let mut index = 0;
    while index < ALPHABET.len() {
        table[ALPHABET[index] as usize] = index as u8;
        index += 1;
    }
    table
};
const CRC: [u16; 256] = {
    let mut table = [0; 256];
    let mut index = 0;
    while index < table.len() {
        let mut crc = (index as u16) << 8;
        let mut bit = 0;
        while bit < 8 {
            crc = (crc << 1) ^ if crc & 0x8000 != 0 { 0x1021 } else { 0 };
            bit += 1;
        }
        table[index] = crc;
        index += 1;
    }
    table
};

pub(super) fn decode(
    source: &Path,
    directory: &Path,
    cancel: &AtomicBool,
) -> io::Result<Vec<PathBuf>> {
    let metadata = source_file(source)?;
    let mut input = BufReader::new(File::open(source)?.take(metadata.len()));
    let mut line_start = true;
    let mut found = false;
    for index in 0..64 * 1024 {
        if index % 4096 == 0 {
            files::check_cancel(cancel)?;
        }
        let mut byte = [0];
        if input.read(&mut byte)? == 0 {
            break;
        }
        if line_start && byte[0] == b':' {
            found = true;
            break;
        }
        line_start = matches!(byte[0], b'\r' | b'\n');
    }
    if !found {
        return Err(invalid("Kein BinHex-Datenblock gefunden."));
    }
    let mut decoded = HqxReader::new(input, cancel);
    let mut header = [0; 83];
    decoded.read_exact(&mut header[..1])?;
    let name_len = header[0] as usize;
    if !(1..=63).contains(&name_len) {
        return Err(invalid("Die BinHex-Namenslänge ist ungültig."));
    }
    let header = &mut header[..name_len + 20];
    decoded.read_exact(&mut header[1..])?;
    if header[name_len + 1] != 0 {
        return Err(invalid("Die BinHex-Header-Version wird nicht unterstützt."));
    }
    check_crc(&mut decoded, crc_update(0, header))?;
    let name = encoding_rs::MACINTOSH
        .decode_without_bom_handling(&header[1..=name_len])
        .0;
    files::leaf_name(OsStr::new(&*name))?;
    let size = |offset: usize| {
        u64::from(u32::from_be_bytes(
            header[offset..offset + 4].try_into().expect("four bytes"),
        ))
    };
    let data_len = size(name_len + 12);
    let resource_len = size(name_len + 16);
    if data_len + resource_len > metadata.len().saturating_mul(128) {
        return Err(invalid("Die BinHex-Nutzdatenlänge ist unplausibel."));
    }
    let directory = fs::canonicalize(directory)?;
    let mut file = NamedTempFile::new_in(&directory)?;
    copy_fork(&mut decoded, file.as_file_mut(), data_len)?;
    let mut staged = Vec::new();
    if resource_len == 0 {
        check_crc(&mut decoded, 0)?;
    } else {
        let mut resource = NamedTempFile::new_in(&directory)?;
        copy_fork(&mut decoded, resource.as_file_mut(), resource_len)?;
        staged.push((directory.join(format!("{name}.rsrc")), resource));
    }
    if decoded.read(&mut [0])? != 0 {
        return Err(invalid("Unerwartete Daten am Ende des BinHex-Blocks."));
    }
    files::ensure_unchanged(source, &metadata)?;
    staged.push((directory.join(&*name), file));
    publish_group(staged, cancel)
}

fn copy_fork(input: &mut impl Read, output: &mut File, mut len: u64) -> io::Result<()> {
    let mut buffer = [0; 16 * 1024];
    let mut output = BufWriter::new(output);
    let mut crc = 0;
    while len > 0 {
        let n = len.min(buffer.len() as u64) as usize;
        input.read_exact(&mut buffer[..n])?;
        output.write_all(&buffer[..n])?;
        crc = crc_update(crc, &buffer[..n]);
        len -= n as u64;
    }
    check_crc(input, crc)?;
    output.flush()
}

fn crc_update(mut crc: u16, bytes: &[u8]) -> u16 {
    for &byte in bytes {
        crc = (crc << 8) ^ CRC[(byte ^ (crc >> 8) as u8) as usize];
    }
    crc
}

fn check_crc(input: &mut impl Read, expected: u16) -> io::Result<()> {
    let mut stored = [0; 2];
    input.read_exact(&mut stored)?;
    if u16::from_be_bytes(stored) == expected {
        Ok(())
    } else {
        Err(invalid("Die BinHex-Prüfsumme stimmt nicht."))
    }
}

struct HqxReader<'a, R> {
    input: R,
    cancel: &'a AtomicBool,
    bits: u8,
    packed: u16,
    previous: Option<u8>,
    repeats: u8,
    ended: bool,
}

impl<'a, R: Read> HqxReader<'a, R> {
    fn new(input: R, cancel: &'a AtomicBool) -> Self {
        Self {
            input,
            cancel,
            bits: 0,
            packed: 0,
            previous: None,
            repeats: 0,
            ended: false,
        }
    }

    fn packed_byte(&mut self) -> io::Result<Option<u8>> {
        while self.bits < 8 && !self.ended {
            files::check_cancel(self.cancel)?;
            let mut byte = [0];
            self.input.read_exact(&mut byte).map_err(|error| {
                if error.kind() == io::ErrorKind::UnexpectedEof {
                    invalid("Der abschließende Doppelpunkt im BinHex-Block fehlt.")
                } else {
                    error
                }
            })?;
            match byte[0] {
                b'\r' | b'\n' | b' ' | b'\t' => continue,
                b':' => {
                    if self.bits == 6 || self.packed & ((1 << self.bits) - 1) != 0 {
                        return Err(invalid("Ungültige Restbits im BinHex-Block."));
                    }
                    self.ended = true;
                    return Ok(None);
                }
                byte => {
                    let value = SYMBOLS[byte as usize];
                    if value == u8::MAX {
                        return Err(invalid("Ungültiges Zeichen im BinHex-Block."));
                    }
                    self.packed = (self.packed << 6) | u16::from(value);
                    self.bits += 6;
                }
            }
        }
        if self.ended {
            return Ok(None);
        }
        self.bits -= 8;
        Ok(Some((self.packed >> self.bits) as u8))
    }

    fn next_byte(&mut self) -> io::Result<Option<u8>> {
        loop {
            if self.repeats > 0 {
                self.repeats -= 1;
                return Ok(self.previous);
            }
            let Some(byte) = self.packed_byte()? else {
                return Ok(None);
            };
            if byte == MARKER {
                let count = self
                    .packed_byte()?
                    .ok_or_else(|| invalid("Unvollständige BinHex-Wiederholung."))?;
                if count != 0 {
                    if self.previous.is_none() || count < 2 {
                        return Err(invalid("Ungültige BinHex-Wiederholung."));
                    }
                    self.repeats = count - 1;
                    continue;
                }
            }
            self.previous = Some(byte);
            return Ok(Some(byte));
        }
    }
}

impl<R: Read> Read for HqxReader<'_, R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        files::check_cancel(self.cancel)?;
        for (count, target) in buffer.iter_mut().enumerate() {
            let Some(value) = self.next_byte()? else {
                return Ok(count);
            };
            *target = value;
        }
        Ok(buffer.len())
    }

    fn read_exact(&mut self, buffer: &mut [u8]) -> io::Result<()> {
        if self.read(buffer)? != buffer.len() {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "Der BinHex-Datenblock ist unvollständig.",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binhex_decodes_cpython_data_and_resource_forks_and_checks_crc() {
        // Generated by Python 3.10 binhex.BinHex with a data and a resource fork.
        let fixture = b"(This file must be converted with BinHex 4.0)\r\r:#9\"bEf*P,R4iG!!rN!J!N!8+!!!!#cmB8T!)N!!\"0U+!8Q9cEh9bBf9cN!\"-F3:\r";
        let temp = tempfile::tempdir().unwrap();
        let input = temp.path().join("probe.hqx");
        let cancel = AtomicBool::new(false);
        fs::write(&input, fixture).unwrap();
        let result = decode(&input, temp.path(), &cancel).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(
            fs::read(temp.path().join("Probe.txt")).unwrap(),
            b"RRRRRRRR\x90\x01"
        );
        assert_eq!(
            fs::read(temp.path().join("Probe.txt.rsrc")).unwrap(),
            b"\x80Resources\x90"
        );
        for file in result {
            fs::remove_file(file).unwrap();
        }
        let mut corrupted = fixture.to_vec();
        let index = fixture
            .windows(3)
            .position(|chars| chars == b"F3:")
            .unwrap();
        corrupted[index] = b'E';
        fs::write(&input, corrupted).unwrap();
        assert!(
            decode(&input, temp.path(), &cancel)
                .unwrap_err()
                .to_string()
                .contains("Prüfsumme")
        );
        assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 1);
    }
}
