use super::ftp_sync::{modified, scan};
use super::{App, FtpEndpoint, LoadedDirectory, Navigation, Outcome, StagedPreview};
use crate::panel::{FtpLocation, FtpTab, Location};
use crate::{display_path, human_size, preview};
use arco_core::archive_view::MAX_PREVIEW;
use arco_core::files::{self, Directory, Kind, TransferReport};
use arco_core::profile::TabLock;
use arco_core::sync::Scope;
use arco_windows::ftp;
use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

pub(crate) enum FtpEdit {
    Create,
    Rename(ftp::Entry),
    Delete(Vec<ftp::Entry>),
    Permissions(Vec<ftp::Entry>),
}

pub(super) fn read_panel(
    location: &FtpLocation,
    cancel: &AtomicBool,
) -> io::Result<LoadedDirectory> {
    let listing = location.session.list(&location.directory, cancel)?;
    let mut directory = Directory {
        entries: Vec::new(),
        errors: Vec::new(),
    };
    let mut names = BTreeSet::new();
    for entry in &listing.entries {
        let path = match files::leaf_name(OsStr::new(&entry.name)) {
            Ok(name) if names.insert(name) => PathBuf::from(name),
            _ => {
                directory.errors.push(format!(
                    "Ungültiger oder doppelter FTP-Name: {}",
                    entry.name
                ));
                continue;
            }
        };
        let kind = match entry.directory {
            Some(true) => Kind::Directory,
            Some(false) => Kind::File,
            None => Kind::Other,
        };
        directory.entries.push(files::Entry::from_remote(
            path,
            kind,
            entry.size.filter(|_| kind == Kind::File),
            modified(entry).ok().flatten(),
        ));
    }
    Ok(LoadedDirectory {
        directory,
        space: None,
        archive: None,
        ftp: Some(FtpTab {
            location: location.clone(),
            listing,
        }),
        shell: Default::default(),
    })
}

impl App {
    pub(crate) fn ftp_endpoint(&self) -> Option<FtpEndpoint> {
        let browser = || {
            self.ftp
                .as_ref()
                .map(|view| (view.session.clone(), view.directory.clone()))
        };
        if self.ui().get_dialog_kind().as_str() == "ftp-browser" {
            return browser();
        }
        [self.active, 1 - self.active]
            .into_iter()
            .find_map(|side| {
                self.panels[side]
                    .current()
                    .ftp
                    .as_ref()
                    .map(|ftp| (ftp.location.session.clone(), ftp.location.directory.clone()))
            })
            .or_else(browser)
    }

    pub(super) fn navigate_ftp_text(&mut self, mut location: FtpLocation, value: &str) {
        let result = (|| -> io::Result<FtpLocation> {
            if value.starts_with("ftp://") || value.starts_with("ftps://") {
                let directory = location.session.directory_url(value)?;
                return Ok(location.at(directory));
            }
            if value.starts_with('/') {
                location.directory = location.session.directory("/")?;
            }
            for part in value.split('/').filter(|part| !part.is_empty()) {
                match part {
                    "." => (),
                    ".." => {
                        if let Some(parent) = location.parent() {
                            location = parent;
                        }
                    }
                    name => {
                        location.directory = location
                            .session
                            .relative_directory(&location.directory, Path::new(name))?;
                    }
                }
            }
            Ok(location)
        })();
        match result {
            Ok(location) => {
                self.load_location(self.active, Location::Ftp(location), Navigation::Jump)
            }
            Err(error) => self.status(error.to_string()),
        }
    }

    pub(crate) fn go_ftp_root(&mut self) {
        if let Some(tab) = &self.current().ftp {
            self.navigate_ftp_text(tab.location.clone(), "/");
        }
    }

    pub(super) fn open_ftp_entry(&mut self, location: FtpLocation, entry: ftp::Entry) {
        if entry.directory == Some(true) {
            match location
                .session
                .relative_directory(&location.directory, Path::new(&entry.name))
            {
                Ok(directory) => self.load_location(
                    self.active,
                    Location::Ftp(location.at(directory)),
                    Navigation::Jump,
                ),
                Err(error) => self.status(error.to_string()),
            }
        } else {
            self.preview_ftp_entry(location, entry, false);
        }
    }

    pub(crate) fn view_ftp_panel_entry(&mut self, separate: bool) {
        let tab = self.current();
        let Some(ftp) = &tab.ftp else {
            return;
        };
        let Some(entry) = tab
            .display_row(tab.cursor)
            .and_then(|entry| ftp.entry(&entry.path))
        else {
            return;
        };
        self.preview_ftp_entry(ftp.location.clone(), entry.clone(), separate);
    }

    fn preview_ftp_entry(&mut self, location: FtpLocation, entry: ftp::Entry, separate: bool) {
        if entry.directory == Some(true) {
            self.status("Wähle eine Datei für die FTP-Vorschau.");
            return;
        }
        if entry.size.is_some_and(|size| size > MAX_PREVIEW) {
            self.status("Die FTP-Vorschau unterstützt bis zu 64 MiB pro Datei.");
            return;
        }
        let source = format!("{}{}", location.directory, entry.name);
        let description =
            super::JobDescription::new(crate::OperationKind::Ftp, source.clone(), String::new());
        self.start_operation_job("FTP-Vorschau laden", false, description, move |job| {
            let storage = Arc::new(tempfile::tempdir()?);
            let path = storage
                .path()
                .join(files::leaf_name(OsStr::new(&entry.name))?);
            location.session.download_bounded_with_progress(
                &location.directory,
                &entry.name,
                &path,
                MAX_PREVIEW,
                &job.cancel,
                job.ftp_download_progress(source, entry.size),
            )?;
            job.phase("Vorschau vorbereiten …");
            let content = preview::read(&path)?;
            let title = format!("{}{}", location.directory, entry.name);
            Ok(Outcome::StagedPreview(
                StagedPreview {
                    storage,
                    path,
                    title,
                    separate,
                },
                content,
            ))
        });
    }

    pub(crate) fn ftp_panel_properties(&mut self) {
        let tab = self.current();
        let Some(ftp) = &tab.ftp else {
            return;
        };
        let description = tab
            .display_row(tab.cursor)
            .and_then(|entry| ftp.entry(&entry.path))
            .map_or_else(
                || ftp.location.directory.to_string(),
                |entry| {
                    format!(
                        "{}{}\n{}\n{}",
                        ftp.location.directory,
                        entry.name,
                        entry.size.map(human_size).unwrap_or_default(),
                        entry
                            .modified
                            .as_deref()
                            .map(|time| format!("MLSD UTC {time}"))
                            .unwrap_or_default()
                    )
                },
            );
        self.text("FTP-Eigenschaften", &description);
    }

    pub(crate) fn disconnect_ftp(&mut self) {
        let Some((session, _)) = self.ftp_endpoint() else {
            return;
        };
        if self.ftp_session_upload_running(&session) {
            self.status("Beende zuerst die aktive FTP-Rückübertragung im Transfermanager.");
            return;
        }
        if self
            .ftp
            .as_ref()
            .is_some_and(|view| Arc::ptr_eq(&view.session, &session))
        {
            self.ftp = None;
        }
        if self
            .fxp_source
            .as_ref()
            .is_some_and(|(source, _)| Arc::ptr_eq(source, &session))
        {
            self.fxp_source = None;
        }
        for side in 0..2 {
            let panel = &mut self.panels[side];
            let mut reload = panel.loading;
            for (index, tab) in panel.tabs.iter_mut().enumerate() {
                let retain = |location: &Location| !matches!(location, Location::Ftp(ftp) if Arc::ptr_eq(&ftp.session, &session));
                tab.back.retain(retain);
                tab.forward.retain(retain);
                if tab.lock_base.as_ref().is_some_and(|base| !retain(base)) {
                    tab.set_lock(TabLock::Unlocked);
                }
                if tab
                    .ftp
                    .as_ref()
                    .is_some_and(|ftp| Arc::ptr_eq(&ftp.location.session, &session))
                {
                    if tab.options.lock == TabLock::Fixed {
                        tab.set_lock(TabLock::Unlocked);
                    }
                    tab.change_location(&Location::Local(tab.path.clone()), None);
                    tab.space = None;
                    tab.populate(Vec::new(), self.profile.show_hidden, String::new());
                    tab.loaded = false;
                    reload |= index == panel.tab;
                }
            }
            if reload {
                self.refresh(side);
            } else {
                self.publish(side);
            }
        }
        self.forget_closed_ftp_tabs(&session);
        let drafts = self.close_ftp_drafts(&session);
        if drafts == 0 {
            self.status("FTP-Verbindung beendet.");
        } else {
            self.status(format!(
                "FTP-Verbindung beendet. {drafts} lokale Arbeitskopien bleiben erhalten."
            ));
        }
    }

    pub(crate) fn execute_ftp_edit(
        &mut self,
        location: FtpLocation,
        edit: FtpEdit,
        name: &str,
    ) -> io::Result<()> {
        match edit {
            FtpEdit::Permissions(entries) => {
                let mode = name.trim();
                if !matches!(mode.len(), 3 | 4)
                    || !mode.bytes().all(|byte| (b'0'..=b'7').contains(&byte))
                {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "Gib drei oder vier Oktalziffern ein, zum Beispiel 644.",
                    ));
                }
                let permissions = u16::from_str_radix(mode, 8).map_err(io::Error::other)?;
                let description = super::JobDescription::new(
                    crate::OperationKind::Ftp,
                    location.directory.to_string(),
                    String::new(),
                );
                self.start_operation_job("FTP-Dateirechte setzen", true, description, move |job| {
                    let mut report = TransferReport::default();
                    let total = entries.len();
                    for (index, entry) in entries.into_iter().enumerate() {
                        job.entry_progress(
                            entry.name.clone(),
                            index,
                            total,
                            "FTP-Dateirechte setzen …",
                        );
                        match location.session.set_unix_permissions(
                            &location.directory,
                            &entry,
                            permissions,
                            &job.cancel,
                        ) {
                            Ok(()) => report.completed += 1,
                            Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                                report.cancelled = true;
                                break;
                            }
                            Err(error) => report.errors.push(format!("{}: {error}", entry.name)),
                        }
                    }
                    Ok(Outcome::Report("FTP-Dateirechte".into(), report))
                });
            }
            FtpEdit::Delete(entries) => {
                let detail = format!(
                    "{} · {}",
                    crate::locale::tr("Ausgewählte Einträge"),
                    entries.len()
                );
                self.start_delete_job("Lösche FTP-Auswahl …", detail, move |job| {
                    let report = delete_selection(
                        location,
                        &entries,
                        &job.cancel,
                        |path, processed, total| {
                            job.deletion_progress(display_path(path), processed, total);
                        },
                    )?;
                    Ok(Outcome::Report("FTP-Löschen".into(), report))
                });
            }
            edit => {
                let name = files::leaf_name(OsStr::new(name))?
                    .to_str()
                    .expect("Unicode name")
                    .to_owned();
                let description = super::JobDescription::new(
                    crate::OperationKind::Ftp,
                    location.directory.to_string(),
                    String::new(),
                );
                self.start_operation_job(
                    "FTP-Verzeichnis bearbeiten",
                    true,
                    description,
                    move |job| {
                        job.entry_progress(name.clone(), 0, 1, "FTP-Verzeichnis bearbeiten …");
                        match edit {
                            FtpEdit::Create => location.session.create_sync_directory(
                                &location.directory,
                                &name,
                                &job.cancel,
                            )?,
                            FtpEdit::Rename(entry) => location.session.rename_sync_entry(
                                &location.directory,
                                &entry,
                                &name,
                                &job.cancel,
                            )?,
                            FtpEdit::Delete(_) | FtpEdit::Permissions(_) => unreachable!(),
                        }
                        Ok(Outcome::Notice(format!(
                            "FTP-Eintrag aktualisiert · {name}"
                        )))
                    },
                );
            }
        }
        Ok(())
    }
}

fn delete_selection(
    location: FtpLocation,
    selected: &[ftp::Entry],
    cancel: &AtomicBool,
    mut progress: impl FnMut(&Path, usize, usize),
) -> io::Result<TransferReport> {
    let paths = selected
        .iter()
        .map(|entry| PathBuf::from(&entry.name))
        .collect::<Vec<_>>();
    let scope = Scope::new(true, &paths)?;
    let endpoint = (location.session, location.directory);
    let (_, entries) = scan(&endpoint, &scope, true, &|_, _| false, cancel, &mut |_| {})?;
    for (path, before) in paths.iter().zip(selected) {
        if entries.get(path) != Some(before) {
            return Err(io::Error::other(format!(
                "Der FTP-Eintrag „{}“ hat sich seit dem Anzeigen geändert.",
                before.name
            )));
        }
    }
    let mut report = TransferReport::default();
    for (index, (path, entry)) in entries.iter().rev().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            report.cancelled = true;
            break;
        }
        progress(path, index, entries.len());
        let result = endpoint
            .0
            .relative_directory(&endpoint.1, path.parent().expect("remote relative path"))
            .and_then(|parent| endpoint.0.delete_sync_entry(&parent, entry, cancel));
        match result {
            Ok(()) => report.completed += 1,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                report.cancelled = true;
                break;
            }
            Err(error) => report.errors.push(format!("{}: {error}", path.display())),
        }
        progress(path, index + 1, entries.len());
    }
    Ok(report)
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    #[ignore = "requires a disposable writable local FTP server"]
    fn mounted_ftp_reads_and_edits_nested_directories() {
        let address = std::env::var("ARCO_TEST_FTP").unwrap();
        let session = Arc::new(
            ftp::Connection::new(&address, "arco", ftp::Security::Plain)
                .unwrap()
                .authenticate("test-only")
                .unwrap(),
        );
        let cancel = AtomicBool::new(false);
        let root = session.initial_directory();
        session
            .create_sync_directory(&root, "panel", &cancel)
            .unwrap();
        let panel = FtpLocation {
            directory: session
                .relative_directory(&root, Path::new("panel"))
                .unwrap(),
            session,
            profile: None,
        };
        panel
            .session
            .create_sync_directory(&panel.directory, "erster Ordner", &cancel)
            .unwrap();
        let listing = read_panel(&panel, &cancel).unwrap();
        assert_eq!(listing.directory.entries[0].kind, Kind::Directory);
        let entry = &listing.ftp.as_ref().unwrap().listing.entries[0];
        panel
            .session
            .rename_sync_entry(&panel.directory, entry, "Zweiter Ordner", &cancel)
            .unwrap();
        let child = panel
            .session
            .relative_directory(&panel.directory, Path::new("Zweiter Ordner"))
            .unwrap();
        assert_eq!(panel.at(child.clone()).parent(), Some(panel.clone()));
        let fixture = tempfile::tempdir().unwrap();
        let file = fixture.path().join("file.txt");
        fs::write(&file, b"remote content").unwrap();
        panel
            .session
            .upload(&child, &file, "file.txt", false, &cancel)
            .unwrap();
        let fresh = read_panel(&panel, &cancel).unwrap();
        let selected = &fresh.ftp.as_ref().unwrap().listing.entries[0];
        let mut progress = Vec::new();
        let report = delete_selection(
            panel.clone(),
            std::slice::from_ref(selected),
            &cancel,
            |_, processed, total| progress.push((processed, total)),
        )
        .unwrap();
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(report.completed, 2);
        assert_eq!(progress.first(), Some(&(0, 2)));
        assert_eq!(progress.last(), Some(&(2, 2)));
        assert!(
            read_panel(&panel, &cancel)
                .unwrap()
                .directory
                .entries
                .is_empty()
        );
    }
}
