use super::{Session, check_cancel};
use std::io::{self, Read, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

const INHERIT: u32 = u32::MAX;

#[derive(Clone, Debug)]
pub struct TransferLimit(Arc<AtomicU32>);

impl Default for TransferLimit {
    fn default() -> Self {
        Self(Arc::new(AtomicU32::new(INHERIT)))
    }
}

impl TransferLimit {
    pub fn override_kib(&self) -> Option<u32> {
        let value = self.0.load(Ordering::Relaxed);
        (value != INHERIT).then_some(value)
    }

    pub fn set_override_kib(&self, value: Option<u32>) -> io::Result<()> {
        if value.is_some_and(|value| value > 1_048_576) {
            return Err(super::invalid_input(
                "Gib ein Durchsatzlimit von 0 bis 1048576 KiB/s ein.",
            ));
        }
        self.0.store(value.unwrap_or(INHERIT), Ordering::Relaxed);
        Ok(())
    }
}

pub(super) struct Rate {
    started: Instant,
    base: u64,
    completed: u64,
    active: Option<u64>,
    configured: Option<u32>,
    live: Option<TransferLimit>,
    divisor: u64,
}

impl Session {
    pub fn for_file_worker(&self, index: usize, width: usize) -> Self {
        let mut session = if index == 0 {
            self.clone()
        } else {
            self.with_transfer_connection()
        };
        session.file_workers = width.clamp(1, 8);
        session
    }

    pub fn with_transfer_limit(&self, limit: TransferLimit) -> Self {
        let mut session = self.clone();
        session.transfer_limit = Some(limit);
        session
    }

    pub(super) fn transfer_rate(&self) -> Rate {
        let mut rate = Rate {
            started: Instant::now(),
            base: 0,
            completed: 0,
            active: None,
            configured: self.transfer_settings().speed_kib,
            live: self.transfer_limit.clone(),
            divisor: self.file_workers as u64,
        };
        rate.active = rate.bytes_per_second();
        rate
    }
}

impl Rate {
    pub(super) fn for_connection(mut self, count: usize) -> Self {
        self.divisor *= count as u64;
        self.active = self.bytes_per_second();
        self
    }

    pub(super) fn adjustable(&self) -> bool {
        self.live.is_some()
    }

    pub(super) fn bytes_per_second(&self) -> Option<u64> {
        self.live
            .as_ref()
            .and_then(TransferLimit::override_kib)
            .or(self.configured)
            .filter(|&rate| rate > 0)
            .map(|rate| u64::from(rate) * 1024 / self.divisor)
    }

    /// Keep progress and cancellation responsive at low configured rates.
    pub(super) fn chunk(&self, maximum: usize) -> usize {
        self.bytes_per_second()
            .map_or(maximum, |rate| maximum.min((rate / 4).max(1) as usize))
    }

    fn delay(&mut self, transferred: u64, now: Instant) -> Duration {
        let rate = self.bytes_per_second();
        if rate != self.active {
            self.started = now;
            self.base = self.completed;
            self.active = rate;
        }
        let Some(rate) = rate else {
            return Duration::ZERO;
        };
        let transferred = transferred.saturating_sub(self.base);
        let expected = Duration::from_secs(transferred / rate)
            + Duration::from_nanos((transferred % rate) * 1_000_000_000 / rate);
        expected.saturating_sub(now.duration_since(self.started))
    }

    pub(super) fn wait(&mut self, transferred: u64, cancel: &AtomicBool) -> io::Result<()> {
        loop {
            check_cancel(cancel)?;
            let remaining = self.delay(transferred, Instant::now());
            if remaining.is_zero() {
                self.completed = transferred;
                return Ok(());
            }
            std::thread::sleep(remaining.min(Duration::from_millis(75)));
        }
    }

    pub(super) async fn wait_async(
        &mut self,
        transferred: u64,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        loop {
            check_cancel(cancel)?;
            let remaining = self.delay(transferred, Instant::now());
            if remaining.is_zero() {
                self.completed = transferred;
                return Ok(());
            }
            tokio::time::sleep(remaining.min(Duration::from_millis(75))).await;
        }
    }
}

pub(super) fn copy_output(
    mut incoming: impl Read,
    mut output: impl Write,
    mut rate: Rate,
    cancel: &AtomicBool,
) -> io::Result<Vec<u8>> {
    let mut buffer = [0; 8 * 1024];
    let mut total = 0;
    loop {
        check_cancel(cancel)?;
        let capacity = rate.chunk(buffer.len());
        let bytes = incoming.read(&mut buffer[..capacity])?;
        if bytes == 0 {
            return Ok(Vec::new());
        }
        total += bytes as u64;
        rate.wait(total, cancel)?;
        output.write_all(&buffer[..bytes])?;
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::ftp::{Connection, Security, Url};

    #[test]
    fn changing_a_limit_charges_only_bytes_from_the_current_block() {
        let limit = TransferLimit::default();
        let session = Connection::new("ftp://localhost/", "arco", Security::Plain)
            .unwrap()
            .authenticate("")
            .unwrap()
            .with_transfer_limit(limit.clone());
        let mut rate = session.transfer_rate();
        let now = rate.started;
        assert_eq!(rate.delay(1024 * 1024, now), Duration::ZERO);
        rate.completed = 1024 * 1024;
        limit.set_override_kib(Some(4)).unwrap();
        assert_eq!(
            rate.delay(1024 * 1024 + 1024, now),
            Duration::from_millis(250)
        );
        assert_eq!(
            rate.delay(1024 * 1024 + 1024, now + Duration::from_millis(100)),
            Duration::from_millis(150)
        );
        limit.set_override_kib(Some(0)).unwrap();
        assert_eq!(
            rate.delay(1024 * 1024 + 1024, now + Duration::from_millis(100)),
            Duration::ZERO
        );
        assert!(limit.set_override_kib(Some(1_048_577)).is_err());
        assert_eq!(limit.override_kib(), Some(0));
    }

    pub(in crate::ftp) fn check_live_download(session: &Session, directory: &Url) {
        let work = tempfile::tempdir().unwrap();
        let source = work.path().join("source");
        let result = work.path().join("result");
        let payload: Vec<_> = (0..128 * 1024).map(|i| (i % 251) as u8).collect();
        std::fs::write(&source, &payload).unwrap();
        let cancel = AtomicBool::new(false);
        let name = "arco-live-limit.bin";
        session
            .upload(directory, &source, name, false, &cancel)
            .unwrap();

        let limit = TransferLimit::default();
        limit.set_override_kib(Some(8)).unwrap();
        let copy = session.with_transfer_limit(limit.clone());
        let started = Instant::now();
        let mut changed = false;
        copy.download_with_progress(directory, name, &result, &cancel, |progress| {
            if !changed && progress.received > 0 && progress.received < payload.len() as u64 {
                limit.set_override_kib(Some(0)).unwrap();
                changed = true;
            }
        })
        .unwrap();
        assert!(changed, "expected progress during the transfer");
        assert!(
            started.elapsed() < Duration::from_secs(8),
            "the initial limit remained in effect"
        );
        assert_eq!(std::fs::read(&result).unwrap(), payload);

        limit.set_override_kib(Some(8)).unwrap();
        let cancelled = work.path().join("cancelled");
        let error = copy
            .download_with_progress(directory, name, &cancelled, &cancel, |progress| {
                if progress.received > 0 {
                    cancel.store(true, Ordering::Relaxed);
                }
            })
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        assert!(!cancelled.exists());
        cancel.store(false, Ordering::Relaxed);

        let listing = session.list_sync(directory, &cancel).unwrap();
        session
            .delete_sync_entry(directory, listing.regular_file(name).unwrap(), &cancel)
            .unwrap();
    }
}
