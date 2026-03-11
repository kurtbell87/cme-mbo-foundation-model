"""
Precompute Phase 3 sidecars: commit_positions and direction_targets.

Runs once before training. Produces:
  - commit_positions.bin: uint64 array of global token positions for each COMMIT
  - direction_targets.bin: int8 array (1=up, 0=down, -1=excluded)

Direction is defined on tradeable prices: bbo_level = bid_rel + ask_rel.
A "BBO level change" occurs when this sum changes between consecutive commits.
Direction = 1 if the new level is higher, 0 if lower. -1 if no change within
max_lookahead commits.

No mid-price used anywhere.

Peak RAM: ~2.8 GB (bbo_level float32 array from book_state).
Output: ~6.3 GB on disk.

Usage:
    python precompute_phase3.py \
        --tokens /data/tokens.bin \
        --book-state /data/tokens.bin.book_state \
        --out-dir /data
"""

import argparse
import os
import time

import numpy as np

COMMIT_TOKEN = 3


def scan_commit_positions(tokens_path, out_path, chunk_size=100_000_000):
    """Scan tokens.bin for COMMIT positions, save to disk as uint64."""
    if os.path.exists(out_path):
        n = os.path.getsize(out_path) // 8
        print(f"commit_positions already cached: {n:,} entries in {out_path}")
        return

    tokens = np.memmap(tokens_path, dtype=np.uint16, mode="r")
    n = len(tokens)
    print(f"Scanning {n:,} tokens for COMMIT positions...")
    t0 = time.time()

    parts = []
    for offset in range(0, n, chunk_size):
        end = min(offset + chunk_size, n)
        chunk = np.array(tokens[offset:end])
        hits = np.where(chunk == COMMIT_TOKEN)[0].astype(np.uint64) + offset
        parts.append(hits)

    positions = np.concatenate(parts)
    assert np.all(positions[1:] > positions[:-1]), "COMMIT positions not strictly increasing"
    positions.tofile(out_path)
    dt = time.time() - t0
    print(f"Saved {len(positions):,} COMMIT positions to {out_path} "
          f"({os.path.getsize(out_path) / 1e9:.2f} GB, {dt:.0f}s)")


def compute_direction_targets(book_state_path, out_path, max_lookahead=2000):
    """Compute next-BBO-level-change direction targets from tradeable prices.

    BBO level = bid_rel + ask_rel (sum of tradeable BBO prices).
    Direction = 1 if next change is up, 0 if down, -1 if no change within lookahead.

    Reads book_state in chunks to extract BBO level, then vectorized target computation.
    Peak RAM: ~2.8 GB (bbo_level float32 array for 695M commits).
    """
    if os.path.exists(out_path):
        n = os.path.getsize(out_path)
        print(f"direction_targets already cached: {n:,} entries in {out_path}")
        return

    # Memory-map book_state: (N, 12) float32, fields [bid_rel, ask_rel, ...]
    flat = np.memmap(book_state_path, dtype=np.float32, mode="r")
    n_rows = len(flat) // 12
    assert len(flat) == n_rows * 12, f"Book state size {len(flat)} not divisible by 12"
    book_state = flat.reshape(n_rows, 12)
    n = n_rows

    print(f"Computing direction targets from book_state ({n:,} commits)...")
    t0 = time.time()

    # BBO level = bid_rel + ask_rel (tradeable prices, no mid-price)
    # Process in chunks to avoid materializing full (N, 12) in RAM
    chunk_size = 50_000_000
    bbo_level = np.empty(n, dtype=np.float32)
    for offset in range(0, n, chunk_size):
        end = min(offset + chunk_size, n)
        chunk = np.array(book_state[offset:end])  # (chunk, 12) into RAM
        bbo_level[offset:end] = chunk[:, 0] + chunk[:, 1]
    del book_state, flat

    print(f"  BBO level computed ({bbo_level.nbytes / 1e9:.2f} GB RAM)")

    # Find change points (where bbo_level differs from previous commit)
    changes = np.where(bbo_level[1:] != bbo_level[:-1])[0] + 1
    print(f"  {len(changes):,} BBO level change points ({len(changes)/n*100:.1f}%)")

    targets = np.full(n, -1, dtype=np.int8)
    if len(changes) > 0:
        proc_chunk = 10_000_000
        for start in range(0, n, proc_chunk):
            end = min(start + proc_chunk, n)
            chunk_i = np.arange(start, end, dtype=np.int64)

            pos = np.searchsorted(changes, chunk_i, side='right')
            valid = pos < len(changes)
            next_j = np.where(valid, changes[np.minimum(pos, len(changes) - 1)], n)

            distance = next_j - chunk_i
            within = distance <= max_lookahead
            mask = valid & within & (next_j < n)

            targets[start:end][mask] = (
                bbo_level[next_j[mask]] > bbo_level[chunk_i[mask]]
            ).astype(np.int8)

    del bbo_level, changes

    n_up = (targets == 1).sum()
    n_down = (targets == 0).sum()
    n_excl = (targets == -1).sum()
    print(f"  Up: {n_up:,} ({n_up/n*100:.1f}%), Down: {n_down:,} ({n_down/n*100:.1f}%), "
          f"Excluded: {n_excl:,} ({n_excl/n*100:.1f}%)")

    targets.tofile(out_path)
    elapsed = time.time() - t0
    print(f"Saved direction targets to {out_path} "
          f"({os.path.getsize(out_path) / 1e9:.2f} GB, {elapsed:.0f}s)")


def main():
    parser = argparse.ArgumentParser(description="Precompute Phase 3 sidecars")
    parser.add_argument("--tokens", required=True, help="tokens.bin path")
    parser.add_argument("--book-state", required=True, help=".book_state sidecar path")
    parser.add_argument("--out-dir", required=True, help="output directory for precomputed files")
    args = parser.parse_args()

    os.makedirs(args.out_dir, exist_ok=True)

    scan_commit_positions(
        args.tokens,
        os.path.join(args.out_dir, "commit_positions.bin"),
    )
    compute_direction_targets(
        args.book_state,
        os.path.join(args.out_dir, "direction_targets.bin"),
    )

    print("\nPrecompute complete.")


if __name__ == "__main__":
    main()
