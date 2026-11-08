use std::{ffi::OsString, io, os::windows::ffi::OsStringExt, path::PathBuf};
use windows::{
    Win32::{
        System::Com::{
            CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
            CoTaskMemFree, CoUninitialize,
        },
        UI::Shell::{
            FOS_FORCEFILESYSTEM, FOS_PATHMUSTEXIST, FOS_PICKFOLDERS, FileOpenDialog,
            IFileOpenDialog, SIGDN_FILESYSPATH,
        },
    },
    core::{HRESULT, w},
};
use windows_sys::Win32::{
    Foundation::{ERROR_CANCELLED, NO_ERROR},
    Security::Credentials::{
        CREDUI_FLAGS_ALWAYS_SHOW_UI, CREDUI_FLAGS_DO_NOT_PERSIST, CREDUI_FLAGS_GENERIC_CREDENTIALS,
        CREDUI_FLAGS_PASSWORD_ONLY_OK, CREDUI_INFOW, CREDUI_MAX_USERNAME_LENGTH,
        CredUIPromptForCredentialsW,
    },
    UI::WindowsAndMessaging::{MB_ICONERROR, MB_ICONINFORMATION, MB_OK, MessageBoxW},
};
use zeroize::Zeroizing;

pub fn destination() -> io::Result<Option<PathBuf>> {
    struct Com;
    impl Drop for Com {
        fn drop(&mut self) {
            unsafe { CoUninitialize() };
        }
    }
    unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok() }.map_err(io::Error::other)?;
    let _com = Com;
    let result = (|| -> windows::core::Result<Option<PathBuf>> {
        let dialog: IFileOpenDialog =
            unsafe { CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)? };
        unsafe {
            dialog.SetOptions(
                dialog.GetOptions()? | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM | FOS_PATHMUSTEXIST,
            )?;
            dialog.SetTitle(w!("Zielordner für das ZIP-Archiv wählen"))?;
            if let Err(error) = dialog.Show(None) {
                if error.code() == HRESULT::from_win32(1223) {
                    return Ok(None);
                }
                return Err(error);
            }
            let selected = dialog.GetResult()?.GetDisplayName(SIGDN_FILESYSPATH)?;
            let path = PathBuf::from(OsString::from_wide(selected.as_wide()));
            CoTaskMemFree(Some(selected.0.cast()));
            Ok(Some(path))
        }
    })();
    result.map_err(io::Error::other)
}

pub fn password(limit: usize) -> io::Result<Option<Zeroizing<String>>> {
    let info = CREDUI_INFOW {
        cbSize: size_of::<CREDUI_INFOW>() as u32,
        pszCaptionText: w!("Arco ZIP-SFX").as_ptr(),
        pszMessageText: w!("Gib das Passwort für das ZIP-Archiv ein.").as_ptr(),
        ..CREDUI_INFOW::default()
    };
    let mut username = [0; CREDUI_MAX_USERNAME_LENGTH as usize + 1];
    let mut password = Zeroizing::new(vec![0u16; limit + 1]);
    let result = unsafe {
        CredUIPromptForCredentialsW(
            &info,
            w!("Arco ZIP-SFX").as_ptr(),
            std::ptr::null(),
            0,
            username.as_mut_ptr(),
            username.len() as u32,
            password.as_mut_ptr(),
            password.len() as u32,
            std::ptr::null_mut(),
            CREDUI_FLAGS_GENERIC_CREDENTIALS
                | CREDUI_FLAGS_DO_NOT_PERSIST
                | CREDUI_FLAGS_ALWAYS_SHOW_UI
                | CREDUI_FLAGS_PASSWORD_ONLY_OK,
        )
    };
    match result {
        ERROR_CANCELLED => Ok(None),
        NO_ERROR => {
            let end = password.iter().position(|&unit| unit == 0).unwrap_or(limit);
            let value = String::from_utf16(&password[..end]).map_err(io::Error::other)?;
            Ok(Some(Zeroizing::new(value)))
        }
        error => Err(io::Error::from_raw_os_error(error as i32)),
    }
}

pub fn notice(message: &str, failed: bool) {
    let message: Vec<_> = message.encode_utf16().chain(Some(0)).collect();
    let title: Vec<_> = "Arco ZIP-SFX".encode_utf16().chain(Some(0)).collect();
    let icon = if failed {
        MB_ICONERROR
    } else {
        MB_ICONINFORMATION
    };
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            message.as_ptr(),
            title.as_ptr(),
            MB_OK | icon,
        )
    };
}
