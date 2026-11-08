use super::{Entry, check_segment};

pub(super) fn parse_list(bytes: &[u8]) -> Option<Vec<Entry>> {
    let text = std::str::from_utf8(bytes).ok()?;
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
                size,
                modified: None,
                unique: None,
            });
        }
    }
    Some(entries)
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
