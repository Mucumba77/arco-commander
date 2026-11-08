use super::archive_actions::check_archive;
use super::ftp_sync::{FtpPlan, FtpRelayPlan};
use super::{App, FtpEndpoint, Outcome, strings};
use crate::commands::Pending;
use crate::panel::ArchiveTab;
use crate::{SyncOptionsData, SyncRowData, display_path, human_size};
use arco_core::archive_view::{self, Selection};
use arco_core::files::{self, Kind, TransferReport};
use arco_core::sync::{
    self, Action, Comparison, EntryInfo, FileMasks, MarkRule, Mode, Options, PanelMarks, Plan, Row,
};
use arco_core::tools;
use chrono::{DateTime, Local};
use slint::{Model, ModelRc, VecModel};
use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const LAST_USED: &str = "Letzte Einstellungen";

pub(crate) struct SyncSetup {
    source: SyncSource,
    labels: [String; 2],
    selected: Vec<PathBuf>,
}

enum SyncSource {
    Disk {
        roots: [PathBuf; 2],
        archive: Option<(usize, ArchiveTab)>,
    },
    Ftp {
        local: PathBuf,
        endpoint: FtpEndpoint,
    },
}

impl SyncSource {
    fn archive(&self) -> Option<&(usize, ArchiveTab)> {
        match self {
            Self::Disk { archive, .. } => archive.as_ref(),
            Self::Ftp { .. } => None,
        }
    }
}

pub(crate) struct SyncReview {
    plan: ReviewPlan,
    rows: Vec<Row>,
    labels: [String; 2],
    visible: Vec<usize>,
    archive: Option<StagedArchive>,
    copy_only: bool,
}

enum ReviewPlan {
    Disk(Plan),
    Ftp(FtpPlan),
    FtpRelay(FtpRelayPlan),
}

impl ReviewPlan {
    fn apply(
        self,
        rows: Vec<Row>,
        cancel: &AtomicBool,
        progress: impl FnMut(&Path),
    ) -> TransferReport {
        match self {
            Self::Disk(mut plan) => {
                plan.rows = rows;
                plan.apply(cancel, progress)
            }
            Self::Ftp(mut plan) => {
                plan.rows = rows;
                plan.apply(cancel, progress)
            }
            Self::FtpRelay(mut plan) => {
                plan.rows = rows;
                plan.apply(cancel, progress)
            }
        }
    }
}

struct StagedArchive {
    storage: tempfile::TempDir,
    source: ArchiveTab,
}

impl SyncReview {
    pub(crate) fn close(self) {
        if let Some(archive) = self.archive {
            std::thread::spawn(move || drop(archive));
        }
    }
}

impl App {
    pub(crate) fn compare_panels(
        &mut self,
        include_folders: bool,
        rule: MarkRule,
        hide_equal: bool,
    ) {
        if self
            .panels
            .iter()
            .any(|panel| panel.loading || !panel.current().is_local())
        {
            self.status("Öffne zwei lokale Verzeichnisse und warte, bis sie geladen sind.");
            return;
        }
        let show_hidden = self.profile.show_hidden;
        if hide_equal
            && self
                .panels
                .iter()
                .any(|panel| panel.current().has_comparison_filter())
        {
            for side in 0..2 {
                self.panels[side]
                    .current_mut()
                    .set_comparison_filter(None, show_hidden);
                self.publish(side);
            }
            self.status("Gleiche Dateien werden wieder angezeigt.");
            return;
        }
        let roots = self
            .panels
            .each_ref()
            .map(|panel| panel.current().path.clone());
        let requests = self.panels.each_ref().map(|panel| panel.request);
        self.start_job("Vergleiche die Dateifenster …", false, move |job| {
            let marks = sync::compare_panels(
                roots.each_ref().map(PathBuf::as_path),
                include_folders,
                show_hidden,
                rule,
                &job.cancel,
            )?;
            Ok(Outcome::ComparedPanels {
                roots,
                requests,
                marks,
                hide_equal,
            })
        });
    }

    pub(super) fn show_panel_comparison(
        &mut self,
        roots: &[PathBuf; 2],
        requests: [u64; 2],
        marks: PanelMarks,
        hide_equal: bool,
    ) {
        if self
            .panels
            .iter()
            .zip(roots)
            .zip(requests)
            .any(|((panel, root), request)| {
                panel.loading
                    || panel.request != request
                    || panel.current().path != *root
                    || !panel.current().is_local()
            })
        {
            self.status("Die Dateifenster haben sich seit Beginn des Vergleichs geändert.");
            return;
        }
        let [left, right] = marks.marked.each_ref().map(|paths| paths.len());
        for (side, equal) in marks.equal.into_iter().enumerate() {
            let tab = self.panels[side].current_mut();
            tab.remember_selection();
            tab.set_comparison_filter(hide_equal.then_some(equal), self.profile.show_hidden);
            tab.replace_selection(|entry| marks.marked[side].contains(&entry.path));
            self.publish(side);
        }
        self.status(format!(
            "Vergleich abgeschlossen · {left} links und {right} rechts zum Markieren gefunden."
        ));
    }

    pub(crate) fn open_sync(&mut self) {
        if self
            .panels
            .iter()
            .any(|panel| panel.current().ftp.is_some())
        {
            self.open_ftp_sync();
            return;
        }
        if self.job.is_some() || self.panels.iter().any(|panel| panel.loading) {
            self.status("Warte, bis beide Dateifenster und der laufende Auftrag fertig sind.");
            return;
        }
        let [left, right] = self.panels.each_ref().map(|panel| panel.current());
        let archive = match (&left.archive, &right.archive) {
            (Some(_), Some(_)) => {
                self.status(
                    "Öffne für den Archivabgleich einen lokalen Zielordner im anderen Fenster.",
                );
                return;
            }
            (Some(archive), None) => Some((0, archive.clone())),
            (None, Some(archive)) => Some((1, archive.clone())),
            _ => None,
        };
        let roots = self
            .panels
            .each_ref()
            .map(|panel| panel.current().path.clone());
        let labels = self
            .panels
            .each_ref()
            .map(|panel| panel.current().location().display());
        let selected: Vec<_> = self
            .panels
            .iter()
            .flat_map(|panel| {
                let tab = panel.current();
                let base = tab
                    .archive
                    .as_ref()
                    .map_or(&tab.path, |archive| &archive.folder);
                tab.marked_paths()
                    .iter()
                    .filter_map(|path| path.strip_prefix(base).ok().map(|part| part.to_path_buf()))
                    .collect::<Vec<_>>()
            })
            .collect();
        self.begin_sync(SyncSetup {
            source: SyncSource::Disk { roots, archive },
            labels,
            selected,
        });
    }

    pub(crate) fn open_ftp_sync(&mut self) {
        let Some(endpoint) = self.ftp_endpoint() else {
            return;
        };
        let side = if self.current().ftp.is_some() {
            1 - self.active
        } else {
            self.active
        };
        let panel = &self.panels[side];
        let tab = panel.current();
        if self.job.is_some() || self.panels.iter().any(|panel| panel.loading) || !tab.is_local() {
            self.status(
                "Öffne einen lokalen Ordner neben dem FTP-Ordner und warte auf laufende Aufträge.",
            );
            return;
        }
        let local = tab.path.clone();
        let mut selected = tab
            .marked_paths()
            .iter()
            .filter_map(|path| path.strip_prefix(&local).ok().map(Path::to_path_buf))
            .collect::<Vec<_>>();
        let remote = self.panels[1 - side].current();
        if remote.ftp.as_ref().is_some_and(|tab| {
            std::sync::Arc::ptr_eq(&tab.location.session, &endpoint.0)
                && tab.location.directory == endpoint.1
        }) {
            selected.extend(remote.marked_paths());
        }
        let labels = [display_path(&local), endpoint.1.to_string()];
        self.begin_sync(SyncSetup {
            source: SyncSource::Ftp { local, endpoint },
            labels,
            selected,
        });
    }

    pub(crate) fn open_ftp_copy(&mut self) {
        let other = 1 - self.active;
        if let (Some(source), Some(target)) =
            (&self.current().ftp, &self.panels[other].current().ftp)
        {
            let selected = self.current().selected_paths();
            if selected.is_empty() {
                self.status("Wähle Dateien oder Ordner für die FTP-Kopie.");
                return;
            }
            let endpoints = [&source.location, &target.location]
                .map(|location| (location.session.clone(), location.directory.clone()));
            let labels = endpoints.each_ref().map(|endpoint| endpoint.1.to_string());
            self.start_job(
                "Bereite Kopie zwischen FTP-Ordnern vor …",
                false,
                move |job| {
                    let mut plan = FtpRelayPlan::for_copy(endpoints, &selected, &job.cancel)?;
                    let rows = std::mem::take(&mut plan.rows);
                    Ok(Outcome::Sync(Box::new(SyncReview {
                        plan: ReviewPlan::FtpRelay(plan),
                        rows,
                        labels,
                        visible: Vec::new(),
                        archive: None,
                        copy_only: true,
                    })))
                },
            );
            return;
        }
        let (local, remote, upload) =
            if self.current().is_local() && self.panels[other].current().ftp.is_some() {
                (self.active, other, true)
            } else if self.current().ftp.is_some() && self.panels[other].current().is_local() {
                (other, self.active, false)
            } else {
                self.status(
                "F5 benötigt für diese FTP-Übertragung einen lokalen Ordner im anderen Fenster.",
            );
                return;
            };
        let root = self.panels[local].current().path.clone();
        let remote = &self.panels[remote]
            .current()
            .ftp
            .as_ref()
            .expect("remote tab")
            .location;
        let endpoint = (remote.session.clone(), remote.directory.clone());
        let labels = [display_path(&root), remote.directory.to_string()];
        let selected = if upload {
            self.current()
                .paths()
                .iter()
                .filter_map(|path| path.strip_prefix(&root).ok().map(Path::to_path_buf))
                .collect()
        } else {
            self.current().selected_paths()
        };
        if selected.is_empty() {
            self.status("Wähle Dateien oder Ordner für die FTP-Kopie.");
            return;
        }
        self.start_job("Bereite FTP-Kopie vor …", false, move |job| {
            let mut plan =
                FtpPlan::for_copy(&root, endpoint, &selected, upload, &job.cancel, |_| {})?;
            let rows = std::mem::take(&mut plan.rows);
            Ok(Outcome::Sync(Box::new(SyncReview {
                plan: ReviewPlan::Ftp(plan),
                rows,
                labels,
                visible: Vec::new(),
                archive: None,
                copy_only: true,
            })))
        });
    }

    fn begin_sync(&mut self, setup: SyncSetup) {
        let ui = self.ui();
        ui.set_sync_roots(strings(setup.labels.iter().map(String::as_str)));
        ui.set_sync_has_selection(!setup.selected.is_empty());
        let archive = setup.source.archive();
        let ftp = matches!(setup.source, SyncSource::Ftp { .. });
        ui.set_sync_ftp(ftp);
        ui.set_sync_archive_side(archive.as_ref().map_or(-1, |(side, _)| *side as i32));
        ui.set_sync_archive_secret(archive.as_ref().is_some_and(|(_, archive)| {
            archive_view::is_zip(&archive.file) && archive.catalog.contains_encrypted_entries()
        }));
        ui.set_sync_notice("".into());
        self.publish_sync_profiles();
        let mut options = self.profile.sync_options.clone();
        options.selected_only &= !setup.selected.is_empty();
        if let Some((side, _)) = archive {
            set_archive_options(&mut options, *side);
        }
        ui.set_sync_draft(options_data(&options));
        self.pending = Some(Pending::SyncSetup(setup));
        ui.set_dialog_kind("sync-options".into());
    }

    fn publish_sync_profiles(&self) {
        self.ui().set_sync_presets(strings(
            std::iter::once(LAST_USED).chain(self.profile.sync_profiles.keys().map(String::as_str)),
        ));
    }

    pub(super) fn configure_sync(&mut self, data: SyncOptionsData, preset: &str, operation: i32) {
        let Some(Pending::SyncSetup(setup)) = self.pending.as_ref() else {
            return;
        };
        let ui = self.ui();
        ui.set_sync_notice("".into());
        if operation == 0 {
            let settings = if preset == LAST_USED {
                Some(&self.profile.sync_options)
            } else {
                self.profile.sync_profiles.get(preset)
            };
            if let Some(options) = settings {
                let mut options = options.clone();
                options.selected_only &= !setup.selected.is_empty();
                ui.set_sync_draft(options_data(&options));
                ui.set_sync_draft_revision(ui.get_sync_draft_revision().wrapping_add(1));
            }
            return;
        }
        let mut options = Options {
            mode: match data.mode {
                1 => Mode::LeftToRight,
                2 => Mode::Mirror,
                3 => Mode::RightToLeft,
                _ => Mode::BothWays,
            },
            recursive: data.recursive,
            empty_dirs: data.empty_dirs,
            selected_only: data.selected_only,
            by_content: data.by_content,
            ignore_date: data.ignore_date,
            masks: data.masks.trim().to_owned(),
        };
        if let Some((side, _)) = setup.source.archive() {
            set_archive_options(&mut options, *side);
        }
        if let Err(error) = options.validate() {
            ui.set_sync_notice(error.to_string().into());
            return;
        }
        if operation == 1 {
            let name = preset.trim();
            if name.is_empty()
                || name == LAST_USED
                || name.chars().count() > 80
                || name.chars().any(char::is_control)
            {
                ui.set_sync_notice("Gib einen Profilnamen mit höchstens 80 Zeichen ein.".into());
                return;
            }
            self.profile.sync_profiles.insert(name.to_owned(), options);
            self.publish_sync_profiles();
            self.save();
            ui.set_sync_notice(format!("Profil „{name}“ gespeichert.").into());
            return;
        }
        if options.selected_only && setup.selected.is_empty() {
            ui.set_sync_notice("Markiere zuerst Dateien oder Ordner in einem Dateifenster.".into());
            return;
        }
        let Some(Pending::SyncSetup(setup)) = self.pending.take() else {
            return;
        };
        if matches!(setup.source, SyncSource::Disk { archive: None, .. }) {
            self.profile.sync_options = options.clone();
            self.save();
        }
        let password =
            (!preset.is_empty() && setup.source.archive().is_some()).then(|| preset.to_owned());
        self.close_dialog();
        self.start_job("Vergleiche Verzeichnisse …", false, move |job| {
            let SyncSource::Disk { mut roots, archive } = setup.source else {
                let SyncSource::Ftp { local, endpoint } = setup.source else {
                    unreachable!()
                };
                let mut last = Instant::now() - Duration::from_secs(1);
                let mut plan = FtpPlan::compare(
                    &local,
                    endpoint,
                    &options,
                    &setup.selected,
                    &job.cancel,
                    |path| {
                        if last.elapsed() >= Duration::from_millis(150) {
                            last = Instant::now();
                            job.progress(format!("Vergleiche FTP · {}", display_path(path)));
                        }
                    },
                )?;
                let rows = std::mem::take(&mut plan.rows);
                return Ok(Outcome::Sync(Box::new(SyncReview {
                    plan: ReviewPlan::Ftp(plan),
                    rows,
                    labels: setup.labels,
                    visible: Vec::new(),
                    archive: None,
                    copy_only: false,
                })));
            };
            let archive = if let Some((side, source)) = archive {
                job.progress("Entpacke Archivdaten für den Vergleich …".into());
                let staged = stage_archive(
                    source,
                    &options,
                    &setup.selected,
                    password.as_deref(),
                    &job.cancel,
                )?;
                roots[side] = staged.storage.path().to_path_buf();
                Some((side, staged))
            } else {
                None
            };
            job.progress("Vergleiche Verzeichnisse …".into());
            let mut plan = sync::compare(
                roots.each_ref().map(PathBuf::as_path),
                &options,
                &setup.selected,
                &job.cancel,
            )?;
            let labels = if let Some((side, archive)) = &archive {
                plan.protect_path(1 - *side, &archive.source.file)?;
                setup.labels
            } else {
                plan.roots().each_ref().map(|root| display_path(root))
            };
            Ok(Outcome::Sync(Box::new(SyncReview {
                rows: std::mem::take(&mut plan.rows),
                plan: ReviewPlan::Disk(plan),
                labels,
                visible: Vec::new(),
                archive: archive.map(|(_, stage)| stage),
                copy_only: false,
            })))
        });
    }

    pub(super) fn show_sync(&mut self, review: Box<SyncReview>) {
        let ui = self.ui();
        ui.set_sync_roots(strings(review.labels.iter().map(String::as_str)));
        ui.set_sync_copy_only(review.copy_only);
        self.pending = Some(Pending::SyncReview(review));
        self.filter_sync(0);
        ui.set_dialog_kind("sync-review".into());
    }

    pub(super) fn filter_sync(&mut self, filter: i32) {
        let Some(Pending::SyncReview(review)) = self.pending.as_mut() else {
            return;
        };
        review.visible = review
            .rows
            .iter()
            .enumerate()
            .filter_map(|(index, row)| {
                let include = match filter {
                    0 => row.comparison != Comparison::Equal,
                    2 => row.comparison == Comparison::Equal,
                    3 => {
                        matches!(
                            row.comparison,
                            Comparison::TypeConflict | Comparison::Protected
                        ) || (row.comparison == Comparison::Different
                            && row.suggested == Action::Skip)
                    }
                    _ => true,
                };
                include.then_some(index)
            })
            .collect();
        let rows = review
            .visible
            .iter()
            .map(|&index| row_data(index, &review.rows[index], review.copy_only))
            .collect::<Vec<_>>();
        let summary = summary(review);
        let ui = self.ui();
        ui.set_sync_rows(ModelRc::from(Rc::new(VecModel::from(rows))));
        publish_summary(&ui, summary);
    }

    pub(super) fn choose_sync_action(&mut self, index: i32, action: i32) {
        let Ok(index) = usize::try_from(index) else {
            return;
        };
        let Some(Pending::SyncReview(review)) = self.pending.as_mut() else {
            return;
        };
        let Ok(position) = review.visible.binary_search(&index) else {
            return;
        };
        let action = match action {
            1 => Action::CopyLeft,
            2 => Action::CopyRight,
            3 => Action::DeleteRight,
            _ => return,
        };
        review.rows[index].toggle(action);
        let data = row_data(index, &review.rows[index], review.copy_only);
        let summary = summary(review);
        let ui = self.ui();
        let rows = ui.get_sync_rows();
        if let Some(rows) = rows.as_any().downcast_ref::<VecModel<SyncRowData>>() {
            rows.set_row_data(position, data);
        }
        publish_summary(&ui, summary);
    }

    pub(super) fn run_sync(&mut self, confirm_deletes: bool) {
        let Some(Pending::SyncReview(review)) = self.pending.as_ref() else {
            return;
        };
        let (_, copies, deletes) = summary(review);
        if copies + deletes == 0 || (deletes > 0 && !confirm_deletes) {
            return;
        }
        if self.job.is_some() {
            self.status("Ein Auftrag läuft bereits.");
            return;
        }
        let Some(Pending::SyncReview(review)) = self.pending.take() else {
            return;
        };
        let mut review = *review;
        let mut index = 0;
        review.rows.retain(|row| {
            let keep = row.action != Action::Skip && review.visible.binary_search(&index).is_ok();
            index += 1;
            keep
        });
        self.close_dialog();
        self.start_job("Synchronisiere …", true, move |job| {
            if let Some(archive) = &review.archive {
                check_archive(&archive.source)?;
            }
            let mut last = Instant::now() - Duration::from_secs(1);
            let report = review.plan.apply(review.rows, &job.cancel, |path| {
                if last.elapsed() >= Duration::from_millis(150) {
                    last = Instant::now();
                    job.progress(format!("Synchronisiere {}", display_path(path)));
                }
            });
            Ok(Outcome::Report(
                if review.copy_only {
                    "FTP-Kopie".into()
                } else {
                    "Synchronisierung".into()
                },
                report,
            ))
        });
    }
}

fn options_data(options: &Options) -> SyncOptionsData {
    SyncOptionsData {
        mode: match options.mode {
            Mode::BothWays => 0,
            Mode::LeftToRight => 1,
            Mode::Mirror => 2,
            Mode::RightToLeft => 3,
        },
        recursive: options.recursive,
        empty_dirs: options.empty_dirs,
        selected_only: options.selected_only,
        by_content: options.by_content,
        ignore_date: options.ignore_date,
        masks: options.masks.as_str().into(),
    }
}

fn set_archive_options(options: &mut Options, side: usize) {
    options.mode = if side == 0 {
        Mode::LeftToRight
    } else {
        Mode::RightToLeft
    };
    options.by_content = true;
    options.ignore_date = true;
}

fn stage_archive(
    source: ArchiveTab,
    options: &Options,
    selected: &[PathBuf],
    password: Option<&str>,
    cancel: &AtomicBool,
) -> io::Result<StagedArchive> {
    check_archive(&source)?;
    let storage = tempfile::tempdir()?;
    let query = tools::Search {
        names: files::compile_masks("*")?,
        text: None,
        min_size: None,
        max_size: None,
        max_depth: (!options.recursive).then_some(1),
        show_hidden: true,
    };
    let rows = source
        .catalog
        .search(&source.folder, &query, None, cancel)?
        .entries;
    let masks = FileMasks::new(&options.masks)?;
    let fold = |path: &Path| PathBuf::from(path.to_string_lossy().to_lowercase());
    let selected: BTreeSet<_> = selected.iter().map(|path| fold(path)).collect();
    let mut roots = Vec::new();
    for row in rows {
        if cancel.load(Ordering::Relaxed) {
            return Err(io::ErrorKind::Interrupted.into());
        }
        let relative = row
            .path
            .strip_prefix(&source.folder)
            .map_err(io::Error::other)?;
        if options.selected_only
            && !fold(relative)
                .ancestors()
                .any(|part| selected.contains(part))
        {
            continue;
        }
        if row.kind == Kind::Directory {
            if options.recursive && options.empty_dirs {
                fs::create_dir_all(storage.path().join(relative))?;
            }
        } else if masks.includes(relative) {
            roots.push(row.path);
        }
    }
    if !roots.is_empty() {
        let selection = Selection::new(source.folder.clone(), roots)?;
        archive_view::extract(&source.file, storage.path(), &selection, password, cancel)?;
    }
    check_archive(&source)?;
    Ok(StagedArchive { storage, source })
}

fn row_data(index: usize, row: &Row, copy_only: bool) -> SyncRowData {
    SyncRowData {
        index: index as i32,
        path: display_path(&row.relative).into(),
        left: describe(row.info(0)).into(),
        right: describe(row.info(1)).into(),
        comparison: match row.comparison {
            Comparison::Equal => "Gleich",
            Comparison::Different if copy_only => "Zieldatei vorhanden",
            Comparison::Different => "Abweichend",
            Comparison::OnlyLeft => "Nur links",
            Comparison::OnlyRight => "Nur rechts",
            Comparison::TypeConflict => "Datei/Ordner-Konflikt",
            Comparison::Protected => "Quellarchiv geschützt",
        }
        .into(),
        action: match row.action {
            Action::Skip => 0,
            Action::CopyLeft => 1,
            Action::CopyRight => 2,
            Action::DeleteRight => 3,
        },
        can_left: row.can(Action::CopyLeft),
        can_right: row.can(Action::CopyRight),
        can_delete: row.can(Action::DeleteRight),
    }
}

fn describe(meta: Option<EntryInfo>) -> String {
    let Some(meta) = meta else {
        return "—".into();
    };
    let Some(size) = meta.size else {
        return "Ordner".into();
    };
    let mut text = human_size(size);
    if let Some(time) = meta.modified {
        let time = DateTime::<Local>::from(time);
        text.push_str(&format!(" · {}", time.format("%d.%m.%y %H:%M")));
    }
    text
}

fn summary(review: &SyncReview) -> (usize, usize, usize) {
    let mut copies = 0;
    let mut deletes = 0;
    for &index in &review.visible {
        match review.rows[index].action {
            Action::CopyLeft | Action::CopyRight => copies += 1,
            Action::DeleteRight => deletes += 1,
            Action::Skip => (),
        }
    }
    (review.visible.len(), copies, deletes)
}

fn publish_summary(ui: &crate::MainWindow, (visible, copies, deletes): (usize, usize, usize)) {
    ui.set_sync_summary(
        format!("{visible} angezeigt · {copies} Kopien · {deletes} Löschungen rechts").into(),
    );
    ui.set_sync_has_actions(copies + deletes > 0);
    ui.set_sync_has_deletes(deletes > 0);
}

#[cfg(test)]
mod tests {
    use super::*;
    use arco_core::archive_view::Catalog;
    use arco_core::archives::create_zip;
    use std::fs;
    use std::sync::Arc;
    use std::time::SystemTime;

    #[test]
    fn archive_sync_uses_selected_members_and_protects_its_own_source_archive() {
        let fixture = tempfile::tempdir().unwrap();
        let source = fixture.path().join("source");
        let local = fixture.path().join("local");
        fs::create_dir_all(source.join("docs")).unwrap();
        fs::create_dir(&local).unwrap();
        let wanted = source.join("docs").join("probe.txt");
        fs::write(&wanted, b"archived").unwrap();
        fs::write(source.join("docs").join("unused.txt"), b"ignore").unwrap();
        fs::write(
            source.join("sample.zip"),
            b"must not overwrite source archive",
        )
        .unwrap();
        let timestamp = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        fs::File::options()
            .write(true)
            .open(&wanted)
            .unwrap()
            .set_modified(timestamp)
            .unwrap();
        let archive = local.join("sample.zip");
        let cancel = AtomicBool::new(false);
        create_zip(
            &[source.join("docs"), source.join("sample.zip")],
            &source,
            &archive,
            None,
            &cancel,
        )
        .unwrap();
        let before = fs::read(&archive).unwrap();
        let source = ArchiveTab {
            catalog: Arc::new(Catalog::read(&archive, &cancel).unwrap()),
            file: archive.clone(),
            folder: PathBuf::new(),
        };
        let mut options = Options {
            selected_only: true,
            ..Options::default()
        };
        set_archive_options(&mut options, 1);
        let selection = [
            PathBuf::from("sample.zip"),
            PathBuf::from("docs").join("probe.txt"),
        ];
        let stage = stage_archive(source, &options, &selection, None, &cancel).unwrap();
        assert!(
            !stage
                .storage
                .path()
                .join("docs")
                .join("unused.txt")
                .exists()
        );
        let mut plan = sync::compare(
            [&local, stage.storage.path()],
            &options,
            &selection,
            &cancel,
        )
        .unwrap();
        plan.protect_path(0, &archive).unwrap();
        let protected = plan
            .rows
            .iter()
            .find(|row| row.relative == Path::new("sample.zip"))
            .unwrap();
        assert_eq!(protected.comparison, Comparison::Protected);
        assert!(!protected.can(Action::CopyLeft));
        let report = plan.apply(&cancel, |_| {});
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(report.completed, 2);
        assert_eq!(fs::read(&archive).unwrap(), before);
        let restored = local.join("docs").join("probe.txt");
        assert_eq!(fs::read(&restored).unwrap(), b"archived");
        assert_eq!(
            fs::metadata(restored).unwrap().modified().unwrap(),
            timestamp
        );
    }
}
