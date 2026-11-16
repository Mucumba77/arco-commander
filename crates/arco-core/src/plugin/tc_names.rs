use std::{io, sync::atomic::AtomicBool};

pub(super) fn random_number(cancel: &AtomicBool) -> io::Result<i64> {
    // Rejection sampling keeps the documented inclusive 0..99999 range unbiased.
    loop {
        crate::files::check_cancel(cancel)?;
        let mut bytes = [0u8; 4];
        getrandom::fill(&mut bytes).map_err(io::Error::other)?;
        let value = u32::from_le_bytes(bytes);
        let limit = u32::MAX - u32::MAX % 100_000;
        if value < limit {
            return Ok(i64::from(value % 100_000));
        }
    }
}

pub(super) fn decode(text: &str, field: &str) -> io::Result<String> {
    let utf16 = field.ends_with("(utf-16)");
    let utf8 = field.ends_with("(utf-8)");
    let mut output = String::new();
    let mut index = 0;
    while index < text.len() {
        let rest = &text[index..];
        if !rest.starts_with('%') {
            let ch = rest.chars().next().unwrap();
            output.push(ch);
            index += ch.len_utf8();
            continue;
        }
        if utf16 {
            let mut units = Vec::new();
            let mut cursor = index;
            while let Some((unit, used)) = unicode_escape(&text[cursor..]) {
                units.push(unit);
                cursor += used;
            }
            if !units.is_empty() {
                output.push_str(&String::from_utf16(&units).map_err(|_| {
                    io::Error::other("Ungültige UTF-16-Zeichen im kodierten Dateinamen.")
                })?);
                index = cursor;
                continue;
            }
        }
        let mut bytes = Vec::new();
        let mut cursor = index;
        while let Some(byte) = byte_escape(&text[cursor..]) {
            bytes.push(byte);
            cursor += 3;
        }
        if bytes.is_empty() {
            output.push('%');
            index += 1;
            continue;
        }
        if utf8 {
            output.push_str(
                std::str::from_utf8(&bytes).map_err(|_| {
                    io::Error::other("Ungültige UTF-8-Bytes im kodierten Dateinamen.")
                })?,
            );
        } else if utf16 {
            if bytes.len() % 2 != 0 {
                return Err(io::Error::other(
                    "UTF-16 benötigt vollständige Zeichenpaare.",
                ));
            }
            let big = bytes.starts_with(&[0xfe, 0xff])
                || (!bytes.starts_with(&[0xff, 0xfe]) && bytes[0] == 0);
            let units = bytes
                .chunks_exact(2)
                .map(|pair| {
                    if big {
                        u16::from_be_bytes([pair[0], pair[1]])
                    } else {
                        u16::from_le_bytes([pair[0], pair[1]])
                    }
                })
                .collect::<Vec<_>>();
            let units = units.strip_prefix(&[0xfeff]).unwrap_or(&units);
            output.push_str(&String::from_utf16(units).map_err(|_| {
                io::Error::other("Ungültige UTF-16-Bytes im kodierten Dateinamen.")
            })?);
        } else {
            #[cfg(windows)]
            output.push_str(&super::tc_native::decode_ansi(&bytes)?);
            #[cfg(not(windows))]
            output.push_str(&encoding_rs::WINDOWS_1252.decode(&bytes).0);
        }
        index = cursor;
    }
    Ok(output)
}

fn byte_escape(text: &str) -> Option<u8> {
    let bytes = text.as_bytes();
    if bytes.len() < 3 || bytes[0] != b'%' {
        return None;
    }
    Some(((bytes[1] as char).to_digit(16)? * 16 + (bytes[2] as char).to_digit(16)?) as u8)
}
fn unicode_escape(text: &str) -> Option<(u16, usize)> {
    let bytes = text.as_bytes();
    if bytes.first() != Some(&b'%') {
        return None;
    }
    let skip = if bytes.get(1).is_some_and(|ch| *ch == b'u' || *ch == b'U') {
        2
    } else {
        1
    };
    let digits = bytes.get(skip..skip + 4)?;
    let mut unit = 0u16;
    for &digit in digits {
        unit = unit * 16 + (digit as char).to_digit(16)? as u16;
    }
    Some((unit, skip + 4))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn percent_encoded_names_preserve_literals_and_decode_all_three_modes() {
        assert_eq!(
            decode("File%20%23+name", "decode html characters (text)").unwrap(),
            "File #+name"
        );
        assert_eq!(
            decode("caf%C3%A9%20%E6%9D%B1", "decode html characters (utf-8)").unwrap(),
            "café 東"
        );
        for input in [
            "%u0041%uD83D%uDE00",
            "%0041%D83D%DE00",
            "%41%00%3D%D8%00%DE",
            "%FE%FF%00%41%D8%3D%DE%00",
        ] {
            assert_eq!(
                decode(input, "decode html characters (utf-16)").unwrap(),
                "A😀",
                "{input}"
            );
        }
        assert_eq!(
            decode("raw%xx+name", "decode html characters (text)").unwrap(),
            "raw%xx+name"
        );
        assert!(decode("%FF", "decode html characters (utf-8)").is_err());
        assert!(decode("%uD800", "decode html characters (utf-16)").is_err());
        let cancel = AtomicBool::new(false);
        for _ in 0..100 {
            assert!((0..100_000).contains(&random_number(&cancel).unwrap()));
        }
        assert!(random_number(&AtomicBool::new(true)).is_err());
    }
}
