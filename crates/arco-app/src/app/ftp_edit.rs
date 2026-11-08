use super::{App, Outcome};
use crate::commands::{Dialog, Pending};
use crate::display_path;
use crate::panel::FtpLocation;
use arco_core::{files, tools};
use arco_windows::ftp::{Entry, Session, SyncUpload};
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tempfile::NamedTempFile;

const MAX_EDIT_SIZE: u64 = 64 * 1024 * 1024;

#[derive(Clone)]
pub(crate) struct Draft {
    location: FtpLocation,
    entry: Entry,
    path: PathBuf,
    baseline: String,
}

impl Draft {
    fn download(
        location: FtpLocation,
        entry: Entry,
        recovery: &Path,
        cancel: &AtomicBool,
    ) -> io::Result<Self> {
        if !matches!(entry.size, Some(size) if size <= MAX_EDIT_SIZE)
            || entry.directory != Some(false)
        {
            return Err(io::Error::other(
                "Wähle eine FTP-Datei mit MLSD-Metadaten und höchstens 64 MiB.",
            ));
        }
        let name = files::leaf_name(OsStr::new(&entry.name))?;
        fs::create_dir_all(recovery)?;
        let directory = tempfile::Builder::new()
            .prefix("bearbeitung-")
            .tempdir_in(recovery)?;
        let path = directory.path().join(name);
        let file = File::create_new(&path)?;
        location
            .session
            .fetch_sync(&location.directory, &entry, &file, cancel)?;
        file.sync_all()?;
        let baseline = checksum(&path, cancel)?;
        drop(file);
        let _ = directory.keep();
        Ok(Self {
            location,
            entry,
            path,
            baseline,
        })
    }

    fn upload(&mut self, cancel: &AtomicBool) -> io::Result<bool> {
        let mut staged = NamedTempFile::new()?;
        let size = io::copy(
            &mut open_edit(&self.path)?.take(MAX_EDIT_SIZE + 1),
            staged.as_file_mut(),
        )?;
        if size > MAX_EDIT_SIZE {
            return Err(io::Error::other(
                "FTP-Arbeitskopien dürfen höchstens 64 MiB groß sein.",
            ));
        }
        staged.as_file_mut().flush()?;
        let edited = checksum(staged.path(), cancel)?;
        if edited == self.baseline {
            return Ok(false);
        }
        self.entry = self.location.session.upload_sync(
            &self.location.directory,
            SyncUpload {
                source: staged.path(),
                name: &self.entry.name,
                expected: Some(&self.entry),
                size,
                modified_utc: None,
            },
            || {
                // Check content again after staging, including changes with the same size and time.
                let current = NamedTempFile::new()?;
                self.location.session.fetch_sync(
                    &self.location.directory,
                    &self.entry,
                    current.as_file(),
                    cancel,
                )?;
                if checksum(current.path(), cancel)? != self.baseline {
                    return Err(io::Error::other(
                        "Die Serverdatei wurde seit der letzten Übertragung geändert.",
                    ));
                }
                if tools::checksum_file(open_edit(&self.path)?, tools::Hash::Blake3, cancel)?
                    != edited
                {
                    return Err(io::Error::other(
                        "Die Arbeitskopie wurde während der Übertragung erneut gespeichert.",
                    ));
                }
                Ok(())
            },
            cancel,
        )?;
        self.baseline = edited;
        Ok(true)
    }
}

fn open_edit(path: &Path) -> io::Result<File> {
    check_workspace(path)?;
    let metadata = fs::symlink_metadata(path)?;
    if files::is_link(&metadata) || !metadata.is_file() {
        return Err(io::Error::other(
            "Die FTP-Arbeitskopie muss eine reguläre lokale Datei sein.",
        ));
    }
    if metadata.len() > MAX_EDIT_SIZE {
        return Err(io::Error::other(
            "FTP-Arbeitskopien dürfen höchstens 64 MiB groß sein.",
        ));
    }
    File::open(path)
}

fn check_workspace(path: &Path) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("Arbeitsordner fehlt."))?;
    let metadata = fs::symlink_metadata(parent)?;
    if !metadata.is_dir() || files::is_link(&metadata) {
        return Err(io::Error::other("Der Arbeitsordner wurde ersetzt."));
    }
    Ok(())
}

fn checksum(path: &Path, cancel: &AtomicBool) -> io::Result<String> {
    tools::checksum(path, tools::Hash::Blake3, cancel)
}

fn recovery_directory() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map_or_else(std::env::temp_dir, PathBuf::from)
        .join("Arco Commander")
        .join("ftp-edits")
}

impl App {
    pub(crate) fn edit_ftp_file(&mut self) {
        if self.panels[self.active].loading || self.job.is_some() {
            self.status("Warte, bis das FTP-Verzeichnis und der laufende Auftrag fertig sind.");
            return;
        }
        let tab = self.current();
        let Some(ftp) = &tab.ftp else { return };
        let Some(entry) = tab
            .display_row(tab.cursor)
            .and_then(|row| ftp.entry(&row.path))
            .cloned()
        else {
            return;
        };
        if let Some(path) = self
            .ftp_drafts
            .iter()
            .find(|draft| draft.location == ftp.location && draft.entry.name == entry.name)
            .map(|draft| draft.path.clone())
        {
            self.review_ftp_draft(&path);
            return;
        }
        if entry.directory != Some(false) {
            self.status("Wähle eine Datei in einem FTP-Verzeichnis mit MLSD.");
            return;
        }
        let location = ftp.location.clone();
        self.start_job("Lade FTP-Arbeitskopie …", false, move |job| {
            let draft = Draft::download(location, entry, &recovery_directory(), &job.cancel)?;
            Ok(Outcome::FtpEditReady(Box::new(draft)))
        });
    }

    pub(super) fn open_downloaded_ftp_draft(&mut self, draft: Draft) {
        let path = draft.path.clone();
        self.ftp_drafts.push(draft);
        if let Err(error) = arco_windows::edit(&path, self.profile.editor.as_deref()) {
            self.status(format!(
                "Editor konnte nicht geöffnet werden. Arbeitskopie unter {} · {error}",
                display_path(&path)
            ));
            return;
        }
        let ui = self.ui();
        if ui.get_dialog_kind().is_empty() && !ui.get_settings_open() && ui.get_drive_side() < 0 {
            self.review_ftp_draft(&path);
        } else {
            self.status(format!(
                "FTP-Arbeitskopie geöffnet · {}",
                display_path(&path)
            ));
        }
    }

    pub(crate) fn choose_ftp_draft(&mut self) {
        if let [draft] = &self.ftp_drafts[..] {
            let path = draft.path.clone();
            self.review_ftp_draft(&path);
        } else if self.ftp_drafts.is_empty() {
            self.text(
                "Keine offenen FTP-Bearbeitungen",
                &format!(
                    "Lokale Arbeitskopien liegen unter {}",
                    display_path(&recovery_directory())
                ),
            );
        } else {
            let paths = self
                .ftp_drafts
                .iter()
                .map(|draft| draft.path.clone())
                .collect();
            let labels: Vec<_> = self
                .ftp_drafts
                .iter()
                .map(|draft| format!("{} · {}", draft.entry.name, draft.location.directory))
                .collect();
            let options: Vec<_> = labels.iter().map(String::as_str).collect();
            self.dialog(
                "confirm",
                Pending::FtpEditChoose(paths),
                Dialog {
                    title: "Offene FTP-Bearbeitungen",
                    description: "Wähle die gewünschte Arbeitskopie.",
                    options: &options,
                    ..Dialog::default()
                },
            );
        }
    }

    pub(crate) fn review_ftp_draft(&mut self, path: &Path) {
        let Some(draft) = self.ftp_drafts.iter().find(|draft| draft.path == path) else {
            return;
        };
        let title = draft.entry.name.clone();
        let description = format!(
            "Speichere im Editor und wähle anschließend „Änderungen hochladen“.\n{}\nArbeitskopie: {}",
            draft.location.directory,
            display_path(path),
        );
        self.dialog(
            "confirm",
            Pending::FtpEditAction(path.to_owned()),
            Dialog {
                title: &title,
                description: &description,
                options: &[
                    "Änderungen hochladen",
                    "Im Editor öffnen",
                    "Bearbeitung beenden und Kopie behalten",
                    "Lokale Arbeitskopie löschen …",
                ],
                ..Dialog::default()
            },
        );
    }

    pub(crate) fn ftp_draft_action(&mut self, path: &Path, option: usize) -> io::Result<()> {
        let Some(draft) = self.ftp_drafts.iter().find(|draft| draft.path == path) else {
            return Ok(());
        };
        if self.job.is_some() {
            return Err(io::Error::other("Ein Auftrag läuft bereits."));
        }
        match option {
            0 => {
                let mut draft = draft.clone();
                self.start_job(
                    "Prüfe und übertrage FTP-Änderungen …",
                    false,
                    move |job| match draft.upload(&job.cancel) {
                        Ok(true) => Ok(Outcome::FtpEditUploaded(Box::new(draft))),
                        Ok(false) => Ok(Outcome::Notice(
                            "Die Arbeitskopie enthält keine neuen Änderungen.".into(),
                        )),
                        Err(error) => {
                            let label = if error.kind() == io::ErrorKind::Interrupted
                                || job.cancel.load(Ordering::Relaxed)
                            {
                                "FTP-Übertragung abgebrochen"
                            } else {
                                "FTP-Übertragung konnte nicht abgeschlossen werden"
                            };
                            Ok(Outcome::Text(
                                label.into(),
                                format!("{error}\n\nArbeitskopie: {}", display_path(&draft.path)),
                            ))
                        }
                    },
                );
            }
            1 => {
                open_edit(path)?;
                arco_windows::edit(path, self.profile.editor.as_deref())?;
                self.review_ftp_draft(path);
            }
            2 => {
                self.ftp_drafts.retain(|draft| draft.path != path);
                self.status(format!("Arbeitskopie behalten · {}", display_path(path)));
            }
            3 => self.dialog(
                "confirm",
                Pending::FtpEditDelete(path.to_owned()),
                Dialog {
                    title: "FTP-Arbeitskopie löschen",
                    description: &format!(
                        "Lokale Kopie einschließlich aller ungesendeten Änderungen löschen?\n{}",
                        display_path(path)
                    ),
                    ..Dialog::default()
                },
            ),
            _ => (),
        }
        Ok(())
    }

    pub(crate) fn delete_ftp_draft(&mut self, path: &Path) -> io::Result<()> {
        if self.ftp_drafts.iter().any(|draft| draft.path == path) {
            match check_workspace(path) {
                Ok(()) => (),
                Err(error) if error.kind() == io::ErrorKind::NotFound => (),
                Err(error) => return Err(error),
            }
            match fs::remove_file(path) {
                Ok(()) => (),
                Err(error) if error.kind() == io::ErrorKind::NotFound => (),
                Err(error) => return Err(error),
            }
            if let Some(directory) = path.parent() {
                let _ = fs::remove_dir(directory);
            }
            self.ftp_drafts.retain(|draft| draft.path != path);
            self.status("FTP-Arbeitskopie gelöscht.");
        }
        Ok(())
    }

    pub(super) fn finish_ftp_upload(&mut self, uploaded: Draft) {
        for side in 0..self.panels.len() {
            if !self.panels[side].loading
                && self.panels[side]
                    .current()
                    .ftp
                    .as_ref()
                    .is_some_and(|tab| tab.location == uploaded.location)
            {
                self.refresh(side);
            }
        }
        self.status(format!(
            "FTP-Änderungen hochgeladen · {}",
            uploaded.entry.name
        ));
        if let Some(draft) = self
            .ftp_drafts
            .iter_mut()
            .find(|draft| draft.path == uploaded.path)
        {
            *draft = uploaded;
        }
    }

    pub(super) fn close_ftp_drafts(&mut self, session: &Arc<Session>) -> usize {
        let before = self.ftp_drafts.len();
        self.ftp_drafts
            .retain(|draft| !Arc::ptr_eq(&draft.location.session, session));
        before - self.ftp_drafts.len()
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use arco_windows::ftp::{Connection, Security};
    use std::fs::FileTimes;

    #[test]
    #[ignore = "requires a disposable writable local FTP server and ARCO_TEST_FTP_ROOT"]
    fn ftp_edits_can_be_published_twice_and_detect_content_conflicts() {
        let address = std::env::var("ARCO_TEST_FTP").unwrap();
        let server_root = PathBuf::from(std::env::var_os("ARCO_TEST_FTP_ROOT").unwrap());
        let session = Arc::new(
            Connection::new(&address, "arco", Security::Plain)
                .unwrap()
                .authenticate("test-only")
                .unwrap(),
        );
        let cancel = AtomicBool::new(false);
        let directory = session.initial_directory();
        let name = "Entwurf mit Leerzeichen.txt";
        let server_file = server_root.join(name);
        fs::write(&server_file, b"first version").unwrap();
        let entry = session
            .list_sync(&directory, &cancel)
            .unwrap()
            .entries
            .into_iter()
            .find(|entry| entry.name == name)
            .unwrap();
        let recovery = tempfile::tempdir().unwrap();
        let mut draft = Draft::download(
            FtpLocation {
                session: session.clone(),
                directory,
                profile: None,
            },
            entry,
            recovery.path(),
            &cancel,
        )
        .unwrap();
        assert!(!draft.upload(&cancel).unwrap());
        fs::write(&draft.path, b"revised one").unwrap();
        assert!(draft.upload(&cancel).unwrap());
        assert_eq!(fs::read(&server_file).unwrap(), b"revised one");
        fs::write(&draft.path, b"revised two").unwrap();
        assert!(draft.upload(&cancel).unwrap());
        assert_eq!(fs::read(&server_file).unwrap(), b"revised two");

        let before = fs::metadata(&server_file).unwrap().modified().unwrap();
        fs::write(&server_file, b"server edit").unwrap();
        File::options()
            .write(true)
            .open(&server_file)
            .unwrap()
            .set_times(FileTimes::new().set_modified(before))
            .unwrap();
        session
            .verify_sync_entry(&draft.location.directory, &draft.entry, &cancel)
            .unwrap();
        fs::write(&draft.path, b"client edit").unwrap();
        let error = draft.upload(&cancel).unwrap_err();
        assert!(error.to_string().contains("Serverdatei wurde"));
        assert_eq!(fs::read(&draft.path).unwrap(), b"client edit");
        assert_eq!(fs::read(&server_file).unwrap(), b"server edit");
    }
}
