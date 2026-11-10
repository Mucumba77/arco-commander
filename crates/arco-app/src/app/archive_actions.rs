use super::{
    App, Navigation, Outcome, strings,
    transfer_queue::{
        ConflictSide, VIRTUAL_CONFLICT_PREVIEW_LIMIT, ask_conflict, conflict_preview,
        request_conflict,
    },
};
use crate::panel::{ArchiveTab, Location};
use crate::{display_path, preview};
use arco_core::archive_view::{self, Selection};
use arco_core::{archives, files, tools};
use std::cell::Cell;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

pub(crate) struct StagedPreview {
    pub storage: Arc<tempfile::TempDir>,
    pub path: PathBuf,
    pub title: String,
    pub separate: bool,
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

pub(super) fn stage_archive_preview(
    file: &Path,
    member: &Path,
    cancel: &AtomicBool,
) -> io::Result<(tempfile::TempDir, PathBuf)> {
    let storage = tempfile::tempdir()?;
    let path = extract_preview(
        file,
        member,
        storage.path(),
        &Selection::preview(member)?,
        None,
        cancel,
    )?;
    Ok((storage, path))
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

fn archive_conflict_preview(
    archive: &Path,
    member: &files::Entry,
    password: Option<&str>,
    cancel: &AtomicBool,
) -> io::Result<String> {
    let Some(size) = member
        .size
        .filter(|size| *size <= VIRTUAL_CONFLICT_PREVIEW_LIMIT)
    else {
        return Ok("Archivvorschau für Dateien bekannter Größe bis 64 KiB verfügbar.".into());
    };
    let load = || {
        let storage = tempfile::tempdir()?;
        let path = extract_preview(
            archive,
            &member.path,
            storage.path(),
            &Selection::preview_bounded(&member.path, VIRTUAL_CONFLICT_PREVIEW_LIMIT)?,
            password,
            cancel,
        )?;
        let metadata = files::regular_metadata(&path)?;
        if metadata.len() != size {
            return Err(io::Error::other("Der Archiveintrag hat sich geändert."));
        }
        Ok(conflict_preview(&path, &metadata))
    };
    match load() {
        Ok(preview) => Ok(preview),
        Err(error) if error.kind() == io::ErrorKind::Interrupted => Err(error),
        Err(_) => Ok("Die Archivdatei konnte für die Vorschau nicht gelesen werden.".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archive_conflict_previews_use_the_selected_member_and_enforce_the_limit() {
        let fixture = tempfile::tempdir().unwrap();
        let base = fixture.path();
        let file = base.join("unterordner").join("text.txt");
        std::fs::create_dir(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "original").unwrap();
        let archive = base.join("fixture.zip");
        let cancel = AtomicBool::new(false);
        archives::create_zip(&[file], base, &archive, None, &cancel).unwrap();
        let mut entry = files::Entry::from_remote(
            Path::new("unterordner").join("text.txt"),
            files::Kind::File,
            Some(8),
            None,
        );
        assert_eq!(
            archive_conflict_preview(&archive, &entry, None, &cancel).unwrap(),
            "original"
        );
        entry.size = Some(VIRTUAL_CONFLICT_PREVIEW_LIMIT + 1);
        assert!(
            archive_conflict_preview(&archive, &entry, None, &cancel)
                .unwrap()
                .contains("64 KiB")
        );
    }
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
                password.as_deref().or(archive.password()),
                &job.cancel,
            )?;
            Ok(Outcome::Entries {
                side,
                request,
                entries,
                label: "Suchtreffer im Archiv".into(),
                scope: arco_core::view_settings::Scope::Search,
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
        self.enqueue_virtual_transfer("Archivauswahl entpacken", true, move |job| {
            check_archive(&archive)?;
            let target = std::fs::canonicalize(target)?;
            let staging = tempfile::tempdir_in(&target)?;
            archive_view::extract_with_progress(
                &archive.file,
                staging.path(),
                &selection,
                password.as_deref().or(archive.password()),
                &job.cancel,
                job.archive_progress(),
            )?;
            job.progress(format!("→ {}", display_path(&target)));
            check_archive(&archive)?;
            let mut paths = std::fs::read_dir(staging.path())?
                .map(|entry| entry.map(|entry| entry.path()))
                .collect::<io::Result<Vec<_>>>()?;
            paths.sort();
            let remaining = Cell::new(None);
            let resolve = |conflict: files::FileConflict<'_>| {
                if let Some(rule) = remaining.get()
                    && (conflict.target_metadata.is_file()
                        && !files::is_link(conflict.target_metadata)
                        || matches!(rule, files::Conflict::Skip | files::Conflict::Rename))
                {
                    return Ok(rule.into());
                }
                let member = archive.folder.join(
                    conflict
                        .source
                        .strip_prefix(staging.path())
                        .map_err(io::Error::other)?,
                );
                let label = format!("{}::{}", display_path(&archive.file), display_path(&member));
                let (decision, all) =
                    request_conflict(&job, conflict, files::Transfer::Move, Some(label))?;
                if all && let files::ConflictDecision::Apply(rule) = decision {
                    remaining.set(Some(rule));
                }
                Ok(decision)
            };
            let report = files::transfer(
                &paths,
                &target,
                files::Transfer::Move,
                files::TransferPolicy {
                    resolve_conflict: Some(&resolve),
                    ntfs: files::NtfsCopyOptions {
                        streams: true,
                        ..Default::default()
                    },
                    ..files::Conflict::Skip.into()
                },
                &job.cancel,
                Some(arco_windows::rename_noreplace),
                |_| {},
            )?;
            Ok(Outcome::Report("Archivauswahl entpackt".into(), report))
        });
    }

    pub(crate) fn preview_archive_member(
        &mut self,
        archive: ArchiveTab,
        member: PathBuf,
        password: Option<String>,
        separate: bool,
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
                password.as_deref().or(archive.password()),
                &job.cancel,
            )?;
            let content = preview::read(&path)?;
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
        Ok(())
    }

    pub(crate) fn insert_into_archive(
        &mut self,
        input: ArchiveInput,
        target: ArchiveTab,
        target_password: Option<String>,
        source_password: Option<String>,
    ) {
        self.enqueue_virtual_transfer("In Archiv kopieren", true, move |job| {
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
                        source_password.as_deref().or(archive.password()),
                        &job.cancel,
                    )?;
                    let paths = std::fs::read_dir(&base)?
                        .map(|entry| entry.map(|entry| entry.path()))
                        .collect::<io::Result<Vec<_>>>()?;
                    (base, paths)
                }
            };
            check_archive(&target)?;
            let password = target_password.as_deref().or(target.password());
            let result = archive_view::append(
                &target.file,
                &target.folder,
                &paths,
                &base,
                password,
                &job.cancel,
            );
            let count = match result {
                Ok(count) => count,
                Err(error)
                    if error.kind() == io::ErrorKind::AlreadyExists
                        && target.catalog.can_sync_write() =>
                {
                    let mut remaining = None;
                    let report = target.catalog.append_resolving(
                        &target.folder,
                        &paths,
                        &base,
                        password,
                        &job.cancel,
                        |conflict| {
                            let can_replace = conflict.source_metadata.is_file()
                                && conflict.target.kind == files::Kind::File;
                            if let Some(rule) = remaining
                                && (can_replace
                                    || matches!(
                                        rule,
                                        files::Conflict::Skip | files::Conflict::Rename
                                    ))
                            {
                                return Ok(files::ConflictDecision::Apply(rule));
                            }
                            let mut actions = vec![
                                (files::Conflict::Skip, "Überspringen"),
                                (files::Conflict::Rename, "Unter freiem Namen speichern"),
                            ];
                            if can_replace {
                                actions.extend([
                                    (files::Conflict::Replace, "Überschreiben"),
                                    (
                                        files::Conflict::Newer,
                                        "Nur mit neuerer Datei überschreiben",
                                    ),
                                    (
                                        files::Conflict::Larger,
                                        "Nur mit größerer Datei überschreiben",
                                    ),
                                    (files::Conflict::Append, "An Zieldatei anhängen"),
                                ]);
                            }
                            let path = format!(
                                "{}::{}",
                                display_path(&target.file),
                                display_path(&conflict.target.path)
                            );
                            let mut destination = ConflictSide::virtual_entry(
                                path,
                                conflict.target.size,
                                conflict.target.modified,
                            );
                            if conflict.target.kind == files::Kind::File {
                                destination.preview = archive_conflict_preview(
                                    &target.file,
                                    conflict.target,
                                    password,
                                    &job.cancel,
                                )?;
                            }
                            let (decision, all) = ask_conflict(
                                &job,
                                ConflictSide::local(
                                    conflict.source,
                                    conflict.source_metadata,
                                    None,
                                ),
                                destination,
                                actions,
                            )?;
                            if all && let files::ConflictDecision::Apply(rule) = decision {
                                remaining = Some(rule);
                            }
                            Ok(decision)
                        },
                    )?;
                    return Ok(Outcome::Report("In Archiv kopiert".into(), report));
                }
                Err(error) => return Err(error),
            };
            Ok(Outcome::Report(
                "In Archiv kopiert".into(),
                files::TransferReport {
                    completed: count,
                    ..Default::default()
                },
            ))
        });
    }

    pub(crate) fn edit_archive(
        &mut self,
        archive: ArchiveTab,
        edit: archives::ZipEdit,
        password: &str,
    ) {
        let password = (!password.is_empty()).then(|| zeroize::Zeroizing::new(password.to_owned()));
        self.start_progress_job(
            "Bearbeite Archiv …",
            true,
            display_path(&archive.file),
            move |job| {
                check_archive(&archive)?;
                let secret = password
                    .as_ref()
                    .map(|value| value.as_str())
                    .or(archive.password());
                let count = archive_view::edit(&archive.file, edit, secret, &job.cancel)?;
                Ok(Outcome::Notice(format!(
                    "{count} Archiveinträge bearbeitet."
                )))
            },
        );
    }

    pub(super) fn show_staged_preview(
        &mut self,
        archive: StagedPreview,
        content: preview::Content,
    ) {
        if archive.separate || matches!(content, preview::Content::Rich(_)) {
            self.open_staged_lister(archive, content);
            return;
        }
        let path = archive.path.clone();
        let title = archive.title.clone();
        self.staged_preview = Some(archive);
        self.viewer = None;
        match content {
            preview::Content::Page(mode, page) => self.show_preview(path, mode, page),
            preview::Content::Image(image) => {
                let ui = self.ui();
                self.preview_image_path = Some(path);
                ui.set_dialog_title(title.into());
                ui.set_dialog_description(
                    format!("{} × {} px", image.width(), image.height()).into(),
                );
                ui.set_viewer_notice("".into());
                ui.set_dialog_options(strings(std::iter::empty::<&str>()));
                ui.set_viewer_image(preview::to_slint(&image));
                ui.set_dialog_kind("image".into());
            }
            preview::Content::Rich(_) => unreachable!("rich previews use a modeless lister"),
        }
    }
}
