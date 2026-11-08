use super::*;
use std::ffi::{OsStr, OsString};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::os::windows::io::AsRawHandle;
use std::process::Command;
use std::ptr::{null, null_mut};
use windows_sys::Win32::Foundation::{GlobalFree, INVALID_HANDLE_VALUE, POINT};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_COMPRESSED, FILE_ATTRIBUTE_REPARSE_POINT, FILE_ATTRIBUTE_SPARSE_FILE,
    FILE_BASIC_INFO, FILE_COMPRESSION_INFO, FILE_FLAG_BACKUP_SEMANTICS,
    FILE_FLAG_OPEN_REPARSE_POINT, FILE_ID_INFO, FILE_INFO_BY_HANDLE_CLASS, FILE_READ_ATTRIBUTES,
    FILE_STANDARD_INFO, FILE_WRITE_ATTRIBUTES, FileBasicInfo, FileCompressionInfo, FileIdInfo,
    FileStandardInfo, GetDiskFreeSpaceExW, GetFileInformationByHandleEx, GetLogicalDrives,
    MoveFileExW, SetFileInformationByHandle,
};
use windows_sys::Win32::System::Console::{
    ATTACH_PARENT_PROCESS, AttachConsole, GetStdHandle, STD_OUTPUT_HANDLE,
};
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, RegisterClipboardFormatW,
    SetClipboardData,
};
use windows_sys::Win32::System::Memory::{
    GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetActiveWindow, GetDoubleClickTime};
use windows_sys::Win32::UI::Shell::{
    DragQueryFileW, SEE_MASK_INVOKEIDLIST, SHELLEXECUTEINFOW, ShellExecuteExW, ShellExecuteW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetCursorPos, GetSystemMetrics, SM_CXDOUBLECLK, SM_CYDOUBLECLK, SW_SHOWNORMAL,
};

const CF_UNICODETEXT: u32 = 13;
const CF_HDROP: u32 = 15;
const COPY_EFFECT: u32 = 1;
const MOVE_EFFECT: u32 = 2;

pub fn attach_parent_console_for_help() {
    // GUI launches stay detached. Preserve an existing redirected output handle.
    let output = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
    if output.is_null() || output == INVALID_HANDLE_VALUE {
        unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
    }
}

pub(crate) fn wide(value: impl AsRef<OsStr>) -> io::Result<Vec<u16>> {
    let mut wide: Vec<_> = value.as_ref().encode_wide().collect();
    if wide.contains(&0) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "embedded NUL"));
    }
    wide.push(0);
    Ok(wide)
}

pub fn open(path: &Path) -> io::Result<()> {
    let path = wide(path)?;
    // The path is UTF-16 and null-terminated for the lifetime of this call.
    let result = unsafe {
        ShellExecuteW(
            null_mut(),
            null(),
            path.as_ptr(),
            null(),
            null(),
            SW_SHOWNORMAL,
        )
    } as isize;
    if result > 32 {
        Ok(())
    } else {
        Err(io::Error::other(format!("Windows Shell error {result}")))
    }
}

pub fn cursor_screen() -> Option<(i32, i32)> {
    let mut point = POINT::default();
    // GetCursorPos writes one POINT in screen coordinates.
    (unsafe { GetCursorPos(&mut point) } != 0).then_some((point.x, point.y))
}

pub fn is_double_click(
    first: (i32, i32),
    second: (i32, i32),
    elapsed: std::time::Duration,
) -> bool {
    let (time, width, height) = unsafe {
        (
            GetDoubleClickTime(),
            GetSystemMetrics(SM_CXDOUBLECLK),
            GetSystemMetrics(SM_CYDOUBLECLK),
        )
    };
    elapsed <= std::time::Duration::from_millis(u64::from(time))
        && first.0.abs_diff(second.0) <= (width.max(0) / 2) as u32
        && first.1.abs_diff(second.1) <= (height.max(0) / 2) as u32
}

pub fn properties(path: &Path) -> io::Result<()> {
    let path = wide(path)?;
    let verb = wide("properties")?;
    let mut request = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_INVOKEIDLIST,
        lpVerb: verb.as_ptr(),
        lpFile: path.as_ptr(),
        nShow: SW_SHOWNORMAL,
        ..Default::default()
    };
    // All input buffers outlive the synchronous ShellExecuteExW call.
    if unsafe { ShellExecuteExW(&mut request) } != 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

pub fn edit(path: &Path, editor: Option<&Path>) -> io::Result<()> {
    let default_editor = system_program("notepad.exe")?;
    Command::new(editor.unwrap_or(&default_editor))
        .arg(path)
        .spawn()
        .map(|_| ())
}

pub fn terminal(path: &Path) -> io::Result<()> {
    Command::new(system_program("cmd.exe")?)
        .current_dir(path)
        .spawn()
        .map(|_| ())
}

pub fn system_information(version_only: bool) -> io::Result<()> {
    let name = if version_only {
        "winver.exe"
    } else {
        "msinfo32.exe"
    };
    Command::new(system_program(name)?).spawn().map(|_| ())
}

fn system_program(name: &str) -> io::Result<PathBuf> {
    let root =
        std::env::var_os("SystemRoot").ok_or_else(|| io::Error::other("SystemRoot is missing"))?;
    Ok(PathBuf::from(root).join("System32").join(name))
}

pub fn recycle(path: &Path) -> io::Result<()> {
    trash::delete(path).map_err(io::Error::other)
}

pub fn rename_noreplace(source: &Path, target: &Path) -> io::Result<()> {
    let source = wide(extended_path(source)?)?;
    let target = wide(extended_path(target)?)?;
    // Flags zero prevent replacing another entry and disable copy across volumes.
    let result = unsafe { MoveFileExW(source.as_ptr(), target.as_ptr(), 0) };
    if result != 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

pub fn file_allocation(path: &Path) -> io::Result<FileAllocation> {
    let file = std::fs::OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    let attributes = file.metadata()?.file_attributes();
    if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(io::Error::other(
            "Datei wurde durch eine Verknüpfung ersetzt.",
        ));
    }
    let standard: FILE_STANDARD_INFO = query_file_info(&file, FileStandardInfo)?;
    if standard.Directory || standard.DeletePending {
        return Err(io::Error::other(
            "Datei steht für die Größenabfrage nicht mehr bereit.",
        ));
    }
    let allocated = if attributes & (FILE_ATTRIBUTE_COMPRESSED | FILE_ATTRIBUTE_SPARSE_FILE) != 0 {
        let compression: FILE_COMPRESSION_INFO = query_file_info(&file, FileCompressionInfo)?;
        compression.CompressedFileSize
    } else {
        standard.AllocationSize
    };
    let hardlink_id = if standard.NumberOfLinks > 1 {
        let info: FILE_ID_INFO = query_file_info(&file, FileIdInfo)?;
        Some((info.VolumeSerialNumber, info.FileId.Identifier))
    } else {
        None
    };
    Ok(FileAllocation {
        bytes: u64::try_from(allocated).map_err(io::Error::other)?,
        hardlink_id,
    })
}

fn query_file_info<T: Default>(
    file: &std::fs::File,
    class: FILE_INFO_BY_HANDLE_CLASS,
) -> io::Result<T> {
    let mut info = T::default();
    let ok = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            class,
            (&raw mut info).cast(),
            std::mem::size_of::<T>() as u32,
        )
    };
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(info)
    }
}

pub fn edit_metadata(path: &Path, edit: MetadataEdit) -> io::Result<()> {
    let file = std::fs::OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES | FILE_WRITE_ATTRIBUTES)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    edit_file_metadata(&file, edit)
}

pub fn edit_file_metadata(file: &std::fs::File, edit: MetadataEdit) -> io::Result<()> {
    const EDITABLE: u32 = 1 | 2 | 4 | 32;
    const NORMAL: u32 = 128;
    if (edit.set | edit.clear) & !EDITABLE != 0 || edit.set & edit.clear != 0 {
        return Err(io::Error::other("Ungültige Attributänderung."));
    }
    let old = query_file_info::<FILE_BASIC_INFO>(file, FileBasicInfo)?;
    if old.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(io::Error::other(
            "Attribute von Verknüpfungen werden übersprungen.",
        ));
    }
    let flags = if edit.set | edit.clear == 0 {
        0
    } else {
        let value = (old.FileAttributes | edit.set) & !(edit.clear | NORMAL);
        if value == 0 { NORMAL } else { value }
    };
    let information = FILE_BASIC_INFO {
        CreationTime: windows_ticks(edit.created)?,
        LastAccessTime: windows_ticks(edit.accessed)?,
        LastWriteTime: windows_ticks(edit.modified)?,
        ChangeTime: 0,
        FileAttributes: flags,
    };
    let ok = unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle(),
            FileBasicInfo,
            (&raw const information).cast(),
            std::mem::size_of::<FILE_BASIC_INFO>() as u32,
        )
    };
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn windows_ticks(time: Option<SystemTime>) -> io::Result<i64> {
    let Some(time) = time else { return Ok(0) };
    let nanos = match time.duration_since(std::time::UNIX_EPOCH) {
        Ok(duration) => duration.as_nanos() as i128,
        Err(error) => -(error.duration().as_nanos() as i128),
    };
    let ticks = (11_644_473_600_000_000_000i128 + nanos) / 100;
    i64::try_from(ticks)
        .ok()
        .filter(|&time| time > 0)
        .ok_or_else(|| io::Error::other("Der Zeitpunkt liegt außerhalb des Windows-Zeitbereichs."))
}

#[cfg(test)]
mod metadata_tests {
    use super::*;
    use std::os::windows::fs::MetadataExt;
    use std::time::{Duration, UNIX_EPOCH};

    #[test]
    fn edits_windows_attributes_and_timestamps_on_a_file_and_directory() {
        let fixture = tempfile::tempdir().unwrap();
        let file = fixture.path().join("file");
        std::fs::write(&file, b"data").unwrap();
        let created = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let modified = created + Duration::from_secs(300);
        let accessed = modified + Duration::from_secs(300);
        for path in [file.as_path(), fixture.path()] {
            let handle = std::fs::OpenOptions::new()
                .access_mode(FILE_READ_ATTRIBUTES | FILE_WRITE_ATTRIBUTES)
                .share_mode(0)
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
                .open(path)
                .unwrap();
            edit_file_metadata(
                &handle,
                MetadataEdit {
                    set: 2,
                    clear: 32,
                    created: Some(created),
                    modified: Some(modified),
                    accessed: Some(accessed),
                },
            )
            .unwrap();
            // NTFS may update access time again once other readers can open the entry.
            let metadata = query_file_info::<FILE_BASIC_INFO>(&handle, FileBasicInfo).unwrap();
            assert_eq!(metadata.FileAttributes & (2 | 32), 2);
            assert_eq!(metadata.CreationTime, windows_ticks(Some(created)).unwrap());
            assert_eq!(
                metadata.LastWriteTime,
                windows_ticks(Some(modified)).unwrap()
            );
            assert_eq!(
                metadata.LastAccessTime,
                windows_ticks(Some(accessed)).unwrap()
            );
            drop(handle);
            edit_metadata(
                path,
                MetadataEdit {
                    clear: 2,
                    ..Default::default()
                },
            )
            .unwrap();
            assert_eq!(std::fs::metadata(path).unwrap().file_attributes() & 2, 0);
        }
    }
}

pub(crate) fn extended_path(path: &Path) -> io::Result<PathBuf> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("root cannot be renamed"))?;
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::other("missing file name"))?;
    Ok(std::fs::canonicalize(parent)?.join(name))
}

pub fn drives() -> Vec<PathBuf> {
    // GetLogicalDrives takes no pointers and returns the drive-letter bitmask.
    let mask = unsafe { GetLogicalDrives() };
    (0..26)
        .filter(|index| mask & (1 << index) != 0)
        .map(|index| PathBuf::from(format!("{}:\\", char::from(b'A' + index))))
        .collect()
}

pub fn disk_space(path: &Path) -> Option<crate::DiskSpace> {
    let path = wide(path).ok()?;
    let mut available = 0;
    let mut total = 0;
    let success =
        unsafe { GetDiskFreeSpaceExW(path.as_ptr(), &mut available, &mut total, null_mut()) };
    (success != 0).then_some(crate::DiskSpace { available, total })
}

struct Clipboard;

impl Clipboard {
    fn open(write: bool) -> io::Result<Self> {
        let owner = if write {
            unsafe { GetActiveWindow() }
        } else {
            null_mut()
        };
        if write && owner.is_null() {
            return Err(io::Error::other("no active clipboard owner window"));
        }
        if unsafe { OpenClipboard(owner) } != 0 {
            Ok(Self)
        } else {
            Err(io::Error::last_os_error())
        }
    }

    fn clear(&self) -> io::Result<()> {
        if unsafe { EmptyClipboard() } != 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    fn set(&self, format: u32, bytes: &[u8]) -> io::Result<()> {
        // The clipboard takes ownership of this movable allocation only on success.
        let memory = unsafe { GlobalAlloc(GMEM_MOVEABLE, bytes.len()) };
        if memory.is_null() {
            return Err(io::Error::last_os_error());
        }
        let target = unsafe { GlobalLock(memory) }.cast::<u8>();
        if target.is_null() {
            let error = io::Error::last_os_error();
            unsafe { GlobalFree(memory) };
            return Err(error);
        }
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), target, bytes.len());
            GlobalUnlock(memory);
        }
        if unsafe { SetClipboardData(format, memory) }.is_null() {
            let error = io::Error::last_os_error();
            unsafe { GlobalFree(memory) };
            return Err(error);
        }
        Ok(())
    }
}

impl Drop for Clipboard {
    fn drop(&mut self) {
        unsafe { CloseClipboard() };
    }
}

pub fn copy_text(text: &str) -> io::Result<()> {
    let bytes: Vec<_> = wide(text)?.into_iter().flat_map(u16::to_le_bytes).collect();
    let clipboard = Clipboard::open(true)?;
    clipboard.clear()?;
    clipboard.set(CF_UNICODETEXT, &bytes)
}

pub fn paste_text() -> io::Result<String> {
    let _clipboard = Clipboard::open(false)?;
    let memory = unsafe { GetClipboardData(CF_UNICODETEXT) };
    if memory.is_null() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "Die Zwischenablage enthält keinen Text.",
        ));
    }
    let size = unsafe { GlobalSize(memory) };
    if !(2..=16 * 1024 * 1024).contains(&size) {
        return Err(io::Error::other("Ungültige Größe des Zwischenablagetexts."));
    }
    let pointer = unsafe { GlobalLock(memory) }.cast::<u16>();
    if pointer.is_null() {
        return Err(io::Error::last_os_error());
    }
    // The clipboard remains open and the allocation locked during the read.
    let data = unsafe { std::slice::from_raw_parts(pointer, size / 2) };
    let text = data
        .iter()
        .position(|&character| character == 0)
        .ok_or_else(|| io::Error::other("Der Zwischenablagetext ist unvollständig."))
        .and_then(|end| String::from_utf16(&data[..end]).map_err(io::Error::other));
    unsafe { GlobalUnlock(memory) };
    text
}

pub fn copy_paths(paths: &[PathBuf], cut: bool) -> io::Result<()> {
    if paths.is_empty() {
        return Err(io::Error::other("no files selected"));
    }
    let mut dropfiles = vec![0; 20];
    dropfiles[..4].copy_from_slice(&20u32.to_le_bytes()); // DROPFILES.pFiles
    dropfiles[16..20].copy_from_slice(&1u32.to_le_bytes()); // DROPFILES.fWide
    for path in paths {
        dropfiles.extend(wide(path)?.into_iter().flat_map(u16::to_le_bytes));
    }
    dropfiles.extend([0, 0]);
    let format = effect_format()?;
    let clipboard = Clipboard::open(true)?;
    clipboard.clear()?;
    clipboard.set(CF_HDROP, &dropfiles)?;
    clipboard.set(
        format,
        &(if cut { MOVE_EFFECT } else { COPY_EFFECT }).to_le_bytes(),
    )
}

fn effect_format() -> io::Result<u32> {
    let name = wide("Preferred DropEffect")?;
    let format = unsafe { RegisterClipboardFormatW(name.as_ptr()) };
    if format == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(format)
    }
}

pub fn paste_paths() -> io::Result<(Vec<PathBuf>, bool)> {
    let _clipboard = Clipboard::open(false)?;
    let drop = unsafe { GetClipboardData(CF_HDROP) };
    if drop.is_null() {
        return Err(io::Error::other("clipboard contains no file paths"));
    }
    let count = unsafe { DragQueryFileW(drop, u32::MAX, null_mut(), 0) };
    let mut paths = Vec::new();
    for index in 0..count {
        let size = unsafe { DragQueryFileW(drop, index, null_mut(), 0) };
        if size == 0 || size > 32_767 {
            return Err(io::Error::other("invalid clipboard path size"));
        }
        let mut buffer = vec![0; (size + 1) as usize];
        let written = unsafe { DragQueryFileW(drop, index, buffer.as_mut_ptr(), size + 1) };
        if written != size {
            return Err(io::Error::other("invalid clipboard path"));
        }
        paths.push(PathBuf::from(OsString::from_wide(&buffer[..size as usize])));
    }
    let effect = unsafe { GetClipboardData(effect_format()?) };
    let cut = if !effect.is_null() && unsafe { GlobalSize(effect) } >= std::mem::size_of::<u32>() {
        let ptr = unsafe { GlobalLock(effect) }.cast::<u32>();
        if ptr.is_null() {
            false
        } else {
            let value = unsafe { ptr.read_unaligned() };
            unsafe { GlobalUnlock(effect) };
            value == MOVE_EFFECT
        }
    } else {
        false
    };
    Ok((paths, cut))
}
