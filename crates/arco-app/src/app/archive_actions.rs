use super::{App, Navigation, Outcome, strings};
use crate::panel::{ArchiveTab, Location};
use crate::{display_path, preview};
use arco_core::archive_view::{self, Selection};
use arco_core::{archives, tools};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

pub(crate) struct StagedPreview {
    pub storage: Arc<tempfile::TempDir>,
    pub path: PathBuf,
    pub title: String,
}

pub(crate) enum ArchiveInput {
    Local {
        base: PathBuf,
        paths: Vec<PathBuf>,
    },
    Archive {
        archive: ArchiveTab,
        selection: Selection,
    },
}

pub(super) fn check_archive(archive: &ArchiveTab) -> io::Result<()> {
    if archive.catalog.unchanged(&archive.file) {
        Ok(())
    } else {
        Err(io::Error::other(
            "Das Archiv hat sich geändert. Aktualisiere die Ansicht mit F2.",
        ))
    }
}

pub(super) fn read_archive_preview(file: &Path, member: &Path) -> io::Result<preview::Content> {
    let storage = tempfile::tempdir()?;
    let path = extract_preview(
        file,
        member,
        storage.path(),
        &Selection::preview(member)?,
        None,
        &AtomicBool::new(false),
    )?;
    preview::read(&path)
}

fn extract_preview(
    file: &Path,
    member: &Path,
    storage: &Path,
    selection: &Selection,
    password: Option<&str>,
    cancel: &AtomicBool,
) -> io::Result<PathBuf> {
    let name = member
        .file_name()
        .ok_or_else(|| io::Error::other("Dateiname fehlt."))?;
    archive_view::extract(file, storage, selection, password, cancel)?;
    Ok(storage.join(name))
}

impl App {
    pub(crate) fn search_archive(
        &mut self,
        side: usize,
        request: u64,
        archive: ArchiveTab,
        query: tools::Search,
        password: Option<String>,
    ) {
        self.start_job("Suche im Archiv …", false, move |job| {
            let entries = archive.catalog.search(
                &archive.folder,
                &query,
                password.as_deref(),
                &job.cancel,
            )?;
            Ok(Outcome::Entries {
                side,
                request,
                entries,
                label: "Suchtreffer im Archiv".into(),
                location: Location::Archive {
                    file: archive.file,
                    folder: archive.folder,
                },
            })
        });
    }
    pub(crate) fn open_archive(&mut self, file: PathBuf) {
        self.load_location(
            self.active,
            Location::Archive {
                file,
                folder: PathBuf::new(),
            },
            Navigation::Jump,
        );
    }

    pub(crate) fn extract_archive_selection(
        &mut self,
        archive: ArchiveTab,
        target: PathBuf,
        selection: Selection,
        password: Option<String>,
    ) {
        self.start_job("Entpacke Archivauswahl …", true, move |job| {
            check_archive(&archive)?;
            let count = archive_view::extract(
                &archive.file,
                &target,
                &selection,
                password.as_deref(),
                &job.cancel,
            )?;
            Ok(Outcome::Notice(format!(
                "{count} Dateien entpackt. Markierte Ordner wurden übernommen."
            )))
        });
    }

    pub(crate) fn preview_archive_member(
        &mut self,
        archive: ArchiveTab,
        member: PathBuf,
        password: Option<String>,
    ) -> io::Result<()> {
        let selection = Selection::preview(&member)?;
        let title = format!("{}::{}", display_path(&archive.file), display_path(&member));
        self.start_job("Lese Archivvorschau …", false, move |job| {
            check_archive(&archive)?;
            let storage = Arc::new(tempfile::tempdir()?);
            let path = extract_preview(
                &archive.file,
                &member,
                storage.path(),
                &selection,
                password.as_deref(),
                &job.cancel,
            )?;
            let content = preview::read(&path)?;
            Ok(Outcome::StagedPreview(
                StagedPreview {
                    storage,
                    path,
                    title,
                },
                content,
            ))
        });
        Ok(())
    }

    pub(crate) fn insert_into_zip(
        &mut self,
        input: ArchiveInput,
        target: ArchiveTab,
        target_password: Option<String>,
        source_password: Option<String>,
    ) {
        self.start_job("Ergänze ZIP-Archiv …", true, move |job| {
            let storage;
            let (base, paths) = match input {
                ArchiveInput::Local { base, paths } => (base, paths),
                ArchiveInput::Archive { archive, selection } => {
                    check_archive(&archive)?;
                    storage = tempfile::tempdir()?;
                    let base = storage.path().to_path_buf();
                    archive_view::extract(
                        &archive.file,
                        &base,
                        &selection,
                        source_password.as_deref(),
                        &job.cancel,
                    )?;
                    let paths = std::fs::read_dir(&base)?
                        .map(|entry| entry.map(|entry| entry.path()))
                        .collect::<io::Result<Vec<_>>>()?;
                    (base, paths)
                }
            };
            check_archive(&target)?;
            let count = archives::append_zip(
                &target.file,
                &target.folder,
                &paths,
                &base,
                target_password.as_deref(),
                &job.cancel,
            )?;
            Ok(Outcome::Notice(format!("{count} Dateien ins ZIP kopiert.")))
        });
    }

    pub(crate) fn edit_zip(&mut self, archive: ArchiveTab, edit: archives::ZipEdit) {
        self.start_job("Bearbeite ZIP-Archiv …", true, move |job| {
            check_archive(&archive)?;
            let count = archives::edit_zip(&archive.file, edit, &job.cancel)?;
            Ok(Outcome::Notice(format!("{count} ZIP-Einträge bearbeitet.")))
        });
    }

    pub(super) fn show_staged_preview(
        &mut self,
        archive: StagedPreview,
        content: preview::Content,
    ) {
        let path = archive.path.clone();
        let title = archive.title.clone();
        self.staged_preview = Some(archive);
        self.viewer = None;
        match content {
            preview::Content::Page(mode, page) => self.show_preview(path, mode, page),
            preview::Content::Image(image) => {
                let ui = self.ui();
                ui.set_dialog_title(title.into());
                ui.set_dialog_description("".into());
                ui.set_dialog_options(strings(std::iter::empty::<&str>()));
                ui.set_viewer_image(preview::to_slint(&image));
                ui.set_dialog_kind("image".into());
            }
        }
    }
}
