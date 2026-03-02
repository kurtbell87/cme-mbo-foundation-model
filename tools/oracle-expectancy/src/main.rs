use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Result};
use clap::Parser;

use backtest::{LabelMethod, OracleConfig, OracleReplay};
use common::execution_costs::ExecutionCosts;
use oracle_expectancy::{
    aggregate_day_results, build_bars_from_dbn, build_rollover_calendar, date_to_quarter,
    get_available_days, get_contract_symbol, get_instrument_id,
    select_stratified_days, CostsSummary, DayResult, FullReport, ReportConfig,
};

/// Run oracle backtest with parameterized triple barriers on MES MBO data.
#[derive(Parser, Debug)]
#[command(name = "oracle-expectancy")]
#[command(about = "Run oracle backtest with parameterized triple barriers")]
struct Args {
    /// Path to DBN data directory containing glbx-mdp3-YYYYMMDD.mbo.dbn.zst files
    #[arg(long)]
    data: String,

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

    /// Number of days per quarter for stratified sampling
    #[arg(long, default_value = "5")]
    days_per_quarter: usize,

    /// Run all available days (overrides --days-per-quarter)
    #[arg(long)]
    all_days: bool,

    /// Run a single day (YYYYMMDD)
    #[arg(long)]
    day: Option<i32>,

    /// Run specific days (comma-separated YYYYMMDD)
    #[arg(long, value_delimiter = ',')]
    days: Option<Vec<i32>>,

    /// Output JSON path
    #[arg(long)]
    output: Option<String>,
}

fn main() -> Result<()> {
    let args = Args::parse();

    let data_dir = Path::new(&args.data);
    if !data_dir.exists() {
        bail!("Data directory does not exist: {}", args.data);
    }

    let costs = ExecutionCosts::default();

    // Build oracle configs for both label methods
    let base_cfg = OracleConfig {
        target_ticks: args.target,
        stop_ticks: args.stop,
        take_profit_ticks: args.take_profit,
        max_time_horizon_s: args.max_time_horizon,
        volume_horizon: args.volume_horizon,
        tick_size: 0.25,
        label_method: LabelMethod::FirstToHit,
    };

    let fth_cfg = OracleConfig {
        label_method: LabelMethod::FirstToHit,
        ..base_cfg.clone()
    };
    let tb_cfg = OracleConfig {
        label_method: LabelMethod::TripleBarrier,
        ..base_cfg.clone()
    };

    // Select days
    let selected_days = if let Some(ref explicit_days) = args.days {
        let mut d = explicit_days.clone();
        d.sort();
        d
    } else if let Some(single_day) = args.day {
        vec![single_day]
    } else if args.all_days {
        let calendar = build_rollover_calendar();
        get_available_days(data_dir)?
            .into_iter()
            .filter(|d| !calendar.is_excluded(*d))
            .collect()
    } else {
        select_stratified_days(data_dir, args.days_per_quarter)?
    };

    // Print config banner
    println!("=======================================");
    println!("  Oracle Expectancy Backtest");
    println!("=======================================");
    println!("  Target:     {} ticks", args.target);
    println!("  Stop:       {} ticks", args.stop);
    println!("  Take Profit:{} ticks", args.take_profit);
    println!("  Time Horizon: {} s", args.max_time_horizon);
    println!("  Volume Horizon: {}", args.volume_horizon);
    println!("  Commission: ${:.2}/side", costs.commission_per_side);
    println!("  Spread:     {} tick(s) fixed", costs.fixed_spread_ticks);
    println!("  Multiplier: ${:.2}", costs.contract_multiplier);
    println!("  Days selected: {}", selected_days.len());
    println!("=======================================\n");

    if selected_days.is_empty() {
        bail!("No days selected — check data directory");
    }

    // Print selected days grouped by quarter
    {
        let mut by_quarter: BTreeMap<&str, Vec<i32>> = BTreeMap::new();
        for &d in &selected_days {
            by_quarter.entry(date_to_quarter(d)).or_default().push(d);
        }
        for (q, days) in &by_quarter {
            let day_strs: Vec<String> = days.iter().map(|d| d.to_string()).collect();
            println!("  {}: {}", q, day_strs.join(", "));
        }
        println!();
    }

    // Process each day
    let mut day_results = Vec::new();
    let mut days_skipped = 0;

    for (i, &date) in selected_days.iter().enumerate() {
        let date_str = format!("{}", date);
        let instrument_id = get_instrument_id(date);
        let contract = get_contract_symbol(date);
        let dbn_path = data_dir.join(format!("glbx-mdp3-{}.mbo.dbn.zst", date_str));

        print!(
            "[{}/{}] {} ({}, id={}) ... ",
            i + 1,
            selected_days.len(),
            date_str,
            contract,
            instrument_id
        );

        if !dbn_path.exists() {
            println!("SKIP (file not found)");
            days_skipped += 1;
            continue;
        }

        // Build bars
        let bars = match build_bars_from_dbn(&dbn_path, instrument_id, &date_str) {
            Ok(b) => b,
            Err(e) => {
                println!("SKIP ({})", e);
                days_skipped += 1;
                continue;
            }
        };

        if bars.len() < 10 {
            println!("SKIP ({} bars < 10)", bars.len());
            days_skipped += 1;
            continue;
        }

        // Run both oracle replays
        let fth_replay = OracleReplay::new(fth_cfg.clone(), costs.clone());
        let tb_replay = OracleReplay::new(tb_cfg.clone(), costs.clone());

        let fth_result = fth_replay.run(&bars);
        let tb_result = tb_replay.run(&bars);

        println!(
            "{} bars | FTH: {} trades, ${:.2} | TB: {} trades, ${:.2}",
            bars.len(),
            fth_result.total_trades,
            fth_result.net_pnl,
            tb_result.total_trades,
            tb_result.net_pnl,
        );

        day_results.push(DayResult {
            date,
            fth_result,
            tb_result,
            bar_count: bars.len(),
        });
    }

    println!();

    if day_results.is_empty() {
        bail!("No days processed successfully");
    }

    // Aggregate results
    let mut report = aggregate_day_results(&day_results);
    report.days_skipped = days_skipped;

    // Print summary table
    println!("=======================================");
    println!("  RESULTS SUMMARY");
    println!("=======================================");
    println!(
        "  Days processed: {} | Days skipped: {}",
        report.days_processed, report.days_skipped
    );
    println!();
    print_method_summary("First-To-Hit", &report.first_to_hit);
    println!();
    print_method_summary("Triple-Barrier", &report.triple_barrier);
    println!();

    // Per-quarter summary
    if !report.per_quarter.is_empty() {
        println!("  Per-Quarter Breakdown:");
        println!("  {:<10} {:>8} {:>8} {:>10} {:>8} {:>10} {:>8}",
            "Quarter", "FTH-T", "TB-T", "FTH-PnL", "FTH-WR", "TB-PnL", "TB-WR");
        println!("  {}", "-".repeat(66));
        for (q, qr) in &report.per_quarter {
            println!(
                "  {:<10} {:>8} {:>8} {:>10.2} {:>7.1}% {:>10.2} {:>7.1}%",
                q,
                qr.first_to_hit.total_trades,
                qr.triple_barrier.total_trades,
                qr.first_to_hit.net_pnl,
                qr.first_to_hit.win_rate * 100.0,
                qr.triple_barrier.net_pnl,
                qr.triple_barrier.win_rate * 100.0,
            );
        }
        println!();
    }

    // Build full JSON report
    let full_report = FullReport {
        config: ReportConfig {
            bar_type: "time".to_string(),
            bar_interval_s: 5,
            target_ticks: args.target,
            stop_ticks: args.stop,
            take_profit_ticks: args.take_profit,
            max_time_horizon_s: args.max_time_horizon,
            volume_horizon: args.volume_horizon,
        },
        costs: CostsSummary::from_costs(&costs),
        days_processed: report.days_processed,
        days_skipped: report.days_skipped,
        first_to_hit: report.first_to_hit,
        triple_barrier: report.triple_barrier,
        per_quarter: report.per_quarter,
    };

    let json = serde_json::to_string_pretty(&full_report)?;

    // Write to --output if specified
    if let Some(ref output_path) = args.output {
        std::fs::write(output_path, &json)?;
        println!("  JSON written to: {}", output_path);
    }

    // Write to .kit/results/oracle-expectancy/metrics.json
    let kit_dir = PathBuf::from(".kit/results/oracle-expectancy");
    if let Err(e) = std::fs::create_dir_all(&kit_dir) {
        eprintln!("  Warning: could not create {}: {}", kit_dir.display(), e);
    } else {
        let kit_path = kit_dir.join("metrics.json");
        std::fs::write(&kit_path, &json)?;
        println!("  JSON written to: {}", kit_path.display());
    }

    Ok(())
}

fn print_method_summary(name: &str, s: &oracle_expectancy::BacktestResultSummary) {
    println!("  {} ({} trades):", name, s.total_trades);
    println!("    Win Rate:       {:.1}%", s.win_rate * 100.0);
    println!("    Gross PnL:      ${:.2}", s.gross_pnl);
    println!("    Net PnL:        ${:.2}", s.net_pnl);
    println!("    Expectancy:     ${:.2}/trade", s.expectancy);
    println!("    Profit Factor:  {:.2}", s.profit_factor);
    println!("    Sharpe:         {:.3}", s.sharpe);
    println!("    Ann. Sharpe:    {:.3}", s.annualized_sharpe);
    println!("    Max Drawdown:   ${:.2}", s.max_drawdown);
    println!("    Avg Bars Held:  {:.1}", s.avg_bars_held);
    println!("    Trades/Day:     {:.1}", s.trades_per_day);
    println!(
        "    Exits: T={} S={} TP={} Exp={} Sess={}",
        s.exit_reasons.target,
        s.exit_reasons.stop,
        s.exit_reasons.take_profit,
        s.exit_reasons.expiry,
        s.exit_reasons.session_end,
    );
}
