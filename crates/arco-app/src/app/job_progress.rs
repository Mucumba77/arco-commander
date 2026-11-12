use crate::{JobMetrics, OperationKind, human_size, locale::tr};
use std::collections::VecDeque;
use std::time::{Duration, Instant};

#[derive(Default)]
pub(super) struct Sample {
    pub phase: String,
    pub completed: u64,
    pub total: Option<u64>,
    pub source: String,
    pub target: String,
    pub items: Option<(usize, Option<usize>)>,
    pub processed_items: Option<usize>,
    pub scan: Option<(usize, Option<usize>, usize)>,
    pub recovering: bool,
    pub file_fraction: Option<f32>,
}

#[derive(Clone, Default)]
pub(crate) struct Description {
    pub kind: OperationKind,
    pub origin: String,
    pub target: String,
    pub detail: String,
}

impl Description {
    pub fn new(kind: OperationKind, origin: String, target: String) -> Self {
        Self {
            kind,
            origin,
            target,
            detail: String::new(),
        }
    }

    pub fn files(
        kind: OperationKind,
        paths: &[std::path::PathBuf],
        target: &std::path::Path,
    ) -> Self {
        let origin = if let [path] = paths {
            crate::display_path(path)
        } else {
            format!("{} · {}", tr("Ausgewählte Einträge"), paths.len())
        };
        Self::new(kind, origin, crate::display_path(target))
    }
}

pub(super) struct Tracker {
    started: Instant,
    rendered: Instant,
    sample: Sample,
    measured: bool,
    history: VecDeque<(Instant, u64)>,
    paused: bool,
}

impl Default for Tracker {
    fn default() -> Self {
        let now = Instant::now();
        Self {
            started: now,
            rendered: now - Duration::from_secs(1),
            sample: Sample::default(),
            measured: false,
            history: VecDeque::new(),
            paused: false,
        }
    }
}

impl Tracker {
    pub fn update(&mut self, sample: Sample) {
        self.measured = true;
        let now = Instant::now();
        if sample.completed < self.sample.completed {
            self.history.clear();
        }
        if self.history.is_empty() {
            self.history.push_back((now, self.sample.completed));
        }
        self.history.push_back((now, sample.completed));
        while self.history.len() > 2 && self.history[1].0 + Duration::from_secs(4) < now {
            self.history.pop_front();
        }
        self.sample = sample;
    }

    pub fn unmeasured(&mut self) {
        self.sample = Sample::default();
        self.measured = false;
        self.history.clear();
    }

    pub fn render(
        &mut self,
        paused: bool,
        waiting: bool,
        kind: OperationKind,
    ) -> Option<JobMetrics> {
        let now = Instant::now();
        if now.duration_since(self.rendered) < Duration::from_millis(200) {
            return None;
        }
        self.rendered = now;
        if paused != self.paused || waiting {
            self.history.clear();
            self.paused = paused;
        }
        let speed = self.history.front().and_then(|&(time, bytes)| {
            let elapsed = now.duration_since(time).as_secs_f64();
            (elapsed >= 0.4 && !paused && !waiting)
                .then(|| self.sample.completed.saturating_sub(bytes) as f64 / elapsed)
                .filter(|speed| *speed >= 1.0)
        });
        let remaining = self.sample.total.and_then(|total| {
            speed.map(|speed| {
                Duration::from_secs_f64(total.saturating_sub(self.sample.completed) as f64 / speed)
            })
        });
        let items = self
            .sample
            .items
            .map_or_else(String::new, |(index, total)| {
                total.map_or_else(
                    || format!("{} {index}", tr("Eintrag")),
                    |total| format!("{} {index} / {total}", tr("Eintrag")),
                )
            });
        Some(JobMetrics {
            source: self.sample.source.as_str().into(),
            target: self.sample.target.as_str().into(),
            bytes: if !self.measured || (self.sample.completed == 0 && self.sample.total.is_none())
            {
                String::new()
            } else {
                self.sample.total.map_or_else(
                    || human_size(self.sample.completed),
                    |total| {
                        format!(
                            "{} / {}",
                            human_size(self.sample.completed),
                            human_size(total)
                        )
                    },
                )
            }
            .into(),
            speed: speed
                .map_or_else(
                    || "—".into(),
                    |speed| format!("{}/s", human_size(speed as u64)),
                )
                .into(),
            elapsed: duration(now.duration_since(self.started)).into(),
            remaining: remaining.map_or_else(|| "—".into(), duration).into(),
            finish: remaining
                .and_then(|remaining| {
                    let span = chrono::Duration::from_std(remaining).ok()?;
                    let end = chrono::Local::now().checked_add_signed(span)?;
                    Some(
                        end.format(if remaining.as_secs() >= 86_400 {
                            "%d.%m. %H:%M:%S"
                        } else {
                            "%H:%M:%S"
                        })
                        .to_string(),
                    )
                })
                .unwrap_or_default()
                .into(),
            items: items.into(),
            processed: self
                .sample
                .scan
                .map(|(examined, _, _)| examined)
                .or(self.sample.processed_items)
                .map(|count| count.to_string())
                .unwrap_or_default()
                .into(),
            pending: self
                .sample
                .processed_items
                .and_then(|processed| {
                    self.sample
                        .items
                        .and_then(|(_, total)| total)
                        .map(|total| total.saturating_sub(processed).to_string())
                })
                .unwrap_or_default()
                .into(),
            matched: self
                .sample
                .scan
                .and_then(|(_, matched, _)| matched)
                .map(|count| count.to_string())
                .unwrap_or_default()
                .into(),
            errors: self
                .sample
                .scan
                .map(|(_, _, errors)| errors.to_string())
                .unwrap_or_default()
                .into(),
            recovering: self.sample.recovering,
            phase: tr(if waiting {
                "Warte auf Entscheidung"
            } else if paused {
                if kind == OperationKind::Move {
                    "Pause am nächsten Eintrag"
                } else {
                    "Pausiert"
                }
            } else if !self.sample.phase.is_empty() {
                &self.sample.phase
            } else {
                match kind {
                    OperationKind::Copy => "Dateien werden kopiert …",
                    OperationKind::Move => "Dateien werden verschoben …",
                    OperationKind::Pack => "Archiv wird erstellt …",
                    OperationKind::Extract => "Archiv wird entpackt …",
                    OperationKind::Delete => "Auswahl wird verarbeitet …",
                    OperationKind::Search => "Suchorte durchsuchen …",
                    OperationKind::Rename => "Dateien werden umbenannt …",
                    OperationKind::Sync => "Verzeichnisse werden synchronisiert …",
                    OperationKind::Ftp => "FTP-Auftrag läuft …",
                    OperationKind::Plugin => "Plugin-Auftrag läuft …",
                    OperationKind::Task => "Auftrag läuft",
                }
            })
            .into(),
            file_fraction: self.sample.file_fraction.unwrap_or(-1.0),
        })
    }
}

fn duration(value: Duration) -> String {
    let seconds = value.as_secs();
    if seconds >= 3600 {
        format!(
            "{}:{:02}:{:02}",
            seconds / 3600,
            seconds / 60 % 60,
            seconds % 60
        )
    } else {
        format!("{}:{:02}", seconds / 60, seconds % 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ftp_validation_clears_transfer_totals_and_changed_sizes_stay_indeterminate() {
        use crate::app::{Event, JobContext};
        use arco_windows::ftp::{DownloadPhase, DownloadProgress};
        use std::sync::{Arc, atomic::AtomicBool, mpsc};
        let (sender, receiver) = mpsc::channel();
        let job = JobContext {
            sender,
            cancel: Arc::new(AtomicBool::new(false)),
        };
        let mut progress = job.ftp_download_progress("ftp://example.test/file".into(), Some(1024));
        progress(DownloadProgress {
            received: 1024,
            total: Some(1024),
            phase: DownloadPhase::Receiving,
        });
        progress(DownloadProgress {
            received: 1024,
            total: Some(1024),
            phase: DownloadPhase::Verifying,
        });
        let Event::Progress {
            fraction,
            sample: Some(sample),
            ..
        } = receiver.recv().unwrap()
        else {
            panic!("expected a measured download")
        };
        assert_eq!(fraction, 1.0);
        assert_eq!(sample.total, Some(1024));
        let Event::Progress {
            fraction,
            sample: Some(sample),
            ..
        } = receiver.recv().unwrap()
        else {
            panic!("expected the validation phase")
        };
        assert_eq!(fraction, -1.0);
        assert_eq!(sample.total, None);
        assert!(sample.phase.contains("prüfen"));
        let mut progress = job.ftp_download_progress("ftp://example.test/file".into(), Some(512));
        progress(DownloadProgress {
            received: 1024,
            total: None,
            phase: DownloadPhase::Receiving,
        });
        let Event::Progress {
            fraction,
            sample: Some(sample),
            ..
        } = receiver.recv().unwrap()
        else {
            panic!("expected unknown total")
        };
        assert_eq!(fraction, -1.0);
        assert_eq!(sample.completed, 1024);
        assert_eq!(sample.total, None);
    }

    #[test]
    fn search_and_recovery_do_not_invent_remaining_work_or_byte_rates() {
        let mut tracker = Tracker::default();
        tracker.update(Sample {
            scan: Some((23, Some(7), 2)),
            ..Default::default()
        });
        let data = tracker.render(false, false, OperationKind::Search).unwrap();
        assert_eq!(data.processed, "23");
        assert_eq!(data.matched, "7");
        assert_eq!(data.errors, "2");
        assert_eq!(data.pending, "");
        assert_eq!(data.speed, "—");
        assert_eq!(data.finish, "");
        tracker.update(Sample {
            recovering: true,
            phase: "Vorherige Namen wiederherstellen …".into(),
            ..Default::default()
        });
        tracker.rendered -= Duration::from_secs(1);
        let data = tracker.render(false, false, OperationKind::Rename).unwrap();
        assert!(data.recovering);
        assert_eq!(data.processed, "");
        assert_eq!(data.bytes, "");
        assert_eq!(data.finish, "");
    }

    #[test]
    fn operation_and_phase_remain_clear_when_no_byte_total_is_available() {
        for (kind, expected) in [
            (OperationKind::Move, "Dateien werden verschoben …"),
            (OperationKind::Pack, "Archiv wird erstellt …"),
            (OperationKind::Extract, "Archiv wird entpackt …"),
        ] {
            let mut tracker = Tracker::default();
            let data = tracker.render(false, false, kind).unwrap();
            assert_eq!(data.phase, tr(expected));
            assert_eq!(data.speed, "—");
            assert_eq!(data.remaining, "—");
            tracker.update(Sample {
                phase: "Archiv wird abgeschlossen …".into(),
                ..Default::default()
            });
            tracker.rendered -= Duration::from_secs(1);
            assert_eq!(
                tracker.render(false, false, kind).unwrap().phase,
                tr("Archiv wird abgeschlossen …")
            );
            tracker.rendered -= Duration::from_secs(1);
            assert_eq!(
                tracker.render(true, false, kind).unwrap().phase,
                tr(if kind == OperationKind::Move {
                    "Pause am nächsten Eintrag"
                } else {
                    "Pausiert"
                })
            );
        }
    }

    #[test]
    fn deletion_counts_processed_and_remaining_entries_without_byte_statistics() {
        let mut tracker = Tracker::default();
        tracker.update(Sample {
            source: "Beispielordner".into(),
            items: Some((4, Some(5))),
            processed_items: Some(3),
            ..Default::default()
        });
        let data = tracker.render(false, false, OperationKind::Task).unwrap();
        assert_eq!(data.processed, "3");
        assert_eq!(data.pending, "2");
        assert_eq!(data.bytes, "");
        assert_eq!(data.speed, "—");
        assert_eq!(data.finish, "");
        tracker.update(Sample {
            items: Some((5, Some(5))),
            processed_items: Some(5),
            ..Default::default()
        });
        tracker.rendered -= Duration::from_secs(1);
        let data = tracker.render(false, false, OperationKind::Task).unwrap();
        assert_eq!(data.processed, "5");
        assert_eq!(data.pending, "0");
    }

    #[test]
    fn unknown_totals_and_pauses_do_not_show_an_eta() {
        let mut tracker = Tracker::default();
        tracker.update(Sample {
            completed: 1024,
            ..Default::default()
        });
        let data = tracker.render(false, false, OperationKind::Task).unwrap();
        assert_eq!(data.remaining, "—");
        assert_eq!(data.finish, "");
        tracker.rendered -= Duration::from_secs(1);
        let data = tracker.render(true, false, OperationKind::Task).unwrap();
        assert_eq!(data.remaining, "—");
        assert_eq!(data.speed, "—");
        assert_eq!(data.finish, "");
    }

    #[test]
    fn estimates_from_recent_bytes_and_clamps_remaining_work() {
        let mut tracker = Tracker::default();
        tracker.update(Sample {
            completed: 1024,
            total: Some(2048),
            ..Default::default()
        });
        tracker.history = VecDeque::from([(Instant::now() - Duration::from_secs(1), 0)]);
        let data = tracker.render(false, false, OperationKind::Task).unwrap();
        assert_ne!(data.speed, "—");
        assert_ne!(data.remaining, "—");
        assert!(!data.finish.is_empty());
        tracker.rendered -= Duration::from_secs(1);
        tracker.update(Sample {
            completed: 3072,
            total: Some(2048),
            ..Default::default()
        });
        assert_eq!(
            tracker
                .render(false, false, OperationKind::Task)
                .unwrap()
                .remaining,
            "0:00"
        );
        assert_eq!(duration(Duration::from_secs(3661)), "1:01:01");
    }

    #[test]
    fn step_only_work_and_new_phases_do_not_reuse_byte_statistics() {
        let mut tracker = Tracker::default();
        tracker.update(Sample {
            items: Some((2, Some(5))),
            ..Default::default()
        });
        let data = tracker.render(false, false, OperationKind::Task).unwrap();
        assert_eq!(data.bytes, "");
        assert_eq!(data.remaining, "—");
        assert!(data.items.ends_with("2 / 5"));
        tracker.update(Sample {
            completed: 1024,
            total: Some(2048),
            ..Default::default()
        });
        tracker.unmeasured();
        tracker.rendered -= Duration::from_secs(1);
        let data = tracker.render(false, false, OperationKind::Task).unwrap();
        assert_eq!(data.bytes, "");
        assert_eq!(data.speed, "—");
        assert_eq!(data.source, "");
    }
}
