#!/usr/bin/env bash
# Launch univariate gate test on EC2.
#
# Downloads event Parquets from S3, builds gate-test, runs it,
# uploads JSON report. Lightweight job: ~50KB memory for accumulators,
# I/O bound reading 21.7GB of Parquets sequentially.
#
# Instance shuts down on completion only (no auto-termination timer).
#
# Usage: bash research/03-event-lob-probability/scripts/ec2-launch-gate-test.sh
set -euo pipefail

# ── Config ──────────────────────────────────────────────────────
S3_BUCKET="kenoma-labs-research"
AWS_REGION="us-east-1"
SSH_KEY="kenoma-research"
AMI_ID="ami-0f3caa1cf4417e51b"          # Amazon Linux 2023 x86_64
IAM_PROFILE="cloud-run-ec2"

# Source data: the full export with flow features
DATA_S3_PREFIX="cloud-runs/event-export-full-20260303T071423Z-9ad1b1de/events-bbo"

INSTANCE_TYPE="c7a.xlarge"              # 4 vCPU, 8 GB — plenty for streaming
RUN_ID="gate-test-$(date +%Y%m%dT%H%M%SZ)-$(openssl rand -hex 4)"
S3_PREFIX="cloud-runs/${RUN_ID}"
PROJECT_ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"

echo "══════════════════════════════════════════"
echo "  Gate Test EC2 Launch"
echo "══════════════════════════════════════════"
echo "  Run ID:    ${RUN_ID}"
echo "  Instance:  ${INSTANCE_TYPE}"
echo "  Region:    ${AWS_REGION}"
echo "  Data:      s3://${S3_BUCKET}/${DATA_S3_PREFIX}/"
echo "  S3:        s3://${S3_BUCKET}/${S3_PREFIX}/"
echo ""

# ── Step 1: Package source code ────────────────────────────────
echo "[1/3] Packaging source code..."
SRC_TAR=$(mktemp /tmp/mbo-dl-rust-src-XXXXXX.tar.gz)
tar -czf "${SRC_TAR}" \
    -C "${PROJECT_ROOT}" \
    --exclude='target' \
    --exclude='.git' \
    --exclude='.kit/results' \
    --exclude='.kit/experiments' \
    --exclude='orchestration-kit' \
    --exclude='research/01-bar-level-cpcv/results' \
    --exclude='research/02-tick-level-serial/results' \
    --exclude='.worktrees' \
    Cargo.toml Cargo.lock \
    crates/ tools/ src/ tests/
echo "  Source: $(du -h "${SRC_TAR}" | cut -f1)"

# ── Step 2: Upload to S3 ───────────────────────────────────────
echo "[2/3] Uploading to S3..."
aws s3 cp "${SRC_TAR}" "s3://${S3_BUCKET}/${S3_PREFIX}/source.tar.gz" \
    --region "${AWS_REGION}" --quiet
echo "  Uploaded source.tar.gz"
rm -f "${SRC_TAR}"

# ── Step 3: Launch EC2 ──────────────────────────────────────────
echo "[3/3] Launching EC2 instance..."

USER_DATA=$(cat <<'BOOTSTRAP'
#!/bin/bash
set -eo pipefail
export HOME=/root
export PATH="/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin"
exec > /var/log/experiment.log 2>&1
echo "=== Bootstrap start: $(date -u) ==="

# Injected variables (replaced below)
RUN_ID="__RUN_ID__"
S3_BUCKET="__S3_BUCKET__"
S3_PREFIX="__S3_PREFIX__"
AWS_REGION="__AWS_REGION__"
DATA_S3_PREFIX="__DATA_S3_PREFIX__"

WORK=/work
mkdir -p ${WORK}/results ${WORK}/events-bbo

# Heartbeat (every 60s)
(while true; do
    date -u +%Y-%m-%dT%H:%M:%SZ | aws s3 cp - "s3://${S3_BUCKET}/${S3_PREFIX}/heartbeat" --region "${AWS_REGION}" 2>/dev/null
    sleep 60
done) &

# ── Download event Parquets from S3 ──
echo "=== Downloading event Parquets ==="
DL_START=$(date +%s)
aws s3 sync "s3://${S3_BUCKET}/${DATA_S3_PREFIX}/" ${WORK}/events-bbo/ --region "${AWS_REGION}"
DL_END=$(date +%s)
N_FILES=$(ls ${WORK}/events-bbo/*.parquet 2>/dev/null | wc -l)
DL_SIZE=$(du -sh ${WORK}/events-bbo/ | cut -f1)
echo "  Downloaded ${N_FILES} files (${DL_SIZE}) in $((DL_END - DL_START))s"

# ── Install build dependencies ──
echo "=== Installing dependencies ==="
dnf install -y gcc gcc-c++ make cmake git openssl-devel clang 2>&1 | tail -5

# ── Install Rust ──
echo "=== Installing Rust ==="
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable 2>&1 | tail -3
source "$HOME/.cargo/env"
rustc --version
cargo --version

# ── Download source from S3 ──
echo "=== Downloading source ==="
cd ${WORK}
aws s3 cp "s3://${S3_BUCKET}/${S3_PREFIX}/source.tar.gz" source.tar.gz --region "${AWS_REGION}"
mkdir -p src
tar -xzf source.tar.gz -C src/

# ── Build gate-test (release) ──
echo "=== Building gate-test ==="
cd ${WORK}/src
BUILD_START=$(date +%s)
cargo build --release --package gate-test 2>&1 | tail -20
BUILD_END=$(date +%s)
echo "Build completed in $((BUILD_END - BUILD_START))s"

GATE_BIN=${WORK}/src/target/release/gate-test

# ── Run gate test ──
echo "=== Running gate test ==="
RUN_START=$(date +%s)
${GATE_BIN} \
    --data-dir ${WORK}/events-bbo \
    --output ${WORK}/results/gate-test-report.json \
    --tick-size 0.25
RUN_END=$(date +%s)
echo "Gate test completed in $((RUN_END - RUN_START))s"

# ── Upload results ──
echo "=== Uploading results ==="
cp /var/log/experiment.log ${WORK}/results/experiment.log
aws s3 sync ${WORK}/results/ "s3://${S3_BUCKET}/${S3_PREFIX}/results/" --region "${AWS_REGION}"

# Write exit code
echo "0" | aws s3 cp - "s3://${S3_BUCKET}/${S3_PREFIX}/exit_code" --region "${AWS_REGION}"
echo "=== Done. Shutting down. ==="

shutdown -h now
BOOTSTRAP
)

# Inject variables into user-data
USER_DATA="${USER_DATA//__RUN_ID__/${RUN_ID}}"
USER_DATA="${USER_DATA//__S3_BUCKET__/${S3_BUCKET}}"
USER_DATA="${USER_DATA//__S3_PREFIX__/${S3_PREFIX}}"
USER_DATA="${USER_DATA//__AWS_REGION__/${AWS_REGION}}"
USER_DATA="${USER_DATA//__DATA_S3_PREFIX__/${DATA_S3_PREFIX}}"

# Create security group
VPC_ID=$(aws ec2 describe-vpcs --filters "Name=isDefault,Values=true" \
    --query "Vpcs[0].VpcId" --output text --region "${AWS_REGION}")

SG_ID=$(aws ec2 create-security-group \
    --group-name "cloud-run-${RUN_ID}" \
    --description "Gate test ${RUN_ID}" \
    --vpc-id "${VPC_ID}" \
    --region "${AWS_REGION}" \
    --output text --query "GroupId")

aws ec2 authorize-security-group-ingress \
    --group-id "${SG_ID}" \
    --protocol tcp --port 22 --cidr 0.0.0.0/0 \
    --region "${AWS_REGION}" >/dev/null

aws ec2 create-tags --resources "${SG_ID}" \
    --tags Key=ManagedBy,Value=cloud-run Key=RunId,Value="${RUN_ID}" \
    --region "${AWS_REGION}"

# Block device: root 50GB gp3 (enough for 22GB parquets + build artifacts)
BDM='[{"DeviceName":"/dev/xvda","Ebs":{"VolumeSize":50,"VolumeType":"gp3","DeleteOnTermination":true}}]'

# Try spot first, fall back to on-demand
LAUNCH_MODE="spot"
SPOT_ERR=$(mktemp)
INSTANCE_ID=$(aws ec2 run-instances \
    --image-id "${AMI_ID}" \
    --instance-type "${INSTANCE_TYPE}" \
    --key-name "${SSH_KEY}" \
    --security-group-ids "${SG_ID}" \
    --iam-instance-profile Name="${IAM_PROFILE}" \
    --user-data "${USER_DATA}" \
    --instance-initiated-shutdown-behavior terminate \
    --instance-market-options '{"MarketType":"spot","SpotOptions":{"SpotInstanceType":"one-time"}}' \
    --block-device-mappings "${BDM}" \
    --tag-specifications "ResourceType=instance,Tags=[{Key=Name,Value=gate-test-${RUN_ID}},{Key=ManagedBy,Value=cloud-run},{Key=RunId,Value=${RUN_ID}}]" \
    --client-token "cloud-run-spot-${RUN_ID:0:59}" \
    --region "${AWS_REGION}" \
    --query "Instances[0].InstanceId" --output text 2>"${SPOT_ERR}") || true

if [[ -z "${INSTANCE_ID}" ]] || [[ "${INSTANCE_ID}" == "None" ]]; then
    echo "  Spot failed: $(cat ${SPOT_ERR})"
    echo "  Launching on-demand..."
    LAUNCH_MODE="on-demand"
    INSTANCE_ID=$(aws ec2 run-instances \
        --image-id "${AMI_ID}" \
        --instance-type "${INSTANCE_TYPE}" \
        --key-name "${SSH_KEY}" \
        --security-group-ids "${SG_ID}" \
        --iam-instance-profile Name="${IAM_PROFILE}" \
        --user-data "${USER_DATA}" \
        --instance-initiated-shutdown-behavior terminate \
        --block-device-mappings "${BDM}" \
        --tag-specifications "ResourceType=instance,Tags=[{Key=Name,Value=gate-test-${RUN_ID}},{Key=ManagedBy,Value=cloud-run},{Key=RunId,Value=${RUN_ID}}]" \
        --client-token "cloud-run-od-${RUN_ID:0:61}" \
        --region "${AWS_REGION}" \
        --query "Instances[0].InstanceId" --output text)
fi
rm -f "${SPOT_ERR}"

echo "  Instance: ${INSTANCE_ID} (${LAUNCH_MODE})"

# Wait for public IP
echo -n "  Waiting for IP"
for i in $(seq 1 30); do
    PUBLIC_IP=$(aws ec2 describe-instances --instance-ids "${INSTANCE_ID}" \
        --query "Reservations[0].Instances[0].PublicIpAddress" --output text \
        --region "${AWS_REGION}" 2>/dev/null || echo "None")
    if [[ "${PUBLIC_IP}" != "None" && "${PUBLIC_IP}" != "" ]]; then
        break
    fi
    echo -n "."
    sleep 5
done
echo ""

echo ""
echo "══════════════════════════════════════════"
echo "  Launched!"
echo "══════════════════════════════════════════"
echo "  Instance:  ${INSTANCE_ID} (${INSTANCE_TYPE}, ${LAUNCH_MODE})"
echo "  Public IP: ${PUBLIC_IP:-pending}"
echo "  Run ID:    ${RUN_ID}"
echo ""
echo "  SSH:       ssh -i ~/.ssh/kenoma-research.pem ec2-user@${PUBLIC_IP:-<ip>}"
echo "  Logs:      ssh ... 'sudo tail -f /var/log/experiment.log'"
echo ""
echo "  Poll results:"
echo "    aws s3 cp s3://${S3_BUCKET}/${S3_PREFIX}/exit_code - --region ${AWS_REGION} 2>/dev/null"
echo ""
echo "  Download report:"
echo "    aws s3 cp s3://${S3_BUCKET}/${S3_PREFIX}/results/gate-test-report.json research/03-event-lob-probability/results/ --region ${AWS_REGION}"
echo ""
echo "  Cleanup SG (after termination):"
echo "    aws ec2 delete-security-group --group-id ${SG_ID} --region ${AWS_REGION}"
echo "══════════════════════════════════════════"
