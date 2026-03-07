use anyhow::{Context, Result};
use std::process::Command;

use crate::config::Config;
use crate::userdata;

/// Result of launching an instance
pub struct LaunchResult {
    pub instance_id: String,
    pub instance_type: String,
    pub s3_base: String,
}

/// Launch an EC2 instance with the experiment container.
pub fn launch(config: &Config, run_id: &str) -> Result<LaunchResult> {
    let region = &config.instance.region;

    // Get AWS account ID
    let account_id = get_account_id(region)?;
    let ecr_image = config.ecr_image(&account_id);
    let ecr_registry = format!("{}.dkr.ecr.{}.amazonaws.com", account_id, region);

    // Find a GPU-capable AMI (ECS-optimized GPU)
    let ami_id = find_gpu_ami(region)?;

    // Generate user-data script
    let userdata_script = userdata::generate(config, run_id, &ecr_image, &ecr_registry);
    let userdata_b64 = base64_encode(&userdata_script);

    // Build the run-instances command
    let s3_base = format!("{}/{}", config.results.s3_prefix, run_id);
    let instance_id = run_ec2_launch(config, run_id, &ami_id, &userdata_b64, region)?;

    Ok(LaunchResult {
        instance_id,
        instance_type: config.instance.instance_type.clone(),
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

fn find_gpu_ami(region: &str) -> Result<String> {
    // Use ECS-optimized GPU AMI — has Docker + NVIDIA drivers + AWS CLI
    let output = Command::new("aws")
        .args([
            "ssm",
            "get-parameter",
            "--name",
            "/aws/service/ecs/optimized-ami/amazon-linux-2/gpu/recommended/image_id",
            "--query",
            "Parameter.Value",
            "--output",
            "text",
            "--region",
            region,
        ])
        .output()
        .context("Failed to look up ECS GPU AMI")?;

    if !output.status.success() {
        anyhow::bail!(
            "Failed to find GPU AMI: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let ami = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if ami.is_empty() {
        anyhow::bail!("No GPU AMI found");
    }
    Ok(ami)
}

fn run_ec2_launch(
    config: &Config,
    run_id: &str,
    ami_id: &str,
    userdata_b64: &str,
    region: &str,
) -> Result<String> {
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
        userdata_b64.to_string(),
        "--instance-initiated-shutdown-behavior".to_string(),
        "terminate".to_string(),
        "--block-device-mappings".to_string(),
        r#"[{"DeviceName":"/dev/xvda","Ebs":{"VolumeSize":80,"VolumeType":"gp3","DeleteOnTermination":true}}]"#.to_string(),
        "--tag-specifications".to_string(),
        format!(
            "ResourceType=instance,Tags=[{{Key=Name,Value=cloud-run-{name}}},{{Key=cloud-run-id,Value={run_id}}},{{Key=cloud-run-name,Value={name}}}]",
            name = config.experiment.name,
        ),
        "--query".to_string(),
        "Instances[0].InstanceId".to_string(),
        "--output".to_string(),
        "text".to_string(),
    ];

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
        anyhow::bail!("EC2 launch failed: {}", stderr);
    }

    let instance_id = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if instance_id.is_empty() {
        anyhow::bail!("EC2 launch returned no instance ID");
    }

    Ok(instance_id)
}

fn base64_encode(input: &str) -> String {
    use std::io::Write;
    let mut encoder = Vec::new();
    // Simple base64 encoding using standard library
    let encoded = base64_impl(input.as_bytes());
    let _ = write!(encoder, "{}", encoded);
    String::from_utf8(encoder).unwrap_or_default()
}

fn base64_impl(data: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        result.push(CHARS[((triple >> 18) & 0x3F) as usize] as char);
        result.push(CHARS[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            result.push(CHARS[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
        if chunk.len() > 2 {
            result.push(CHARS[(triple & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
    }
    result
}
