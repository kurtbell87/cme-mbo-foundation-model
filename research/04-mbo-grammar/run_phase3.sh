#!/bin/bash
set -eo pipefail

echo "=== Phase 3: Signal Check (B vs C, 15-fold CPCV) === $(date -u)"

export PYTORCH_CUDA_ALLOC_CONF=expandable_segments:True

# Precompute sidecars from book_state (tradeable prices, no mid-price)
if [ ! -f /data/commit_positions.bin ] || [ ! -f /data/direction_targets.bin ]; then
    echo "--- Precomputing sidecars ---"
    python -u /experiment/precompute_phase3.py \
        --tokens /data/tokens.bin \
        --book-state /data/tokens.bin.book_state \
        --out-dir /data
fi

# Condition B (pretrained + recon + direction)
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

# Condition C (random init + direction only, no book_state)
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
