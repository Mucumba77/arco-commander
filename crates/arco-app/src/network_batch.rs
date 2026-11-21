use arco_core::files::TransferReport;
use arco_core::sync::Row;
use arco_windows::ftp::TransferConnections;
use std::collections::BTreeSet;
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::Relaxed};
use std::sync::mpsc;

#[derive(Clone, Copy)]
pub struct Batch<'a> {
    pub connections: Option<&'a TransferConnections>,
    pub cancel: &'a AtomicBool,
}

impl Batch<'_> {
    pub fn run<T: Sync, Worker>(
        self,
        items: &[T],
        stop_on_error: bool,
        worker: impl Fn(usize, usize) -> Worker + Sync,
        transfer: impl Fn(&Worker, &T) -> io::Result<()> + Sync,
        mut progress: impl FnMut(&T, usize),
    ) -> TransferReport {
        let mut report = TransferReport::default();
        if items.is_empty() {
            return report;
        }
        let slots: Vec<_> = std::iter::from_fn(|| self.connections?.try_claim())
            .take(items.len() - 1)
            .collect();
        let width = slots.len() + 1;
        let stop = AtomicBool::new(false);
        let next = AtomicUsize::new(0);
        let mut processed = 0;
        let mut errors = Vec::new();
        let should_stop = |result: &io::Result<()>| {
            result
                .as_ref()
                .is_err_and(|error| stop_on_error || error.kind() == io::ErrorKind::Interrupted)
        };
        let mut record = |index, result: io::Result<()>| {
            processed += 1;
            match result {
                Ok(()) => report.completed += 1,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => report.cancelled = true,
                Err(error) => errors.push((index, error.to_string())),
            }
            progress(&items[index], processed);
        };
        if width == 1 {
            let worker = worker(0, 1);
            for (index, item) in items.iter().enumerate() {
                if self.cancel.load(Relaxed) {
                    break;
                }
                let result = transfer(&worker, item);
                let stop = should_stop(&result);
                record(index, result);
                if stop {
                    break;
                }
            }
        } else {
            std::thread::scope(|scope| {
                let (sender, events) = mpsc::channel();
                let run = |index, sender: &mpsc::Sender<_>| {
                    let worker = worker(index, width);
                    while !stop.load(Relaxed) && !self.cancel.load(Relaxed) {
                        let index = next.fetch_add(1, Relaxed);
                        let Some(item) = items.get(index) else {
                            break;
                        };
                        if stop.load(Relaxed) || self.cancel.load(Relaxed) {
                            break;
                        }
                        let result = transfer(&worker, item);
                        if should_stop(&result) {
                            stop.store(true, Relaxed);
                        }
                        if sender.send((index, result)).is_err() {
                            break;
                        }
                    }
                };
                for index in 0..width {
                    let sender = sender.clone();
                    scope.spawn(move || run(index, &sender));
                }
                drop(sender);
                for (index, result) in events {
                    record(index, result);
                }
            });
        }
        errors.sort_unstable_by_key(|&(index, _)| index);
        report.errors = errors.into_iter().map(|(_, error)| error).collect();
        report.cancelled |= self.cancel.load(Relaxed);
        report.skipped = items.len() - processed;
        report
    }
}

pub fn copy_groups<'a, Worker>(
    batch: Batch<'_>,
    rows: impl Iterator<Item = &'a Row>,
    target: impl Fn(&Row) -> Option<String>,
    worker: impl Fn(usize, usize) -> Worker + Sync,
    transfer: impl Fn(&Worker, &Row) -> io::Result<()> + Sync,
    mut progress: impl FnMut(&Path),
) -> TransferReport {
    let rows: Vec<_> = rows.collect();
    let mut report = TransferReport::default();
    let mut start = 0;
    while start < rows.len() && !batch.cancel.load(Relaxed) {
        let mut end = start + 1;
        // Directory creation and conflict decisions can change later paths.
        // Keep such rows, repeated targets and changes of direction in order.
        if let Some(first) = target(rows[start]) {
            let mut targets = BTreeSet::from([first]);
            while let Some(row) = rows.get(end).filter(|row| row.action == rows[start].action) {
                let Some(path) = target(row) else { break };
                if !targets.insert(path) {
                    break;
                }
                end += 1;
            }
        }
        let part = batch.run(
            &rows[start..end],
            false,
            &worker,
            |worker, row| {
                transfer(worker, row).map_err(|error| {
                    io::Error::new(error.kind(), format!("{}: {error}", row.relative.display()))
                })
            },
            |row, _| progress(&row.relative),
        );
        report.completed += part.completed;
        report.skipped += part.skipped;
        report.errors.extend(part.errors);
        start = end;
        if part.cancelled {
            report.cancelled = true;
            break;
        }
    }
    report.cancelled |= batch.cancel.load(Relaxed);
    report.skipped += rows.len() - start;
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn copied_files_share_slots_and_report_each_result_once() {
        let connections = TransferConnections::default();
        connections.set_limit(2);
        let _job = connections.try_claim().unwrap();
        let cancel = AtomicBool::new(false);
        let started = AtomicUsize::new(0);
        let batch = Batch {
            connections: Some(&connections),
            cancel: &cancel,
        };
        let mut progress = Vec::new();
        let report = batch.run(
            &[1, 2, 3, 4],
            false,
            |_, width| assert_eq!(width, 2),
            |_, item| {
                assert!(connections.try_claim().is_none());
                let wave = (started.fetch_add(1, Relaxed) + 1).div_ceil(2) * 2;
                let deadline = Instant::now() + Duration::from_secs(5);
                while started.load(Relaxed) < wave {
                    assert!(
                        Instant::now() < deadline,
                        "two files must reach each transfer wave"
                    );
                    std::thread::sleep(Duration::from_millis(5));
                }
                if *item == 3 {
                    Err(io::Error::other("third file"))
                } else {
                    Ok(())
                }
            },
            |item, completed| progress.push((*item, completed)),
        );
        assert_eq!(report.completed, 3);
        assert_eq!(report.errors, ["third file"]);
        assert_eq!(report.skipped, 0);
        assert_eq!(
            progress.iter().map(|(_, n)| *n).collect::<Vec<_>>(),
            [1, 2, 3, 4]
        );
        progress.sort_unstable_by_key(|&(item, _)| item);
        assert_eq!(
            progress.iter().map(|(item, _)| *item).collect::<Vec<_>>(),
            [1, 2, 3, 4]
        );
        assert!(connections.try_claim().is_some());
    }

    #[test]
    fn a_failed_download_stops_the_remaining_list_when_no_extra_slot_is_free() {
        let pool = TransferConnections::default();
        pool.set_limit(1);
        let _job = pool.try_claim().unwrap();
        let cancel = AtomicBool::new(false);
        let batch = Batch {
            connections: Some(&pool),
            cancel: &cancel,
        };
        let mut reported = Vec::new();
        let report = batch.run(
            &[1, 2, 3],
            true,
            |_, width| assert_eq!(width, 1),
            |_, item| match item {
                1 => Ok(()),
                2 => Err(io::Error::other("second file")),
                _ => panic!("a failed download must stop the remaining list"),
            },
            |item, _| reported.push(*item),
        );
        assert_eq!(reported, [1, 2]);
        assert_eq!(report.completed, 1);
        assert_eq!(report.errors, ["second file"]);
        assert_eq!(report.skipped, 1);
        assert!(!report.cancelled);
    }
}
