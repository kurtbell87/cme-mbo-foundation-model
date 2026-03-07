use anyhow::{Context, Result};
use std::path::Path;
use std::process::Command;

use crate::config::Config;

/// Build a Docker image from the experiment's Dockerfile and push to ECR.
pub fn build_and_push(config: &Config, config_dir: &Path) -> Result<String> {
    let region = &config.instance.region;

    // Get account ID and construct image URI
    let account_id = get_account_id(region)?;
    let image_uri = config.ecr_image(&account_id);
    let ecr_registry = format!("{}.dkr.ecr.{}.amazonaws.com", account_id, region);

    // Ensure ECR repository exists
    ensure_ecr_repo(&ecr_registry, region)?;

    // Resolve Dockerfile path
    let dockerfile = config
        .container
        .dockerfile
        .as_ref()
        .context("No dockerfile specified in config")?;
    let dockerfile_path = config_dir.join(dockerfile);
    let context_dir = config
        .container
        .context
        .as_ref()
        .map(|c| config_dir.join(c))
        .unwrap_or_else(|| {
            dockerfile_path
                .parent()
                .unwrap_or(config_dir)
                .to_path_buf()
        });

    eprintln!("  Building image: {}", image_uri);
    eprintln!("  Dockerfile: {}", dockerfile_path.display());
    eprintln!("  Context: {}", context_dir.display());

    // Docker build (--platform linux/amd64 for cross-platform on Apple Silicon)
    let status = Command::new("docker")
        .args([
            "build",
            "--platform",
            "linux/amd64",
            "-t",
            &image_uri,
            "-f",
            &dockerfile_path.to_string_lossy(),
            &context_dir.to_string_lossy(),
        ])
        .status()
        .context("Failed to run docker build")?;

    if !status.success() {
        anyhow::bail!("docker build failed");
    }

    // ECR login
    eprintln!("  Logging into ECR...");
    let login_password = Command::new("aws")
        .args([
            "ecr",
            "get-login-password",
            "--region",
            region,
        ])
        .output()
        .context("Failed to get ECR login password")?;

    if !login_password.status.success() {
        anyhow::bail!(
            "ECR login failed: {}",
            String::from_utf8_lossy(&login_password.stderr)
        );
    }

    let docker_login = Command::new("docker")
        .args(["login", "--username", "AWS", "--password-stdin", &ecr_registry])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .context("Failed to start docker login")?;

    use std::io::Write;
    if let Some(mut stdin) = docker_login.stdin {
        stdin.write_all(&login_password.stdout)?;
    }

    // Push
    eprintln!("  Pushing to ECR...");
    let status = Command::new("docker")
        .args(["push", &image_uri])
        .status()
        .context("Failed to push to ECR")?;

    if !status.success() {
        anyhow::bail!("docker push failed");
    }

    eprintln!("  Image pushed: {}", image_uri);
    Ok(image_uri)
}

/// Run the container locally for testing (CPU mode, no GPU).
pub fn dry_run(config: &Config, config_dir: &Path) -> Result<()> {
    let image_tag = format!("cloud-run-local:{}", config.experiment.name);

    let dockerfile = config
        .container
        .dockerfile
        .as_ref()
        .context("No dockerfile specified")?;
    let dockerfile_path = config_dir.join(dockerfile);
    let context_dir = config
        .container
        .context
        .as_ref()
        .map(|c| config_dir.join(c))
        .unwrap_or_else(|| {
            dockerfile_path
                .parent()
                .unwrap_or(config_dir)
                .to_path_buf()
        });

    eprintln!("  Building for local dry-run...");
    let status = Command::new("docker")
        .args([
            "build",
            "--platform",
            "linux/amd64",
            "-t",
            &image_tag,
            "-f",
            &dockerfile_path.to_string_lossy(),
            &context_dir.to_string_lossy(),
        ])
        .status()
        .context("Failed to run docker build")?;

    if !status.success() {
        anyhow::bail!("docker build failed");
    }

    // Build docker run command with data mounts
    let mut docker_args = vec!["run".to_string(), "--rm".to_string()];

    // Note: no --gpus flag for local dry-run (CPU mode)
    for src in &config.data.sources {
        // For dry-run, user must have data locally at the same path
        // or we skip data mounts
        eprintln!("  NOTE: data mount {} → {} (must exist locally)", src.s3, src.path);
    }

    // Create a temp results dir
    let results_dir = std::env::temp_dir().join(format!("cloud-run-{}", config.experiment.name));
    std::fs::create_dir_all(&results_dir)?;
    docker_args.extend([
        "-v".to_string(),
        format!("{}:/results", results_dir.display()),
    ]);

    for (k, v) in &config.run.env {
        docker_args.extend(["-e".to_string(), format!("{}={}", k, v)]);
    }

    docker_args.push(image_tag);

    // Add command
    for part in config.run.command.split_whitespace() {
        docker_args.push(part.to_string());
    }

    eprintln!("  Running locally (CPU mode)...");
    let status = Command::new("docker")
        .args(&docker_args)
        .status()
        .context("Failed to run container")?;

    if !status.success() {
        anyhow::bail!("Container exited with code {}", status.code().unwrap_or(-1));
    }

    eprintln!("  Dry-run completed. Results in: {}", results_dir.display());
    Ok(())
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
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn ensure_ecr_repo(_registry: &str, region: &str) -> Result<()> {
    // Try to create the repo — ignore "already exists" error
    let output = Command::new("aws")
        .args([
            "ecr",
            "create-repository",
            "--repository-name",
            "kenoma/experiments",
            "--region",
            region,
        ])
        .output()
        .context("Failed to check ECR repository")?;

    // Ignore errors (repo likely already exists)
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !stderr.contains("RepositoryAlreadyExistsException") {
            eprintln!("  Warning: ECR repo check: {}", stderr.trim());
        }
    }
    Ok(())
}
