//! Opt-in, per-user association for Arco's own session URLs.

use std::io;
use std::path::Path;
use std::ptr::{null, null_mut};
use windows_sys::Win32::Foundation::{
    ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, ERROR_SUCCESS, WIN32_ERROR,
};
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CLASSES_ROOT, HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_READ, KEY_WRITE,
    REG_CREATED_NEW_KEY, REG_DWORD, REG_OPTION_NON_VOLATILE, REG_SZ, RRF_RT_REG_DWORD, RegCloseKey,
    RegCreateKeyExW, RegDeleteTreeW, RegGetValueW, RegOpenKeyExW, RegSetKeyValueW,
};
use windows_sys::Win32::UI::Shell::{SHCNE_ASSOCCHANGED, SHCNF_IDLIST, SHChangeNotify};

const SCHEME: &str = "arco";
const REGISTRY_PATH: &str = r"Software\Classes\arco";
const MARKER: &str = "ArcoCommanderSessionProtocol";

pub fn install(executable: &Path, profile: &Path) -> io::Result<()> {
    let command = open_command(executable, profile)?;
    if Key::open(HKEY_CLASSES_ROOT, SCHEME)?.is_some() && !ours(REGISTRY_PATH)? {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "Windows hat arco:// bereits einer anderen Anwendung zugeordnet.",
        ));
    }
    install_at(REGISTRY_PATH, &command, REG_OPTION_NON_VOLATILE)?;
    notify();
    Ok(())
}

pub fn remove() -> io::Result<()> {
    if ours(REGISTRY_PATH)? {
        delete(REGISTRY_PATH)?;
        notify();
    }
    Ok(())
}

fn open_command(executable: &Path, profile: &Path) -> io::Result<String> {
    fn argument(path: &Path) -> io::Result<&str> {
        path.to_str()
            .filter(|text| {
                path.is_absolute()
                    && !text.contains(['%', '"'])
                    && !text.ends_with('\\')
                    && !text.chars().any(char::is_control)
            })
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "Der Protokollhandler benötigt absolute, aufrufbare Windows-Pfade.",
                )
            })
    }
    Ok(format!(
        r#""{}" --profile "{}" --arco-link "%1""#,
        argument(executable)?,
        argument(profile)?
    ))
}

fn install_at(path: &str, command: &str, options: u32) -> io::Result<()> {
    let path_wide = wide(path);
    let mut root = null_mut();
    let mut disposition = 0;
    // Open only the per-user path passed by the caller. The protocol handler runs as this user.
    status(unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            path_wide.as_ptr(),
            0,
            null(),
            options,
            KEY_READ | KEY_WRITE,
            null(),
            &mut root,
            &mut disposition,
        )
    })?;
    let root = Key(root);
    let created = disposition == REG_CREATED_NEW_KEY;
    if !created && !root.owned()? {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "Der vorhandene URL-Handler gehört einer anderen Anwendung.",
        ));
    }
    let open = create_subkey(&root, r"shell\open\command", options);
    let result = open.and_then(|open| {
        if created {
            root.string("", "URL:Arco Commander")?;
            root.string("URL Protocol", "")?;
        }
        open.string("", command)?;
        if created {
            let one = 1u32.to_ne_bytes();
            root.set(MARKER, REG_DWORD, &one)?;
        }
        Ok(())
    });
    drop(root);
    if let Err(error) = result {
        if created && let Err(rollback) = delete(path) {
            return Err(io::Error::other(format!(
                "{error}. Der neue Protokolleintrag konnte nicht zurückgenommen werden: {rollback}"
            )));
        }
        return Err(error);
    }
    Ok(())
}

fn create_subkey(root: &Key, name: &str, options: u32) -> io::Result<Key> {
    let name = wide(name);
    let mut key = null_mut();
    status(unsafe {
        RegCreateKeyExW(
            root.0,
            name.as_ptr(),
            0,
            null(),
            options,
            KEY_WRITE,
            null(),
            &mut key,
            null_mut(),
        )
    })?;
    Ok(Key(key))
}

fn ours(path: &str) -> io::Result<bool> {
    Key::open(HKEY_CURRENT_USER, path)?.map_or(Ok(false), |key| key.owned())
}

struct Key(HKEY);

impl Key {
    fn open(root: HKEY, path: &str) -> io::Result<Option<Self>> {
        let path = wide(path);
        let mut key = null_mut();
        match unsafe { RegOpenKeyExW(root, path.as_ptr(), 0, KEY_QUERY_VALUE, &mut key) } {
            ERROR_SUCCESS => Ok(Some(Self(key))),
            ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND => Ok(None),
            code => Err(io::Error::from_raw_os_error(code as i32)),
        }
    }

    fn owned(&self) -> io::Result<bool> {
        let marker = wide(MARKER);
        let mut value = 0u32;
        let mut size = 4;
        match unsafe {
            RegGetValueW(
                self.0,
                null(),
                marker.as_ptr(),
                RRF_RT_REG_DWORD,
                null_mut(),
                (&mut value as *mut u32).cast(),
                &mut size,
            )
        } {
            ERROR_SUCCESS => Ok(size == 4 && value == 1),
            ERROR_FILE_NOT_FOUND => Ok(false),
            code => Err(io::Error::from_raw_os_error(code as i32)),
        }
    }

    fn set(&self, name: &str, kind: u32, bytes: &[u8]) -> io::Result<()> {
        let name = wide(name);
        status(unsafe {
            RegSetKeyValueW(
                self.0,
                null(),
                name.as_ptr(),
                kind,
                bytes.as_ptr().cast(),
                bytes.len() as u32,
            )
        })
    }

    fn string(&self, name: &str, value: &str) -> io::Result<()> {
        let bytes = wide(value)
            .into_iter()
            .flat_map(u16::to_ne_bytes)
            .collect::<Vec<_>>();
        self.set(name, REG_SZ, &bytes)
    }
}

impl Drop for Key {
    fn drop(&mut self) {
        // All wrapped handles were returned by a successful registry call.
        unsafe { RegCloseKey(self.0) };
    }
}

fn delete(path: &str) -> io::Result<()> {
    let name = wide(path);
    let code = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, name.as_ptr()) };
    if matches!(code, ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND) {
        Ok(())
    } else {
        status(code)
    }
}

fn notify() {
    unsafe { SHChangeNotify(SHCNE_ASSOCCHANGED as i32, SHCNF_IDLIST, null(), null()) };
}

fn status(code: WIN32_ERROR) -> io::Result<()> {
    if code == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(code as i32))
    }
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain([0]).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::System::Registry::REG_OPTION_VOLATILE;

    #[test]
    fn writes_and_releases_only_a_disposable_owned_registry_key() {
        let id = unsafe { windows::Win32::System::Com::CoCreateGuid() }
            .unwrap()
            .to_u128();
        let path = format!(r"Software\Arco-SessionProtocol-Reference-{id:x}");
        struct Cleanup(String);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = delete(&self.0);
            }
        }
        let _cleanup = Cleanup(path.clone());
        let command = open_command(
            Path::new(r"C:\Tools & Test\arco-commander.exe"),
            Path::new(r"C:\Data\Arco Profil.json"),
        )
        .unwrap();
        assert_eq!(
            command,
            r#""C:\Tools & Test\arco-commander.exe" --profile "C:\Data\Arco Profil.json" --arco-link "%1""#
        );
        install_at(&path, &command, REG_OPTION_VOLATILE).unwrap();
        assert!(ours(&path).unwrap());
        assert!(
            Key::open(HKEY_CURRENT_USER, &format!(r"{path}\shell\open\command"))
                .unwrap()
                .is_some()
        );
        delete(&path).unwrap();
        assert!(!ours(&path).unwrap());
    }
}
