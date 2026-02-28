use clap::Parser;

/// Oracle backtest with parameterized barriers.
#[derive(Parser, Debug)]
#[command(name = "oracle-expectancy")]
#[command(about = "Run oracle backtest with parameterized triple barriers")]
struct Args {
    /// Target in ticks
    #[arg(long, default_value = "10")]
    target: i32,

    /// Stop in ticks
    #[arg(long, default_value = "5")]
    stop: i32,

    /// Take profit in ticks
    #[arg(long, default_value = "20")]
    take_profit: i32,

    /// Maximum time horizon in seconds
    #[arg(long, default_value = "3600")]
    max_time_horizon: u32,

    /// Volume horizon in contracts
    #[arg(long, default_value = "50000")]
    volume_horizon: u32,

    /// Output JSON path
    #[arg(long)]
    output: Option<String>,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    println!("oracle-expectancy");
    println!("  target: {}, stop: {}, tp: {}", args.target, args.stop, args.take_profit);

    // TODO: Phase 3 implementation
    eprintln!("Not yet implemented — Phase 3");
    std::process::exit(1);
}
