use anyhow::{Context, Result};
use chrono::Utc;
use std::process::Command;

use crate::config::Config;
use crate::userdata;

/// Result of launching an instance
pub struct LaunchResult {
    pub instance_id: String,
    pub instance_type: String,
    pub pricing_model: String,
    pub s3_base: String,
}

/// Launch an EC2 instance with the experiment container.
/// Spot-first: tries spot, falls back to on-demand if spot fails.
pub fn launch(config: &Config, run_id: &str) -> Result<LaunchResult> {
    let region = &config.instance.region;

    // Get AWS account ID
    let account_id = get_account_id(region)?;
    let ecr_image = config.ecr_image(&account_id);
    let ecr_registry = format!("{}.dkr.ecr.{}.amazonaws.com", account_id, region);

    // Select AMI based on GPU requirement
    let ami_id = if config.instance.gpu {
        find_gpu_ami(region)?
    } else {
        find_cpu_ami(region)?
    };

    // Generate user-data script and write to temp file.
    // AWS CLI auto-base64-encodes when passed via file://.
    let userdata_script = userdata::generate(config, run_id, &ecr_image, &ecr_registry);
    let userdata_file = std::env::temp_dir().join(format!("cloud-run-userdata-{}.sh", run_id));
    std::fs::write(&userdata_file, &userdata_script)
        .context("Failed to write user-data to temp file")?;

    let s3_base = format!("{}/{}", config.results.s3_prefix, run_id);

    // Spot-first with on-demand fallback
    let (instance_id, pricing_model) = if config.instance.spot {
        match run_ec2_launch(config, run_id, &ami_id, &userdata_file, region, true) {
            Ok(id) => (id, "spot".to_string()),
            Err(e) => {
                eprintln!("  Spot launch failed: {}", e);
                eprintln!("  Falling back to on-demand...");
                let id =
                    run_ec2_launch(config, run_id, &ami_id, &userdata_file, region, false)?;
                (id, "on-demand".to_string())
            }
        }
    } else {
        let id = run_ec2_launch(config, run_id, &ami_id, &userdata_file, region, false)?;
        (id, "on-demand".to_string())
    };

    // Clean up temp file
    let _ = std::fs::remove_file(&userdata_file);

    Ok(LaunchResult {
        instance_id,
        instance_type: config.instance.instance_type.clone(),
        pricing_model,
        s3_base,
    })
}

/// Terminate an EC2 instance by instance ID.
pub fn terminate(instance_id: &str, region: &str) -> Result<()> {
    let output = Command::new("aws")
        .args([
            "ec2",
            "terminate-instances",
            "--instance-ids",
            instance_id,
            "--region",
            region,
            "--output",
            "json",
        ])
        .output()
        .context("Failed to run aws ec2 terminate-instances")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("Failed to terminate {}: {}", instance_id, stderr);
    }
    Ok(())
}

/// Look up instance ID for a run by checking tags.
pub fn find_instance_by_run_id(run_id: &str, region: &str) -> Result<Option<String>> {
    let output = Command::new("aws")
        .args([
            "ec2",
            "describe-instances",
            "--region",
            region,
            "--filters",
            &format!("Name=tag:cloud-run-id,Values={}", run_id),
            "Name=instance-state-name,Values=pending,running,stopping,stopped",
            "--query",
            "Reservations[0].Instances[0].InstanceId",
            "--output",
            "text",
        ])
        .output()
        .context("Failed to query instances")?;

    let id = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if id.is_empty() || id == "None" {
        Ok(None)
    } else {
        Ok(Some(id))
    }
}

/// Find and terminate instances that have exceeded their max_ttl tag.
pub fn cleanup_expired(region: &str) -> Result<Vec<String>> {
    let output = Command::new("aws")
        .args([
            "ec2",
            "describe-instances",
            "--region",
            region,
            "--filters",
            "Name=tag-key,Values=cloud-run-id",
            "Name=instance-state-name,Values=running",
            "--query",
            "Reservations[].Instances[].{Id:InstanceId,Launch:LaunchTime,MaxTTL:Tags[?Key=='max_ttl']|[0].Value}",
            "--output",
            "json",
        ])
        .output()
        .context("Failed to query instances for cleanup")?;

    if !output.status.success() {
        return Ok(vec![]);
    }

    let body = String::from_utf8_lossy(&output.stdout);
    let instances: Vec<serde_json::Value> = serde_json::from_str(&body).unwrap_or_default();
    let now = Utc::now();
    let mut terminated = vec![];

    for inst in &instances {
        let id = inst["Id"].as_str().unwrap_or("");
        let launch_str = inst["Launch"].as_str().unwrap_or("");
        let max_ttl_str = inst["MaxTTL"].as_str().unwrap_or("0");

        if id.is_empty() || launch_str.is_empty() {
            continue;
        }

        let max_ttl_min: u64 = max_ttl_str.parse().unwrap_or(0);
        if max_ttl_min == 0 {
            continue;
        }

        if let Ok(launch_time) = chrono::DateTime::parse_from_rfc3339(launch_str) {
            let elapsed_min = (now - launch_time.with_timezone(&Utc)).num_minutes() as u64;
            if elapsed_min > max_ttl_min {
                eprintln!(
                    "  TTL EXPIRED: {} ran {}min (max {}min) — terminating",
                    id, elapsed_min, max_ttl_min
                );
                if terminate(id, region).is_ok() {
                    terminated.push(id.to_string());
                }
            }
        }
    }

    Ok(terminated)
}

/// Clean up orphaned security groups created by cloud-run.
pub fn cleanup_security_groups(region: &str) -> Result<u32> {
    let output = Command::new("aws")
        .args([
            "ec2",
            "describe-security-groups",
            "--region",
            region,
            "--filters",
            "Name=tag:ManagedBy,Values=cloud-run",
            "--query",
            "SecurityGroups[].{Id:GroupId,Name:GroupName}",
            "--output",
            "json",
        ])
        .output()
        .context("Failed to list security groups")?;

    if !output.status.success() {
        return Ok(0);
    }

    let body = String::from_utf8_lossy(&output.stdout);
    let sgs: Vec<serde_json::Value> = serde_json::from_str(&body).unwrap_or_default();
    let mut deleted = 0u32;

    for sg in &sgs {
        let sg_id = sg["Id"].as_str().unwrap_or("");
        if sg_id.is_empty() {
            continue;
        }

        let del_output = Command::new("aws")
            .args([
                "ec2",
                "delete-security-group",
                "--group-id",
                sg_id,
                "--region",
                region,
            ])
            .output();

        match del_output {
            Ok(o) if o.status.success() => {
                eprintln!("  Deleted SG: {}", sg_id);
                deleted += 1;
            }
            _ => {
                // SG likely still in use — skip
            }
        }
    }

    Ok(deleted)
}

/// Check CPU utilization of an instance via CloudWatch.
/// Returns average CPU% over the last `minutes` minutes, or None if unavailable.
pub fn check_cpu_utilization(instance_id: &str, region: &str, minutes: u32) -> Option<f64> {
    let end = Utc::now();
    let start = end - chrono::Duration::minutes(minutes as i64);

    let output = Command::new("aws")
        .args([
            "cloudwatch",
            "get-metric-statistics",
            "--namespace",
            "AWS/EC2",
            "--metric-name",
            "CPUUtilization",
            "--dimensions",
            &format!("Name=InstanceId,Value={}", instance_id),
            "--start-time",
            &start.to_rfc3339(),
            "--end-time",
            &end.to_rfc3339(),
            "--period",
            &(minutes * 60).to_string(),
            "--statistics",
            "Average",
            "--region",
            region,
            "--output",
            "json",
        ])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let body = String::from_utf8_lossy(&output.stdout);
    let v: serde_json::Value = serde_json::from_str(&body).ok()?;
    let datapoints = v["Datapoints"].as_array()?;
    if datapoints.is_empty() {
        return None;
    }

    // Average across all datapoints
    let sum: f64 = datapoints
        .iter()
        .filter_map(|dp| dp["Average"].as_f64())
        .sum();
    let count = datapoints
        .iter()
        .filter(|dp| dp["Average"].is_f64())
        .count();

    if count == 0 {
        None
    } else {
        Some(sum / count as f64)
    }
}

fn get_account_id(region: &str) -> Result<String> {
    let output = Command::new("aws")
        .args([
            "sts",
            "get-caller-identity",
            "--query",
            "Account",
            "--output",
            "text",
            "--region",
            region,
        ])
        .output()
        .context("Failed to get AWS account ID")?;

    if !output.status.success() {
        anyhow::bail!(
            "aws sts get-caller-identity failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Find AWS Deep Learning Base GPU AMI (Ubuntu).
/// Has Docker, NVIDIA drivers, AWS CLI, Python 3 cloud-init.
/// Root device is /dev/sda1, SSH user is ubuntu.
fn find_gpu_ami(region: &str) -> Result<String> {
    let output = Command::new("aws")
        .args([
            "ec2",
            "describe-images",
            "--region",
            region,
            "--owners",
            "amazon",
            "--filters",
            "Name=name,Values=Deep Learning Base GPU AMI (Ubuntu 22.04)*",
            "Name=state,Values=available",
            "--query",
            "sort_by(Images, &CreationDate)[-1].ImageId",
            "--output",
            "text",
        ])
        .output()
        .context("Failed to look up Deep Learning GPU AMI")?;

    if !output.status.success() {
        anyhow::bail!(
            "Failed to find GPU AMI: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let ami = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if ami.is_empty() || ami == "None" {
        anyhow::bail!("No Deep Learning GPU AMI found");
    }
    Ok(ami)
}

/// Find ECS-optimized CPU AMI (has Docker + AWS CLI, no GPU drivers)
fn find_cpu_ami(region: &str) -> Result<String> {
    let output = Command::new("aws")
        .args([
            "ssm",
            "get-parameter",
            "--name",
            "/aws/service/ecs/optimized-ami/amazon-linux-2023/recommended/image_id",
            "--query",
            "Parameter.Value",
            "--output",
            "text",
            "--region",
            region,
        ])
        .output()
        .context("Failed to look up ECS CPU AMI")?;

    if !output.status.success() {
        // Fallback to AL2
        let output2 = Command::new("aws")
            .args([
                "ssm",
                "get-parameter",
                "--name",
                "/aws/service/ecs/optimized-ami/amazon-linux-2/recommended/image_id",
                "--query",
                "Parameter.Value",
                "--output",
                "text",
                "--region",
                region,
            ])
            .output()
            .context("Failed to look up ECS AL2 AMI")?;

        if !output2.status.success() {
            anyhow::bail!(
                "Failed to find CPU AMI: {}",
                String::from_utf8_lossy(&output2.stderr)
            );
        }

        let ami = String::from_utf8_lossy(&output2.stdout).trim().to_string();
        if ami.is_empty() {
            anyhow::bail!("No CPU AMI found");
        }
        return Ok(ami);
    }

    let ami = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if ami.is_empty() {
        anyhow::bail!("No CPU AMI found");
    }
    Ok(ami)
}

fn run_ec2_launch(
    config: &Config,
    run_id: &str,
    ami_id: &str,
    userdata_file: &std::path::Path,
    region: &str,
    use_spot: bool,
) -> Result<String> {
    let launch_time = Utc::now().to_rfc3339();
    // TTL = max_runtime * 1.5 (buffer for boot + cleanup)
    let max_ttl = (config.instance.max_runtime_minutes as f64 * 1.5).ceil() as u32;
    let pricing_model = if use_spot { "spot" } else { "on-demand" };

    // Root device: /dev/sda1 for Ubuntu (GPU AMI), /dev/xvda for Amazon Linux (CPU AMI)
    let root_device = if config.instance.gpu {
        "/dev/sda1"
    } else {
        "/dev/xvda"
    };
    let bdm = format!(
        r#"[{{"DeviceName":"{}","Ebs":{{"VolumeSize":{},"VolumeType":"gp3","DeleteOnTermination":true}}}}]"#,
        root_device, config.instance.root_volume_gb
    );

    let tags = format!(
        concat!(
            "ResourceType=instance,Tags=[",
            "{{Key=Name,Value=cloud-run-{name}}}",
            ",{{Key=cloud-run-id,Value={run_id}}}",
            ",{{Key=cloud-run-name,Value={name}}}",
            ",{{Key=max_ttl,Value={max_ttl}}}",
            ",{{Key=launch_time,Value={launch_time}}}",
            ",{{Key=pricing_model,Value={pricing_model}}}",
            ",{{Key=max_runtime_minutes,Value={max_runtime}}}",
            "]",
        ),
        name = config.experiment.name,
        run_id = run_id,
        max_ttl = max_ttl,
        launch_time = launch_time,
        pricing_model = pricing_model,
        max_runtime = config.instance.max_runtime_minutes,
    );

    // Also tag volumes with DeleteOnTermination + cloud-run-id for orphan tracking
    let vol_tags = format!(
        "ResourceType=volume,Tags=[{{Key=cloud-run-id,Value={}}},{{Key=DeleteOnTermination,Value=true}}]",
        run_id
    );

    let mut args = vec![
        "ec2".to_string(),
        "run-instances".to_string(),
        "--region".to_string(),
        region.to_string(),
        "--image-id".to_string(),
        ami_id.to_string(),
        "--instance-type".to_string(),
        config.instance.instance_type.clone(),
        "--user-data".to_string(),
        format!("file://{}", userdata_file.display()),
        "--instance-initiated-shutdown-behavior".to_string(),
        "terminate".to_string(),
        "--block-device-mappings".to_string(),
        bdm,
        "--tag-specifications".to_string(),
        tags,
        vol_tags,
        "--query".to_string(),
        "Instances[0].InstanceId".to_string(),
        "--output".to_string(),
        "text".to_string(),
    ];

    if use_spot {
        args.extend([
            "--instance-market-options".to_string(),
            r#"{"MarketType":"spot","SpotOptions":{"SpotInstanceType":"one-time"}}"#.to_string(),
        ]);
    }

    if let Some(ref key) = config.instance.key_name {
        args.extend(["--key-name".to_string(), key.clone()]);
    }
    if let Some(ref subnet) = config.instance.subnet_id {
        args.extend(["--subnet-id".to_string(), subnet.clone()]);
    }
    if !config.instance.security_group_ids.is_empty() {
        args.push("--security-group-ids".to_string());
        args.extend(config.instance.security_group_ids.clone());
    }
    if let Some(ref profile) = config.instance.iam_profile {
        args.extend([
            "--iam-instance-profile".to_string(),
            format!("Name={}", profile),
        ]);
    }

    let output = Command::new("aws")
        .args(&args)
        .output()
        .context("Failed to run aws ec2 run-instances")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("EC2 launch failed ({}): {}", pricing_model, stderr);
    }

    let instance_id = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if instance_id.is_empty() {
        anyhow::bail!("EC2 launch returned no instance ID");
    }

    Ok(instance_id)
}

