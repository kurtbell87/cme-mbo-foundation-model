#!/usr/bin/env bash
# Barrier sweep: test directional signal with different serial execution barriers.
# Tick series data reused from previous run (already in S3).
# Runs 3 configs: original 19:7, symmetric 19:19, pure time-exit (no barriers).
#
# Usage: bash .kit/scripts/ec2-barrier-sweep.sh
set -euo pipefail

# ── Config ──────────────────────────────────────────────────────
S3_BUCKET="kenoma-labs-research"
AWS_REGION="us-east-1"
SSH_KEY="kenoma-research"
INSTANCE_TYPE="c7a.32xlarge"            # 128 vCPU, 256 GB
AMI_ID="ami-0f3caa1cf4417e51b"          # Amazon Linux 2023 x86_64
IAM_PROFILE="cloud-run-ec2"
DBN_SNAPSHOT="snap-0efa355754c9a329d"
# Tick series from previous run
TICK_S3="cloud-runs/cpcv-20260301T193617Z-300f1a48/tick-series"
RUN_ID="barrier-sweep-$(date +%Y%m%dT%H%M%SZ)-$(openssl rand -hex 4)"
S3_PREFIX="cloud-runs/${RUN_ID}"

PROJECT_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
FEATURES_DIR="${PROJECT_ROOT}/.kit/results/label-geometry-1h/geom_19_7"

echo "══════════════════════════════════════════"
echo "  Barrier Sweep EC2 Launch"
echo "══════════════════════════════════════════"
echo "  Run ID:    ${RUN_ID}"
echo "  Instance:  ${INSTANCE_TYPE}"
echo "  Configs:   19:7 (baseline), 19:19 (symmetric), time-exit-only"
echo ""

# ── Step 1: Package source ────────────────────────────────────
echo "[1/3] Packaging source code..."
SRC_TAR=$(mktemp /tmp/mbo-dl-rust-src-XXXXXX.tar.gz)
tar -czf "${SRC_TAR}" \
    -C "${PROJECT_ROOT}" \
    --exclude='target' \
    --exclude='.git' \
    --exclude='.kit/results' \
    --exclude='.kit/experiments' \
    --exclude='orchestration-kit' \
    Cargo.toml Cargo.lock \
    crates/ tools/ src/ tests/ .kit/scripts/
echo "  Source: $(du -h "${SRC_TAR}" | cut -f1)"

# ── Step 2: Upload source + feature parquets ──────────────────
echo "[2/3] Uploading to S3..."
aws s3 cp "${SRC_TAR}" "s3://${S3_BUCKET}/${S3_PREFIX}/source.tar.gz" \
    --region "${AWS_REGION}" --quiet
echo "  Uploaded source.tar.gz"

aws s3 sync "${FEATURES_DIR}/" "s3://${S3_BUCKET}/${S3_PREFIX}/features/" \
    --region "${AWS_REGION}" --quiet --exclude '*-ticks.parquet'
echo "  Uploaded feature parquets"

rm -f "${SRC_TAR}"

# ── Step 3: Generate user-data bootstrap ──────────────────────
echo "[3/3] Launching EC2 instance..."

USER_DATA=$(cat <<'BOOTSTRAP'
#!/bin/bash
set -eo pipefail
export HOME=/root
export PATH="/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin"
exec > /var/log/experiment.log 2>&1
echo "=== Bootstrap start: $(date -u) ==="

RUN_ID="__RUN_ID__"
S3_BUCKET="__S3_BUCKET__"
S3_PREFIX="__S3_PREFIX__"
AWS_REGION="__AWS_REGION__"
TICK_S3="__TICK_S3__"

WORK=/work
mkdir -p ${WORK}/results ${WORK}/features ${WORK}/tick-series

# Heartbeat
(while true; do
    date -u +%Y-%m-%dT%H:%M:%SZ | aws s3 cp - "s3://${S3_BUCKET}/${S3_PREFIX}/heartbeat" --region "${AWS_REGION}" 2>/dev/null
    sleep 60
done) &

# Results sync
(while true; do
    sleep 120
    aws s3 sync ${WORK}/results/ "s3://${S3_BUCKET}/${S3_PREFIX}/results/" --region "${AWS_REGION}" --quiet 2>/dev/null || true
done) &

# ── Install + build ──
echo "=== Installing dependencies ==="
dnf install -y gcc gcc-c++ make cmake git openssl-devel clang 2>&1 | tail -3

echo "=== Installing Rust ==="
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable 2>&1 | tail -3
source "$HOME/.cargo/env"

echo "=== Downloading source + data ==="
cd ${WORK}
aws s3 cp "s3://${S3_BUCKET}/${S3_PREFIX}/source.tar.gz" source.tar.gz --region "${AWS_REGION}"
mkdir -p src
tar -xzf source.tar.gz -C src/
aws s3 sync "s3://${S3_BUCKET}/${S3_PREFIX}/features/" features/ --region "${AWS_REGION}"
echo "  $(ls features/*.parquet 2>/dev/null | wc -l) feature files"

# Download tick series from previous run
echo "=== Downloading tick series ==="
aws s3 sync "s3://${S3_BUCKET}/${TICK_S3}/" tick-series/ --region "${AWS_REGION}"
TICK_COUNT=$(ls tick-series/*-ticks.parquet 2>/dev/null | wc -l)
echo "  ${TICK_COUNT} tick series files"

# Pre-build xgboost-sys
echo "=== Pre-building xgboost-sys ==="
mkdir -p xgb-prebuild && cd xgb-prebuild
cat > Cargo.toml <<'XGBEOF'
[package]
name = "xgb-prebuild"
version = "0.1.0"
edition = "2021"
[dependencies]
xgboost-sys = "0.1.2"
XGBEOF
mkdir -p src && echo "use xgboost_sys;" > src/lib.rs
cargo build --release --target-dir ${WORK}/src/target 2>&1 | tail -3

echo "=== Building cpcv-backtest ==="
cd ${WORK}/src
BUILD_START=$(date +%s)
cargo build --release --package cpcv-backtest 2>&1 | tail -10
BUILD_END=$(date +%s)
echo "Build completed in $((BUILD_END - BUILD_START))s"

NCPU=$(nproc)
NPAR=$((NCPU / 4))
if [[ ${NPAR} -lt 4 ]]; then NPAR=4; fi
BIN=./target/release/cpcv-backtest
TICK_FLAG="--tick-series-dir ${WORK}/tick-series"

# ══════════════════════════════════════════════════════════════════
# Config 1: Original 19:7 (baseline — serial uses training barriers)
# ══════════════════════════════════════════════════════════════════
echo ""
echo "══════════════════════════════════════════"
echo "  Config 1/3: Original 19:7"
echo "══════════════════════════════════════════"
T1=$(date +%s)
${BIN} --features-dir ${WORK}/features --all-days \
    --target 19 --stop 7 --tick-size 0.25 \
    --parallel-folds ${NPAR} ${TICK_FLAG} \
    --output ${WORK}/results/cpcv-19-7.json 2>&1
echo "  CPCV 19:7 done in $(($(date +%s) - T1))s"

T1=$(date +%s)
${BIN} --features-dir ${WORK}/features --all-days \
    --target 19 --stop 7 --tick-size 0.25 \
    --temporal-holdout 81 ${TICK_FLAG} \
    --output ${WORK}/results/holdout-19-7.json 2>&1
echo "  Holdout 19:7 done in $(($(date +%s) - T1))s"

# ══════════════════════════════════════════════════════════════════
# Config 2: Symmetric 19:19 (widen stop to match target)
# ══════════════════════════════════════════════════════════════════
echo ""
echo "══════════════════════════════════════════"
echo "  Config 2/3: Symmetric 19:19"
echo "══════════════════════════════════════════"
T1=$(date +%s)
${BIN} --features-dir ${WORK}/features --all-days \
    --target 19 --stop 7 --tick-size 0.25 \
    --serial-target 19 --serial-stop 19 \
    --parallel-folds ${NPAR} ${TICK_FLAG} \
    --output ${WORK}/results/cpcv-19-19.json 2>&1
echo "  CPCV 19:19 done in $(($(date +%s) - T1))s"

T1=$(date +%s)
${BIN} --features-dir ${WORK}/features --all-days \
    --target 19 --stop 7 --tick-size 0.25 \
    --serial-target 19 --serial-stop 19 \
    --temporal-holdout 81 ${TICK_FLAG} \
    --output ${WORK}/results/holdout-19-19.json 2>&1
echo "  Holdout 19:19 done in $(($(date +%s) - T1))s"

# ══════════════════════════════════════════════════════════════════
# Config 3: Pure time exit (no barriers — hold until 3600s horizon)
# ══════════════════════════════════════════════════════════════════
echo ""
echo "══════════════════════════════════════════"
echo "  Config 3/3: Time exit only (no barriers)"
echo "══════════════════════════════════════════"
T1=$(date +%s)
${BIN} --features-dir ${WORK}/features --all-days \
    --target 19 --stop 7 --tick-size 0.25 \
    --serial-target 99999 --serial-stop 99999 \
    --parallel-folds ${NPAR} ${TICK_FLAG} \
    --output ${WORK}/results/cpcv-time-exit.json 2>&1
echo "  CPCV time-exit done in $(($(date +%s) - T1))s"

T1=$(date +%s)
${BIN} --features-dir ${WORK}/features --all-days \
    --target 19 --stop 7 --tick-size 0.25 \
    --serial-target 99999 --serial-stop 99999 \
    --temporal-holdout 81 ${TICK_FLAG} \
    --output ${WORK}/results/holdout-time-exit.json 2>&1
echo "  Holdout time-exit done in $(($(date +%s) - T1))s"

# ── Upload final results ──
echo ""
echo "=== Uploading results ==="
cp /var/log/experiment.log ${WORK}/results/experiment.log
aws s3 sync ${WORK}/results/ "s3://${S3_BUCKET}/${S3_PREFIX}/results/" --region "${AWS_REGION}"

echo "0" | aws s3 cp - "s3://${S3_BUCKET}/${S3_PREFIX}/exit_code" --region "${AWS_REGION}"
echo "=== Done. Shutting down. ==="
shutdown -h now
BOOTSTRAP
)

# Inject variables
USER_DATA="${USER_DATA//__RUN_ID__/${RUN_ID}}"
USER_DATA="${USER_DATA//__S3_BUCKET__/${S3_BUCKET}}"
USER_DATA="${USER_DATA//__S3_PREFIX__/${S3_PREFIX}}"
USER_DATA="${USER_DATA//__AWS_REGION__/${AWS_REGION}}"
USER_DATA="${USER_DATA//__TICK_S3__/${TICK_S3}}"

# ── Launch ────────────────────────────────────────────────────
VPC_ID=$(aws ec2 describe-vpcs --filters "Name=isDefault,Values=true" \
    --query "Vpcs[0].VpcId" --output text --region "${AWS_REGION}")

SG_ID=$(aws ec2 create-security-group \
    --group-name "cloud-run-${RUN_ID}" \
    --description "Barrier sweep ${RUN_ID}" \
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

# No DBN volume needed — tick data from S3
INSTANCE_ID=$(aws ec2 run-instances \
    --image-id "${AMI_ID}" \
    --instance-type "${INSTANCE_TYPE}" \
    --key-name "${SSH_KEY}" \
    --security-group-ids "${SG_ID}" \
    --iam-instance-profile Name="${IAM_PROFILE}" \
    --user-data "${USER_DATA}" \
    --instance-initiated-shutdown-behavior terminate \
    --instance-market-options '{"MarketType":"spot","SpotOptions":{"SpotInstanceType":"one-time"}}' \
    --block-device-mappings '[{"DeviceName":"/dev/xvda","Ebs":{"VolumeSize":100,"VolumeType":"gp3","DeleteOnTermination":true}}]' \
    --tag-specifications "ResourceType=instance,Tags=[{Key=Name,Value=barrier-sweep-${RUN_ID}},{Key=ManagedBy,Value=cloud-run},{Key=RunId,Value=${RUN_ID}}]" \
    --client-token "cloud-run-${RUN_ID:0:64}" \
    --region "${AWS_REGION}" \
    --query "Instances[0].InstanceId" --output text 2>&1) || true

if [[ "${INSTANCE_ID}" == *"error"* ]] || [[ "${INSTANCE_ID}" == *"Error"* ]] || [[ -z "${INSTANCE_ID}" ]]; then
    echo "  Spot failed, launching on-demand..."
    INSTANCE_ID=$(aws ec2 run-instances \
        --image-id "${AMI_ID}" \
        --instance-type "${INSTANCE_TYPE}" \
        --key-name "${SSH_KEY}" \
        --security-group-ids "${SG_ID}" \
        --iam-instance-profile Name="${IAM_PROFILE}" \
        --user-data "${USER_DATA}" \
        --instance-initiated-shutdown-behavior terminate \
        --block-device-mappings '[{"DeviceName":"/dev/xvda","Ebs":{"VolumeSize":100,"VolumeType":"gp3","DeleteOnTermination":true}}]' \
        --tag-specifications "ResourceType=instance,Tags=[{Key=Name,Value=barrier-sweep-${RUN_ID}},{Key=ManagedBy,Value=cloud-run},{Key=RunId,Value=${RUN_ID}}]" \
        --client-token "cloud-run-od-${RUN_ID:0:61}" \
        --region "${AWS_REGION}" \
        --query "Instances[0].InstanceId" --output text)
fi

echo "  Instance: ${INSTANCE_ID}"

echo -n "  Waiting for IP"
for i in $(seq 1 30); do
    PUBLIC_IP=$(aws ec2 describe-instances --instance-ids "${INSTANCE_ID}" \
        --query "Reservations[0].Instances[0].PublicIpAddress" --output text \
        --region "${AWS_REGION}" 2>/dev/null || echo "None")
    if [[ "${PUBLIC_IP}" != "None" && "${PUBLIC_IP}" != "" ]]; then break; fi
    echo -n "."
    sleep 5
done
echo ""

echo ""
echo "══════════════════════════════════════════"
echo "  Launched!"
echo "══════════════════════════════════════════"
echo "  Instance:  ${INSTANCE_ID} (${INSTANCE_TYPE})"
echo "  Public IP: ${PUBLIC_IP:-pending}"
echo "  Run ID:    ${RUN_ID}"
echo ""
echo "  SSH:       ssh -i ~/.ssh/kenoma-research.pem ec2-user@${PUBLIC_IP:-<ip>}"
echo "  Logs:      ssh ... 'sudo tail -f /var/log/experiment.log'"
echo ""
echo "  Poll: aws s3 cp s3://${S3_BUCKET}/${S3_PREFIX}/exit_code - --region ${AWS_REGION} 2>/dev/null"
echo "  Results: aws s3 sync s3://${S3_BUCKET}/${S3_PREFIX}/results/ .kit/results/ec2-${RUN_ID}/ --region ${AWS_REGION}"
echo "  Cleanup: aws ec2 delete-security-group --group-id ${SG_ID} --region ${AWS_REGION}"
echo "══════════════════════════════════════════"
