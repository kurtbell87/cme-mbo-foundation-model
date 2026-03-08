use anyhow::{Context, Result};
use std::path::Path;
use std::process::Command;

use crate::config::Config;

/// Build a Docker image from the experiment's Dockerfile and push to ECR (AWS).
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

    // Docker buildx build (--platform linux/amd64 for Fargate/EC2 compatibility)
    // --provenance=false avoids OCI attestation manifests that break platform matching
    let status = Command::new("docker")
        .args([
            "buildx",
            "build",
            "--platform",
            "linux/amd64",
            "--provenance=false",
            "--load",
            "-t",
            &image_uri,
            "-f",
            &dockerfile_path.to_string_lossy(),
            &context_dir.to_string_lossy(),
        ])
        .status()
        .context("Failed to run docker buildx build")?;

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

/// Build Docker image on a remote EC2 instance and push to ECR.
/// Used when local build is impractical (e.g., xgboost-sys on Apple Silicon).
///
/// The builder instance gets full cloud-run safety: TTL tags, heartbeat monitoring,
/// spot-with-fallback, idle detection, and auto-terminate on completion.
pub fn build_remote(
    config: &Config,
    config_dir: &Path,
    builder_instance_type: Option<String>,
    follow: bool,
) -> Result<String> {
    let region = &config.instance.region;
    let account_id = get_account_id(region)?;
    let image_uri = config.ecr_image(&account_id);
    let ecr_registry = format!("{}.dkr.ecr.{}.amazonaws.com", account_id, region);

    // Ensure ECR repo exists before launching
    ensure_ecr_repo(&ecr_registry, region)?;

    // Resolve dockerfile and context from config
    let dockerfile = config
        .container
        .dockerfile
        .as_ref()
        .context("build-remote requires 'dockerfile' in [container]")?;
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
    let context_dir = context_dir
        .canonicalize()
        .unwrap_or(context_dir);

    // Compute relative dockerfile path within the build context
    let rel_dockerfile = dockerfile_path
        .canonicalize()
        .unwrap_or_else(|_| dockerfile_path.clone())
        .strip_prefix(&context_dir)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| dockerfile.clone());

    let build_id = format!(
        "build-{}-{}",
        config.experiment.name,
        chrono::Utc::now().format("%Y%m%dT%H%M%SZ")
    );
    let s3_base = format!("{}/{}", config.results.s3_prefix, build_id);

    eprintln!("  Build ID: {}", build_id);
    eprintln!("  Image:    {}", image_uri);
    eprintln!("  Dockerfile: {} (in context)", rel_dockerfile);

    // 1. Create and upload build context tarball
    eprintln!("\n[1/3] Packaging build context...");
    let tarball = std::env::temp_dir().join(format!("{}.tar.gz", build_id));
    create_build_context_tar(&context_dir, &tarball)?;

    let tar_size = std::fs::metadata(&tarball)?.len();
    eprintln!("  Context: {:.1} MB", tar_size as f64 / 1024.0 / 1024.0);

    eprintln!("  Uploading to {}...", s3_base);
    let status = Command::new("aws")
        .args([
            "s3",
            "cp",
            &tarball.to_string_lossy(),
            &format!("{}/context.tar.gz", s3_base),
            "--region",
            region,
        ])
        .status()
        .context("Failed to upload context to S3")?;

    if !status.success() {
        anyhow::bail!("Failed to upload build context to S3");
    }
    let _ = std::fs::remove_file(&tarball);

    // 2. Generate builder user-data and launch
    eprintln!("\n[2/3] Launching builder instance...");
    let userdata = generate_builder_userdata(
        &build_id,
        &s3_base,
        region,
        &ecr_registry,
        &image_uri,
        &rel_dockerfile,
    );
    let userdata_file =
        std::env::temp_dir().join(format!("cloud-run-builder-{}.sh", build_id));
    std::fs::write(&userdata_file, &userdata)?;

    // Create builder config with appropriate overrides
    let builder_type = builder_instance_type.unwrap_or_else(|| "c7a.4xlarge".to_string());
    let mut builder_config = config.clone();
    builder_config.experiment.name = format!("build-{}", config.experiment.name);
    builder_config.instance.instance_type = builder_type.clone();
    builder_config.instance.gpu = false;
    builder_config.instance.max_runtime_minutes = 30;
    builder_config.instance.root_volume_gb = 80;
    builder_config.heartbeat.idle_timeout_minutes = 20;

    let ami_id = crate::launch::find_cpu_ami(region)?;

    let (instance_id, pricing) = if builder_config.instance.spot {
        match crate::launch::run_ec2_launch(
            &builder_config,
            &build_id,
            &ami_id,
            &userdata_file,
            region,
            true,
        ) {
            Ok(id) => (id, "spot"),
            Err(e) => {
                eprintln!("  Spot failed: {}", e);
                eprintln!("  Falling back to on-demand...");
                let id = crate::launch::run_ec2_launch(
                    &builder_config,
                    &build_id,
                    &ami_id,
                    &userdata_file,
                    region,
                    false,
                )?;
                (id, "on-demand")
            }
        }
    } else {
        let id = crate::launch::run_ec2_launch(
            &builder_config,
            &build_id,
            &ami_id,
            &userdata_file,
            region,
            false,
        )?;
        (id, "on-demand")
    };

    let _ = std::fs::remove_file(&userdata_file);

    eprintln!("  Instance: {} ({}, {})", instance_id, builder_type, pricing);

    if !follow {
        eprintln!("\n  Build launched. Check with:");
        eprintln!("    cloud-run status {}", build_id);
        return Ok(image_uri);
    }

    // 3. Monitor build
    eprintln!("\n[3/3] Monitoring build...");
    let backend = crate::monitor::Backend::Aws;
    match crate::monitor::monitor(&builder_config, &build_id, &instance_id, &backend) {
        Ok(status) => {
            eprintln!();
            if status.exit_code == 0 {
                eprintln!("Build complete! Image: {}", image_uri);
                Ok(image_uri)
            } else {
                anyhow::bail!(
                    "Build failed (exit code {}). Logs: cloud-run logs {}",
                    status.exit_code,
                    build_id,
                )
            }
        }
        Err(e) => {
            anyhow::bail!(
                "Build monitoring failed: {}. Check: cloud-run logs {}",
                e,
                build_id
            )
        }
    }
}

/// Build a Docker image and push to Docker Hub (for RunPod).
/// Requires the user to have already run `docker login`.
pub fn build_and_push_dockerhub(config: &Config, config_dir: &Path) -> Result<String> {
    let repo = config
        .container
        .dockerhub_repo
        .as_ref()
        .context("container.dockerhub_repo is required for RunPod builds")?;
    let image_uri = format!("{}:{}", repo, config.experiment.name);

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

    // Build for linux/amd64 (RunPod runs x86_64)
    let status = Command::new("docker")
        .args([
            "buildx",
            "build",
            "--platform",
            "linux/amd64",
            "--provenance=false",
            "--load",
            "-t",
            &image_uri,
            "-f",
            &dockerfile_path.to_string_lossy(),
            &context_dir.to_string_lossy(),
        ])
        .status()
        .context("Failed to run docker buildx build")?;

    if !status.success() {
        anyhow::bail!("docker build failed");
    }

    // Push to Docker Hub (user must have run `docker login` already)
    eprintln!("  Pushing to Docker Hub...");
    let status = Command::new("docker")
        .args(["push", &image_uri])
        .status()
        .context("Failed to push to Docker Hub")?;

    if !status.success() {
        anyhow::bail!(
            "docker push failed. Have you run `docker login`?"
        );
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

/// Create a tarball of the build context, excluding build artifacts and VCS.
fn create_build_context_tar(context_dir: &Path, output: &Path) -> Result<()> {
    let status = Command::new("tar")
        .args([
            "-czf",
            &output.to_string_lossy(),
            "--exclude=target",
            "--exclude=.git",
            "--exclude=.kit/results",
            "--exclude=data",
            "--exclude=*.dbn.zst",
            "--exclude=*.parquet",
            "--exclude=node_modules",
            "-C",
            &context_dir.to_string_lossy(),
            ".",
        ])
        .status()
        .context("Failed to create build context tarball")?;

    if !status.success() {
        anyhow::bail!("tar failed to create context archive");
    }
    Ok(())
}

/// Generate user-data script for a builder EC2 instance.
/// Downloads context from S3, runs docker build, pushes to ECR, then shuts down.
fn generate_builder_userdata(
    build_id: &str,
    s3_base: &str,
    region: &str,
    ecr_registry: &str,
    image_uri: &str,
    rel_dockerfile: &str,
) -> String {
    format!(
        r#"#!/bin/bash
set -eo pipefail

RUN_ID="{build_id}"
S3_BASE="{s3_base}"
REGION="{region}"
ECR_REGISTRY="{ecr_registry}"
IMAGE_URI="{image_uri}"
DOCKERFILE="{rel_dockerfile}"

exec > >(tee /var/log/cloud-run.log) 2>&1
echo "cloud-run-build: $RUN_ID starting at $(date -u +%FT%TZ)"

# ECS-optimized AMI has Docker; ensure daemon is running
systemctl start docker 2>/dev/null || true

# ECR login
aws ecr get-login-password --region $REGION \
    | docker login --username AWS --password-stdin $ECR_REGISTRY

# Download and extract build context
echo "cloud-run-build: downloading context..."
aws s3 cp "$S3_BASE/context.tar.gz" /tmp/context.tar.gz --region $REGION
mkdir -p /build && cd /build
tar xzf /tmp/context.tar.gz
rm /tmp/context.tar.gz
echo "cloud-run-build: context extracted ($(du -sh /build | cut -f1))"

# Heartbeat loop — runs independently of the build
(
    while true; do
        printf '{{"ts":"%s","status":"building","run_id":"%s"}}' \
            "$(date -u +%FT%TZ)" "$RUN_ID" \
            | aws s3 cp - "$S3_BASE/heartbeat.json" --region $REGION 2>/dev/null
        sleep 30
    done
) &
HEARTBEAT_PID=$!

# Build Docker image
echo "cloud-run-build: starting docker build at $(date -u)"
set +e
docker build --platform linux/amd64 \
    -t "$IMAGE_URI" \
    -f "$DOCKERFILE" \
    . 2>&1 | tee /var/log/docker-build.log
EXIT_CODE=${{PIPESTATUS[0]}}
set -e

if [ $EXIT_CODE -eq 0 ]; then
    echo "cloud-run-build: pushing to ECR..."
    docker push "$IMAGE_URI" 2>&1 | tee -a /var/log/docker-build.log
    PUSH_EXIT=${{PIPESTATUS[0]}}
    if [ $PUSH_EXIT -ne 0 ]; then
        EXIT_CODE=$PUSH_EXIT
    fi
fi

# Cleanup
kill $HEARTBEAT_PID 2>/dev/null || true
END_TS="$(date -u +%FT%TZ)"
echo "cloud-run-build: finished with exit code $EXIT_CODE at $END_TS"

# Upload logs (docker-build.log as experiment.log so monitor can stream it)
aws s3 cp /var/log/docker-build.log "$S3_BASE/experiment.log" --region $REGION 2>/dev/null || true
aws s3 cp /var/log/cloud-run.log "$S3_BASE/cloud-run.log" --region $REGION 2>/dev/null || true

# Signal completion
printf '{{"ts":"%s","status":"completed","exit_code":%d,"run_id":"%s","image":"%s"}}' \
    "$END_TS" $EXIT_CODE "$RUN_ID" "$IMAGE_URI" \
    | aws s3 cp - "$S3_BASE/status.json" --region $REGION

echo "cloud-run-build: shutting down"
shutdown -h now
"#
    )
}
