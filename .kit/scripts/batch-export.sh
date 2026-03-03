#!/usr/bin/env bash
# Batch re-export all 2022 DBN files to Parquet with close_mid + fwd_return_720.
# Usage: bash .kit/scripts/batch-export.sh
set -euo pipefail

DBN_DIR="/Users/brandonbell/LOCAL_DEV/MBO-DL-02152026/DATA/GLBX-20260207-L953CAPU5B"
OUT_DIR="/Users/brandonbell/LOCAL_DEV/mbo-dl-rust/.kit/results/label-geometry-1h/geom_19_7"
BIN="/Users/brandonbell/LOCAL_DEV/mbo-dl-rust/target/release/bar-feature-export"

mkdir -p "$OUT_DIR"

# Instrument ID lookup: MES 2022 contract table
get_instrument_id() {
    local d=$1
    if   (( d >= 20220103 && d <= 20220318 )); then echo 11355  # MESH2
    elif (( d >= 20220319 && d <= 20220617 )); then echo 13615  # MESM2
    elif (( d >= 20220618 && d <= 20220916 )); then echo 10039  # MESU2
    elif (( d >= 20220917 && d <= 20221216 )); then echo 10299  # MESZ2
    elif (( d >= 20221217 && d <= 20221230 )); then echo 2080   # MESH3
    else echo 13615  # default
    fi
}

# Rollover exclusion dates (rollover date + 3 preceding calendar days)
is_excluded() {
    local d=$1
    case $d in
        20220315|20220316|20220317|20220318) return 0 ;;  # MESH2 rollover
        20220614|20220615|20220616|20220617) return 0 ;;  # MESM2 rollover
        20220913|20220914|20220915|20220916) return 0 ;;  # MESU2 rollover
        20221213|20221214|20221215|20221216) return 0 ;;  # MESZ2 rollover
        *) return 1 ;;
    esac
}

total=0
exported=0
skipped=0
failed=0

for dbn_file in "$DBN_DIR"/glbx-mdp3-*.mbo.dbn.zst; do
    fname=$(basename "$dbn_file")
    # Extract YYYYMMDD from glbx-mdp3-YYYYMMDD.mbo.dbn.zst
    date_str=${fname#glbx-mdp3-}
    date_str=${date_str%.mbo.dbn.zst}
    total=$((total + 1))

    # Skip weekends (crude: check if date is a known non-trading day)
    # Skip rollover dates
    if is_excluded "$date_str"; then
        skipped=$((skipped + 1))
        continue
    fi

    # Format output as YYYY-MM-DD.parquet
    out_date="${date_str:0:4}-${date_str:4:2}-${date_str:6:2}"
    out_file="$OUT_DIR/${out_date}.parquet"

    # Skip if already exported (both feature parquet AND tick series)
    tick_file="$OUT_DIR/${out_date}-ticks.parquet"
    if [[ -f "$out_file" ]] && [[ -f "$tick_file" ]] && [[ $(stat -f%z "$tick_file") -gt 1000 ]]; then
        exported=$((exported + 1))
        continue
    fi

    instrument_id=$(get_instrument_id "$date_str")

    echo "[$(date +%H:%M:%S)] Exporting $date_str (instrument=$instrument_id)..."
    if "$BIN" \
        --input "$dbn_file" \
        --output "$out_file" \
        --instrument-id "$instrument_id" \
        --bar-type time \
        --bar-param 5 \
        --target 19 \
        --stop 7 \
        --emit-tick-series \
        --date "$date_str" 2>&1; then
        exported=$((exported + 1))
    else
        echo "  FAILED: $date_str"
        failed=$((failed + 1))
    fi
done

echo ""
echo "========================================="
echo "  Batch export complete"
echo "  Total DBN files: $total"
echo "  Exported:        $exported"
echo "  Skipped:         $skipped"
echo "  Failed:          $failed"
echo "========================================="
