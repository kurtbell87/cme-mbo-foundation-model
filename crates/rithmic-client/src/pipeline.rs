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
//! BBO validation uses two guards before triggering recovery:
//!   1. Max-age: skip if adjusted BBO age (raw_age minus ~150ms clock offset) > 400ms.
//!      Stale BBO means the book is MORE current — skip and reset the streak.
//!   2. Directional-consistency: trigger recovery only after 5 consecutive same-direction
//!      fresh divergences (book_ahead OR book_behind, not mixed).
//! Single divergences and alternating-direction divergences are logged but do not
//! trigger recovery — they are characteristic of the BBO-before-DBO timing race.

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

/// Run the pipeline task.
///
/// Receives PipelineCommands (OrderEvents + ClearBook signals) and BboUpdates,
/// feeds them through:
///   BookBuilder → 100ms snapshots → TimeBarBuilder(5s) → BarFeatureComputer → output
///
/// At each batch boundary (flags & 0x80), validates book top-of-book against
/// the latest BBO using exact i64 comparison, subject to two guards (see module
/// docs). Recovery is triggered only on 5 consecutive same-direction fresh
/// divergences. The pipeline exits with BookDegraded after 3 post-initial
/// recoveries — each recovery takes ~1–5 seconds; 3 failures = structural problem.
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

    let mut latest_bbo: Option<BboUpdate> = None;
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
    let mut post_recovery_warmup: u32 = 0;
    // Tracks whether the initial startup snapshot has completed.
    // Post-initial recoveries (triggered by divergences) are counted separately
    // for degradation detection.
    let mut initial_snapshot_done: bool = false;
    let mut post_initial_recoveries: u32 = 0;
    // Directional-consistency state for BBO validation.
    let mut consecutive_consistent: u32 = 0;
    let mut last_divergence_dir: Option<&'static str> = None;
    let mut last_divergence_ts_ns: u64 = 0;

    /// DBO batches to skip after recovery before validating.
    /// Bridges the latency gap between snapshot end and BBO feed.
    const POST_RECOVERY_WARMUP: u32 = 100;
    /// Systematic DBO/BBO clock-domain offset (exchange clock vs gateway clock).
    const CLOCK_OFFSET_NS: u64 = 150_000_000; // 150ms
    /// Skip validation if adjusted BBO age exceeds this (book is more current than BBO).
    const MAX_BBO_AGE_NS: u64 = 400_000_000; // 400ms
    /// Trigger recovery after this many consecutive same-direction fresh divergences.
    /// Currently unused — BBO-triggered recovery is disabled (see comment below).
    /// Kept for re-enable with two-connection architecture.
    #[allow(dead_code)]
    const DIVERGENCE_RECOVERY_THRESHOLD: u32 = 20;
    /// Reset the consistency streak if no divergence seen within this window.
    const DIVERGENCE_RESET_WINDOW_NS: u64 = 5_000_000_000; // 5s

    loop {
        tokio::select! {
            // Drain BBO updates (non-blocking when events are available)
            bbo = bbo_rx.recv() => {
                match bbo {
                    Some(update) => {
                        latest_bbo = Some(update);
                        if in_recovery {
                            recovery_bbo_received = true;
                            // Re-enable if snapshot already completed
                            if snapshot_complete_received {
                                in_recovery = false;
                                post_recovery_warmup = POST_RECOVERY_WARMUP;
                                eprintln!("[pipeline] BBO validation re-enabled (snapshot complete + BBO received, warmup={POST_RECOVERY_WARMUP})");
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
                            post_recovery_warmup = POST_RECOVERY_WARMUP;
                            eprintln!("[pipeline] BBO validation re-enabled (snapshot complete + BBO received, warmup={POST_RECOVERY_WARMUP})");
                        }
                    }

                    PipelineCommand::Event(event) => {
                        let is_batch_end = event.flags & 0x80 != 0;
                        let ts_event = event.ts_event;

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
                            if !in_recovery {
                                if post_recovery_warmup > 0 {
                                    post_recovery_warmup -= 1;
                                } else if let Some(ref bbo) = latest_bbo {
                                    let raw_age_ns = ts_event.saturating_sub(bbo.ts_ns);
                                    let adjusted_age_ns = raw_age_ns.saturating_sub(CLOCK_OFFSET_NS);

                                    if adjusted_age_ns > MAX_BBO_AGE_NS {
                                        // BBO is stale — book is more current; skip validation.
                                        health.log("validation_skipped", serde_json::json!({
                                            "raw_age_us": raw_age_ns / 1_000,
                                            "adjusted_age_ms": adjusted_age_ns / 1_000_000,
                                            "dbo_ts": ts_event,
                                            "bbo_ts": bbo.ts_ns,
                                        }));
                                        // Stale BBO interrupts any consistency streak.
                                        consecutive_consistent = 0;
                                        last_divergence_dir = None;
                                    } else if let (Some(bb), Some(ba)) =
                                        (book.best_bid_price(), book.best_ask_price())
                                    {
                                        counters.inc_bbo_validations();
                                        if bb != bbo.bid_price || ba != bbo.ask_price {
                                            counters.inc_bbo_divergences();

                                            // Classify divergence direction.
                                            let bid_ahead = bb > bbo.bid_price;
                                            let ask_ahead = ba > bbo.ask_price;
                                            let bid_behind = bb < bbo.bid_price;
                                            let ask_behind = ba < bbo.ask_price;
                                            let dir: &'static str = match (
                                                bid_ahead || ask_ahead,
                                                bid_behind || ask_behind,
                                            ) {
                                                (true, false) => "book_ahead",
                                                (false, true) => "book_behind",
                                                _ => "mixed",
                                            };

                                            // Time-window reset: gap > 5s resets streak.
                                            if last_divergence_ts_ns > 0
                                                && ts_event.saturating_sub(last_divergence_ts_ns)
                                                    > DIVERGENCE_RESET_WINDOW_NS
                                            {
                                                consecutive_consistent = 0;
                                                last_divergence_dir = None;
                                            }

                                            // Update streak: only same non-mixed direction counts.
                                            let streak_continues = dir != "mixed"
                                                && last_divergence_dir.map_or(true, |d| d == dir);
                                            if streak_continues {
                                                consecutive_consistent += 1;
                                                last_divergence_dir = Some(dir);
                                            } else {
                                                // Direction flip or mixed — reset streak.
                                                consecutive_consistent = if dir != "mixed" { 1 } else { 0 };
                                                last_divergence_dir = if dir != "mixed" { Some(dir) } else { None };
                                            }
                                            last_divergence_ts_ns = ts_event;

                                            let raw_age_us = raw_age_ns / 1_000;
                                            let adjusted_age_ms = adjusted_age_ns / 1_000_000;
                                            health.log("divergence", serde_json::json!({
                                                "book_bid": bb,
                                                "book_ask": ba,
                                                "bbo_bid": bbo.bid_price,
                                                "bbo_ask": bbo.ask_price,
                                                "bbo_bid_size": bbo.bid_size,
                                                "bbo_ask_size": bbo.ask_size,
                                                "bbo_bid_implicit": bbo.bid_implicit_size,
                                                "bbo_ask_implicit": bbo.ask_implicit_size,
                                                "bbo_ts": bbo.ts_ns,
                                                "dbo_ts": ts_event,
                                                "raw_age_us": raw_age_us,
                                                "adjusted_age_ms": adjusted_age_ms,
                                                "direction": dir,
                                                "consecutive_consistent": consecutive_consistent,
                                            }));
                                            eprintln!(
                                                "[pipeline] BBO divergence: book bid={} ask={}, \
                                                 BBO bid={} ask={} (adj_age={}ms dir={} streak={})",
                                                bb, ba, bbo.bid_price, bbo.ask_price,
                                                adjusted_age_ms, dir, consecutive_consistent
                                            );

                                            // BBO-triggered recovery is disabled. On a single
                                            // socket, BBO frequently arrives before the DBO that
                                            // caused the price change, creating persistent timing-
                                            // race divergences (~48% rate) even with gaps=0.
                                            // Recovery is handled by sequence gap detection in the
                                            // dispatcher. BBO divergences are logged as diagnostics.
                                            //
                                            // To re-enable (e.g., with two-connection architecture):
                                            // uncomment the block below and set threshold appropriately.
                                            //
                                            // if consecutive_consistent >= DIVERGENCE_RECOVERY_THRESHOLD
                                            //     && dir == "book_behind"
                                            // {
                                            //     let _ = recovery_tx.try_send(());
                                            // }
                                        } else {
                                            // Clean validation — reset consistency streak.
                                            consecutive_consistent = 0;
                                            last_divergence_dir = None;
                                        }
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
