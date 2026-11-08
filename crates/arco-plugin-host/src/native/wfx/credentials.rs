use super::{PromptText, cancelled, symbol};
use arco_windows::credentials;
use libloading::os::windows::Library;
use std::fs::{self, OpenOptions};
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use zeroize::Zeroizing;

const PLUGIN_NUMBER: i32 = 1;
const CRYPTO_NUMBER: i32 = 1;
const OK: i32 = 0;
const READ_ERROR: i32 = 3;
const WRITE_ERROR: i32 = 4;
const UNSUPPORTED: i32 = 6;
const TEXT_LIMIT: usize = 4096;

const SAVE: i32 = 1;
const LOAD: i32 = 2;
const LOAD_NO_UI: i32 = 3;
const COPY: i32 = 4;
const MOVE: i32 = 5;
const DELETE: i32 = 6;

type Crypt<T> = unsafe extern "system" fn(i32, i32, i32, *const T, *mut T, i32) -> i32;
type SetCrypt<T> = unsafe extern "system" fn(Crypt<T>, i32, i32);

static STORE_DIR: OnceLock<PathBuf> = OnceLock::new();

pub(super) fn initialize(library: &Library, dir: PathBuf, unicode: bool) -> io::Result<()> {
    let wide = unicode
        .then(|| symbol::<SetCrypt<u16>>(library, b"FsSetCryptCallbackW"))
        .flatten();
    let ansi = symbol::<SetCrypt<u8>>(library, b"FsSetCryptCallback");
    if wide.is_none() && ansi.is_none() {
        return Ok(());
    }
    fs::create_dir_all(&dir)?;
    STORE_DIR
        .set(dir)
        .map_err(|_| io::Error::other("WFX-Kennwortspeicher ist bereits eingerichtet."))?;
    // A Windows logon unlocks the vault. Arco has no separate Total Commander master password.
    if let Some(set) = wide {
        unsafe { set(crypt::<u16>, CRYPTO_NUMBER, 0) };
    } else if let Some(set) = ansi {
        unsafe { set(crypt::<u8>, CRYPTO_NUMBER, 0) };
    }
    Ok(())
}

unsafe extern "system" fn crypt<T: PromptText>(
    plugin: i32,
    crypto: i32,
    mode: i32,
    connection: *const T,
    password: *mut T,
    max: i32,
) -> i32 {
    if plugin != PLUGIN_NUMBER || crypto != CRYPTO_NUMBER || cancelled() {
        return UNSUPPORTED;
    }
    let Some(dir) = STORE_DIR.get() else {
        return UNSUPPORTED;
    };
    let Some(connection) = (unsafe { read_text(connection) }).filter(|name| !name.is_empty())
    else {
        return UNSUPPORTED;
    };
    // Both host architectures share the plugin configuration directory and the same lock.
    let Ok(lock) = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join("credentials.lock"))
    else {
        return WRITE_ERROR;
    };
    if lock.lock().is_err() || cancelled() {
        return UNSUPPORTED;
    }
    let target = target_name(dir, &connection);
    match mode {
        SAVE => {
            let Some(secret) = (unsafe { read_text(password) }) else {
                return WRITE_ERROR;
            };
            write_result(credentials::write(&target, &secret))
        }
        LOAD | LOAD_NO_UI => {
            let Some(secret) = credentials::read(&target).ok().flatten() else {
                return READ_ERROR;
            };
            if password.is_null() || max <= 0 || secret.contains('\0') {
                return READ_ERROR;
            }
            let Ok(encoded) = T::encode(&secret) else {
                return READ_ERROR;
            };
            let encoded = Zeroizing::new(encoded);
            if encoded.len() > max as usize {
                return READ_ERROR;
            }
            // The SDK guarantees max writable code units.
            unsafe { std::ptr::copy_nonoverlapping(encoded.as_ptr(), password, encoded.len()) };
            OK
        }
        COPY | MOVE => {
            let Some(destination) =
                (unsafe { read_text(password) }).filter(|name| !name.is_empty())
            else {
                return WRITE_ERROR;
            };
            let Some(secret) = credentials::read(&target).ok().flatten() else {
                return READ_ERROR;
            };
            let destination = target_name(dir, &destination);
            if target == destination {
                return OK;
            }
            write_result(credentials::write(&destination, &secret).and_then(|()| {
                if mode == MOVE {
                    credentials::delete(&target)
                } else {
                    Ok(())
                }
            }))
        }
        DELETE => write_result(credentials::delete(&target)),
        _ => UNSUPPORTED,
    }
}

fn write_result(result: io::Result<()>) -> i32 {
    if result.is_ok() { OK } else { WRITE_ERROR }
}

fn target_name(dir: &Path, connection: &str) -> String {
    let mut hash = blake3::Hasher::new();
    for unit in dir.as_os_str().encode_wide() {
        hash.update(&unit.to_le_bytes());
    }
    hash.update(&[0, 0]);
    hash.update(connection.as_bytes());
    format!("Arco Commander WFX/{}", hash.finalize().to_hex())
}

unsafe fn read_text<T: PromptText>(text: *const T) -> Option<Zeroizing<String>> {
    if text.is_null() {
        return None;
    }
    let mut units = Zeroizing::new(Vec::new());
    for index in 0..TEXT_LIMIT {
        // The SDK supplies a NUL-terminated input. max is defined only for loads.
        let unit = unsafe { *text.add(index) };
        units.push(unit);
        if unit == T::default() {
            let decoded = Zeroizing::new(T::decode(&units[..index]));
            let round_trip = Zeroizing::new(T::encode(&decoded).ok()?);
            return (units.as_slice() == round_trip.as_slice()).then_some(decoded);
        }
    }
    None
}
