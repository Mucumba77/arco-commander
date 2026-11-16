use super::*;
use std::io::Cursor;

pub(super) fn unescape(text: &str) -> io::Result<String> {
    let mut chars = text.chars();
    let mut output = String::new();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            output.push(ch);
            continue;
        }
        output.push(match chars.next() {
            Some('t') => '\t',
            Some('n') => '\n',
            Some('r') => '\r',
            Some('\\') => '\\',
            _ => {
                return Err(io::Error::other(
                    r"Verwende \t, \n, \r oder \\ im Suchtext.",
                ));
            }
        });
    }
    Ok(output)
}

pub(super) fn quantity(text: &str, age: bool) -> io::Result<u64> {
    let split = text
        .find(|ch: char| !ch.is_ascii_digit() && ch != '.' && ch != ',')
        .unwrap_or(text.len());
    let number = text[..split].replace(',', ".");
    let unit = text[split..].trim().to_ascii_lowercase();
    let factor: u128 = if age {
        match unit.as_str() {
            "" | "d" | "day" | "days" | "tage" => 86400,
            "s" | "sec" => 1,
            "m" | "min" => 60,
            "h" | "hours" => 3600,
            "w" | "weeks" => 604800,
            "mo" | "months" => 2592000,
            "y" | "years" => 31536000,
            _ => {
                return Err(io::Error::other(
                    "Zeitraum: Zahl und Einheit min, h, d, w, mo oder y.",
                ));
            }
        }
    } else {
        match unit.as_str() {
            "" | "b" | "bytes" => 1,
            "k" | "kb" | "kib" => 1024,
            "m" | "mb" | "mib" => 1024_u128.pow(2),
            "g" | "gb" | "gib" => 1024_u128.pow(3),
            "t" | "tb" | "tib" => 1024_u128.pow(4),
            _ => {
                return Err(io::Error::other(
                    "Größe: Zahl und Einheit B, KiB, MiB, GiB oder TiB.",
                ));
            }
        }
    };
    let (whole, fraction) = number.split_once('.').unwrap_or((&number, ""));
    if whole.is_empty()
        || fraction.len() > 9
        || !whole
            .bytes()
            .chain(fraction.bytes())
            .all(|ch| ch.is_ascii_digit())
    {
        return Err(io::Error::other(
            "Gib eine positive Zahl mit höchstens neun Nachkommastellen ein.",
        ));
    }
    let scale = 10_u128.pow(fraction.len() as u32);
    let value = whole
        .parse::<u128>()
        .ok()
        .and_then(|n| n.checked_mul(scale))
        .and_then(|n| n.checked_add(fraction.parse::<u128>().unwrap_or(0)))
        .and_then(|n| n.checked_mul(factor))
        .map(|n| n / scale)
        .and_then(|n| u64::try_from(n).ok());
    value.ok_or_else(|| io::Error::other("Die Größen- oder Zeitangabe ist zu groß."))
}

pub(super) fn age(text: &str) -> io::Result<Option<std::time::Duration>> {
    (!text.trim().is_empty())
        .then(|| quantity(text.trim(), true).map(std::time::Duration::from_secs))
        .transpose()
}

pub(super) fn decoded_matches(data: &[u8], encoding: u8, matches: impl Fn(&str) -> bool) -> bool {
    use encoding_rs::{UTF_8, UTF_16BE, UTF_16LE, WINDOWS_1252};
    match encoding {
        1 => matches(&UTF_8.decode_with_bom_removal(data).0),
        2 => matches(&UTF_16LE.decode_with_bom_removal(data).0),
        3 => matches(&UTF_16BE.decode_with_bom_removal(data).0),
        4 => matches(&WINDOWS_1252.decode_without_bom_handling(data).0),
        5 => matches(&oem(data, OEM850)),
        6 => {
            matches(&UTF_8.decode_with_bom_removal(data).0)
                || matches(&UTF_16LE.decode_with_bom_removal(data).0)
                || matches(&UTF_16BE.decode_with_bom_removal(data).0)
                || matches(&WINDOWS_1252.decode_without_bom_handling(data).0)
                || matches(&oem(data, OEM850))
                || matches(&oem(data, OEM437))
        }
        7 => matches(&oem(data, OEM437)),
        _ => matches(&tools::decode_text(data)),
    }
}

fn oem(data: &[u8], table: &str) -> String {
    let table: Vec<_> = table.chars().collect();
    data.iter()
        .map(|byte| {
            if *byte < 128 {
                char::from(*byte)
            } else {
                table[usize::from(*byte - 128)]
            }
        })
        .collect()
}

pub(super) fn is_office(path: &Path) -> bool {
    path.extension().is_some_and(|ext| {
        matches!(
            ext.to_string_lossy().to_ascii_lowercase().as_str(),
            "docx"
                | "docm"
                | "xlsx"
                | "xlsm"
                | "pptx"
                | "pptm"
                | "odt"
                | "ods"
                | "odp"
                | "ott"
                | "epub"
        )
    })
}

pub(super) fn office_matches(
    data: &[u8],
    matches: impl Fn(&str) -> bool,
    cancel: &AtomicBool,
) -> io::Result<bool> {
    let mut archive = zip::ZipArchive::new(Cursor::new(data)).map_err(io::Error::other)?;
    if archive.len() > 10000 {
        return Err(io::Error::other("Zu viele Office-/EPUB-Bestandteile."));
    }
    let mut total = 0u64;
    for index in 0..archive.len() {
        files::check_cancel(cancel)?;
        let mut file = archive.by_index(index).map_err(io::Error::other)?;
        let name = file.name().map_err(io::Error::other)?.to_ascii_lowercase();
        if !name.ends_with(".xml")
            && !name.ends_with(".html")
            && !name.ends_with(".xhtml")
            && !name.ends_with(".htm")
        {
            continue;
        }
        total = total.saturating_add(file.size());
        if total > CONTENT_LIMIT {
            return Err(io::Error::other(
                "Entpackte Office-/EPUB-Texte überschreiten 64 MiB.",
            ));
        }
        let mut text = Vec::new();
        tools::copy_cancelled((&mut file).take(CONTENT_LIMIT + 1), &mut text, cancel)?;
        if matches(&xml_text(&tools::decode_text(&text))) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn xml_text(text: &str) -> String {
    let mut output = String::new();
    let mut chars = text.char_indices().peekable();
    while let Some((index, ch)) = chars.next() {
        if ch == '<' {
            let mut quote = None;
            let mut end = index;
            for (i, ch) in chars.by_ref() {
                end = i;
                if quote == Some(ch) {
                    quote = None;
                } else if quote.is_none() && matches!(ch, '\'' | '"') {
                    quote = Some(ch);
                } else if quote.is_none() && ch == '>' {
                    break;
                }
            }
            let tag = text[index..=end].to_ascii_lowercase();
            if [
                "</p",
                "</w:p",
                "</text:p",
                "</a:p",
                "<br",
                "<w:br",
                "<text:line-break",
                "</tr",
                "</td",
            ]
            .iter()
            .any(|prefix| tag.starts_with(prefix))
            {
                output.push('\n');
            }
        } else {
            output.push(ch);
        }
    }
    // Decode only character entities. DTDs and external entities are never resolved.
    let mut result = String::new();
    let mut rest = output.as_str();
    while let Some((prefix, tail)) = rest.split_once('&') {
        result.push_str(prefix);
        let Some((entity, next)) = tail
            .split_once(';')
            .filter(|(entity, _)| entity.len() <= 12)
        else {
            result.push('&');
            rest = tail;
            continue;
        };
        let value = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some(' '),
            _ => entity
                .strip_prefix("#x")
                .and_then(|n| u32::from_str_radix(n, 16).ok())
                .or_else(|| entity.strip_prefix('#').and_then(|n| n.parse().ok()))
                .and_then(char::from_u32),
        };
        if let Some(value) = value {
            result.push(value);
        } else {
            result.push('&');
            result.push_str(entity);
            result.push(';');
        }
        rest = next;
    }
    result.push_str(rest);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;
    #[test]
    fn independent_legacy_encodings_units_and_markup() {
        assert!(decoded_matches(b"Gr\x81\xe1e", 5, |text| text == "Grüße"));
        assert!(decoded_matches(b"Gr\x81\xe1e", 6, |text| text == "Grüße"));
        assert_eq!(quantity("0.1 GiB", false).unwrap(), 107374182);
        assert_eq!(quantity("1,5 h", true).unwrap(), 5400);
        for text in ["-1", "NaN", "1e3", "1..3", "18446744073709551616"] {
            assert!(quantity(text, false).is_err());
        }
        assert_eq!(
            xml_text("<w:p>Gr<w:r>üße &amp; &#x41;</w:r></w:p>"),
            "Grüße & A\n"
        );
        assert_eq!(unescape(r"a\tb\n\\").unwrap(), "a\tb\n\\");
    }
    #[test]
    fn search_profiles_combine_age_words_inversion_and_office_content() {
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path().join("letter.docx");
        let mut writer = zip::ZipWriter::new(File::create(&path).unwrap());
        writer
            .start_file(
                "word/document.xml",
                zip::write::SimpleFileOptions::default(),
            )
            .unwrap();
        use std::io::Write;
        writer
            .write_all(b"<w:document><w:p>He works <w:r>hard</w:r> &amp; well.</w:p></w:document>")
            .unwrap();
        writer.finish().unwrap();
        let options = Options {
            content: "hard & well".into(),
            content_mode: 1,
            whole_words: true,
            office_xml: true,
            minimum_age: "7 d".into(),
            maximum_age: "14 d".into(),
            ..Default::default()
        };
        let mut query = options.compile().unwrap();
        query.now = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(30 * 86400);
        let mut entry = Entry::from_remote(
            path.clone(),
            Kind::File,
            Some(fs::metadata(&path).unwrap().len()),
            Some(SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(20 * 86400)),
        );
        assert!(query.matches(&entry, &AtomicBool::new(false)).unwrap());
        query.content_not = true;
        assert!(!query.matches(&entry, &AtomicBool::new(false)).unwrap());
        entry.modified = Some(query.now);
        assert!(!query.matches_metadata(&entry));
        assert!(
            Options {
                minimum_age: "3 w".into(),
                maximum_age: "2 d".into(),
                ..Default::default()
            }
            .compile()
            .is_err()
        );
        let query = Options {
            content: r#""PK"03 04"#.into(),
            content_mode: 3,
            ..Default::default()
        }
        .compile()
        .unwrap();
        entry.modified = None;
        assert!(query.matches(&entry, &AtomicBool::new(false)).unwrap());
        let saved = serde_json::to_string(&options).unwrap();
        let restored: Options = serde_json::from_str(&saved).unwrap();
        assert!(restored.whole_words && restored.office_xml);
        assert_eq!(restored.minimum_age, "7 d");
    }
}

const OEM850: &str = "ÇüéâäàåçêëèïîìÄÅÉæÆôöòûùÿÖÜø£Ø×ƒáíóúñÑªº¿®¬½¼¡«»░▒▓│┤ÁÂÀ©╣║╗╝¢¥┐└┴┬├─┼ãÃ╚╔╩╦╠═╬¤ðÐÊËÈıÍÎÏ┘┌█▄¦Ì▀ÓßÔÒõÕµþÞÚÛÙýÝ¯´\u{ad}±‗¾¶§÷¸°¨·¹³²■ ";

const OEM437: &str = "ÇüéâäàåçêëèïîìÄÅÉæÆôöòûùÿÖÜ¢£¥₧ƒáíóúñÑªº¿⌐¬½¼¡«»░▒▓│┤╡╢╖╕╣║╗╝╜╛┐└┴┬├─┼╞╟╚╔╩╦╠═╬╧╨╤╥╙╘╒╓╫╪┘┌█▄▌▐▀αßΓπΣσµτΦΘΩδ∞φε∩≡±≥≤⌠⌡÷≈°∙·√ⁿ²■ ";
