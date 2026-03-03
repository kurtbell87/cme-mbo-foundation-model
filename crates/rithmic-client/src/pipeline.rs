//! Live pipeline task: feeds OrderEvents into BookBuilder, validates against
//! BBO, emits 100ms snapshots → TimeBarBuilder → BarFeatureComputer → output.
//!
//! Handles ClearBook commands during sequence gap recovery: resets the book
//! builder and suppresses BBO validation until BOTH conditions are met:
//!   1. At least one incremental DBO batch processed post-recovery
//!   2. At least one fresh BBO received post-recovery
//! This prevents false divergence panics from the tokio::select! race between
//! the BBO and DBO channels — a BBO can arrive before the book has processed
//! enough incremental updates to match it.

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
/// latest BBO using exact i64 comparison. BBO validation is suppressed after
/// ClearBook until the first batch boundary after snapshot rebuild.
pub async fn run_pipeline(
    mut command_rx: mpsc::Receiver<PipelineCommand>,
    mut bbo_rx: mpsc::Receiver<BboUpdate>,
    output_tx: mpsc::Sender<FeatureOutput>,
    counters: MessageCounters,
    instrument_id: u32,
    tick_size: f64,
    dev_mode: bool,
) -> Result<(), RithmicError> {
    let mut book = BookBuilder::new(instrument_id);
    let mut bar_builder = TimeBarBuilder::new(5); // 5-second bars
    let mut feature_computer = BarFeatureComputer::with_tick_size(tick_size as f32);

    let mut latest_bbo: Option<BboUpdate> = None;
    let mut last_snapshot_boundary: u64 = 0;

    // Recovery gating: suppresses BBO validation after ClearBook until
    // SnapshotComplete is received AND at least one fresh BBO arrives.
    let mut snapshot_complete_received: bool = false;
    let mut recovery_bbo_received: bool = false;
    let mut in_recovery: bool = false;

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
                                eprintln!("[pipeline] BBO validation re-enabled (snapshot complete + BBO received)");
                            }
                        }
                    }
                    None => {
                        eprintln!("[pipeline] BBO channel closed");
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
                                if let Some(ref bbo) = latest_bbo {
                                    validate_bbo(&book, bbo, &counters, dev_mode);
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

/// Validate book top-of-book against BBO. Zero tolerance — exact i64 match.
fn validate_bbo(
    book: &BookBuilder,
    bbo: &BboUpdate,
    counters: &MessageCounters,
    dev_mode: bool,
) {
    counters.inc_bbo_validations();

    let book_bid = book.best_bid_price();
    let book_ask = book.best_ask_price();

    // Only validate when both book and BBO have values
    let (Some(bb), Some(ba)) = (book_bid, book_ask) else {
        return;
    };

    if bb != bbo.bid_price || ba != bbo.ask_price {
        counters.inc_bbo_divergences();

        if dev_mode {
            panic!(
                "BBO DIVERGENCE: book bid={} ask={}, BBO bid={} ask={}",
                bb, ba, bbo.bid_price, bbo.ask_price
            );
        } else {
            eprintln!(
                "[pipeline] BBO divergence: book bid={} ask={}, BBO bid={} ask={}",
                bb, ba, bbo.bid_price, bbo.ask_price
            );
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
