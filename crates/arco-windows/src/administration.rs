use crate::{LocalShare, Volume, platform::wide};
use std::ffi::{OsString, c_void};
use std::io;
use std::os::windows::ffi::OsStringExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use windows_sys::Win32::Foundation::{ERROR_MORE_DATA, LocalFree};
use windows_sys::Win32::NetworkManagement::NetManagement::NetApiBufferFree;
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
};
use windows_sys::Win32::Security::{GetTokenInformation, TOKEN_QUERY, TOKEN_USER, TokenUser};
use windows_sys::Win32::Storage::FileSystem::{
    GetDriveTypeW, GetVolumeInformationW, GetVolumeNameForVolumeMountPointW, GetVolumePathNameW,
    NetShareAdd, NetShareDel, NetShareEnum, NetShareGetInfo, SHARE_INFO_2, SHARE_INFO_502,
    STYPE_DISKTREE, STYPE_MASK, STYPE_SPECIAL, SetVolumeLabelW,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

struct NetBuffer(*mut u8);

impl Drop for NetBuffer {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { NetApiBufferFree(self.0.cast()) };
        }
    }
}

struct LocalBuffer(*mut c_void);

impl Drop for LocalBuffer {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { LocalFree(self.0) };
        }
    }
}

fn from_wide(buffer: &[u16]) -> OsString {
    let end = buffer
        .iter()
        .position(|&word| word == 0)
        .unwrap_or(buffer.len());
    OsString::from_wide(&buffer[..end])
}

fn net_text(pointer: *const u16) -> OsString {
    if pointer.is_null() {
        return OsString::new();
    }
    // The Win32 routines guarantee a readable, NUL-terminated wide string.
    let mut len = 0;
    while unsafe { *pointer.add(len) } != 0 {
        len += 1;
    }
    OsString::from_wide(unsafe { std::slice::from_raw_parts(pointer, len) })
}

fn status(code: u32) -> io::Result<()> {
    if code == 0 {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(code as i32))
    }
}

fn disk_share(entry: &SHARE_INFO_2) -> Option<LocalShare> {
    if entry.shi2_type & STYPE_MASK != STYPE_DISKTREE {
        return None;
    }
    Some(LocalShare {
        name: net_text(entry.shi2_netname).to_string_lossy().into_owned(),
        path: PathBuf::from(net_text(entry.shi2_path)),
        special: entry.shi2_type & STYPE_SPECIAL != 0,
    })
}

pub fn local_shares() -> io::Result<Vec<LocalShare>> {
    let mut shares = Vec::new();
    let mut resume = 0;
    loop {
        let mut buffer = NetBuffer(null_mut());
        let mut count = 0;
        let mut total = 0;
        let previous = resume;
        let code = unsafe {
            NetShareEnum(
                null(),
                2,
                &mut buffer.0,
                64 * 1024,
                &mut count,
                &mut total,
                &mut resume,
            )
        };
        if code != ERROR_MORE_DATA {
            status(code)?;
        }
        if count > 0 {
            if buffer.0.is_null() {
                return Err(io::Error::other(
                    "Windows lieferte eine leere Freigabeliste.",
                ));
            }
            let entries = unsafe {
                std::slice::from_raw_parts(buffer.0.cast::<SHARE_INFO_2>(), count as usize)
            };
            shares.extend(entries.iter().filter_map(disk_share));
        }
        if code == 0 {
            break;
        }
        if previous == resume || shares.len() > 16384 {
            return Err(io::Error::other(
                "Windows konnte die Freigabeliste nicht vollständig lesen.",
            ));
        }
    }
    shares.sort_by_cached_key(|share| share.name.to_lowercase());
    Ok(shares)
}

fn current_account_sid() -> io::Result<String> {
    let mut token = null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let token = unsafe { OwnedHandle::from_raw_handle(token) };
    let mut bytes = 0;
    unsafe { GetTokenInformation(token.as_raw_handle(), TokenUser, null_mut(), 0, &mut bytes) };
    if bytes == 0 || bytes > 65536 {
        return Err(io::Error::last_os_error());
    }
    let mut words = vec![0usize; (bytes as usize).div_ceil(std::mem::size_of::<usize>())];
    if unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            words.as_mut_ptr().cast(),
            bytes,
            &mut bytes,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let user = unsafe { &*words.as_ptr().cast::<TOKEN_USER>() };
    let mut sid = null_mut();
    if unsafe { ConvertSidToStringSidW(user.User.Sid, &mut sid) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let _guard = LocalBuffer(sid.cast());
    net_text(sid)
        .into_string()
        .map_err(|_| io::Error::other("Windows lieferte eine ungültige SID."))
}

fn validate_share_name(name: &str) -> io::Result<()> {
    let reserved = name.eq_ignore_ascii_case("IPC$")
        || name.eq_ignore_ascii_case("ADMIN$")
        || (name.len() == 2 && name.ends_with('$') && name.as_bytes()[0].is_ascii_alphabetic());
    if name.is_empty()
        || name.encode_utf16().count() > 80
        || reserved
        || name.ends_with(['.', ' '])
        || name.contains([
            '\\', '/', ':', '*', '?', '"', '<', '>', '|', ',', ';', '=', '+', '[', ']',
        ])
        || name.chars().any(char::is_control)
    {
        return Err(io::Error::other(
            "Ungültiger oder reservierter Freigabename.",
        ));
    }
    Ok(())
}

pub fn create_share(directory: &Path, name: &str, read_only: bool) -> io::Result<()> {
    validate_share_name(name)?;
    if !directory.is_absolute() || !directory.is_dir() {
        return Err(io::Error::other("Wähle einen bestehenden lokalen Ordner."));
    }
    let directory = std::fs::canonicalize(directory)?;
    let directory = directory
        .to_str()
        .ok_or_else(|| io::Error::other("Der Pfad enthält ungültigen Unicode."))?;
    let directory = directory.strip_prefix(r"\\?\").unwrap_or(directory);
    if !directory
        .as_bytes()
        .get(1..3)
        .is_some_and(|prefix| prefix == b":\\")
    {
        return Err(io::Error::other(
            "Die Freigabe benötigt einen Pfad auf einem lokalen Laufwerk.",
        ));
    }
    let mut name = wide(name)?;
    let mut path = wide(directory)?;
    let sid = current_account_sid()?;
    let access = if read_only { "0x1200a9" } else { "FA" };
    let sddl = wide(format!("D:P(A;;{access};;;{sid})"))?;
    let mut security = LocalBuffer(null_mut());
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            1,
            &mut security.0,
            null_mut(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let info = SHARE_INFO_502 {
        shi502_netname: name.as_mut_ptr(),
        shi502_type: STYPE_DISKTREE,
        shi502_path: path.as_mut_ptr(),
        shi502_max_uses: u32::MAX,
        shi502_security_descriptor: security.0,
        ..Default::default()
    };
    let mut parameter = 0;
    let code = unsafe { NetShareAdd(null(), 502, (&raw const info).cast(), &mut parameter) };
    status(code)
}

pub fn remove_share(share: &LocalShare) -> io::Result<()> {
    let name = wide(&share.name)?;
    let mut buffer = NetBuffer(null_mut());
    status(unsafe { NetShareGetInfo(null(), name.as_ptr(), 2, &mut buffer.0) })?;
    if buffer.0.is_null()
        || disk_share(unsafe { &*buffer.0.cast::<SHARE_INFO_2>() }).as_ref() != Some(share)
    {
        return Err(io::Error::other(
            "Die Freigabe wurde seit der Anzeige geändert.",
        ));
    }
    status(unsafe { NetShareDel(null(), name.as_ptr(), 0) })
}

pub fn volume(path: &Path) -> io::Result<Volume> {
    if !path.is_absolute() {
        return Err(io::Error::other(
            "Ein absoluter Laufwerkspfad ist erforderlich.",
        ));
    }
    let path = wide(path)?;
    let mut buffer = vec![0u16; 32768];
    if unsafe { GetVolumePathNameW(path.as_ptr(), buffer.as_mut_ptr(), buffer.len() as u32) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let root = PathBuf::from(from_wide(&buffer));
    let wide_root = wide(&root)?;
    const DRIVE_REMOVABLE: u32 = 2;
    const DRIVE_FIXED: u32 = 3;
    if !matches!(
        unsafe { GetDriveTypeW(wide_root.as_ptr()) },
        DRIVE_REMOVABLE | DRIVE_FIXED
    ) {
        return Err(io::Error::other(
            "Datenträgerbezeichnungen sind für lokale Laufwerke verfügbar.",
        ));
    }
    let mut label = [0u16; 256];
    if unsafe {
        GetVolumeInformationW(
            wide_root.as_ptr(),
            label.as_mut_ptr(),
            label.len() as u32,
            null_mut(),
            null_mut(),
            null_mut(),
            null_mut(),
            0,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    if unsafe {
        GetVolumeNameForVolumeMountPointW(
            wide_root.as_ptr(),
            buffer.as_mut_ptr(),
            buffer.len() as u32,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(Volume {
        root,
        label: from_wide(&label).to_string_lossy().into_owned(),
        identifier: PathBuf::from(from_wide(&buffer)),
    })
}

pub fn rename_volume(expected: &Volume, label: &str) -> io::Result<()> {
    if volume(&expected.root)? != *expected {
        return Err(io::Error::other(
            "Der Datenträger oder seine Bezeichnung hat sich geändert.",
        ));
    }
    let root = wide(&expected.identifier)?;
    let label = (!label.is_empty()).then(|| wide(label)).transpose()?;
    let label = label.as_ref().map_or_else(null, |value| value.as_ptr());
    if unsafe { SetVolumeLabelW(root.as_ptr(), label) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_share_names_and_resolves_the_local_account() {
        assert!(validate_share_name("Arco Test").is_ok());
        for name in ["", "C$", "ADMIN$", "Verzeichnis\\Ordner", "hinten."] {
            assert!(validate_share_name(name).is_err());
        }
        assert!(current_account_sid().unwrap().starts_with("S-1-"));
    }

    #[test]
    fn reads_volume_identity_from_a_local_folder_without_writing_it() {
        let fixture = tempfile::tempdir().unwrap();
        let disk = volume(fixture.path()).unwrap();
        assert!(disk.root.is_absolute());
        assert!(
            disk.identifier
                .to_string_lossy()
                .starts_with(r"\\?\Volume{")
        );
        assert_eq!(volume(&disk.root).unwrap(), disk);
    }
}
