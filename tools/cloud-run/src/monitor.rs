use anyhow::Result;
use serde::Deserialize;
use std::process::Command;
use std::time::{Duration, Instant};

use crate::config::Config;

#[derive(Debug, Deserialize)]
pub struct HeartbeatStatus {
    pub ts: String,
}

#[derive(Debug, Deserialize)]
pub struct CompletionStatus {
    pub ts: String,
    pub status: String,
    pub exit_code: i32,
}

/// Monitor a running experiment, polling S3 for heartbeat and status.
/// Enforces: heartbeat timeout, max runtime, and idle CPU detection.
pub fn monitor(
    config: &Config,
    run_id: &str,
    instance_id: &str,
) -> Result<CompletionStatus> {
    let region = &config.instance.region;
    let s3_base = format!("{}/{}", config.results.s3_prefix, run_id);
    let poll_interval = Duration::from_secs(30);
    let heartbeat_timeout = Duration::from_secs(config.heartbeat.timeout_minutes as u64 * 60);
    let max_runtime = Duration::from_secs(config.instance.max_runtime_minutes as u64 * 60);
    let idle_timeout_min = config.heartbeat.idle_timeout_minutes;

    let start = Instant::now();
    let mut last_heartbeat = Instant::now();
    let mut last_log_size: u64 = 0;
    let mut booted = false;
    let mut consecutive_idle_checks: u32 = 0;
    // Check idle every 5 poll cycles (2.5 min) to avoid excessive CloudWatch calls
    let idle_check_interval: u32 = 5;
    let mut poll_count: u32 = 0;

    eprintln!("  Monitoring run {} on {}", run_id, instance_id);
    eprintln!(
        "  Max runtime: {} min, heartbeat timeout: {} min, idle timeout: {} min",
        config.instance.max_runtime_minutes,
        config.heartbeat.timeout_minutes,
        idle_timeout_min,
    );
    eprintln!("  Results: {}/", s3_base);
    eprintln!();

    loop {
        std::thread::sleep(poll_interval);
        let elapsed = start.elapsed();
        poll_count += 1;

        // Check for completion first
        if let Some(status) = check_completion(&s3_base, region) {
            pull_new_logs(&s3_base, region, &mut last_log_size);
            return Ok(status);
        }

        // Check heartbeat
        if let Some(_hb) = check_heartbeat(&s3_base, region) {
            if !booted {
                eprintln!("  Instance booted, experiment running");
                booted = true;
            }
            last_heartbeat = Instant::now();
        }

        // Stream new log lines
        pull_new_logs(&s3_base, region, &mut last_log_size);

        // Check instance is still running
        if !is_instance_running(instance_id, region) {
            if let Some(status) = check_completion(&s3_base, region) {
                return Ok(status);
            }
            anyhow::bail!(
                "Instance {} is no longer running and no completion status found",
                instance_id
            );
        }

        // Heartbeat timeout (only after we've seen at least one heartbeat)
        if booted && last_heartbeat.elapsed() > heartbeat_timeout {
            eprintln!(
                "\n  TIMEOUT: No heartbeat for {} min — terminating",
                config.heartbeat.timeout_minutes
            );
            crate::launch::terminate(instance_id, region)?;
            anyhow::bail!("Heartbeat timeout after {} minutes", config.heartbeat.timeout_minutes);
        }

        // Max runtime guard
        if elapsed > max_runtime {
            eprintln!(
                "\n  MAX RUNTIME: {} min exceeded — terminating",
                config.instance.max_runtime_minutes
            );
            crate::launch::terminate(instance_id, region)?;
            anyhow::bail!(
                "Max runtime exceeded ({} minutes)",
                config.instance.max_runtime_minutes
            );
        }

        // Idle CPU detection (only after boot, every idle_check_interval polls)
        if booted && idle_timeout_min > 0 && poll_count % idle_check_interval == 0 {
            if let Some(cpu_pct) =
                crate::launch::check_cpu_utilization(instance_id, region, idle_timeout_min)
            {
                if cpu_pct < 5.0 {
                    consecutive_idle_checks += 1;
                    eprintln!(
                        "\n  IDLE WARNING: CPU at {:.1}% (check {}/{})",
                        cpu_pct,
                        consecutive_idle_checks,
                        // Need 2 consecutive idle checks before terminating
                        2
                    );
                    if consecutive_idle_checks >= 2 {
                        eprintln!(
                            "  IDLE TERMINATE: CPU below 5% for >{} min — job likely failed",
                            idle_timeout_min
                        );
                        crate::launch::terminate(instance_id, region)?;
                        anyhow::bail!(
                            "Instance idle (CPU <5% for {} min), terminated",
                            idle_timeout_min
                        );
                    }
                } else {
                    consecutive_idle_checks = 0;
                }
            }
        }

        // Progress update
        let mins = elapsed.as_secs() / 60;
        let secs = elapsed.as_secs() % 60;
        if !booted {
            eprint!("\r  Waiting for boot... [{}:{:02}]", mins, secs);
        } else {
            eprint!("\r  Running [{}:{:02}]", mins, secs);
        }
    }
}

fn check_heartbeat(s3_base: &str, region: &str) -> Option<HeartbeatStatus> {
    let output = Command::new("aws")
        .args([
            "s3",
            "cp",
            &format!("{}/heartbeat.json", s3_base),
            "-",
            "--region",
            region,
        ])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let body = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(&body).ok()
}

fn check_completion(s3_base: &str, region: &str) -> Option<CompletionStatus> {
    let output = Command::new("aws")
        .args([
            "s3",
            "cp",
            &format!("{}/status.json", s3_base),
            "-",
            "--region",
            region,
        ])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let body = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(&body).ok()
}

fn pull_new_logs(s3_base: &str, region: &str, last_size: &mut u64) {
    let output = Command::new("aws")
        .args([
            "s3",
            "cp",
            &format!("{}/experiment.log", s3_base),
            "-",
            "--region",
            region,
        ])
        .output();

    if let Ok(output) = output {
        if output.status.success() {
            let content = String::from_utf8_lossy(&output.stdout);
            let bytes = content.len() as u64;
            if bytes > *last_size {
                let new_content = &content[*last_size as usize..];
                for line in new_content.lines() {
                    eprintln!("  | {}", line);
                }
                *last_size = bytes;
            }
        }
    }
}

fn is_instance_running(instance_id: &str, region: &str) -> bool {
    let output = Command::new("aws")
        .args([
            "ec2",
            "describe-instances",
            "--instance-ids",
            instance_id,
            "--region",
            region,
            "--query",
            "Reservations[0].Instances[0].State.Name",
            "--output",
            "text",
        ])
        .output();

    match output {
        Ok(o) => {
            let state = String::from_utf8_lossy(&o.stdout).trim().to_string();
            state == "running" || state == "pending"
        }
        Err(_) => false,
    }
}
