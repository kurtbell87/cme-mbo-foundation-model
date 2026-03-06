//! Live pipeline task: feeds OrderEvents into BookBuilder, validates against
//! BBO, emits 100ms snapshots → TimeBarBuilder → BarFeatureComputer → output.
//!
//! Handles ClearBook commands during snapshot recovery: resets the book
//! builder and suppresses BBO validation until BOTH conditions are met:
//!   1. SnapshotComplete received (dispatcher finished replaying 116 messages)
//!   2. At least one fresh BBO received post-clear
//! After re-enable, a warmup period lets the DBO stream catch up with the
//! BBO feed before strict validation begins.
//!
//! BBO instrumentation: on every batch boundary where both book top-of-book
//! and latest BBO exist, logs a `bbo_check` event with raw timestamps from
//! both clock domains (exchange time for DBO, gateway time for BBO) plus
//! price comparisons. Uses a ring buffer of recent BBOs for temporal alignment.
//!
//! Latency instrumentation: tracks four histogram instances measuring
//! exchange→gateway, gateway→local, end-to-end, and feed alignment latencies.
//! Summaries logged every 10s.

use std::time::SystemTime;

use book_builder::BookBuilder;
use bars::TimeBarBuilder;
use bars::BarBuilder;
use common::book::SNAPSHOT_INTERVAL_NS;
use features::{BarFeatureComputer, BarFeatureRow};
use tokio::sync::mpsc;

use crate::counters::MessageCounters;
use crate::dispatcher::PipelineCommand;
use crate::adapter::BboUpdate;
use crate::error::RithmicError;
use crate::health_log::HealthLogger;

/// Output row from the pipeline (timestamp + feature values).
#[derive(Debug, Clone)]
pub struct FeatureOutput {
    pub timestamp: u64,
    pub features: Vec<f64>,
}

// =========================================================================
// LatencyHistogram — zero-alloc, fixed 12-bucket powers-of-2 from <0.5ms
// =========================================================================

/// Bucket boundaries in microseconds: <500, <1000, <2000, ..., <512000, >=512000
const BUCKET_COUNT: usize = 12;
const BUCKET_BOUNDS_US: [u64; BUCKET_COUNT - 1] = [
    500, 1_000, 2_000, 4_000, 8_000, 16_000, 32_000, 64_000, 128_000, 256_000, 512_000,
];

struct LatencyHistogram {
    buckets: [u64; BUCKET_COUNT],
    count: u64,
    sum_us: u64,
}

impl LatencyHistogram {
    const fn new() -> Self {
        Self {
            buckets: [0; BUCKET_COUNT],
            count: 0,
            sum_us: 0,
        }
    }

    /// Record a latency measurement in nanoseconds.
    #[inline]
    fn record_ns(&mut self, ns: u64) {
        let us = ns / 1_000;
        self.count += 1;
        self.sum_us += us;
        for (i, &bound) in BUCKET_BOUNDS_US.iter().enumerate() {
            if us < bound {
                self.buckets[i] += 1;
                return;
            }
        }
        self.buckets[BUCKET_COUNT - 1] += 1;
    }

    /// Compute percentile in microseconds (p50, p95, p99).
    fn percentile(&self, pct: f64) -> u64 {
        if self.count == 0 {
            return 0;
        }
        let target = ((self.count as f64) * pct / 100.0).ceil() as u64;
        let mut cumulative = 0u64;
        for (i, &cnt) in self.buckets.iter().enumerate() {
            cumulative += cnt;
            if cumulative >= target {
                if i < BUCKET_BOUNDS_US.len() {
                    return BUCKET_BOUNDS_US[i];
                } else {
                    return if self.count > 0 { self.sum_us / self.count } else { 0 };
                }
            }
        }
        0
    }

    /// Drain: return (count, p50_us, p95_us, p99_us, mean_us) and reset.
    fn drain(&mut self) -> (u64, u64, u64, u64, u64) {
        let count = self.count;
        if count == 0 {
            return (0, 0, 0, 0, 0);
        }
        let p50 = self.percentile(50.0);
        let p95 = self.percentile(95.0);
        let p99 = self.percentile(99.0);
        let mean = self.sum_us / count;
        self.buckets = [0; BUCKET_COUNT];
        self.count = 0;
        self.sum_us = 0;
        (count, p50, p95, p99, mean)
    }
}

// =========================================================================
// BBO Ring Buffer — fixed [Option<BboUpdate>; 8]
// =========================================================================

const BBO_RING_SIZE: usize = 8;

struct BboRingBuffer {
    buf: [Option<BboUpdate>; BBO_RING_SIZE],
    write_idx: usize,
}

impl BboRingBuffer {
    fn new() -> Self {
        Self {
            buf: Default::default(),
            write_idx: 0,
        }
    }

    fn push(&mut self, bbo: BboUpdate) {
        self.buf[self.write_idx] = Some(bbo);
        self.write_idx = (self.write_idx + 1) % BBO_RING_SIZE;
    }

    /// Find best-match BBO for a DBO event:
    /// 1. Among BBOs with exact price match on both bid and ask, pick closest gateway ts
    /// 2. If none match, pick closest gateway ts overall
    /// Returns (matched_bbo, is_exact_match)
    fn best_match(&self, book_bid: i64, book_ask: i64, dbo_gw_ts: u64) -> Option<(&BboUpdate, bool)> {
        let mut best_exact: Option<(&BboUpdate, u64)> = None;
        let mut best_any: Option<(&BboUpdate, u64)> = None;

        for slot in &self.buf {
            if let Some(bbo) = slot {
                let age = if dbo_gw_ts >= bbo.ts_ns {
                    dbo_gw_ts - bbo.ts_ns
                } else {
                    bbo.ts_ns - dbo_gw_ts
                };

                if best_any.is_none() || age < best_any.unwrap().1 {
                    best_any = Some((bbo, age));
                }

                if bbo.bid_price == book_bid && bbo.ask_price == book_ask {
                    if best_exact.is_none() || age < best_exact.unwrap().1 {
                        best_exact = Some((bbo, age));
                    }
                }
            }
        }

        if let Some((bbo, _)) = best_exact {
            Some((bbo, true))
        } else {
            best_any.map(|(bbo, _)| (bbo, false))
        }
    }

}

// =========================================================================
// Pipeline
// =========================================================================

/// Run the pipeline task.
///
/// Receives PipelineCommands (OrderEvents + ClearBook signals) and BboUpdates,
/// feeds them through:
///   BookBuilder → 100ms snapshots → TimeBarBuilder(5s) → BarFeatureComputer → output
///
/// At each batch boundary (flags & 0x80), logs a `bbo_check` event comparing
/// book top-of-book against the best-match BBO from the ring buffer.
/// Logs latency histograms every 10s.
const MAX_POST_INITIAL_RECOVERIES: u32 = 10;

pub async fn run_pipeline(
    mut command_rx: mpsc::Receiver<PipelineCommand>,
    mut bbo_rx: mpsc::Receiver<BboUpdate>,
    output_tx: mpsc::Sender<FeatureOutput>,
    _recovery_tx: mpsc::Sender<()>,
    health: HealthLogger,
    counters: MessageCounters,
    instrument_id: u32,
    tick_size: f64,
) -> Result<(), RithmicError> {
    let mut book = BookBuilder::new(instrument_id);
    let mut bar_builder = TimeBarBuilder::new(5); // 5-second bars
    let mut feature_computer = BarFeatureComputer::with_tick_size(tick_size as f32);

    let mut bbo_ring = BboRingBuffer::new();
    // Tick size in fixed-point for computing tick deltas
    let tick_size_fixed = crate::adapter::price_to_fixed(tick_size);
    let mut last_snapshot_boundary: u64 = 0;

    // Recovery gating: suppresses BBO validation after ClearBook until
    // SnapshotComplete is received AND at least one fresh BBO arrives.
    // After re-enable, a warmup period lets the DBO stream catch up with
    // the BBO feed before strict validation begins.
    //
    // Start in recovery so the initial snapshot (116+161) must complete
    // before BBO validation is enabled.  The dispatcher starts in
    // LoadingSnapshot state and sends SnapshotComplete after 161 — no
    // ClearBook is sent at startup, so we initialise the flags manually.
    let mut snapshot_complete_received: bool = false;
    let mut recovery_bbo_received: bool = false;
    let mut in_recovery: bool = true;
    // Tracks whether the initial startup snapshot has completed.
    // Post-initial recoveries (triggered by divergences) are counted separately
    // for degradation detection.
    let mut initial_snapshot_done: bool = false;
    let mut post_initial_recoveries: u32 = 0;
    // Latency histograms
    let mut hist_exchange_to_gateway = LatencyHistogram::new();
    let mut hist_gateway_to_local = LatencyHistogram::new();
    let mut hist_end_to_end = LatencyHistogram::new();
    let mut hist_feed_alignment = LatencyHistogram::new();

    // Periodic summary timer
    let mut summary_interval = tokio::time::interval(std::time::Duration::from_secs(10));
    summary_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            // Drain BBO updates (non-blocking when events are available)
            bbo = bbo_rx.recv() => {
                match bbo {
                    Some(update) => {
                        // Record gateway→local latency for BBO
                        if update.receive_wall_ns > 0 && update.ts_ns > 0 {
                            if update.receive_wall_ns > update.ts_ns {
                                hist_gateway_to_local.record_ns(update.receive_wall_ns - update.ts_ns);
                            }
                        }
                        bbo_ring.push(update);
                        if in_recovery {
                            recovery_bbo_received = true;
                            // Re-enable if snapshot already completed
                            if snapshot_complete_received {
                                in_recovery = false;
                                eprintln!("[pipeline] BBO validation re-enabled (snapshot complete + BBO received)");
                            }
                        }
                    }
                    None => {
                        eprintln!("[pipeline] BBO channel closed, shutting down");
                        return Ok(());
                    }
                }
            }

            cmd = command_rx.recv() => {
                let cmd = match cmd {
                    Some(c) => c,
                    None => {
                        eprintln!("[pipeline] command channel closed, shutting down");
                        return Ok(());
                    }
                };

                match cmd {
                    PipelineCommand::ClearBook => {
                        eprintln!("[pipeline] ClearBook received — resetting book builder");
                        book = BookBuilder::new(instrument_id);
                        last_snapshot_boundary = 0;
                        in_recovery = true;
                        snapshot_complete_received = false;
                        recovery_bbo_received = false;
                        // Don't reset bar_builder/feature_computer — they accumulate
                        // across the session and a brief gap shouldn't lose bar history.
                    }

                    PipelineCommand::SnapshotComplete => {
                        snapshot_complete_received = true;

                        if !initial_snapshot_done {
                            initial_snapshot_done = true;
                            health.log("snapshot_complete", serde_json::json!({
                                "initial": true,
                                "post_initial_recoveries": 0,
                            }));
                        } else {
                            post_initial_recoveries += 1;
                            health.log("snapshot_complete", serde_json::json!({
                                "initial": false,
                                "post_initial_recoveries": post_initial_recoveries,
                            }));
                            if post_initial_recoveries >= MAX_POST_INITIAL_RECOVERIES {
                                let msg = format!(
                                    "{post_initial_recoveries} divergence-triggered recoveries — \
                                     book cannot stabilize, exiting"
                                );
                                eprintln!("[pipeline] DEGRADED: {msg}");
                                health.log("degraded", serde_json::json!({
                                    "reason": msg,
                                    "post_initial_recoveries": post_initial_recoveries,
                                }));
                                return Err(RithmicError::BookDegraded(msg));
                            }
                        }

                        // Re-enable validation only when snapshot is done AND
                        // a fresh BBO has arrived post-clear.
                        if in_recovery && recovery_bbo_received {
                            in_recovery = false;
                            eprintln!("[pipeline] BBO validation re-enabled (snapshot complete + BBO received)");
                        }
                    }

                    PipelineCommand::Event(event) => {
                        let is_batch_end = event.flags & 0x80 != 0;
                        let ts_event = event.ts_event;
                        let gateway_ts = event.gateway_ts_ns;
                        let receive_wall = event.receive_wall_ns;

                        // Record latency: exchange→gateway (DBO only, when both timestamps available)
                        if event.action != 'T' && ts_event > 0 && gateway_ts > 0 && gateway_ts > ts_event {
                            hist_exchange_to_gateway.record_ns(gateway_ts - ts_event);
                        }

                        // Record latency: gateway→local
                        if receive_wall > 0 && gateway_ts > 0 && receive_wall > gateway_ts {
                            hist_gateway_to_local.record_ns(receive_wall - gateway_ts);
                        }

                        // Record latency: end-to-end (exchange→now)
                        if ts_event > 0 && receive_wall > 0 {
                            let now_ns = SystemTime::now()
                                .duration_since(SystemTime::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_nanos() as u64;
                            if now_ns > ts_event {
                                hist_end_to_end.record_ns(now_ns - ts_event);
                            }
                        }

                        // Feed into book builder
                        book.process_event(
                            event.ts_event,
                            event.order_id,
                            event.instrument_id,
                            event.action,
                            event.side,
                            event.price,
                            event.size,
                            event.flags,
                        );

                        // On batch boundary: validate BBO + emit snapshots
                        if is_batch_end {
                            if initial_snapshot_done {
                                if let (Some(bb), Some(ba)) =
                                    (book.best_bid_price(), book.best_ask_price())
                                {
                                    if let Some((bbo, is_exact)) = bbo_ring.best_match(bb, ba, gateway_ts) {
                                        counters.inc_bbo_validations();

                                        let feed_age_ns = if gateway_ts >= bbo.ts_ns {
                                            gateway_ts - bbo.ts_ns
                                        } else {
                                            bbo.ts_ns - gateway_ts
                                        };

                                        hist_feed_alignment.record_ns(feed_age_ns);

                                        let bid_delta = if tick_size_fixed != 0 {
                                            (bb - bbo.bid_price) / tick_size_fixed
                                        } else {
                                            bb - bbo.bid_price
                                        };
                                        let ask_delta = if tick_size_fixed != 0 {
                                            (ba - bbo.ask_price) / tick_size_fixed
                                        } else {
                                            ba - bbo.ask_price
                                        };

                                        if !is_exact {
                                            counters.inc_bbo_divergences();
                                        }

                                        health.log("bbo_check", serde_json::json!({
                                            "book_bid": bb,
                                            "book_ask": ba,
                                            "bbo_bid": bbo.bid_price,
                                            "bbo_ask": bbo.ask_price,
                                            "bbo_bid_size": bbo.bid_size,
                                            "bbo_ask_size": bbo.ask_size,
                                            "dbo_ts": ts_event,
                                            "dbo_gw_ts": gateway_ts,
                                            "bbo_ts": bbo.ts_ns,
                                            "match": is_exact,
                                            "feed_age_ns": feed_age_ns,
                                            "bid_delta_ticks": bid_delta,
                                            "ask_delta_ticks": ask_delta,
                                        }));
                                    }
                                }
                            }

                            // Emit 100ms snapshots
                            if last_snapshot_boundary == 0 {
                                last_snapshot_boundary = (ts_event / SNAPSHOT_INTERVAL_NS) * SNAPSHOT_INTERVAL_NS;
                            }

                            let next_boundary = last_snapshot_boundary + SNAPSHOT_INTERVAL_NS;
                            if ts_event >= next_boundary {
                                let snapshots = book.emit_snapshots(last_snapshot_boundary, ts_event);
                                last_snapshot_boundary = (ts_event / SNAPSHOT_INTERVAL_NS) * SNAPSHOT_INTERVAL_NS;

                                for snap in &snapshots {
                                    if let Some(bar) = bar_builder.on_snapshot(snap) {
                                        let row = feature_computer.update(&bar);
                                        let output = FeatureOutput {
                                            timestamp: bar.close_ts,
                                            features: extract_features(&row),
                                        };
                                        if output_tx.send(output).await.is_err() {
                                            return Err(RithmicError::Channel(
                                                "output_tx closed".into(),
                                            ));
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            _ = summary_interval.tick() => {
                // Drain all histograms and log summaries
                let (e2e_n, e2e_p50, e2e_p95, e2e_p99, e2e_mean) = hist_end_to_end.drain();
                let (exg_n, exg_p50, exg_p95, exg_p99, _) = hist_exchange_to_gateway.drain();
                let (g2l_n, g2l_p50, g2l_p95, g2l_p99, _) = hist_gateway_to_local.drain();
                let (fa_n, fa_p50, fa_p95, fa_p99, _) = hist_feed_alignment.drain();

                if e2e_n > 0 {
                    health.log("latency_end_to_end", serde_json::json!({
                        "n": e2e_n,
                        "p50_us": e2e_p50,
                        "p95_us": e2e_p95,
                        "p99_us": e2e_p99,
                        "mean_us": e2e_mean,
                    }));
                    eprintln!(
                        "[latency] e2e: n={} p50={}us p95={}us p99={}us | exg→gw: n={} p50={}us p95={}us | gw→local: n={} p50={}us p95={}us | feed_align: n={} p50={}us p95={}us",
                        e2e_n, e2e_p50, e2e_p95, e2e_p99,
                        exg_n, exg_p50, exg_p95,
                        g2l_n, g2l_p50, g2l_p95,
                        fa_n, fa_p50, fa_p95,
                    );
                }

                if exg_n > 0 {
                    health.log("latency_exchange_to_gateway", serde_json::json!({
                        "n": exg_n, "p50_us": exg_p50, "p95_us": exg_p95, "p99_us": exg_p99,
                    }));
                }
                if g2l_n > 0 {
                    health.log("latency_gateway_to_local", serde_json::json!({
                        "n": g2l_n, "p50_us": g2l_p50, "p95_us": g2l_p95, "p99_us": g2l_p99,
                    }));
                }
                if fa_n > 0 {
                    health.log("latency_feed_alignment", serde_json::json!({
                        "n": fa_n, "p50_us": fa_p50, "p95_us": fa_p95, "p99_us": fa_p99,
                    }));
                }
            }
        }
    }
}

/// Extract the 20 core features from a BarFeatureRow as f64.
fn extract_features(row: &BarFeatureRow) -> Vec<f64> {
    vec![
        row.book_imbalance_1 as f64,
        row.book_imbalance_3 as f64,
        row.book_imbalance_5 as f64,
        row.book_imbalance_10 as f64,
        row.weighted_imbalance as f64,
        row.spread as f64,
        row.net_volume as f64,
        row.volume_imbalance as f64,
        row.trade_count as f64,
        row.avg_trade_size as f64,
        row.vwap_distance as f64,
        row.return_1 as f64,
        row.return_5 as f64,
        row.volatility_20 as f64,
        row.volatility_50 as f64,
        row.momentum as f64,
        row.high_low_range_20 as f64,
        row.volume_surprise as f64,
        row.cancel_add_ratio as f64,
        row.message_rate as f64,
    ]
}
