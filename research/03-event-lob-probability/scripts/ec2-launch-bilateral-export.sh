#!/usr/bin/env bash
# Launch OFI-directed bilateral Parquet export on EC2.
#
# Re-exports all 287 trading days with --ofi-direction flag, producing
# Parquets with a `direction` column (+1/-1) based on ofi_fast sign.
# Entry price: ask for longs, bid for shorts.
#
# Mounts DBN data from EBS snapshot, builds event-export, exports in parallel.
# Instance shuts down on completion only (no auto-termination timer).
#
# Usage: bash research/03-event-lob-probability/scripts/ec2-launch-bilateral-export.sh
set -euo pipefail

# ── Config ──────────────────────────────────────────────────────
S3_BUCKET="kenoma-labs-research"
AWS_REGION="us-east-1"
SSH_KEY="kenoma-research"
AMI_ID="ami-0f3caa1cf4417e51b"          # Amazon Linux 2023 x86_64
IAM_PROFILE="cloud-run-ec2"
DBN_SNAPSHOT="snap-0efa355754c9a329d"   # mbo-data-2022 (60GB, 316 DBN files)
INSTANCE_TYPE="c7a.4xlarge"             # 16 vCPU, 32 GB — fits vCPU limit

RUN_ID="bilateral-export-$(date +%Y%m%dT%H%M%SZ)-$(openssl rand -hex 4)"
S3_PREFIX="cloud-runs/${RUN_ID}"
PROJECT_ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"

echo "══════════════════════════════════════════"
echo "  Bilateral Export (OFI-Directed) — EC2"
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

# Injected variables (replaced below)
RUN_ID="__RUN_ID__"
S3_BUCKET="__S3_BUCKET__"
S3_PREFIX="__S3_PREFIX__"
AWS_REGION="__AWS_REGION__"

WORK=/work
DBN_MNT=/mnt/dbn
mkdir -p ${WORK}/results ${WORK}/events-bilateral ${DBN_MNT}

# Heartbeat (every 60s) — monitor liveness
(while true; do
    date -u +%Y-%m-%dT%H:%M:%SZ | aws s3 cp - "s3://${S3_BUCKET}/${S3_PREFIX}/heartbeat" --region "${AWS_REGION}" 2>/dev/null
    sleep 60
done) &

# Results sync (every 2 minutes)
(while true; do
    sleep 120
    aws s3 sync ${WORK}/results/ "s3://${S3_BUCKET}/${S3_PREFIX}/results/" --region "${AWS_REGION}" --quiet 2>/dev/null || true
    aws s3 sync ${WORK}/events-bilateral/ "s3://${S3_BUCKET}/${S3_PREFIX}/events-bilateral/" --region "${AWS_REGION}" --quiet 2>/dev/null || true
done) &

# ── Mount DBN data EBS volume ──
echo "=== Mounting DBN data volume ==="
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

# ── Download source from S3 ──
echo "=== Downloading source ==="
cd ${WORK}
aws s3 cp "s3://${S3_BUCKET}/${S3_PREFIX}/source.tar.gz" source.tar.gz --region "${AWS_REGION}"
mkdir -p src
tar -xzf source.tar.gz -C src/

# ── Pre-build xgboost-sys ──
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

# ── Build event-export (release) ──
echo "=== Building event-export ==="
cd ${WORK}/src
BUILD_START=$(date +%s)
cargo build --release --package event-export 2>&1 | tail -20
BUILD_END=$(date +%s)
echo "Build completed in $((BUILD_END - BUILD_START))s"

EXPORT_BIN=${WORK}/src/target/release/event-export

# ── Instrument ID lookup ──
get_instrument_id() {
    local d=$1
    if   (( d >= 20220103 && d <= 20220318 )); then echo 11355   # MESH2
    elif (( d >= 20220319 && d <= 20220617 )); then echo 13615   # MESM2
    elif (( d >= 20220618 && d <= 20220916 )); then echo 10039   # MESU2
    elif (( d >= 20220917 && d <= 20221216 )); then echo 10299   # MESZ2
    elif (( d >= 20221217 && d <= 20221230 )); then echo 2080    # MESH3
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

# ── Build job list ──
echo "=== Building job list ==="

# Helper: run a single export job (called by GNU parallel)
run_export() {
    local date_str="$1" instrument_id="$2" dbn_file="$3" out_file="$4" log_file="$5"
    ${EXPORT_BIN} \
        --input "${dbn_file}" \
        --output "${out_file}" \
        --instrument-id "${instrument_id}" \
        --date "${date_str}" \
        --lookback-events 200 \
        --max-horizon-s 3600 \
        --tick-size 0.25 \
        --ofi-direction \
        2>"${log_file}" && {
        echo "  OK ${date_str} ($(du -h "${out_file}" | cut -f1))"
    } || {
        echo "  FAIL ${date_str} (see $(basename ${log_file}))"
    }
}
export -f run_export

JOB_FILE=${WORK}/export-jobs.txt
> ${JOB_FILE}

for dbn_file in ${DBN_DIR}/glbx-mdp3-*.mbo.dbn.zst; do
    fname=$(basename "$dbn_file")
    date_str=${fname#glbx-mdp3-}
    date_str=${date_str%.mbo.dbn.zst}
    if is_excluded "$date_str"; then continue; fi
    instrument_id=$(get_instrument_id "$date_str")
    out_file="${WORK}/events-bilateral/${date_str}-events.parquet"
    log_file="${WORK}/results/${date_str}-bilateral-export.log"
    if [[ -f "$out_file" ]] && [[ $(stat -c%s "$out_file" 2>/dev/null || echo 0) -gt 100000 ]]; then continue; fi
    echo "${date_str} ${instrument_id} ${dbn_file} ${out_file} ${log_file}" >> ${JOB_FILE}
done
echo "  $(wc -l < ${JOB_FILE}) days to export (bilateral, parallel)"

# ── Run exports in parallel ──
NCPU=$(nproc)
MEM_GB=$(awk '/MemTotal/{printf "%d", $2/1048576}' /proc/meminfo)
MAX_BY_MEM=$((MEM_GB / 4))
NJOBS_PAR=$((NCPU < MAX_BY_MEM ? NCPU : MAX_BY_MEM))
if [[ ${NJOBS_PAR} -lt 1 ]]; then NJOBS_PAR=1; fi
echo "  Parallelism: ${NJOBS_PAR} (${NCPU} cores, ${MEM_GB} GB RAM)"

EXPORT_START=$(date +%s)
echo "=== Exporting (${NJOBS_PAR} parallel jobs) ==="

cat ${JOB_FILE} | parallel --colsep ' ' -j ${NJOBS_PAR} \
    "run_export {1} {2} {3} {4} {5}"

EXPORT_END=$(date +%s)
echo "=== Export completed in $((EXPORT_END - EXPORT_START))s ==="

BIL_COUNT=$(ls ${WORK}/events-bilateral/*.parquet 2>/dev/null | wc -l)
echo "  Bilateral Parquets: ${BIL_COUNT}"

# ── Upload results ──
echo "=== Uploading ==="
cp /var/log/experiment.log ${WORK}/results/experiment.log
aws s3 sync ${WORK}/results/ "s3://${S3_BUCKET}/${S3_PREFIX}/results/" --region "${AWS_REGION}"
aws s3 sync ${WORK}/events-bilateral/ "s3://${S3_BUCKET}/${S3_PREFIX}/events-bilateral/" --region "${AWS_REGION}"

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

# ── Step 4: Create security group + launch ──────────────────────
VPC_ID=$(aws ec2 describe-vpcs --filters "Name=isDefault,Values=true" \
    --query "Vpcs[0].VpcId" --output text --region "${AWS_REGION}")

SG_ID=$(aws ec2 create-security-group \
    --group-name "cloud-run-${RUN_ID}" \
    --description "Bilateral export ${RUN_ID}" \
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
    --tag-specifications "ResourceType=instance,Tags=[{Key=Name,Value=bilateral-export-${RUN_ID}},{Key=ManagedBy,Value=cloud-run},{Key=RunId,Value=${RUN_ID}}]" \
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
        --tag-specifications "ResourceType=instance,Tags=[{Key=Name,Value=bilateral-export-${RUN_ID}},{Key=ManagedBy,Value=cloud-run},{Key=RunId,Value=${RUN_ID}}]" \
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
echo "  Download bilateral Parquets:"
echo "    aws s3 sync s3://${S3_BUCKET}/${S3_PREFIX}/events-bilateral/ research/03-event-lob-probability/events-bilateral/ --region ${AWS_REGION}"
echo ""
echo "  Cleanup SG (after termination):"
echo "    aws ec2 delete-security-group --group-id ${SG_ID} --region ${AWS_REGION}"
echo "══════════════════════════════════════════"
