#!/usr/bin/env bash
set -euo pipefail

# Batch event-level export for all trading days.
# Run on EC2 with EBS snapshot snap-0efa355754c9a329d mounted at /DATA.
#
# Usage:
#   ./batch-export.sh /DATA/GLBX-20260207-L953CAPU5B /output/events 11355
#
# Outputs one Parquet per day: /output/events/YYYYMMDD-events.parquet

DBN_DIR="${1:?Usage: batch-export.sh <dbn-dir> <output-dir> <instrument-id>}"
OUTPUT_DIR="${2:?Usage: batch-export.sh <dbn-dir> <output-dir> <instrument-id>}"
INSTRUMENT_ID="${3:?Usage: batch-export.sh <dbn-dir> <output-dir> <instrument-id>}"

BINARY="./target/release/event-export"
MAX_PARALLEL="${MAX_PARALLEL:-8}"  # Tune based on available CPUs

mkdir -p "$OUTPUT_DIR"

echo "Event-level batch export"
echo "  DBN dir:       $DBN_DIR"
echo "  Output dir:    $OUTPUT_DIR"
echo "  Instrument:    $INSTRUMENT_ID"
echo "  Max parallel:  $MAX_PARALLEL"

# Build release binary
echo "[1/2] Building release binary..."
cargo build --release -p event-export

# Discover all .dbn.zst files and extract dates
echo "[2/2] Exporting..."
TOTAL=0
DONE=0
FAILED=0

export DBN_DIR OUTPUT_DIR INSTRUMENT_ID BINARY

find "$DBN_DIR" -name "*.mbo.dbn.zst" -type f | sort | while read -r DBN_FILE; do
    # Extract date from filename: glbx-mdp3-YYYYMMDD.mbo.dbn.zst
    DATE=$(basename "$DBN_FILE" | grep -oP '\d{8}')
    OUTPUT_FILE="$OUTPUT_DIR/${DATE}-events.parquet"

    if [ -f "$OUTPUT_FILE" ]; then
        echo "  SKIP $DATE (already exists)"
        continue
    fi

    TOTAL=$((TOTAL + 1))
    echo "  EXPORT $DATE..."

    $BINARY \
        --input "$DBN_FILE" \
        --output "$OUTPUT_FILE" \
        --instrument-id "$INSTRUMENT_ID" \
        --date "$DATE" \
        --lookback-events 200 \
        --max-horizon-s 3600 \
        --tick-size 0.25 \
        2>"$OUTPUT_DIR/${DATE}-export.log" && {
        DONE=$((DONE + 1))
        echo "    OK: $OUTPUT_FILE"
    } || {
        FAILED=$((FAILED + 1))
        echo "    FAIL: $DATE (see $OUTPUT_DIR/${DATE}-export.log)"
    }
done

echo ""
echo "Batch export complete: $DONE succeeded, $FAILED failed out of $TOTAL total"
