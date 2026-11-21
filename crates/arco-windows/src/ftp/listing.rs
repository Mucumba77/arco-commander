use super::{Entry, check_segment};

pub(super) fn parse_custom(text: &str, pattern: &regex::Regex) -> std::io::Result<Vec<Entry>> {
    use std::io;
    let mut entries = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let invalid = || {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "FTP-LIST: Zeile {} passt nicht zum Serverformat.",
                    index + 1
                ),
            )
        };
        let fields = pattern.captures(line).ok_or_else(invalid)?;
        let name = fields.name("name").ok_or_else(invalid)?.as_str();
        if matches!(name, "." | "..") {
            continue;
        }
        check_segment(name)?;
        let kind = fields.name("kind").ok_or_else(invalid)?.as_str();
        let directory = match kind.to_ascii_lowercase().as_str() {
            "d" | "dir" | "<dir>" => true,
            "-" | "f" | "file" => false,
            "l" | "link" => continue,
            _ => return Err(invalid()),
        };
        let size = if directory {
            None
        } else {
            fields
                .name("size")
                .filter(|size| !size.is_empty())
                .map(|size| size.as_str().parse::<u64>().map_err(|_| invalid()))
                .transpose()?
        };
        entries.push(Entry {
            name: name.to_owned(),
            directory: Some(directory),
            symlink: false,
            size,
            modified: None,
            unique: None,
        });
    }
    Ok(entries)
}

pub(super) fn parse_list(bytes: &[u8]) -> Option<Vec<Entry>> {
    let text = std::str::from_utf8(bytes).ok()?;
    if text
        .lines()
        .find(|line| !line.trim().is_empty())
        .is_some_and(|line| line.starts_with("Directory "))
    {
        return parse_vms(text);
    }
    let mut entries = Vec::new();
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        if line
            .strip_prefix("total ")
            .is_some_and(|count| count.trim().parse::<u64>().is_ok())
        {
            continue;
        }
        let (name, directory, size) = unix_entry(line).or_else(|| dos_entry(line))?;
        if let Some(directory) = directory
            && check_segment(name).is_ok()
        {
            entries.push(Entry {
                name: name.into(),
                directory: Some(directory),
                symlink: false,
                size,
                modified: None,
                unique: None,
            });
        }
    }
    Some(entries)
}

fn parse_vms(text: &str) -> Option<Vec<Entry>> {
    let mut entries = Vec::new();
    let mut pending = None;
    for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
        if line.starts_with("Directory ") || line.starts_with("Total of ") {
            if pending.is_some() {
                return None;
            }
            continue;
        }
        let mut fields = line.split_ascii_whitespace();
        let name = pending
            .take()
            .unwrap_or_else(|| fields.next().unwrap_or_default());
        let (stem, version) = name.rsplit_once(';')?;
        if stem.is_empty() || version.parse::<u32>().ok()? == 0 {
            return None;
        }
        let Some(blocks) = fields.next() else {
            pending = Some(name);
            continue;
        };
        let used = blocks.split_once('/').map_or(blocks, |(used, _)| used);
        used.parse::<u64>().ok()?;
        let date = fields.next()?;
        let time = fields.next()?;
        if date.split('-').count() != 3 || !time.contains(':') {
            return None;
        }
        let folder = stem
            .get(stem.len().saturating_sub(4)..)
            .is_some_and(|suffix| suffix.eq_ignore_ascii_case(".dir"));
        let name = if folder {
            &stem[..stem.len() - 4]
        } else {
            name
        };
        check_segment(name).ok()?;
        entries.push(Entry {
            name: name.to_owned(),
            directory: Some(folder),
            symlink: false,
            // VMS reports allocated/used blocks. Neither is the exact byte size.
            size: None,
            modified: None,
            unique: None,
        });
    }
    pending.is_none().then_some(entries)
}

type Row<'a> = (&'a str, Option<bool>, Option<u64>);

fn unix_entry(line: &str) -> Option<Row<'_>> {
    let ([mode, links, _, _, size, month, day, time], name) = fields(line)?;
    let kind = mode.as_bytes().first()?;
    if mode.len() < 10
        || links.parse::<u32>().is_err()
        || month.len() != 3
        || day.parse::<u8>().is_err()
        || !time
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b':')
    {
        return None;
    }
    match kind {
        b'd' => Some((name, Some(true), None)),
        b'-' => Some((name, Some(false), Some(size.parse().ok()?))),
        // LIST link targets and special files cannot be safely treated as regular files.
        b'l' | b'b' | b'c' | b'p' | b's' => Some((name, None, None)),
        _ => None,
    }
}

fn dos_entry(line: &str) -> Option<Row<'_>> {
    let ([date, time, size], name) = fields(line)?;
    let mut date_parts = date.split('-');
    let valid_date = date_parts.by_ref().take(3).count() == 3
        && date_parts.next().is_none()
        && date
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b'-');
    if !valid_date || !time.contains(':') {
        return None;
    }
    if size.eq_ignore_ascii_case("<dir>") {
        return Some((name, Some(true), None));
    }
    Some((name, Some(false), Some(size.parse().ok()?)))
}

fn fields<const N: usize>(mut line: &str) -> Option<([&str; N], &str)> {
    let mut values = [""; N];
    for value in &mut values {
        (*value, line) = line
            .trim_start_matches([' ', '\t'])
            .split_once([' ', '\t'])?;
    }
    Some((values, line.trim_start_matches([' ', '\t'])))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vms_preserves_file_versions_and_does_not_invent_byte_sizes() {
        let entries = parse_list(b"Directory DISK:[ARCO]\r\n\r\nBUILD.DIR;1 1/3 01-JAN-2026 13:55\r\nREPORT.TXT;7\r\n 3/4 02-FEB-2026 01:21\r\nTotal of 2 files.\r\n").unwrap();
        assert_eq!(
            (&*entries[0].name, entries[0].directory),
            ("BUILD", Some(true))
        );
        assert_eq!((&*entries[1].name, entries[1].size), ("REPORT.TXT;7", None));
        assert!(parse_list(b"Directory DISK:[ARCO]\nFILE.TXT;1\nTotal of 1 file.\n").is_none());
    }

    #[test]
    fn list_preserves_names_and_recognizes_unix_and_iis_directories() {
        let entries = parse_list(
            b"total 4\r\n\
            drwxr-xr-x 2 user group 4096 Apr 12 07:15 a folder\r\n\
            -rw-r--r-- 1 user group 78 Apr 12 2024 a file.txt\r\n\
            lrwxrwxrwx 1 user group 7 Apr 12 2024 link -> /unsafe\r\n\
            04-12-24  07:15AM       <DIR>          another folder\r\n\
            04-12-24  07:17AM                   89 another file.txt\r\n",
        )
        .unwrap();
        assert_eq!(entries.len(), 4);
        assert_eq!(
            (&*entries[0].name, entries[0].directory),
            ("a folder", Some(true))
        );
        assert_eq!(
            (&*entries[1].name, entries[1].size),
            ("a file.txt", Some(78))
        );
        assert_eq!(
            (&*entries[2].name, entries[2].directory),
            ("another folder", Some(true))
        );
        assert_eq!(
            (&*entries[3].name, entries[3].size),
            ("another file.txt", Some(89))
        );
        assert!(parse_list(b"unknown listing format\r\n").is_none());
    }
}
