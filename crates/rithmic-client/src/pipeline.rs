//! Live pipeline task: feeds OrderEvents into BookBuilder, logs BBO/DBO
//! comparisons, emits 100ms snapshots → TimeBarBuilder → BarFeatureComputer → output.
//!
//! BBO instrumentation: on every batch boundary where both book top-of-book
//! and latest BBO exist, logs a `bbo_check` event with raw timestamps from
//! both clock domains (exchange time for DBO, gateway time for BBO) plus
//! price comparisons. No recovery triggers, no DEGRADED exits — just data
//! for offline lag analysis.

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
/// Receives PipelineCommands (OrderEvents + SnapshotComplete) and BboUpdates,
/// feeds them through:
///   BookBuilder → 100ms snapshots → TimeBarBuilder(5s) → BarFeatureComputer → output
///
/// At each batch boundary (flags & 0x80), logs a `bbo_check` event comparing
/// book top-of-book against the latest BBO with raw timestamps from both
/// clock domains. No recovery, no DEGRADED exit.
pub async fn run_pipeline(
    mut command_rx: mpsc::Receiver<PipelineCommand>,
    mut bbo_rx: mpsc::Receiver<BboUpdate>,
    output_tx: mpsc::Sender<FeatureOutput>,
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

    // Wait for initial snapshot before logging bbo_check events.
    let mut initial_snapshot_done: bool = false;

    // Tick size in fixed-point for computing tick deltas
    let tick_size_fixed = crate::adapter::price_to_fixed(tick_size);

    loop {
        tokio::select! {
            bbo = bbo_rx.recv() => {
                match bbo {
                    Some(update) => {
                        latest_bbo = Some(update);
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
                    }

                    PipelineCommand::SnapshotComplete => {
                        if !initial_snapshot_done {
                            initial_snapshot_done = true;
                            health.log("snapshot_complete", serde_json::json!({
                                "initial": true,
                            }));
                            eprintln!("[pipeline] initial snapshot complete — bbo_check logging enabled");
                        } else {
                            health.log("snapshot_complete", serde_json::json!({
                                "initial": false,
                            }));
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

                        // On batch boundary: log bbo_check + emit snapshots
                        if is_batch_end {
                            if initial_snapshot_done {
                                if let Some(ref bbo) = latest_bbo {
                                    if let (Some(bb), Some(ba)) =
                                        (book.best_bid_price(), book.best_ask_price())
                                    {
                                        counters.inc_bbo_validations();

                                        let matches = bb == bbo.bid_price && ba == bbo.ask_price;
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

                                        if !matches {
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
                                            "bbo_ts": bbo.ts_ns,
                                            "match": matches,
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
