use std::time::{Duration, Instant};

pub(super) struct Clicks<K> {
    previous: Option<(K, (i32, i32), Instant)>,
}

impl<K> Default for Clicks<K> {
    fn default() -> Self {
        Self { previous: None }
    }
}

impl<K: PartialEq> Clicks<K> {
    pub fn clear(&mut self) {
        self.previous = None;
    }

    pub fn click(
        &mut self,
        key: K,
        point: (i32, i32),
        now: Instant,
        is_double: impl FnOnce((i32, i32), (i32, i32), Duration) -> bool,
    ) -> bool {
        if let Some((previous, position, time)) = self.previous.take()
            && previous == key
            && is_double(position, point, now.saturating_duration_since(time))
        {
            return true;
        }
        self.previous = Some((key, point, now));
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(first: (i32, i32), second: (i32, i32), elapsed: Duration) -> bool {
        first.0.abs_diff(second.0) <= 2
            && first.1.abs_diff(second.1) <= 2
            && elapsed <= Duration::from_millis(300)
    }

    #[test]
    fn opens_only_matching_pairs_and_consumes_each_pair() {
        let mut clicks = Clicks::default();
        let now = Instant::now();
        assert!(!clicks.click((0, "folder", 1), (20, 20), now, close));
        assert!(clicks.click(
            (0, "folder", 1),
            (21, 20),
            now + Duration::from_millis(100),
            close
        ));
        assert!(!clicks.click(
            (0, "folder", 1),
            (20, 20),
            now + Duration::from_millis(150),
            close
        ));
        clicks.clear();
        assert!(!clicks.click(
            (0, "folder", 1),
            (20, 20),
            now + Duration::from_millis(200),
            close
        ));
    }

    #[test]
    fn other_panels_locations_rows_timeouts_and_movement_do_not_open() {
        for (key, point, delay) in [
            ((1, "folder", 1), (20, 20), 100),
            ((0, "other-folder", 1), (20, 20), 100),
            ((0, "folder", 2), (20, 20), 100),
            ((0, "folder", 1), (24, 20), 100),
            ((0, "folder", 1), (20, 20), 400),
        ] {
            let mut clicks = Clicks::default();
            let now = Instant::now();
            assert!(!clicks.click((0, "folder", 1), (20, 20), now, close));
            assert!(!clicks.click(key, point, now + Duration::from_millis(delay), close));
        }
    }
}
