#!/usr/bin/env bash
# Launch distributed imbalance CPCV across multiple EC2 instances.
#
# Shards 45 CPCV folds across N machines (default: 4× c7a.8xlarge).
# Each machine runs its fold subset serially (--parallel-folds 1),
# peaking at ~41 GB RAM — well within c7a.8xlarge's 64 GB.
# 4 × 32 vCPU = 128 vCPU, maxing the spot quota.
#
# After all machines finish, aggregate locally:
#   aws s3 sync s3://bucket/run/ ./partial/ --exclude "*" --include "*/fold-results-partial.json"
#   cargo run --release -p event-backtest -- --mode aggregate --data-dir ./partial/ --output-dir ./final/
#
# Instance shuts down on completion only (no auto-termination timer).
#
# Usage: bash research/03-event-lob-probability/scripts/ec2-launch-imbalance-cpcv-distributed.sh [OPTIONS]
#   --n-machines N       Number of machines (default: 4)
#   --data-prefix S3PFX  S3 prefix for bilateral Parquets (required)
#   --ofi-threshold N    Minimum |ofi_fast| (default: 2.0)
#   --geometry T:S       Single geometry to evaluate (default: 10:5)
#   --margin N           Decision margin above null (default: 0.02)
#   --dry-run            Print config and fold ranges, exit without launching
set -euo pipefail

# ── Parse args ────────────────────────────────────────────────
N_MACHINES=4
OFI_THRESHOLD=2.0
GEOMETRY="10:5"
MARGIN=0.02
DATA_PREFIX=""
DRY_RUN=false

while [[ $# -gt 0 ]]; do
    case $1 in
        --n-machines) N_MACHINES="$2"; shift 2 ;;
        --ofi-threshold) OFI_THRESHOLD="$2"; shift 2 ;;
        --geometry) GEOMETRY="$2"; shift 2 ;;
        --margin) MARGIN="$2"; shift 2 ;;
        --data-prefix) DATA_PREFIX="$2"; shift 2 ;;
        --dry-run) DRY_RUN=true; shift ;;
        *) echo "Unknown arg: $1"; exit 1 ;;
    esac
done

if [[ -z "${DATA_PREFIX}" ]]; then
    echo "ERROR: --data-prefix is required (S3 path to bilateral Parquets)"
    echo "  e.g. --data-prefix cloud-runs/bilateral-export-XXXXX/events-bilateral"
    exit 1
fi

# ── Config ──────────────────────────────────────────────────────
S3_BUCKET="kenoma-labs-research"
AWS_REGION="us-east-1"
SSH_KEY="kenoma-research"
AMI_ID="ami-0f3caa1cf4417e51b"          # Amazon Linux 2023 x86_64
IAM_PROFILE="cloud-run-ec2"
INSTANCE_TYPE="c7a.8xlarge"             # 32 vCPU, 64 GB RAM

TOTAL_FOLDS=45  # C(10,2) = 45 for n_groups=10, k_test=2
RUN_ID="imbalance-cpcv-dist-$(date +%Y%m%dT%H%M%SZ)-$(openssl rand -hex 4)"
S3_PREFIX="cloud-runs/${RUN_ID}"
PROJECT_ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"

# ── Compute fold ranges ───────────────────────────────────────
declare -a FOLD_RANGES
FOLDS_PER_MACHINE=$((TOTAL_FOLDS / N_MACHINES))
REMAINDER=$((TOTAL_FOLDS % N_MACHINES))

START=0
for i in $(seq 0 $((N_MACHINES - 1))); do
    # Distribute remainder across first N machines
    EXTRA=0
    if [[ $i -lt $REMAINDER ]]; then
        EXTRA=1
    fi
    END=$((START + FOLDS_PER_MACHINE + EXTRA))
    FOLD_RANGES[$i]="${START}:${END}"
    START=$END
done

echo "══════════════════════════════════════════"
echo "  Distributed Imbalance CPCV — EC2 Launch"
echo "══════════════════════════════════════════"
echo "  Run ID:          ${RUN_ID}"
echo "  Machines:        ${N_MACHINES}× ${INSTANCE_TYPE} (spot)"
echo "  Total vCPU:      $((N_MACHINES * 32))"
echo "  Region:          ${AWS_REGION}"
echo "  Data source:     s3://${S3_BUCKET}/${DATA_PREFIX}/"
echo "  Output:          s3://${S3_BUCKET}/${S3_PREFIX}/"
echo "  OFI threshold:   ${OFI_THRESHOLD}"
echo "  Geometry:        ${GEOMETRY}"
echo "  Margin:          ${MARGIN}"
echo ""
echo "  Fold ranges:"
for i in $(seq 0 $((N_MACHINES - 1))); do
    echo "    machine-${i}: folds ${FOLD_RANGES[$i]}"
done
echo ""

if $DRY_RUN; then
    echo "  [DRY RUN] Would launch ${N_MACHINES}× ${INSTANCE_TYPE} spot instances."
    echo ""
    echo "  After completion, aggregate with:"
    echo "    aws s3 sync s3://${S3_BUCKET}/${S3_PREFIX}/ ./partial/ --exclude '*' --include '*/fold-results-partial.json'"
    echo "    cargo run --release -p event-backtest -- --mode aggregate --data-dir ./partial/ --output-dir ./final/ --margin ${MARGIN} --commission 1.24"
    exit 0
fi

# ── Step 1: Package source code (once) ─────────────────────────
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

# ── Step 2: Upload source to S3 ──────────────────────────────
echo "[2/3] Uploading source to S3..."
aws s3 cp "${SRC_TAR}" "s3://${S3_BUCKET}/${S3_PREFIX}/source.tar.gz" \
    --region "${AWS_REGION}" --quiet
echo "  Uploaded source.tar.gz"
rm -f "${SRC_TAR}"

# ── Step 3: Launch machines ────────────────────────────────────
echo "[3/3] Launching ${N_MACHINES} instances..."

# Create shared security group
VPC_ID=$(aws ec2 describe-vpcs --filters "Name=isDefault,Values=true" \
    --query "Vpcs[0].VpcId" --output text --region "${AWS_REGION}")

SG_ID=$(aws ec2 create-security-group \
    --group-name "cloud-run-${RUN_ID}" \
    --description "Distributed imbalance CPCV ${RUN_ID}" \
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

# Root volume: 100 GB for data + build artifacts
BDM='[
  {"DeviceName":"/dev/xvda","Ebs":{"VolumeSize":100,"VolumeType":"gp3","Iops":6000,"Throughput":400,"DeleteOnTermination":true}}
]'

declare -a INSTANCE_IDS

for MACHINE_IDX in $(seq 0 $((N_MACHINES - 1))); do
    FOLD_RANGE="${FOLD_RANGES[$MACHINE_IDX]}"
    MACHINE_PREFIX="machine-${MACHINE_IDX}"

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
OFI_THRESHOLD="__OFI_THRESHOLD__"
GEOMETRY="__GEOMETRY__"
MARGIN="__MARGIN__"
FOLD_RANGE="__FOLD_RANGE__"
MACHINE_ID="__MACHINE_ID__"
MACHINE_PREFIX="__MACHINE_PREFIX__"

WORK=/work
DATA_DIR=${WORK}/events-bilateral
RESULTS_DIR=${WORK}/results
mkdir -p ${DATA_DIR} ${RESULTS_DIR}

# Heartbeat (every 60s)
(while true; do
    date -u +%Y-%m-%dT%H:%M:%SZ | aws s3 cp - "s3://${S3_BUCKET}/${S3_PREFIX}/${MACHINE_PREFIX}/heartbeat" --region "${AWS_REGION}" 2>/dev/null
    sleep 60
done) &

# ── Phase 1: Download bilateral Parquets ──
echo "=== Phase 1: Downloading bilateral Parquets ==="
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

# ── Phase 2: Run Imbalance CPCV (fold subset) ──
echo "=== Phase 2: Running folds ${FOLD_RANGE} (machine ${MACHINE_ID}) ==="
CPCV_START=$(date +%s)

${BACKTEST_BIN} \
    --data-dir "${DATA_DIR}" \
    --output-dir "${RESULTS_DIR}" \
    --mode imbalance \
    --ofi-threshold "${OFI_THRESHOLD}" \
    --geometry "${GEOMETRY}" \
    --parallel-folds 1 \
    --fold-range "${FOLD_RANGE}" \
    --margin "${MARGIN}" \
    --s3-output "s3://${S3_BUCKET}/${S3_PREFIX}/${MACHINE_PREFIX}/" \
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
echo "Folds ${FOLD_RANGE} completed in $((CPCV_END - CPCV_START))s ($((($CPCV_END - $CPCV_START) / 60)) minutes)"

# ── Upload results ──
echo "=== Uploading results ==="
cp /var/log/experiment.log ${RESULTS_DIR}/experiment.log
aws s3 sync ${RESULTS_DIR}/ "s3://${S3_BUCKET}/${S3_PREFIX}/${MACHINE_PREFIX}/results/" --region "${AWS_REGION}"

echo "0" | aws s3 cp - "s3://${S3_BUCKET}/${S3_PREFIX}/${MACHINE_PREFIX}/exit_code" --region "${AWS_REGION}"
echo "=== Machine ${MACHINE_ID} done. Shutting down. ==="

shutdown -h now
BOOTSTRAP
)

    # Inject variables
    USER_DATA="${USER_DATA//__RUN_ID__/${RUN_ID}}"
    USER_DATA="${USER_DATA//__S3_BUCKET__/${S3_BUCKET}}"
    USER_DATA="${USER_DATA//__S3_PREFIX__/${S3_PREFIX}}"
    USER_DATA="${USER_DATA//__S3_DATA_PREFIX__/${DATA_PREFIX}}"
    USER_DATA="${USER_DATA//__AWS_REGION__/${AWS_REGION}}"
    USER_DATA="${USER_DATA//__OFI_THRESHOLD__/${OFI_THRESHOLD}}"
    USER_DATA="${USER_DATA//__GEOMETRY__/${GEOMETRY}}"
    USER_DATA="${USER_DATA//__MARGIN__/${MARGIN}}"
    USER_DATA="${USER_DATA//__FOLD_RANGE__/${FOLD_RANGE}}"
    USER_DATA="${USER_DATA//__MACHINE_ID__/${MACHINE_IDX}}"
    USER_DATA="${USER_DATA//__MACHINE_PREFIX__/${MACHINE_PREFIX}}"

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
        --tag-specifications "ResourceType=instance,Tags=[{Key=Name,Value=dist-cpcv-${MACHINE_PREFIX}-${RUN_ID}},{Key=ManagedBy,Value=cloud-run},{Key=RunId,Value=${RUN_ID}},{Key=MachineId,Value=${MACHINE_IDX}}]" \
        --client-token "cloud-run-spot-${RUN_ID:0:50}-m${MACHINE_IDX}" \
        --region "${AWS_REGION}" \
        --query "Instances[0].InstanceId" --output text)

    if [[ -z "${INSTANCE_ID}" ]] || [[ "${INSTANCE_ID}" == "None" ]]; then
        echo "  ERROR: Spot request failed for machine-${MACHINE_IDX}. Cleaning up..."
        # Terminate any already-launched instances
        for prev_id in "${INSTANCE_IDS[@]}"; do
            aws ec2 terminate-instances --instance-ids "${prev_id}" --region "${AWS_REGION}" >/dev/null 2>&1 || true
        done
        aws ec2 delete-security-group --group-id "${SG_ID}" --region "${AWS_REGION}" 2>/dev/null || true
        exit 1
    fi

    INSTANCE_IDS+=("${INSTANCE_ID}")
    echo "  machine-${MACHINE_IDX}: ${INSTANCE_ID} (folds ${FOLD_RANGE})"
done

# Wait for public IPs
echo ""
echo -n "  Waiting for IPs"
declare -a PUBLIC_IPS
for attempt in $(seq 1 30); do
    ALL_READY=true
    for i in $(seq 0 $((N_MACHINES - 1))); do
        if [[ -z "${PUBLIC_IPS[$i]:-}" ]]; then
            IP=$(aws ec2 describe-instances --instance-ids "${INSTANCE_IDS[$i]}" \
                --query "Reservations[0].Instances[0].PublicIpAddress" --output text \
                --region "${AWS_REGION}" 2>/dev/null || echo "None")
            if [[ "${IP}" != "None" && "${IP}" != "" ]]; then
                PUBLIC_IPS[$i]="${IP}"
            else
                ALL_READY=false
            fi
        fi
    done
    if $ALL_READY; then break; fi
    echo -n "."
    sleep 5
done
echo ""

echo ""
echo "══════════════════════════════════════════"
echo "  Launched ${N_MACHINES} machines!"
echo "══════════════════════════════════════════"
for i in $(seq 0 $((N_MACHINES - 1))); do
    echo "  machine-${i}: ${INSTANCE_IDS[$i]} (${PUBLIC_IPS[$i]:-pending}) — folds ${FOLD_RANGES[$i]}"
done
echo ""
echo "  Run ID: ${RUN_ID}"
echo ""
echo "  SSH:"
for i in $(seq 0 $((N_MACHINES - 1))); do
    echo "    ssh -i ~/.ssh/kenoma-research.pem ec2-user@${PUBLIC_IPS[$i]:-<ip>}  # machine-${i}"
done
echo ""
echo "  Monitor:"
echo "    # Check heartbeats"
for i in $(seq 0 $((N_MACHINES - 1))); do
    echo "    aws s3 cp s3://${S3_BUCKET}/${S3_PREFIX}/machine-${i}/heartbeat - --region ${AWS_REGION} 2>/dev/null"
done
echo ""
echo "    # Check completion"
for i in $(seq 0 $((N_MACHINES - 1))); do
    echo "    aws s3 cp s3://${S3_BUCKET}/${S3_PREFIX}/machine-${i}/exit_code - --region ${AWS_REGION} 2>/dev/null"
done
echo ""
echo "  After all machines finish, aggregate:"
echo "    aws s3 sync s3://${S3_BUCKET}/${S3_PREFIX}/ ./partial-${RUN_ID}/ --exclude '*' --include '*/fold-results-partial.json'"
echo "    cargo run --release -p event-backtest -- \\"
echo "      --mode aggregate \\"
echo "      --data-dir ./partial-${RUN_ID}/ \\"
echo "      --output-dir ./final-${RUN_ID}/ \\"
echo "      --margin ${MARGIN} \\"
echo "      --commission 1.24"
echo ""
echo "  Cleanup SG (after all terminate):"
echo "    aws ec2 delete-security-group --group-id ${SG_ID} --region ${AWS_REGION}"
echo "══════════════════════════════════════════"
