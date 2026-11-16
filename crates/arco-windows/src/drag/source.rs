use crate::platform::{query_file_info, wide};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::path::{Component, Path, PathBuf, Prefix};
use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, DELETE, FILE_ATTRIBUTE_REPARSE_POINT, FILE_BASIC_INFO,
    FILE_DISPOSITION_FLAG_DELETE, FILE_DISPOSITION_FLAG_FORCE_IMAGE_SECTION_CHECK,
    FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE, FILE_DISPOSITION_FLAG_POSIX_SEMANTICS,
    FILE_DISPOSITION_INFO, FILE_DISPOSITION_INFO_EX, FILE_FLAG_BACKUP_SEMANTICS,
    FILE_FLAG_OPEN_REPARSE_POINT, FILE_ID_INFO, FILE_LIST_DIRECTORY, FILE_NAME_OPENED,
    FILE_READ_ATTRIBUTES, FILE_READ_DATA, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    FILE_STANDARD_INFO, FileBasicInfo, FileDispositionInfo, FileDispositionInfoEx, FileIdInfo,
    FileStandardInfo, GetFileInformationByHandle, GetFinalPathNameByHandleW,
    GetVolumeNameForVolumeMountPointW, SetFileInformationByHandle, VOLUME_NAME_GUID,
};

// Keep the cost and number of handles bounded for a large GUI selection.
const MAX_CLASSIC_MOVE_SOURCES: usize = 256;
const MAX_CLASSIC_MOVE_DIRECTORIES: usize = 256;
const MAX_CLASSIC_MOVE_NODES: usize = 16_384;

pub(super) struct Sources {
    files: Vec<Source>,
    roots: BTreeMap<PathBuf, PathBuf>,
    directories: BTreeMap<PathBuf, File>,
    selected: Vec<PathBuf>,
    exposed: Vec<PathBuf>,
}

struct Source {
    _pin: File,
    path: PathBuf,
    identity: Identity,
    children: Option<BTreeSet<OsString>>,
}

#[derive(Clone, PartialEq)]
struct Identity {
    volume: u64,
    file_id: [u8; 16],
    changed: i64,
    written: i64,
    length: i64,
    attributes: u32,
    created: i64,
    links: u32,
    directory: bool,
}

impl Sources {
    pub(super) fn capture(paths: &[PathBuf]) -> io::Result<Self> {
        if paths.len() > MAX_CLASSIC_MOVE_SOURCES {
            return Err(unsupported());
        }
        let mut roots = BTreeMap::new();
        let mut directories = BTreeMap::new();
        let mut files = Vec::with_capacity(paths.len());
        let mut selected = Vec::with_capacity(paths.len());
        let mut exposed = Vec::with_capacity(paths.len());
        let mut seen = BTreeSet::new();
        for path in paths {
            let parent = std::fs::canonicalize(path.parent().ok_or_else(unsupported)?)?;
            let stable_parent = protect_parent(&parent, &mut roots, &mut directories)?;
            let name = path.file_name().ok_or_else(unsupported)?;
            if name.encode_wide().any(|unit| unit == u16::from(b':')) {
                return Err(unsupported());
            }
            exposed.push(shell_path(parent.join(name)));
            let path = stable_parent.join(name);
            selected.push(path.clone());
            let mut pending = vec![path];
            while let Some(path) = pending.pop() {
                if !seen.insert(path.clone()) {
                    continue;
                }
                if files.len() >= MAX_CLASSIC_MOVE_NODES {
                    return Err(unsupported());
                }
                // Pin data against writers, while allowing an optimized target to
                // rename the selected tree. A reparse node is never traversed.
                let file = OpenOptions::new()
                    .access_mode(FILE_READ_ATTRIBUTES | FILE_READ_DATA)
                    .share_mode(FILE_SHARE_READ | FILE_SHARE_DELETE)
                    .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
                    .open(&path)?;
                let identity = Identity::read(&file)?;
                if identity.file_id == [0; 16] {
                    return Err(unsupported());
                }
                let children = (identity.directory
                    && identity.attributes & FILE_ATTRIBUTE_REPARSE_POINT == 0)
                    .then(|| child_names(&path))
                    .transpose()?;
                if let Some(children) = &children {
                    pending.extend(children.iter().rev().map(|name| path.join(name)));
                }
                files.push(Source {
                    _pin: file,
                    path,
                    identity,
                    children,
                });
            }
        }
        // Selected directories are already pinned with delete sharing. Keeping
        // an additional ancestor lock on them would prevent optimized moves.
        directories.retain(|path, _| {
            !files
                .iter()
                .any(|node| node.path == *path && node.children.is_some())
        });
        Ok(Self {
            files,
            roots,
            directories,
            selected,
            exposed,
        })
    }

    pub(super) fn exposed_paths(&self) -> &[PathBuf] {
        &self.exposed
    }

    pub(super) fn complete(mut self) -> usize {
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
            .iter()
            .map(|(path, original)| {
                let directory = lock_directory(path, FILE_SHARE_READ)?;
                let before = Identity::read(original)?;
                let now = Identity::read(&directory)?;
                if before.volume != now.volume || before.file_id != now.file_id {
                    return Err(unsupported());
                }
                Ok(directory)
            })
            .collect::<io::Result<Vec<_>>>();
        let Ok(_parents) = parents else { return 0 };
        // Validate the complete selection before deleting anything. Delete
        // handles are opened individually so selected hardlinks do not conflict
        // with one another's delete access. Each final handle excludes races.
        for source in &self.files {
            if source.open_for_delete(0, false).is_err()
                || source.children.as_ref().is_some_and(|expected| {
                    child_names(&source.path).as_ref().ok() != Some(expected)
                })
            {
                return 0;
            }
        }
        let mut removed = BTreeSet::new();
        let mut removed_links = BTreeMap::<(u64, [u8; 16]), u32>::new();
        while let Some(source) = self.files.pop() {
            let key = (source.identity.volume, source.identity.file_id);
            let aliases = removed_links.get(&key).copied().unwrap_or_default();
            let changed_directory = source.children.is_some()
                && removed
                    .iter()
                    .any(|path: &PathBuf| path.parent() == Some(source.path.as_path()));
            let Ok(handle) = source.open_for_delete(aliases, changed_directory) else {
                break;
            };
            if source.children.is_some()
                && child_names(&source.path).is_ok_and(|children| !children.is_empty())
            {
                break;
            }
            let path = source.path.clone();
            drop(source);
            if remove_by_handle(&handle).is_err() {
                break;
            }
            drop(handle);
            removed.insert(path);
            *removed_links.entry(key).or_default() += 1;
        }
        self.selected
            .iter()
            .filter(|path| removed.contains(*path))
            .count()
    }
}

impl Source {
    fn open_for_delete(&self, aliases_removed: u32, changed_directory: bool) -> io::Result<File> {
        let file = OpenOptions::new()
            .access_mode(DELETE | FILE_READ_ATTRIBUTES)
            .share_mode(FILE_SHARE_READ)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(&self.path)?;
        let current = Identity::read(&file)?;
        let mut expected = self.identity.clone();
        if changed_directory {
            expected.changed = current.changed;
            expected.written = current.written;
            expected.length = current.length;
        }
        if aliases_removed > 0 {
            expected.links = expected.links.saturating_sub(aliases_removed);
            expected.changed = current.changed;
        }
        if expected != current {
            return Err(io::Error::other("Die Drag-Quelle wurde geändert."));
        }
        Ok(file)
    }
}

impl Identity {
    fn read(file: &File) -> io::Result<Self> {
        let basic: FILE_BASIC_INFO = query_file_info(file, FileBasicInfo)?;
        let standard: FILE_STANDARD_INFO = query_file_info(file, FileStandardInfo)?;
        if standard.DeletePending {
            return Err(unsupported());
        }
        let (volume, file_id) = match query_file_info::<FILE_ID_INFO>(file, FileIdInfo) {
            Ok(id) => (id.VolumeSerialNumber, id.FileId.Identifier),
            Err(error) if matches!(error.raw_os_error(), Some(1 | 50 | 87)) => {
                let mut info = BY_HANDLE_FILE_INFORMATION::default();
                if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
                    return Err(io::Error::last_os_error());
                }
                let mut id = [0; 16];
                id[..8].copy_from_slice(
                    &((u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow))
                        .to_le_bytes(),
                );
                (u64::from(info.dwVolumeSerialNumber), id)
            }
            Err(error) => return Err(error),
        };
        Ok(Self {
            volume,
            file_id,
            changed: basic.ChangeTime,
            written: basic.LastWriteTime,
            length: standard.EndOfFile,
            attributes: basic.FileAttributes,
            created: basic.CreationTime,
            links: standard.NumberOfLinks,
            directory: standard.Directory,
        })
    }
}

fn child_names(path: &Path) -> io::Result<BTreeSet<OsString>> {
    let mut children = BTreeSet::new();
    for entry in std::fs::read_dir(path)? {
        if children.len() >= MAX_CLASSIC_MOVE_NODES {
            return Err(unsupported());
        }
        children.insert(entry?.file_name());
    }
    Ok(children)
}

fn shell_path(path: PathBuf) -> PathBuf {
    // The shell rejects ordinary \\?\ paths. Retain that prefix when removing
    // it would change Win32 name parsing (trailing dots/spaces or DOS devices).
    let ordinary = path.components().all(|part| match part {
        Component::Normal(name) => {
            let units: Vec<_> = name.encode_wide().collect();
            let stem = name
                .to_string_lossy()
                .split('.')
                .next()
                .unwrap_or_default()
                .to_uppercase();
            let device = matches!(
                stem.as_str(),
                "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
            ) || ((stem.starts_with("COM") || stem.starts_with("LPT"))
                && matches!(
                    stem.get(3..),
                    Some("1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³")
                ));
            !device && !matches!(units.last(), Some(32 | 46))
        }
        _ => true,
    });
    if !ordinary {
        return path;
    }
    let units: Vec<_> = path.as_os_str().encode_wide().collect();
    match path.components().next() {
        Some(Component::Prefix(prefix)) => match prefix.kind() {
            Prefix::VerbatimDisk(_) => OsString::from_wide(&units[4..]).into(),
            Prefix::VerbatimUNC(_, _) => {
                OsString::from_wide(&[vec![92, 92], units[8..].to_vec()].concat()).into()
            }
            _ => path,
        },
        _ => path,
    }
}

fn remove_by_handle(file: &File) -> io::Result<()> {
    let extended = FILE_DISPOSITION_INFO_EX {
        Flags: FILE_DISPOSITION_FLAG_DELETE
            | FILE_DISPOSITION_FLAG_POSIX_SEMANTICS
            | FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE
            | FILE_DISPOSITION_FLAG_FORCE_IMAGE_SECTION_CHECK,
    };
    if unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle(),
            FileDispositionInfoEx,
            (&raw const extended).cast(),
            size_of::<FILE_DISPOSITION_INFO_EX>() as u32,
        )
    } != 0
    {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if !matches!(error.raw_os_error(), Some(1 | 50 | 87)) {
        return Err(error);
    }
    let simple = FILE_DISPOSITION_INFO { DeleteFile: true };
    if unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle(),
            FileDispositionInfo,
            (&raw const simple).cast(),
            size_of::<FILE_DISPOSITION_INFO>() as u32,
        )
    } == 0
    {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn protect_parent(
    path: &Path,
    roots: &mut BTreeMap<PathBuf, PathBuf>,
    directories: &mut BTreeMap<PathBuf, File>,
) -> io::Result<PathBuf> {
    let mut parts = path.components();
    let disk = match parts.next() {
        Some(Component::Prefix(prefix)) => match prefix.kind() {
            Prefix::Disk(_) | Prefix::VerbatimDisk(_) => true,
            Prefix::UNC(_, _) | Prefix::VerbatimUNC(_, _) => false,
            _ => return Err(unsupported()),
        },
        _ => return Err(unsupported()),
    };
    if parts.next() != Some(Component::RootDir) {
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
        let stable = if disk {
            volume_root(&handle)?
        } else {
            root.to_path_buf()
        };
        if disk && !same_mount(root, &stable) {
            return Err(unsupported());
        }
        if disk {
            roots.insert(root.to_owned(), stable.clone());
        }
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

fn volume_root(root: &File) -> io::Result<PathBuf> {
    let handle = root.as_raw_handle();
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
    fn shell_paths_preserve_verbatim_name_semantics() {
        assert_eq!(
            shell_path(PathBuf::from(r"\\?\C:\folder\file.txt")),
            PathBuf::from(r"C:\folder\file.txt")
        );
        assert_eq!(
            shell_path(PathBuf::from(r"\\?\UNC\host\share\file.txt")),
            PathBuf::from(r"\\host\share\file.txt")
        );
        for path in [
            r"\\?\C:\folder.\file.txt",
            r"\\?\C:\folder\file. ",
            r"\\?\C:\folder\CON.txt",
            r"\\?\C:\folder\COM¹",
        ] {
            assert_eq!(shell_path(PathBuf::from(path)), PathBuf::from(path));
        }
    }

    #[test]
    #[ignore = "requires ARCO_DRAG_REFERENCE_DIRECTORY on the filesystem or SMB share to be tested"]
    fn classical_move_reference_on_another_filesystem_or_smb_share() {
        let base = PathBuf::from(
            std::env::var_os("ARCO_DRAG_REFERENCE_DIRECTORY").expect("reference directory"),
        );
        let fixture = tempfile::tempdir_in(base).unwrap();
        let source = fixture.path().join("source");
        fs::create_dir_all(source.join("nested/empty")).unwrap();
        let file = source.join("nested/Grüße.txt");
        fs::write(&file, b"reference").unwrap();
        let copied = tempfile::tempdir().unwrap();
        let sources = Sources::capture(std::slice::from_ref(&source)).unwrap();
        fs::copy(&file, copied.path().join("copy.txt")).unwrap();
        assert_eq!(sources.complete(), 1);
        assert!(!source.exists());
        assert_eq!(
            fs::read(copied.path().join("copy.txt")).unwrap(),
            b"reference"
        );
    }

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
    fn drag_sources_remove_only_the_selected_hardlink_and_resolve_junction_parents() {
        use std::os::windows::process::CommandExt;

        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("target");
        let alias = root.path().join("alias");
        fs::create_dir(&target).unwrap();
        let file = target.join("selected.txt");
        let hardlink = root.path().join("hardlink.txt");
        fs::write(&file, b"original").unwrap();
        fs::hard_link(&file, &hardlink).unwrap();
        let sources = Sources::capture(std::slice::from_ref(&file)).unwrap();
        assert_eq!(sources.complete(), 1);
        assert!(!file.exists());
        assert_eq!(fs::read(&hardlink).unwrap(), b"original");
        fs::rename(&hardlink, &file).unwrap();

        let junction = std::process::Command::new("cmd.exe")
            .creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW)
            .args(["/d", "/v:off", "/c"])
            .env("ARCO_DRAG_TEST_ALIAS", &alias)
            .env("ARCO_DRAG_TEST_TARGET", &target)
            .raw_arg(r#"mklink /J "%ARCO_DRAG_TEST_ALIAS%" "%ARCO_DRAG_TEST_TARGET%""#)
            .output()
            .unwrap();
        assert!(junction.status.success(), "{junction:?}");
        let sources = Sources::capture(&[alias.join("selected.txt")]).unwrap();
        assert_eq!(
            fs::canonicalize(&sources.exposed_paths()[0]).unwrap(),
            fs::canonicalize(&file).unwrap()
        );
        drop(sources);
        assert_eq!(fs::read(file).unwrap(), b"original");
    }

    #[test]
    fn drag_sources_complete_nested_directories_without_following_junction_children() {
        use std::os::windows::process::CommandExt;
        let fixture = tempfile::tempdir().unwrap();
        let source = fixture.path().join("source");
        let external = fixture.path().join("external");
        fs::create_dir_all(source.join("nested/empty")).unwrap();
        fs::create_dir(&external).unwrap();
        fs::write(source.join("nested/first.txt"), b"original").unwrap();
        fs::hard_link(source.join("nested/first.txt"), source.join("second.txt")).unwrap();
        fs::write(external.join("keep.txt"), b"outside").unwrap();
        let alias = source.join("link");
        let result = std::process::Command::new("cmd.exe")
            .creation_flags(0x0800_0000)
            .args(["/d", "/v:off", "/c"])
            .env("ARCO_DRAG_TEST_ALIAS", &alias)
            .env("ARCO_DRAG_TEST_TARGET", &external)
            .raw_arg(r#"mklink /J "%ARCO_DRAG_TEST_ALIAS%" "%ARCO_DRAG_TEST_TARGET%""#)
            .output()
            .unwrap();
        assert!(result.status.success(), "{result:?}");
        let sources = Sources::capture(std::slice::from_ref(&source)).unwrap();
        assert_eq!(sources.complete(), 1);
        assert!(!source.exists());
        assert_eq!(fs::read(external.join("keep.txt")).unwrap(), b"outside");
    }

    #[test]
    fn drag_sources_pin_descendants_and_release_everything_after_a_cancelled_drop() {
        let fixture = tempfile::tempdir().unwrap();
        let source = fixture.path().join("source");
        fs::create_dir(&source).unwrap();
        let file = source.join("original.txt");
        let other = fixture.path().join("other.txt");
        fs::write(&file, b"old").unwrap();
        fs::write(&other, b"keep").unwrap();
        let sources = Sources::capture(&[source.clone(), other.clone()]).unwrap();
        assert!(fs::rename(&file, source.join("renamed.txt")).is_err());
        assert!(fs::write(&file, b"replacement").is_err());
        drop(sources);
        assert_eq!(fs::read(&file).unwrap(), b"old");
        assert_eq!(fs::read(other).unwrap(), b"keep");
    }

    #[test]
    fn drag_sources_handle_two_readonly_hardlinks_and_reject_descendant_writers() {
        let fixture = tempfile::tempdir().unwrap();
        let first = fixture.path().join("first.txt");
        let second = fixture.path().join("second.txt");
        let outside = fixture.path().join("outside.txt");
        fs::write(&first, b"shared").unwrap();
        fs::hard_link(&first, &second).unwrap();
        fs::hard_link(&first, &outside).unwrap();
        let mut permissions = fs::metadata(&first).unwrap().permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&first, permissions).unwrap();
        let sources = Sources::capture(&[first.clone(), second.clone()]).unwrap();
        assert_eq!(sources.complete(), 2);
        assert!(!first.exists() && !second.exists());
        assert_eq!(fs::read(&outside).unwrap(), b"shared");
        assert!(fs::metadata(&outside).unwrap().permissions().readonly());
        let mut permissions = fs::metadata(&outside).unwrap().permissions();
        permissions.set_readonly(false);
        fs::set_permissions(&outside, permissions).unwrap();
        let directory = fixture.path().join("directory");
        fs::create_dir(&directory).unwrap();
        let file = directory.join("busy.txt");
        fs::write(&file, b"busy").unwrap();
        let _writer = OpenOptions::new().write(true).open(&file).unwrap();
        assert!(Sources::capture(&[directory]).is_err());
    }
}
