#!/bin/bash
set -eo pipefail

echo "=== Phase 3C: Signal Check (Condition C — random init) === $(date -u)"

export PYTORCH_CUDA_ALLOC_CONF=expandable_segments:True

# Condition C: no --book-state, no --checkpoint
# Precomputed sidecars must exist (uploaded by condition B pod or separate precompute job)
for f in /data/commit_positions.bin /data/direction_targets.bin; do
    if [ ! -f "$f" ]; then
        echo "ERROR: $f not found. Run condition B first (it uploads precomputed files to S3)."
        exit 1
    fi
done

python -u /experiment/phase3_signal_check.py \
    --condition C \
    --tokens /data/tokens.bin \
    --commit-positions /data/commit_positions.bin \
    --direction-targets /data/direction_targets.bin \
    --results-dir /results \
    --epochs 5 \
    --batch-size 256 \
    --context-len 512 \
    --stride 32768 \
    --lr-c 3e-4 \
    --num-workers 0

echo "=== Done: $(date -u) ==="
