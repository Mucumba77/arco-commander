use crate::advanced_search::Query;
use crate::files::{self, Directory, Entry, Kind};
use crate::ignore::IgnoreList;
use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

const LIMIT: usize = 100_000;
const EXPORT_LIMIT: u64 = 64 * 1024 * 1024;

pub fn search(
    executable: &Path,
    roots: Option<&[PathBuf]>,
    expression: &str,
    query: &Query,
    hidden: bool,
    ignore: Option<&IgnoreList>,
    cancel: &AtomicBool,
) -> io::Result<Directory> {
    if !executable.is_absolute()
        || !executable
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("exe"))
    {
        return Err(io::Error::other(
            "Konfiguriere einen absoluten Pfad zu es.exe.",
        ));
    }
    let mut found = Directory {
        entries: Vec::new(),
        errors: Vec::new(),
    };
    let mut seen = BTreeSet::new();
    if roots.is_some_and(|roots| roots.len() > 128) {
        return Err(io::Error::other(
            "Everything unterstützt hier bis zu 128 Suchorte.",
        ));
    }
    let scopes: Vec<_> = match roots {
        Some(roots) => roots
            .iter()
            .map(|path| fs::canonicalize(path).map(Some))
            .collect::<io::Result<_>>()?,
        None => vec![None],
    };
    for scope in scopes {
        let folder = scope.as_deref().map(|path| {
            if path.is_file() {
                path.parent().unwrap_or(path)
            } else {
                path
            }
        });
        let temp = tempfile::tempdir()?;
        let export = temp.path().join("search.efu");
        run(executable, folder, expression, &export, cancel)?;
        let (rows, size_index) = read_export(&export)?;
        for row in rows {
            files::check_cancel(cancel)?;
            let Some(path) = row.first().map(PathBuf::from) else {
                continue;
            };
            if !path.is_absolute() {
                continue;
            }
            let result = (|| {
                let resolved = fs::canonicalize(&path)?;
                if scope.as_ref().is_some_and(|root| {
                    resolved.strip_prefix(root).map_or(true, |relative| {
                        query.max_depth.is_some_and(|depth| {
                            relative.components().count() > depth.saturating_add(1)
                        })
                    })
                }) || seen.contains(&resolved)
                {
                    return Ok(None);
                }
                let name = path.file_name().unwrap_or_default().to_os_string();
                let mut entry = Entry::read(path.clone(), name)?;
                if entry.kind == Kind::Link
                    || (!hidden && entry.hidden)
                    || ignore.is_some_and(|ignore| {
                        ignore.local(&path, entry.kind == Kind::Directory)
                            || path
                                .ancestors()
                                .skip(1)
                                .any(|parent| ignore.local(parent, true))
                    })
                {
                    return Ok(None);
                }
                if entry.kind == Kind::Directory {
                    entry.size = size_index
                        .and_then(|index| row.get(index))
                        .and_then(|value| value.parse().ok());
                }
                if !query.matches(&entry, cancel)? {
                    return Ok(None);
                }
                seen.insert(resolved);
                Ok::<_, io::Error>(Some(entry))
            })();
            match result {
                Ok(Some(entry)) => found.entries.push(entry),
                Ok(None) => (),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => return Err(error),
                Err(error) => found.errors.push(format!("{} · {error}", path.display())),
            }
            if found.entries.len() >= LIMIT {
                found
                    .errors
                    .push("Everything-Ergebnis auf 100.000 Einträge begrenzt.".into());
                return Ok(found);
            }
        }
    }
    Ok(found)
}

fn run(
    executable: &Path,
    scope: Option<&Path>,
    expression: &str,
    export: &Path,
    cancel: &AtomicBool,
) -> io::Result<()> {
    files::check_cancel(cancel)?;
    if expression.contains(['\r', '\n', '\0']) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Die Everything-Abfrage enthält Steuerzeichen.",
        ));
    }
    let build_command = || {
        let mut command = Command::new(executable);
        command
            .args(["-n", "100000", "-timeout", "5000", "-export-efu"])
            .arg(export);
        if let Some(scope) = scope {
            command.arg("-path").arg(index_path(scope));
        }
        command
    };
    let mut command = build_command();
    if !expression.is_empty() {
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            // ES treats quotes in its raw command line as search syntax.
            // This switch hands the remainder to the search parser without parsing more options.
            command.arg("-search*").raw_arg(format!("<{expression}>"));
        }
        #[cfg(not(windows))]
        command.arg(format!("<{expression}>"));
    }
    let status = run_command(command, cancel)?;
    #[cfg(windows)]
    let status = if status.code() == Some(6) && !expression.is_empty() {
        use std::os::windows::process::CommandExt;
        if !legacy_expression_safe(expression) {
            return Err(io::Error::other(
                "Die Everything-Abfrage benötigt eine es.exe mit Unterstützung für -search*.",
            ));
        }
        let mut command = build_command();
        command.raw_arg(format!("<{expression}>"));
        run_command(command, cancel)?
    } else {
        status
    };
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "Everything-Abfrage fehlgeschlagen (es.exe {status})."
        )))
    }
}

#[cfg(windows)]
fn legacy_expression_safe(expression: &str) -> bool {
    !expression
        .split_whitespace()
        .any(|token| token.trim_start_matches('"').starts_with(['-', '/']))
}

fn run_command(mut command: Command, cancel: &AtomicBool) -> io::Result<std::process::ExitStatus> {
    files::check_cancel(cancel)?;
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    let mut child = command.spawn()?;
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        let ready = files::check_cancel(cancel).and_then(|()| {
            if start.elapsed() >= Duration::from_secs(30) {
                Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "Everything antwortet nicht.",
                ))
            } else {
                Ok(())
            }
        });
        if let Err(error) = ready {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn index_path(path: &Path) -> PathBuf {
    let Some(path_text) = path.to_str() else {
        return path.to_owned();
    };
    if let Some(unc) = path_text.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{unc}"));
    }
    PathBuf::from(path_text.strip_prefix(r"\\?\").unwrap_or(path_text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn legacy_search_does_not_accept_es_options_as_terms() {
        assert!(legacy_expression_safe(
            r#"ext:txt "ein name" | path:C:\Daten"#
        ));
        assert!(!legacy_expression_safe(r#"ext:txt -clear-settings"#));
        assert!(!legacy_expression_safe(r#"ext:txt "/exit""#));
    }

    #[test]
    fn reads_unicode_file_lists_with_a_reordered_header() {
        let export = tempfile::NamedTempFile::new().unwrap();
        fs::write(
            export.path(),
            "\u{feff}Size,Filename,Attributes\r\n22,\"C:\\Größe, geprüft.txt\",32\r\n",
        )
        .unwrap();
        let (rows, size) = read_export(export.path()).unwrap();
        assert_eq!(rows[0][0], r"C:\Größe, geprüft.txt");
        assert_eq!(rows[0][size.unwrap()], "22");
        assert!(csv_line("\"missing closing quote").is_err());
        assert_eq!(
            index_path(Path::new(r"\\?\C:\folder")),
            Path::new(r"C:\folder")
        );
        assert_eq!(
            index_path(Path::new(r"\\?\UNC\server\share")),
            Path::new(r"\\server\share")
        );
    }
}

fn read_export(path: &Path) -> io::Result<(Vec<Vec<String>>, Option<usize>)> {
    let mut input = BufReader::new(File::open(path)?.take(EXPORT_LIMIT + 1));
    let mut line = String::new();
    if input.read_line(&mut line)? == 0 {
        return Err(io::Error::other("Leere Everything-Dateiliste."));
    }
    let header = csv_line(
        line.trim_start_matches('\u{feff}')
            .trim_end_matches(['\r', '\n']),
    )?;
    let name = header
        .iter()
        .rposition(|name| name.eq_ignore_ascii_case("filename"))
        .ok_or_else(|| io::Error::other("Everything-Dateiliste ohne Dateinamen."))?;
    let size = header
        .iter()
        .rposition(|name| name.eq_ignore_ascii_case("size"));
    let mut total = line.len() as u64;
    let mut rows = Vec::new();
    loop {
        line.clear();
        let count = input.read_line(&mut line)?;
        if count == 0 {
            break;
        }
        total += count as u64;
        if total > EXPORT_LIMIT || rows.len() == LIMIT {
            return Err(io::Error::other(
                "Die Everything-Dateiliste überschreitet das Limit.",
            ));
        }
        let mut row = csv_line(line.trim_end_matches(['\r', '\n']))?;
        if row.len() <= name {
            continue;
        }
        row.swap(0, name);
        rows.push(row);
    }
    let size = size.map(|index| if index == 0 { name } else { index });
    Ok((rows, size))
}

fn csv_line(line: &str) -> io::Result<Vec<String>> {
    let mut fields = Vec::new();
    let mut rest = line;
    loop {
        if let Some(quoted) = rest.strip_prefix('"') {
            let mut value = String::new();
            let mut search = quoted;
            loop {
                let (piece, remaining) = search
                    .split_once('"')
                    .ok_or_else(|| io::Error::other("Ungültiges CSV-Feld."))?;
                value.push_str(piece);
                if let Some(after) = remaining.strip_prefix('"') {
                    value.push('"');
                    search = after;
                } else {
                    rest = remaining;
                    break;
                }
            }
            fields.push(value);
            if rest.is_empty() {
                break;
            }
            rest = rest
                .strip_prefix(',')
                .ok_or_else(|| io::Error::other("Ungültiges CSV-Trennzeichen."))?;
        } else if let Some((field, remaining)) = rest.split_once(',') {
            fields.push(field.to_owned());
            rest = remaining;
        } else {
            fields.push(rest.to_owned());
            break;
        }
    }
    Ok(fields)
}
