use clap::Parser;
use std::path::Path;
use std::process;

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

    /// Trading day to process (YYYYMMDD format)
    #[arg(long)]
    day: Option<String>,

    /// Max absolute deviation tolerance
    #[arg(long, default_value = "1e-5")]
    tolerance: f64,
}

fn main() {
    let cli = Cli::parse();

    let ref_path = Path::new(&cli.reference);
    if !ref_path.exists() {
        eprintln!(
            "Error: reference parquet directory not found: {}",
            cli.reference
        );
        eprintln!("Cannot count reference bar rows for parity comparison.");
        process::exit(1);
    }

    let data_path = Path::new(&cli.data);
    if !data_path.exists() {
        eprintln!("Error: data directory not found: {}", cli.data);
        process::exit(1);
    }

    // TODO: implement full parity comparison pipeline
    println!("pass/fail summary placeholder");
}
