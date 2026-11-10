use super::windows::file_id;
use crate::files::{self, NtfsCopyOptions};
use std::fs::{File, OpenOptions};
use std::io;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;
use std::ptr::{null, null_mut};
use windows_sys::Win32::Foundation::{
    ERROR_NO_TOKEN, ERROR_NOT_ALL_ASSIGNED, GetLastError, HANDLE, LocalFree,
};
use windows_sys::Win32::Security::Authorization::{
    GetSecurityInfo, SE_FILE_OBJECT, SetSecurityInfo,
};
use windows_sys::Win32::Security::{
    ACL, ACL_REVISION, AdjustTokenPrivileges, DACL_SECURITY_INFORMATION, DuplicateTokenEx,
    EqualSid, InitializeAcl, LUID_AND_ATTRIBUTES, LookupPrivilegeValueW,
    OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION,
    PROTECTED_SACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, SACL_SECURITY_INFORMATION,
    SE_PRIVILEGE_ENABLED, SE_RESTORE_NAME, SE_SECURITY_NAME, SecurityImpersonation,
    TOKEN_ADJUST_PRIVILEGES, TOKEN_DUPLICATE, TOKEN_IMPERSONATE, TOKEN_PRIVILEGES, TOKEN_QUERY,
    TokenImpersonation,
};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES, READ_CONTROL,
    WRITE_DAC, WRITE_OWNER,
};
use windows_sys::Win32::System::SystemServices::ACCESS_SYSTEM_SECURITY;
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentThread, OpenProcessToken, OpenThreadToken, SetThreadToken,
};

pub(super) struct Descriptor {
    pub(super) raw: PSECURITY_DESCRIPTOR,
    pub(super) owner: PSID,
    pub(super) dacl: *mut ACL,
    pub(super) sacl: *mut ACL,
}

impl Drop for Descriptor {
    fn drop(&mut self) {
        unsafe { LocalFree(self.raw) };
    }
}

impl Descriptor {
    pub(super) fn read(file: &File, fields: u32) -> io::Result<Self> {
        let mut descriptor = Self {
            raw: null_mut(),
            owner: null_mut(),
            dacl: null_mut(),
            sacl: null_mut(),
        };
        let status = unsafe {
            GetSecurityInfo(
                file.as_raw_handle(),
                SE_FILE_OBJECT,
                fields,
                &mut descriptor.owner,
                null_mut(),
                &mut descriptor.dacl,
                &mut descriptor.sacl,
                &mut descriptor.raw,
            )
        };
        if status == 0 {
            Ok(descriptor)
        } else {
            Err(io::Error::from_raw_os_error(status as i32))
        }
    }
}

pub(super) fn open(path: &Path, access: u32) -> io::Result<File> {
    let file = OpenOptions::new()
        .access_mode(access | FILE_READ_ATTRIBUTES)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    if files::is_link(&file.metadata()?) {
        return Err(io::ErrorKind::Unsupported.into());
    }
    Ok(file)
}

fn reopen(path: &Path, reference: &File, access: u32) -> io::Result<File> {
    let file = open(path, access)?;
    if file_id(&file)? != file_id(reference)? {
        return Err(io::Error::other(
            "Der Pfad für die NTFS-Sicherheitsdaten hat sich geändert.",
        ));
    }
    Ok(file)
}

pub(super) fn copy(
    source: &File,
    source_path: &Path,
    (target_path, target): (&Path, &File),
    options: NtfsCopyOptions,
) -> io::Result<()> {
    if !options.permissions && !options.owner && !options.audit {
        return Ok(());
    }
    let mut flags = 0;
    let mut access = 0;
    if options.permissions {
        flags |= DACL_SECURITY_INFORMATION;
        access |= WRITE_DAC;
    }
    if options.owner {
        flags |= OWNER_SECURITY_INFORMATION;
    }
    let ordinary = if flags != 0 {
        Some(Descriptor::read(source, flags)?)
    } else {
        None
    };
    if options.owner {
        let source_owner = ordinary.as_ref().expect("requested owner").owner;
        let target_owner = Descriptor::read(target, OWNER_SECURITY_INFORMATION)?;
        if source_owner.is_null() || target_owner.owner.is_null() {
            return Err(io::Error::other("Windows lieferte keinen NTFS-Besitzer."));
        }
        if unsafe { EqualSid(source_owner, target_owner.owner) } != 0 {
            flags &= !OWNER_SECURITY_INFORMATION;
        } else {
            access |= WRITE_OWNER;
        }
    }
    if flags == 0 && !options.audit {
        return Ok(());
    }
    let _privileges = PrivilegedToken::new(access & WRITE_OWNER != 0, options.audit)?.install()?;
    let audit = if options.audit {
        flags |= SACL_SECURITY_INFORMATION | PROTECTED_SACL_SECURITY_INFORMATION;
        access |= ACCESS_SYSTEM_SECURITY;
        let source = reopen(source_path, source, ACCESS_SYSTEM_SECURITY)?;
        Some(Descriptor::read(&source, SACL_SECURITY_INFORMATION)?)
    } else {
        None
    };
    if options.permissions {
        flags |= PROTECTED_DACL_SECURITY_INFORMATION;
    }
    let owner = ordinary
        .as_ref()
        .filter(|_| flags & OWNER_SECURITY_INFORMATION != 0)
        .map_or(null_mut(), |sd| sd.owner);
    let mut empty_sacl = ACL::default();
    let sacl = match &audit {
        Some(sd) if sd.sacl.is_null() => {
            if unsafe {
                InitializeAcl(
                    &mut empty_sacl,
                    std::mem::size_of::<ACL>() as u32,
                    ACL_REVISION,
                )
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
            &raw const empty_sacl
        }
        Some(sd) => sd.sacl.cast_const(),
        None => null(),
    };
    let target = reopen(target_path, target, access | READ_CONTROL).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("NTFS-Ziel für Sicherheitsdaten öffnen: {error}"),
        )
    })?;
    let status = unsafe {
        SetSecurityInfo(
            target.as_raw_handle(),
            SE_FILE_OBJECT,
            flags,
            owner,
            null_mut(),
            ordinary.as_ref().map_or(null(), |sd| sd.dacl.cast_const()),
            sacl,
        )
    };
    if status == 0 {
        Ok(())
    } else {
        let error = io::Error::from_raw_os_error(status as i32);
        Err(io::Error::new(
            error.kind(),
            format!("NTFS-Sicherheitsdaten übernehmen: {error}"),
        ))
    }
}

pub(super) fn check_audit() -> io::Result<()> {
    PrivilegedToken::new(false, true).map(|_| ())
}

struct PrivilegedToken {
    duplicate: Option<OwnedHandle>,
    original: Option<OwnedHandle>,
}

impl PrivilegedToken {
    fn new(owner: bool, audit: bool) -> io::Result<Self> {
        if !owner && !audit {
            return Ok(Self {
                duplicate: None,
                original: None,
            });
        }
        let mut handle = null_mut();
        let original = if unsafe {
            OpenThreadToken(
                GetCurrentThread(),
                TOKEN_DUPLICATE | TOKEN_IMPERSONATE,
                1,
                &mut handle,
            )
        } != 0
        {
            Some(unsafe { OwnedHandle::from_raw_handle(handle) })
        } else {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(ERROR_NO_TOKEN as i32) {
                return Err(error);
            }
            None
        };
        let process;
        let current = if let Some(thread) = &original {
            thread
        } else {
            if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_DUPLICATE, &mut handle) } == 0 {
                return Err(io::Error::last_os_error());
            }
            process = unsafe { OwnedHandle::from_raw_handle(handle) };
            &process
        };
        if unsafe {
            DuplicateTokenEx(
                current.as_raw_handle(),
                TOKEN_QUERY | TOKEN_IMPERSONATE | TOKEN_ADJUST_PRIVILEGES,
                null(),
                SecurityImpersonation,
                TokenImpersonation,
                &mut handle,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let token = unsafe { OwnedHandle::from_raw_handle(handle) };
        let restore = owner && enable(token.as_raw_handle(), SE_RESTORE_NAME)?;
        if audit && !enable(token.as_raw_handle(), SE_SECURITY_NAME)? {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Überwachungs-ACLs benötigen das Windows-Recht SeSecurityPrivilege.",
            ));
        }
        Ok(Self {
            duplicate: (restore || audit).then_some(token),
            original,
        })
    }

    fn install(self) -> io::Result<Option<ThreadToken>> {
        let Some(token) = self.duplicate else {
            return Ok(None);
        };
        if unsafe { SetThreadToken(null(), token.as_raw_handle()) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Some(ThreadToken(self.original)))
    }
}

fn enable(token: HANDLE, name: windows_sys::core::PCWSTR) -> io::Result<bool> {
    let mut privilege = LUID_AND_ATTRIBUTES {
        Attributes: SE_PRIVILEGE_ENABLED,
        ..Default::default()
    };
    if unsafe { LookupPrivilegeValueW(null(), name, &mut privilege.Luid) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let state = TOKEN_PRIVILEGES {
        PrivilegeCount: 1,
        Privileges: [privilege],
    };
    let ok = unsafe { AdjustTokenPrivileges(token, 0, &state, 0, null_mut(), null_mut()) };
    let status = unsafe { GetLastError() };
    if ok != 0 && status == ERROR_NOT_ALL_ASSIGNED {
        Ok(false)
    } else if ok != 0 && status == 0 {
        Ok(true)
    } else {
        Err(io::Error::from_raw_os_error(status as i32))
    }
}

struct ThreadToken(Option<OwnedHandle>);

impl Drop for ThreadToken {
    fn drop(&mut self) {
        let original = self
            .0
            .as_ref()
            .map_or(null_mut(), AsRawHandle::as_raw_handle);
        // Continuing on this thread with the temporary privileges would be unsafe.
        if unsafe { SetThreadToken(null(), original) } == 0 {
            std::process::abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::AtomicBool;
    use windows_sys::Win32::Security::{
        AddAuditAccessAceEx, CreateWellKnownSid, GetSecurityDescriptorControl, SE_SACL_PROTECTED,
        WinLocalSystemSid, WinWorldSid,
    };
    use windows_sys::Win32::Storage::FileSystem::FILE_READ_DATA;

    #[test]
    #[ignore = "requires SeRestorePrivilege on an NTFS volume"]
    fn reference_copies_an_owner_different_from_the_current_account() {
        let fixture = tempfile::tempdir().unwrap();
        let source = fixture.path().join("source");
        let target = fixture.path().join("target");
        fs::write(&source, b"data").unwrap();
        fs::create_dir(&target).unwrap();
        let mut sid = [0u32; 17];
        let mut size = size_of_val(&sid) as u32;
        let system = sid.as_mut_ptr().cast();
        assert_ne!(
            unsafe { CreateWellKnownSid(WinLocalSystemSid, null_mut(), system, &mut size) },
            0
        );
        let original =
            Descriptor::read(&File::open(&source).unwrap(), OWNER_SECURITY_INFORMATION).unwrap();
        assert_eq!(
            unsafe { EqualSid(original.owner, system) },
            0,
            "run as an elevated regular account"
        );
        {
            let _privilege = PrivilegedToken::new(true, false)
                .unwrap()
                .install()
                .unwrap()
                .expect("SeRestorePrivilege is required for the fixture");
            let file = open(&source, WRITE_OWNER).unwrap();
            assert_eq!(
                unsafe {
                    SetSecurityInfo(
                        file.as_raw_handle(),
                        SE_FILE_OBJECT,
                        OWNER_SECURITY_INFORMATION,
                        system,
                        null_mut(),
                        null(),
                        null(),
                    )
                },
                0
            );
        }
        let report = files::transfer(
            &[source],
            &target,
            files::Transfer::Copy,
            files::TransferPolicy {
                ntfs: NtfsCopyOptions {
                    owner: true,
                    ..Default::default()
                },
                ..files::Conflict::Fail.into()
            },
            &AtomicBool::new(false),
            None,
            |_| {},
        )
        .unwrap();
        assert_eq!(report.completed, 1, "{report:?}");
        let copied = Descriptor::read(
            &File::open(target.join("source")).unwrap(),
            OWNER_SECURITY_INFORMATION,
        )
        .unwrap();
        assert_ne!(unsafe { EqualSid(copied.owner, system) }, 0);
    }

    #[test]
    #[ignore = "requires an NTFS reference environment with predictable SACL access"]
    fn audit_copy_preserves_acl_or_fails_before_publishing() {
        let fixture = tempfile::tempdir().unwrap();
        let source = fixture.path().join("source");
        let target = fixture.path().join("target");
        fs::create_dir(&source).unwrap();
        fs::create_dir(&target).unwrap();
        let source_file = source.join("data");
        let target_file = target.join("data");
        fs::write(&source_file, b"new").unwrap();
        fs::write(&target_file, b"old").unwrap();
        let policy = files::TransferPolicy {
            ntfs: NtfsCopyOptions {
                audit: true,
                ..Default::default()
            },
            ..files::Conflict::Replace.into()
        };
        let cancel = AtomicBool::new(false);
        let result = PrivilegedToken::new(false, true);
        if let Err(error) = result {
            assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
            assert!(error.to_string().contains("SeSecurityPrivilege"));
            let report = files::transfer(
                &[source_file.clone(), source.clone()],
                &target,
                files::Transfer::Move,
                policy,
                &cancel,
                None,
                |_| {},
            )
            .unwrap();
            assert_eq!(report.completed, 0);
            assert_eq!(report.errors.len(), 2);
            assert_eq!(fs::read(&source_file).unwrap(), b"new");
            assert_eq!(fs::read(&target_file).unwrap(), b"old");
            assert_eq!(fs::read_dir(&target).unwrap().count(), 1);
            return;
        }

        {
            let _privilege = result.unwrap().install().unwrap();
            let file = open(&source_file, ACCESS_SYSTEM_SECURITY).unwrap();
            let mut sid = [0u32; 17];
            let mut sid_size = std::mem::size_of_val(&sid) as u32;
            let mut acl_buffer = [0u32; 64];
            let acl = acl_buffer.as_mut_ptr().cast();
            unsafe {
                assert_ne!(
                    CreateWellKnownSid(
                        WinWorldSid,
                        null_mut(),
                        sid.as_mut_ptr().cast(),
                        &mut sid_size
                    ),
                    0
                );
                assert_ne!(
                    InitializeAcl(acl, std::mem::size_of_val(&acl_buffer) as u32, ACL_REVISION),
                    0
                );
                assert_ne!(
                    AddAuditAccessAceEx(
                        acl,
                        ACL_REVISION,
                        0,
                        FILE_READ_DATA,
                        sid.as_mut_ptr().cast(),
                        1,
                        0
                    ),
                    0
                );
                assert_eq!(
                    SetSecurityInfo(
                        file.as_raw_handle(),
                        SE_FILE_OBJECT,
                        SACL_SECURITY_INFORMATION,
                        null_mut(),
                        null_mut(),
                        null(),
                        acl
                    ),
                    0
                );
            }
        }
        let report = files::transfer(
            &[source],
            &target,
            files::Transfer::Copy,
            policy,
            &cancel,
            None,
            |_| {},
        )
        .unwrap();
        assert!(report.errors.is_empty(), "{report:?}");
        let _privilege = PrivilegedToken::new(false, true)
            .unwrap()
            .install()
            .unwrap();
        let copy = target.join("source/data");
        let descriptor = Descriptor::read(
            &open(&copy, ACCESS_SYSTEM_SECURITY).unwrap(),
            SACL_SECURITY_INFORMATION,
        )
        .unwrap();
        assert!(!descriptor.sacl.is_null());
        assert_eq!(unsafe { (*descriptor.sacl).AceCount }, 1);
        let mut control = 0;
        let mut revision = 0;
        assert_ne!(
            unsafe { GetSecurityDescriptorControl(descriptor.raw, &mut control, &mut revision) },
            0
        );
        assert_ne!(control & SE_SACL_PROTECTED, 0);
    }
}
