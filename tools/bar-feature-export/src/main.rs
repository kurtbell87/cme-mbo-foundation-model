use clap::Parser;

/// Export .dbn.zst MBO data to 152-column Parquet via bar construction + feature computation.
#[derive(Parser, Debug)]
#[command(name = "bar-feature-export")]
#[command(about = "Convert Databento .dbn.zst MBO data to bar features in Parquet format")]
struct Args {
    /// Input .dbn.zst file path
    #[arg(long)]
    input: String,

    /// Output .parquet file path
    #[arg(long)]
    output: String,

    /// Bar type: time, tick, volume, dollar
    #[arg(long, default_value = "time")]
    bar_type: String,

    /// Bar parameter (e.g., 5 for 5-second time bars)
    #[arg(long, default_value = "5")]
    bar_param: f64,

    /// Triple barrier target in ticks
    #[arg(long, default_value = "19")]
    target: i32,

    /// Triple barrier stop in ticks
    #[arg(long, default_value = "7")]
    stop: i32,

    /// Maximum time horizon in seconds
    #[arg(long, default_value = "3600")]
    max_time_horizon: u32,

    /// Volume horizon in contracts
    #[arg(long, default_value = "50000")]
    volume_horizon: u32,

    /// Use legacy 149-column output format
    #[arg(long)]
    legacy_labels: bool,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    println!("bar-feature-export");
    println!("  input:  {}", args.input);
    println!("  output: {}", args.output);
    println!("  bar:    {} (param={})", args.bar_type, args.bar_param);
    println!("  target: {} ticks, stop: {} ticks", args.target, args.stop);
    println!("  time_horizon: {}s, volume_horizon: {}", args.max_time_horizon, args.volume_horizon);

    // TODO: Phase 3 implementation
    // 1. Read .dbn.zst file using databento-ingest
    // 2. Build book snapshots using book-builder
    // 3. Construct bars using bars crate
    // 4. Compute features using features crate
    // 5. Compute triple barrier labels using labels crate
    // 6. Write to Parquet using arrow/parquet crates

    eprintln!("Not yet implemented — Phase 3");
    std::process::exit(1);
}
