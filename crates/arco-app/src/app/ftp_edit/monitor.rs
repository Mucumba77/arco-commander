use super::{App, open_edit};
use arco_windows::{EditFileRevision, edit_file_revision};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};

#[derive(Clone, Default)]
pub(super) struct Automatic {
    pub(super) enabled: bool,
    pub(super) paused: bool,
    generation: u64,
    observed: Option<EditFileRevision>,
    attempted: Option<EditFileRevision>,
}

impl Automatic {
    pub(super) fn toggle(&mut self) {
        self.enabled = !self.enabled;
        self.paused = false;
        self.generation = self.generation.wrapping_add(1);
        self.observed = None;
        self.attempted = None;
    }

    pub(super) fn pause(&mut self) {
        if self.enabled {
            self.toggle();
            self.paused = true;
        }
    }

    pub(super) fn retry_later(&mut self) {
        self.attempted = None;
        self.observed = None;
    }

    fn ready(&mut self, revision: Option<EditFileRevision>, baseline: EditFileRevision) -> bool {
        let previous = self.observed;
        self.observed = revision;
        revision.is_some_and(|revision| {
            self.enabled
                && Some(revision) == previous
                && revision != baseline
                && Some(revision) != self.attempted
        })
    }
}

struct Observation {
    path: PathBuf,
    generation: u64,
    revision: Option<EditFileRevision>,
}

#[derive(Default)]
pub(in crate::app) struct Monitor {
    next: Option<Instant>,
    scan: Option<Receiver<Vec<Observation>>>,
}

impl App {
    pub(in crate::app) fn poll_ftp_drafts(&mut self) {
        if let Some(scan) = &self.ftp_edit_monitor.scan {
            match scan.try_recv() {
                Ok(scanned) => {
                    self.ftp_edit_monitor.scan = None;
                    for entry in scanned {
                        self.observe_ftp_draft(entry);
                    }
                }
                Err(TryRecvError::Disconnected) => self.ftp_edit_monitor.scan = None,
                Err(TryRecvError::Empty) => return,
            }
        }
        let now = Instant::now();
        if self.ftp_edit_monitor.next.is_some_and(|next| now < next) {
            return;
        }
        self.ftp_edit_monitor.next = Some(now + Duration::from_millis(700));
        let pending = self
            .ftp_drafts
            .iter()
            .filter(|draft| draft.automatic.enabled && !self.ftp_upload_pending(&draft.path))
            .map(|draft| (draft.path.clone(), draft.automatic.generation))
            .collect::<Vec<_>>();
        if pending.is_empty() {
            return;
        }
        let (send, receive) = mpsc::channel();
        self.ftp_edit_monitor.scan = Some(receive);
        std::thread::spawn(move || {
            let observations = pending
                .into_iter()
                .map(|(path, generation)| {
                    let revision = open_edit(&path)
                        .and_then(|file| edit_file_revision(&file))
                        .ok();
                    Observation {
                        path,
                        generation,
                        revision,
                    }
                })
                .collect();
            let _ = send.send(observations);
        });
    }

    fn observe_ftp_draft(&mut self, observation: Observation) {
        if self.ftp_upload_pending(&observation.path) {
            return;
        }
        let Some(draft) = self.ftp_drafts.iter_mut().find(|draft| {
            draft.path == observation.path && draft.automatic.generation == observation.generation
        }) else {
            return;
        };
        if !draft
            .automatic
            .ready(observation.revision, draft.local_revision)
        {
            return;
        }
        let draft = draft.clone();
        if self.queue_ftp_draft(draft, observation.revision).is_ok()
            && let Some(draft) = self
                .ftp_drafts
                .iter_mut()
                .find(|draft| draft.path == observation.path)
        {
            draft.automatic.attempted = observation.revision;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn automatic_upload_waits_for_two_matching_revisions_and_pauses_after_failure() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("copy.txt");
        let revision = |bytes: &[u8]| {
            fs::write(&path, bytes).unwrap();
            edit_file_revision(&open_edit(&path).unwrap()).unwrap()
        };
        let baseline = revision(b"before");
        let first = revision(b"part of the edit");
        let settled = revision(b"complete edit saved");
        let mut automatic = Automatic::default();
        automatic.toggle();
        assert!(!automatic.ready(Some(first), baseline));
        assert!(!automatic.ready(None, baseline));
        assert!(!automatic.ready(Some(settled), baseline));
        assert!(automatic.ready(Some(settled), baseline));
        automatic.attempted = Some(settled);
        assert!(!automatic.ready(Some(settled), baseline));
        automatic.pause();
        assert!(automatic.paused && !automatic.enabled);
        assert!(!automatic.ready(Some(first), baseline));
        assert!(!automatic.ready(Some(first), baseline));
        automatic.toggle();
        assert!(!automatic.ready(Some(settled), baseline));
        assert!(automatic.ready(Some(settled), baseline));
    }
}
