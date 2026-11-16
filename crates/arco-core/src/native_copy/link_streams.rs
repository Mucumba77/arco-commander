use super::windows::file_id;
use crate::files::check_cancel;
use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::os::windows::ffi::OsStringExt;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use windows_sys::Win32::Foundation::{
    ERROR_HANDLE_EOF, ERROR_INSUFFICIENT_BUFFER, ERROR_MORE_DATA,
};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE, FILE_SHARE_READ,
    FileStreamInfo, GetFileInformationByHandleEx,
};

// Enumerate the opened reparse node, never the directory to which it points.
pub(super) fn streams(file: &File) -> io::Result<Vec<(Vec<u16>, u64)>> {
    let mut words = vec![0_u64; 512];
    loop {
        let ok = unsafe {
            GetFileInformationByHandleEx(
                file.as_raw_handle(),
                FileStreamInfo,
                words.as_mut_ptr().cast(),
                (words.len() * 8) as u32,
            )
        };
        if ok != 0 {
            let bytes =
                unsafe { std::slice::from_raw_parts(words.as_ptr().cast::<u8>(), words.len() * 8) };
            return parse(bytes);
        }
        let error = io::Error::last_os_error();
        match error.raw_os_error().map(|code| code as u32) {
            Some(ERROR_HANDLE_EOF) => return Ok(Vec::new()),
            Some(ERROR_MORE_DATA | ERROR_INSUFFICIENT_BUFFER) if words.len() < 131_072 => {
                words.resize(words.len() * 2, 0)
            }
            _ => return Err(error),
        }
    }
}

fn parse(bytes: &[u8]) -> io::Result<Vec<(Vec<u16>, u64)>> {
    let invalid = || io::Error::other("Windows lieferte ungültige Datenstrominformationen.");
    let mut offset = 0_usize;
    let mut found = Vec::new();
    loop {
        let record = bytes
            .get(offset..)
            .filter(|b| b.len() >= 24)
            .ok_or_else(invalid)?;
        let next = u32::from_le_bytes(record[..4].try_into().unwrap()) as usize;
        let len = u32::from_le_bytes(record[4..8].try_into().unwrap()) as usize;
        let size = u64::try_from(i64::from_le_bytes(record[8..16].try_into().unwrap()))
            .map_err(|_| invalid())?;
        if len < 2 || len % 2 != 0 {
            return Err(invalid());
        }
        let end = 24_usize.checked_add(len).ok_or_else(invalid)?;
        let name: Vec<_> = record
            .get(24..end)
            .ok_or_else(invalid)?
            .chunks_exact(2)
            .map(|u| u16::from_le_bytes([u[0], u[1]]))
            .collect();
        let suffix: Vec<_> = ":$DATA".encode_utf16().collect();
        if name.len() < suffix.len() + 1
            || name.first() != Some(&(b':' as u16))
            || !name.ends_with(&suffix)
            || name[1..name.len() - suffix.len()]
                .iter()
                .any(|ch| [0, b':' as u16, b'\\' as u16, b'/' as u16].contains(ch))
        {
            return Err(invalid());
        }
        if name.len() > suffix.len() + 1 {
            found.push((name, size));
        }
        if next == 0 {
            return Ok(found);
        }
        if next < end || next % 8 != 0 {
            return Err(invalid());
        }
        offset = offset
            .checked_add(next)
            .filter(|n| *n < bytes.len())
            .ok_or_else(invalid)?;
    }
}

fn path(base: &Path, name: &[u16]) -> PathBuf {
    let mut path = base.as_os_str().to_owned();
    path.push(OsString::from_wide(name));
    path.into()
}

pub(super) fn copy(
    input: &File,
    source: &Path,
    output: &File,
    target: &Path,
    cancel: &AtomicBool,
) -> io::Result<()> {
    let source_id = file_id(input)?;
    let target_id = file_id(output)?;
    for (name, expected) in streams(input)? {
        check_cancel(cancel)?;
        let read = OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_DELETE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path(source, &name))?;
        if file_id(&read)? != source_id {
            return Err(io::Error::other(
                "Der Junction-Datenstrom gehört zu einer anderen Quelle.",
            ));
        }
        let mut write = OpenOptions::new()
            .write(true)
            .create_new(true)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path(target, &name))?;
        if file_id(&write)? != target_id {
            return Err(io::Error::other(
                "Der Junction-Datenstrom gehört zu einem anderen Ziel.",
            ));
        }
        let copied = crate::tools::copy_cancelled(
            read.take(expected.saturating_add(1)),
            &mut write,
            cancel,
        )?;
        if copied != expected {
            return Err(io::Error::other(
                "Der Junction-Datenstrom wurde während des Kopierens geändert.",
            ));
        }
        write.sync_all()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(name: &str) -> Vec<u8> {
        let units: Vec<_> = name.encode_utf16().collect();
        let mut data = 0_u32.to_le_bytes().to_vec();
        data.extend(((units.len() * 2) as u32).to_le_bytes());
        data.extend(4_i64.to_le_bytes());
        data.extend(8_i64.to_le_bytes());
        data.extend(units.into_iter().flat_map(u16::to_le_bytes));
        data
    }

    #[test]
    fn stream_records_bound_offsets_and_exclude_names_with_path_or_stream_injection() {
        assert_eq!(parse(&record("::$DATA")).unwrap().len(), 0);
        assert_eq!(parse(&record(":Zone.Identifier:$DATA")).unwrap()[0].1, 4);
        for name in [
            ":x\\y:$DATA",
            ":../x:$DATA",
            ":x:y:$DATA",
            ":x\0:$DATA",
            ":x:$INDEX_ALLOCATION",
            ":$DATA",
            "x",
        ] {
            assert!(parse(&record(name)).is_err(), "{name:?}");
        }
        let mut data = record(":x:$DATA");
        data[..4].copy_from_slice(&8_u32.to_le_bytes());
        assert!(parse(&data).is_err());
        data[..4].fill(0);
        data[8..16].copy_from_slice(&(-1_i64).to_le_bytes());
        assert!(parse(&data).is_err());
    }
}
