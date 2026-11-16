use super::*;

#[derive(Clone)]
struct Pattern {
    regex: Regex,
    path: bool,
    absolute: bool,
    no_extension: bool,
    root_only: bool,
}

impl Pattern {
    fn new(text: &str, directory: bool) -> io::Result<Self> {
        let mut text = text.replace('\\', "/");
        let anchored = text.starts_with('/');
        let absolute = text.as_bytes().get(1) == Some(&b':') || text.starts_with("//");
        if anchored && !absolute {
            text.remove(0);
        }
        let root_only = directory && text.is_empty();
        let recursive = directory && text.ends_with("/**");
        if recursive {
            text.truncate(text.len() - 3);
        }
        let path = directory || anchored || absolute || text.contains('/');
        let no_extension = !directory && text.ends_with('.') && text != "*.*";
        if no_extension {
            text.pop();
        }
        if text == "*.*" {
            text = "*".into();
        }
        if !path && !text.contains(['.', '*', '?']) {
            text = format!("*{text}*");
        }
        let mut expression = String::from("^");
        if path && !anchored && !absolute && !root_only {
            expression.push_str("(?:.*/)?");
        }
        let mut chars = text.chars().peekable();
        while let Some(ch) = chars.next() {
            match ch {
                '*' if chars.peek() == Some(&'*') => {
                    while chars.peek() == Some(&'*') {
                        chars.next();
                    }
                    if chars.peek() == Some(&'/') {
                        chars.next();
                        expression.push_str("(?:.*/)?");
                    } else {
                        expression.push_str(".*");
                    }
                }
                '*' => expression.push_str("[^/]*"),
                '?' => expression.push_str("[^/]"),
                ch => expression.push_str(&regex::escape(&ch.to_string())),
            }
        }
        if recursive {
            expression.push_str("(?:/.*)?");
        }
        expression.push('$');
        Ok(Self {
            regex: RegexBuilder::new(&expression)
                .case_insensitive(true)
                .build()
                .map_err(io::Error::other)?,
            path,
            absolute,
            no_extension,
            root_only,
        })
    }
    fn matches(&self, relative: &Path, absolute: &Path) -> bool {
        let candidate = if self.absolute {
            absolute
        } else if self.path {
            relative
        } else {
            Path::new(relative.file_name().unwrap_or_default())
        };
        if self.no_extension && candidate.extension().is_some() {
            return false;
        }
        self.regex
            .is_match(&candidate.to_string_lossy().replace('\\', "/"))
    }
}

#[derive(Clone)]
pub(super) struct SearchMasks {
    files: Vec<Pattern>,
    excluded_files: Vec<Pattern>,
    directories: Vec<Pattern>,
    excluded_directories: Vec<Pattern>,
    pub remove_empty: bool,
}

fn terms(text: &str) -> io::Result<Vec<String>> {
    let mut terms = Vec::new();
    let mut term = String::new();
    let mut quoted = false;
    for ch in text.chars() {
        if ch == '"' {
            quoted = !quoted;
        } else if (ch.is_whitespace() || ch == ';') && !quoted {
            if !term.is_empty() {
                terms.push(std::mem::take(&mut term));
            }
        } else {
            term.push(ch);
        }
    }
    if quoted {
        return Err(io::Error::other(
            "Die Suchmaske enthält ein offenes Anführungszeichen.",
        ));
    }
    if !term.is_empty() {
        terms.push(term);
    }
    if terms.len() > 256 {
        return Err(io::Error::other("Bis zu 256 Suchmasken sind erlaubt."));
    }
    Ok(terms)
}

impl SearchMasks {
    pub fn new(text: &str) -> io::Result<Self> {
        if text.len() > 8192 {
            return Err(io::Error::other("Die Suchmasken überschreiten 8192 Bytes."));
        }
        let (include, exclude) = text.split_once('|').unwrap_or((text, ""));
        if exclude.contains('|') {
            return Err(io::Error::other(
                "Verwende ein einzelnes | vor den Ausschlüssen.",
            ));
        }
        let mut result = Self {
            files: Vec::new(),
            excluded_files: Vec::new(),
            directories: Vec::new(),
            excluded_directories: Vec::new(),
            remove_empty: false,
        };
        for (excluded, text) in [(false, include), (true, exclude)] {
            let mut masks = terms(text)?;
            if !excluded
                && exclude.is_empty()
                && !text.contains(['"', ';'])
                && text.trim().contains(' ')
                && masks.len() > 1
            {
                masks.push(text.trim().into());
            }
            for mask in masks {
                let directory = mask.ends_with(['\\', '/']);
                if excluded && matches!(mask.as_str(), "**\\" | "**/") {
                    result.remove_empty = true;
                    continue;
                }
                let mask = if directory {
                    &mask[..mask.len() - 1]
                } else {
                    &mask
                };
                let pattern = Pattern::new(mask, directory)?;
                match (excluded, directory) {
                    (false, false) => &mut result.files,
                    (true, false) => &mut result.excluded_files,
                    (false, true) => &mut result.directories,
                    (true, true) => &mut result.excluded_directories,
                }
                .push(pattern);
            }
        }
        Ok(result)
    }
    fn relative<'a>(path: &'a Path, root: Option<&Path>) -> &'a Path {
        root.and_then(|root| path.strip_prefix(root).ok())
            .unwrap_or(path)
    }
    pub fn visits(&self, path: &Path, root: Option<&Path>) -> bool {
        let relative = Self::relative(path, root);
        !self
            .excluded_directories
            .iter()
            .filter(|pattern| !pattern.root_only)
            .any(|pattern| {
                relative
                    .ancestors()
                    .filter(|path| !path.as_os_str().is_empty())
                    .any(|parent| {
                        let absolute = root
                            .map(|root| root.join(parent))
                            .unwrap_or_else(|| parent.to_path_buf());
                        pattern.matches(parent, &absolute)
                    })
            })
    }
    pub fn matches(&self, entry: &Entry, root: Option<&Path>) -> bool {
        let path = Self::relative(&entry.path, root);
        let parent = path.parent().unwrap_or(Path::new(""));
        if !self.visits(&entry.path, root) {
            return false;
        }
        if self
            .excluded_directories
            .iter()
            .filter(|pattern| pattern.root_only)
            .any(|pattern| pattern.matches(parent, entry.path.parent().unwrap_or(&entry.path)))
        {
            return false;
        }
        if !self.directories.is_empty()
            && !self
                .directories
                .iter()
                .any(|pattern| pattern.matches(parent, entry.path.parent().unwrap_or(&entry.path)))
        {
            return false;
        }
        (self.files.is_empty()
            || self
                .files
                .iter()
                .any(|pattern| pattern.matches(path, &entry.path)))
            && !self
                .excluded_files
                .iter()
                .any(|pattern| pattern.matches(path, &entry.path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn traversal_prunes_excluded_directories_and_removes_empty_result_folders() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path();
        fs::create_dir_all(root.join("windows/sub")).unwrap();
        fs::create_dir_all(root.join("docs/empty")).unwrap();
        fs::write(root.join("windows/sub/hidden.ini"), b"ignored").unwrap();
        fs::write(root.join("docs/test.ini"), b"wanted").unwrap();
        let query = Options {
            names: r"*.ini | windows\".into(),
            ..Default::default()
        }
        .compile()
        .unwrap();
        let mut visited = Vec::new();
        let found = search_with_progress(
            &[root.to_path_buf()],
            false,
            &query,
            true,
            None,
            &AtomicBool::new(false),
            |update| visited.push(update.path.to_path_buf()),
        );
        assert!(found.directory.errors.is_empty());
        assert_eq!(found.directory.entries.len(), 1);
        assert!(
            !visited
                .iter()
                .any(|path| path.starts_with(root.join("windows")))
        );
        let query = Options {
            names: r"* | **\".into(),
            ..Default::default()
        }
        .compile()
        .unwrap();
        let found = search(
            &[root.to_path_buf()],
            false,
            &query,
            true,
            None,
            &AtomicBool::new(false),
        );
        assert!(
            found
                .directory
                .entries
                .iter()
                .any(|entry| entry.path == root.join("docs"))
        );
        assert!(
            !found
                .directory
                .entries
                .iter()
                .any(|entry| entry.path == root.join("docs/empty"))
        );
    }
    #[test]
    fn manufacturer_masks_keep_path_depth_quotes_and_exclusions_distinct() {
        let root = Path::new("root");
        let matches = |masks: &str, name: &str| {
            let path = root.join(name);
            SearchMasks::new(masks).unwrap().matches(
                &Entry::from_remote(path, Kind::File, Some(1), None),
                Some(root),
            )
        };
        assert!(matches("meier", "Brief an Meier wegen Bestellung.doc"));
        assert!(matches("*.bak *.sik *.old", "data.sik"));
        assert!(matches("*.*", "README"));
        assert!(matches(r"\*.txt", "root.txt"));
        assert!(!matches(r"\*.txt", "nested/child.txt"));
        assert!(matches(
            r"windows\ system32\ *.ini",
            "deep/windows/test.ini"
        ));
        assert!(!matches(
            r"windows\ system32\ *.ini",
            "deep/windows/sub/test.ini"
        ));
        assert!(matches(r"windows\**\ *.ini", "deep/windows/sub/test.ini"));
        assert!(!matches(r"\windows\ *.ini", "deep/windows/test.ini"));
        assert!(matches(r"\windows\ *.ini", "windows/test.ini"));
        assert!(matches(r"dir1\**\dir2\*.dll", "dir1/dir2/a.dll"));
        assert!(matches(r"dir1\**\dir2\*.dll", "other/dir1/sub/dir2/a.dll"));
        assert!(!matches(r"*.ini | windows\", "windows/sub/a.ini"));
        assert!(matches(
            r#""Brief an Meier.doc" | *.bak"#,
            "Brief an Meier.doc"
        ));
        assert!(matches(r"totalcmd\**\ | \", "totalcmd/a.txt"));
        assert!(!matches(r"totalcmd\**\ | \", "a.txt"));
        assert!(SearchMasks::new("\"bad").is_err());
    }
}
