#!/usr/bin/env bash
# Launch CPCV XGBoost training on EC2.
#
# Downloads event Parquets from S3, builds event-backtest, runs 45-fold CPCV
# with 4 parallel folds, uploads JSON report, and shuts down.
#
# Target: c7a.32xlarge (128 vCPU, 256 GB RAM, spot ~$1.70/hr)
# Estimated: ~3 hours, ~$5 on spot.
#
# Usage: bash research/03-event-lob-probability/scripts/ec2-launch-cpcv.sh [OPTIONS]
#   --subsample-pct N   Eval-point subsampling percentage (default: 15)
#   --parallel-folds N  Folds to run in parallel (default: 4)
#   --margin N          Decision margin above null (default: 0.02)
#   --dry-run           Print config and exit without launching
set -euo pipefail

# ── Parse args ────────────────────────────────────────────────
SUBSAMPLE_PCT=15
PARALLEL_FOLDS=4
MARGIN=0.02
DRY_RUN=false

while [[ $# -gt 0 ]]; do
    case $1 in
        --subsample-pct) SUBSAMPLE_PCT="$2"; shift 2 ;;
        --parallel-folds) PARALLEL_FOLDS="$2"; shift 2 ;;
        --margin) MARGIN="$2"; shift 2 ;;
        --dry-run) DRY_RUN=true; shift ;;
        *) echo "Unknown arg: $1"; exit 1 ;;
    esac
done

# ── Config ──────────────────────────────────────────────────────
S3_BUCKET="kenoma-labs-research"
S3_DATA_PREFIX="cloud-runs/event-export-full-20260302T140214Z-a1fbd38a/events-bbo"
AWS_REGION="us-east-1"
SSH_KEY="kenoma-research"
AMI_ID="ami-0f3caa1cf4417e51b"          # Amazon Linux 2023 x86_64
IAM_PROFILE="cloud-run-ec2"
INSTANCE_TYPE="c7a.32xlarge"             # 128 vCPU, 256 GB RAM

RUN_ID="event-cpcv-$(date +%Y%m%dT%H%M%SZ)-$(openssl rand -hex 4)"
S3_PREFIX="cloud-runs/${RUN_ID}"
PROJECT_ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"

echo "══════════════════════════════════════════"
echo "  Event-Level CPCV Training — EC2 Launch"
echo "══════════════════════════════════════════"
echo "  Run ID:          ${RUN_ID}"
echo "  Instance:        ${INSTANCE_TYPE}"
echo "  Region:          ${AWS_REGION}"
echo "  Data source:     s3://${S3_BUCKET}/${S3_DATA_PREFIX}/"
echo "  Output:          s3://${S3_BUCKET}/${S3_PREFIX}/"
echo "  Subsample:       ${SUBSAMPLE_PCT}%"
echo "  Parallel folds:  ${PARALLEL_FOLDS}"
echo "  Margin:          ${MARGIN}"
echo ""

if $DRY_RUN; then
    echo "  [DRY RUN] Would launch ${INSTANCE_TYPE} spot instance."
    exit 0
fi

# ── Step 1: Package source code ────────────────────────────────
echo "[1/4] Packaging source code..."
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
echo "[2/4] Uploading to S3..."
aws s3 cp "${SRC_TAR}" "s3://${S3_BUCKET}/${S3_PREFIX}/source.tar.gz" \
    --region "${AWS_REGION}" --quiet
echo "  Uploaded source.tar.gz"
rm -f "${SRC_TAR}"

# ── Step 3: Generate user-data bootstrap ────────────────────────
echo "[3/4] Launching EC2 instance..."

USER_DATA=$(cat <<'BOOTSTRAP'
#!/bin/bash
set -eo pipefail
export HOME=/root
export PATH="/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin"
exec > /var/log/experiment.log 2>&1
echo "=== Bootstrap start: $(date -u) ==="

# Injected variables
RUN_ID="__RUN_ID__"
S3_BUCKET="__S3_BUCKET__"
S3_PREFIX="__S3_PREFIX__"
S3_DATA_PREFIX="__S3_DATA_PREFIX__"
AWS_REGION="__AWS_REGION__"
SUBSAMPLE_PCT="__SUBSAMPLE_PCT__"
PARALLEL_FOLDS="__PARALLEL_FOLDS__"
MARGIN="__MARGIN__"

WORK=/work
DATA_DIR=${WORK}/events-bbo
RESULTS_DIR=${WORK}/results
mkdir -p ${DATA_DIR} ${RESULTS_DIR}

# Heartbeat (every 60s)
(while true; do
    date -u +%Y-%m-%dT%H:%M:%SZ | aws s3 cp - "s3://${S3_BUCKET}/${S3_PREFIX}/heartbeat" --region "${AWS_REGION}" 2>/dev/null
    sleep 60
done) &

# ── Phase 1: Download event Parquets from S3 ──
echo "=== Phase 1: Downloading event Parquets ==="
DOWNLOAD_START=$(date +%s)
aws s3 sync "s3://${S3_BUCKET}/${S3_DATA_PREFIX}/" "${DATA_DIR}/" \
    --region "${AWS_REGION}" \
    --only-show-errors
DOWNLOAD_END=$(date +%s)
NFILES=$(ls ${DATA_DIR}/*.parquet 2>/dev/null | wc -l)
DATA_SIZE=$(du -sh ${DATA_DIR} | cut -f1)
echo "  Downloaded ${NFILES} files (${DATA_SIZE}) in $((DOWNLOAD_END - DOWNLOAD_START))s"

# ── Install dependencies ──
echo "=== Installing dependencies ==="
dnf install -y gcc gcc-c++ make cmake git openssl-devel clang 2>&1 | tail -5

# ── Install Rust ──
echo "=== Installing Rust ==="
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable 2>&1 | tail -3
source "$HOME/.cargo/env"
rustc --version

# ── Download and build source ──
echo "=== Building event-backtest ==="
cd ${WORK}
aws s3 cp "s3://${S3_BUCKET}/${S3_PREFIX}/source.tar.gz" source.tar.gz --region "${AWS_REGION}"
mkdir -p src
tar -xzf source.tar.gz -C src/

# Pre-build xgboost-sys
cd ${WORK}
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
cargo build --release --target-dir ${WORK}/src/target 2>&1 | tail -5

cd ${WORK}/src
BUILD_START=$(date +%s)
cargo build --release --package event-backtest 2>&1 | tail -20
BUILD_END=$(date +%s)
echo "Build completed in $((BUILD_END - BUILD_START))s"

BACKTEST_BIN=${WORK}/src/target/release/event-backtest

# ── Phase 4: Run CPCV ──
echo "=== Phase 4: Running CPCV (${PARALLEL_FOLDS} parallel folds, ${SUBSAMPLE_PCT}% subsample) ==="
CPCV_START=$(date +%s)

${BACKTEST_BIN} \
    --data-dir "${DATA_DIR}" \
    --output-dir "${RESULTS_DIR}" \
    --mode cpcv \
    --subsample-pct "${SUBSAMPLE_PCT}" \
    --parallel-folds "${PARALLEL_FOLDS}" \
    --margin "${MARGIN}" \
    --s3-output "s3://${S3_BUCKET}/${S3_PREFIX}/cpcv-report.json" \
    --n-groups 10 \
    --k-test 2 \
    --max-depth 6 \
    --eta 0.01 \
    --min-child-weight 100 \
    --subsample 0.6 \
    --colsample-bytree 0.7 \
    --n-estimators 3000 \
    --early-stopping 100 \
    --max-bin 256 \
    --commission 1.24

CPCV_END=$(date +%s)
echo "CPCV completed in $((CPCV_END - CPCV_START))s ($((($CPCV_END - $CPCV_START) / 60)) minutes)"

# ── Upload results ──
echo "=== Uploading results ==="
cp /var/log/experiment.log ${RESULTS_DIR}/experiment.log
aws s3 sync ${RESULTS_DIR}/ "s3://${S3_BUCKET}/${S3_PREFIX}/results/" --region "${AWS_REGION}"

echo "0" | aws s3 cp - "s3://${S3_BUCKET}/${S3_PREFIX}/exit_code" --region "${AWS_REGION}"
echo "=== Done. Shutting down. ==="

shutdown -h now
BOOTSTRAP
)

# Inject variables
USER_DATA="${USER_DATA//__RUN_ID__/${RUN_ID}}"
USER_DATA="${USER_DATA//__S3_BUCKET__/${S3_BUCKET}}"
USER_DATA="${USER_DATA//__S3_PREFIX__/${S3_PREFIX}}"
USER_DATA="${USER_DATA//__S3_DATA_PREFIX__/${S3_DATA_PREFIX}}"
USER_DATA="${USER_DATA//__AWS_REGION__/${AWS_REGION}}"
USER_DATA="${USER_DATA//__SUBSAMPLE_PCT__/${SUBSAMPLE_PCT}}"
USER_DATA="${USER_DATA//__PARALLEL_FOLDS__/${PARALLEL_FOLDS}}"
USER_DATA="${USER_DATA//__MARGIN__/${MARGIN}}"

# ── Step 4: Create security group + launch ──────────────────────
VPC_ID=$(aws ec2 describe-vpcs --filters "Name=isDefault,Values=true" \
    --query "Vpcs[0].VpcId" --output text --region "${AWS_REGION}")

SG_ID=$(aws ec2 create-security-group \
    --group-name "cloud-run-${RUN_ID}" \
    --description "CPCV training ${RUN_ID}" \
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

# Root volume: 200 GB for data + build artifacts
BDM='[
  {"DeviceName":"/dev/xvda","Ebs":{"VolumeSize":200,"VolumeType":"gp3","Iops":6000,"Throughput":400,"DeleteOnTermination":true}}
]'

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
    --tag-specifications "ResourceType=instance,Tags=[{Key=Name,Value=event-cpcv-${RUN_ID}},{Key=ManagedBy,Value=cloud-run},{Key=RunId,Value=${RUN_ID}}]" \
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
        --tag-specifications "ResourceType=instance,Tags=[{Key=Name,Value=event-cpcv-${RUN_ID}},{Key=ManagedBy,Value=cloud-run},{Key=RunId,Value=${RUN_ID}}]" \
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
echo "    aws s3 cp s3://${S3_BUCKET}/${S3_PREFIX}/cpcv-report.json . --region ${AWS_REGION}"
echo "    aws s3 sync s3://${S3_BUCKET}/${S3_PREFIX}/results/ research/03-event-lob-probability/results/cpcv-${RUN_ID}/ --region ${AWS_REGION}"
echo ""
echo "  Cleanup SG (after termination):"
echo "    aws ec2 delete-security-group --group-id ${SG_ID} --region ${AWS_REGION}"
echo "══════════════════════════════════════════"
