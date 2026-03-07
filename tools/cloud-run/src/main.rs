mod build;
mod config;
mod launch;
mod monitor;
mod userdata;

use anyhow::Result;
use chrono::Utc;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "cloud-run", about = "Launch experiments on EC2 with Docker containers")]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// Path to cloud-run.toml config file
    #[arg(long, short, global = true, default_value = "cloud-run.toml")]
    config: PathBuf,

    /// Output format (text or json)
    #[arg(long, global = true, default_value = "text")]
    output: OutputFormat,
}

#[derive(Clone, clap::ValueEnum)]
enum OutputFormat {
    Text,
    Json,
}

#[derive(Subcommand)]
enum Commands {
    /// Build Docker image and push to ECR
    Build,

    /// Test locally (docker run on CPU, no EC2)
    DryRun,

    /// Launch experiment on EC2
    Launch {
        /// Stream logs and wait for completion
        #[arg(long, short)]
        follow: bool,

        /// Override instance type
        #[arg(long)]
        instance_type: Option<String>,
    },

    /// Check status of a running experiment
    Status {
        /// Run ID (e.g., mbo-grammar-20260307T1234Z)
        run_id: String,
    },

    /// Stream logs from a running or completed experiment
    Logs {
        /// Run ID
        run_id: String,
    },

    /// Stop a running experiment
    Stop {
        /// Run ID
        run_id: String,
    },

    /// List recent runs
    List,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    let config_path = cli.config.canonicalize().unwrap_or_else(|_| cli.config.clone());
    let config_dir = config_path
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .to_path_buf();

    match cli.command {
        Commands::Build => {
            let config = config::Config::load(&config_path)?;
            eprintln!("Building image for '{}'", config.experiment.name);
            let image_uri = build::build_and_push(&config, &config_dir)?;
            match cli.output {
                OutputFormat::Json => {
                    println!(
                        "{}",
                        serde_json::json!({"image": image_uri, "status": "pushed"})
                    );
                }
                OutputFormat::Text => {
                    eprintln!("Done. Image: {}", image_uri);
                }
            }
        }

        Commands::DryRun => {
            let config = config::Config::load(&config_path)?;
            eprintln!("Dry-run for '{}'", config.experiment.name);
            build::dry_run(&config, &config_dir)?;
        }

        Commands::Launch { follow, instance_type } => {
            let mut config = config::Config::load(&config_path)?;
            if let Some(it) = instance_type {
                config.instance.instance_type = it;
            }

            let run_id = generate_run_id(&config.experiment.name);
            eprintln!("Launching '{}' as {}", config.experiment.name, run_id);

            // Build and push image if dockerfile is specified and no image override
            if config.container.image.is_none() {
                eprintln!("\n[1/3] Building container image");
                let image_uri = build::build_and_push(&config, &config_dir)?;
                config.container.image = Some(image_uri);
            }

            // Launch EC2 instance
            eprintln!("\n[2/3] Launching EC2 instance");
            let result = launch::launch(&config, &run_id)?;
            eprintln!(
                "  Instance: {} ({})",
                result.instance_id, result.instance_type
            );
            eprintln!("  Results will be at: {}/", result.s3_base);

            if follow {
                // Monitor until completion
                eprintln!("\n[3/3] Monitoring");
                match monitor::monitor(&config, &run_id, &result.instance_id) {
                    Ok(status) => {
                        eprintln!();
                        match cli.output {
                            OutputFormat::Json => {
                                println!(
                                    "{}",
                                    serde_json::json!({
                                        "run_id": run_id,
                                        "instance_id": result.instance_id,
                                        "instance_type": result.instance_type,
                                        "status": status.status,
                                        "exit_code": status.exit_code,
                                        "results_s3": format!("{}/results/", result.s3_base),
                                        "log_s3": format!("{}/experiment.log", result.s3_base),
                                    })
                                );
                            }
                            OutputFormat::Text => {
                                if status.exit_code == 0 {
                                    eprintln!("Experiment completed successfully.");
                                } else {
                                    eprintln!(
                                        "Experiment failed with exit code {}.",
                                        status.exit_code
                                    );
                                }
                                eprintln!("Results: {}/results/", result.s3_base);
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("Monitor error: {}", e);
                        std::process::exit(1);
                    }
                }
            } else {
                // Fire and forget
                match cli.output {
                    OutputFormat::Json => {
                        println!(
                            "{}",
                            serde_json::json!({
                                "run_id": run_id,
                                "instance_id": result.instance_id,
                                "instance_type": result.instance_type,
                                "status": "launched",
                                "results_s3": format!("{}/", result.s3_base),
                            })
                        );
                    }
                    OutputFormat::Text => {
                        eprintln!();
                        eprintln!("Launched. Check status with:");
                        eprintln!("  cloud-run status {}", run_id);
                    }
                }
            }
        }

        Commands::Status { run_id } => {
            let config = config::Config::load(&config_path)?;
            let region = &config.instance.region;
            let s3_base = format!("{}/{}", config.results.s3_prefix, run_id);

            // Check S3 for status
            let output = std::process::Command::new("aws")
                .args([
                    "s3", "cp",
                    &format!("{}/status.json", s3_base),
                    "-",
                    "--region", region,
                ])
                .output()?;

            if output.status.success() {
                let body = String::from_utf8_lossy(&output.stdout);
                let status: monitor::CompletionStatus = serde_json::from_str(&body)?;
                match cli.output {
                    OutputFormat::Json => println!("{}", body.trim()),
                    OutputFormat::Text => {
                        eprintln!("Run:    {}", run_id);
                        eprintln!("Status: {} (exit code {})", status.status, status.exit_code);
                        eprintln!("Time:   {}", status.ts);
                    }
                }
            } else {
                // Check heartbeat
                let hb_output = std::process::Command::new("aws")
                    .args([
                        "s3", "cp",
                        &format!("{}/heartbeat.json", s3_base),
                        "-",
                        "--region", region,
                    ])
                    .output()?;

                if hb_output.status.success() {
                    let body = String::from_utf8_lossy(&hb_output.stdout);
                    match cli.output {
                        OutputFormat::Json => println!("{}", body.trim()),
                        OutputFormat::Text => {
                            let hb: monitor::HeartbeatStatus = serde_json::from_str(&body)?;
                            eprintln!("Run:    {}", run_id);
                            eprintln!("Status: running");
                            eprintln!("Last heartbeat: {}", hb.ts);
                        }
                    }
                } else {
                    eprintln!("No status found for run '{}'", run_id);
                    std::process::exit(1);
                }
            }
        }

        Commands::Logs { run_id } => {
            let config = config::Config::load(&config_path)?;
            let s3_base = format!("{}/{}", config.results.s3_prefix, run_id);

            let output = std::process::Command::new("aws")
                .args([
                    "s3", "cp",
                    &format!("{}/experiment.log", s3_base),
                    "-",
                    "--region", &config.instance.region,
                ])
                .output()?;

            if output.status.success() {
                print!("{}", String::from_utf8_lossy(&output.stdout));
            } else {
                eprintln!("No logs found for run '{}'", run_id);
                std::process::exit(1);
            }
        }

        Commands::Stop { run_id } => {
            let config = config::Config::load(&config_path)?;
            let region = &config.instance.region;

            if let Some(instance_id) = launch::find_instance_by_run_id(&run_id, region)? {
                eprintln!("Terminating instance {} for run {}", instance_id, run_id);
                launch::terminate(&instance_id, region)?;
                eprintln!("Terminated.");
            } else {
                eprintln!("No running instance found for run '{}'", run_id);
            }
        }

        Commands::List => {
            let config = config::Config::load(&config_path)?;

            // List instances tagged with cloud-run-id
            let output = std::process::Command::new("aws")
                .args([
                    "ec2",
                    "describe-instances",
                    "--region",
                    &config.instance.region,
                    "--filters",
                    "Name=tag-key,Values=cloud-run-id",
                    "--query",
                    "Reservations[].Instances[].{Id:InstanceId,Type:InstanceType,State:State.Name,RunId:Tags[?Key=='cloud-run-id']|[0].Value,Name:Tags[?Key=='cloud-run-name']|[0].Value,Launch:LaunchTime}",
                    "--output",
                    "table",
                ])
                .output()?;

            if output.status.success() {
                print!("{}", String::from_utf8_lossy(&output.stdout));
            }

            // Also list recent S3 results
            eprintln!("\nRecent results in S3:");
            let s3_output = std::process::Command::new("aws")
                .args([
                    "s3",
                    "ls",
                    &format!("{}/", config.results.s3_prefix),
                    "--region",
                    &config.instance.region,
                ])
                .output()?;

            if s3_output.status.success() {
                print!("{}", String::from_utf8_lossy(&s3_output.stdout));
            }
        }
    }

    Ok(())
}

fn generate_run_id(name: &str) -> String {
    let ts = Utc::now().format("%Y%m%dT%H%M%SZ");
    format!("{}-{}", name, ts)
}
