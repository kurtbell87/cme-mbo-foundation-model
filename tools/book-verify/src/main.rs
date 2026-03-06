//! Book verification tool: replays Databento MBO through BookBuilder and
//! compares the reconstructed top-10 against exchange MBP-10 snapshots.
//!
//! Usage:
//!   book-verify --mbo path/to/mbo.dbn.zst --mbp10 path/to/mbp10.dbn.zst
//!
//! Compares at every MBP-10 message (which corresponds to a book update event).
//! Reports per-instrument pass/fail with mismatch details.

use std::collections::HashMap;

use book_builder::BookBuilder;
use clap::Parser;
use dbn::decode::{DbnDecoder, DbnMetadata, DecodeRecord};
use dbn::{Mbp10Msg, MboMsg};

const BOOK_DEPTH: usize = 10;
const PRICE_SCALE: f64 = 1e9;

#[derive(Parser)]
#[command(name = "book-verify", about = "Verify BookBuilder against MBP-10 ground truth")]
struct Args {
    /// Path to the MBO .dbn.zst file
    #[arg(long)]
    mbo: String,

    /// Path to the MBP-10 .dbn.zst file
    #[arg(long)]
    mbp10: String,

    /// Max mismatches to print per instrument before suppressing
    #[arg(long, default_value = "20")]
    max_print: usize,
}

/// Snapshot of exchange top-10 from an MBP-10 message.
#[derive(Clone)]
struct Mbp10Snapshot {
    ts_event: u64,
    instrument_id: u32,
    /// (price_i64, size) for top 10 bid levels, descending by price.
    bids: [(i64, u32); BOOK_DEPTH],
    /// (price_i64, size) for top 10 ask levels, ascending by price.
    asks: [(i64, u32); BOOK_DEPTH],
    n_bids: usize,
    n_asks: usize,
}

fn extract_mbp10_snapshot(msg: &Mbp10Msg) -> Mbp10Snapshot {
    let mut bids = [(0i64, 0u32); BOOK_DEPTH];
    let mut asks = [(0i64, 0u32); BOOK_DEPTH];
    let mut n_bids = 0;
    let mut n_asks = 0;

    for (i, level) in msg.levels.iter().enumerate() {
        if i >= BOOK_DEPTH {
            break;
        }
        // Databento uses i64::MAX (UNDEF_PRICE) for empty levels
        if level.bid_px > 0 && level.bid_px < i64::MAX && level.bid_sz > 0 {
            bids[n_bids] = (level.bid_px, level.bid_sz);
            n_bids += 1;
        }
        if level.ask_px > 0 && level.ask_px < i64::MAX && level.ask_sz > 0 {
            asks[n_asks] = (level.ask_px, level.ask_sz);
            n_asks += 1;
        }
    }

    Mbp10Snapshot {
        ts_event: msg.hd.ts_event,
        instrument_id: msg.hd.instrument_id,
        bids,
        asks,
        n_bids,
        n_asks,
    }
}

fn format_price(p: i64) -> String {
    format!("{:.2}", p as f64 / PRICE_SCALE)
}

struct InstrumentStats {
    symbol: String,
    total_checks: u64,
    bid_price_mismatches: u64,
    ask_price_mismatches: u64,
    bid_size_mismatches: u64,
    ask_size_mismatches: u64,
    level_count_mismatches: u64,
    printed_mismatches: usize,
}

fn main() {
    let args = Args::parse();

    // ---------------------------------------------------------------
    // Phase 1: Load MBP-10 snapshots indexed by (instrument_id, ts_event)
    // ---------------------------------------------------------------
    eprintln!("[1/3] Loading MBP-10 snapshots from {}...", args.mbp10);

    let mut mbp10_decoder =
        DbnDecoder::from_zstd_file(&args.mbp10).expect("failed to open MBP-10 file");

    // Extract symbology from MBP-10 metadata
    let mut id_to_symbol: HashMap<u32, String> = HashMap::new();
    {
        let meta = mbp10_decoder.metadata();
        for mapping in &meta.mappings {
            for interval in &mapping.intervals {
                // interval.symbol is the instrument_id as string (stype_out=instrument_id)
                // mapping.raw_symbol is the parent symbol (e.g. "ES.FUT")
                if let Ok(id) = interval.symbol.parse::<u32>() {
                    id_to_symbol.insert(id, mapping.raw_symbol.clone());
                }
            }
        }
    }
    eprintln!("  Symbology: {:?}", id_to_symbol);

    // Index MBP-10 snapshots by (instrument_id, ts_event)
    // Only keep the latest snapshot per instrument (overwrite on same ts)
    let mut mbp10_map: HashMap<(u32, u64), Mbp10Snapshot> = HashMap::new();
    let mut mbp10_count = 0u64;

    while let Some(msg) = mbp10_decoder
        .decode_record::<Mbp10Msg>()
        .expect("MBP-10 decode error")
    {
        let flags = msg.flags.raw();
        // Only use F_LAST snapshots (complete book state)
        if flags & 0x80 != 0 {
            let snap = extract_mbp10_snapshot(msg);
            mbp10_map.insert((snap.instrument_id, snap.ts_event), snap);
            mbp10_count += 1;
        }
    }

    eprintln!(
        "  Loaded {} MBP-10 F_LAST snapshots for {} instruments",
        mbp10_count,
        id_to_symbol.len()
    );

    // ---------------------------------------------------------------
    // Phase 2: Replay MBO through BookBuilder, compare at each F_LAST
    // ---------------------------------------------------------------
    eprintln!("[2/3] Replaying MBO from {} through BookBuilder...", args.mbo);

    let mut mbo_decoder =
        DbnDecoder::from_zstd_file(&args.mbo).expect("failed to open MBO file");

    // Also extract symbology from MBO file to fill any gaps
    {
        let meta = mbo_decoder.metadata();
        for mapping in &meta.mappings {
            for interval in &mapping.intervals {
                if let Ok(id) = interval.symbol.parse::<u32>() {
                    id_to_symbol.entry(id).or_insert_with(|| mapping.raw_symbol.clone());
                }
            }
        }
    }

    let mut builders: HashMap<u32, BookBuilder> = HashMap::new();
    let mut stats: HashMap<u32, InstrumentStats> = HashMap::new();
    let mut total_mbo = 0u64;
    let mut total_checks = 0u64;
    let mut total_matches = 0u64;

    while let Some(msg) = mbo_decoder
        .decode_record::<MboMsg>()
        .expect("MBO decode error")
    {
        total_mbo += 1;

        let id = msg.hd.instrument_id;
        let action = msg.action as u8 as char;
        let side = msg.side as u8 as char;
        let flags = msg.flags.raw();

        // Get or create builder for this instrument
        let builder = builders.entry(id).or_insert_with(|| BookBuilder::new(id));

        // Handle clear action
        if action == 'R' {
            *builder = BookBuilder::new(id);
            if flags & 0x80 != 0 {
                // Nothing to compare after a clear
            }
            continue;
        }

        builder.process_event(
            msg.hd.ts_event,
            msg.order_id,
            id,
            action,
            side,
            msg.price,
            msg.size,
            flags,
        );

        // At F_LAST boundary: compare against MBP-10 if we have a matching snapshot
        if flags & 0x80 != 0 {
            if let Some(expected) = mbp10_map.get(&(id, msg.hd.ts_event)) {
                total_checks += 1;

                let inst_stats = stats.entry(id).or_insert_with(|| InstrumentStats {
                    symbol: id_to_symbol.get(&id).cloned().unwrap_or_else(|| format!("id={}", id)),
                    total_checks: 0,
                    bid_price_mismatches: 0,
                    ask_price_mismatches: 0,
                    bid_size_mismatches: 0,
                    ask_size_mismatches: 0,
                    level_count_mismatches: 0,
                    printed_mismatches: 0,
                });
                inst_stats.total_checks += 1;

                // Get our book's top-10
                let our_bids = builder.bid_levels_raw(); // ascending, best = last
                let our_asks = builder.ask_levels_raw(); // ascending, best = first

                // Compare top-10 bids (descending by price)
                let our_n_bids = our_bids.len().min(BOOK_DEPTH);
                let exp_n_bids = expected.n_bids;

                let mut mismatch = false;
                let mut mismatch_detail = String::new();

                // Compare bid levels
                let check_bids = our_n_bids.min(exp_n_bids);
                for i in 0..check_bids {
                    // Our bids: best is last, so top-i is at len-1-i
                    let our_idx = our_bids.len() - 1 - i;
                    let (our_px, our_sz) = our_bids[our_idx];
                    let (exp_px, exp_sz) = expected.bids[i];

                    if our_px != exp_px {
                        inst_stats.bid_price_mismatches += 1;
                        mismatch = true;
                        mismatch_detail += &format!(
                            "  bid[{}] price: ours={} expected={}\n",
                            i,
                            format_price(our_px),
                            format_price(exp_px)
                        );
                    }
                    if our_sz != exp_sz {
                        inst_stats.bid_size_mismatches += 1;
                        mismatch = true;
                        mismatch_detail += &format!(
                            "  bid[{}] size: ours={} expected={} (px={})\n",
                            i, our_sz, exp_sz,
                            format_price(exp_px)
                        );
                    }
                }

                // Compare ask levels
                let our_n_asks = our_asks.len().min(BOOK_DEPTH);
                let exp_n_asks = expected.n_asks;
                let check_asks = our_n_asks.min(exp_n_asks);

                for i in 0..check_asks {
                    let (our_px, our_sz) = our_asks[i];
                    let (exp_px, exp_sz) = expected.asks[i];

                    if our_px != exp_px {
                        inst_stats.ask_price_mismatches += 1;
                        mismatch = true;
                        mismatch_detail += &format!(
                            "  ask[{}] price: ours={} expected={}\n",
                            i,
                            format_price(our_px),
                            format_price(exp_px)
                        );
                    }
                    if our_sz != exp_sz {
                        inst_stats.ask_size_mismatches += 1;
                        mismatch = true;
                        mismatch_detail += &format!(
                            "  ask[{}] size: ours={} expected={} (px={})\n",
                            i, our_sz, exp_sz,
                            format_price(exp_px)
                        );
                    }
                }

                // Level count mismatch (only if we have fewer levels)
                if our_n_bids < exp_n_bids || our_n_asks < exp_n_asks {
                    inst_stats.level_count_mismatches += 1;
                    mismatch = true;
                    mismatch_detail += &format!(
                        "  level count: our_bids={} exp_bids={} our_asks={} exp_asks={}\n",
                        our_n_bids, exp_n_bids, our_n_asks, exp_n_asks
                    );
                }

                if mismatch {
                    if inst_stats.printed_mismatches < args.max_print {
                        eprintln!(
                            "MISMATCH {} (id={}) ts={}:\n{}",
                            inst_stats.symbol,
                            id,
                            msg.hd.ts_event,
                            mismatch_detail
                        );
                        inst_stats.printed_mismatches += 1;
                    }
                } else {
                    total_matches += 1;
                }
            }
        }

        // Progress
        if total_mbo % 10_000_000 == 0 {
            eprintln!("  processed {}M MBO records, {} checks so far...", total_mbo / 1_000_000, total_checks);
        }
    }

    // ---------------------------------------------------------------
    // Phase 3: Report
    // ---------------------------------------------------------------
    eprintln!("\n[3/3] Results\n");
    eprintln!("Total MBO records: {}", total_mbo);
    eprintln!("Total MBP-10 F_LAST snapshots: {}", mbp10_count);
    eprintln!("Total comparisons: {}", total_checks);
    eprintln!("Total exact matches: {} ({:.2}%)\n",
        total_matches,
        if total_checks > 0 { total_matches as f64 / total_checks as f64 * 100.0 } else { 0.0 }
    );

    let mut all_pass = true;

    for (id, s) in stats.iter() {
        let total_mismatches = s.bid_price_mismatches + s.ask_price_mismatches
            + s.bid_size_mismatches + s.ask_size_mismatches + s.level_count_mismatches;
        let pass = total_mismatches == 0;
        if !pass {
            all_pass = false;
        }

        println!(
            "{} {} (id={}): checks={} | bid_px_err={} ask_px_err={} bid_sz_err={} ask_sz_err={} lvl_ct_err={}",
            if pass { "PASS" } else { "FAIL" },
            s.symbol,
            id,
            s.total_checks,
            s.bid_price_mismatches,
            s.ask_price_mismatches,
            s.bid_size_mismatches,
            s.ask_size_mismatches,
            s.level_count_mismatches,
        );
    }

    if stats.is_empty() {
        println!("WARNING: no comparisons were made (no matching ts_event between MBO F_LAST and MBP-10 F_LAST)");
        std::process::exit(2);
    }

    println!("\n{}", if all_pass { "ALL INSTRUMENTS PASS" } else { "SOME INSTRUMENTS FAILED" });
    std::process::exit(if all_pass { 0 } else { 1 });
}
