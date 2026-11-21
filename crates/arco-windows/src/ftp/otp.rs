//! RFC 2289 and RFC 1760 login on the same control connection as the transfer.
use super::{check_cancel, invalid_input};
use md5::{Digest, Md5};
use sha1::Sha1;
use std::io;
use std::sync::atomic::AtomicBool;
use zeroize::Zeroizing;

pub(super) fn response(
    challenge: &str,
    passphrase: &str,
    cancel: &AtomicBool,
) -> io::Result<Zeroizing<String>> {
    let mut tokens = challenge.split_ascii_whitespace();
    let hash = tokens
        .by_ref()
        .map(|word| word.trim_start_matches(['[', '(']))
        .find(|word| word.starts_with("otp-") || word.eq_ignore_ascii_case("s/key"))
        .ok_or_else(|| {
            invalid_input("Die FTP-Anmeldung bietet keine OTP- oder S/KEY-Challenge an.")
        })?;
    let legacy = hash.eq_ignore_ascii_case("s/key");
    let sequence: u32 = tokens
        .next()
        .and_then(|word| word.parse().ok())
        .filter(|count| *count <= 9999)
        .ok_or_else(|| invalid_input("Ungültige OTP-Sequenz."))?;
    let seed = tokens
        .next()
        .map(|word| word.trim_end_matches([']', ')']))
        .filter(|word| {
            !word.is_empty()
                && (legacy || word.len() <= 16)
                && word.bytes().all(|byte| byte.is_ascii_alphanumeric())
        })
        .ok_or_else(|| invalid_input("Ungültiger OTP-Seed."))?;
    let valid_passphrase = if legacy {
        passphrase.len() >= 9
    } else {
        (10..=63).contains(&passphrase.len())
    };
    if !valid_passphrase || !passphrase.is_ascii() {
        return Err(invalid_input(if legacy {
            "Die S/KEY-Passphrase benötigt mindestens 9 ASCII-Zeichen."
        } else {
            "Die OTP-Passphrase benötigt 10–63 ASCII-Zeichen."
        }));
    }
    let fold: fn(&[u8]) -> io::Result<[u8; 8]> = match if legacy { "otp-md4" } else { hash } {
        "otp-md4" => fold_md4,
        "otp-md5" => |input| {
            let bytes = Md5::digest(input);
            Ok(std::array::from_fn(|index| bytes[index] ^ bytes[index + 8]))
        },
        "otp-sha1" => |input| {
            let bytes = Sha1::digest(input);
            let mut folded = [0; 8];
            for (index, byte) in bytes.into_iter().enumerate() {
                folded[index / 4 % 2 * 4 + (3 - index % 4)] ^= byte;
            }
            Ok(folded)
        },
        _ => {
            return Err(invalid_input(
                "Diese FTP-OTP-Variante wird nicht unterstützt. Wähle RFC 2289 mit MD4, MD5 oder SHA-1.",
            ));
        }
    };
    let seed = if legacy {
        seed.to_owned()
    } else {
        seed.to_ascii_lowercase()
    };
    let initial = Zeroizing::new(format!("{seed}{passphrase}"));
    let mut value = Zeroizing::new(fold(initial.as_bytes())?);
    for _ in 0..sequence {
        check_cancel(cancel)?;
        *value = fold(&*value)?;
    }
    let mut password = Zeroizing::new(String::from("PASS "));
    if legacy {
        append_words(&value, &mut password);
        return Ok(password);
    }
    use std::fmt::Write as _;
    for byte in value.iter() {
        write!(&mut *password, "{byte:02x}").expect("format OTP");
    }
    Ok(password)
}

fn append_words(value: &[u8; 8], password: &mut String) {
    // The standard dictionary is defined by RFC 1760 and RFC 2289 Appendix D.
    let dictionary = include_str!("skey-words.txt");
    let checksum: u128 = value
        .iter()
        .map(|byte| {
            (0..4)
                .map(|pair| u128::from((byte >> (2 * pair)) & 3))
                .sum::<u128>()
        })
        .sum();
    let encoded = Zeroizing::new((u128::from(u64::from_be_bytes(*value)) << 2) | (checksum & 3));
    for word in (0..6).rev() {
        if word != 5 {
            password.push(' ');
        }
        let index = ((*encoded >> (11 * word)) & 2047) as usize;
        password.push_str(
            dictionary
                .split_ascii_whitespace()
                .nth(index)
                .expect("standard OTP dictionary"),
        );
    }
}

#[cfg(windows)]
fn fold_md4(input: &[u8]) -> io::Result<[u8; 8]> {
    use windows_sys::Win32::Security::Cryptography::{BCRYPT_MD4_ALG_HANDLE, BCryptHash};

    let mut digest = Zeroizing::new([0; 16]);
    // The Windows 10/11 CNG MD4 pseudo-handle needs no open or close operation.
    let status = unsafe {
        BCryptHash(
            BCRYPT_MD4_ALG_HANDLE,
            std::ptr::null(),
            0,
            input.as_ptr(),
            input.len() as u32,
            digest.as_mut_ptr(),
            digest.len() as u32,
        )
    };
    if status < 0 {
        return Err(io::Error::other(format!(
            "MD4 für FTP-OTP ist nicht verfügbar (0x{status:08x})."
        )));
    }
    Ok(std::array::from_fn(|index| {
        digest[index] ^ digest[index + 8]
    }))
}

#[cfg(not(windows))]
fn fold_md4(_: &[u8]) -> io::Result<[u8; 8]> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "MD4 für FTP-OTP benötigt Windows.",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc2289_matches_known_responses_without_plaintext_fallback() {
        let cancel = AtomicBool::new(false);
        for (algorithm, count, expected) in [
            #[cfg(windows)]
            ("md4", 0, "d1854218ebbb0b51"),
            #[cfg(windows)]
            ("md4", 1, "63473ef01cd0b444"),
            ("md5", 0, "9e876134d90499dd"),
            ("md5", 99, "50fe1962c4965880"),
            ("sha1", 0, "bb9e6ae1979d8ff4"),
            ("sha1", 99, "87fec7768b73ccf9"),
        ] {
            let challenge = format!("331 otp-{algorithm} {count} TeSt ext");
            assert_eq!(
                *response(&challenge, "This is a test.", &cancel).unwrap(),
                format!("PASS {expected}")
            );
        }
        assert!(response("331 Password", "This is a test.", &cancel).is_err());
        assert!(response("331 otp-md5 10000 test", "This is a test.", &cancel).is_err());
    }

    #[test]
    fn standard_six_word_encoding_includes_checksum() {
        let mut words = String::new();
        append_words(&0x85c43ee03857765b_u64.to_be_bytes(), &mut words);
        assert_eq!(words, "FOWL KID MASH DEAD DUAL OAF");
    }

    #[test]
    #[cfg(windows)]
    fn legacy_skey_answers_with_six_words() {
        let cancel = AtomicBool::new(false);
        assert_eq!(
            *response("331 Password [s/key 1 test]", "This is a test.", &cancel).unwrap(),
            "PASS CARD SAD MINI RYE COL KIN"
        );
        assert!(response("331 s/key 0 bad_seed", "This is a test.", &cancel).is_err());
    }
}
