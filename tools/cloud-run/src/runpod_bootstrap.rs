use crate::config::Config;

/// Generate the bootstrap bash script for a RunPod pod.
///
/// This script runs inside the pod container (no nested Docker).
/// It installs AWS CLI, starts heartbeat, runs the experiment,
/// uploads results to S3, and stops the pod on completion.
pub fn generate(config: &Config, run_id: &str) -> String {
    let rp = config.runpod.as_ref().expect("runpod config required");
    let region = &config.instance.region;
    let s3_base = format!("{}/{}", config.results.s3_prefix, run_id);
    let heartbeat_interval = config.heartbeat.interval_seconds;
    let gpu_type = &rp.gpu_type;
    let command = &config.run.command;

    // Data staging commands
    let data_stage: String = config
        .data
        .sources
        .iter()
        .map(|src| {
            format!(
                "mkdir -p \"{path}\"\naws s3 cp \"{s3}\" \"{path}\" --region {region}\n",
                s3 = src.s3,
                path = src.path,
            )
        })
        .collect();

    format!(
        r##"#!/bin/bash
set -eo pipefail

RUN_ID="{run_id}"
S3_BASE="{s3_base}"
REGION="{region}"
GPU_TYPE="{gpu_type}"
HEARTBEAT_INTERVAL={heartbeat_interval}

exec > >(tee /tmp/experiment.log) 2>&1
echo "cloud-run: RunPod bootstrap starting at $(date -u +%FT%TZ)"
echo "cloud-run: run_id=$RUN_ID gpu=$GPU_TYPE"

# ── Install AWS CLI ────────────────────────────────
if ! command -v aws &>/dev/null; then
    echo "cloud-run: Installing AWS CLI..."
    curl -s "https://awscli.amazonaws.com/awscli-exe-linux-x86_64.zip" -o /tmp/awscliv2.zip
    cd /tmp && unzip -q awscliv2.zip && ./aws/install --update 2>/dev/null
    cd /
    rm -rf /tmp/awscliv2.zip /tmp/aws
fi

# ── Stage data from S3 ────────────────────────────
{data_stage}
# ── Heartbeat ──────────────────────────────────────
mkdir -p /results
(
    while true; do
        printf '{{"ts":"%s","status":"running","run_id":"%s","gpu_type":"%s"}}' \
            "$(date -u +%FT%TZ)" "$RUN_ID" "$GPU_TYPE" \
            | aws s3 cp - "$S3_BASE/heartbeat.json" --region $REGION 2>/dev/null
        sleep $HEARTBEAT_INTERVAL
    done
) &
HEARTBEAT_PID=$!

# ── Run experiment ─────────────────────────────────
echo "cloud-run: starting experiment at $(date -u)"
set +e
{command} 2>&1 | tee -a /tmp/experiment.log
EXIT_CODE=${{PIPESTATUS[0]}}
set -e

END_TS="$(date -u +%FT%TZ)"
echo "cloud-run: experiment exited with code $EXIT_CODE at $END_TS"

# ── Cleanup ────────────────────────────────────────
kill $HEARTBEAT_PID 2>/dev/null || true

# Upload results
aws s3 sync /results/ "$S3_BASE/results/" --region $REGION 2>/dev/null || true
aws s3 cp /tmp/experiment.log "$S3_BASE/experiment.log" --region $REGION 2>/dev/null || true

# Signal completion
printf '{{"ts":"%s","status":"completed","exit_code":%d,"run_id":"%s","gpu_type":"%s"}}' \
    "$END_TS" $EXIT_CODE "$RUN_ID" "$GPU_TYPE" \
    | aws s3 cp - "$S3_BASE/status.json" --region $REGION

echo "cloud-run: results uploaded, stopping pod"

# Stop pod via RunPod API (RUNPOD_POD_ID is auto-injected by RunPod)
if [ -n "${{RUNPOD_POD_ID:-}}" ] && [ -n "${{RUNPOD_API_KEY:-}}" ]; then
    curl -s -X POST https://api.runpod.io/graphql \
        -H "Authorization: Bearer $RUNPOD_API_KEY" \
        -H "Content-Type: application/json" \
        -d '{{"query": "mutation {{ podStop(input: {{podId: \"'"$RUNPOD_POD_ID"'\"}}) {{ id desiredStatus }} }}"}}' \
        > /dev/null 2>&1 || true
fi
"##
    )
}
