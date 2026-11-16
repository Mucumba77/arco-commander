use crate::files::check_cancel;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::mem::{offset_of, size_of};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::ptr::{copy_nonoverlapping, null, null_mut};
use std::sync::atomic::AtomicBool;
use windows_sys::Win32::Storage::FileSystem::{
    DELETE, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT, FILE_DISPOSITION_INFO,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_GENERIC_WRITE,
    FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES, FILE_RENAME_INFO, FILE_SHARE_READ,
    FileDispositionInfo, FileRenameInfo, MAXIMUM_REPARSE_DATA_BUFFER_SIZE,
    SetFileInformationByHandle,
};
use windows_sys::Win32::System::IO::DeviceIoControl;
use windows_sys::Win32::System::Ioctl::{FSCTL_GET_REPARSE_POINT, FSCTL_SET_REPARSE_POINT};
use windows_sys::Win32::System::SystemServices::{
    IO_REPARSE_TAG_MOUNT_POINT, IO_REPARSE_TAG_SYMLINK,
};

pub(crate) struct Junction {
    input: File,
    data: Vec<u8>,
    directory: bool,
}

impl Junction {
    pub(crate) fn open_for_transfer(source: &Path, moving: bool) -> io::Result<Self> {
        let input = OpenOptions::new()
            .access_mode(
                FILE_READ_ATTRIBUTES | FILE_LIST_DIRECTORY | if moving { DELETE } else { 0 },
            )
            .share_mode(FILE_SHARE_READ)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(source)?;
        let attributes = input.metadata()?.file_attributes();
        if attributes & FILE_ATTRIBUTE_REPARSE_POINT == 0 {
            return Err(unsupported());
        }
        let data = read_data(&input)?;
        validate(&data)?;
        let directory = attributes & FILE_ATTRIBUTE_DIRECTORY != 0;
        if tag(&data) == IO_REPARSE_TAG_MOUNT_POINT && !directory {
            return Err(unsupported());
        }
        Ok(Self {
            input,
            data,
            directory,
        })
    }

    pub(crate) fn check(&self) -> io::Result<()> {
        if read_data(&self.input)? == self.data {
            Ok(())
        } else {
            Err(io::Error::other(
                "Die Verzeichnisjunction wurde während des Auftrags geändert.",
            ))
        }
    }

    pub(crate) fn remove(self) -> io::Result<()> {
        self.check()?;
        let info = FILE_DISPOSITION_INFO { DeleteFile: true };
        let ok = unsafe {
            SetFileInformationByHandle(
                self.input.as_raw_handle(),
                FileDispositionInfo,
                (&raw const info).cast(),
                size_of::<FILE_DISPOSITION_INFO>() as u32,
            )
        };
        if ok == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    pub(crate) fn copy_to(
        &self,
        source: &Path,
        target: &Path,
        native: &super::State,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        check_cancel(cancel)?;
        let parent = target
            .parent()
            .ok_or_else(|| io::Error::other("Der Zielordner der Verzeichnisjunction fehlt."))?;
        let staging = tempfile::tempdir_in(parent)?;
        let staged = staging.path().join("junction");
        let symbolic = tag(&self.data) == IO_REPARSE_TAG_SYMLINK;
        if symbolic {
            // The standard Windows API also supports Developer Mode without
            // SeCreateSymbolicLinkPrivilege. Never open or enumerate the target.
            let target = symbolic_target(&self.data)?;
            if self.directory {
                std::os::windows::fs::symlink_dir(target, &staged)?;
            } else {
                std::os::windows::fs::symlink_file(target, &staged)?;
            }
        } else {
            fs::create_dir(&staged)?;
        }
        let output = OpenOptions::new()
            .access_mode(DELETE | FILE_GENERIC_WRITE | FILE_READ_ATTRIBUTES)
            .share_mode(FILE_SHARE_READ)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(&staged)?;
        let metadata = output.metadata()?;
        let attributes = metadata.file_attributes();
        if (attributes & FILE_ATTRIBUTE_DIRECTORY != 0) != self.directory
            || (attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0) != symbolic
        {
            return Err(io::Error::other(
                "Der temporäre Junction-Ordner wurde ersetzt.",
            ));
        }
        if !symbolic {
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
        }
        let output_data = read_data(&output)?;
        let matches = if symbolic {
            equivalent_symbolic(&self.data, &output_data)?
        } else {
            output_data == self.data
        };
        if !matches || read_data(&self.input)? != self.data {
            return Err(io::Error::other(
                "Die Verzeichnisjunction wurde während des Kopierens geändert.",
            ));
        }
        native.prepare_link(&self.input, source, (&staged, &output), cancel)?;
        self.check()?;
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
    if tag(data) == IO_REPARSE_TAG_SYMLINK {
        symbolic_target(data)?;
        return Ok(());
    }
    if tag(data) != IO_REPARSE_TAG_MOUNT_POINT {
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

fn tag(data: &[u8]) -> u32 {
    u32::from_le_bytes(data[..4].try_into().expect("validated reparse header"))
}

fn symbolic_target(data: &[u8]) -> io::Result<PathBuf> {
    let invalid = || {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "Ungültige Daten der symbolischen Verknüpfung.",
        )
    };
    if data.len() < 20
        || tag(data) != IO_REPARSE_TAG_SYMLINK
        || u16::from_le_bytes([data[4], data[5]]) as usize + 8 != data.len()
    {
        return Err(invalid());
    }
    let flags = u32::from_le_bytes(data[16..20].try_into().unwrap());
    if flags > 1 {
        return Err(invalid());
    }
    let name = |offset| {
        let start = usize::from(u16::from_le_bytes([data[offset], data[offset + 1]]));
        let len = usize::from(u16::from_le_bytes([data[offset + 2], data[offset + 3]]));
        if start % 2 != 0 || len % 2 != 0 {
            return Err(invalid());
        }
        let bytes = data[20..].get(start..start + len).ok_or_else(invalid)?;
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
    if substitute.is_empty() {
        return Err(invalid());
    }
    if flags == 1 {
        let path = PathBuf::from(&substitute);
        if path.has_root()
            || path
                .components()
                .any(|component| matches!(component, std::path::Component::Prefix(_)))
            || substitute.contains(':')
        {
            return Err(invalid());
        }
        Ok(path)
    } else {
        let path = substitute.strip_prefix(r"\??\").ok_or_else(unsupported)?;
        if let Some(unc) = path.strip_prefix("UNC\\") {
            let mut parts = unc.split('\\');
            if parts.next().is_none_or(str::is_empty)
                || parts.next().is_none_or(str::is_empty)
                || unc.contains([':', '/'])
            {
                return Err(unsupported());
            }
            Ok(PathBuf::from(format!(r"\\{unc}")))
        } else if matches!(path.as_bytes(), [letter, b':', b'\\', rest @ ..] if letter.is_ascii_alphabetic() && !rest.contains(&b':') && !rest.contains(&b'/'))
        {
            Ok(PathBuf::from(path))
        } else {
            Err(unsupported())
        }
    }
}

fn equivalent_symbolic(source: &[u8], output: &[u8]) -> io::Result<bool> {
    let left = symbolic_target(source)?;
    let right = symbolic_target(output)?;
    let relative = source[16..20] == [1, 0, 0, 0];
    Ok(source[16..20] == output[16..20]
        && if relative {
            left == right
        } else {
            left.as_os_str().eq_ignore_ascii_case(right.as_os_str())
        })
}

fn unsupported() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "Unterstützt werden symbolische Verknüpfungen und lokale Verzeichnisjunctions. Andere Reparse-Typen und Volume-Mounts werden nicht übertragen.",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reparse_fixture(tag: u32, target: &str) -> Vec<u8> {
        let substitute: Vec<_> = target.encode_utf16().flat_map(u16::to_le_bytes).collect();
        let len = substitute.len() as u16;
        let mut data = tag.to_le_bytes().to_vec();
        for value in [8 + len + 4, 0, 0, len, len + 2, 0] {
            data.extend(value.to_le_bytes());
        }
        data.extend(substitute);
        data.extend([0, 0, 0, 0]);
        data
    }

    fn link(path: &Path, target: &Path) {
        fs::create_dir(path).unwrap();
        let file = OpenOptions::new()
            .access_mode(FILE_GENERIC_WRITE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
            .unwrap();
        let target = format!(r"\??\{}", target.display());
        let data = reparse_fixture(IO_REPARSE_TAG_MOUNT_POINT, &target);
        let mut count = 0;
        assert_ne!(
            unsafe {
                DeviceIoControl(
                    file.as_raw_handle(),
                    FSCTL_SET_REPARSE_POINT,
                    data.as_ptr().cast(),
                    data.len() as u32,
                    null_mut(),
                    0,
                    &mut count,
                    null_mut(),
                )
            },
            0,
            "{}",
            io::Error::last_os_error()
        );
    }

    #[test]
    fn recursive_transfer_copies_and_moves_junction_nodes_without_traversing_their_targets() {
        use crate::files::{self, Conflict, NtfsCopyOptions, Transfer, TransferPolicy};
        for operation in [Transfer::Copy, Transfer::Move] {
            let root = tempfile::tempdir().unwrap();
            let source = root.path().join("source");
            let destination = root.path().join("destination");
            let external = root.path().join("external");
            for dir in [&source, &destination, &external] {
                fs::create_dir(dir).unwrap();
            }
            fs::write(source.join("ordinary.txt"), b"local").unwrap();
            fs::write(external.join("must-stay.txt"), b"outside").unwrap();
            link(&source.join("linked"), &external);
            // A dangling junction is still a transferable node.
            link(&source.join("dangling"), &root.path().join("missing"));
            let policy = TransferPolicy {
                ntfs: NtfsCopyOptions {
                    copy_links: true,
                    ..Default::default()
                },
                ..Conflict::Fail.into()
            };
            let report = files::transfer(
                std::slice::from_ref(&source),
                &destination,
                operation,
                policy,
                &AtomicBool::new(false),
                None,
                |_| {},
            )
            .unwrap();
            assert!(report.errors.is_empty(), "{:?}", report.errors);
            assert_eq!(report.completed, 1);
            assert_eq!(
                fs::read(external.join("must-stay.txt")).unwrap(),
                b"outside"
            );
            let published = destination.join("source");
            assert_eq!(fs::read(published.join("ordinary.txt")).unwrap(), b"local");
            for name in ["linked", "dangling"] {
                let path = published.join(name);
                assert!(files::is_link(&fs::symlink_metadata(&path).unwrap()));
                assert!(Junction::open_for_transfer(&path, false).is_ok());
            }
            assert_eq!(source.exists(), operation == Transfer::Copy);
        }
    }

    #[test]
    fn moving_selected_junctions_and_skipped_nested_links_preserves_external_contents() {
        use crate::files::{self, Conflict, Transfer};
        let root = tempfile::tempdir().unwrap();
        let external = root.path().join("external");
        let destination = root.path().join("destination");
        fs::create_dir(&external).unwrap();
        fs::create_dir(&destination).unwrap();
        fs::write(external.join("keep.txt"), b"keep").unwrap();
        let selected = root.path().join("selected");
        link(&selected, &external);
        let result = files::transfer(
            std::slice::from_ref(&selected),
            &destination,
            Transfer::Move,
            Conflict::Fail,
            &AtomicBool::new(false),
            None,
            |_| {},
        )
        .unwrap();
        assert!(result.errors.is_empty(), "{:?}", result.errors);
        assert_eq!(result.completed, 1);
        assert!(fs::symlink_metadata(&selected).is_err());
        let source = root.path().join("source");
        fs::create_dir(&source).unwrap();
        link(&source.join("nested"), &external);
        let target = destination.join("source");
        fs::create_dir(&target).unwrap();
        fs::create_dir(target.join("nested")).unwrap();
        let result = files::transfer(
            std::slice::from_ref(&source),
            &destination,
            Transfer::Move,
            Conflict::Skip,
            &AtomicBool::new(false),
            None,
            |_| {},
        )
        .unwrap();
        assert!(result.errors.is_empty(), "{:?}", result.errors);
        assert_eq!(result.skipped, 1);
        assert!(files::is_link(
            &fs::symlink_metadata(source.join("nested")).unwrap()
        ));
        assert_eq!(fs::read(external.join("keep.txt")).unwrap(), b"keep");
    }

    #[test]
    fn junction_transfers_honor_directory_ignores_and_cancel_before_publication() {
        use crate::files::{self, Conflict, NtfsCopyOptions, Transfer, TransferPolicy};
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let destination = root.path().join("destination");
        let external = root.path().join("external");
        for path in [&source, &destination, &external] {
            fs::create_dir(path).unwrap();
        }
        fs::write(source.join("ordinary.txt"), b"ordinary").unwrap();
        fs::write(external.join("keep.txt"), b"outside").unwrap();
        link(&source.join("ignored"), &external);
        let ignore = crate::ignore::IgnoreList::parse("ignored/").unwrap();
        let policy = TransferPolicy {
            ignore: Some(&ignore),
            ..Conflict::Fail.into()
        };
        let report = files::transfer(
            std::slice::from_ref(&source),
            &destination,
            Transfer::Move,
            policy,
            &AtomicBool::new(false),
            None,
            |_| {},
        )
        .unwrap();
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(report.ignored, 1);
        assert!(files::is_link(
            &fs::symlink_metadata(source.join("ignored")).unwrap()
        ));
        assert!(!source.join("ordinary.txt").exists());
        assert_eq!(
            fs::read(destination.join("source/ordinary.txt")).unwrap(),
            b"ordinary"
        );
        let selected = root.path().join("selected");
        link(&selected, &external);
        let cancel = AtomicBool::new(false);
        let policy = TransferPolicy {
            ntfs: NtfsCopyOptions {
                copy_links: true,
                ..Default::default()
            },
            ..Conflict::Fail.into()
        };
        let report = files::transfer(
            std::slice::from_ref(&selected),
            &destination,
            Transfer::Copy,
            policy,
            &cancel,
            None,
            |_| cancel.store(true, std::sync::atomic::Ordering::Relaxed),
        )
        .unwrap();
        assert!(report.cancelled);
        assert!(files::is_link(&fs::symlink_metadata(&selected).unwrap()));
        assert!(fs::symlink_metadata(destination.join("selected")).is_err());
        assert_eq!(fs::read(external.join("keep.txt")).unwrap(), b"outside");
    }

    #[test]
    fn junction_metadata_and_streams_belong_to_the_link_instead_of_its_target() {
        use crate::files::{self, Conflict, NtfsCopyOptions, Transfer, TransferPolicy};
        use std::io::Write;
        use std::path::PathBuf;
        use windows_sys::Win32::Security::{
            DACL_SECURITY_INFORMATION, GetSecurityDescriptorControl, SE_DACL_PROTECTED,
        };
        use windows_sys::Win32::Storage::FileSystem::{FILE_BASIC_INFO, FileBasicInfo};
        let root = tempfile::tempdir().unwrap();
        let external = root.path().join("external");
        let destination = root.path().join("destination");
        fs::create_dir(&external).unwrap();
        fs::create_dir(&destination).unwrap();
        let source = root.path().join("selected");
        link(&source, &external);
        let write_stream = |base: &Path, name: &str, data: &[u8]| {
            let mut path = base.as_os_str().to_owned();
            path.push(name);
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
                .open(PathBuf::from(path))
                .unwrap();
            file.write_all(data).unwrap();
        };
        write_stream(&source, ":link-only:$DATA", b"link metadata");
        write_stream(&external, ":target-only:$DATA", b"target metadata");
        let before_target = fs::metadata(&external).unwrap();
        let input = OpenOptions::new()
            .access_mode(windows_sys::Win32::Storage::FileSystem::FILE_WRITE_ATTRIBUTES)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(&source)
            .unwrap();
        let created = fs::symlink_metadata(&source).unwrap().creation_time() as i64 - 20_000_000;
        let basic = FILE_BASIC_INFO {
            CreationTime: created,
            FileAttributes: 0x26,
            ..Default::default()
        };
        assert_ne!(
            unsafe {
                SetFileInformationByHandle(
                    input.as_raw_handle(),
                    FileBasicInfo,
                    (&raw const basic).cast(),
                    size_of::<FILE_BASIC_INFO>() as u32,
                )
            },
            0
        );
        drop(input);
        let options = NtfsCopyOptions {
            streams: true,
            permissions: true,
            owner: true,
            creation_time: true,
            copy_links: true,
            ..Default::default()
        };
        let policy = TransferPolicy {
            ntfs: options,
            ..Conflict::Fail.into()
        };
        let report = files::transfer(
            std::slice::from_ref(&source),
            &destination,
            Transfer::Copy,
            policy,
            &AtomicBool::new(false),
            None,
            |_| {},
        )
        .unwrap();
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        let target = destination.join("selected");
        let opened = Junction::open_for_transfer(&target, false).unwrap();
        let meta = opened.input.metadata().unwrap();
        assert_eq!(meta.creation_time() as i64, created);
        assert_eq!(meta.file_attributes() & 0x26, 0x26);
        let streams = super::super::link_streams::streams(&opened.input).unwrap();
        assert_eq!(streams.len(), 1);
        assert_eq!(
            String::from_utf16(&streams[0].0).unwrap(),
            ":link-only:$DATA"
        );
        let mut stream = target.as_os_str().to_owned();
        stream.push(":link-only:$DATA");
        let bytes = OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(PathBuf::from(stream))
            .unwrap();
        use std::io::Read;
        let mut contents = Vec::new();
        bytes.take(100).read_to_end(&mut contents).unwrap();
        assert_eq!(contents, b"link metadata");
        let security_handle = OpenOptions::new()
            .access_mode(windows_sys::Win32::Storage::FileSystem::READ_CONTROL)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(&target)
            .unwrap();
        let descriptor =
            super::super::security::Descriptor::read(&security_handle, DACL_SECURITY_INFORMATION)
                .unwrap();
        let mut control = 0;
        let mut revision = 0;
        assert_ne!(
            unsafe { GetSecurityDescriptorControl(descriptor.raw, &mut control, &mut revision) },
            0
        );
        assert_ne!(control & SE_DACL_PROTECTED, 0);
        assert_eq!(
            fs::metadata(&external).unwrap().creation_time(),
            before_target.creation_time()
        );
        let directory = OpenOptions::new()
            .access_mode(FILE_READ_ATTRIBUTES | FILE_LIST_DIRECTORY)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(&external)
            .unwrap();
        let external_streams = super::super::link_streams::streams(&directory).unwrap();
        assert_eq!(external_streams.len(), 1);
        assert_eq!(
            String::from_utf16(&external_streams[0].0).unwrap(),
            ":target-only:$DATA"
        );
    }

    #[test]
    fn junction_data_excludes_volume_mounts_other_tags_and_invalid_names() {
        let tag = IO_REPARSE_TAG_MOUNT_POINT;
        let local = r"\??\C:\Temp\Ordner ü";
        assert!(validate(&reparse_fixture(tag, local)).is_ok());
        for (tag, path) in [
            (0xa000_001d, local),
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

    fn symbolic_fixture(target: &str, relative: bool) -> Vec<u8> {
        let name: Vec<_> = target.encode_utf16().flat_map(u16::to_le_bytes).collect();
        let len = name.len() as u16;
        let mut data = IO_REPARSE_TAG_SYMLINK.to_le_bytes().to_vec();
        for value in [12 + len + 4, 0, 0, len, len + 2, 0] {
            data.extend(value.to_le_bytes());
        }
        data.extend(u32::from(relative).to_le_bytes());
        data.extend(name);
        data.extend([0, 0, 0, 0]);
        data
    }

    #[test]
    fn symbolic_reparse_validation_preserves_relative_targets_and_rejects_namespaces_and_corruption()
     {
        for (target, relative, expected) in [
            (r"..\outside", true, r"..\outside"),
            (r"\??\C:\outside", false, r"C:\outside"),
            (r"\??\UNC\host\share\folder", false, r"\\host\share\folder"),
        ] {
            let data = symbolic_fixture(target, relative);
            validate(&data).unwrap();
            assert_eq!(symbolic_target(&data).unwrap(), Path::new(expected));
            assert!(equivalent_symbolic(&data, &data).unwrap());
        }
        for (target, relative) in [
            (r"\??\Volume{01234567-89ab-cdef-0123-456789abcdef}\", false),
            (r"C:\absolute", true),
            (r"target:stream", true),
            (r"\??\UNC\host", false),
            ("", true),
        ] {
            assert!(
                validate(&symbolic_fixture(target, relative)).is_err(),
                "{target}"
            );
        }
        let mut corrupt = symbolic_fixture("relative", true);
        corrupt[16] = 2;
        assert!(validate(&corrupt).is_err());
        let mut corrupt = symbolic_fixture("relative", true);
        corrupt[8] = 1;
        assert!(validate(&corrupt).is_err());
    }

    #[test]
    #[ignore = "Requires Windows Developer Mode or SeCreateSymbolicLinkPrivilege; no elevation prompt is started"]
    fn symbolic_links_preserve_relative_absolute_and_dangling_targets_without_traversal() {
        use crate::files::{self, Conflict, NtfsCopyOptions, Transfer, TransferPolicy};
        for operation in [Transfer::Copy, Transfer::Move] {
            let fixture = tempfile::tempdir().unwrap();
            let source = fixture.path().join("source");
            let destination = fixture.path().join("destination");
            let outside = fixture.path().join("outside");
            for folder in [&source, &destination, &outside] {
                fs::create_dir(folder).unwrap();
            }
            fs::write(outside.join("keep.txt"), b"outside").unwrap();
            fs::write(source.join("inside.txt"), b"inside").unwrap();
            std::os::windows::fs::symlink_file("inside.txt", source.join("relative.txt")).unwrap();
            std::os::windows::fs::symlink_file(
                outside.join("keep.txt"),
                source.join("absolute.txt"),
            )
            .unwrap();
            std::os::windows::fs::symlink_dir(&outside, source.join("directory")).unwrap();
            std::os::windows::fs::symlink_file("missing.txt", source.join("dangling.txt")).unwrap();
            std::os::windows::fs::symlink_dir("missing-folder", source.join("dangling-dir"))
                .unwrap();
            let expected: Vec<_> = [
                "relative.txt",
                "absolute.txt",
                "directory",
                "dangling.txt",
                "dangling-dir",
            ]
            .map(|name| (name, fs::read_link(source.join(name)).unwrap()))
            .into();
            let policy = TransferPolicy {
                ntfs: NtfsCopyOptions {
                    copy_links: true,
                    streams: true,
                    creation_time: true,
                    ..Default::default()
                },
                ..Conflict::Fail.into()
            };
            let report = files::transfer(
                std::slice::from_ref(&source),
                &destination,
                operation,
                policy,
                &AtomicBool::new(false),
                None,
                |_| {},
            )
            .unwrap();
            assert!(report.errors.is_empty(), "{:?}", report.errors);
            assert_eq!(report.completed, 1);
            for (name, target) in expected {
                assert_eq!(
                    fs::read_link(destination.join("source").join(name)).unwrap(),
                    target
                );
            }
            assert_eq!(fs::read(outside.join("keep.txt")).unwrap(), b"outside");
            assert_eq!(source.exists(), operation == Transfer::Copy);
        }
    }
}
