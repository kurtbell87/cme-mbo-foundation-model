use clap::Parser;
use std::path::Path;
use std::process;
use std::time::Instant;

use parity_test::{
    compare_features, get_instrument_id, load_reference_parquet, match_day_files,
    run_rust_pipeline, FEATURE_NAMES,
};

/// Validate feature parity between C++ reference and Rust bar pipelines.
///
/// Compares the 20 model features produced by each pipeline and reports
/// per-feature max absolute deviation, bar count mismatches, and overall
/// pass/fail status.
#[derive(Parser)]
#[command(name = "parity-test")]
struct Cli {
    /// Path to reference Parquet directory (C++ pipeline output)
    #[arg(long)]
    reference: String,

    /// Path to Databento .dbn.zst data directory
    #[arg(long)]
    data: String,

    /// Filter to a single trading day (YYYYMMDD format)
    #[arg(long)]
    day: Option<String>,

    /// Max absolute deviation tolerance
    #[arg(long, default_value = "1e-5")]
    tolerance: f64,

    /// Override instrument ID (default: auto-detect from contract rollover table)
    #[arg(long)]
    instrument_id: Option<u32>,
}

fn main() {
    let cli = Cli::parse();

    let ref_path = Path::new(&cli.reference);
    if !ref_path.exists() {
        eprintln!(
            "Error: reference parquet directory not found: {}",
            cli.reference
        );
        process::exit(1);
    }

    let data_path = Path::new(&cli.data);
    if !data_path.exists() {
        eprintln!("Error: data directory not found: {}", cli.data);
        process::exit(1);
    }

    let pairs = match match_day_files(ref_path, data_path) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Error scanning directories: {}", e);
            process::exit(1);
        }
    };

    // Apply day filter if specified
    let pairs: Vec<_> = if let Some(ref day) = cli.day {
        pairs.into_iter().filter(|p| p.date == *day).collect()
    } else {
        pairs
    };

    if pairs.is_empty() {
        eprintln!("No matched day files found.");
        if let Some(ref day) = cli.day {
            eprintln!("  (filtered to --day {})", day);
        }
        process::exit(1);
    }

    eprintln!(
        "Parity test: {} days, tolerance={:.0e}",
        pairs.len(),
        cli.tolerance
    );

    let total_start = Instant::now();
    let mut days_passed = 0usize;
    let mut days_failed = 0usize;
    let mut days_error = 0usize;
    let mut failed_days: Vec<String> = Vec::new();
    let mut feature_fail_counts = [0u32; 20];

    for (i, pair) in pairs.iter().enumerate() {
        let day_start = Instant::now();
        eprint!(
            "[{}/{}] {} ... ",
            i + 1,
            pairs.len(),
            pair.date
        );

        // Load reference
        let reference = match load_reference_parquet(&pair.reference_path) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("ERROR loading reference: {}", e);
                days_error += 1;
                failed_days.push(format!("{} (ref load error)", pair.date));
                continue;
            }
        };

        // Determine instrument_id from contract table or CLI override
        let inst_id = cli.instrument_id.unwrap_or_else(|| get_instrument_id(&pair.date));

        // Run Rust pipeline
        let rust = match run_rust_pipeline(&pair.dbn_path, inst_id, &pair.date) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("ERROR running pipeline: {}", e);
                days_error += 1;
                failed_days.push(format!("{} (pipeline error)", pair.date));
                continue;
            }
        };

        // Compare
        let result = compare_features(&rust, &reference, cli.tolerance);
        let elapsed = day_start.elapsed();

        if result.bar_count_rust != result.bar_count_ref {
            eprintln!(
                "FAIL  bars: rust={} ref={}  ({:.1}s)",
                result.bar_count_rust, result.bar_count_ref, elapsed.as_secs_f64()
            );
            days_failed += 1;
            failed_days.push(format!("{} (bar count {} vs {})", pair.date, result.bar_count_rust, result.bar_count_ref));
        } else if result.passed {
            eprintln!(
                "PASS  bars={}  ({:.1}s)",
                result.bar_count_rust, elapsed.as_secs_f64()
            );
            days_passed += 1;
        } else {
            let failing: Vec<&str> = result
                .per_feature
                .iter()
                .filter(|f| !f.passed)
                .map(|f| f.name.as_str())
                .collect();
            eprintln!(
                "FAIL  bars={}  failing=[{}]  ({:.1}s)",
                result.bar_count_rust,
                failing.join(", "),
                elapsed.as_secs_f64()
            );
            days_failed += 1;
            failed_days.push(format!("{} (features: {})", pair.date, failing.join(", ")));

            for feat in &result.per_feature {
                if !feat.passed {
                    if let Some(idx) = FEATURE_NAMES.iter().position(|&n| n == feat.name) {
                        feature_fail_counts[idx] += 1;
                    }
                }
            }
        }
    }

    let total_elapsed = total_start.elapsed();

    // Summary
    println!();
    println!("=== Parity Test Summary ===");
    println!(
        "Days: {} total, {} passed, {} failed, {} errors",
        pairs.len(),
        days_passed,
        days_failed,
        days_error
    );
    println!("Tolerance: {:.0e}", cli.tolerance);
    println!("Elapsed: {:.1}s ({:.1}s/day avg)", total_elapsed.as_secs_f64(), total_elapsed.as_secs_f64() / pairs.len() as f64);

    if !failed_days.is_empty() {
        println!();
        println!("Failed days:");
        for d in &failed_days {
            println!("  {}", d);
        }
    }

    // Per-feature failure summary
    let any_feature_fails = feature_fail_counts.iter().any(|&c| c > 0);
    if any_feature_fails {
        println!();
        println!("Per-feature failure counts:");
        for (i, &count) in feature_fail_counts.iter().enumerate() {
            if count > 0 {
                println!("  {:<25} {} days", FEATURE_NAMES[i], count);
            }
        }
    }

    println!();
    if days_failed == 0 && days_error == 0 {
        println!("Overall: PASS");
    } else {
        println!("Overall: FAIL");
        process::exit(1);
    }
}
