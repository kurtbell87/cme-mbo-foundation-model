use crate::config::Config;

/// Generate the user-data bash script for the EC2 instance.
///
/// This script is intentionally minimal — just docker pull + docker run.
/// No pip, no conda, no venv. The container has everything.
pub fn generate(config: &Config, run_id: &str, ecr_image: &str, ecr_registry: &str) -> String {
    let region = &config.instance.region;
    let s3_base = format!("{}/{}", config.results.s3_prefix, run_id);
    let heartbeat_interval = config.heartbeat.interval_seconds;

    // Data staging commands
    let data_stage: String = config
        .data
        .sources
        .iter()
        .map(|src| {
            let host_path = format!("/data{}", src.path);
            // Ensure parent directory exists
            let parent = std::path::Path::new(&host_path)
                .parent()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|| "/data".to_string());
            format!(
                "mkdir -p \"{parent}\"\naws s3 cp \"{s3}\" \"{host_path}\" --region {region}\n",
                s3 = src.s3,
            )
        })
        .collect();

    // Docker volume mounts for data
    let data_mounts: String = config
        .data
        .sources
        .iter()
        .map(|src| {
            let host_path = format!("/data{}", src.path);
            format!("-v \"{host_path}:{}\" ", src.path)
        })
        .collect();

    // Environment variables
    let env_flags: String = config
        .run
        .env
        .iter()
        .map(|(k, v)| format!("-e \"{k}={v}\" "))
        .collect();

    let command = &config.run.command;

    format!(
        r#"#!/bin/bash
set -eo pipefail

RUN_ID="{run_id}"
S3_BASE="{s3_base}"
REGION="{region}"

exec > >(tee /var/log/cloud-run.log) 2>&1
echo "cloud-run: $RUN_ID starting at $(date -u)"

# ── ECR login ───────────────────────────────────────
aws ecr get-login-password --region $REGION \
    | docker login --username AWS --password-stdin {ecr_registry}

# ── Pull image ──────────────────────────────────────
docker pull {ecr_image}

# ── Stage data from S3 ─────────────────────────────
{data_stage}
# ── Heartbeat ───────────────────────────────────────
mkdir -p /results
(
    while true; do
        printf '{{"ts":"%s","status":"running","run_id":"%s"}}' \
            "$(date -u +%FT%TZ)" "$RUN_ID" \
            | aws s3 cp - "$S3_BASE/heartbeat.json" --region $REGION 2>/dev/null
        sleep {heartbeat_interval}
    done
) &
HEARTBEAT_PID=$!

# ── Run experiment ──────────────────────────────────
echo "cloud-run: starting container at $(date -u)"
set +e
docker run --gpus all \
    {data_mounts}\
    -v /results:/results \
    {env_flags}\
    {ecr_image} \
    {command} 2>&1 | tee /var/log/experiment.log
EXIT_CODE=${{PIPESTATUS[0]}}
set -e

echo "cloud-run: container exited with code $EXIT_CODE at $(date -u)"

# ── Cleanup ─────────────────────────────────────────
kill $HEARTBEAT_PID 2>/dev/null || true

# Upload results
aws s3 sync /results/ "$S3_BASE/results/" --region $REGION 2>/dev/null || true
aws s3 cp /var/log/experiment.log "$S3_BASE/experiment.log" --region $REGION 2>/dev/null || true
aws s3 cp /var/log/cloud-run.log "$S3_BASE/cloud-run.log" --region $REGION 2>/dev/null || true

# Signal completion
printf '{{"ts":"%s","status":"completed","exit_code":%d,"run_id":"%s"}}' \
    "$(date -u +%FT%TZ)" $EXIT_CODE "$RUN_ID" \
    | aws s3 cp - "$S3_BASE/status.json" --region $REGION

echo "cloud-run: results uploaded, shutting down"
shutdown -h now
"#
    )
}
