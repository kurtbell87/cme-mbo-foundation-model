//! Message counters for monitoring pipeline health.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Atomic counters for tracking message processing.
#[derive(Debug, Clone)]
pub struct MessageCounters {
    inner: Arc<CountersInner>,
}

#[derive(Debug)]
struct CountersInner {
    messages_received: AtomicU64,
    messages_processed: AtomicU64,
    dbo_messages: AtomicU64,
    bbo_messages: AtomicU64,
    trade_messages: AtomicU64,
    sequence_gaps: AtomicU64,
    bbo_validations: AtomicU64,
    bbo_divergences: AtomicU64,
    capture_drops: AtomicU64,
    snapshot_recoveries: AtomicU64,
}

impl MessageCounters {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(CountersInner {
                messages_received: AtomicU64::new(0),
                messages_processed: AtomicU64::new(0),
                dbo_messages: AtomicU64::new(0),
                bbo_messages: AtomicU64::new(0),
                trade_messages: AtomicU64::new(0),
                sequence_gaps: AtomicU64::new(0),
                bbo_validations: AtomicU64::new(0),
                bbo_divergences: AtomicU64::new(0),
                capture_drops: AtomicU64::new(0),
                snapshot_recoveries: AtomicU64::new(0),
            }),
        }
    }

    pub fn inc_received(&self) {
        self.inner.messages_received.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_processed(&self) {
        self.inner.messages_processed.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_dbo(&self) {
        self.inner.dbo_messages.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_bbo(&self) {
        self.inner.bbo_messages.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_trade(&self) {
        self.inner.trade_messages.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_sequence_gaps(&self) {
        self.inner.sequence_gaps.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_bbo_validations(&self) {
        self.inner.bbo_validations.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_bbo_divergences(&self) {
        self.inner.bbo_divergences.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_capture_drops(&self) {
        self.inner.capture_drops.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_snapshot_recoveries(&self) {
        self.inner.snapshot_recoveries.fetch_add(1, Ordering::Relaxed);
    }

    pub fn received(&self) -> u64 {
        self.inner.messages_received.load(Ordering::Relaxed)
    }

    pub fn processed(&self) -> u64 {
        self.inner.messages_processed.load(Ordering::Relaxed)
    }

    pub fn dbo(&self) -> u64 {
        self.inner.dbo_messages.load(Ordering::Relaxed)
    }

    pub fn bbo(&self) -> u64 {
        self.inner.bbo_messages.load(Ordering::Relaxed)
    }

    pub fn trades(&self) -> u64 {
        self.inner.trade_messages.load(Ordering::Relaxed)
    }

    pub fn sequence_gaps(&self) -> u64 {
        self.inner.sequence_gaps.load(Ordering::Relaxed)
    }

    pub fn bbo_validations(&self) -> u64 {
        self.inner.bbo_validations.load(Ordering::Relaxed)
    }

    pub fn bbo_divergences(&self) -> u64 {
        self.inner.bbo_divergences.load(Ordering::Relaxed)
    }

    pub fn capture_drops(&self) -> u64 {
        self.inner.capture_drops.load(Ordering::Relaxed)
    }

    pub fn snapshot_recoveries(&self) -> u64 {
        self.inner.snapshot_recoveries.load(Ordering::Relaxed)
    }

    /// Format a summary line for logging.
    pub fn summary(&self) -> String {
        format!(
            "recv={} proc={} dbo={} bbo={} trade={} gaps={} validations={} divergences={} drops={} recoveries={}",
            self.received(),
            self.processed(),
            self.dbo(),
            self.bbo(),
            self.trades(),
            self.sequence_gaps(),
            self.bbo_validations(),
            self.bbo_divergences(),
            self.capture_drops(),
            self.snapshot_recoveries(),
        )
    }
}

impl Default for MessageCounters {
    fn default() -> Self {
        Self::new()
    }
}
