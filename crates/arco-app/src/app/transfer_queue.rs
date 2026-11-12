use super::{App, Event, JobContext, JobDescription, Outcome};
use crate::commands::{Dialog, Pending, TransferDestination};
use crate::{FileConflictData, OperationKind, display_path};
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
        let wait_before_move = |_: &Path| {
            while paused.load(Ordering::Relaxed) {
                files::check_cancel(&job.cancel)?;
                std::thread::sleep(Duration::from_millis(40));
            }
            files::check_cancel(&job.cancel)
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
            before_entry: (self.operation == Transfer::Move).then_some(&wait_before_move),
        };
        let mut last = Instant::now() - Duration::from_secs(1);
        let mut previous_bytes = 0;
        job.phase("Dateiumfang wird ermittelt …");
        let usage =
            files::measure_ignoring(&self.paths, &job.cancel, self.ignore.as_deref(), |_| {})?;
        let targets = match &self.destination {
            TransferDestination::Marked(targets) => targets.len(),
            _ => 1,
        };
        let totals_known = usage.errors.is_empty() && usage.links == 0 && self.filter.is_none();
        let total_bytes = totals_known.then(|| usage.bytes.saturating_mul(targets as u64));
        let total_files = totals_known.then(|| usage.files.saturating_mul(targets));
        let mut current = None;
        let mut transferred = 0u64;
        let mut file_index = 0usize;
        let mut progress = |target: &Path, progress: files::Progress<'_>| {
            let key = (target.to_path_buf(), progress.path.to_path_buf());
            if current.as_ref() != Some(&key) {
                current = Some(key);
                previous_bytes = 0;
                file_index += 1;
            }
            let delta = progress.copied.saturating_sub(previous_bytes);
            previous_bytes = progress.copied;
            transferred = transferred.saturating_add(delta);
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
            if last.elapsed() >= Duration::from_millis(100) || progress.copied == progress.total {
                last = Instant::now();
                job.measured_progress(
                    String::new(),
                    super::job_progress::Sample {
                        completed: transferred,
                        total: total_bytes,
                        source: display_path(progress.path),
                        target: display_path(target),
                        items: Some((file_index, total_files)),
                        file_fraction: (progress.total > 0).then(|| {
                            (progress.copied.min(progress.total) as f64 / progress.total as f64)
                                as f32
                        }),
                        ..Default::default()
                    },
                );
            }
        };
        job.phase(if self.operation == Transfer::Move {
            "Dateien werden verschoben …"
        } else {
            "Dateien werden kopiert …"
        });
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
    ftp_draft: Option<PathBuf>,
}

type TransferWork = Box<dyn FnOnce(JobContext) -> io::Result<Outcome> + Send>;

enum QueuedTransfer {
    Local(TransferTask),
    Virtual {
        label: String,
        presentation: JobDescription,
        refresh: bool,
        ftp_draft: Option<PathBuf>,
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
        let deadline = Instant::now() + Duration::from_secs(4);
        let ticket = loop {
            match receiver
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap()
            {
                Event::TransferConflict(ticket) => break ticket,
                Event::Progress { .. } => (),
                _ => panic!("expected progress or a transfer conflict"),
            }
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

    #[test]
    fn native_move_honors_pause_before_renaming_a_selected_entry() {
        let fixture = tempfile::tempdir().unwrap();
        let source = fixture.path().join("data.txt");
        let target = fixture.path().join("target");
        fs::write(&source, b"original contents").unwrap();
        fs::create_dir(&target).unwrap();
        let task = TransferTask {
            paths: vec![source.clone()],
            destination: TransferDestination::Folder(target.clone()),
            operation: Transfer::Move,
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
        let (sender, receiver) = mpsc::channel();
        let job = JobContext {
            cancel: Arc::new(AtomicBool::new(false)),
            sender,
        };
        let worker = std::thread::spawn(move || task.run(&job, &worker_pause, 0));
        let deadline = Instant::now() + Duration::from_secs(4);
        let observed = loop {
            match receiver.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(Event::Progress {
                    sample: Some(sample),
                    ..
                }) if sample.phase == "Dateien werden verschoben …" => break true,
                Ok(_) => (),
                Err(_) => break false,
            }
        };
        std::thread::sleep(Duration::from_millis(100));
        let waiting =
            !worker.is_finished() && source.is_file() && !target.join("data.txt").exists();
        paused.store(false, Ordering::Relaxed);
        let report = worker.join().unwrap().unwrap();
        assert!(
            observed && waiting,
            "native move must wait at the pause boundary"
        );
        assert_eq!(report.completed, 1);
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert!(!source.exists());
        assert_eq!(
            fs::read(target.join("data.txt")).unwrap(),
            b"original contents"
        );
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

impl Queue {
    pub(super) fn paused(&self) -> bool {
        self.current
            .as_ref()
            .and_then(|running| running.local.as_ref())
            .is_some_and(|(_, paused)| paused.load(Ordering::Relaxed))
    }

    pub(super) fn pausable(&self) -> bool {
        self.current
            .as_ref()
            .is_some_and(|running| running.local.is_some())
    }
}

impl App {
    pub(super) fn toggle_transfer_pause(&mut self) {
        if let Some((_, paused)) = self
            .transfers
            .current
            .as_ref()
            .and_then(|running| running.local.as_ref())
        {
            paused.fetch_xor(true, Ordering::Relaxed);
            self.ui().set_job_paused(self.transfers.paused());
            self.publish_transfer_manager();
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
        self.enqueue_virtual_operation(label, refresh, JobDescription::default(), work);
    }

    pub(crate) fn enqueue_virtual_operation(
        &mut self,
        label: &str,
        refresh: bool,
        presentation: JobDescription,
        work: impl FnOnce(JobContext) -> io::Result<Outcome> + Send + 'static,
    ) {
        if let Err(error) = self.enqueue_work(QueuedTransfer::Virtual {
            label: label.into(),
            presentation,
            refresh,
            ftp_draft: None,
            work: Box::new(work),
        }) {
            self.status(error.to_string());
        }
    }

    pub(super) fn enqueue_ftp_upload(
        &mut self,
        label: String,
        path: PathBuf,
        work: impl FnOnce(JobContext) -> io::Result<Outcome> + Send + 'static,
    ) -> io::Result<()> {
        self.enqueue_work(QueuedTransfer::Virtual {
            label,
            presentation: JobDescription::files(
                OperationKind::Copy,
                std::slice::from_ref(&path),
                Path::new(""),
            ),
            refresh: false,
            ftp_draft: Some(path),
            work: Box::new(work),
        })
    }

    pub(super) fn ftp_upload_running(&self, path: &Path) -> bool {
        self.transfers
            .current
            .as_ref()
            .and_then(|job| job.ftp_draft.as_deref())
            == Some(path)
    }

    pub(super) fn ftp_upload_pending(&self, path: &Path) -> bool {
        self.ftp_upload_running(path) || self.transfers.waiting.iter().any(|(_, task)| {
            matches!(task, QueuedTransfer::Virtual { ftp_draft: Some(draft), .. } if draft == path)
        })
    }

    pub(super) fn remove_waiting_ftp_uploads(&mut self, paths: &[PathBuf]) {
        self.transfers.waiting.retain(|(_, task)| {
            !matches!(task, QueuedTransfer::Virtual { ftp_draft: Some(draft), .. } if paths.contains(draft))
        });
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
                presentation,
                refresh,
                ftp_draft,
                work,
            } => {
                self.transfers.current = Some(Running {
                    description: label.clone(),
                    local: None,
                    ftp_draft,
                });
                let description = label.clone();
                self.start_operation_job(&label, refresh, presentation, move |job| {
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
                    if let Some(log) = log {
                        match &mut outcome {
                            Outcome::Report(_, report) => {
                                if let Err(error) = write_log(&log, &description, report) {
                                    report.errors.push(format!("Transferprotokoll · {error}"));
                                }
                            }
                            Outcome::FtpEditUploaded(_, log_error) => {
                                let report = TransferReport {
                                    completed: 1,
                                    ..Default::default()
                                };
                                if let Err(error) = write_log(&log, &description, &report) {
                                    *log_error = Some(error.to_string());
                                }
                            }
                            _ => (),
                        }
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
            ftp_draft: None,
        });
        let rate = self.profile.transfer_rate_kib;
        let kind = if task.operation == Transfer::Move {
            OperationKind::Move
        } else {
            OperationKind::Copy
        };
        let target = match &task.destination {
            TransferDestination::Folder(path) => display_path(path),
            TransferDestination::Marked(paths) => paths
                .iter()
                .map(|path| display_path(path))
                .collect::<Vec<_>>()
                .join(" · "),
        };
        let mut presentation = JobDescription::files(kind, &task.paths, Path::new(""));
        presentation.target = target;
        self.start_operation_job(task.label(), true, presentation, move |job| {
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
        self.publish_transfer_manager();
        self.dialog(
            "transfer-manager",
            Pending::TransferManager,
            Dialog::default(),
        );
    }

    pub(super) fn publish_transfer_manager(&self) {
        let running = self.transfers.current.as_ref();
        let local = running.and_then(|running| running.local.as_ref());
        let waiting = self
            .transfers
            .waiting
            .iter()
            .map(|(_, task)| task.describe());
        self.ui()
            .set_transfer_manager_data(crate::TransferManagerData {
                active: running
                    .map(|running| running.description.as_str())
                    .unwrap_or_default()
                    .into(),
                pausable: local.is_some(),
                paused: local.is_some_and(|(_, paused)| paused.load(Ordering::Relaxed)),
                waiting: super::strings(waiting),
                retry: self.transfers.retry.is_some(),
                descriptions: self.profile.transfer_descriptions,
            });
    }

    pub(crate) fn transfer_manager_action(&mut self, action: usize) -> io::Result<()> {
        match action {
            0 => {
                if !self.transfers.pausable() {
                    return Err(io::Error::other("Der aktuelle Auftrag bietet keine Pause."));
                }
                self.toggle_transfer_pause();
                self.status(if self.transfers.paused() {
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
                self.open_transfer_manager();
            }
            3 => {
                let task = self.transfers.retry.clone().ok_or_else(|| {
                    io::Error::other("Keine fehlgeschlagene lokale Kopie vorhanden.")
                })?;
                self.enqueue_transfer(task)?;
            }
            4 => self.open_settings(super::SettingsPage::Transfers),
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
            6.. => {
                if let Some((id, _)) = self.transfers.waiting.get(action - 6) {
                    self.open_queued_transfer(*id);
                }
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
