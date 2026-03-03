#!/usr/bin/env bash
# Diagnostic: export tick series + run label diagnostic on 10 sample days.
# Usage: bash .kit/scripts/diagnostic-tick-export.sh
set -euo pipefail

DBN_DIR="/Users/brandonbell/LOCAL_DEV/MBO-DL-02152026/DATA/GLBX-20260207-L953CAPU5B"
OUT_DIR="/Users/brandonbell/LOCAL_DEV/mbo-dl-rust/.kit/results/label-geometry-1h/geom_19_7"
BIN="/Users/brandonbell/LOCAL_DEV/mbo-dl-rust/target/release/bar-feature-export"

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

# 10 sample days spread across 2022
DATES=(20220103 20220207 20220304 20220411 20220516 20220620 20220718 20220815 20220919 20221114)

for date_str in "${DATES[@]}"; do
    dbn_file="$DBN_DIR/glbx-mdp3-${date_str}.mbo.dbn.zst"
    out_date="${date_str:0:4}-${date_str:4:2}-${date_str:6:2}"
    out_file="$OUT_DIR/${out_date}.parquet"
    instrument_id=$(get_instrument_id "$date_str")

    echo "=== Exporting $date_str (instrument=$instrument_id) ==="
    "$BIN" \
        --input "$dbn_file" \
        --output "$out_file" \
        --instrument-id "$instrument_id" \
        --bar-type time \
        --bar-param 5 \
        --target 19 \
        --stop 7 \
        --emit-tick-series \
        --label-diagnostic \
        --date "$date_str" 2>&1
    echo ""
done

echo "=== Tick series files ==="
ls -lh "$OUT_DIR"/*-ticks.parquet 2>/dev/null || echo "No tick files found"
