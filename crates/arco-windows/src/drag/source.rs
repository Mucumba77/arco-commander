use crate::platform::{query_file_info, wide};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::path::{Component, Path, PathBuf, Prefix};
use std::ptr::null_mut;
use windows_sys::Win32::Storage::FileSystem::{
    DELETE, FILE_ATTRIBUTE_READONLY, FILE_ATTRIBUTE_REPARSE_POINT, FILE_BASIC_INFO,
    FILE_DISPOSITION_INFO, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_ID_INFO,
    FILE_LIST_DIRECTORY, FILE_NAME_OPENED, FILE_READ_ATTRIBUTES, FILE_READ_DATA, FILE_SHARE_DELETE,
    FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_STANDARD_INFO, FileBasicInfo, FileDispositionInfo,
    FileIdInfo, FileStandardInfo, GetFinalPathNameByHandleW, GetVolumeInformationByHandleW,
    GetVolumeNameForVolumeMountPointW, SetFileInformationByHandle, VOLUME_NAME_GUID,
};

// Keep the cost and number of handles bounded for a large GUI selection.
const MAX_CLASSIC_MOVE_SOURCES: usize = 256;
const MAX_CLASSIC_MOVE_DIRECTORIES: usize = 256;

pub(super) struct Sources {
    files: Vec<Source>,
    roots: BTreeMap<PathBuf, PathBuf>,
    directories: BTreeMap<PathBuf, File>,
}

struct Source {
    _pin: File,
    path: PathBuf,
    identity: Identity,
}

#[derive(PartialEq)]
struct Identity {
    volume: u64,
    file_id: [u8; 16],
    changed: i64,
    written: i64,
    length: i64,
    attributes: u32,
}

impl Sources {
    pub(super) fn capture(paths: &[PathBuf]) -> io::Result<Self> {
        if paths.len() > MAX_CLASSIC_MOVE_SOURCES {
            return Err(unsupported());
        }
        let mut roots = BTreeMap::new();
        let mut directories = BTreeMap::new();
        let mut files = Vec::with_capacity(paths.len());
        for path in paths {
            let parent = path.parent().ok_or_else(unsupported)?;
            let stable_parent = protect_parent(parent, &mut roots, &mut directories)?;
            let name = path.file_name().ok_or_else(unsupported)?;
            if name.encode_wide().any(|unit| unit == u16::from(b':')) {
                return Err(unsupported());
            }
            let path = stable_parent.join(name);
            // Read access participates in sharing checks and excludes writers.
            // Share delete so a target can still do its own optimized move.
            let file = OpenOptions::new()
                .access_mode(FILE_READ_ATTRIBUTES | FILE_READ_DATA)
                .share_mode(FILE_SHARE_READ | FILE_SHARE_DELETE)
                .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
                .open(&path)?;
            let identity = Identity::read(&file)?;
            files.push(Source {
                _pin: file,
                path,
                identity,
            });
        }
        Ok(Self {
            files,
            roots,
            directories,
        })
    }

    pub(super) fn complete(self) -> usize {
        if !self.roots.iter().all(|(root, guid)| same_mount(root, guid))
            || !self
                .directories
                .values()
                .all(|directory| check_directory(directory).is_ok())
        {
            return 0;
        }
        // An optimized target may need write access to the parent to move a
        // child. Block parent writes only now, after the classical drop.
        let parents = self
            .directories
            .keys()
            .map(|directory| lock_directory(directory, FILE_SHARE_READ))
            .collect::<io::Result<Vec<_>>>();
        let Ok(_parents) = parents else { return 0 };
        // Acquire every delete handle and verify every identity before making
        // changes. Holding these handles closes the check/rename race.
        let handles = self
            .files
            .iter()
            .map(Source::open_for_delete)
            .collect::<io::Result<Vec<_>>>();
        let Ok(handles) = handles else { return 0 };
        let mut completed = 0;
        for handle in handles {
            let disposition = FILE_DISPOSITION_INFO { DeleteFile: true };
            let marked = unsafe {
                SetFileInformationByHandle(
                    handle.as_raw_handle(),
                    FileDispositionInfo,
                    (&raw const disposition).cast(),
                    size_of::<FILE_DISPOSITION_INFO>() as u32,
                )
            };
            if marked == 0 {
                break;
            }
            completed += 1;
        }
        drop(self);
        completed
    }
}

impl Source {
    fn open_for_delete(&self) -> io::Result<File> {
        let file = OpenOptions::new()
            .access_mode(DELETE | FILE_READ_ATTRIBUTES)
            .share_mode(FILE_SHARE_READ)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(&self.path)?;
        if self.identity != Identity::read(&file)? {
            return Err(io::Error::other("Die Drag-Quelle wurde geändert."));
        }
        Ok(file)
    }
}

impl Identity {
    fn read(file: &File) -> io::Result<Self> {
        let basic: FILE_BASIC_INFO = query_file_info(file, FileBasicInfo)?;
        let standard: FILE_STANDARD_INFO = query_file_info(file, FileStandardInfo)?;
        if basic.FileAttributes & (FILE_ATTRIBUTE_REPARSE_POINT | FILE_ATTRIBUTE_READONLY) != 0
            || standard.Directory
            || standard.DeletePending
            || standard.NumberOfLinks != 1
            || basic.ChangeTime <= 0
        {
            return Err(unsupported());
        }
        let id: FILE_ID_INFO = query_file_info(file, FileIdInfo)?;
        Ok(Self {
            volume: id.VolumeSerialNumber,
            file_id: id.FileId.Identifier,
            changed: basic.ChangeTime,
            written: basic.LastWriteTime,
            length: standard.EndOfFile,
            attributes: basic.FileAttributes,
        })
    }
}

fn protect_parent(
    path: &Path,
    roots: &mut BTreeMap<PathBuf, PathBuf>,
    directories: &mut BTreeMap<PathBuf, File>,
) -> io::Result<PathBuf> {
    let mut parts = path.components();
    if !matches!(parts.next(), Some(Component::Prefix(prefix))
        if matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_)))
        || parts.next() != Some(Component::RootDir)
    {
        return Err(unsupported());
    }
    let root = path.ancestors().last().ok_or_else(unsupported)?;
    let mut stable = if let Some(stable) = roots.get(root) {
        stable.clone()
    } else {
        if directories.len() >= MAX_CLASSIC_MOVE_DIRECTORIES {
            return Err(unsupported());
        }
        let handle = lock_directory(root, FILE_SHARE_READ | FILE_SHARE_WRITE)?;
        let stable = ntfs_volume_root(&handle)?;
        if !same_mount(root, &stable) {
            return Err(unsupported());
        }
        roots.insert(root.to_owned(), stable.clone());
        directories.entry(stable.clone()).or_insert(handle);
        stable
    };
    for part in parts {
        let Component::Normal(name) = part else {
            return Err(unsupported());
        };
        if name.encode_wide().any(|unit| unit == u16::from(b':')) {
            return Err(unsupported());
        }
        stable.push(name);
        if !directories.contains_key(&stable) {
            if directories.len() >= MAX_CLASSIC_MOVE_DIRECTORIES {
                return Err(unsupported());
            }
            let directory = lock_directory(&stable, FILE_SHARE_READ | FILE_SHARE_WRITE)?;
            directories.insert(stable.clone(), directory);
        }
    }
    Ok(stable)
}

fn same_mount(root: &Path, expected: &Path) -> bool {
    let Ok(root) = wide(root) else { return false };
    let mut buffer = [0u16; 64];
    let found = unsafe {
        GetVolumeNameForVolumeMountPointW(root.as_ptr(), buffer.as_mut_ptr(), buffer.len() as u32)
    };
    found != 0
        && buffer
            .iter()
            .position(|unit| *unit == 0)
            .is_some_and(|end| {
                OsString::from_wide(&buffer[..end]).eq_ignore_ascii_case(expected.as_os_str())
            })
}

fn lock_directory(path: &Path, share: u32) -> io::Result<File> {
    let directory = OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES | FILE_LIST_DIRECTORY)
        .share_mode(share)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    check_directory(&directory)?;
    Ok(directory)
}

fn check_directory(directory: &File) -> io::Result<()> {
    let basic: FILE_BASIC_INFO = query_file_info(directory, FileBasicInfo)?;
    let standard: FILE_STANDARD_INFO = query_file_info(directory, FileStandardInfo)?;
    if basic.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
        || !standard.Directory
        || standard.DeletePending
    {
        return Err(unsupported());
    }
    Ok(())
}

fn ntfs_volume_root(root: &File) -> io::Result<PathBuf> {
    let handle = root.as_raw_handle();
    let mut filesystem = [0u16; 32];
    let result = unsafe {
        GetVolumeInformationByHandleW(
            handle,
            null_mut(),
            0,
            null_mut(),
            null_mut(),
            null_mut(),
            filesystem.as_mut_ptr(),
            filesystem.len() as u32,
        )
    };
    if result == 0 {
        return Err(io::Error::last_os_error());
    }
    if filesystem[..5] != [b'N' as u16, b'T' as u16, b'F' as u16, b'S' as u16, 0] {
        return Err(unsupported());
    }
    let mut path = [0u16; 64];
    let len = unsafe {
        GetFinalPathNameByHandleW(
            handle,
            path.as_mut_ptr(),
            path.len() as u32,
            VOLUME_NAME_GUID | FILE_NAME_OPENED,
        )
    } as usize;
    if len == 0 {
        return Err(io::Error::last_os_error());
    }
    if len >= path.len() {
        return Err(unsupported());
    }
    let path = OsString::from_wide(&path[..len]);
    // A volume GUID path pins the namespace independently of drive-letter
    // mappings. A SUBST root resolves below that root and is rejected.
    let is_volume_root = path.to_str().is_some_and(|name| {
        name.strip_prefix(r"\\?\Volume{")
            .and_then(|name| name.strip_suffix("}\\"))
            .is_some_and(|guid| {
                guid.len() == 36
                    && guid
                        .bytes()
                        .all(|byte| byte.is_ascii_hexdigit() || byte == b'-')
            })
    });
    if !is_volume_root {
        return Err(unsupported());
    }
    Ok(path.into())
}

fn unsupported() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "Die Drag-Quelle kann nicht sicher entfernt werden.",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn drag_sources_remove_regular_files_by_handle_and_preserve_other_files() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source.txt");
        let second = directory.path().join("second.txt");
        let copy = directory.path().join("copy.txt");
        fs::write(&source, b"source contents").unwrap();
        fs::write(&second, b"second contents").unwrap();
        let sources = Sources::capture(&[source.clone(), second.clone()]).unwrap();
        fs::copy(&source, &copy).unwrap();
        assert_eq!(sources.complete(), 2);
        assert!(!source.exists() && !second.exists());
        assert_eq!(fs::read(copy).unwrap(), b"source contents");
    }

    #[test]
    fn drag_sources_preserve_entire_selection_after_a_concurrent_replacement() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source.txt");
        let second = directory.path().join("second.txt");
        let old = directory.path().join("old.txt");
        fs::write(&source, b"original").unwrap();
        fs::write(&second, b"second").unwrap();
        let sources = Sources::capture(&[second.clone(), source.clone()]).unwrap();
        fs::rename(&source, &old).unwrap();
        fs::write(&source, b"replacement").unwrap();
        assert_eq!(sources.complete(), 0);
        assert_eq!(fs::read(source).unwrap(), b"replacement");
        assert_eq!(fs::read(old).unwrap(), b"original");
        assert_eq!(fs::read(second).unwrap(), b"second");
    }

    #[test]
    fn drag_sources_exclude_active_writers_and_protect_the_parent() {
        let root = tempfile::tempdir().unwrap();
        let parent = root.path().join("parent");
        fs::create_dir(&parent).unwrap();
        let source = parent.join("source.txt");
        fs::write(&source, b"original").unwrap();
        let writer = OpenOptions::new().write(true).open(&source).unwrap();
        assert!(Sources::capture(std::slice::from_ref(&source)).is_err());
        drop(writer);
        let sources = Sources::capture(std::slice::from_ref(&source)).unwrap();
        assert!(fs::rename(&parent, root.path().join("replacement")).is_err());
        assert!(OpenOptions::new().write(true).open(&source).is_err());
        // The initial pin allows writes to the parent. An existing writer
        // must prevent upgrading the parent locks for source cleanup.
        let _parent_writer = OpenOptions::new()
            .access_mode(windows_sys::Win32::Storage::FileSystem::FILE_ADD_FILE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(&parent)
            .unwrap();
        assert_eq!(sources.complete(), 0);
        assert_eq!(fs::read(source).unwrap(), b"original");
    }

    #[test]
    fn drag_sources_keep_hardlinks_and_files_below_a_junction() {
        use std::os::windows::process::CommandExt;

        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("target");
        let alias = root.path().join("alias");
        fs::create_dir(&target).unwrap();
        let file = target.join("selected.txt");
        let hardlink = root.path().join("hardlink.txt");
        fs::write(&file, b"original").unwrap();
        fs::hard_link(&file, &hardlink).unwrap();
        assert!(Sources::capture(std::slice::from_ref(&file)).is_err());
        fs::remove_file(hardlink).unwrap();

        let junction = std::process::Command::new("cmd.exe")
            .creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW)
            .args(["/d", "/v:off", "/c"])
            .env("ARCO_DRAG_TEST_ALIAS", &alias)
            .env("ARCO_DRAG_TEST_TARGET", &target)
            .raw_arg(r#"mklink /J "%ARCO_DRAG_TEST_ALIAS%" "%ARCO_DRAG_TEST_TARGET%""#)
            .output()
            .unwrap();
        assert!(junction.status.success(), "{junction:?}");
        assert!(Sources::capture(&[alias.join("selected.txt")]).is_err());
        assert_eq!(fs::read(file).unwrap(), b"original");
    }
}
