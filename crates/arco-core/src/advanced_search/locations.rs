use super::*;
use std::ffi::OsString;

/// Split interactive locations at newlines/semicolons, retaining quoted path segments.
pub fn search_locations(text: &str) -> io::Result<Vec<String>> {
    let mut rows = Vec::new();
    let mut row = String::new();
    let mut quoted = false;
    for ch in text.chars() {
        if ch == '"' {
            quoted = !quoted;
            row.push(ch);
        } else if matches!(ch, ';' | '\n' | '\r') && !quoted {
            if !row.trim().is_empty() {
                rows.push(row.trim().to_owned());
            }
            row.clear();
        } else if ch.is_control() {
            return Err(io::Error::other("Ein Suchort enthält ein Steuerzeichen."));
        } else {
            row.push(ch);
        }
    }
    if quoted {
        return Err(io::Error::other(
            "Ein Anführungszeichen im Suchort ist offen.",
        ));
    }
    if !row.trim().is_empty() {
        rows.push(row.trim().to_owned());
    }
    if rows.len() > 10000 {
        return Err(io::Error::other("Bis zu 10.000 Suchorte sind erlaubt."));
    }
    Ok(rows)
}

pub(super) struct Location {
    pub path: PathBuf,
    pub names: Option<String>,
    pub include_self: bool,
    pub self_only: bool,
}

pub(super) fn expand(
    roots: &[PathBuf],
    include_self: bool,
    depth: Option<usize>,
    cancel: &AtomicBool,
) -> (Vec<Location>, Vec<String>) {
    let mut locations = Vec::new();
    let mut errors = Vec::new();
    for root in roots {
        if files::check_cancel(cancel).is_err() {
            break;
        }
        let text = root.to_string_lossy();
        let Some(list) = text.strip_prefix('@') else {
            locations.push(Location {
                path: root.clone(),
                names: None,
                include_self,
                self_only: false,
            });
            continue;
        };
        let list = Path::new(list.trim_matches('"'));
        let result = (|| -> io::Result<Vec<Location>> {
            let data = tools::read_text_file(list, 4 * 1024 * 1024, cancel)?;
            let base = list.parent().unwrap_or(Path::new("."));
            let mut result = Vec::new();
            for line in data.lines().map(str::trim).filter(|line| !line.is_empty()) {
                files::check_cancel(cancel)?;
                if result.len() >= 10000 {
                    return Err(io::Error::other(
                        "Die Suchortliste überschreitet 10.000 Einträge.",
                    ));
                }
                if line.chars().any(char::is_control) {
                    return Err(io::Error::other(
                        "Die Suchortliste enthält ein Steuerzeichen.",
                    ));
                }
                let line = line.trim_matches('"');
                if let Some(index) = line
                    .find('<')
                    .filter(|index| *index == 0 || line[..*index].ends_with(['\\', '/']))
                {
                    if depth != Some(0) {
                        result.push(Location {
                            path: base.join(&line[..index]),
                            names: Some(line[index..].into()),
                            include_self: false,
                            self_only: false,
                        });
                    }
                    continue;
                }
                let path = base.join(line);
                let name = path.file_name().unwrap_or_default().to_string_lossy();
                let expression = name.starts_with('<') || name.contains(['*', '?']);
                if expression && depth == Some(0) {
                    continue;
                }
                let (path, names) = if expression {
                    (
                        path.parent().unwrap_or(base).to_path_buf(),
                        name.into_owned(),
                    )
                } else {
                    (path, "*".into())
                };
                result.push(Location {
                    path,
                    names: Some(names),
                    include_self: depth == Some(0),
                    self_only: depth == Some(0),
                });
            }
            Ok(result)
        })();
        match result {
            Ok(rows) => locations.extend(rows),
            Err(error) => errors.push(format!("{}: {error}", list.display())),
        }
    }
    (locations, errors)
}

pub fn list_marker(base: &Path, input: &str) -> PathBuf {
    let mut value = OsString::from("@");
    value.push(base.join(input.trim_matches('"')));
    PathBuf::from(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn result_refinement_with_zero_depth_checks_only_the_supplied_entries() {
        let fixture = tempfile::tempdir().unwrap();
        let folder = fixture.path().join("docs");
        fs::create_dir(&folder).unwrap();
        fs::write(folder.join("child.txt"), b"needle").unwrap();
        let file = fixture.path().join("one.txt");
        fs::write(&file, b"needle").unwrap();
        let roots = [folder.clone(), file.clone()];
        let query = Options {
            names: "*.ignored".into(),
            depth: "0".into(),
            ..Default::default()
        }
        .compile()
        .unwrap()
        .only_roots();
        let found = search(&roots, true, &query, true, None, &AtomicBool::new(false));
        assert_eq!(found.directory.entries.len(), 2);
        assert!(
            found
                .directory
                .entries
                .iter()
                .all(|entry| entry.path == folder || entry.path == file)
        );
        let query = Options {
            names: "*.ignored".into(),
            depth: "0".into(),
            content_mode: 1,
            content: "needle".into(),
            ..Default::default()
        }
        .compile()
        .unwrap()
        .only_roots();
        let found = search(&roots, true, &query, true, None, &AtomicBool::new(false));
        assert_eq!(found.directory.entries.len(), 1);
        assert_eq!(found.directory.entries[0].path, file);
    }
    #[test]
    fn quoted_locations_and_bom_lists_keep_the_documented_scope() {
        assert_eq!(
            search_locations("\"C:\\a;b\";D:\\other\r\n@list.txt").unwrap(),
            ["\"C:\\a;b\"", "D:\\other", "@list.txt"]
        );
        assert!(search_locations("\"broken").is_err());
        let fixture = tempfile::tempdir().unwrap();
        let base = fixture.path();
        fs::create_dir_all(base.join("docs/nested")).unwrap();
        fs::write(base.join("docs/a.txt"), b"needle").unwrap();
        fs::write(base.join("docs/b.bin"), b"needle").unwrap();
        let list = base.join("input.txt");
        let mut bytes = vec![0xff, 0xfe];
        for unit in "docs\\a.txt\r\ndocs\\*.bin\r\n".encode_utf16() {
            bytes.extend(unit.to_le_bytes());
        }
        #[cfg(not(windows))]
        {
            bytes = b"\xef\xbb\xbfdocs/a.txt\ndocs/*.bin\n".to_vec();
        }
        fs::write(&list, bytes).unwrap();
        let roots = [list_marker(base, "input.txt")];
        let query = Options {
            names: "*.not-used".into(),
            content_mode: 1,
            content: "needle".into(),
            ..Default::default()
        }
        .compile()
        .unwrap();
        let found = search(&roots, false, &query, true, None, &AtomicBool::new(false));
        assert!(
            found.directory.errors.is_empty(),
            "{:?}",
            found.directory.errors
        );
        assert_eq!(found.directory.entries.len(), 2);
        let query = Options {
            depth: "0".into(),
            ..Default::default()
        }
        .compile()
        .unwrap();
        let found = search(&roots, false, &query, true, None, &AtomicBool::new(false));
        assert_eq!(found.directory.entries.len(), 1);
        assert!(found.directory.entries[0].path.ends_with("a.txt"));
    }
}
