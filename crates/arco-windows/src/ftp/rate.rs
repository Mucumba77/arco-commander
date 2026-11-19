use super::{Session, check_cancel};
use std::io;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

pub(super) struct Rate {
    started: Instant,
    bytes_per_second: Option<u64>,
}

impl Session {
    pub(super) fn transfer_rate(&self) -> Rate {
        Rate {
            started: Instant::now(),
            bytes_per_second: self
                .transfer_settings()
                .speed_kib
                .map(|rate| u64::from(rate) * 1024),
        }
    }
}

impl Rate {
    /// Keep progress and cancellation responsive at low configured rates.
    pub(super) fn chunk(&self, maximum: usize) -> usize {
        self.bytes_per_second
            .map_or(maximum, |rate| maximum.min((rate / 4).max(1) as usize))
    }

    fn delay(&self, transferred: u64) -> Duration {
        let Some(rate) = self.bytes_per_second else {
            return Duration::ZERO;
        };
        let expected = Duration::from_secs(transferred / rate)
            + Duration::from_nanos((transferred % rate) * 1_000_000_000 / rate);
        expected.saturating_sub(self.started.elapsed())
    }

    pub(super) fn wait(&self, transferred: u64, cancel: &AtomicBool) -> io::Result<()> {
        loop {
            let remaining = self.delay(transferred);
            if remaining.is_zero() {
                return Ok(());
            }
            check_cancel(cancel)?;
            std::thread::sleep(remaining.min(Duration::from_millis(75)));
        }
    }

    pub(super) async fn wait_async(&self, transferred: u64, cancel: &AtomicBool) -> io::Result<()> {
        loop {
            let remaining = self.delay(transferred);
            if remaining.is_zero() {
                return Ok(());
            }
            check_cancel(cancel)?;
            tokio::time::sleep(remaining.min(Duration::from_millis(75))).await;
        }
    }
}
