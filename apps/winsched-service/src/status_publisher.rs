//! Monotonic gate for immediate status receipts and bounded heartbeats.

#![forbid(unsafe_code)]

pub const DEFAULT_STATUS_HEARTBEAT_INTERVAL_MS: u64 = 10_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatusPublishGate {
    heartbeat_interval_ms: u64,
    last_published_ms: Option<u64>,
}

impl Default for StatusPublishGate {
    fn default() -> Self {
        Self::new(DEFAULT_STATUS_HEARTBEAT_INTERVAL_MS)
    }
}

impl StatusPublishGate {
    #[must_use]
    pub const fn new(heartbeat_interval_ms: u64) -> Self {
        Self {
            heartbeat_interval_ms,
            last_published_ms: None,
        }
    }

    #[must_use]
    pub fn should_publish(&self, now_ms: u64, force: bool) -> bool {
        force
            || self
                .last_published_ms
                .is_none_or(|last| now_ms.saturating_sub(last) >= self.heartbeat_interval_ms)
    }

    pub fn mark_published(&mut self, now_ms: u64) {
        self.last_published_ms = Some(now_ms);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emits_initial_status_and_ten_second_heartbeats() {
        let mut gate = StatusPublishGate::default();
        assert!(gate.should_publish(0, false));
        gate.mark_published(0);
        assert!(!gate.should_publish(9_999, false));
        assert!(gate.should_publish(10_000, false));
        gate.mark_published(10_000);
        assert!(!gate.should_publish(19_999, false));
        assert!(gate.should_publish(20_000, false));
    }

    #[test]
    fn immediate_receipt_resets_heartbeat_deadline_without_double_write() {
        let mut gate = StatusPublishGate::default();
        assert!(gate.should_publish(0, false));
        gate.mark_published(0);
        assert!(gate.should_publish(3_000, true));
        gate.mark_published(3_000);
        assert!(!gate.should_publish(10_000, false));
        assert!(gate.should_publish(13_000, false));
    }

    #[test]
    fn monotonic_jump_after_sleep_forces_next_heartbeat() {
        let mut gate = StatusPublishGate::default();
        assert!(gate.should_publish(1_000, false));
        gate.mark_published(1_000);
        assert!(gate.should_publish(600_000, false));
    }
}
