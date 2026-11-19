//! Read-only access to saved SSH session data for import.

use std::collections::BTreeMap;
use std::io;
use std::ptr::{null, null_mut};
use windows_sys::Win32::Foundation::{
    ERROR_FILE_NOT_FOUND, ERROR_NO_MORE_ITEMS, ERROR_PATH_NOT_FOUND, ERROR_SUCCESS,
};
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_ENUMERATE_SUB_KEYS, KEY_QUERY_VALUE, REG_DWORD, REG_SZ,
    RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RegCloseKey, RegEnumKeyExW, RegEnumValueW, RegGetValueW,
    RegOpenKeyExW,
};

#[derive(Clone, Copy)]
pub enum Source {
    WinScp,
    Putty,
    Kitty,
}

pub struct RegistrySessions {
    pub profiles: Vec<(String, BTreeMap<String, String>)>,
    pub keys: BTreeMap<String, String>,
}

pub fn read(source: Source, fields: &[&str], max_sessions: usize) -> io::Result<RegistrySessions> {
    let root = match source {
        Source::WinScp => r"Software\Martin Prikryl\WinSCP 2",
        Source::Putty => r"Software\SimonTatham\PuTTY",
        Source::Kitty => r"Software\9bis.com\KiTTY",
    };
    read_root(root, fields, max_sessions)
}

fn read_root(root: &str, fields: &[&str], max_sessions: usize) -> io::Result<RegistrySessions> {
    let path = |name| format!("{root}\\{name}");
    let mut profiles = Vec::new();
    let mut keys = BTreeMap::new();
    if let Some(sessions) = Key::open(HKEY_CURRENT_USER, &path("Sessions"))? {
        for name in sessions.names(false, max_sessions)? {
            if let Some(session) = Key::open(sessions.0, &name)? {
                let mut values = BTreeMap::new();
                for &field in fields {
                    if let Some(value) = session.read(field)? {
                        values.insert(field.to_owned(), value);
                    }
                }
                profiles.push((name, values));
            }
        }
    }
    if let Some(cache) = Key::open(HKEY_CURRENT_USER, &path("SshHostKeys"))? {
        for name in cache.names(true, 20_000)? {
            if let Some(value) = cache.read(&name)? {
                keys.insert(name, value);
            }
        }
    }
    Ok(RegistrySessions { profiles, keys })
}

struct Key(HKEY);

impl Key {
    fn open(parent: HKEY, path: &str) -> io::Result<Option<Self>> {
        let path = wide(path);
        let mut opened = null_mut();
        // The input and output storage remain alive for the Win32 call.
        let status = unsafe {
            RegOpenKeyExW(
                parent,
                path.as_ptr(),
                0,
                KEY_QUERY_VALUE | KEY_ENUMERATE_SUB_KEYS,
                &mut opened,
            )
        };
        match status {
            ERROR_SUCCESS => Ok(Some(Self(opened))),
            ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND => Ok(None),
            error => Err(io::Error::from_raw_os_error(error as i32)),
        }
    }

    fn names(&self, values: bool, limit: usize) -> io::Result<Vec<String>> {
        let mut buffer = vec![0u16; if values { 16_384 } else { 256 }];
        let mut names = Vec::new();
        for index in 0..=limit {
            let mut size = buffer.len() as u32;
            // The Win32 APIs receive the length of the caller-owned UTF-16 buffer.
            let status = unsafe {
                if values {
                    RegEnumValueW(
                        self.0,
                        index as u32,
                        buffer.as_mut_ptr(),
                        &mut size,
                        null(),
                        null_mut(),
                        null_mut(),
                        null_mut(),
                    )
                } else {
                    RegEnumKeyExW(
                        self.0,
                        index as u32,
                        buffer.as_mut_ptr(),
                        &mut size,
                        null(),
                        null_mut(),
                        null_mut(),
                        null_mut(),
                    )
                }
            };
            match status {
                ERROR_NO_MORE_ITEMS => return Ok(names),
                ERROR_SUCCESS if index < limit => {
                    let name =
                        String::from_utf16(&buffer[..size as usize]).map_err(io::Error::other)?;
                    names.push(name);
                }
                ERROR_SUCCESS => {
                    return Err(io::Error::other(
                        "Die Registry-Importquelle überschreitet die Eintragsgrenze.",
                    ));
                }
                error => return Err(io::Error::from_raw_os_error(error as i32)),
            }
        }
        unreachable!("registry enumeration completed")
    }

    fn read(&self, name: &str) -> io::Result<Option<String>> {
        let name = wide(name);
        let mut kind = 0;
        let mut buffer = [0u16; 2048];
        let mut size = std::mem::size_of_val(&buffer) as u32;
        // RegGetValueW checks the buffer size and writes strings with a terminator.
        let status = unsafe {
            RegGetValueW(
                self.0,
                null(),
                name.as_ptr(),
                RRF_RT_REG_SZ | RRF_RT_REG_DWORD,
                &mut kind,
                buffer.as_mut_ptr().cast(),
                &mut size,
            )
        };
        match (status, kind) {
            (ERROR_FILE_NOT_FOUND, _) => Ok(None),
            (ERROR_SUCCESS, REG_DWORD) if size == 4 => {
                let value = u32::from(buffer[0]) | (u32::from(buffer[1]) << u16::BITS);
                Ok(Some(value.to_string()))
            }
            (ERROR_SUCCESS, REG_SZ) if size % 2 == 0 => {
                let data = &buffer[..size as usize / 2];
                let text = data
                    .strip_suffix(&[0])
                    .ok_or_else(|| io::Error::other("Registry-Text ohne Abschluss."))?;
                if text.contains(&0) {
                    return Err(io::Error::other(
                        "Registry-Text mit eingebettetem Nullzeichen.",
                    ));
                }
                Ok(Some(String::from_utf16(text).map_err(io::Error::other)?))
            }
            (ERROR_SUCCESS, _) => Err(io::Error::other(
                "Ungültiger Registry-Wert im Verbindungsprofil.",
            )),
            (error, _) => Err(io::Error::from_raw_os_error(error as i32)),
        }
    }
}

impl Drop for Key {
    fn drop(&mut self) {
        // Only handles returned by a successful RegOpenKeyExW are wrapped.
        unsafe { RegCloseKey(self.0) };
    }
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain([0]).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::System::Registry::{
        KEY_SET_VALUE, REG_OPTION_VOLATILE, RegCreateKeyExW, RegDeleteTreeW, RegSetValueExW,
    };

    #[test]
    fn reads_named_session_values_and_the_host_key_cache() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = format!(
            r"Software\ArcoCommander-RegistryReference-{}-{stamp}",
            std::process::id()
        );
        let path_wide = wide(&path);
        struct RemoveTestKey(Vec<u16>);
        impl Drop for RemoveTestKey {
            fn drop(&mut self) {
                // This uniquely named tree was created exclusively for this test.
                unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, self.0.as_ptr()) };
            }
        }
        let _cleanup = RemoveTestKey(path_wide);
        let create = |suffix| {
            let name = wide(&format!("{path}\\{suffix}"));
            let mut key = null_mut();
            let status = unsafe {
                RegCreateKeyExW(
                    HKEY_CURRENT_USER,
                    name.as_ptr(),
                    0,
                    null(),
                    REG_OPTION_VOLATILE,
                    KEY_SET_VALUE,
                    null(),
                    &mut key,
                    null_mut(),
                )
            };
            assert_eq!(status, ERROR_SUCCESS);
            Key(key)
        };
        let write = |key: &Key, name, kind, data: &[u16]| {
            let name = wide(name);
            let status = unsafe {
                RegSetValueExW(
                    key.0,
                    name.as_ptr(),
                    0,
                    kind,
                    data.as_ptr().cast(),
                    std::mem::size_of_val(data) as u32,
                )
            };
            assert_eq!(status, ERROR_SUCCESS);
        };
        {
            let session = create(r"Sessions\Team%20Lab");
            write(&session, "HostName", REG_SZ, &wide("example.test"));
            write(&session, "PortNumber", REG_DWORD, &[2222, 0]);
            write(&session, "Password", REG_SZ, &wide("ignored"));
            let cache = create("SshHostKeys");
            write(
                &cache,
                "rsa2@2222:example.test",
                REG_SZ,
                &wide("key fixture"),
            );
        }
        let source = read_root(&path, &["HostName", "PortNumber"], 10).unwrap();
        assert_eq!(source.profiles.len(), 1);
        let (name, fields) = &source.profiles[0];
        assert_eq!(name, "Team%20Lab");
        assert_eq!(fields.len(), 2);
        assert_eq!(fields["HostName"], "example.test");
        assert_eq!(fields["PortNumber"], "2222");
        assert_eq!(source.keys["rsa2@2222:example.test"], "key fixture");
    }
}
