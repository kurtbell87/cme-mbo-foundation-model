#!/usr/bin/env python3
"""Augment C++ reference Parquet files with fwd_return_720 and close_mid columns.

Reads from the C++ full-year-export directory, computes the missing columns,
and writes augmented files to the cpcv-backtest features directory.

fwd_return_720[i] = sum(fwd_return_1[i:i+720])  (in ticks, NaN if insufficient lookahead)
close_mid[i] = base + cumsum(fwd_return_1[0:i]) * tick_size  (reconstructed, base=0)
"""

import sys
import os
from pathlib import Path

import pyarrow as pa
import pyarrow.parquet as pq
import numpy as np

SRC_DIR = Path("/Users/brandonbell/LOCAL_DEV/MBO-DL-02152026/.kit/results/full-year-export")
DST_DIR = Path("/Users/brandonbell/LOCAL_DEV/mbo-dl-rust/.kit/results/label-geometry-1h/geom_19_7")
TICK_SIZE = 0.25
FWD_HORIZON = 720

def augment_file(src_path: Path, dst_path: Path) -> int:
    """Augment a single Parquet file. Returns row count."""
    table = pq.read_table(src_path)
    n = table.num_rows

    # Extract fwd_return_1 as numpy array
    fwd1 = table.column("fwd_return_1").to_numpy()

    # Compute fwd_return_720 via rolling sum
    fwd720 = np.full(n, np.nan, dtype=np.float64)
    if n > FWD_HORIZON:
        # Efficient rolling sum using cumsum
        cs = np.cumsum(fwd1)
        cs = np.insert(cs, 0, 0.0)  # prepend 0 for offset indexing
        end = n - FWD_HORIZON
        fwd720[:end] = cs[FWD_HORIZON : FWD_HORIZON + end] - cs[:end]

    # Reconstruct close_mid from cumulative fwd_return_1
    # close_mid[i] = base_price + cumsum(fwd_return_1[0:i]) * tick_size
    # We don't know the absolute base, so use 0.0 (relative reconstruction).
    # This is fine — cpcv-backtest only uses fwd_return_720, not close_mid.
    cumulative = np.cumsum(fwd1) * TICK_SIZE
    close_mid = np.insert(cumulative, 0, 0.0)[:n]  # shift: close_mid[0]=0, close_mid[1]=fwd1[0]*tick

    # Add columns to table
    table = table.append_column("close_mid", pa.array(close_mid, type=pa.float64()))
    table = table.append_column("fwd_return_720", pa.array(fwd720, type=pa.float64()))

    # Write with ZSTD compression
    pq.write_table(table, dst_path, compression="zstd")
    return n


def main():
    DST_DIR.mkdir(parents=True, exist_ok=True)

    src_files = sorted(SRC_DIR.glob("*.parquet"))
    print(f"Found {len(src_files)} source Parquet files")

    total_rows = 0
    for i, src in enumerate(src_files):
        dst = DST_DIR / src.name
        try:
            rows = augment_file(src, dst)
            total_rows += rows
            if (i + 1) % 25 == 0 or i + 1 == len(src_files):
                print(f"  [{i+1}/{len(src_files)}] {src.name} — {rows} rows")
        except Exception as e:
            print(f"  FAILED: {src.name} — {e}", file=sys.stderr)

    print(f"\nDone. Augmented {len(src_files)} files, {total_rows} total rows")
    print(f"Output: {DST_DIR}")


if __name__ == "__main__":
    main()
