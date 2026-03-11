mod build;
mod config;
mod launch;
mod monitor;
mod runpod;
mod runpod_bootstrap;
mod userdata;

use anyhow::Result;
use chrono::Utc;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "cloud-run",
    about = "Launch experiments on EC2 or RunPod with Docker containers"
)]
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
    /// Build Docker image and push to ECR (AWS) or Docker Hub (RunPod)
    Build,

    /// Build Docker image on a remote EC2 instance (avoids QEMU on Apple Silicon)
    BuildRemote {
        /// Launch builder and exit without waiting for completion
        #[arg(long)]
        no_follow: bool,

        /// Override builder instance type (default: c7a.4xlarge)
        #[arg(long)]
        instance_type: Option<String>,
    },

    /// Test locally (docker run on CPU, no EC2/RunPod)
    DryRun,

    /// Launch experiment on EC2 or RunPod (auto-detected from config)
    Launch {
        /// Stream logs and wait for completion
        #[arg(long, short)]
        follow: bool,

        /// Override instance type (EC2) or GPU type (RunPod)
        #[arg(long)]
        instance_type: Option<String>,

        /// Force on-demand (skip spot attempt) — EC2 only
        #[arg(long)]
        on_demand: bool,
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

    /// Clean up orphaned resources (expired instances, unused SGs, exited pods)
    Cleanup,
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
            let image_uri = if config.is_runpod() {
                build::build_and_push_dockerhub(&config, &config_dir)?
            } else {
                build::build_and_push(&config, &config_dir)?
            };
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

        Commands::BuildRemote {
            no_follow,
            instance_type,
        } => {
            let config = config::Config::load(&config_path)?;
            if config.is_runpod() {
                anyhow::bail!("build-remote is only supported for AWS backend");
            }
            eprintln!("Building image remotely for '{}'", config.experiment.name);
            let image_uri =
                build::build_remote(&config, &config_dir, instance_type, !no_follow)?;
            match cli.output {
                OutputFormat::Json => {
                    println!(
                        "{}",
                        serde_json::json!({"image": image_uri, "status": "pushed"})
                    );
                }
                OutputFormat::Text => {
                    eprintln!("Image: {}", image_uri);
                }
            }
        }

        Commands::DryRun => {
            let config = config::Config::load(&config_path)?;
            eprintln!("Dry-run for '{}'", config.experiment.name);
            build::dry_run(&config, &config_dir)?;
        }

        Commands::Launch {
            follow,
            instance_type,
            on_demand,
        } => {
            let mut config = config::Config::load(&config_path)?;

            if config.is_runpod() {
                // ── RunPod launch ──────────────────────────────────
                if let Some(it) = instance_type {
                    config.runpod.as_mut().unwrap().gpu_type = it;
                }
                launch_runpod(&mut config, &config_dir, follow, &cli.output)?;
            } else {
                // ── AWS EC2 launch ────────────────────────────────
                if let Some(it) = instance_type {
                    config.instance.instance_type = it;
                }
                if on_demand {
                    config.instance.spot = false;
                }
                launch_ec2(&mut config, &config_dir, follow, &cli.output)?;
            }
        }

        Commands::Status { run_id } => {
            let config = config::Config::load(&config_path)?;
            let region = &config.instance.region;
            let s3_base = format!("{}/{}", config.results.s3_prefix, run_id);

            let output = std::process::Command::new("aws")
                .args([
                    "s3",
                    "cp",
                    &format!("{}/status.json", s3_base),
                    "-",
                    "--region",
                    region,
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
                let hb_output = std::process::Command::new("aws")
                    .args([
                        "s3",
                        "cp",
                        &format!("{}/heartbeat.json", s3_base),
                        "-",
                        "--region",
                        region,
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
                    "s3",
                    "cp",
                    &format!("{}/experiment.log", s3_base),
                    "-",
                    "--region",
                    &config.instance.region,
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

            if config.is_runpod() {
                let api_key = runpod::resolve_api_key()?;
                if let Some(pod_id) = runpod::find_pod_by_run_id(&api_key, &run_id)? {
                    eprintln!("Terminating RunPod pod {} for run {}", pod_id, run_id);
                    runpod::terminate_pod(&api_key, &pod_id)?;
                    eprintln!("Terminated.");
                } else {
                    eprintln!("No running RunPod pod found for run '{}'", run_id);
                }
            } else {
                let region = &config.instance.region;
                if let Some(instance_id) = launch::find_instance_by_run_id(&run_id, region)? {
                    eprintln!("Terminating instance {} for run {}", instance_id, run_id);
                    launch::terminate(&instance_id, region)?;
                    eprintln!("Terminated.");
                } else {
                    eprintln!("No running instance found for run '{}'", run_id);
                }
            }
        }

        Commands::List => {
            let config = config::Config::load(&config_path)?;

            // AWS EC2 instances
            eprintln!("EC2 instances:");
            let output = std::process::Command::new("aws")
                .args([
                    "ec2",
                    "describe-instances",
                    "--region",
                    &config.instance.region,
                    "--filters",
                    "Name=tag-key,Values=cloud-run-id",
                    "--query",
                    "Reservations[].Instances[].{Id:InstanceId,Type:InstanceType,State:State.Name,RunId:Tags[?Key=='cloud-run-id']|[0].Value,Name:Tags[?Key=='cloud-run-name']|[0].Value,Pricing:Tags[?Key=='pricing_model']|[0].Value,TTL:Tags[?Key=='max_ttl']|[0].Value,Launch:LaunchTime}",
                    "--output",
                    "table",
                ])
                .output()?;

            if output.status.success() {
                print!("{}", String::from_utf8_lossy(&output.stdout));
            }

            // RunPod pods
            if let Ok(api_key) = runpod::resolve_api_key() {
                eprintln!("\nRunPod pods:");
                let _ = runpod::list_pods(&api_key);
            }

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

        Commands::Cleanup => {
            let config = config::Config::load(&config_path)?;
            let region = &config.instance.region;

            eprintln!("Cleaning up orphaned resources...");

            // 1. AWS: TTL-expired instances
            eprintln!("\n[1/3] Checking for TTL-expired EC2 instances...");
            match launch::cleanup_expired(region) {
                Ok(terminated) => {
                    if terminated.is_empty() {
                        eprintln!("  No expired instances found.");
                    } else {
                        eprintln!("  Terminated {} expired instance(s).", terminated.len());
                    }
                }
                Err(e) => eprintln!("  Error checking expired instances: {}", e),
            }

            // 2. AWS: orphaned security groups
            eprintln!("\n[2/3] Cleaning up orphaned security groups...");
            match launch::cleanup_security_groups(region) {
                Ok(count) => {
                    if count == 0 {
                        eprintln!("  No orphaned security groups found.");
                    } else {
                        eprintln!("  Deleted {} security group(s).", count);
                    }
                }
                Err(e) => eprintln!("  Error cleaning security groups: {}", e),
            }

            // 3. RunPod: exited pods
            eprintln!("\n[3/3] Cleaning up exited RunPod pods...");
            match runpod::resolve_api_key() {
                Ok(api_key) => match runpod::cleanup_pods(&api_key) {
                    Ok(cleaned) => {
                        if cleaned.is_empty() {
                            eprintln!("  No exited pods found.");
                        } else {
                            for pod in &cleaned {
                                eprintln!("  Terminated: {}", pod);
                            }
                        }
                    }
                    Err(e) => eprintln!("  Error cleaning RunPod pods: {}", e),
                },
                Err(_) => eprintln!("  Skipped (no RunPod API key configured)."),
            }

            eprintln!("\nCleanup complete.");
        }
    }

    Ok(())
}

/// Launch an experiment on RunPod.
fn launch_runpod(
    config: &mut config::Config,
    config_dir: &std::path::Path,
    follow: bool,
    output_fmt: &OutputFormat,
) -> Result<()> {
    let rp = config.runpod.as_ref().unwrap();
    let run_id = generate_run_id(&config.experiment.name);
    let api_key = runpod::resolve_api_key()?;

    eprintln!("Launching '{}' as {} [RunPod]", config.experiment.name, run_id);
    eprintln!(
        "  GPU: {} x{} | TTL: {} min",
        rp.gpu_type, rp.gpu_count, config.instance.max_runtime_minutes,
    );

    // Step 1: Resolve image
    let image = if let Some(ref img) = config.container.image {
        eprintln!("\n[1/3] Using pre-built image: {}", img);
        img.clone()
    } else {
        eprintln!("\n[1/3] Building container image for Docker Hub");
        let uri = build::build_and_push_dockerhub(config, config_dir)?;
        config.container.image = Some(uri.clone());
        uri
    };

    // Step 2: Launch RunPod pod
    eprintln!("\n[2/3] Launching RunPod pod");
    let rp = config.runpod.as_ref().unwrap();

    // Resolve datacenter
    let datacenter = rp
        .datacenter
        .clone()
        .unwrap_or_else(|| runpod::resolve_datacenter(&api_key, &rp.gpu_type));
    eprintln!("  Datacenter: {}", datacenter);

    // Generate bootstrap script
    let bootstrap_script = runpod_bootstrap::generate(config, &run_id);
    let bootstrap_b64 = base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        bootstrap_script.as_bytes(),
    );
    let docker_args = format!("bash -c 'echo {} | base64 -d | bash'", bootstrap_b64);

    // Build env vars for the pod
    let mut env_vars: Vec<(String, String)> = Vec::new();
    env_vars.push(("RUNPOD_API_KEY".to_string(), api_key.clone()));

    // Inject AWS credentials for S3 access
    let (aws_key, aws_secret, aws_token) = runpod::get_aws_credentials()?;
    env_vars.push(("AWS_ACCESS_KEY_ID".to_string(), aws_key));
    env_vars.push(("AWS_SECRET_ACCESS_KEY".to_string(), aws_secret));
    if let Some(token) = aws_token {
        env_vars.push(("AWS_SESSION_TOKEN".to_string(), token));
    }
    env_vars.push((
        "AWS_DEFAULT_REGION".to_string(),
        config.instance.region.clone(),
    ));

    // Add user-specified env vars
    for (k, v) in &config.run.env {
        env_vars.push((k.clone(), v.clone()));
    }

    let rp = config.runpod.as_ref().unwrap();
    let pod_name = format!("cr-{}", &run_id[..run_id.len().min(28)]);
    let pod_id = runpod::create_pod(
        &api_key,
        &pod_name,
        &image,
        &rp.gpu_type,
        rp.gpu_count,
        &datacenter,
        rp.container_disk_gb,
        rp.volume_gb,
        &docker_args,
        &env_vars,
    )?;

    let s3_base = format!("{}/{}", config.results.s3_prefix, run_id);
    eprintln!("  Pod: {}", pod_id);
    eprintln!("  Results: {}/", s3_base);

    // Wait for pod to start
    eprintln!("  Waiting for pod to start...");
    runpod::wait_ready(&api_key, &pod_id, 600)?;
    eprintln!("  Pod running.");

    if follow {
        eprintln!("\n[3/3] Monitoring");
        let backend = monitor::Backend::Runpod {
            api_key: api_key.clone(),
        };
        match monitor::monitor(config, &run_id, &pod_id, &backend) {
            Ok(status) => {
                eprintln!();
                match output_fmt {
                    OutputFormat::Json => {
                        println!(
                            "{}",
                            serde_json::json!({
                                "run_id": run_id,
                                "pod_id": pod_id,
                                "backend": "runpod",
                                "gpu_type": config.runpod.as_ref().unwrap().gpu_type,
                                "status": status.status,
                                "exit_code": status.exit_code,
                                "results_s3": format!("{}/results/", s3_base),
                                "log_s3": format!("{}/experiment.log", s3_base),
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
                        eprintln!("Results: {}/results/", s3_base);
                    }
                }
            }
            Err(e) => {
                eprintln!("Monitor error: {}", e);
                std::process::exit(1);
            }
        }
    } else {
        match output_fmt {
            OutputFormat::Json => {
                println!(
                    "{}",
                    serde_json::json!({
                        "run_id": run_id,
                        "pod_id": pod_id,
                        "backend": "runpod",
                        "gpu_type": config.runpod.as_ref().unwrap().gpu_type,
                        "status": "launched",
                        "results_s3": format!("{}/", s3_base),
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

    Ok(())
}

/// Launch an experiment on AWS EC2.
fn launch_ec2(
    config: &mut config::Config,
    config_dir: &std::path::Path,
    follow: bool,
    output_fmt: &OutputFormat,
) -> Result<()> {
    let run_id = generate_run_id(&config.experiment.name);
    eprintln!("Launching '{}' as {} [EC2]", config.experiment.name, run_id);
    eprintln!(
        "  Instance: {} | GPU: {} | Spot: {} | TTL: {} min",
        config.instance.instance_type,
        config.instance.gpu,
        config.instance.spot,
        config.instance.max_runtime_minutes,
    );

    // Build and push image if dockerfile is specified and no image override
    if config.container.image.is_none() {
        eprintln!("\n[1/3] Building container image");
        let image_uri = build::build_and_push(config, config_dir)?;
        config.container.image = Some(image_uri);
    }

    // Launch EC2 instance
    eprintln!("\n[2/3] Launching EC2 instance");
    let result = launch::launch(config, &run_id)?;
    eprintln!(
        "  Instance: {} ({}, {})",
        result.instance_id, result.instance_type, result.pricing_model
    );
    eprintln!("  Results: {}/", result.s3_base);

    if follow {
        eprintln!("\n[3/3] Monitoring");
        let backend = monitor::Backend::Aws;
        match monitor::monitor(config, &run_id, &result.instance_id, &backend) {
            Ok(status) => {
                eprintln!();
                match output_fmt {
                    OutputFormat::Json => {
                        println!(
                            "{}",
                            serde_json::json!({
                                "run_id": run_id,
                                "instance_id": result.instance_id,
                                "instance_type": result.instance_type,
                                "pricing_model": result.pricing_model,
                                "backend": "aws",
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
        match output_fmt {
            OutputFormat::Json => {
                println!(
                    "{}",
                    serde_json::json!({
                        "run_id": run_id,
                        "instance_id": result.instance_id,
                        "instance_type": result.instance_type,
                        "pricing_model": result.pricing_model,
                        "backend": "aws",
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

    Ok(())
}

fn generate_run_id(name: &str) -> String {
    let ts = Utc::now().format("%Y%m%dT%H%M%SZ");
    format!("{}-{}", name, ts)
}
