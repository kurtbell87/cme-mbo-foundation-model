//! Bidirectional heartbeat management.
//!
//! - Send-side: sends RequestHeartbeat on interval, but skips when other
//!   messages are flowing (per Rithmic Reference Guide).
//! - Read-side: tracks last inbound message timestamp. If elapsed > 2x
//!   heartbeat_interval, declares the connection dead.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::rti;

/// Shared liveness tracker updated by the WebSocket read task.
#[derive(Debug, Clone)]
pub struct LivenessTracker {
    /// Nanoseconds since process start of last inbound message.
    last_inbound_ns: Arc<AtomicU64>,
    epoch: Instant,
}

impl Default for LivenessTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl LivenessTracker {
    pub fn new() -> Self {
        let epoch = Instant::now();
        Self {
            last_inbound_ns: Arc::new(AtomicU64::new(0)),
            epoch,
        }
    }

    /// Record that a message was received.
    pub fn record_inbound(&self) {
        let elapsed = self.epoch.elapsed().as_nanos() as u64;
        self.last_inbound_ns.store(elapsed, Ordering::Relaxed);
    }

    /// Check if the connection appears dead (no message in 2x heartbeat_interval).
    pub fn is_dead(&self, heartbeat_interval: Duration) -> bool {
        let last = self.last_inbound_ns.load(Ordering::Relaxed);
        if last == 0 {
            return false; // haven't received first message yet
        }
        let elapsed_since_last =
            Duration::from_nanos(self.epoch.elapsed().as_nanos() as u64 - last);
        elapsed_since_last > heartbeat_interval * 2
    }

    /// Get elapsed time since last inbound message.
    pub fn elapsed_since_last(&self) -> Option<Duration> {
        let last = self.last_inbound_ns.load(Ordering::Relaxed);
        if last == 0 {
            return None;
        }
        let now = self.epoch.elapsed().as_nanos() as u64;
        Some(Duration::from_nanos(now - last))
    }
}

/// Create a heartbeat request message.
pub fn make_heartbeat_request() -> rti::RequestHeartbeat {
    rti::RequestHeartbeat {
        template_id: Some(18),
        user_msg: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn liveness_tracker_starts_alive() {
        let tracker = LivenessTracker::new();
        assert!(!tracker.is_dead(Duration::from_secs(30)));
    }

    #[test]
    fn liveness_tracker_records_inbound() {
        let tracker = LivenessTracker::new();
        tracker.record_inbound();
        assert!(tracker.elapsed_since_last().is_some());
    }

    #[test]
    fn heartbeat_request_has_correct_template_id() {
        let hb = make_heartbeat_request();
        assert_eq!(hb.template_id, Some(18));
    }
}
