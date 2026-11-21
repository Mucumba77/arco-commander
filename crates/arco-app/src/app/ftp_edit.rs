use super::{App, Outcome};
use crate::commands::{Dialog, Pending};
use crate::display_path;
use crate::panel::FtpLocation;
use arco_core::{files, tools};
use arco_windows::ftp::{Entry, Session, SyncUpload};
use arco_windows::{EditFileRevision, edit_file_revision};
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use tempfile::NamedTempFile;

mod monitor;
mod publication;
mod text;
pub(super) use monitor::Monitor;
pub(crate) use text::TextEdit;

const MAX_EDIT_SIZE: u64 = 64 * 1024 * 1024;

#[derive(Clone)]
pub(crate) struct Draft {
    location: FtpLocation,
    entry: Entry,
    path: PathBuf,
    baseline: String,
    local_revision: EditFileRevision,
    automatic: monitor::Automatic,
    publication: publication::Target,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DraftUpload {
    Published,
    Unchanged,
    ChangedAgain,
}

#[derive(Clone, Copy)]
pub(crate) enum DraftEditor {
    External,
    Text,
}

impl Draft {
    #[cfg(test)]
    fn download(
        location: FtpLocation,
        entry: Entry,
        recovery: &Path,
        cancel: &AtomicBool,
    ) -> io::Result<Self> {
        Self::download_with_progress(location, entry, recovery, cancel, |_| {})
    }

    fn download_with_progress(
        location: FtpLocation,
        entry: Entry,
        recovery: &Path,
        cancel: &AtomicBool,
        progress: impl FnMut(arco_windows::ftp::DownloadProgress),
    ) -> io::Result<Self> {
        if location.session.is_http_forward_ftp() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Diese Verbindung erlaubt nur Lesezugriffe.",
            ));
        }
        if !matches!(entry.size, Some(size) if size <= MAX_EDIT_SIZE)
            || entry.directory != Some(false)
        {
            return Err(io::Error::other(
                "Wähle eine Serverdatei mit bekannter Größe von höchstens 64 MiB.",
            ));
        }
        let name = files::leaf_name(OsStr::new(&entry.name))
            .map(|name| name.to_string_lossy().into_owned())
            .or_else(|_| {
                arco_core::transfer_names::NameRules {
                    windows: true,
                    ..Default::default()
                }
                .download(&entry.name)
            })?;
        fs::create_dir_all(recovery)?;
        let directory = tempfile::Builder::new()
            .prefix("bearbeitung-")
            .tempdir_in(recovery)?;
        let path = directory.path().join(name);
        let file = File::create_new(&path)?;
        location.session.fetch_sync_with_progress(
            &location.directory,
            &entry,
            &file,
            cancel,
            progress,
        )?;
        file.sync_all()?;
        let baseline = checksum(&path, cancel)?;
        drop(file);
        let local_revision = edit_file_revision(&open_edit(&path)?)?;
        let publication = publication::Target::for_entry(&location, &entry.name)?;
        let _ = directory.keep();
        Ok(Self {
            location,
            entry,
            path,
            baseline,
            local_revision,
            automatic: Default::default(),
            publication,
        })
    }

    fn upload(
        &mut self,
        expected: Option<EditFileRevision>,
        cancel: &AtomicBool,
    ) -> io::Result<DraftUpload> {
        let mut source = match open_edit(&self.path) {
            Err(error)
                if expected.is_some()
                    && (error.kind() == io::ErrorKind::NotFound
                        || matches!(error.raw_os_error(), Some(32 | 33))) =>
            {
                return Ok(DraftUpload::ChangedAgain);
            }
            result => result?,
        };
        let revision = edit_file_revision(&source)?;
        if expected.is_some_and(|expected| expected != revision) {
            return Ok(DraftUpload::ChangedAgain);
        }
        let mut staged = NamedTempFile::new()?;
        let size = io::copy(
            &mut (&mut source).take(MAX_EDIT_SIZE + 1),
            staged.as_file_mut(),
        )?;
        if size > MAX_EDIT_SIZE {
            return Err(io::Error::other(
                "Server-Arbeitskopien dürfen höchstens 64 MiB groß sein.",
            ));
        }
        staged.as_file_mut().flush()?;
        let edited = checksum(staged.path(), cancel)?;
        if edited == self.baseline {
            self.local_revision = revision;
            return Ok(DraftUpload::Unchanged);
        }
        let _publication = self.publication.acquire(cancel)?;
        let result = self.location.session.upload_sync(
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
                    return Err(io::Error::new(
                        io::ErrorKind::WouldBlock,
                        "Die Arbeitskopie wurde während der Übertragung erneut gespeichert.",
                    ));
                }
                Ok(())
            },
            cancel,
        );
        self.entry = match result {
            Err(error) if expected.is_some() && error.kind() == io::ErrorKind::WouldBlock => {
                return Ok(DraftUpload::ChangedAgain);
            }
            result => result?,
        };
        self.baseline = edited;
        self.local_revision = revision;
        Ok(DraftUpload::Published)
    }
}

fn open_edit(path: &Path) -> io::Result<File> {
    check_workspace(path)?;
    let file = arco_windows::open_edit_copy(path)?;
    let metadata = file.metadata()?;
    if files::is_link(&metadata) || !metadata.is_file() {
        return Err(io::Error::other(
            "Die Server-Arbeitskopie muss eine reguläre lokale Datei sein.",
        ));
    }
    if metadata.len() > MAX_EDIT_SIZE {
        return Err(io::Error::other(
            "Server-Arbeitskopien dürfen höchstens 64 MiB groß sein.",
        ));
    }
    Ok(file)
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
    pub(super) fn refresh_ftp_edit_status(&self) {
        if let Some(Pending::FtpEditAction(path)) = &self.pending {
            let ui = self.ui();
            ui.set_ftp_edit_pending(self.ftp_upload_pending(path));
            if let Some(draft) = self.ftp_drafts.iter().find(|draft| &draft.path == path) {
                ui.set_ftp_edit_automatic(draft.automatic.enabled);
                ui.set_ftp_edit_auto_paused(draft.automatic.paused);
            }
        }
    }

    pub(crate) fn edit_ftp_file(&mut self, editor: DraftEditor) {
        if self.panels[self.active].loading || self.job.is_some() {
            self.status("Warte, bis das Serververzeichnis und der laufende Auftrag fertig sind.");
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
            match editor {
                DraftEditor::External => self.review_ftp_draft(&path),
                DraftEditor::Text => {
                    if let Err(error) = self.open_ftp_text_editor(&path) {
                        self.error(
                            "Texteditor konnte nicht geöffnet werden",
                            &error.to_string(),
                        );
                    }
                }
            }
            return;
        }
        if entry.directory != Some(false) {
            self.status("Wähle eine reguläre Serverdatei.");
            return;
        }
        let location = ftp.location.clone();
        let source = format!("{}{}", location.directory, entry.name);
        let description =
            super::JobDescription::new(crate::OperationKind::Ftp, source.clone(), String::new());
        self.start_operation_job(
            "Server-Arbeitskopie laden",
            false,
            description,
            move |job| {
                let size = entry.size;
                let draft = Draft::download_with_progress(
                    location,
                    entry,
                    &recovery_directory(),
                    &job.cancel,
                    job.ftp_download_progress(source, size),
                )?;
                Ok(Outcome::FtpEditReady(Box::new(draft), editor))
            },
        );
    }

    pub(super) fn open_downloaded_ftp_draft(&mut self, draft: Draft, editor: DraftEditor) {
        let path = draft.path.clone();
        self.ftp_drafts.push(draft);
        if matches!(editor, DraftEditor::Text) {
            if let Err(error) = self.open_ftp_text_editor(&path) {
                self.review_ftp_draft(&path);
                self.error(
                    "Texteditor konnte nicht geöffnet werden",
                    &error.to_string(),
                );
            }
            return;
        }
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
                "Server-Arbeitskopie geöffnet · {}",
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
                "Keine offenen Serverbearbeitungen",
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
                    title: "Offene Serverbearbeitungen",
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
        let remote = format!("{}{}", draft.location.directory, draft.entry.name);
        let ui = self.ui();
        ui.set_ftp_edit_pending(self.ftp_upload_pending(path));
        ui.set_ftp_edit_automatic(draft.automatic.enabled);
        ui.set_ftp_edit_auto_paused(draft.automatic.paused);
        self.dialog(
            "ftp-edit",
            Pending::FtpEditAction(path.to_owned()),
            Dialog {
                title: &title,
                value: &display_path(path),
                second: &remote,
                ..Dialog::default()
            },
        );
    }

    pub(crate) fn ftp_draft_action(&mut self, path: &Path, option: usize) -> io::Result<()> {
        let Some(draft) = self.ftp_drafts.iter().find(|draft| draft.path == path) else {
            return Ok(());
        };
        if !matches!(option, 1 | 5) && self.ftp_upload_pending(path) {
            return Err(io::Error::other(
                "Die Rückübertragung ist bereits im Transfermanager vorgemerkt.",
            ));
        }
        match option {
            0 => {
                self.queue_ftp_draft(draft.clone(), None)?;
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
                    title: "Server-Arbeitskopie löschen",
                    description: &format!(
                        "Lokale Kopie einschließlich aller ungesendeten Änderungen löschen?\n{}",
                        display_path(path)
                    ),
                    ..Dialog::default()
                },
            ),
            4 => {
                if let Some(draft) = self.ftp_drafts.iter_mut().find(|draft| draft.path == path) {
                    draft.automatic.toggle();
                }
                self.review_ftp_draft(path);
            }
            5 => self.open_ftp_text_editor(path)?,
            _ => (),
        }
        Ok(())
    }

    fn queue_ftp_draft(
        &mut self,
        mut draft: Draft,
        expected: Option<EditFileRevision>,
    ) -> io::Result<()> {
        let label = format!("Serveränderungen hochladen · {}", draft.entry.name);
        let destination = format!("{}{}", draft.location.directory, draft.entry.name);
        let path = draft.path.clone();
        self.enqueue_ftp_upload(
            label.clone(),
            path,
            destination,
            expected.is_some(),
            move |job| {
                job.phase("Serveränderungen prüfen und zurückübertragen …");
                match draft.upload(expected, &job.cancel) {
                    Ok(result) => Ok(Outcome::FtpEditFinished(Box::new(draft), result, None)),
                    Err(error) => {
                        let report = if error.kind() == io::ErrorKind::Interrupted {
                            files::TransferReport {
                                cancelled: true,
                                ..Default::default()
                            }
                        } else {
                            files::TransferReport {
                                errors: vec![format!(
                                    "{error}\nArbeitskopie: {}",
                                    display_path(&draft.path)
                                )],
                                ..Default::default()
                            }
                        };
                        Ok(Outcome::FtpEditStopped(draft.path, label, report))
                    }
                }
            },
        )
    }

    pub(crate) fn delete_ftp_draft(&mut self, path: &Path) -> io::Result<()> {
        if self.ftp_upload_pending(path) {
            return Err(io::Error::other(
                "Die Rückübertragung ist im Transfermanager vorgemerkt.",
            ));
        }
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
            self.status("Server-Arbeitskopie gelöscht.");
        }
        Ok(())
    }

    pub(super) fn finish_ftp_upload(
        &mut self,
        uploaded: Draft,
        result: DraftUpload,
        log_error: Option<String>,
    ) {
        if let Some(draft) = self
            .ftp_drafts
            .iter_mut()
            .find(|draft| draft.path == uploaded.path)
        {
            draft.local_revision = uploaded.local_revision;
            draft.entry = uploaded.entry.clone();
            draft.baseline = uploaded.baseline;
            if result == DraftUpload::ChangedAgain {
                draft.automatic.retry_later();
            }
        }
        if result != DraftUpload::Published {
            if result == DraftUpload::Unchanged {
                self.status("Server-Arbeitskopie unverändert");
            }
            return;
        }
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
        let status = format!("Serveränderungen hochgeladen · {}", uploaded.entry.name);
        if let Some(error) = log_error {
            self.status(format!("{status} · Transferprotokoll: {error}"));
        } else {
            self.status(status);
        }
    }

    pub(super) fn stop_ftp_upload(&mut self, path: &Path) {
        if let Some(draft) = self.ftp_drafts.iter_mut().find(|draft| draft.path == path) {
            draft.automatic.pause();
        }
    }

    pub(super) fn close_ftp_drafts(&mut self, session: &Arc<Session>) -> usize {
        let paths: Vec<_> = self
            .ftp_drafts
            .iter()
            .filter(|draft| Arc::ptr_eq(&draft.location.session, session))
            .map(|draft| draft.path.clone())
            .collect();
        self.remove_waiting_ftp_uploads(&paths);
        let before = self.ftp_drafts.len();
        self.ftp_drafts
            .retain(|draft| !Arc::ptr_eq(&draft.location.session, session));
        before - self.ftp_drafts.len()
    }

    pub(super) fn ftp_session_upload_running(&self, session: &Arc<Session>) -> bool {
        self.ftp_drafts.iter().any(|draft| {
            Arc::ptr_eq(&draft.location.session, session) && self.ftp_upload_running(&draft.path)
        })
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use arco_windows::ftp::{Connection, ConnectionOptions, Security};
    use std::fs::FileTimes;

    #[test]
    #[ignore = "requires scripts/test-native-http-reference.py"]
    fn http_drafts_publish_and_keep_conflicting_edits() {
        let dav = Connection::new(
            &std::env::var("ARCO_TEST_WEBDAV").unwrap(),
            "arco",
            Security::WebDav,
        )
        .unwrap()
        .authenticate("fixture-only")
        .unwrap();
        let s3 = Connection::new(
            &std::env::var("ARCO_TEST_S3").unwrap(),
            "ARCOACCESS",
            Security::S3,
        )
        .unwrap()
        .with_s3_region("eu-central-1")
        .unwrap()
        .authenticate("fixture-only")
        .unwrap();
        verify_remote_edits(dav);
        verify_remote_edits(s3);
    }

    #[test]
    #[ignore = "requires scripts/test-sftp-reference.py"]
    fn ssh_drafts_publish_and_keep_conflicting_edits() {
        for (prefix, security) in [
            ("ARCO_SFTP_TEST", Security::Sftp),
            ("ARCO_SCP_TEST", Security::Scp),
        ] {
            let mut address =
                arco_windows::ftp::Url::parse(&std::env::var(format!("{prefix}_URL")).unwrap())
                    .unwrap();
            if security == Security::Scp {
                address.set_scheme("scp").unwrap();
            }
            let session = Connection::new(address.as_str(), "arco", security)
                .unwrap()
                .with_ssh(
                    &std::env::var(format!("{prefix}_FINGERPRINT")).unwrap(),
                    None,
                )
                .unwrap()
                .authenticate("fixture-only")
                .unwrap();
            verify_remote_edits(session);
        }
    }

    #[test]
    #[ignore = "requires the protected FTPS editor fixtures in scripts/test-ftp-area5-reference.py"]
    fn ftps_drafts_detect_competing_publications() {
        let cert = PathBuf::from(std::env::var_os("ARCO_TEST_FTPS_CA").unwrap());
        for (variable, security) in [
            ("ARCO_TEST_FTPS_EDIT_EXPLICIT", Security::ExplicitTls),
            ("ARCO_TEST_FTPS_EDIT_IMPLICIT", Security::ImplicitTls),
        ] {
            let address = std::env::var(variable).unwrap();
            let connect = || {
                Arc::new(
                    Connection::new(&address, "arco", security)
                        .unwrap()
                        .with_options(ConnectionOptions {
                            ca_bundle: Some(cert.clone()),
                            ..Default::default()
                        })
                        .unwrap()
                        .authenticate("test-only")
                        .unwrap(),
                )
            };
            let cancel = &AtomicBool::new(false);
            let recovery = tempfile::tempdir().unwrap();
            let name = "note with spaces.txt";
            let get_entry = |session: &Session, folder: &arco_windows::ftp::Url| {
                session
                    .list_sync(folder, cancel)
                    .unwrap()
                    .regular_file(name)
                    .unwrap()
                    .clone()
            };
            let open = |session: Arc<Session>| {
                let directory = session.initial_directory();
                let entry = get_entry(&session, &directory);
                Draft::download(
                    FtpLocation {
                        session,
                        directory,
                        profile: None,
                    },
                    entry,
                    recovery.path(),
                    cancel,
                )
                .unwrap()
            };
            let mut left = open(connect());
            let mut right = open(connect());
            assert!(!Arc::ptr_eq(
                &left.location.session,
                &right.location.session
            ));
            assert_ne!(left.path, right.path);
            assert_eq!(fs::read(&left.path).unwrap(), b"initial text");
            assert_eq!(fs::read(&right.path).unwrap(), b"initial text");
            assert_eq!(left.upload(None, cancel).unwrap(), DraftUpload::Unchanged);
            fs::write(&left.path, b"alpha update").unwrap();
            fs::write(&right.path, b"bravo update").unwrap();
            let together = std::sync::Barrier::new(3);
            let (left_result, right_result) = std::thread::scope(|scope| {
                let left = scope.spawn(|| {
                    together.wait();
                    left.upload(None, cancel)
                });
                let right = scope.spawn(|| {
                    together.wait();
                    right.upload(None, cancel)
                });
                together.wait();
                (left.join().unwrap(), right.join().unwrap())
            });
            let (winner, loser, published, rejected, error) = match (left_result, right_result) {
                (Ok(DraftUpload::Published), Err(error)) => {
                    (&mut left, &right, b"alpha update", b"bravo update", error)
                }
                (Err(error), Ok(DraftUpload::Published)) => {
                    (&mut right, &left, b"bravo update", b"alpha update", error)
                }
                (left, right) => {
                    panic!("{security:?}: exactly one reply should publish: {left:?}, {right:?}")
                }
            };
            assert!(error.to_string().contains("Serverdatei wurde"), "{error}");
            assert_eq!(winner.entry.size, loser.entry.size);
            assert_eq!(winner.entry.modified, loser.entry.modified);
            assert_eq!(fs::read(&loser.path).unwrap(), rejected);
            assert_eq!(fs::read(&winner.path).unwrap(), published);
            let observer = connect();
            let directory = observer.initial_directory();
            let observe = || {
                let copy = NamedTempFile::new().unwrap();
                let entry = get_entry(&observer, &directory);
                observer
                    .fetch_sync(&directory, &entry, copy.as_file(), cancel)
                    .unwrap();
                fs::read(copy.path()).unwrap()
            };
            assert_eq!(observe(), published);
            fs::write(&winner.path, b"later update").unwrap();
            let saved = edit_file_revision(&open_edit(&winner.path).unwrap()).unwrap();
            assert_eq!(
                winner.upload(Some(saved), cancel).unwrap(),
                DraftUpload::Published
            );
            assert_eq!(observe(), b"later update");
            assert_eq!(
                observer
                    .list_sync(&directory, cancel)
                    .unwrap()
                    .entries
                    .len(),
                1
            );
            println!(
                "{security:?}: protected uploads, competing sessions and recovery copy verified"
            );
        }
    }

    fn verify_remote_edits(session: Session) {
        let cancel = AtomicBool::new(false);
        let root = session.initial_directory();
        let folder = "arco-edit-fixture";
        session
            .create_sync_directory(&root, folder, &cancel)
            .unwrap();
        let directory = session
            .relative_directory(&root, Path::new(folder))
            .unwrap();
        let recovery = tempfile::tempdir().unwrap();
        let name = "Büro Entwurf.txt";
        let source = recovery.path().join("source.txt");
        let set_remote = |expected: Option<&Entry>, bytes: &[u8]| {
            fs::write(&source, bytes).unwrap();
            session
                .upload_sync(
                    &directory,
                    SyncUpload {
                        source: &source,
                        name,
                        expected,
                        size: bytes.len() as u64,
                        modified_utc: None,
                    },
                    || Ok(()),
                    &cancel,
                )
                .unwrap()
        };
        let entry = set_remote(None, b"original");
        let mut draft = Draft::download(
            FtpLocation {
                session: Arc::new(session.clone()),
                directory: directory.clone(),
                profile: None,
            },
            entry,
            recovery.path(),
            &cancel,
        )
        .unwrap();
        fs::write(&draft.path, b"client change").unwrap();
        let ready = edit_file_revision(&open_edit(&draft.path).unwrap()).unwrap();
        fs::write(&draft.path, b"a newer client change").unwrap();
        assert_eq!(
            draft.upload(Some(ready), &cancel).unwrap(),
            DraftUpload::ChangedAgain
        );
        let ready = edit_file_revision(&open_edit(&draft.path).unwrap()).unwrap();
        assert_eq!(
            draft.upload(Some(ready), &cancel).unwrap(),
            DraftUpload::Published
        );
        let published = NamedTempFile::new().unwrap();
        session
            .fetch_sync(&directory, &draft.entry, published.as_file(), &cancel)
            .unwrap();
        assert_eq!(
            fs::read(published.path()).unwrap(),
            b"a newer client change"
        );
        let for_server = b"external server edit after opening the file";
        let rival = set_remote(Some(&draft.entry), for_server);
        fs::write(&draft.path, b"my conflicting draft").unwrap();
        assert!(draft.upload(None, &cancel).is_err());
        let current = NamedTempFile::new().unwrap();
        session
            .fetch_sync(&directory, &rival, current.as_file(), &cancel)
            .unwrap();
        assert_eq!(fs::read(current.path()).unwrap(), for_server);
        assert_eq!(fs::read(&draft.path).unwrap(), b"my conflicting draft");
    }

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
        let restored = session
            .restore_directory(Some(&format!("{address}/legacy%20folder")))
            .unwrap();
        assert_eq!(restored.path(), "/legacy%20folder/");
        assert!(
            session
                .restore_directory(Some("ftp://another-host.invalid/legacy%20folder/"))
                .is_err()
        );
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
        assert_eq!(draft.upload(None, &cancel).unwrap(), DraftUpload::Unchanged);
        fs::write(&draft.path, b"revised one").unwrap();
        assert_eq!(draft.upload(None, &cancel).unwrap(), DraftUpload::Published);
        assert_eq!(fs::read(&server_file).unwrap(), b"revised one");
        fs::write(&draft.path, b"revised two").unwrap();
        assert_eq!(draft.upload(None, &cancel).unwrap(), DraftUpload::Published);
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
        let error = draft.upload(None, &cancel).unwrap_err();
        assert!(error.to_string().contains("Serverdatei wurde"));
        assert_eq!(fs::read(&draft.path).unwrap(), b"client edit");
        assert_eq!(fs::read(&server_file).unwrap(), b"server edit");
    }
}
