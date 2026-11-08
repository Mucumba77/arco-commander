use crate::platform::{extended_path, rename_noreplace, wide};
use std::{io, path::Path};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
    CoUninitialize, IPersistFile,
};
use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
use windows::core::{Interface, PCWSTR};
use windows_sys::Win32::Foundation::{ERROR_CANCELLED, RPC_E_CHANGED_MODE};
use windows_sys::Win32::Storage::FileSystem::ReplaceFileW;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetActiveWindow;
use windows_sys::Win32::UI::Shell::{
    SEE_MASK_FLAG_NO_UI, SEE_MASK_INVOKEIDLIST, SHELLEXECUTEINFOW, ShellExecuteExW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

pub fn execute_verb(path: &Path, verb: &str) -> io::Result<()> {
    let path = wide(path)?;
    let verb = wide(verb)?;
    let mut request = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_INVOKEIDLIST | SEE_MASK_FLAG_NO_UI,
        hwnd: unsafe { GetActiveWindow() },
        lpVerb: verb.as_ptr(),
        lpFile: path.as_ptr(),
        nShow: SW_SHOWNORMAL,
        ..Default::default()
    };
    if unsafe { ShellExecuteExW(&mut request) } != 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(ERROR_CANCELLED as i32) {
        Ok(())
    } else {
        Err(error)
    }
}

pub fn parse_windows_arguments(arguments: &str) -> io::Result<Vec<String>> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::UI::Shell::CommandLineToArgvW;
    let command = wide(format!("arco {arguments}"))?;
    let mut count = 0;
    // The dummy executable keeps CommandLineToArgvW's first-word rules out of the arguments.
    let argv = unsafe { CommandLineToArgvW(command.as_ptr(), &mut count) };
    if argv.is_null() {
        return Err(io::Error::last_os_error());
    }
    let values = unsafe { std::slice::from_raw_parts(argv, count as usize) };
    let result = values
        .iter()
        .skip(1)
        .map(|argument| unsafe { PCWSTR(*argument).to_string() }.map_err(io::Error::other))
        .collect();
    unsafe { LocalFree(argv.cast()) };
    result
}

pub fn create_shortcut(source: &Path, target: &Path) -> io::Result<()> {
    if !source.is_absolute()
        || !target.is_absolute()
        || !target
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("lnk"))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Eine Verknüpfung benötigt absolute Pfade und die Endung .lnk.",
        ));
    }
    let directory = target
        .parent()
        .ok_or_else(|| io::Error::other("Der Zielordner fehlt."))?;
    let stage = tempfile::tempdir_in(directory)?;
    let staged = stage.path().join("shortcut.lnk");
    let source_name = wide(source)?;
    let staged_name = wide(&staged)?;
    let working = source.parent().map(wide).transpose()?;
    let apartment = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    if apartment.0 != RPC_E_CHANGED_MODE {
        apartment.ok().map_err(io::Error::other)?;
    }
    let result = (|| {
        let link: IShellLinkW =
            unsafe { CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)? };
        unsafe { link.SetPath(PCWSTR(source_name.as_ptr()))? };
        if let Some(working) = working {
            unsafe { link.SetWorkingDirectory(PCWSTR(working.as_ptr()))? };
        }
        let persist: IPersistFile = link.cast()?;
        unsafe { persist.Save(PCWSTR(staged_name.as_ptr()), true)? };
        Ok::<_, windows::core::Error>(())
    })();
    if apartment.is_ok() {
        unsafe { CoUninitialize() };
    }
    result.map_err(io::Error::other)?;
    rename_noreplace(&staged, target)
}

pub fn replace_preserving_attributes(
    staged: &Path,
    target: &Path,
    backup: &Path,
) -> io::Result<()> {
    let target_wide = wide(extended_path(target)?)?;
    let staged = wide(extended_path(staged)?)?;
    let backup_wide = wide(extended_path(backup)?)?;
    let success = unsafe {
        ReplaceFileW(
            target_wide.as_ptr(),
            staged.as_ptr(),
            backup_wide.as_ptr(),
            0,
            std::ptr::null(),
            std::ptr::null(),
        )
    };
    if success != 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if target
        .symlink_metadata()
        .is_err_and(|error| error.kind() == io::ErrorKind::NotFound)
        && backup.is_file()
    {
        rename_noreplace(backup, target).map_err(|restore| {
            io::Error::other(format!(
                "{error}. Sicherung konnte nicht wiederhergestellt werden: {restore}"
            ))
        })?;
    }
    Err(error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn replacement_keeps_ntfs_named_data_streams() {
        let fixture = tempfile::tempdir().unwrap();
        let staged = fixture.path().join("staged.txt");
        let target = fixture.path().join("target.txt");
        let backup = fixture.path().join("backup.txt");
        let stream = fixture.path().join("target.txt:arco-test");
        fs::write(&target, b"before").unwrap();
        fs::write(&stream, b"metadata").unwrap();
        fs::write(&staged, b"after").unwrap();
        replace_preserving_attributes(&staged, &target, &backup).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"after");
        assert_eq!(fs::read(&stream).unwrap(), b"metadata");
        assert_eq!(fs::read(&backup).unwrap(), b"before");
    }

    #[test]
    fn creates_a_shell_link_without_replacing_an_existing_link() {
        let fixture = tempfile::tempdir().unwrap();
        let source = fixture.path().join("Quelle ä.txt");
        let target = fixture.path().join("Ziel.lnk");
        fs::write(&source, b"content").unwrap();
        create_shortcut(&source, &target).unwrap();
        let link = fs::read(&target).unwrap();
        assert_eq!(link[..4], 76u32.to_le_bytes());
        assert!(create_shortcut(&source, &target).is_err());
        assert_eq!(fs::read(target).unwrap(), link);
    }
}
