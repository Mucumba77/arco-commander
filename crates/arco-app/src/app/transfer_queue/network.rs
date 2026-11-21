use super::{App, Event, JobContext, Outcome, QueuedTransfer, TransferReport};
use crate::app::job_progress::{Sample, Tracker};
use crate::{JobMetrics, OperationKind};
use arco_windows::ftp::{
    ConnectionSlots, Security, Session, TransferConnections, TransferLimit, Url,
};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug)]
pub(crate) struct Access {
    path: TransferPath,
    write: bool,
}

pub(crate) struct NetworkTransfer {
    pub(super) access: Vec<Access>,
    limit: Option<TransferLimit>,
    minimum_kib: u32,
    parallel_files: bool,
    connections: TransferConnections,
}

impl NetworkTransfer {
    pub(crate) fn server_direct(mut self) -> Self {
        self.limit = None;
        self.parallel_files = false;
        self
    }

    pub(crate) fn file_connections(&self) -> Option<TransferConnections> {
        self.parallel_files.then(|| self.connections.clone())
    }

    pub(crate) fn session(&self, session: &Session) -> Session {
        let session = session
            .with_transfer_connection()
            .with_parallel_sftp_downloads(self.minimum_kib, self.connections.clone());
        if let Some(limit) = &self.limit {
            session.with_transfer_limit(limit.clone())
        } else {
            session
        }
    }
}

#[derive(Clone, Debug)]
enum TransferPath {
    Local(PathBuf),
    Remote {
        host: String,
        port: u16,
        parts: Vec<String>,
    },
}

impl Access {
    pub(crate) fn local(path: &Path, write: bool) -> Self {
        let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_owned());
        let resolved = absolute
            .ancestors()
            .find_map(|parent| {
                let real = std::fs::canonicalize(parent).ok()?;
                let remaining = absolute.strip_prefix(parent).ok()?;
                Some(real.join(remaining))
            })
            .unwrap_or(absolute);
        Self {
            path: TransferPath::Local(if cfg!(windows) {
                resolved.to_string_lossy().to_lowercase().into()
            } else {
                resolved
            }),
            write,
        }
    }

    pub(crate) fn remote(
        session: &Session,
        directory: &Url,
        name: Option<&str>,
        write: bool,
    ) -> io::Result<Self> {
        let port = directory
            .port_or_known_default()
            .unwrap_or_else(|| match session.security() {
                Security::Sftp | Security::Scp => 22,
                Security::ImplicitTls => 990,
                _ => 21,
            });
        let host = directory
            .host_str()
            .unwrap_or_default()
            .to_ascii_lowercase();
        let mut parts: Vec<_> = session
            .remote_path(directory)?
            .split('/')
            .filter(|part| !part.is_empty())
            .map(str::to_lowercase)
            .collect();
        if let Some(name) = name {
            parts.push(name.to_lowercase());
        }
        Ok(Self {
            path: TransferPath::Remote { host, port, parts },
            write,
        })
    }

    fn conflicts(&self, other: &Self) -> bool {
        (self.write || other.write)
            && match (&self.path, &other.path) {
                (TransferPath::Local(left), TransferPath::Local(right)) => {
                    left.starts_with(right) || right.starts_with(left)
                }
                (
                    TransferPath::Remote { host, port, parts },
                    TransferPath::Remote {
                        host: other_host,
                        port: other_port,
                        parts: other_parts,
                    },
                ) => {
                    host == other_host
                        && port == other_port
                        && (parts.starts_with(other_parts) || other_parts.starts_with(parts))
                }
                _ => false,
            }
    }
}

pub(super) fn can_start<T>(
    requests: &[Access],
    active: impl IntoIterator<Item = T>,
    limit: usize,
) -> bool
where
    T: AsRef<[Access]>,
{
    for (index, running) in active.into_iter().enumerate() {
        if index + 1 >= limit
            || requests
                .iter()
                .any(|request| running.as_ref().iter().any(|path| request.conflicts(path)))
        {
            return false;
        }
    }
    limit > 0
}

pub(super) struct Running {
    pub(super) id: u64,
    pub(super) label: String,
    pub(super) access: Vec<Access>,
    pub(super) cancel: Arc<AtomicBool>,
    pub(super) limit: Option<TransferLimit>,
    _slot: ConnectionSlots,
    receiver: Receiver<Event>,
    feed: Option<Arc<Mutex<Option<Sample>>>>,
    tracker: Tracker,
    kind: OperationKind,
    pub(super) fraction: f32,
    pub(super) detail: String,
    pub(super) target: String,
    pub(super) metrics: JobMetrics,
}

impl Running {
    fn observe(&mut self, detail: String, fraction: f32, sample: Option<Sample>) {
        if self.cancel.load(Ordering::Relaxed) {
            return;
        }
        self.fraction = fraction;
        match sample {
            Some(sample) => {
                self.detail = if sample.phase.is_empty() {
                    detail
                } else {
                    sample.phase.clone()
                };
                self.tracker.update(sample);
            }
            None => {
                self.detail = detail;
                self.tracker.unmeasured();
            }
        }
    }
}

impl App {
    pub(crate) fn new_network_transfer(&self, access: Vec<Access>) -> NetworkTransfer {
        NetworkTransfer {
            access,
            limit: Some(TransferLimit::default()),
            minimum_kib: self.profile.parallel_sftp_minimum_kib,
            parallel_files: self.profile.parallel_network_files,
            connections: self.transfers.connections.clone(),
        }
    }

    pub(super) fn start_network_transfer(
        &mut self,
        id: u64,
        task: QueuedTransfer,
        slot: ConnectionSlots,
    ) {
        let QueuedTransfer::Virtual {
            label,
            presentation,
            work,
            refresh,
            network: Some(NetworkTransfer { access, limit, .. }),
            ..
        } = task
        else {
            unreachable!("only network transfers use the background queue");
        };
        let (sender, receiver) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let context = JobContext {
            archives: self.profile.archives,
            cancel: cancel.clone(),
            sender: sender.clone(),
        };
        let language = self.profile.language;
        let log = self.profile.transfer_log.clone();
        let name = label.clone();
        std::thread::spawn(move || {
            crate::locale::worker_language(language);
            let outcome = super::virtual_result(&name, log.as_deref(), work(context));
            let _ = sender.send(Event::Finished {
                outcome: Ok(outcome),
                refresh,
            });
        });
        self.transfers.network.push(Running {
            id,
            label,
            access,
            cancel,
            limit,
            _slot: slot,
            receiver,
            feed: None,
            tracker: Tracker::default(),
            kind: presentation.kind,
            fraction: -1.0,
            detail: presentation.detail,
            target: presentation.target,
            metrics: JobMetrics::default(),
        });
        self.publish_transfer_manager();
    }

    pub(crate) fn drain_network_transfers(&mut self) {
        let mut completed = Vec::new();
        let mut refresh = false;
        let mut changed = false;
        let mut index = 0;
        while index < self.transfers.network.len() {
            let running = &mut self.transfers.network[index];
            let outcome = loop {
                match running.receiver.try_recv() {
                    Ok(Event::Progress {
                        detail,
                        fraction,
                        sample,
                    }) => {
                        running.observe(detail, fraction, sample);
                        changed = true;
                    }
                    Ok(Event::ProgressFeed(feed)) => running.feed = Some(feed),
                    Ok(Event::Finished {
                        outcome,
                        refresh: reload,
                    }) => {
                        refresh |= reload;
                        break Some(outcome);
                    }
                    Ok(Event::TransferConflict(ticket)) => self.transfers.prompts.push_back(ticket),
                    Ok(event) => {
                        let _ = self.sender.send(event);
                    }
                    Err(TryRecvError::Empty) => break None,
                    Err(TryRecvError::Disconnected) => {
                        break Some(Err("Der Netzwerktransfer wurde unerwartet beendet.".into()));
                    }
                }
            };
            if let Some(outcome) = outcome {
                let running = self.transfers.network.remove(index);
                completed.push(outcome.unwrap_or_else(|error| {
                    Outcome::Report(
                        running.label,
                        TransferReport {
                            errors: vec![error],
                            ..Default::default()
                        },
                    )
                }));
                changed = true;
            } else {
                let sample = running
                    .feed
                    .as_ref()
                    .and_then(|feed| feed.lock().ok()?.take());
                if let Some(sample) = sample {
                    let fraction = sample
                        .total
                        .filter(|&total| total > 0)
                        .map_or(-1.0, |total| {
                            (sample.completed.min(total) as f64 / total as f64) as f32
                        });
                    running.observe(String::new(), fraction, Some(sample));
                    changed = true;
                }
                if let Some(metrics) = running.tracker.render(false, false, running.kind)
                    && metrics != running.metrics
                {
                    running.metrics = metrics;
                    changed = true;
                }
                index += 1;
            }
        }
        if refresh {
            self.refresh_both();
        }
        for outcome in completed {
            self.completed(outcome);
        }
        if changed {
            self.publish_network_transfers();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arco_windows::ftp::Connection;

    #[test]
    fn distinct_files_can_run_together_and_overlapping_writers_wait() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let first = [Access::local(&root.join("first"), true)];
        let second = [Access::local(&root.join("second"), true)];
        let read_first = [Access::local(&root.join("first"), false)];
        let folder = [Access::local(root, true)];
        assert!(can_start(&second, [&first], 2));
        assert!(!can_start(&second, [&first], 1));
        assert!(!can_start(&read_first, [&first], 2));
        assert!(!can_start(&folder, [&first], 2));
        assert!(can_start(&read_first, [&read_first], 2));
    }

    #[test]
    fn remote_tree_access_protects_only_the_matching_server_and_path() {
        let session = Connection::new("ftp://archive.example:2222/", "arco", Security::Plain)
            .unwrap()
            .authenticate("")
            .unwrap();
        let root = session.initial_directory();
        let input = session
            .relative_directory(&root, Path::new("Prüfung"))
            .unwrap();
        let output = session
            .relative_directory(&root, Path::new("Abgabe"))
            .unwrap();
        let tree = [Access::remote(&session, &input, None, true).unwrap()];
        let child = [Access::remote(&session, &input, Some("Bericht.txt"), false).unwrap()];
        let sibling = [Access::remote(&session, &output, Some("Bericht.txt"), true).unwrap()];
        assert!(!can_start(&child, [&tree], 2));
        assert!(can_start(&sibling, [&tree], 2));
    }
}
