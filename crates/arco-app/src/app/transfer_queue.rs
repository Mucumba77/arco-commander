use super::{App, Event, JobContext, Outcome};
use crate::commands::{Dialog, Pending, TransferDestination};
use crate::{FileConflictData, NtfsDraft, display_path, human_size};
use arco_core::advanced_search::Query;
use arco_core::files::{self, Conflict, ConflictDecision, FileConflict, Transfer, TransferReport};
use arco_core::ignore::IgnoreList;
use arco_core::tools::{self, PreviewMode};
use chrono::{DateTime, Local};
use std::cell::Cell;
use std::collections::VecDeque;
use std::fmt::Write as _;
use std::fs::Metadata;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

#[derive(Clone)]
pub(crate) struct TransferTask {
    pub paths: Vec<PathBuf>,
    pub destination: TransferDestination,
    pub operation: Transfer,
    pub mask: String,
    pub conflict: Conflict,
    pub ask_conflicts: bool,
    pub ntfs: files::NtfsCopyOptions,
    pub descriptions: bool,
    pub filter: Option<Arc<Query>>,
    pub ignore: Option<Arc<IgnoreList>>,
}

impl TransferTask {
    fn label(&self) -> &'static str {
        match self.operation {
            Transfer::Copy => "Kopieren",
            Transfer::Move => "Verschieben",
        }
    }

    fn describe(&self) -> String {
        let target = match &self.destination {
            TransferDestination::Folder(path) => display_path(path),
            TransferDestination::Marked(paths) => format!("{} Zielordner", paths.len()),
        };
        format!(
            "{} · {} Einträge → {target}",
            self.label(),
            self.paths.len()
        )
    }

    fn run(
        &self,
        job: &JobContext,
        paused: &AtomicBool,
        rate_kib: u32,
    ) -> io::Result<TransferReport> {
        let remaining = Cell::new(None);
        let resolve = |conflict: FileConflict<'_>| {
            let regular_target =
                conflict.target_metadata.is_file() && !files::is_link(conflict.target_metadata);
            if let Some(rule) = remaining.get()
                && (regular_target || matches!(rule, Conflict::Skip | Conflict::Rename))
            {
                return Ok(rule.into());
            }
            let (decision, for_all) = request_conflict(job, conflict, self.operation, None)?;
            if for_all && let ConflictDecision::Apply(rule) = decision {
                remaining.set(Some(rule));
            }
            Ok(decision)
        };
        let policy = files::TransferPolicy {
            conflict: self.conflict,
            resolve_conflict: self
                .ask_conflicts
                .then_some(&resolve as &files::ResolveConflict<'_>),
            ntfs: self.ntfs,
            descriptions: (self.descriptions
                && !self
                    .paths
                    .iter()
                    .any(|path| arco_core::descriptions::is_sidecar(path)))
            .then_some(super::metadata_actions::apply_description_attributes),
            ignore: self.ignore.as_deref(),
            filter: self.filter.as_deref(),
        };
        let mut last = Instant::now() - Duration::from_secs(1);
        let mut previous_bytes = 0;
        let label = self.label();
        let mut progress = |target: &Path, progress: files::Progress<'_>| {
            let delta = progress.copied.saturating_sub(previous_bytes);
            previous_bytes = progress.copied;
            let mut delay = if rate_kib == 0 {
                Duration::ZERO
            } else {
                Duration::from_secs_f64(delta as f64 / (f64::from(rate_kib) * 1024.0))
            };
            while paused.load(Ordering::Relaxed) || !delay.is_zero() {
                if job.cancel.load(Ordering::Relaxed) {
                    return;
                }
                if paused.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(40));
                } else {
                    let step = delay.min(Duration::from_millis(40));
                    std::thread::sleep(step);
                    delay -= step;
                }
            }
            if last.elapsed() >= Duration::from_millis(100) {
                last = Instant::now();
                job.progress(format!(
                    "{label} · {} → {} · {} / {}",
                    display_path(progress.path),
                    display_path(target),
                    human_size(progress.copied),
                    human_size(progress.total)
                ));
            }
        };
        match &self.destination {
            TransferDestination::Folder(target) if !self.mask.is_empty() => files::transfer_masked(
                &self.paths,
                target,
                &self.mask,
                self.operation,
                policy,
                &job.cancel,
                Some(arco_windows::rename_noreplace),
                |event| progress(target, event),
            ),
            TransferDestination::Folder(target) => files::transfer(
                &self.paths,
                target,
                self.operation,
                policy,
                &job.cancel,
                Some(arco_windows::rename_noreplace),
                |event| progress(target, event),
            ),
            TransferDestination::Marked(targets) => {
                files::copy_to_directories(&self.paths, targets, policy, &job.cancel, progress)
            }
        }
    }
}

struct Running {
    description: String,
    local: Option<(TransferTask, Arc<AtomicBool>)>,
}

type TransferWork = Box<dyn FnOnce(JobContext) -> io::Result<Outcome> + Send>;

enum QueuedTransfer {
    Local(TransferTask),
    Virtual {
        label: String,
        refresh: bool,
        work: TransferWork,
    },
}

impl QueuedTransfer {
    fn describe(&self) -> String {
        match self {
            Self::Local(task) => task.describe(),
            Self::Virtual { label, .. } => label.clone(),
        }
    }
}

pub(crate) struct ConflictTicket {
    source: String,
    target: String,
    source_info: String,
    target_info: String,
    source_preview: String,
    target_preview: String,
    actions: Vec<(Conflict, &'static str)>,
    reply: mpsc::Sender<(ConflictDecision, bool)>,
    cancel: Arc<AtomicBool>,
}

impl ConflictTicket {
    #[cfg(test)]
    pub(super) fn previews(&self) -> [&str; 2] {
        [&self.source_preview, &self.target_preview]
    }

    pub(crate) fn answer(self, option: usize, for_all: bool, name: &str) {
        if option == self.actions.len() {
            let _ = self
                .reply
                .send((ConflictDecision::RenameTo(name.into()), false));
        } else if let Some(&(rule, _)) = self.actions.get(option) {
            let _ = self.reply.send((rule.into(), for_all));
        }
    }

    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
}

pub(super) fn request_conflict(
    job: &JobContext,
    conflict: FileConflict<'_>,
    operation: Transfer,
    source_label: Option<String>,
) -> io::Result<(ConflictDecision, bool)> {
    let mut actions = vec![
        (Conflict::Skip, "Überspringen"),
        (Conflict::Rename, "Unter freiem Namen speichern"),
    ];
    if conflict.source_metadata.is_file()
        && conflict.target_metadata.is_file()
        && !files::is_link(conflict.target_metadata)
    {
        actions.extend([
            (Conflict::Replace, "Überschreiben"),
            (Conflict::Newer, "Nur mit neuerer Datei überschreiben"),
            (Conflict::Larger, "Nur mit größerer Datei überschreiben"),
        ]);
        if operation == Transfer::Copy {
            actions.push((Conflict::Append, "An Zieldatei anhängen"));
        }
    }
    ask_conflict(
        job,
        ConflictSide::local(conflict.source, conflict.source_metadata, source_label),
        ConflictSide::local(conflict.target, conflict.target_metadata, None),
        actions,
    )
}

pub(super) struct ConflictSide {
    pub path: String,
    pub info: String,
    pub preview: String,
}

impl ConflictSide {
    pub(super) fn local(path: &Path, metadata: &Metadata, label: Option<String>) -> Self {
        Self {
            path: label.unwrap_or_else(|| display_path(path)),
            info: conflict_metadata(metadata),
            preview: conflict_preview(path, metadata),
        }
    }

    pub(super) fn virtual_entry(
        path: String,
        size: Option<u64>,
        modified: Option<std::time::SystemTime>,
    ) -> Self {
        let kind = size.map_or_else(|| "Ordner".into(), |size| format!("{size} Bytes"));
        let date = modified.map_or_else(
            || "Änderungszeit unbekannt".into(),
            |date| {
                DateTime::<Local>::from(date)
                    .format("%d.%m.%Y %H:%M:%S")
                    .to_string()
            },
        );
        Self {
            path,
            info: format!("{kind} · {date}"),
            preview: "Für diesen Eintrag ist keine Dateivorschau verfügbar.".into(),
        }
    }
}

pub(super) const VIRTUAL_CONFLICT_PREVIEW_LIMIT: u64 = 64 * 1024;

pub(super) fn ask_conflict(
    job: &JobContext,
    source: ConflictSide,
    target: ConflictSide,
    actions: Vec<(Conflict, &'static str)>,
) -> io::Result<(ConflictDecision, bool)> {
    files::check_cancel(&job.cancel)?;
    let (reply, response) = mpsc::channel();
    let ticket = ConflictTicket {
        source: source.path,
        target: target.path,
        source_info: source.info,
        target_info: target.info,
        source_preview: source.preview,
        target_preview: target.preview,
        actions,
        reply,
        cancel: job.cancel.clone(),
    };
    job.sender
        .send(Event::TransferConflict(ticket))
        .map_err(|_| io::ErrorKind::Interrupted)?;
    loop {
        files::check_cancel(&job.cancel)?;
        match response.recv_timeout(Duration::from_millis(50)) {
            Ok(answer) => return Ok(answer),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(io::ErrorKind::Interrupted.into());
            }
            Err(mpsc::RecvTimeoutError::Timeout) => (),
        }
    }
}

fn conflict_metadata(metadata: &Metadata) -> String {
    let kind = if files::is_link(metadata) {
        "Verknüpfung".into()
    } else if metadata.is_dir() {
        "Ordner".into()
    } else {
        format!("{} Bytes", metadata.len())
    };
    let date = metadata.modified().map_or_else(
        |_| "Änderungszeit unbekannt".into(),
        |date| {
            DateTime::<Local>::from(date)
                .format("%d.%m.%Y %H:%M:%S")
                .to_string()
        },
    );
    let protected = if metadata.permissions().readonly() {
        " · Schreibgeschützt"
    } else {
        ""
    };
    format!("{kind} · {date}{protected}")
}

pub(super) fn conflict_preview(path: &Path, metadata: &Metadata) -> String {
    if !metadata.is_file() || files::is_link(metadata) {
        return "Für diesen Eintrag ist keine Dateivorschau verfügbar.".into();
    }
    let read = || {
        files::ensure_unchanged(path, metadata)?;
        let mut page = tools::preview(path, 0, PreviewMode::Text)?;
        if page.text.chars().take(4096).any(|character| {
            character.is_ascii_control() && !matches!(character, '\r' | '\n' | '\t' | '\x0c')
        }) {
            page = tools::preview(path, 0, PreviewMode::Hex)?;
        }
        files::ensure_unchanged(path, metadata)?;
        if page.length == 0 {
            return Ok::<_, io::Error>("Leere Datei".into());
        }
        let mut text: String = page.text.chars().take(8192).collect();
        if text.len() < page.text.len() || page.next < page.length {
            text.push_str("\n…");
        }
        Ok(text)
    };
    read().unwrap_or_else(|error| format!("Vorschau nicht verfügbar · {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::mpsc;

    #[test]
    fn conflict_answer_can_apply_to_the_remaining_files() {
        let fixture = tempfile::tempdir().unwrap();
        let source = fixture.path().join("source");
        let target = fixture.path().join("target");
        fs::create_dir(&source).unwrap();
        fs::create_dir(&target).unwrap();
        let paths: Vec<_> = ["a.txt", "b.txt"]
            .into_iter()
            .map(|name| {
                let path = source.join(name);
                fs::write(&path, "new contents").unwrap();
                fs::write(target.join(name), "old contents").unwrap();
                path
            })
            .collect();
        let task = TransferTask {
            paths,
            destination: TransferDestination::Folder(target.clone()),
            operation: Transfer::Copy,
            mask: String::new(),
            conflict: Conflict::Fail,
            ask_conflicts: true,
            ntfs: Default::default(),
            descriptions: false,
            filter: None,
            ignore: None,
        };
        let (sender, receiver) = mpsc::channel();
        let job = JobContext {
            cancel: Arc::new(AtomicBool::new(false)),
            sender,
        };
        let worker = std::thread::spawn(move || task.run(&job, &AtomicBool::new(false), 0));
        let Event::TransferConflict(ticket) =
            receiver.recv_timeout(Duration::from_secs(4)).unwrap()
        else {
            panic!("expected a transfer conflict");
        };
        assert_eq!(ticket.source_preview, "new contents");
        assert_eq!(ticket.target_preview, "old contents");
        let replace = ticket
            .actions
            .iter()
            .position(|(rule, _)| *rule == Conflict::Replace)
            .unwrap();
        ticket.answer(replace, true, "");
        for event in receiver {
            assert!(
                !matches!(event, Event::TransferConflict(_)),
                "unexpected second prompt"
            );
        }
        let report = worker.join().unwrap().unwrap();
        assert_eq!(report.completed, 2);
        assert_eq!(
            fs::read_to_string(target.join("b.txt")).unwrap(),
            "new contents"
        );
    }

    #[test]
    fn paused_copy_publishes_only_after_resuming() {
        let fixture = tempfile::tempdir().unwrap();
        let source = fixture.path().join("data.bin");
        let target = fixture.path().join("target");
        let data = vec![0x42; 512 * 1024];
        fs::write(&source, &data).unwrap();
        fs::create_dir(&target).unwrap();
        let task = TransferTask {
            paths: vec![source],
            destination: TransferDestination::Folder(target.clone()),
            operation: Transfer::Copy,
            mask: String::new(),
            conflict: Conflict::Fail,
            ask_conflicts: false,
            ntfs: Default::default(),
            descriptions: false,
            filter: None,
            ignore: None,
        };
        let paused = Arc::new(AtomicBool::new(true));
        let worker_pause = paused.clone();
        let (sender, _receiver) = mpsc::channel();
        let job = JobContext {
            cancel: Arc::new(AtomicBool::new(false)),
            sender,
        };
        let worker = std::thread::spawn(move || task.run(&job, &worker_pause, 0));
        let start = Instant::now();
        while fs::read_dir(&target).unwrap().next().is_none()
            && !worker.is_finished()
            && start.elapsed() < Duration::from_secs(3)
        {
            std::thread::sleep(Duration::from_millis(10));
        }
        let waiting_at_stage = !worker.is_finished()
            && fs::read_dir(&target).unwrap().next().is_some()
            && !target.join("data.bin").exists();
        paused.store(false, Ordering::Relaxed);
        let report = worker.join().unwrap().unwrap();
        assert!(waiting_at_stage);
        assert!(report.errors.is_empty());
        assert_eq!(report.completed, 1);
        assert_eq!(fs::read(target.join("data.bin")).unwrap(), data);
    }
}

#[derive(Default)]
pub(super) struct Queue {
    waiting: VecDeque<(u64, QueuedTransfer)>,
    current: Option<Running>,
    retry: Option<TransferTask>,
    pub(super) prompt: Option<ConflictTicket>,
    next_id: u64,
}

impl App {
    pub(crate) fn open_ntfs_options(&mut self) {
        let options = self.profile.transfer_ntfs;
        self.ui().set_ntfs_draft(NtfsDraft {
            streams: options.streams,
            permissions: options.permissions,
            hardlinks: options.hardlinks,
            sparse: options.sparse,
        });
        self.dialog(
            "ntfs-options",
            Pending::NtfsOptions,
            Dialog {
                title: "NTFS-Kopieroptionen",
                ..Default::default()
            },
        );
    }

    pub(super) fn apply_ntfs_options(&mut self, options: NtfsDraft) {
        if !matches!(&self.pending, Some(Pending::NtfsOptions)) {
            return;
        }
        if !self.save_profile {
            self.status("Das Arco-Profil ist nicht verfügbar.");
            return;
        }
        let mut draft = self.profile_snapshot();
        draft.transfer_ntfs = files::NtfsCopyOptions {
            streams: options.streams,
            permissions: options.permissions,
            hardlinks: options.hardlinks,
            sparse: options.sparse,
        };
        match draft.save(&self.profile_path) {
            Ok(()) => {
                self.profile = draft;
                self.close_dialog();
                self.status("NTFS-Kopieroptionen gespeichert.");
            }
            Err(error) => self.status(error.to_string()),
        }
    }

    pub(super) fn open_next_transfer_conflict(&mut self) {
        if matches!(&self.pending, Some(Pending::TransferConflict(ticket)) if ticket.cancelled()) {
            self.close_dialog();
        }
        let ui = self.ui();
        if !ui.get_dialog_kind().is_empty()
            || ui.get_settings_open()
            || ui.get_drive_side() >= 0
            || self.context_menu_open
        {
            return;
        }
        let Some(ticket) = self
            .transfers
            .prompt
            .take()
            .filter(|ticket| !ticket.cancelled())
        else {
            return;
        };
        ui.set_file_conflict(FileConflictData {
            manual_index: ticket.actions.len() as i32,
            source: ticket.source.as_str().into(),
            target: ticket.target.as_str().into(),
            source_info: ticket.source_info.as_str().into(),
            target_info: ticket.target_info.as_str().into(),
            source_preview: ticket.source_preview.as_str().into(),
            target_preview: ticket.target_preview.as_str().into(),
        });
        let mut options: Vec<_> = ticket.actions.iter().map(|(_, label)| *label).collect();
        options.push("Eigenen Zielnamen eingeben");
        self.dialog(
            "transfer-conflict",
            Pending::TransferConflict(ticket),
            Dialog {
                title: "Dateikonflikt",
                options: &options,
                ..Default::default()
            },
        );
    }

    pub(crate) fn enqueue_transfer(&mut self, task: TransferTask) -> io::Result<()> {
        self.enqueue_work(QueuedTransfer::Local(task))
    }

    pub(crate) fn enqueue_virtual_transfer(
        &mut self,
        label: &str,
        refresh: bool,
        work: impl FnOnce(JobContext) -> io::Result<Outcome> + Send + 'static,
    ) {
        if let Err(error) = self.enqueue_work(QueuedTransfer::Virtual {
            label: label.into(),
            refresh,
            work: Box::new(work),
        }) {
            self.status(error.to_string());
        }
    }

    fn enqueue_work(&mut self, task: QueuedTransfer) -> io::Result<()> {
        if self.transfers.waiting.len() >= 256 {
            return Err(io::Error::other(
                "Die Warteschlange enthält bereits 256 Aufträge.",
            ));
        }
        self.transfers.next_id += 1;
        self.transfers
            .waiting
            .push_back((self.transfers.next_id, task));
        self.advance_transfers();
        if !self.transfers.waiting.is_empty() {
            self.status(format!(
                "Transfer vorgemerkt · {} warten.",
                self.transfers.waiting.len()
            ));
        }
        Ok(())
    }

    pub(super) fn advance_transfers(&mut self) {
        let ui = self.ui();
        if self.job.is_some()
            || !ui.get_dialog_kind().is_empty()
            || ui.get_settings_open()
            || ui.get_drive_side() >= 0
            || self.context_menu_open
        {
            return;
        }
        let Some((_, task)) = self.transfers.waiting.pop_front() else {
            return;
        };
        let log = self.profile.transfer_log.clone();
        let task = match task {
            QueuedTransfer::Virtual {
                label,
                refresh,
                work,
            } => {
                self.transfers.current = Some(Running {
                    description: label.clone(),
                    local: None,
                });
                let description = label.clone();
                self.start_job(&label, refresh, move |job| {
                    let mut outcome = match work(job) {
                        Ok(outcome) => outcome,
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                            Outcome::Report(
                                description.clone(),
                                TransferReport {
                                    cancelled: true,
                                    ..Default::default()
                                },
                            )
                        }
                        Err(error) => Outcome::Report(
                            description.clone(),
                            TransferReport {
                                errors: vec![error.to_string()],
                                ..Default::default()
                            },
                        ),
                    };
                    if let (Some(log), Outcome::Report(_, report)) = (log, &mut outcome)
                        && let Err(error) = write_log(&log, &description, report)
                    {
                        report.errors.push(format!("Transferprotokoll · {error}"));
                    }
                    Ok(outcome)
                });
                return;
            }
            QueuedTransfer::Local(task) => task,
        };
        let paused = Arc::new(AtomicBool::new(false));
        self.transfers.current = Some(Running {
            description: task.describe(),
            local: Some((task.clone(), paused.clone())),
        });
        let rate = self.profile.transfer_rate_kib;
        self.start_job(task.label(), true, move |job| {
            let mut report = match task.run(&job, &paused, rate) {
                Ok(report) => report,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => TransferReport {
                    cancelled: true,
                    ..Default::default()
                },
                Err(error) => TransferReport {
                    errors: vec![error.to_string()],
                    ..Default::default()
                },
            };
            if let Some(log) = log
                && let Err(error) = write_log(&log, &task.describe(), &report)
            {
                report.errors.push(format!("Transferprotokoll · {error}"));
            }
            Ok(Outcome::TransferFinished(report))
        });
    }

    pub(super) fn finish_virtual_transfer(&mut self) {
        if self
            .transfers
            .current
            .as_ref()
            .is_some_and(|running| running.local.is_none())
        {
            self.transfers.current = None;
        }
    }

    pub(super) fn complete_transfer(&mut self, report: TransferReport) {
        let Some((task, _)) = self
            .transfers
            .current
            .take()
            .and_then(|running| running.local)
        else {
            return;
        };
        let label = task.label();
        if (report.cancelled || !report.errors.is_empty()) && task.operation == Transfer::Copy {
            self.transfers.retry = Some(TransferTask {
                conflict: Conflict::Skip,
                ask_conflicts: false,
                ..task
            });
        }
        self.completed(Outcome::Report(label.into(), report));
    }

    pub(crate) fn open_transfer_manager(&mut self) {
        let mut body = String::new();
        if let Some(running) = &self.transfers.current {
            writeln!(
                body,
                "{} · {}",
                if running
                    .local
                    .as_ref()
                    .is_some_and(|(_, paused)| paused.load(Ordering::Relaxed))
                {
                    "Pausiert"
                } else {
                    "Aktiv"
                },
                running.description
            )
            .unwrap();
        }
        for (index, (_, task)) in self.transfers.waiting.iter().enumerate() {
            writeln!(body, "{}. {}", index + 1, task.describe()).unwrap();
        }
        if body.is_empty() {
            body.push_str("Keine Transfers vorgemerkt.");
        }
        self.dialog(
            "review",
            Pending::TransferManager,
            Dialog {
                title: "Transfermanager",
                value: &body,
                options: &[
                    "Pause oder Fortsetzen",
                    "Aktiven Transfer abbrechen",
                    "Wartende Aufträge bearbeiten",
                    "Letzte fehlgeschlagene Kopie erneut versuchen",
                    "Geschwindigkeit und Protokoll",
                    if self.profile.transfer_descriptions {
                        "Dateikommentare mitführen · an"
                    } else {
                        "Dateikommentare mitführen · aus"
                    },
                ],
                ..Default::default()
            },
        );
    }

    pub(crate) fn transfer_manager_action(&mut self, action: usize) -> io::Result<()> {
        match action {
            0 => {
                let (_, paused) = self
                    .transfers
                    .current
                    .as_ref()
                    .and_then(|running| running.local.as_ref())
                    .ok_or_else(|| io::Error::other("Der aktuelle Auftrag bietet keine Pause."))?;
                let paused = !paused.fetch_xor(true, Ordering::Relaxed);
                self.status(if paused {
                    "Pause am nächsten Kopierblock."
                } else {
                    "Transfer fortgesetzt."
                });
            }
            1 => {
                if self.transfers.current.is_none() {
                    return Err(io::Error::other("Kein Transfer aktiv."));
                }
                self.cancel();
            }
            2 => {
                let ids: Vec<_> = self.transfers.waiting.iter().map(|(id, _)| *id).collect();
                let names: Vec<_> = self
                    .transfers
                    .waiting
                    .iter()
                    .map(|(_, task)| task.describe())
                    .collect();
                if ids.is_empty() {
                    return Err(io::Error::other("Keine Transfers vorgemerkt."));
                }
                let options: Vec<_> = names.iter().map(String::as_str).collect();
                self.dialog(
                    "confirm",
                    Pending::TransferQueueChoose(ids),
                    Dialog {
                        title: "Wartende Aufträge",
                        options: &options,
                        ..Default::default()
                    },
                );
            }
            3 => {
                let task = self.transfers.retry.clone().ok_or_else(|| {
                    io::Error::other("Keine fehlgeschlagene lokale Kopie vorhanden.")
                })?;
                self.enqueue_transfer(task)?;
            }
            4 => {
                let log = self
                    .profile
                    .transfer_log
                    .as_deref()
                    .map(display_path)
                    .unwrap_or_default();
                let rate = self.profile.transfer_rate_kib.to_string();
                self.dialog("input", Pending::TransferSettings, Dialog {
                    title: "Transfereinstellungen", description: "0 bedeutet unbegrenzt. Ein leerer Protokollpfad deaktiviert das Protokoll.",
                    value: &rate, second: &log, placeholder: "KiB/s für neue Aufträge",
                    second_placeholder: "Absoluter Protokollpfad", ..Default::default()
                });
            }
            5 => {
                if !self.save_profile {
                    return Err(io::Error::other("Das Arco-Profil ist nicht verfügbar."));
                }
                let mut draft = self.profile_snapshot();
                draft.transfer_descriptions = !draft.transfer_descriptions;
                draft.save(&self.profile_path)?;
                self.profile = draft;
                self.open_transfer_manager();
            }
            _ => {}
        }
        Ok(())
    }

    pub(crate) fn open_queued_transfer(&mut self, id: u64) {
        if let Some((_, task)) = self.transfers.waiting.iter().find(|(key, _)| *key == id) {
            let description = task.describe();
            self.dialog(
                "confirm",
                Pending::TransferQueueOrder(id),
                Dialog {
                    title: "Wartenden Auftrag bearbeiten",
                    description: &description,
                    options: &["Weiter nach vorn", "Weiter nach hinten", "Entfernen"],
                    ..Default::default()
                },
            );
        } else {
            self.status("Der Auftrag wartet nicht mehr.");
        }
    }

    pub(crate) fn reorder_transfer(&mut self, id: u64, operation: usize) {
        let waiting = &mut self.transfers.waiting;
        if let Some(index) = waiting.iter().position(|(key, _)| *key == id) {
            match operation {
                0 if index > 0 => waiting.swap(index, index - 1),
                1 if index + 1 < waiting.len() => waiting.swap(index, index + 1),
                2 => {
                    waiting.remove(index);
                }
                _ => {}
            }
        }
        self.open_transfer_manager();
    }

    pub(crate) fn save_transfer_settings(&mut self, rate: &str, log: &str) -> io::Result<()> {
        if !self.save_profile {
            return Err(io::Error::other("Das Arco-Profil ist nicht verfügbar."));
        }
        let mut draft = self.profile_snapshot();
        draft.transfer_rate_kib = rate
            .trim()
            .parse()
            .map_err(|_| io::Error::other("Gib eine ganze Zahl für KiB/s ein."))?;
        let log = log.trim().trim_matches('"');
        draft.transfer_log = (!log.is_empty()).then(|| PathBuf::from(log));
        if draft
            .transfer_log
            .as_ref()
            .is_some_and(|path| !path.is_absolute() || path.file_name().is_none())
        {
            return Err(io::Error::other(
                "Gib einen absoluten Pfad zur Protokolldatei ein.",
            ));
        }
        draft.save(&self.profile_path)?;
        self.profile = draft;
        self.status("Transfereinstellungen gespeichert.");
        Ok(())
    }
}

fn write_log(path: &Path, description: &str, report: &TransferReport) -> io::Result<()> {
    let mut body = format!(
        "{}\t{}\t{} abgeschlossen\t{} übersprungen{}\n",
        chrono::Local::now().to_rfc3339(),
        description,
        report.completed,
        report.skipped,
        if report.cancelled {
            "\tabgebrochen"
        } else {
            ""
        }
    );
    for error in &report.errors {
        writeln!(body, "  {error}").unwrap();
    }
    let mut log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    log.write_all(body.as_bytes())
}
