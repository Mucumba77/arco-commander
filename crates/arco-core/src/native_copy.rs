#[cfg(windows)]
pub(crate) use windows::*;
#[cfg(windows)]
pub(crate) mod junction;
#[cfg(windows)]
mod security;

#[cfg(not(windows))]
pub(crate) struct State;

#[cfg(not(windows))]
impl State {
    pub(crate) fn new(_: crate::files::NtfsCopyOptions) -> Self {
        Self
    }
}

#[cfg(windows)]
mod windows {
    use super::security;
    use crate::files::{self, NtfsCopyOptions};
    use filetime::{FileTime, set_file_handle_times};
    use std::collections::BTreeMap;
    use std::ffi::OsString;
    use std::fs::{self, File, Metadata};
    use std::io::{self, Read, Seek, SeekFrom, Write};
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
    use std::os::windows::io::AsRawHandle;
    use std::path::{Path, PathBuf};
    use std::ptr::{null, null_mut};
    use std::sync::atomic::AtomicBool;
    use windows_sys::Win32::Foundation::{
        ERROR_HANDLE_EOF, ERROR_INVALID_FUNCTION, ERROR_INVALID_PARAMETER, ERROR_MORE_DATA,
        ERROR_NOT_SUPPORTED, HANDLE, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_ARCHIVE, FILE_ATTRIBUTE_HIDDEN, FILE_ATTRIBUTE_NORMAL,
        FILE_ATTRIBUTE_READONLY, FILE_ATTRIBUTE_SPARSE_FILE, FILE_ATTRIBUTE_SYSTEM,
        FILE_BASIC_INFO, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_ID_INFO,
        FILE_WRITE_ATTRIBUTES, FileBasicInfo, FileIdInfo, FindClose, FindFirstStreamW,
        FindNextStreamW, FindStreamInfoStandard, GetFileInformationByHandleEx, READ_CONTROL,
        SetFileInformationByHandle, WIN32_FIND_STREAM_DATA,
    };
    use windows_sys::Win32::System::IO::DeviceIoControl;
    use windows_sys::Win32::System::Ioctl::{
        FILE_ALLOCATED_RANGE_BUFFER, FSCTL_QUERY_ALLOCATED_RANGES, FSCTL_SET_SPARSE,
    };

    pub(crate) type FileId = (u64, [u8; 16]);

    struct PublishedLink {
        path: PathBuf,
        identity: FileId,
        snapshot: Metadata,
    }

    pub(crate) struct State {
        pub(crate) options: NtfsCopyOptions,
        links: BTreeMap<FileId, PublishedLink>,
    }

    impl State {
        pub(crate) fn new(options: NtfsCopyOptions) -> Self {
            Self {
                options,
                links: BTreeMap::new(),
            }
        }

        pub(crate) fn try_link(
            &self,
            input: &File,
            source: &Path,
            snapshot: &Metadata,
            target: &Path,
            previous: Option<&Metadata>,
            cancel: &AtomicBool,
        ) -> io::Result<bool> {
            if !self.options.hardlinks {
                return Ok(false);
            }
            let Some(first) = self.links.get(&file_id(input)?) else {
                return Ok(false);
            };
            let Ok(original) = security::open(&first.path, FILE_WRITE_ATTRIBUTES) else {
                return Ok(false);
            };
            if file_id(&original)? != first.identity
                || files::ensure_unchanged(&first.path, &first.snapshot).is_err()
                || first.snapshot.len() != snapshot.len()
                || first.snapshot.modified()? != snapshot.modified()?
            {
                return Ok(false);
            }
            let parent = target
                .parent()
                .ok_or_else(|| io::Error::other("Der Zielordner fehlt."))?;
            let staging = tempfile::tempdir_in(parent)?;
            let link = staging.path().join("link");
            fs::hard_link(&first.path, &link)?;
            if file_id(&File::open(&link)?)? != first.identity {
                return Err(io::Error::other(
                    "Das bereits kopierte Hardlink-Ziel hat sich geändert.",
                ));
            }
            files::ensure_unchanged(source, snapshot)?;
            files::check_cancel(cancel)?;
            let link = tempfile::TempPath::try_from_path(link)?;
            let result = if let Some(previous) = previous {
                files::ensure_unchanged(target, previous)?;
                link.persist(target)
            } else {
                link.persist_noclobber(target)
            }
            .map_err(|error| error.error);
            // TempPath::persist clears attributes on the staging name. A
            // hardlink shares these attributes with the first published file.
            let attributes = self.copy_basic_attributes(&original, snapshot, snapshot);
            result?;
            attributes?;
            Ok(true)
        }

        pub(crate) fn remember(
            &mut self,
            input: &File,
            published: &File,
            target: &Path,
        ) -> io::Result<()> {
            if self.options.hardlinks {
                self.links.insert(
                    file_id(input)?,
                    PublishedLink {
                        path: target.into(),
                        identity: file_id(published)?,
                        snapshot: published.metadata()?,
                    },
                );
            }
            Ok(())
        }

        pub(crate) fn sparse(
            &self,
            input: &mut File,
            output: &mut File,
            source: &Metadata,
            cancel: &AtomicBool,
            progress: impl FnMut(u64),
        ) -> io::Result<bool> {
            if !self.options.sparse || source.file_attributes() & FILE_ATTRIBUTE_SPARSE_FILE == 0 {
                return Ok(false);
            }
            sparse_copy(input, output, source.len(), cancel, progress)?;
            Ok(true)
        }

        pub(crate) fn prepare(
            &self,
            input: &File,
            source: &Path,
            target: (&Path, &File),
            cancel: &AtomicBool,
        ) -> io::Result<()> {
            if self.options.streams {
                copy_streams(source, target.0, cancel)?;
            }
            files::check_cancel(cancel)?;
            security::copy(input, source, target, self.options)
        }

        pub(crate) fn check_audit_privilege(&self) -> io::Result<()> {
            if self.options.audit {
                security::check_audit()?;
            }
            Ok(())
        }

        fn copies_directory_metadata(&self) -> bool {
            let options = self.options;
            options.streams || options.permissions || options.owner || options.audit
        }

        pub(crate) fn directory_identity(&self, path: &Path) -> io::Result<Option<FileId>> {
            match file_id(&security::open(path, 0)?) {
                Ok(id) => Ok(Some(id)),
                Err(error)
                    if !self.copies_directory_metadata()
                        && !self.options.creation_time
                        && error.raw_os_error().is_some_and(|code| {
                            matches!(
                                code as u32,
                                ERROR_INVALID_FUNCTION
                                    | ERROR_INVALID_PARAMETER
                                    | ERROR_NOT_SUPPORTED
                            )
                        }) =>
                {
                    Ok(None)
                }
                Err(error) => Err(error),
            }
        }

        pub(crate) fn prepare_directory(
            &self,
            source: &Path,
            target: &Path,
            expected: FileId,
            metadata: &Metadata,
            cancel: &AtomicBool,
        ) -> io::Result<()> {
            let access = FILE_WRITE_ATTRIBUTES | if self.options.owner { READ_CONTROL } else { 0 };
            let output = security::open(target, access)?;
            if file_id(&output)? != expected {
                return Err(io::Error::other("Der NTFS-Zielordner hat sich geändert."));
            }
            if self.copies_directory_metadata() {
                let input = fs::OpenOptions::new()
                    .access_mode(READ_CONTROL)
                    .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
                    .open(source)?;
                if files::is_link(&input.metadata()?) {
                    return Err(io::ErrorKind::Unsupported.into());
                }
                self.prepare(&input, source, (target, &output), cancel)?;
            }
            self.copy_basic_attributes(&output, metadata, metadata)?;
            // Writing named streams can change these times, so restore them last.
            set_file_handle_times(
                &output,
                Some(FileTime::from_last_access_time(metadata)),
                Some(FileTime::from_last_modification_time(metadata)),
            )
        }

        pub(crate) fn copy_basic_attributes(
            &self,
            file: &File,
            metadata: &Metadata,
            creation_source: &Metadata,
        ) -> io::Result<()> {
            const DOS: u32 = FILE_ATTRIBUTE_READONLY
                | FILE_ATTRIBUTE_HIDDEN
                | FILE_ATTRIBUTE_SYSTEM
                | FILE_ATTRIBUTE_ARCHIVE;
            let attributes = metadata.file_attributes() & DOS;
            let created = if self.options.creation_time {
                let created =
                    i64::try_from(creation_source.creation_time()).map_err(io::Error::other)?;
                if created == 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::Unsupported,
                        "Die Quelle liefert keine übertragbare Erstellungszeit.",
                    ));
                }
                created
            } else {
                0
            };
            let info = FILE_BASIC_INFO {
                CreationTime: created,
                FileAttributes: if attributes == 0 {
                    FILE_ATTRIBUTE_NORMAL
                } else {
                    attributes
                },
                ..Default::default()
            };
            let ok = unsafe {
                SetFileInformationByHandle(
                    file.as_raw_handle(),
                    FileBasicInfo,
                    (&raw const info).cast(),
                    std::mem::size_of::<FILE_BASIC_INFO>() as u32,
                )
            };
            if ok == 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        }
    }

    fn wide(path: &Path) -> io::Result<Vec<u16>> {
        let mut path: Vec<_> = path.as_os_str().encode_wide().collect();
        if path.contains(&0) {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        path.push(0);
        Ok(path)
    }

    pub(super) fn file_id(file: &File) -> io::Result<FileId> {
        let mut info = FILE_ID_INFO::default();
        let ok = unsafe {
            GetFileInformationByHandleEx(
                file.as_raw_handle(),
                FileIdInfo,
                (&raw mut info).cast(),
                std::mem::size_of::<FILE_ID_INFO>() as u32,
            )
        };
        if ok == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok((info.VolumeSerialNumber, info.FileId.Identifier))
        }
    }

    struct SearchHandle(HANDLE);
    impl Drop for SearchHandle {
        fn drop(&mut self) {
            unsafe { FindClose(self.0) };
        }
    }

    fn stream_path(path: &Path, name: &[u16]) -> PathBuf {
        let mut path = path.as_os_str().to_owned();
        path.push(OsString::from_wide(name));
        path.into()
    }

    fn copy_streams(source: &Path, target: &Path, cancel: &AtomicBool) -> io::Result<()> {
        let source = fs::canonicalize(source)?;
        let target = fs::canonicalize(target)?;
        let filename = wide(&source)?;
        let mut entry = WIN32_FIND_STREAM_DATA::default();
        let handle = unsafe {
            FindFirstStreamW(
                filename.as_ptr(),
                FindStreamInfoStandard,
                (&raw mut entry).cast(),
                0,
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            let error = io::Error::last_os_error();
            return if error.raw_os_error().is_some_and(|code| {
                matches!(
                    code as u32,
                    ERROR_HANDLE_EOF
                        | ERROR_INVALID_PARAMETER
                        | ERROR_INVALID_FUNCTION
                        | ERROR_NOT_SUPPORTED
                )
            }) {
                Ok(())
            } else {
                Err(error)
            };
        }
        let handle = SearchHandle(handle);
        let unnamed: Vec<_> = "::$DATA".encode_utf16().collect();
        loop {
            files::check_cancel(cancel)?;
            let end = entry
                .cStreamName
                .iter()
                .position(|&unit| unit == 0)
                .ok_or_else(|| io::Error::other("Der NTFS-Streamname ist unvollständig."))?;
            let name = &entry.cStreamName[..end];
            if name != unnamed {
                let expected = u64::try_from(entry.StreamSize).map_err(io::Error::other)?;
                let input = fs::OpenOptions::new()
                    .read(true)
                    .share_mode(1)
                    .open(stream_path(&source, name))?;
                let mut output = File::create(stream_path(&target, name))?;
                let copied = crate::tools::copy_cancelled(
                    input.take(expected.saturating_add(1)),
                    &mut output,
                    cancel,
                )?;
                if copied != expected {
                    return Err(io::Error::other(
                        "Ein NTFS-Stream hat sich beim Kopieren geändert.",
                    ));
                }
                output.sync_all()?;
            }
            if unsafe { FindNextStreamW(handle.0, (&raw mut entry).cast()) } == 0 {
                let error = io::Error::last_os_error();
                return if error.raw_os_error() == Some(ERROR_HANDLE_EOF as i32) {
                    Ok(())
                } else {
                    Err(error)
                };
            }
        }
    }

    fn set_sparse(file: &File) -> io::Result<()> {
        let mut bytes = 0;
        let ok = unsafe {
            DeviceIoControl(
                file.as_raw_handle(),
                FSCTL_SET_SPARSE,
                null(),
                0,
                null_mut(),
                0,
                &mut bytes,
                null_mut(),
            )
        };
        if ok == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    fn sparse_copy(
        input: &mut File,
        output: &mut File,
        length: u64,
        cancel: &AtomicBool,
        mut progress: impl FnMut(u64),
    ) -> io::Result<()> {
        set_sparse(output)?;
        output.set_len(length)?;
        let length = i64::try_from(length).map_err(io::Error::other)?;
        let mut buffer = vec![0; 256 * 1024];
        let mut ranges = [FILE_ALLOCATED_RANGE_BUFFER::default(); 256];
        let mut start = 0;
        let mut copied = 0;
        while start < length {
            files::check_cancel(cancel)?;
            let query = FILE_ALLOCATED_RANGE_BUFFER {
                FileOffset: start,
                Length: length - start,
            };
            let mut bytes = 0;
            let ok = unsafe {
                DeviceIoControl(
                    input.as_raw_handle(),
                    FSCTL_QUERY_ALLOCATED_RANGES,
                    (&raw const query).cast(),
                    std::mem::size_of::<FILE_ALLOCATED_RANGE_BUFFER>() as u32,
                    ranges.as_mut_ptr().cast(),
                    std::mem::size_of_val(&ranges) as u32,
                    &mut bytes,
                    null_mut(),
                )
            };
            if ok == 0 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() != Some(ERROR_MORE_DATA as i32) {
                    return Err(error);
                }
            }
            let count = bytes as usize / std::mem::size_of::<FILE_ALLOCATED_RANGE_BUFFER>();
            let previous = start;
            for range in ranges
                .get(..count)
                .ok_or_else(|| io::Error::other("Ungültige Sparse-Bereiche von Windows."))?
            {
                let beginning = range.FileOffset.max(start);
                let end = range.FileOffset.saturating_add(range.Length).min(length);
                if beginning >= end {
                    continue;
                }
                input.seek(SeekFrom::Start(beginning as u64))?;
                output.seek(SeekFrom::Start(beginning as u64))?;
                let mut remaining = (end - beginning) as u64;
                while remaining > 0 {
                    files::check_cancel(cancel)?;
                    let count = remaining.min(buffer.len() as u64) as usize;
                    input.read_exact(&mut buffer[..count])?;
                    output.write_all(&buffer[..count])?;
                    remaining -= count as u64;
                    copied += count as u64;
                    progress(copied);
                }
                start = end;
            }
            if ok != 0 {
                break;
            }
            if start <= previous {
                return Err(io::Error::other(
                    "Windows lieferte keine weiteren Sparse-Bereiche.",
                ));
            }
        }
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::os::windows::fs::MetadataExt;

        #[test]
        fn native_transfer_preserves_named_stream_sparse_file_and_hardlink_group() {
            use windows_sys::Win32::Security::{
                DACL_SECURITY_INFORMATION, EqualSid, GetSecurityDescriptorControl,
                OWNER_SECURITY_INFORMATION, SE_DACL_PROTECTED,
            };
            let fixture = tempfile::tempdir().unwrap();
            let source = fixture.path().join("source");
            let target = fixture.path().join("target");
            fs::create_dir(&source).unwrap();
            fs::create_dir(&target).unwrap();
            let a = source.join("a");
            let b = source.join("b");
            let mut original = File::create(&a).unwrap();
            set_sparse(&original).unwrap();
            original.seek(SeekFrom::Start(4 * 1024 * 1024)).unwrap();
            original.write_all(b"tail").unwrap();
            drop(original);
            let name: Vec<_> = ":Kommentar:$DATA".encode_utf16().collect();
            fs::write(stream_path(&a, &name), b"stream").unwrap();
            fs::hard_link(&a, &b).unwrap();
            let fields = DACL_SECURITY_INFORMATION | OWNER_SECURITY_INFORMATION;
            let original = security::Descriptor::read(&File::open(&a).unwrap(), fields).unwrap();
            let original_directory =
                security::Descriptor::read(&security::open(&source, READ_CONTROL).unwrap(), fields)
                    .unwrap();
            let report = files::transfer(
                &[source],
                &target,
                files::Transfer::Copy,
                files::TransferPolicy {
                    ntfs: NtfsCopyOptions {
                        streams: true,
                        permissions: true,
                        owner: true,
                        hardlinks: true,
                        sparse: true,
                        ..Default::default()
                    },
                    ..files::Conflict::Fail.into()
                },
                &AtomicBool::new(false),
                None,
                |_| {},
            )
            .unwrap();
            assert!(report.errors.is_empty(), "{report:?}");
            assert_eq!(report.completed, 1);
            let target = target.join("source");
            let directory =
                security::Descriptor::read(&security::open(&target, READ_CONTROL).unwrap(), fields)
                    .unwrap();
            assert_ne!(
                unsafe { EqualSid(original_directory.owner, directory.owner) },
                0
            );
            let (a, b) = (target.join("a"), target.join("b"));
            let mut file = File::open(&a).unwrap();
            assert_ne!(
                file.metadata().unwrap().file_attributes() & FILE_ATTRIBUTE_SPARSE_FILE,
                0
            );
            assert_eq!(
                file_id(&file).unwrap(),
                file_id(&File::open(b).unwrap()).unwrap()
            );
            assert_eq!(fs::read(stream_path(&a, &name)).unwrap(), b"stream");
            let descriptor = security::Descriptor::read(&file, fields).unwrap();
            assert_ne!(unsafe { EqualSid(original.owner, descriptor.owner) }, 0);
            assert!(!original.dacl.is_null() && !descriptor.dacl.is_null());
            assert_eq!(unsafe { (*descriptor.dacl).AceCount }, unsafe {
                (*original.dacl).AceCount
            });
            let mut control = 0;
            let mut revision = 0;
            assert_ne!(
                unsafe {
                    GetSecurityDescriptorControl(descriptor.raw, &mut control, &mut revision)
                },
                0
            );
            assert_ne!(control & SE_DACL_PROTECTED, 0);
            let mut first = [1u8; 16];
            file.read_exact(&mut first).unwrap();
            assert_eq!(first, [0; 16]);
            file.seek(SeekFrom::Start(4 * 1024 * 1024)).unwrap();
            let mut tail = Vec::new();
            file.read_to_end(&mut tail).unwrap();
            assert_eq!(tail, b"tail");
        }
    }
}
