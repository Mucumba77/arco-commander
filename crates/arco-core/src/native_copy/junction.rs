use crate::files::check_cancel;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::mem::{offset_of, size_of};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::os::windows::io::AsRawHandle;
use std::path::Path;
use std::ptr::{copy_nonoverlapping, null, null_mut};
use std::sync::atomic::AtomicBool;
use windows_sys::Win32::Storage::FileSystem::{
    DELETE, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS,
    FILE_FLAG_OPEN_REPARSE_POINT, FILE_GENERIC_WRITE, FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES,
    FILE_RENAME_INFO, FILE_SHARE_READ, FileRenameInfo, MAXIMUM_REPARSE_DATA_BUFFER_SIZE,
    SetFileInformationByHandle,
};
use windows_sys::Win32::System::IO::DeviceIoControl;
use windows_sys::Win32::System::Ioctl::{FSCTL_GET_REPARSE_POINT, FSCTL_SET_REPARSE_POINT};
use windows_sys::Win32::System::SystemServices::IO_REPARSE_TAG_MOUNT_POINT;

pub(crate) struct Junction {
    input: File,
    data: Vec<u8>,
}

impl Junction {
    pub(crate) fn open(source: &Path) -> io::Result<Self> {
        let input = OpenOptions::new()
            .access_mode(FILE_READ_ATTRIBUTES | FILE_LIST_DIRECTORY)
            .share_mode(FILE_SHARE_READ)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(source)?;
        let attributes = input.metadata()?.file_attributes();
        let needed = FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT;
        if attributes & needed != needed {
            return Err(unsupported());
        }
        let data = read_data(&input)?;
        validate(&data)?;
        Ok(Self { input, data })
    }

    pub(crate) fn copy_to(&self, target: &Path, cancel: &AtomicBool) -> io::Result<()> {
        check_cancel(cancel)?;
        let parent = target
            .parent()
            .ok_or_else(|| io::Error::other("Der Zielordner der Verzeichnisjunction fehlt."))?;
        let staging = tempfile::tempdir_in(parent)?;
        let staged = staging.path().join("junction");
        fs::create_dir(&staged)?;
        let output = OpenOptions::new()
            .access_mode(DELETE | FILE_GENERIC_WRITE | FILE_READ_ATTRIBUTES)
            .share_mode(FILE_SHARE_READ)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(&staged)?;
        let metadata = output.metadata()?;
        if !metadata.is_dir() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(io::Error::other(
                "Der temporäre Junction-Ordner wurde ersetzt.",
            ));
        }
        let mut written = 0;
        let ok = unsafe {
            DeviceIoControl(
                output.as_raw_handle(),
                FSCTL_SET_REPARSE_POINT,
                self.data.as_ptr().cast(),
                self.data.len() as u32,
                null_mut(),
                0,
                &mut written,
                null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        if read_data(&output)? != self.data || read_data(&self.input)? != self.data {
            return Err(io::Error::other(
                "Die Verzeichnisjunction wurde während des Kopierens geändert.",
            ));
        }
        check_cancel(cancel)?;
        rename_noreplace(&output, target)
    }
}

fn rename_noreplace(file: &File, target: &Path) -> io::Result<()> {
    let mut name: Vec<_> = target.as_os_str().encode_wide().collect();
    if name.contains(&0) {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let name_size = u32::try_from(size_of_val(name.as_slice())).map_err(io::Error::other)?;
    name.push(0);
    let size = offset_of!(FILE_RENAME_INFO, FileName) + size_of_val(name.as_slice());
    // Keep the header correctly aligned and include room for the trailing name.
    let mut buffer =
        vec![FILE_RENAME_INFO::default(); size.div_ceil(size_of::<FILE_RENAME_INFO>())];
    let info = buffer.as_mut_ptr();
    unsafe {
        // ReplaceIfExists and RootDirectory stay zero. The target is absolute.
        (*info).FileNameLength = name_size;
        copy_nonoverlapping(
            name.as_ptr(),
            (&raw mut (*info).FileName).cast::<u16>(),
            name.len(),
        );
    }
    let size = u32::try_from(size).map_err(io::Error::other)?;
    let ok = unsafe {
        SetFileInformationByHandle(file.as_raw_handle(), FileRenameInfo, info.cast(), size)
    };
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn read_data(file: &File) -> io::Result<Vec<u8>> {
    let mut data = vec![0u8; MAXIMUM_REPARSE_DATA_BUFFER_SIZE as usize];
    let mut read = 0;
    let ok = unsafe {
        DeviceIoControl(
            file.as_raw_handle(),
            FSCTL_GET_REPARSE_POINT,
            null(),
            0,
            data.as_mut_ptr().cast(),
            data.len() as u32,
            &mut read,
            null_mut(),
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    data.truncate(read as usize);
    Ok(data)
}

fn validate(data: &[u8]) -> io::Result<()> {
    let invalid = || {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "Ungültige Reparse-Daten der Verzeichnisjunction.",
        )
    };
    if data.len() < 16 || u16::from_le_bytes([data[4], data[5]]) as usize + 8 != data.len() {
        return Err(invalid());
    }
    if u32::from_le_bytes([data[0], data[1], data[2], data[3]]) != IO_REPARSE_TAG_MOUNT_POINT {
        return Err(unsupported());
    }
    let name = |offset: usize| {
        let start = usize::from(u16::from_le_bytes([data[offset], data[offset + 1]]));
        let len = usize::from(u16::from_le_bytes([data[offset + 2], data[offset + 3]]));
        if start % 2 != 0 || len % 2 != 0 {
            return Err(invalid());
        }
        let bytes = data[16..].get(start..start + len).ok_or_else(invalid)?;
        let units: Vec<_> = bytes
            .chunks_exact(2)
            .map(|bytes| u16::from_le_bytes([bytes[0], bytes[1]]))
            .collect();
        if units.contains(&0) {
            return Err(invalid());
        }
        String::from_utf16(&units).map_err(|_| invalid())
    };
    let substitute = name(8)?;
    name(12)?;
    let target = substitute.strip_prefix(r"\??\").ok_or_else(unsupported)?;
    // A volume mount point uses the same tag, but a different substitute
    // namespace. Copy only local DOS-directory junctions.
    if !matches!(target.as_bytes(), [letter, b':', b'\\', rest @ ..]
        if letter.is_ascii_alphabetic() && !rest.contains(&b':') && !rest.contains(&b'/'))
    {
        return Err(unsupported());
    }
    Ok(())
}

fn unsupported() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "Nur Verzeichnisjunctions zu lokalen Laufwerkspfaden können als Links kopiert werden.",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reparse_fixture(tag: u32, target: &str) -> Vec<u8> {
        let substitute: Vec<_> = target.encode_utf16().flat_map(u16::to_le_bytes).collect();
        let len = substitute.len() as u16;
        let mut data = tag.to_le_bytes().to_vec();
        for value in [8 + len + 2, 0, 0, len, len + 2, 0] {
            data.extend(value.to_le_bytes());
        }
        data.extend(substitute);
        data.extend([0, 0]);
        data
    }

    #[test]
    fn junction_data_excludes_volume_mounts_other_tags_and_invalid_names() {
        let tag = IO_REPARSE_TAG_MOUNT_POINT;
        let local = r"\??\C:\Temp\Ordner ü";
        assert!(validate(&reparse_fixture(tag, local)).is_ok());
        for (tag, path) in [
            (0xa000_000c, local),
            (tag, r"\??\Volume{01234567-89ab-cdef-0123-456789abcdef}\"),
            (tag, r"\??\UNC\host\share"),
            (tag, r"\??\C:\folder:stream"),
        ] {
            assert_eq!(
                validate(&reparse_fixture(tag, path)).unwrap_err().kind(),
                io::ErrorKind::Unsupported
            );
        }
        let mut corrupt = reparse_fixture(tag, local);
        corrupt[8] = 1;
        assert_eq!(
            validate(&corrupt).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }
}
