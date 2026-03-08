use crate::config::Config;

/// Generate the user-data bash script for the EC2 instance.
///
/// This script is intentionally minimal — just docker pull + docker run.
/// No pip, no conda, no venv. The container has everything.
pub fn generate(config: &Config, run_id: &str, ecr_image: &str, ecr_registry: &str) -> String {
    let region = &config.instance.region;
    let s3_base = format!("{}/{}", config.results.s3_prefix, run_id);
    let heartbeat_interval = config.heartbeat.interval_seconds;
    let instance_type = &config.instance.instance_type;
    let pricing_model = if config.instance.spot {
        "spot"
    } else {
        "on-demand"
    };

    // Data staging commands
    let data_stage: String = config
        .data
        .sources
        .iter()
        .map(|src| {
            let host_path = format!("/data{}", src.path);
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

    // GPU flag only if instance needs GPU
    let gpu_flag = if config.instance.gpu {
        "--gpus all"
    } else {
        ""
    };

    let command = &config.run.command;

    format!(
        r#"#!/bin/bash
set -eo pipefail

RUN_ID="{run_id}"
S3_BASE="{s3_base}"
REGION="{region}"
INSTANCE_TYPE="{instance_type}"
PRICING_MODEL="{pricing_model}"
LAUNCH_TS="$(date -u +%FT%TZ)"

exec > >(tee /var/log/cloud-run.log) 2>&1
echo "cloud-run: $RUN_ID starting at $LAUNCH_TS"
echo "cloud-run: instance=$INSTANCE_TYPE pricing=$PRICING_MODEL"

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
        printf '{{"ts":"%s","status":"running","run_id":"%s","instance_type":"%s","pricing_model":"%s"}}' \
            "$(date -u +%FT%TZ)" "$RUN_ID" "$INSTANCE_TYPE" "$PRICING_MODEL" \
            | aws s3 cp - "$S3_BASE/heartbeat.json" --region $REGION 2>/dev/null
        sleep {heartbeat_interval}
    done
) &
HEARTBEAT_PID=$!

# ── Run experiment ──────────────────────────────────
echo "cloud-run: starting container at $(date -u)"
set +e
docker run {gpu_flag} \
    {data_mounts}\
    -v /results:/results \
    {env_flags}\
    {ecr_image} \
    {command} 2>&1 | tee /var/log/experiment.log
EXIT_CODE=${{PIPESTATUS[0]}}
set -e

END_TS="$(date -u +%FT%TZ)"
echo "cloud-run: container exited with code $EXIT_CODE at $END_TS"

# ── Cleanup ─────────────────────────────────────────
kill $HEARTBEAT_PID 2>/dev/null || true

# Upload results
aws s3 sync /results/ "$S3_BASE/results/" --region $REGION 2>/dev/null || true
aws s3 cp /var/log/experiment.log "$S3_BASE/experiment.log" --region $REGION 2>/dev/null || true
aws s3 cp /var/log/cloud-run.log "$S3_BASE/cloud-run.log" --region $REGION 2>/dev/null || true

# Signal completion with cost metadata
printf '{{"ts":"%s","status":"completed","exit_code":%d,"run_id":"%s","instance_type":"%s","pricing_model":"%s","launch_ts":"%s"}}' \
    "$END_TS" $EXIT_CODE "$RUN_ID" "$INSTANCE_TYPE" "$PRICING_MODEL" "$LAUNCH_TS" \
    | aws s3 cp - "$S3_BASE/status.json" --region $REGION

echo "cloud-run: results uploaded, shutting down"
shutdown -h now
"#
    )
}
