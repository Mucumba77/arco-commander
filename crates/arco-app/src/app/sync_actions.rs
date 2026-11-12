use super::archive_actions::check_archive;
use super::ftp_sync::{FtpPlan, FtpRelayPlan, ftp_ignores};
use super::{App, FtpEndpoint, Outcome, strings};
use crate::commands::{Dialog, Pending};
use crate::panel::ArchiveTab;
use crate::{SyncOptionsData, SyncRowData, display_path, human_size};
use arco_core::archive_view::{self, Selection};
use arco_core::files::{self, Kind, TransferReport};
use arco_core::profile::SavedSyncProfile;
use arco_core::sync::{
    self, Action, Comparison, EntryInfo, FileMasks, MarkRule, Mode, Options, PanelMarks, Plan, Row,
};
use arco_core::tools;
use chrono::{DateTime, Local};
use slint::{Model, ModelRc, VecModel};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use zeroize::Zeroizing;

const LAST_USED: &str = "Letzte Einstellungen";

pub(crate) struct SyncSetup {
    source: SyncSource,
    labels: [String; 2],
    selected: Vec<PathBuf>,
    fixed_roots: Option<[PathBuf; 2]>,
}

impl SyncSetup {
    fn local_roots(&self) -> Option<&[PathBuf; 2]> {
        match &self.source {
            SyncSource::Disk { roots, archives } if archives.iter().all(Option::is_none) => {
                Some(self.fixed_roots.as_ref().unwrap_or(roots))
            }
            _ => None,
        }
    }

    fn selected_available(&self) -> bool {
        if self.selected.is_empty() {
            return false;
        }
        match (&self.fixed_roots, &self.source) {
            (Some(fixed), SyncSource::Disk { roots, .. }) => fixed == roots,
            _ => true,
        }
    }

    fn displayed_roots(&self) -> [String; 2] {
        self.fixed_roots.as_ref().map_or_else(
            || self.labels.clone(),
            |roots| roots.each_ref().map(|path| display_path(path)),
        )
    }
}

enum SyncSource {
    Disk {
        roots: [PathBuf; 2],
        archives: [Option<ArchiveTab>; 2],
    },
    Ftp {
        local: PathBuf,
        endpoint: FtpEndpoint,
        profile: Option<String>,
    },
}

impl SyncSource {
    fn archives(&self) -> [Option<&ArchiveTab>; 2] {
        match self {
            Self::Disk { archives, .. } => archives.each_ref().map(Option::as_ref),
            Self::Ftp { .. } => [None, None],
        }
    }
}

pub(crate) struct SyncReview {
    plan: ReviewPlan,
    rows: Vec<Row>,
    labels: [String; 2],
    visible: Vec<usize>,
    archives: [Option<StagedArchive>; 2],
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
        job: &super::JobContext,
        copy_conflicts: bool,
        progress: impl FnMut(&Path),
    ) -> io::Result<TransferReport> {
        let cancel = &job.cancel;
        let mut skipped = 0;
        let mut report = match self {
            Self::Disk(mut plan) => {
                plan.rows = rows;
                plan.apply(cancel, progress)
            }
            Self::Ftp(mut plan) => {
                plan.rows = rows;
                if copy_conflicts {
                    skipped = plan.resolve_conflicts(job)?;
                }
                plan.apply(cancel, progress)
            }
            Self::FtpRelay(mut plan) => {
                plan.rows = rows;
                if copy_conflicts {
                    skipped = plan.resolve_conflicts(job)?;
                }
                plan.apply(cancel, progress)
            }
        };
        report.skipped += skipped;
        Ok(report)
    }
}

struct StagedArchive {
    storage: tempfile::TempDir,
    source: ArchiveTab,
    password: Option<Zeroizing<String>>,
}

impl SyncReview {
    pub(crate) fn close(self) {
        if self.archives.iter().any(Option::is_some) {
            std::thread::spawn(move || drop(self.archives));
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
        let description = super::JobDescription::new(
            crate::OperationKind::Sync,
            display_path(&roots[0]),
            display_path(&roots[1]),
        );
        self.start_operation_job(
            "Vergleiche die Dateifenster …",
            false,
            description,
            move |job| {
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
            },
        );
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
        if [left, right].iter().any(|tab| tab.wfx.is_some()) {
            self.status("Der Verzeichnisabgleich benötigt lokale Ordner oder Archivordner.");
            return;
        }
        let archives = [left.archive.clone(), right.archive.clone()];
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
            source: SyncSource::Disk { roots, archives },
            labels,
            selected,
            fixed_roots: None,
        });
    }

    pub(crate) fn open_sync_profiles(&mut self) {
        let mut names = vec![LAST_USED.to_owned()];
        let mut labels = vec![LAST_USED.to_owned()];
        for with_roots in [true, false] {
            for (name, profile) in self
                .profile
                .sync_profiles
                .iter()
                .filter(|(_, profile)| profile.roots.is_some() == with_roots)
            {
                names.push(name.clone());
                labels.push(match &profile.roots {
                    Some([left, right]) => {
                        format!("{name} · {} ↔ {}", display_path(left), display_path(right))
                    }
                    None => format!("{name} · aktuelle Dateifenster"),
                });
            }
        }
        let options: Vec<_> = labels.iter().map(String::as_str).collect();
        self.dialog(
            "confirm",
            Pending::SyncProfilePicker(names),
            Dialog {
                title: "Synchronisierungsprofil öffnen",
                description: "Das gewählte Profil öffnet die Vergleichseinstellungen.",
                options: &options,
                ..Default::default()
            },
        );
    }

    pub(crate) fn open_sync_profile(&mut self, name: &str) -> io::Result<()> {
        if self.job.is_some() || self.panels.iter().any(|panel| panel.loading) {
            return Err(io::Error::other(
                "Warte, bis beide Dateifenster und der laufende Auftrag fertig sind.",
            ));
        }
        if self.panels.iter().all(|panel| panel.current().is_local()) {
            self.open_sync();
        } else {
            let roots = self
                .profile
                .sync_profiles
                .get(name)
                .and_then(|profile| profile.roots.clone())
                .ok_or_else(|| {
                    io::Error::other("Dieses Profil benötigt zwei lokale Dateifenster.")
                })?;
            if roots.iter().any(|path| !path.is_absolute()) {
                return Err(io::Error::other(
                    "Das gespeicherte Verzeichnispaar enthält einen relativen Pfad.",
                ));
            }
            let labels = roots.each_ref().map(|path| display_path(path));
            self.begin_sync(SyncSetup {
                source: SyncSource::Disk {
                    roots,
                    archives: [None, None],
                },
                labels,
                selected: Vec::new(),
                fixed_roots: None,
            });
        }
        self.configure_sync(SyncOptionsData::default(), name, 0, ["", ""]);
        Ok(())
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
        let matching_remote = remote.ftp.as_ref().filter(|tab| {
            std::sync::Arc::ptr_eq(&tab.location.session, &endpoint.0)
                && tab.location.directory == endpoint.1
        });
        if matching_remote.is_some() {
            selected.extend(remote.marked_paths());
        }
        let labels = [display_path(&local), endpoint.1.to_string()];
        self.begin_sync(SyncSetup {
            source: SyncSource::Ftp {
                local,
                endpoint,
                profile: matching_remote.and_then(|tab| tab.location.profile.clone()),
            },
            labels,
            selected,
            fixed_roots: None,
        });
    }

    pub(crate) fn open_ftp_copy(&mut self) {
        let other = 1 - self.active;
        let ignore = self.ignore_list();
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
            let profiles =
                [&source.location, &target.location].map(|location| location.profile.clone());
            let labels = endpoints.each_ref().map(|endpoint| endpoint.1.to_string());
            self.enqueue_virtual_transfer(
                "Bereite Kopie zwischen FTP-Ordnern vor …",
                false,
                move |job| {
                    let from =
                        ftp_ignores(ignore.as_deref(), &endpoints[0].1, profiles[0].as_deref());
                    let to =
                        ftp_ignores(ignore.as_deref(), &endpoints[1].1, profiles[1].as_deref());
                    let mut plan = FtpRelayPlan::for_copy(
                        endpoints.clone(),
                        &selected,
                        &|path, directory| from(path, directory) || to(path, directory),
                        &job.cancel,
                    )?;
                    let mut rows = std::mem::take(&mut plan.rows);
                    super::ftp_sync::enable_copy_conflicts(&mut rows, 0);
                    Ok(Outcome::Sync(Box::new(SyncReview {
                        plan: ReviewPlan::FtpRelay(plan),
                        rows,
                        labels,
                        visible: Vec::new(),
                        archives: [None, None],
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
        let profile = remote.profile.clone();
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
        self.enqueue_virtual_transfer("Bereite FTP-Kopie vor …", false, move |job| {
            let remote_ignored = ftp_ignores(ignore.as_deref(), &endpoint.1, profile.as_deref());
            let ignored = |path: &Path, directory| {
                remote_ignored(path, directory)
                    || ignore
                        .as_ref()
                        .is_some_and(|ignore| ignore.local(&root.join(path), directory))
            };
            let mut plan = FtpPlan::for_copy(
                &root,
                endpoint.clone(),
                &selected,
                upload,
                &ignored,
                &job.cancel,
                |_| {},
            )?;
            let mut rows = std::mem::take(&mut plan.rows);
            super::ftp_sync::enable_copy_conflicts(&mut rows, usize::from(!upload));
            Ok(Outcome::Sync(Box::new(SyncReview {
                plan: ReviewPlan::Ftp(plan),
                rows,
                labels,
                visible: Vec::new(),
                archives: [None, None],
                copy_only: true,
            })))
        });
    }

    fn begin_sync(&mut self, setup: SyncSetup) {
        let ui = self.ui();
        ui.set_sync_roots(strings(setup.labels.iter().map(String::as_str)));
        ui.set_sync_has_selection(!setup.selected.is_empty());
        let archives = setup.source.archives();
        let archive_side = match archives {
            [None, None] => -1,
            [Some(_), None] => 0,
            [None, Some(_)] => 1,
            [Some(_), Some(_)] => 2,
        };
        let ftp = matches!(setup.source, SyncSource::Ftp { .. });
        ui.set_sync_ftp(ftp);
        ui.set_sync_archive_side(archive_side);
        ui.set_sync_archive_writable(matches!(archives, [Some(archive), None] | [None, Some(archive)] if archive.catalog.can_sync_write()));
        let secrets = archives.map(|archive| {
            archive.is_some_and(|archive| {
                archive.password().is_none()
                    && archive_view::supports_password(&archive.file)
                    && archive.catalog.contains_encrypted_entries()
            })
        });
        ui.set_sync_archive_left_secret(secrets[0]);
        ui.set_sync_archive_right_secret(secrets[1]);
        ui.set_sync_notice("".into());
        self.publish_sync_profiles();
        let mut options = self.profile.sync_options.clone();
        options.selected_only &= !setup.selected.is_empty();
        match archives {
            [Some(_), None] => set_archive_options(&mut options, 0),
            [None, Some(_)] => set_archive_options(&mut options, 1),
            [Some(_), Some(_)] => {
                options.mode = Mode::BothWays;
                options.by_content = true;
                options.ignore_date = true;
            }
            [None, None] => {}
        }
        ui.set_sync_draft(options_data(&options, false));
        self.pending = Some(Pending::SyncSetup(setup));
        ui.set_dialog_kind("sync-options".into());
    }

    fn publish_sync_profiles(&self) {
        self.ui().set_sync_presets(strings(
            std::iter::once(LAST_USED).chain(self.profile.sync_profiles.keys().map(String::as_str)),
        ));
    }

    pub(super) fn configure_sync(
        &mut self,
        data: SyncOptionsData,
        preset: &str,
        operation: i32,
        passwords: [&str; 2],
    ) {
        let ui = self.ui();
        let Some(Pending::SyncSetup(setup)) = self.pending.as_mut() else {
            return;
        };
        ui.set_sync_notice("".into());
        if operation == 0 {
            let saved = if preset == LAST_USED {
                Some((self.profile.sync_options.clone(), None))
            } else {
                self.profile
                    .sync_profiles
                    .get(preset)
                    .map(|saved| (saved.options.clone(), saved.roots.clone()))
            };
            if let Some((mut options, roots)) = saved {
                if roots
                    .as_ref()
                    .is_some_and(|paths| paths.iter().any(|path| !path.is_absolute()))
                {
                    ui.set_sync_notice(
                        "Das gespeicherte Verzeichnispaar enthält einen relativen Pfad.".into(),
                    );
                    return;
                }
                setup.fixed_roots = roots;
                options.selected_only &= setup.selected_available();
                ui.set_sync_roots(strings(setup.displayed_roots().iter().map(String::as_str)));
                ui.set_sync_has_selection(setup.selected_available());
                ui.set_sync_draft(options_data(&options, setup.fixed_roots.is_some()));
                ui.set_sync_draft_revision(ui.get_sync_draft_revision().wrapping_add(1));
            }
            return;
        }
        if operation == 3 {
            if self.profile.sync_profiles.remove(preset).is_some() {
                self.publish_sync_profiles();
                self.save();
                ui.set_sync_notice(format!("Profil „{preset}“ entfernt.").into());
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
        let archives = setup.source.archives();
        let has_archive = archives.iter().any(Option::is_some);
        if has_archive {
            options.by_content = true;
            options.ignore_date = true;
            match archives {
                [Some(archive), None] if !archive.catalog.can_sync_write() => {
                    set_archive_options(&mut options, 0);
                }
                [None, Some(archive)] if !archive.catalog.can_sync_write() => {
                    set_archive_options(&mut options, 1);
                }
                [Some(_), Some(_)] => options.mode = Mode::BothWays,
                _ => {}
            }
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
            let roots = if data.remember_roots {
                setup.local_roots().cloned()
            } else {
                None
            };
            self.profile
                .sync_profiles
                .insert(name.to_owned(), SavedSyncProfile { options, roots });
            self.publish_sync_profiles();
            self.save();
            ui.set_sync_notice(format!("Profil „{name}“ gespeichert.").into());
            return;
        }
        if options.selected_only && !setup.selected_available() {
            ui.set_sync_notice("Markiere zuerst Dateien oder Ordner in einem Dateifenster.".into());
            return;
        }
        let Some(Pending::SyncSetup(mut setup)) = self.pending.take() else {
            return;
        };
        if let Some(fixed) = setup.fixed_roots.take()
            && let SyncSource::Disk { roots, .. } = &mut setup.source
        {
            if *roots != fixed {
                setup.selected.clear();
            }
            setup.labels = fixed.each_ref().map(|path| display_path(path));
            *roots = fixed;
        }
        if !has_archive && matches!(setup.source, SyncSource::Disk { .. }) {
            self.profile.sync_options = options.clone();
            self.save();
        }
        let passwords = passwords.map(|password| {
            (!password.is_empty() && has_archive).then(|| Zeroizing::new(password.to_owned()))
        });
        let ignore = self.ignore_list();
        self.close_dialog();
        let description = super::JobDescription::new(
            crate::OperationKind::Sync,
            setup.labels[0].clone(),
            setup.labels[1].clone(),
        );
        self.start_operation_job(
            "Verzeichnisse vergleichen",
            false,
            description,
            move |job| {
                job.phase("Verzeichnisse erfassen und vergleichen …");
                let feed = job.progress_feed();
                let SyncSource::Disk {
                    mut roots,
                    archives,
                } = setup.source
                else {
                    let SyncSource::Ftp {
                        local,
                        endpoint,
                        profile,
                    } = setup.source
                    else {
                        unreachable!()
                    };
                    let mut last = Instant::now() - Duration::from_secs(1);
                    let remote_ignored =
                        ftp_ignores(ignore.as_deref(), &endpoint.1, profile.as_deref());
                    let ignored = |path: &Path, directory| {
                        remote_ignored(path, directory)
                            || ignore
                                .as_ref()
                                .is_some_and(|ignore| ignore.local(&local.join(path), directory))
                    };
                    let mut plan = FtpPlan::compare(
                        &local,
                        endpoint.clone(),
                        &options,
                        &setup.selected,
                        &ignored,
                        &job.cancel,
                        |path| {
                            if last.elapsed() >= Duration::from_millis(150) {
                                last = Instant::now();
                                if let Ok(mut slot) = feed.lock() {
                                    *slot = Some(super::job_progress::Sample {
                                        source: display_path(path),
                                        phase: "FTP-Einträge vergleichen …".into(),
                                        ..Default::default()
                                    });
                                }
                            }
                        },
                    )?;
                    let rows = std::mem::take(&mut plan.rows);
                    return Ok(Outcome::Sync(Box::new(SyncReview {
                        plan: ReviewPlan::Ftp(plan),
                        rows,
                        labels: setup.labels,
                        visible: Vec::new(),
                        archives: [None, None],
                        copy_only: false,
                    })));
                };
                let mut stages = [None, None];
                for (side, (source, password)) in archives.into_iter().zip(passwords).enumerate() {
                    if let Some(source) = source {
                        job.phase("Archivdaten für den Vergleich bereitstellen …");
                        let staged = stage_archive(
                            source,
                            &options,
                            &setup.selected,
                            password.as_ref().map(|password| password.as_str()),
                            &job.cancel,
                        )?;
                        roots[side] = staged.storage.path().to_path_buf();
                        stages[side] = Some(staged);
                    }
                }
                job.phase("Verzeichnisse erfassen und vergleichen …");
                let mut plan = sync::compare_ignoring_with_progress(
                    roots.each_ref().map(PathBuf::as_path),
                    &options,
                    &setup.selected,
                    if has_archive { None } else { ignore.as_deref() },
                    &job.cancel,
                    |path, examined| {
                        if let Ok(mut slot) = feed.lock() {
                            *slot = Some(super::job_progress::Sample {
                                source: display_path(path),
                                processed_items: Some(examined),
                                phase: "Verzeichnisse erfassen und vergleichen …".into(),
                                ..Default::default()
                            });
                        }
                    },
                )?;
                let labels = match &stages {
                    [None, None] => plan.roots().each_ref().map(|root| display_path(root)),
                    [Some(_), Some(_)] => {
                        plan.disable_actions();
                        setup.labels
                    }
                    _ => {
                        for (side, archive) in stages.iter().enumerate() {
                            if let Some(archive) = archive {
                                for file in archive.source.catalog.storage_paths() {
                                    plan.protect_path(1 - side, file)?;
                                }
                            }
                        }
                        setup.labels
                    }
                };
                Ok(Outcome::Sync(Box::new(SyncReview {
                    rows: std::mem::take(&mut plan.rows),
                    plan: ReviewPlan::Disk(plan),
                    labels,
                    visible: Vec::new(),
                    archives: stages,
                    copy_only: false,
                })))
            },
        );
    }

    pub(super) fn show_sync(&mut self, review: Box<SyncReview>) {
        let ui = self.ui();
        ui.set_sync_roots(strings(review.labels.iter().map(String::as_str)));
        ui.set_sync_copy_only(review.copy_only);
        ui.set_sync_comparison_only(review.archives.iter().all(Option::is_some));
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
        if self.job.is_some() && !review.copy_only {
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
        let copy_only = review.copy_only;
        let total = review.rows.len();
        let description = super::JobDescription::new(
            crate::OperationKind::Sync,
            review.labels[0].clone(),
            review.labels[1].clone(),
        );
        let work = move |job: super::JobContext| {
            for archive in review.archives.iter().flatten() {
                check_archive(&archive.source)?;
            }
            let mut last = Instant::now() - Duration::from_secs(1);
            let mut started = 0;
            let progress = |path: &Path| {
                started += 1;
                if last.elapsed() >= Duration::from_millis(100) || started == total {
                    last = Instant::now();
                    job.entry_progress(
                        display_path(path),
                        started.saturating_sub(1),
                        total,
                        "Ausgewählte Aktionen ausführen …",
                    );
                }
            };
            let report = match (review.plan, &review.archives) {
                (ReviewPlan::Disk(plan), [Some(archive), None]) => {
                    apply_archive_sync(plan, review.rows, 0, archive, &job.cancel, progress)
                }
                (ReviewPlan::Disk(plan), [None, Some(archive)]) => {
                    apply_archive_sync(plan, review.rows, 1, archive, &job.cancel, progress)
                }
                (_, [Some(_), Some(_)]) => {
                    return Err(io::Error::other(
                        "Der Vergleich zweier Archive enthält keine Schreibaktionen.",
                    ));
                }
                (plan, _) => plan.apply(review.rows, &job, review.copy_only, progress)?,
            };
            if !report.cancelled {
                job.entry_progress(
                    String::new(),
                    total,
                    total,
                    "Synchronisierung abgeschlossen",
                );
            }
            Ok(Outcome::Report(
                if review.copy_only {
                    "FTP-Kopie".into()
                } else {
                    "Synchronisierung".into()
                },
                report,
            ))
        };
        if copy_only {
            self.enqueue_virtual_operation("FTP-Kopie", true, description, work);
        } else {
            self.start_operation_job("Synchronisieren", true, description, work);
        }
    }
}

fn options_data(options: &Options, remember_roots: bool) -> SyncOptionsData {
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
        remember_roots,
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

fn apply_archive_sync(
    mut plan: Plan,
    rows: Vec<Row>,
    side: usize,
    archive: &StagedArchive,
    cancel: &AtomicBool,
    mut progress: impl FnMut(&Path),
) -> TransferReport {
    let total = rows.len();
    let (archive_rows, local_rows): (Vec<_>, Vec<_>) =
        rows.into_iter().partition(|row| match row.action {
            Action::CopyLeft => side == 0,
            Action::CopyRight | Action::DeleteRight => side == 1,
            Action::Skip => false,
        });
    let mut completed = 0;
    if !archive_rows.is_empty() {
        if !archive.source.catalog.can_sync_write() {
            return TransferReport {
                skipped: total,
                errors: vec!["Das Archiv ist für Änderungen gesperrt.".into()],
                ..Default::default()
            };
        }
        plan.rows = archive_rows;
        // Apply the reviewed snapshot checks before writing anything to the actual archive.
        let staged = plan.apply(cancel, &mut progress);
        if staged.completed != plan.rows.len() || staged.cancelled || !staged.errors.is_empty() {
            return TransferReport {
                completed: 0,
                skipped: total,
                ..staged
            };
        }
        if let Err(error) = write_archive_delta(&plan.rows, side, archive, cancel) {
            let cancelled = error.kind() == io::ErrorKind::Interrupted;
            return TransferReport {
                skipped: total,
                errors: if cancelled {
                    Vec::new()
                } else {
                    vec![error.to_string()]
                },
                cancelled,
                ..Default::default()
            };
        }
        completed = staged.completed;
    }
    plan.rows = local_rows;
    let mut report = plan.apply(cancel, progress);
    report.completed += completed;
    report
}

fn write_archive_delta(
    rows: &[Row],
    side: usize,
    archive: &StagedArchive,
    cancel: &AtomicBool,
) -> io::Result<()> {
    let delta = tempfile::tempdir()?;
    let mut removals = Vec::new();
    let mut additions = Vec::new();
    for row in rows {
        if cancel.load(Ordering::Relaxed) {
            return Err(io::ErrorKind::Interrupted.into());
        }
        let relative = row.path(side);
        let member = archive.source.folder.join(relative);
        if row.action == Action::DeleteRight || row.info(side).is_some() {
            removals.push(member.clone());
        }
        if row.action != Action::DeleteRight {
            let target = delta.path().join(&member);
            let source = archive.storage.path().join(relative);
            if fs::metadata(&source)?.is_dir() {
                if archive.source.catalog.has_directory(&member) {
                    continue;
                }
                fs::create_dir_all(&target)?;
            } else {
                fs::create_dir_all(target.parent().expect("Archive member has a parent"))?;
                fs::rename(source, &target)?;
            }
            additions.push(target);
        }
    }
    archive.source.catalog.apply_sync_changes(
        removals,
        &additions,
        delta.path(),
        archive
            .password
            .as_ref()
            .map(|password| password.as_str())
            .or(archive.source.password()),
        cancel,
    )
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
        ignore: None,
    };
    let rows = source
        .catalog
        .search(&source.folder, &query, None, cancel)?
        .entries;
    let masks = FileMasks::new(&options.masks)?;
    let scope = sync::Scope::new(options.selected_only, selected)?;
    let mut roots = Vec::new();
    for row in rows {
        if cancel.load(Ordering::Relaxed) {
            return Err(io::ErrorKind::Interrupted.into());
        }
        let relative = row
            .path
            .strip_prefix(&source.folder)
            .map_err(io::Error::other)?;
        if !scope.visits(relative) {
            continue;
        }
        if row.kind == Kind::Directory {
            fs::create_dir_all(storage.path().join(relative))?;
        } else if masks.includes(relative) {
            roots.push(row.path);
        }
    }
    if !roots.is_empty() {
        let selection = Selection::new(source.folder.clone(), roots)?;
        archive_view::extract(
            &source.file,
            storage.path(),
            &selection,
            password.or(source.password()),
            cancel,
        )?;
    }
    check_archive(&source)?;
    Ok(StagedArchive {
        storage,
        source,
        password: password.map(|password| Zeroizing::new(password.to_owned())),
    })
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
            password: None,
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

    #[test]
    fn archive_sync_writes_only_reviewed_rows_to_zip_and_split_7z() {
        let cancel = AtomicBool::new(false);
        let password = "test fixture password";
        for split in [false, true] {
            let fixture = tempfile::tempdir().unwrap();
            let local = fixture.path().join("local");
            let input = fixture.path().join("input");
            let folder = input.join("scope");
            fs::create_dir_all(folder.join("Nested")).unwrap();
            let local_nested = local.join(if cfg!(windows) { "nested" } else { "Nested" });
            fs::create_dir_all(&local_nested).unwrap();
            fs::write(folder.join("change.txt"), b"old").unwrap();
            fs::write(folder.join("remove.txt"), b"remove me").unwrap();
            fs::write(folder.join("skip.txt"), b"archive version").unwrap();
            fs::write(folder.join("Nested").join("excluded.bin"), b"keep me").unwrap();
            fs::write(input.join("outside.txt"), b"outside scope").unwrap();
            fs::write(local.join("change.txt"), b"new").unwrap();
            fs::write(local.join("skip.txt"), b"local version").unwrap();
            fs::write(local_nested.join("new.txt"), b"added").unwrap();
            let sources = [folder, input.join("outside.txt")];
            let archive = if split {
                archive_view::pack_split(
                    &sources,
                    &input,
                    &local.join("probe.7z"),
                    89,
                    Some(password),
                    &cancel,
                )
                .unwrap();
                local.join("probe.7z.001")
            } else {
                let archive = local.join("probe.zip");
                create_zip(&sources, &input, &archive, None, &cancel).unwrap();
                archive
            };
            let password = split.then_some(password);
            let options = Options {
                mode: Mode::Mirror,
                recursive: true,
                empty_dirs: false,
                by_content: true,
                ignore_date: true,
                masks: "*.txt;*.zip;*.0*".into(),
                ..Options::default()
            };
            let source = ArchiveTab {
                password: None,
                catalog: Arc::new(
                    Catalog::read_with_password(&archive, password, &cancel).unwrap(),
                ),
                file: archive.clone(),
                folder: PathBuf::from("scope"),
            };
            let stage = stage_archive(source, &options, &[], password, &cancel).unwrap();
            let mut plan =
                sync::compare([&local, stage.storage.path()], &options, &[], &cancel).unwrap();
            for part in stage.source.catalog.storage_paths() {
                plan.protect_path(0, part).unwrap();
                assert!(plan.rows.iter().any(|row| {
                    row.relative == Path::new(part.file_name().unwrap())
                        && row.comparison == Comparison::Protected
                }));
            }
            let mut rows = std::mem::take(&mut plan.rows);
            rows.retain(|row| row.action != Action::Skip && row.relative != Path::new("skip.txt"));
            let report = apply_archive_sync(plan, rows, 1, &stage, &cancel, |_| {});
            assert!(report.errors.is_empty(), "{report:?}");
            assert_eq!(report.completed, 3);
            let output = fixture.path().join("output");
            fs::create_dir(&output).unwrap();
            let scope =
                Selection::new(PathBuf::new(), vec!["scope".into(), "outside.txt".into()]).unwrap();
            archive_view::extract(&archive, &output, &scope, password, &cancel).unwrap();
            let root = output.join("scope");
            assert_eq!(fs::read(root.join("change.txt")).unwrap(), b"new");
            assert_eq!(fs::read(root.join("skip.txt")).unwrap(), b"archive version");
            assert_eq!(
                fs::read(root.join("Nested").join("excluded.bin")).unwrap(),
                b"keep me"
            );
            assert_eq!(
                fs::read(root.join("Nested").join("new.txt")).unwrap(),
                b"added"
            );
            assert_eq!(
                fs::read(output.join("outside.txt")).unwrap(),
                b"outside scope"
            );
            assert!(!root.join("remove.txt").exists());
            assert!(!stage.source.catalog.unchanged(&archive));
            assert_eq!(archive_view::test(&archive, None, &cancel).is_err(), split);
        }
    }
}
