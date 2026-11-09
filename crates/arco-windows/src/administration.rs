use crate::ShareAccess;
use crate::{LocalShare, Volume, platform::wide};
use std::collections::BTreeMap;
use std::ffi::{OsString, c_void};
use std::io;
use std::os::windows::ffi::OsStringExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use windows_sys::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, ERROR_MORE_DATA, LocalFree};
use windows_sys::Win32::NetworkManagement::NetManagement::NetApiBufferFree;
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
    ConvertStringSidToSidW,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, LookupAccountNameW, PSID, TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows_sys::Win32::Security::{
    SidTypeAlias, SidTypeComputer, SidTypeGroup, SidTypeUser, SidTypeWellKnownGroup,
};
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
    sid_string(user.User.Sid)
}

fn sid_string(account: PSID) -> io::Result<String> {
    let mut sid = null_mut();
    if unsafe { ConvertSidToStringSidW(account, &mut sid) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let _guard = LocalBuffer(sid.cast());
    net_text(sid)
        .into_string()
        .map_err(|_| io::Error::other("Windows lieferte eine ungültige SID."))
}

fn named_account_sid(account: &str) -> io::Result<String> {
    let name = wide(account)?;
    if account.starts_with("S-1-") {
        let mut sid = LocalBuffer(null_mut());
        if unsafe { ConvertStringSidToSidW(name.as_ptr(), &mut sid.0) } == 0 {
            return Err(io::Error::last_os_error());
        }
        return sid_string(sid.0);
    }
    let (mut sid_bytes, mut domain_length, mut kind) = (0, 0, 0);
    unsafe {
        LookupAccountNameW(
            null(),
            name.as_ptr(),
            null_mut(),
            &mut sid_bytes,
            null_mut(),
            &mut domain_length,
            &mut kind,
        )
    };
    let error = io::Error::last_os_error();
    if error.raw_os_error() != Some(ERROR_INSUFFICIENT_BUFFER as i32) || sid_bytes == 0 {
        return Err(error);
    }
    let mut sid = vec![0usize; (sid_bytes as usize).div_ceil(std::mem::size_of::<usize>())];
    let mut domain = vec![0u16; domain_length as usize];
    if unsafe {
        LookupAccountNameW(
            null(),
            name.as_ptr(),
            sid.as_mut_ptr().cast(),
            &mut sid_bytes,
            domain.as_mut_ptr(),
            &mut domain_length,
            &mut kind,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    if ![
        SidTypeUser,
        SidTypeGroup,
        SidTypeAlias,
        SidTypeWellKnownGroup,
        SidTypeComputer,
    ]
    .contains(&kind)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Kein verwendbares Windows-Konto oder keine Gruppe.",
        ));
    }
    sid_string(sid.as_mut_ptr().cast())
}

fn share_accounts(
    accounts: &str,
    default_access: ShareAccess,
) -> io::Result<BTreeMap<String, ShareAccess>> {
    if accounts.trim().is_empty() {
        return Ok(BTreeMap::from([(current_account_sid()?, default_access)]));
    }
    let names: Vec<_> = accounts
        .split(';')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .collect();
    if names.is_empty()
        || names.len() > 32
        || names
            .iter()
            .any(|name| name.encode_utf16().count() > 280 || name.chars().any(char::is_control))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Gib bis zu 32 Konten oder Gruppen an.",
        ));
    }
    let mut permissions = BTreeMap::new();
    for name in names {
        let (name, access) = if let Some((name, access)) = name.split_once('=') {
            let access = match access.trim().to_lowercase().as_str() {
                "lesen" | "read" => ShareAccess::Read,
                "ändern" | "change" => ShareAccess::Change,
                "vollzugriff" | "full" => ShareAccess::Full,
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "Erwartet wird Konto=Lesen, Konto=Ändern oder Konto=Vollzugriff.",
                    ));
                }
            };
            (name.trim(), access)
        } else {
            (name, default_access)
        };
        if name.is_empty() || name.encode_utf16().count() > 256 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Ungültiger Windows-Kontoname.",
            ));
        }
        let sid = named_account_sid(name).map_err(|error| {
            io::Error::new(error.kind(), format!("Windows-Konto „{name}“: {error}"))
        })?;
        if permissions
            .insert(sid, access)
            .is_some_and(|previous| previous != access)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("Widersprüchliche Rechte für „{name}“."),
            ));
        }
    }
    Ok(permissions)
}

fn share_security(access: ShareAccess, accounts: &str) -> io::Result<LocalBuffer> {
    let mut sddl = String::from("D:P");
    for (sid, access) in share_accounts(accounts, access)? {
        let access = match access {
            ShareAccess::Read => "0x1200a9",
            ShareAccess::Change => "0x1301bf",
            ShareAccess::Full => "FA",
        };
        use std::fmt::Write as _;
        write!(sddl, "(A;;{access};;;{sid})").expect("write to string");
    }
    let sddl = wide(sddl)?;
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
    Ok(security)
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

pub fn create_share(
    directory: &Path,
    name: &str,
    access: ShareAccess,
    accounts: &str,
) -> io::Result<()> {
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
    let security = share_security(access, accounts)?;
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
    #[ignore = "requires an elevated Windows account and the SMB Server service"]
    fn reference_creates_checks_and_removes_real_smb_shares() {
        struct ShareGuard(Option<LocalShare>);
        impl Drop for ShareGuard {
            fn drop(&mut self) {
                if let Some(share) = &self.0 {
                    let _ = remove_share(share);
                }
            }
        }

        let fixture = tempfile::tempdir().unwrap();
        let canonical = fixture.path().canonicalize().unwrap();
        let directory = canonical.to_str().unwrap().trim_start_matches(r"\\?\");
        let suffix = fixture.path().file_name().unwrap().to_str().unwrap();
        let account = current_account_sid().unwrap();
        for (label, access) in [("read", ShareAccess::Read), ("write", ShareAccess::Change)] {
            let name = format!("Arco-{label}-{suffix}");
            let file = format!("{label}.txt");
            std::fs::write(fixture.path().join(&file), b"share fixture").unwrap();
            create_share(fixture.path(), &name, access, &account).unwrap();
            let expected = LocalShare {
                name: name.clone(),
                path: PathBuf::from(directory),
                special: false,
            };
            let mut guard = ShareGuard(Some(expected.clone()));
            assert!(local_shares().unwrap().contains(&expected));
            let unc = PathBuf::from(format!(r"\\localhost\{name}"));
            assert_eq!(std::fs::read(unc.join(&file)).unwrap(), b"share fixture");
            let write = std::fs::write(unc.join(&file), b"changed by SMB");
            if matches!(access, ShareAccess::Read) {
                assert_eq!(write.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
            } else {
                write.unwrap();
                assert_eq!(
                    std::fs::read(fixture.path().join(&file)).unwrap(),
                    b"changed by SMB"
                );
            }
            remove_share(&expected).unwrap();
            guard.0 = None;
            assert!(!local_shares().unwrap().contains(&expected));
        }
    }

    #[test]
    fn validates_share_names_and_resolves_the_local_account() {
        assert!(validate_share_name("Arco Test").is_ok());
        for name in ["", "C$", "ADMIN$", "Verzeichnis\\Ordner", "hinten."] {
            assert!(validate_share_name(name).is_err());
        }
        assert!(current_account_sid().unwrap().starts_with("S-1-"));
    }

    #[test]
    fn share_permissions_resolve_and_deduplicate_principals_without_publishing() {
        use windows_sys::Win32::Security::{ACCESS_ALLOWED_ACE, GetAce, GetSecurityDescriptorDacl};
        let sid = current_account_sid().unwrap();
        let account = format!(
            "{}\\{}",
            std::env::var("USERDOMAIN").unwrap(),
            std::env::var("USERNAME").unwrap()
        );
        assert_eq!(named_account_sid(&account).unwrap(), sid);
        assert_eq!(
            share_accounts("", ShareAccess::Read).unwrap(),
            BTreeMap::from([(sid.clone(), ShareAccess::Read)])
        );
        let accounts = format!("{account}; {sid}; S-1-5-32-545=Lesen");
        assert_eq!(
            share_accounts(&accounts, ShareAccess::Change)
                .unwrap()
                .len(),
            2
        );
        assert!(share_accounts("S-1-5-32-545)(A;;FA;;;WD", ShareAccess::Read).is_err());
        assert!(
            share_accounts(
                &format!("{account}=Lesen;{sid}=Vollzugriff"),
                ShareAccess::Read
            )
            .is_err()
        );
        let descriptor = share_security(ShareAccess::Change, &accounts).unwrap();
        let (mut present, mut acl, mut defaulted) = (0, null_mut(), 0);
        assert_ne!(
            unsafe {
                GetSecurityDescriptorDacl(descriptor.0, &mut present, &mut acl, &mut defaulted)
            },
            0
        );
        assert_ne!(present, 0);
        assert!(!acl.is_null());
        assert_eq!(unsafe { (*acl).AceCount }, 2);
        let mut masks = Vec::new();
        for index in 0..2 {
            let mut ace = null_mut();
            assert_ne!(unsafe { GetAce(acl, index, &mut ace) }, 0);
            masks.push(unsafe { (*ace.cast::<ACCESS_ALLOWED_ACE>()).Mask });
        }
        masks.sort();
        assert_eq!(masks, [0x1200a9, 0x1301bf]);
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
