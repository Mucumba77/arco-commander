use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
pub struct TransferConnections(Arc<Mutex<Usage>>);

#[derive(Default)]
struct Usage {
    limit: usize,
    in_use: usize,
}

pub struct ConnectionSlots {
    pool: TransferConnections,
    count: usize,
}

impl TransferConnections {
    pub fn set_limit(&self, limit: usize) {
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .limit = limit.clamp(1, 8);
    }

    pub fn try_claim(&self) -> Option<ConnectionSlots> {
        self.try_claim_up_to(1)
    }

    pub(super) fn try_claim_up_to(&self, max: usize) -> Option<ConnectionSlots> {
        let mut usage = self.0.lock().unwrap_or_else(|error| error.into_inner());
        let count = usage.limit.saturating_sub(usage.in_use).min(max);
        if count == 0 {
            return None;
        }
        usage.in_use += count;
        Some(ConnectionSlots {
            pool: self.clone(),
            count,
        })
    }
}

impl ConnectionSlots {
    pub(super) fn count(&self) -> usize {
        self.count
    }
}

impl Drop for ConnectionSlots {
    fn drop(&mut self) {
        let mut usage = self
            .pool
            .0
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        usage.in_use -= self.count;
    }
}

#[derive(Clone)]
pub(super) struct ParallelDownloads {
    pub minimum: u64,
    pub connections: TransferConnections,
}

impl super::Session {
    pub fn with_parallel_sftp_downloads(
        mut self,
        minimum_kib: u32,
        connections: TransferConnections,
    ) -> Self {
        self.parallel_downloads = (self.is_sftp() && minimum_kib > 0).then(|| ParallelDownloads {
            minimum: u64::from(minimum_kib) * 1024,
            connections,
        });
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_ranges_share_capacity_with_background_jobs() {
        let pool = TransferConnections::default();
        pool.set_limit(4);
        let first = pool.try_claim().unwrap();
        let second = pool.try_claim().unwrap();
        let ranges = pool.try_claim_up_to(7).unwrap();
        assert_eq!(ranges.count(), 2);
        assert!(pool.try_claim().is_none());
        pool.set_limit(2);
        drop(ranges);
        assert!(pool.try_claim().is_none());
        drop(second);
        assert!(pool.try_claim().is_some());
        drop(first);
    }
}
