use crate::network_sync::{FtpEndpoint, scan_metadata};
use arco_core::files::{self, TransferReport};
use arco_core::sync::Scope;
use arco_windows::ftp::{Entry, UnixMetadata};
use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

pub fn edit_selection(
    endpoint: &FtpEndpoint,
    selected: &[Entry],
    edit: UnixMetadata,
    recursive: bool,
    cancel: &AtomicBool,
    mut progress: impl FnMut(&Path, usize, usize),
) -> io::Result<TransferReport> {
    edit.validate(endpoint.0.security())?;
    let mut selected_paths = Vec::with_capacity(selected.len());
    for entry in selected {
        if entry.directory.is_none() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Server-Rechte lassen sich nur für reguläre Dateien und Ordner ändern.",
            ));
        }
        selected_paths.push(PathBuf::from(files::leaf_name(OsStr::new(&entry.name))?));
    }
    if selected.is_empty() {
        return Ok(TransferReport::default());
    }
    let entries = if recursive && selected.iter().any(|entry| entry.directory == Some(true)) {
        let scope = Scope::new(true, &selected_paths)?;
        let entries = scan_metadata(endpoint, &scope, cancel, &mut |path| progress(path, 0, 0))?;
        for (path, entry) in selected_paths.iter().zip(selected) {
            if entries.get(path) != Some(entry) {
                return Err(io::Error::other(format!(
                    "Der Servereintrag „{}“ hat sich seit dem Anzeigen geändert.",
                    entry.name
                )));
            }
        }
        entries.into_iter().rev().collect::<Vec<_>>()
    } else {
        selected_paths
            .into_iter()
            .zip(selected.iter().cloned())
            .collect()
    };
    let mut report = TransferReport::default();
    let total = entries.len();
    // Change children before their folders, whose new mode may restrict traversal.
    for (index, (path, entry)) in entries.into_iter().enumerate() {
        progress(&path, index, total);
        let result = files::check_cancel(cancel).and_then(|()| {
            let parent = endpoint.0.relative_directory(
                &endpoint.1,
                path.parent().expect("validated relative server path"),
            )?;
            endpoint.0.set_unix_metadata(&parent, &entry, edit, cancel)
        });
        match result {
            Ok(()) => report.completed += 1,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                report.cancelled = true;
                break;
            }
            Err(error) => report.errors.push(format!("{}: {error}", path.display())),
        }
        progress(&path, index + 1, total);
    }
    Ok(report)
}

pub fn parse_unix_id(value: &str) -> io::Result<u32> {
    value
        .parse::<u32>()
        .ok()
        .filter(|uid| *uid != u32::MAX && value.bytes().all(|byte| byte.is_ascii_digit()))
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "Gib UID und GID als Dezimalzahl zwischen 0 und 4294967294 ein.",
            )
        })
}
