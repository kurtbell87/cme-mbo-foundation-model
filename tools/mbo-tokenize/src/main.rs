//! CLI tool: tokenize .dbn.zst MBO files into binary token sequences.
//!
//! Reads Databento MBO events, converts them to discrete token sequences
//! via `MboTokenizer`, and writes a flat binary file of u16 token IDs
//! suitable for PyTorch `torch.from_file()` or `numpy.fromfile(dtype=np.uint16)`.
//!
//! Output format:
//!   [BOS] [event tokens...] [EOS]
//!   Written as little-endian u16 values.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use anyhow::{bail, Context, Result};
use clap::Parser;
use dbn::decode::{DbnDecoder, DecodeRecord};
use dbn::MboMsg;
use rustc_hash::FxHashMap;

use mbo_tokenizer::{
    display_tokens, MboTokenizer, BOS, EOS, VOCAB_SIZE,
};

#[derive(Parser, Debug)]
#[command(name = "mbo-tokenize")]
#[command(about = "Tokenize .dbn.zst MBO files into binary token sequences for transformer training")]
struct Args {
    /// Input .dbn.zst file(s). Multiple files are concatenated with BOS/EOS framing.
    #[arg(required = true)]
    inputs: Vec<String>,

    /// Output binary file path (u16 little-endian tokens).
    #[arg(long, short)]
    output: String,

    /// Instrument ID. If omitted, auto-detects the most active instrument.
    #[arg(long)]
    instrument_id: Option<u32>,

    /// Tick size for the instrument (default: 0.25 for MES).
    #[arg(long, default_value = "0.25")]
    tick_size: f64,

    /// Print the first N tokens in human-readable form for inspection.
    #[arg(long, default_value = "0")]
    preview: usize,

    /// Also write a .txt sidecar with human-readable token names.
    #[arg(long)]
    text: bool,
}

fn main() -> Result<()> {
    let args = Args::parse();

    // Validate inputs exist
    for input in &args.inputs {
        if !Path::new(input).exists() {
            bail!("Input file not found: {input}");
        }
    }

    // Auto-detect instrument ID from first file if needed
    let instrument_id = match args.instrument_id {
        Some(id) => id,
        None => {
            eprintln!("[0] Auto-detecting instrument ID from {}...", args.inputs[0]);
            detect_instrument_id(&args.inputs[0])?
        }
    };

    eprintln!("[1] Tokenizing {} file(s) for instrument_id={instrument_id}, tick_size={}",
        args.inputs.len(), args.tick_size);
    eprintln!("    Vocabulary size: {VOCAB_SIZE}");

    let mut all_tokens: Vec<u16> = Vec::new();
    let mut tokenizer = MboTokenizer::new(instrument_id, args.tick_size);

    for (file_idx, input) in args.inputs.iter().enumerate() {
        eprintln!("[{}/{}] Processing {}...",
            file_idx + 1, args.inputs.len(), input);

        all_tokens.push(BOS);

        let mut decoder = DbnDecoder::from_zstd_file(input)
            .map_err(|e| anyhow::anyhow!("DBN decode error: {e}"))?;

        let mut file_records = 0u64;
        let file_tokens_start = all_tokens.len();

        while let Some(msg) = decoder
            .decode_record::<MboMsg>()
            .map_err(|e| anyhow::anyhow!("DBN decode error: {e}"))?
        {
            file_records += 1;

            let action = msg.action as u8 as char;
            let side = msg.side as u8 as char;
            let flags = msg.flags.raw();

            tokenizer.feed_event(
                msg.hd.ts_event,
                msg.order_id,
                msg.hd.instrument_id,
                action,
                side,
                msg.price,
                msg.size,
                flags,
                &mut all_tokens,
            );

            if file_records % 5_000_000 == 0 {
                eprintln!("    {file_records} records processed...");
            }
        }

        let file_tokens = all_tokens.len() - file_tokens_start;
        all_tokens.push(EOS);

        eprintln!("    {file_records} records → {file_tokens} tokens");
    }

    // Print stats
    eprintln!("\n{}", tokenizer.stats());
    eprintln!("Total tokens (with BOS/EOS): {}", all_tokens.len());

    // Preview
    if args.preview > 0 {
        let n = args.preview.min(all_tokens.len());
        eprintln!("\nFirst {n} tokens:");
        eprintln!("{}", display_tokens(&all_tokens[..n]));
    }

    // Write binary output
    eprintln!("[2] Writing {} tokens to {}...", all_tokens.len(), args.output);
    write_binary(&args.output, &all_tokens)?;

    // Write text sidecar if requested
    if args.text {
        let text_path = format!("{}.txt", args.output);
        eprintln!("[3] Writing text sidecar to {text_path}...");
        write_text(&text_path, &all_tokens)?;
    }

    // Write metadata
    let meta_path = format!("{}.meta.json", args.output);
    write_metadata(&meta_path, &args, instrument_id, &all_tokens, tokenizer.stats())?;

    eprintln!("Done.");
    Ok(())
}

/// Write tokens as little-endian u16 binary.
fn write_binary(path: &str, tokens: &[u16]) -> Result<()> {
    let file = File::create(path).context("Failed to create output file")?;
    let mut writer = BufWriter::new(file);

    for &token in tokens {
        writer.write_all(&token.to_le_bytes())?;
    }
    writer.flush()?;

    let bytes = tokens.len() * 2;
    eprintln!("    {} bytes ({:.1} MB)", bytes, bytes as f64 / 1_048_576.0);
    Ok(())
}

/// Write tokens as human-readable text (one event per line).
fn write_text(path: &str, tokens: &[u16]) -> Result<()> {
    let file = File::create(path).context("Failed to create text file")?;
    let mut writer = BufWriter::new(file);

    for &token in tokens {
        let name = mbo_tokenizer::token_name(token);
        write!(writer, "{name} ")?;
        // Newline after COMMIT, EOS, BOS for readability
        if token == mbo_tokenizer::COMMIT
            || token == mbo_tokenizer::EOS
            || token == mbo_tokenizer::BOS
        {
            writeln!(writer)?;
        }
    }
    writer.flush()?;
    Ok(())
}

/// Write metadata JSON sidecar.
fn write_metadata(
    path: &str,
    args: &Args,
    instrument_id: u32,
    tokens: &[u16],
    stats: &mbo_tokenizer::TokenizerStats,
) -> Result<()> {
    let file = File::create(path).context("Failed to create metadata file")?;
    let mut w = BufWriter::new(file);
    writeln!(w, "{{")?;
    writeln!(w, "  \"vocab_size\": {VOCAB_SIZE},")?;
    writeln!(w, "  \"total_tokens\": {},", tokens.len())?;
    writeln!(w, "  \"instrument_id\": {instrument_id},")?;
    writeln!(w, "  \"tick_size\": {},", args.tick_size)?;
    writeln!(w, "  \"num_files\": {},", args.inputs.len())?;
    writeln!(w, "  \"events_processed\": {},", stats.events_processed)?;
    writeln!(w, "  \"events_skipped_instrument\": {},", stats.events_skipped_instrument)?;
    writeln!(w, "  \"commits\": {},", stats.commits)?;
    writeln!(w, "  \"price_in_range_pct\": {:.2},", stats.price_in_range_pct())?;
    writeln!(w, "  \"price_far_neg\": {},", stats.price_far_neg)?;
    writeln!(w, "  \"price_far_pos\": {},", stats.price_far_pos)?;
    writeln!(w, "  \"price_no_ref\": {},", stats.price_no_ref)?;
    writeln!(w, "  \"action_counts\": {{\"add\": {}, \"cancel\": {}, \"modify\": {}, \"trade\": {}, \"fill\": {}, \"clear\": {}}}",
        stats.action_counts[0], stats.action_counts[1], stats.action_counts[2],
        stats.action_counts[3], stats.action_counts[4], stats.action_counts[5])?;
    writeln!(w, "}}")?;
    w.flush()?;
    Ok(())
}

/// Auto-detect the most active instrument ID in a .dbn.zst file.
fn detect_instrument_id(path: &str) -> Result<u32> {
    let mut decoder = DbnDecoder::from_zstd_file(path)
        .map_err(|e| anyhow::anyhow!("DBN decode error: {e}"))?;

    let mut counts: FxHashMap<u32, u64> = FxHashMap::default();
    let mut n = 0u64;

    while let Some(msg) = decoder
        .decode_record::<MboMsg>()
        .map_err(|e| anyhow::anyhow!("DBN decode error: {e}"))?
    {
        *counts.entry(msg.hd.instrument_id).or_default() += 1;
        n += 1;
        if n % 5_000_000 == 0 {
            eprintln!("    scanned {n} records...");
        }
    }

    let (&best_id, &best_count) = counts
        .iter()
        .max_by_key(|(_, count)| *count)
        .context("No records in file")?;

    eprintln!("    {} instruments found. Most active: id={best_id} ({best_count} records)",
        counts.len());
    for (&id, &count) in counts.iter() {
        eprintln!("      instrument_id={id}: {count} records");
    }

    Ok(best_id)
}
