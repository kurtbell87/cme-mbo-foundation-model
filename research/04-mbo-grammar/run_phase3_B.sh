#!/bin/bash
set -eo pipefail

echo "=== Phase 3B: Signal Check (Condition B — pretrained) === $(date -u)"

export PYTORCH_CUDA_ALLOC_CONF=expandable_segments:True

# Precompute sidecars if not already cached (~2.8 GB peak RAM, ~2 min)
if [ ! -f /data/commit_positions.bin ] || [ ! -f /data/direction_targets.bin ]; then
    echo "--- Precomputing sidecars from book_state (tradeable prices) ---"
    python -u /experiment/precompute_phase3.py \
        --tokens /data/tokens.bin \
        --book-state /data/tokens.bin.book_state \
        --out-dir /data

    # Upload precomputed files so condition C pods can skip book_state download
    echo "--- Uploading precomputed sidecars to S3 ---"
    aws s3 cp /data/commit_positions.bin s3://kenoma-labs-research/cloud-runs/mbo-grammar/commit_positions.bin
    aws s3 cp /data/direction_targets.bin s3://kenoma-labs-research/cloud-runs/mbo-grammar/direction_targets.bin
fi

python -u /experiment/phase3_signal_check.py \
    --condition B \
    --tokens /data/tokens.bin \
    --commit-positions /data/commit_positions.bin \
    --direction-targets /data/direction_targets.bin \
    --book-state /data/tokens.bin.book_state \
    --checkpoint /data/best_model.pt \
    --results-dir /results \
    --epochs 5 \
    --batch-size 256 \
    --context-len 512 \
    --stride 32768 \
    --lr-b 5e-5 \
    --num-workers 0

echo "=== Done: $(date -u) ==="
