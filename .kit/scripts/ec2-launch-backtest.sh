#!/usr/bin/env bash
# Launch tick extraction + CPCV backtest on EC2.
#
# Mounts DBN data from EBS snapshot, builds bar-feature-export + cpcv-backtest,
# extracts tick series in parallel, then runs CPCV + holdout backtests.
# Instance shuts down on completion only (no auto-termination timer).
#
# Usage: bash .kit/scripts/ec2-launch-backtest.sh
set -euo pipefail

# ── Config ──────────────────────────────────────────────────────
S3_BUCKET="kenoma-labs-research"
AWS_REGION="us-east-1"
SSH_KEY="kenoma-research"
INSTANCE_TYPE="c7a.32xlarge"            # 128 vCPU, 256 GB — max parallelism
AMI_ID="ami-0f3caa1cf4417e51b"          # Amazon Linux 2023 x86_64
IAM_PROFILE="cloud-run-ec2"
DBN_SNAPSHOT="snap-0efa355754c9a329d"   # mbo-data-2022 (60GB, 316 DBN files)
RUN_ID="cpcv-$(date +%Y%m%dT%H%M%SZ)-$(openssl rand -hex 4)"
S3_PREFIX="cloud-runs/${RUN_ID}"

PROJECT_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
FEATURES_DIR="${PROJECT_ROOT}/.kit/results/label-geometry-1h/geom_19_7"

echo "══════════════════════════════════════════"
echo "  CPCV Backtest EC2 Launch"
echo "══════════════════════════════════════════"
echo "  Run ID:    ${RUN_ID}"
echo "  Instance:  ${INSTANCE_TYPE}"
echo "  Region:    ${AWS_REGION}"
echo "  DBN snap:  ${DBN_SNAPSHOT}"
echo "  S3:        s3://${S3_BUCKET}/${S3_PREFIX}/"
echo ""

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
    Cargo.toml Cargo.lock \
    crates/ tools/ src/ tests/ .kit/scripts/
echo "  Source: $(du -h "${SRC_TAR}" | cut -f1)"

# ── Step 2: Upload to S3 ───────────────────────────────────────
echo "[2/4] Uploading to S3..."
aws s3 cp "${SRC_TAR}" "s3://${S3_BUCKET}/${S3_PREFIX}/source.tar.gz" \
    --region "${AWS_REGION}" --quiet
echo "  Uploaded source.tar.gz"

# Upload existing feature Parquets (exclude tick files — we'll regenerate on EC2)
aws s3 sync "${FEATURES_DIR}/" "s3://${S3_BUCKET}/${S3_PREFIX}/features/" \
    --region "${AWS_REGION}" --quiet --exclude '*-ticks.parquet'
FEAT_COUNT=$(ls "${FEATURES_DIR}"/*.parquet 2>/dev/null | grep -v ticks | wc -l | tr -d ' ')
echo "  Uploaded ${FEAT_COUNT} Parquet feature files"

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

# Injected variables (replaced below)
RUN_ID="__RUN_ID__"
S3_BUCKET="__S3_BUCKET__"
S3_PREFIX="__S3_PREFIX__"
AWS_REGION="__AWS_REGION__"
# No auto-termination — shutdown only on completion

WORK=/work
DBN_MNT=/mnt/dbn
mkdir -p ${WORK}/results ${WORK}/features ${WORK}/tick-series ${DBN_MNT}

# NO auto-termination timer. Instance shuts down only on successful completion.
# This avoids killing long-running jobs that are still making progress.

# Heartbeat (every 60s) — use to monitor liveness
(while true; do
    date -u +%Y-%m-%dT%H:%M:%SZ | aws s3 cp - "s3://${S3_BUCKET}/${S3_PREFIX}/heartbeat" --region "${AWS_REGION}" 2>/dev/null
    sleep 60
done) &

# Results sync (every 2 minutes)
(while true; do
    sleep 120
    aws s3 sync ${WORK}/results/ "s3://${S3_BUCKET}/${S3_PREFIX}/results/" --region "${AWS_REGION}" --quiet 2>/dev/null || true
done) &

# ── Mount DBN data EBS volume ──
echo "=== Mounting DBN data volume ==="
# Wait for /dev/sdf (attached as xvdf on nitro instances)
for i in $(seq 1 30); do
    if [[ -b /dev/nvme1n1 ]]; then
        DBN_DEV=/dev/nvme1n1
        break
    elif [[ -b /dev/xvdf ]]; then
        DBN_DEV=/dev/xvdf
        break
    fi
    echo "  Waiting for data volume... ($i)"
    sleep 2
done
echo "  Found device: ${DBN_DEV}"
mount -o ro "${DBN_DEV}" ${DBN_MNT}
DBN_DIR=$(find ${DBN_MNT} -name "glbx-mdp3-*.mbo.dbn.zst" -print -quit | xargs dirname)
echo "  DBN dir: ${DBN_DIR}"
echo "  DBN files: $(ls ${DBN_DIR}/glbx-mdp3-*.mbo.dbn.zst | wc -l)"

# ── Install build dependencies ──
echo "=== Installing dependencies ==="
dnf install -y gcc gcc-c++ make cmake git openssl-devel clang parallel 2>&1 | tail -5

# ── Install Rust ──
echo "=== Installing Rust ==="
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable 2>&1 | tail -3
source "$HOME/.cargo/env"
rustc --version
cargo --version

# ── Download source + features from S3 ──
echo "=== Downloading from S3 ==="
cd ${WORK}
aws s3 cp "s3://${S3_BUCKET}/${S3_PREFIX}/source.tar.gz" source.tar.gz --region "${AWS_REGION}"
mkdir -p src
tar -xzf source.tar.gz -C src/
aws s3 sync "s3://${S3_BUCKET}/${S3_PREFIX}/features/" features/ --region "${AWS_REGION}"
echo "Downloaded $(ls features/*.parquet 2>/dev/null | wc -l) Parquet files"

# ── Build xgboost-sys first (needed by xgboost-ffi's build.rs) ──
echo "=== Pre-building xgboost-sys ==="
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
echo "xgboost-sys pre-built"

# ── Build both binaries (release) ──
echo "=== Building bar-feature-export + cpcv-backtest ==="
cd ${WORK}/src
BUILD_START=$(date +%s)
cargo build --release --package bar-feature-export --package cpcv-backtest 2>&1 | tail -20
BUILD_END=$(date +%s)
echo "Build completed in $((BUILD_END - BUILD_START))s"

# ── Tick series extraction (parallel) ──
echo "=== Extracting tick series (parallel) ==="
EXPORT_BIN=${WORK}/src/target/release/bar-feature-export

get_instrument_id() {
    local d=$1
    if   (( d >= 20220103 && d <= 20220318 )); then echo 11355
    elif (( d >= 20220319 && d <= 20220617 )); then echo 13615
    elif (( d >= 20220618 && d <= 20220916 )); then echo 10039
    elif (( d >= 20220917 && d <= 20221216 )); then echo 10299
    elif (( d >= 20221217 && d <= 20221230 )); then echo 2080
    else echo 13615
    fi
}

is_excluded() {
    local d=$1
    local EXCLUDED="20220315 20220316 20220317 20220318 20220614 20220615 20220616 20220617 20220913 20220914 20220915 20220916 20221213 20221214 20221215 20221216"
    if echo " ${EXCLUDED} " | grep -q " ${d} "; then return 0; else return 1; fi
}

export -f get_instrument_id is_excluded
export EXPORT_BIN WORK DBN_DIR

# Build job list
JOB_FILE=${WORK}/tick-jobs.txt
> ${JOB_FILE}
for dbn_file in ${DBN_DIR}/glbx-mdp3-*.mbo.dbn.zst; do
    fname=$(basename "$dbn_file")
    date_str=${fname#glbx-mdp3-}
    date_str=${date_str%.mbo.dbn.zst}
    if is_excluded "$date_str"; then continue; fi
    out_date="${date_str:0:4}-${date_str:4:2}-${date_str:6:2}"
    out_file="${WORK}/features/${out_date}.parquet"
    tick_file="${WORK}/tick-series/${out_date}-ticks.parquet"
    # Skip if tick series already exists and is non-trivial
    if [[ -f "$tick_file" ]] && [[ $(stat -c%s "$tick_file" 2>/dev/null || echo 0) -gt 1000 ]]; then
        continue
    fi
    instrument_id=$(get_instrument_id "$date_str")
    echo "${date_str} ${instrument_id} ${dbn_file} ${out_file}" >> ${JOB_FILE}
done

NJOBS=$(wc -l < ${JOB_FILE})
echo "  ${NJOBS} days to extract"

# Each bar-feature-export process peaks at ~1-1.5 GB RAM.
# Scale concurrency to available cores (capped by memory: ~1.5 GB per job).
NCPU=$(nproc)
MEM_GB=$(awk '/MemTotal/{printf "%d", $2/1048576}' /proc/meminfo)
MAX_BY_MEM=$((MEM_GB / 2))  # 2 GB budget per job (conservative)
NJOBS_PAR=$((NCPU < MAX_BY_MEM ? NCPU : MAX_BY_MEM))
echo "  Parallelism: ${NJOBS_PAR} (${NCPU} cores, ${MEM_GB} GB RAM)"

TICK_START=$(date +%s)
cat ${JOB_FILE} | parallel --colsep ' ' -j ${NJOBS_PAR} --progress \
    "${EXPORT_BIN} --input {3} --output {4} --instrument-id {2} \
     --bar-type time --bar-param 5 --target 19 --stop 7 \
     --emit-tick-series --date {1} 2>&1 | tail -3"

TICK_END=$(date +%s)
echo "=== Tick extraction completed in $((TICK_END - TICK_START))s ==="

# Move tick files from features dir to tick-series dir (bar-feature-export
# writes them alongside the feature parquet)
mv ${WORK}/features/*-ticks.parquet ${WORK}/tick-series/ 2>/dev/null || true
TICK_COUNT=$(ls ${WORK}/tick-series/*-ticks.parquet 2>/dev/null | wc -l)
echo "  ${TICK_COUNT} tick series files ready"

# Upload tick series to S3 for reuse
aws s3 sync ${WORK}/tick-series/ "s3://${S3_BUCKET}/${S3_PREFIX}/tick-series/" \
    --region "${AWS_REGION}" --quiet
echo "  Uploaded tick series to S3"

# ── Run CPCV backtest (serial + overlapping, all days) ──
NCPU=$(nproc)
NPAR=$((NCPU / 4))  # Each fold uses ~4 threads (XGBoost + rayon)
if [[ ${NPAR} -lt 4 ]]; then NPAR=4; fi
TICK_FLAG="--tick-series-dir ${WORK}/tick-series"
echo "=== Running CPCV backtest (${NPAR} parallel folds on ${NCPU} cores, tick-level serial) ==="
RUN_START=$(date +%s)
cd ${WORK}/src
./target/release/cpcv-backtest \
    --features-dir ${WORK}/features \
    --all-days \
    --target 19 \
    --stop 7 \
    --tick-size 0.25 \
    --parallel-folds ${NPAR} \
    ${TICK_FLAG} \
    --output ${WORK}/results/cpcv-backtest-results.json \
    2>&1

RUN_END=$(date +%s)
echo "=== CPCV backtest completed in $((RUN_END - RUN_START))s ==="

# ── Run temporal holdout (train 170, test 81) ──
echo "=== Running temporal holdout (train 170, test 81) ==="
HOLDOUT_START=$(date +%s)
./target/release/cpcv-backtest \
    --features-dir ${WORK}/features \
    --all-days \
    --target 19 \
    --stop 7 \
    --tick-size 0.25 \
    --temporal-holdout 81 \
    ${TICK_FLAG} \
    --output ${WORK}/results/temporal-holdout-results.json \
    2>&1

HOLDOUT_END=$(date +%s)
echo "=== Temporal holdout completed in $((HOLDOUT_END - HOLDOUT_START))s ==="

# ── Upload final results ──
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
# No MAX_HOURS watchdog — instance shuts down on completion only

# ── Step 4: Create security group + launch ──────────────────────
VPC_ID=$(aws ec2 describe-vpcs --filters "Name=isDefault,Values=true" \
    --query "Vpcs[0].VpcId" --output text --region "${AWS_REGION}")

SG_ID=$(aws ec2 create-security-group \
    --group-name "cloud-run-${RUN_ID}" \
    --description "CPCV backtest ${RUN_ID}" \
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

# Block device mappings: root (100GB gp3) + DBN data from snapshot (60GB, read-only)
BDM='[
  {"DeviceName":"/dev/xvda","Ebs":{"VolumeSize":100,"VolumeType":"gp3","DeleteOnTermination":true}},
  {"DeviceName":"/dev/sdf","Ebs":{"SnapshotId":"__DBN_SNAPSHOT__","VolumeSize":60,"VolumeType":"gp3","DeleteOnTermination":true}}
]'
BDM="${BDM//__DBN_SNAPSHOT__/${DBN_SNAPSHOT}}"

# Try spot first, fall back to on-demand if quota exceeded
LAUNCH_MODE="spot"
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
    --tag-specifications "ResourceType=instance,Tags=[{Key=Name,Value=cpcv-backtest-${RUN_ID}},{Key=ManagedBy,Value=cloud-run},{Key=RunId,Value=${RUN_ID}}]" \
    --client-token "cloud-run-spot-${RUN_ID:0:59}" \
    --region "${AWS_REGION}" \
    --query "Instances[0].InstanceId" --output text 2>&1) || true

if [[ "${INSTANCE_ID}" == *"error"* ]] || [[ "${INSTANCE_ID}" == *"Error"* ]] || [[ -z "${INSTANCE_ID}" ]]; then
    echo "  Spot failed, launching on-demand..."
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
        --tag-specifications "ResourceType=instance,Tags=[{Key=Name,Value=cpcv-backtest-${RUN_ID}},{Key=ManagedBy,Value=cloud-run},{Key=RunId,Value=${RUN_ID}}]" \
        --client-token "cloud-run-od-${RUN_ID:0:61}" \
        --region "${AWS_REGION}" \
        --query "Instances[0].InstanceId" --output text)
fi

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
echo "  Instance:  ${INSTANCE_ID} (${INSTANCE_TYPE})"
echo "  Public IP: ${PUBLIC_IP:-pending}"
echo "  Run ID:    ${RUN_ID}"
echo ""
echo "  SSH:       ssh -i ~/.ssh/kenoma-research.pem ec2-user@${PUBLIC_IP:-<ip>}"
echo "  Logs:      ssh ... 'sudo tail -f /var/log/experiment.log'"
echo ""
echo "  Poll results:"
echo "    aws s3 cp s3://${S3_BUCKET}/${S3_PREFIX}/exit_code - --region ${AWS_REGION} 2>/dev/null"
echo ""
echo "  Download results:"
echo "    aws s3 sync s3://${S3_BUCKET}/${S3_PREFIX}/results/ .kit/results/ec2-${RUN_ID}/ --region ${AWS_REGION}"
echo ""
echo "  Download tick series (for local reuse):"
echo "    aws s3 sync s3://${S3_BUCKET}/${S3_PREFIX}/tick-series/ .kit/results/label-geometry-1h/geom_19_7/ --region ${AWS_REGION}"
echo ""
echo "  Cleanup SG (after termination):"
echo "    aws ec2 delete-security-group --group-id ${SG_ID} --region ${AWS_REGION}"
echo "══════════════════════════════════════════"
