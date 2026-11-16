use super::{Pidl, name, parse_id};
use std::fmt::Write;
use std::io;
use windows::Win32::System::Com::CoTaskMemAlloc;
use windows::Win32::UI::Shell::{ILGetSize, ILIsEqual, SIGDN_FILESYSPATH};

const LIMIT: usize = 64 * 1024;

/// Some Windows providers return localized display names even for the absolute
/// parsing-name request. Retain the absolute ITEMIDLIST when that name cannot be
/// opened again; never derive identities from labels or enumeration positions.
pub(super) fn address(id: &Pidl, preferred: String) -> io::Result<String> {
    if let Ok(restored) = parse_id(&preferred)
        && unsafe { ILIsEqual(id.0, restored.0) }.as_bool()
    {
        return Ok(preferred);
    }
    if let Ok(path) = name(id, SIGDN_FILESYSPATH)
        && let Ok(path) = path.into_string()
        && parse_id(&path).is_ok()
    {
        return Ok(path);
    }
    encode(id)
}

fn encode(id: &Pidl) -> io::Result<String> {
    let size = unsafe { ILGetSize(Some(id.0.cast_const())) } as usize;
    if !(2..=LIMIT).contains(&size) {
        return Err(invalid());
    }
    let bytes = unsafe { std::slice::from_raw_parts(id.0.cast::<u8>(), size) };
    validate(bytes)?;
    let mut address = String::with_capacity(crate::SHELL_PIDL_PREFIX.len() + size * 2);
    address.push_str(crate::SHELL_PIDL_PREFIX);
    for byte in bytes {
        write!(address, "{byte:02x}").expect("write PIDL address");
    }
    Ok(address)
}

pub(super) fn restore(value: &std::ffi::OsStr) -> io::Result<Option<Pidl>> {
    let Some(value) = value.to_str() else {
        return Ok(None);
    };
    let Some(encoded) = value
        .get(..crate::SHELL_PIDL_PREFIX.len())
        .filter(|prefix| prefix.eq_ignore_ascii_case(crate::SHELL_PIDL_PREFIX))
        .map(|_| &value[crate::SHELL_PIDL_PREFIX.len()..])
    else {
        return Ok(None);
    };
    if !(4..=LIMIT * 2).contains(&encoded.len()) || encoded.len() % 2 != 0 {
        return Err(invalid());
    }
    let mut bytes = Vec::with_capacity(encoded.len() / 2);
    for pair in encoded.as_bytes().chunks_exact(2) {
        let digit = |byte: u8| {
            char::from(byte)
                .to_digit(16)
                .map(|digit| digit as u8)
                .ok_or_else(invalid)
        };
        bytes.push(digit(pair[0])? * 16 + digit(pair[1])?);
    }
    validate(&bytes)?;
    let memory = unsafe { CoTaskMemAlloc(bytes.len()) };
    if memory.is_null() {
        return Err(io::Error::other(
            "Windows konnte die Shell-Identität nicht laden.",
        ));
    }
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), memory.cast::<u8>(), bytes.len());
    }
    Ok(Some(Pidl(memory.cast())))
}

fn validate(mut bytes: &[u8]) -> io::Result<()> {
    for _ in 0..512 {
        let header = bytes.get(..2).ok_or_else(invalid)?;
        let size = usize::from(u16::from_le_bytes([header[0], header[1]]));
        if size == 0 {
            return if bytes.len() == 2 {
                Ok(())
            } else {
                Err(invalid())
            };
        }
        if size < 2 {
            return Err(invalid());
        }
        bytes = bytes.get(size..).ok_or_else(invalid)?;
    }
    Err(invalid())
}

fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "Ungültige oder zu große gespeicherte Shell-Identität.",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn absolute_shell_ids_round_trip_and_malformed_ids_are_rejected() {
        let _apartment = super::super::Ole::new().unwrap();
        let id = parse_id(crate::SHELL_APPLICATIONS).unwrap();
        let address = encode(&id).unwrap();
        let restored = restore(address.as_ref()).unwrap().unwrap();
        assert!(unsafe { ILIsEqual(id.0, restored.0) }.as_bool());
        for suffix in ["", "00", "000001", "01000000", "080012340000", "zzzz"] {
            assert!(
                restore(std::ffi::OsStr::new(&format!(
                    "{}{suffix}",
                    crate::SHELL_PIDL_PREFIX
                )))
                .is_err()
            );
        }
        assert!(
            restore(std::ffi::OsStr::new(&format!(
                "{}{}",
                crate::SHELL_PIDL_PREFIX,
                "00".repeat(LIMIT + 1)
            )))
            .is_err()
        );
    }
}
