use super::{App, JobDescription, Outcome, job_progress::Sample};
use crate::commands::{Dialog, Pending};
use crate::{MetadataForm, MutationData, ReviewRow, display_path, locale::tr};
use arco_core::{
    files::{self, TransferReport},
    ignore::IgnoreList,
};
use arco_windows::MetadataEdit;
use chrono::{Local, NaiveDateTime, TimeZone};
use std::{
    collections::BTreeSet,
    fs, io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant, SystemTime},
};

impl App {
    pub(crate) fn open_ftp_permissions(
        &mut self,
        location: crate::panel::FtpLocation,
        entries: Vec<arco_windows::ftp::Entry>,
    ) {
        if self.job.is_some() || entries.is_empty() {
            return;
        }
        let ui = self.ui();
        ui.set_metadata_selection(
            format!(
                "{} · {} {}",
                location.directory,
                entries.len(),
                tr("Einträge")
            )
            .into(),
        );
        ui.set_mutation_data(MutationData::default());
        self.dialog(
            "permissions",
            Pending::FtpPanelEdit(location, super::FtpEdit::Permissions(entries)),
            Dialog::default(),
        );
    }

    pub(super) fn submit_ftp_permissions(&mut self, mode: &str) {
        if self.job.is_some() {
            return;
        }
        let Some(Pending::FtpPanelEdit(location, super::FtpEdit::Permissions(entries))) =
            &self.pending
        else {
            return;
        };
        let location = location.clone();
        let entries = entries.clone();
        self.ui().set_mutation_data(MutationData::default());
        if let Err(error) =
            self.execute_ftp_edit(location, super::FtpEdit::Permissions(entries), mode)
        {
            self.ui().set_mutation_data(MutationData {
                notice: tr(&error.to_string()).into(),
                ..Default::default()
            });
        }
    }
    pub(super) fn open_metadata_paths(&mut self, paths: Vec<PathBuf>) {
        if self.job.is_some() || paths.is_empty() {
            return;
        }
        let ui = self.ui();
        ui.set_metadata_selection(super::properties::selection(&paths).into());
        ui.set_metadata_initial(MetadataForm::default());
        ui.set_metadata_revision(ui.get_metadata_revision().wrapping_add(1));
        ui.set_mutation_data(MutationData::default());
        self.dialog("metadata", Pending::EditMetadata(paths), Dialog::default());
    }

    pub(super) fn submit_metadata_form(&mut self, form: MetadataForm) {
        if self.job.is_some() {
            return;
        }
        let Some(Pending::EditMetadata(paths)) = &self.pending else {
            return;
        };
        let paths = paths.clone();
        match form_options(&form) {
            Ok(edit) => {
                self.ui().set_mutation_data(MutationData::default());
                self.start_metadata_job(paths, edit, form.recursive);
            }
            Err((field, error)) => self.ui().set_mutation_data(MutationData {
                notice: tr(&error.to_string()).into(),
                error_field: field,
                ..Default::default()
            }),
        }
    }

    pub(super) fn start_metadata_job(
        &mut self,
        roots: Vec<PathBuf>,
        edit: MetadataEdit,
        recursive: bool,
    ) {
        let description = JobDescription::new(
            crate::OperationKind::Metadata,
            super::properties::selection(&roots),
            String::new(),
        );
        let ignore = self.ignore_list();
        self.start_operation_job(
            "Attribute und Zeitstempel ändern",
            true,
            description,
            move |job| {
                let mut last = Instant::now() - Duration::from_secs(1);
                let mut applying = false;
                let report = edit_selection(
                    roots,
                    edit,
                    recursive,
                    ignore.as_deref(),
                    &job.cancel,
                    |path, processed, total, errors| {
                        let phase_change = total.is_some() != applying;
                        if phase_change
                            || total == Some(processed)
                            || last.elapsed() >= Duration::from_millis(80)
                        {
                            applying = total.is_some();
                            last = Instant::now();
                            if let Some(total) = total {
                                job.entry_progress(
                                    display_path(path),
                                    processed,
                                    total,
                                    "Attribute und Zeitstempel setzen …",
                                );
                            } else {
                                job.measured_progress(
                                    String::new(),
                                    Sample {
                                        phase: "Auswahl und Unterordner erfassen …".into(),
                                        source: display_path(path),
                                        scan: Some((processed, None, errors)),
                                        ..Default::default()
                                    },
                                );
                            }
                        }
                    },
                );
                Ok(Outcome::MetadataEdited("Attribute und Zeitstempel", report))
            },
        );
    }

    pub(super) fn finish_metadata_edit(&mut self, title: &'static str, report: TransferReport) {
        let ui = self.ui();
        if !matches!(ui.get_dialog_kind().as_str(), "metadata" | "permissions") {
            self.completed(Outcome::Report(title.into(), report));
            return;
        }
        let result = format!(
            "{} · {}: {} · {}: {} · {}: {}",
            tr(if report.cancelled {
                "Abgebrochen"
            } else {
                "Abgeschlossen"
            }),
            tr("Erfolgreich"),
            report.completed,
            tr("Ignoriert"),
            report.ignored,
            tr("Fehler"),
            report.errors.len()
        );
        let errors = report
            .errors
            .into_iter()
            .enumerate()
            .map(|(index, error)| ReviewRow {
                primary: format!("{} {}", tr("Eintrag"), index + 1).into(),
                secondary: tr("Fehlgeschlagen").into(),
                detail: error.into(),
                kind: "error".into(),
            })
            .collect::<Vec<_>>();
        ui.set_mutation_data(MutationData {
            result: result.into(),
            errors: slint::ModelRc::new(slint::VecModel::from(errors)),
            ..Default::default()
        });
    }
}

pub(super) fn parse_mode(mode: &str) -> io::Result<u16> {
    let mode = mode.trim();
    if !matches!(mode.len(), 3 | 4) || !mode.bytes().all(|byte| (b'0'..=b'7').contains(&byte)) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Gib drei oder vier Oktalziffern ein, zum Beispiel 644.",
        ));
    }
    u16::from_str_radix(mode, 8).map_err(io::Error::other)
}

fn form_options(form: &MetadataForm) -> Result<MetadataEdit, (i32, io::Error)> {
    let mut edit = MetadataEdit::default();
    for (state, flag) in [
        (form.readonly, 1),
        (form.hidden, 2),
        (form.system, 4),
        (form.archive, 32),
    ] {
        match state {
            0 => {}
            1 => edit.set |= flag,
            2 => edit.clear |= flag,
            _ => return Err((0, io::Error::other("Ungültige Attributauswahl."))),
        }
    }
    for (enabled, value, field, target) in [
        (form.modified_enabled, &form.modified, 1, &mut edit.modified),
        (form.created_enabled, &form.created, 2, &mut edit.created),
        (form.accessed_enabled, &form.accessed, 3, &mut edit.accessed),
    ] {
        if enabled {
            *target = Some(parse_time(value).map_err(|error| (field, error))?);
        }
    }
    if edit.set | edit.clear == 0
        && edit.modified.is_none()
        && edit.created.is_none()
        && edit.accessed.is_none()
    {
        return Err((0, io::Error::other("Gib mindestens eine Änderung an.")));
    }
    Ok(edit)
}

pub(super) fn parse_time(value: &str) -> io::Result<SystemTime> {
    let time = NaiveDateTime::parse_from_str(value.trim(), "%Y-%m-%d %H:%M:%S")
        .map_err(|_| io::Error::other("Verwende JJJJ-MM-TT HH:MM:SS für Zeitstempel."))?;
    let time = Local
        .from_local_datetime(&time)
        .single()
        .ok_or_else(|| io::Error::other("Die Ortszeit ist nicht eindeutig."))?;
    // The Windows API uses zero/negative FILETIME values as sentinels, not dates.
    let ticks = (i128::from(time.timestamp()) + 11_644_473_600) * 10_000_000;
    if ticks <= 0 || ticks > i128::from(i64::MAX) {
        return Err(io::Error::other(
            "Der Zeitpunkt liegt außerhalb des Windows-Zeitbereichs.",
        ));
    }
    Ok(time.into())
}

fn edit_selection(
    roots: Vec<PathBuf>,
    edit: MetadataEdit,
    recursive: bool,
    ignore: Option<&IgnoreList>,
    cancel: &AtomicBool,
    mut progress: impl FnMut(&Path, usize, Option<usize>, usize),
) -> TransferReport {
    let mut report = TransferReport::default();
    let mut stack = roots;
    let mut paths = Vec::new();
    let mut seen = BTreeSet::new();
    while let Some(path) = stack.pop() {
        if cancel.load(Ordering::Relaxed) {
            report.cancelled = true;
            return report;
        }
        progress(&path, paths.len(), None, report.errors.len());
        match files::regular_metadata(&path)
            .and_then(|metadata| Ok((fs::canonicalize(&path)?, metadata)))
        {
            Ok((canonical, metadata)) if seen.insert(canonical.clone()) => {
                if ignore.is_some_and(|ignore| ignore.local(&path, metadata.is_dir())) {
                    report.ignored += 1;
                    continue;
                }
                let directory = metadata.is_dir();
                paths.push((canonical, metadata));
                if recursive && directory {
                    match files::list_cancellable(&path, cancel) {
                        Ok(directory) => {
                            report.errors.extend(directory.errors);
                            stack.extend(
                                directory
                                    .entries
                                    .into_iter()
                                    .filter(|entry| entry.kind != files::Kind::Link)
                                    .map(|entry| entry.path),
                            );
                        }
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                            report.cancelled = true;
                            return report;
                        }
                        Err(error) => report
                            .errors
                            .push(format!("{}: {error}", display_path(&path))),
                    }
                }
            }
            Ok(_) => {}
            Err(error) => report
                .errors
                .push(format!("{}: {error}", display_path(&path))),
        }
    }
    let total = paths.len();
    for (index, (path, metadata)) in paths.into_iter().rev().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            report.cancelled = true;
            break;
        }
        progress(&path, index, Some(total), report.errors.len());
        // Observers may request cancellation immediately before the next write.
        if cancel.load(Ordering::Relaxed) {
            report.cancelled = true;
            break;
        }
        match files::ensure_unchanged(&path, &metadata)
            .and_then(|()| arco_windows::edit_metadata(&path, edit))
        {
            Ok(()) => report.completed += 1,
            Err(error) => report
                .errors
                .push(format!("{}: {error}", display_path(&path))),
        }
        progress(&path, index + 1, Some(total), report.errors.len());
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unix_modes_accept_all_permission_bits_and_reject_invalid_input() {
        for value in 0..=0o7777 {
            assert_eq!(parse_mode(&format!("{value:03o}")).unwrap(), value);
        }
        assert_eq!(parse_mode(" 0755 ").unwrap(), 0o755);
        for invalid in ["", "64", "888", "10000", "-644", "0x755", "755\n600", "rwx"] {
            assert!(parse_mode(invalid).is_err(), "{invalid}");
        }
    }
    #[test]
    fn metadata_form_preserves_unselected_fields_and_rejects_invalid_dates() {
        let form = MetadataForm {
            hidden: 1,
            readonly: 2,
            modified: "invalid but inactive".into(),
            ..Default::default()
        };
        let edit = form_options(&form).unwrap();
        assert_eq!(edit.set, 2);
        assert_eq!(edit.clear, 1);
        assert!(edit.modified.is_none());
        let bad = MetadataForm {
            created_enabled: true,
            created: "2026-02-30 00:00:00".into(),
            ..form
        };
        assert_eq!(form_options(&bad).err().unwrap().0, 2);
        assert!(parse_time("1500-01-01 00:00:00").is_err());
    }

    #[cfg(windows)]
    #[test]
    fn recursive_metadata_respects_ignore_rules_and_cancels_during_collection() {
        use std::os::windows::fs::MetadataExt;
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("root");
        let ignored = root.join("ignored");
        fs::create_dir_all(&ignored).unwrap();
        let file = root.join("normal.txt");
        let protected = ignored.join("skip.txt");
        fs::write(&file, b"normal").unwrap();
        fs::write(&protected, b"skip").unwrap();
        let edit = MetadataEdit {
            set: 2,
            ..Default::default()
        };
        let cancel = AtomicBool::new(false);
        let report = edit_selection(
            vec![root.clone()],
            edit,
            true,
            None,
            &cancel,
            |_, _, _, _| cancel.store(true, Ordering::Relaxed),
        );
        assert!(report.cancelled);
        assert_eq!(report.completed, 0);
        assert_eq!(fs::metadata(&file).unwrap().file_attributes() & 2, 0);
        let ignore = IgnoreList::parse("ignored/").unwrap();
        let report = edit_selection(
            vec![root.clone(), file.clone()],
            edit,
            true,
            Some(&ignore),
            &AtomicBool::new(false),
            |_, _, _, _| {},
        );
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(report.completed, 2);
        assert_eq!(report.ignored, 1);
        assert_ne!(fs::metadata(file).unwrap().file_attributes() & 2, 0);
        assert_eq!(fs::metadata(protected).unwrap().file_attributes() & 2, 0);
        assert_eq!(fs::metadata(ignored).unwrap().file_attributes() & 2, 0);
    }

    #[cfg(windows)]
    #[test]
    fn metadata_batch_cancels_at_write_boundary_and_deduplicates_paths() {
        use std::os::windows::fs::MetadataExt;
        let fixture = tempfile::tempdir().unwrap();
        let file = fixture.path().join("item.txt");
        fs::write(&file, b"sample").unwrap();
        let cancel = AtomicBool::new(false);
        let edit = MetadataEdit {
            set: 2,
            ..Default::default()
        };
        let report = edit_selection(
            vec![file.clone()],
            edit,
            false,
            None,
            &cancel,
            |_, _, total, _| {
                if total.is_some() {
                    cancel.store(true, Ordering::Relaxed);
                }
            },
        );
        assert!(report.cancelled);
        assert_eq!(report.completed, 0);
        assert_eq!(fs::metadata(&file).unwrap().file_attributes() & 2, 0);
        let report = edit_selection(
            vec![file.clone(), file.clone()],
            edit,
            false,
            None,
            &AtomicBool::new(false),
            |_, _, _, _| {},
        );
        assert_eq!(report.completed, 1);
        assert!(report.errors.is_empty());
        assert_ne!(fs::metadata(file).unwrap().file_attributes() & 2, 0);
    }
}
