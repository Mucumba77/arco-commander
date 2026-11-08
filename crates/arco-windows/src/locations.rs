use crate::KnownFolder;
use std::{ffi::OsString, io, os::windows::ffi::OsStringExt, path::PathBuf, ptr};
use windows::Win32::System::Com::{
    COINIT_APARTMENTTHREADED, CoInitializeEx, CoTaskMemFree, CoUninitialize,
};
use windows::Win32::UI::Shell::{
    FOLDERID_CommonPrograms, FOLDERID_CommonStartMenu, FOLDERID_CommonStartup, FOLDERID_Desktop,
    FOLDERID_Documents, FOLDERID_Downloads, FOLDERID_Fonts, FOLDERID_LocalAppData, FOLDERID_Music,
    FOLDERID_Pictures, FOLDERID_Profile, FOLDERID_ProgramData, FOLDERID_Programs,
    FOLDERID_PublicDesktop, FOLDERID_PublicDocuments, FOLDERID_PublicPictures,
    FOLDERID_RoamingAppData, FOLDERID_StartMenu, FOLDERID_Startup, FOLDERID_Videos,
    KF_FLAG_DEFAULT, SHGetKnownFolderPath,
};
use windows_sys::Win32::Foundation::{ERROR_CANCELLED, RPC_E_CHANGED_MODE};
use windows_sys::Win32::NetworkManagement::WNet::{
    RESOURCETYPE_DISK, WNetConnectionDialog, WNetDisconnectDialog,
};
use windows_sys::Win32::System::Registry::{
    HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ,
    RRF_SUBKEY_WOW6432KEY, RegGetValueW,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetActiveWindow;

pub fn known_folder(folder: KnownFolder) -> io::Result<PathBuf> {
    let id = match folder {
        KnownFolder::Desktop => FOLDERID_Desktop,
        KnownFolder::Documents => FOLDERID_Documents,
        KnownFolder::Downloads => FOLDERID_Downloads,
        KnownFolder::Pictures => FOLDERID_Pictures,
        KnownFolder::Music => FOLDERID_Music,
        KnownFolder::Videos => FOLDERID_Videos,
        KnownFolder::Profile => FOLDERID_Profile,
    };
    known_folder_path(&id)
}

fn known_folder_path(id: &windows::core::GUID) -> io::Result<PathBuf> {
    let apartment = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    if apartment.0 != RPC_E_CHANGED_MODE {
        apartment.ok().map_err(io::Error::other)?;
    }
    let result = unsafe { SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None) }.map(|path| {
        let resolved = PathBuf::from(unsafe { path.to_hstring() }.to_os_string());
        unsafe { CoTaskMemFree(Some(path.0.cast())) };
        resolved
    });
    if apartment.is_ok() {
        unsafe { CoUninitialize() };
    }
    result.map_err(io::Error::other)
}

pub fn tc_favorite_folder(name: &str) -> Option<PathBuf> {
    let id = match name {
        "$APPDATA" => FOLDERID_RoamingAppData,
        "$COMMON_APPDATA" => FOLDERID_ProgramData,
        "$COMMON_DESKTOPDIRECTORY" => FOLDERID_PublicDesktop,
        "$COMMON_DOCUMENTS" => FOLDERID_PublicDocuments,
        "$COMMON_PICTURES" => FOLDERID_PublicPictures,
        "$COMMON_PROGRAMS" => FOLDERID_CommonPrograms,
        "$COMMON_STARTMENU" => FOLDERID_CommonStartMenu,
        "$COMMON_STARTUP" => FOLDERID_CommonStartup,
        "$DESKTOP" => FOLDERID_Desktop,
        "$FONTS" => FOLDERID_Fonts,
        "$LOCAL_APPDATA" => FOLDERID_LocalAppData,
        "$MYMUSIC" => FOLDERID_Music,
        "$MYPICTURES" => FOLDERID_Pictures,
        "$MYVIDEO" => FOLDERID_Videos,
        "$PERSONAL" => FOLDERID_Documents,
        "$PROGRAMS" => FOLDERID_Programs,
        "$STARTMENU" => FOLDERID_StartMenu,
        "$STARTUP" => FOLDERID_Startup,
        _ => return None,
    };
    known_folder_path(&id).ok()
}

pub fn total_commander_install() -> Option<PathBuf> {
    let key = windows_sys::w!(r"Software\Ghisler\Total Commander");
    let value = windows_sys::w!("InstallDir");
    let mut buffer = vec![0u16; 32768];
    for root in [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE] {
        for view in [0, RRF_SUBKEY_WOW6432KEY] {
            let mut bytes = (buffer.len() * size_of::<u16>()) as u32;
            let result = unsafe {
                RegGetValueW(
                    root,
                    key,
                    value,
                    view | RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ,
                    ptr::null_mut(),
                    buffer.as_mut_ptr().cast(),
                    &mut bytes,
                )
            };
            if result == 0 {
                let end = buffer.iter().position(|unit| *unit == 0)?;
                let path = PathBuf::from(OsString::from_wide(&buffer[..end]));
                if path.is_absolute() {
                    return Some(path);
                }
            }
        }
    }
    None
}

pub fn network_drive_dialog(connect: bool) -> io::Result<bool> {
    let owner = unsafe { GetActiveWindow() };
    if owner.is_null() {
        return Err(io::Error::other("Das aktive Arco-Fenster fehlt."));
    }
    let status = unsafe {
        if connect {
            WNetConnectionDialog(owner, RESOURCETYPE_DISK)
        } else {
            WNetDisconnectDialog(owner, RESOURCETYPE_DISK)
        }
    };
    match status {
        0 => Ok(true),
        ERROR_CANCELLED | u32::MAX => Ok(false),
        error => Err(io::Error::from_raw_os_error(error as i32)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_the_current_users_profile_directory() {
        let profile = known_folder(KnownFolder::Profile).unwrap();
        assert!(profile.is_absolute());
        assert!(profile.is_dir());
    }
}
