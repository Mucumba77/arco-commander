use std::{ffi::OsStr, io, os::windows::ffi::OsStrExt, path::Path};
use windows_sys::Win32::Storage::FileSystem::{
    GetFileVersionInfoSizeW, GetFileVersionInfoW, GetShortPathNameW, VS_FIXEDFILEINFO,
    VerQueryValueW,
};

fn wide(value: &OsStr) -> io::Result<Vec<u16>> {
    let mut value = value.encode_wide().collect::<Vec<_>>();
    if value.contains(&0) {
        return Err(io::Error::other("Ein Dateipfad enthält ein Nullzeichen."));
    }
    value.push(0);
    Ok(value)
}

pub(super) fn grouping_separator() -> String {
    locale_separator(windows_sys::Win32::Globalization::LOCALE_STHOUSAND, ",")
}
pub(super) fn decimal_separator() -> String {
    locale_separator(windows_sys::Win32::Globalization::LOCALE_SDECIMAL, ".")
}
fn locale_separator(kind: u32, fallback: &str) -> String {
    use windows_sys::Win32::Globalization::GetLocaleInfoEx;
    let mut buffer = [0u16; 32];
    // Null locale selects the user's current Windows preferences, including overrides.
    let count = unsafe {
        GetLocaleInfoEx(
            std::ptr::null(),
            kind,
            buffer.as_mut_ptr(),
            buffer.len() as i32,
        )
    };
    if count > 0 && count as usize <= buffer.len() {
        String::from_utf16_lossy(&buffer[..count as usize - 1])
    } else {
        fallback.into()
    }
}

pub(super) fn decode_ansi(bytes: &[u8]) -> io::Result<String> {
    use windows_sys::Win32::Globalization::MultiByteToWideChar;
    let count = i32::try_from(bytes.len())
        .map_err(|_| io::Error::other("Der kodierte Name ist zu lang."))?;
    if count == 0 {
        return Ok(String::new());
    }
    let size = unsafe { MultiByteToWideChar(0, 0, bytes.as_ptr(), count, std::ptr::null_mut(), 0) };
    if size == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut text = vec![0u16; size as usize];
    let used = unsafe { MultiByteToWideChar(0, 0, bytes.as_ptr(), count, text.as_mut_ptr(), size) };
    if used == 0 {
        return Err(io::Error::last_os_error());
    }
    String::from_utf16(&text[..used as usize])
        .map_err(|_| io::Error::other("Ungültiger kodierter Dateiname."))
}

pub(super) fn case_sensitive(path: &Path) -> io::Result<bool> {
    use std::os::windows::{fs::OpenOptionsExt, io::AsRawHandle};
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_CASE_SENSITIVE_INFO, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        FileCaseSensitiveInfo, GetFileInformationByHandleEx,
    };
    let file = std::fs::OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    let mut info = FILE_CASE_SENSITIVE_INFO { Flags: 0 };
    if unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileCaseSensitiveInfo,
            (&mut info as *mut FILE_CASE_SENSITIVE_INFO).cast(),
            std::mem::size_of_val(&info) as u32,
        )
    } == 0
    {
        let error = io::Error::last_os_error();
        if matches!(error.raw_os_error(), Some(1 | 50 | 87)) {
            return Ok(false);
        }
        return Err(error);
    }
    Ok(info.Flags & 1 != 0)
}

pub(super) fn short_name(path: &Path) -> io::Result<String> {
    let input = wide(path.as_os_str())?;
    let length = unsafe { GetShortPathNameW(input.as_ptr(), std::ptr::null_mut(), 0) };
    if length == 0 {
        return Err(io::Error::last_os_error());
    }
    if length > 32768 {
        return Err(io::Error::other("Der DOS-Pfad ist zu lang."));
    }
    let mut output = vec![0u16; length as usize];
    let used = unsafe { GetShortPathNameW(input.as_ptr(), output.as_mut_ptr(), length) };
    if used == 0 || used >= length {
        return Err(io::Error::last_os_error());
    }
    let name = String::from_utf16_lossy(&output[..used as usize]);
    Ok(Path::new(&name)
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned())
}

/// Reads PE version resources as data. No image is loaded or executed.
pub(super) fn version(path: &Path) -> io::Result<Option<(String, String)>> {
    let input = wide(path.as_os_str())?;
    let size = unsafe { GetFileVersionInfoSizeW(input.as_ptr(), std::ptr::null_mut()) };
    if size == 0 {
        return Ok(None);
    }
    if size > 16 * 1024 * 1024 {
        return Err(io::Error::other(
            "Die Versionsressource überschreitet 16 MiB.",
        ));
    }
    let mut data = vec![0u32; (size as usize).div_ceil(4)];
    if unsafe { GetFileVersionInfoW(input.as_ptr(), 0, size, data.as_mut_ptr().cast()) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let query = |name: &str| -> Option<(*const u8, usize)> {
        let name = name.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
        let mut value = std::ptr::null_mut();
        let mut length = 0;
        if unsafe { VerQueryValueW(data.as_ptr().cast(), name.as_ptr(), &mut value, &mut length) }
            == 0
            || value.is_null()
        {
            return None;
        }
        let start = data.as_ptr() as usize;
        let at = value as usize;
        if at < start || at >= start + data.len() * 4 {
            return None;
        }
        Some((value.cast(), length as usize))
    };
    let Some((fixed, length)) = query("\\") else {
        return Ok(None);
    };
    let offset = fixed as usize - data.as_ptr() as usize;
    if length < std::mem::size_of::<VS_FIXEDFILEINFO>()
        || offset + std::mem::size_of::<VS_FIXEDFILEINFO>() > data.len() * 4
    {
        return Ok(None);
    }
    let fixed = unsafe { std::ptr::read_unaligned(fixed.cast::<VS_FIXEDFILEINFO>()) };
    if fixed.dwSignature != 0xfeef04bd {
        return Ok(None);
    }
    let number = format!(
        "{}.{}.{}.{}",
        fixed.dwFileVersionMS >> 16,
        fixed.dwFileVersionMS & 65535,
        fixed.dwFileVersionLS >> 16,
        fixed.dwFileVersionLS & 65535
    );
    let mut translations = vec![(0x0409_u16, 0x04b0_u16)];
    if let Some((pointer, length)) = query(r"\VarFileInfo\Translation") {
        let offset = pointer as usize - data.as_ptr() as usize;
        if length <= 1024 && offset + length <= data.len() * 4 {
            for index in (0..length.saturating_sub(3)).step_by(4) {
                let language =
                    unsafe { std::ptr::read_unaligned(pointer.add(index).cast::<u16>()) };
                let codepage =
                    unsafe { std::ptr::read_unaligned(pointer.add(index + 2).cast::<u16>()) };
                translations.insert(0, (language, codepage));
            }
        }
    }
    for (language, codepage) in translations {
        if let Some((pointer, length)) = query(&format!(
            r"\StringFileInfo\{language:04x}{codepage:04x}\FileVersion"
        )) {
            let offset = pointer as usize - data.as_ptr() as usize;
            if length <= 32768
                && (pointer as usize).is_multiple_of(2)
                && offset + length * 2 <= data.len() * 4
            {
                let text = unsafe { std::slice::from_raw_parts(pointer.cast::<u16>(), length) };
                let text = String::from_utf16_lossy(text)
                    .trim_end_matches('\0')
                    .trim()
                    .to_owned();
                if !text.is_empty() {
                    return Ok(Some((text, number)));
                }
            }
        }
    }
    Ok(Some((number.clone(), number)))
}
