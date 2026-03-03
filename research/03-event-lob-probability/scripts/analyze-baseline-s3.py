#!/usr/bin/env python3
"""Streaming baseline analysis against S3 parquets — no full download needed.

Reads each parquet from S3, accumulates per-geometry counters, writes report.
Memory usage: trivial (just counters).
"""

import subprocess
import tempfile
import os
import sys

import pyarrow.parquet as pq
import numpy as np

S3_PREFIX = "s3://kenoma-labs-research/cloud-runs/event-export-full-20260302T140214Z-a1fbd38a/events-bbo/"
REGION = "us-east-1"
MIN_SIZE = 100_000  # skip files < 100KB (holidays/empty)


def list_s3_parquets():
    """List parquet files on S3 with sizes."""
    result = subprocess.run(
        ["aws", "s3", "ls", S3_PREFIX, "--region", REGION],
        capture_output=True, text=True
    )
    files = []
    for line in result.stdout.strip().split("\n"):
        if not line.strip():
            continue
        parts = line.split()
        size = int(parts[2])
        name = parts[3]
        if name.endswith(".parquet") and size > MIN_SIZE:
            files.append((name, size))
    return sorted(files)


def download_and_analyze(name, tmpdir, geo_stats, day_stats):
    """Download one parquet, accumulate counters, delete."""
    local_path = os.path.join(tmpdir, name)
    s3_path = S3_PREFIX + name
    subprocess.run(
        ["aws", "s3", "cp", s3_path, local_path, "--region", REGION, "--quiet"],
        check=True, capture_output=True
    )

    table = pq.read_table(local_path, columns=["target_ticks", "stop_ticks", "outcome"])
    os.remove(local_path)

    targets = table.column("target_ticks").to_numpy()
    stops = table.column("stop_ticks").to_numpy()
    outcomes = table.column("outcome").to_numpy()

    n_rows = len(outcomes)
    n_target = int(np.sum(outcomes == 1))
    n_stop = int(np.sum(outcomes == 0))
    n_horizon = int(np.sum(outcomes == -1))

    date_str = name.replace("-events.parquet", "")
    day_stats.append((date_str, n_rows, n_target, n_stop, n_horizon))

    for t_val in np.unique(targets):
        for s_val in np.unique(stops):
            mask = (targets == t_val) & (stops == s_val)
            geo_outcomes = outcomes[mask]
            key = (int(t_val), int(s_val))
            if key not in geo_stats:
                geo_stats[key] = [0, 0, 0]
            geo_stats[key][0] += int(np.sum(geo_outcomes == 1))
            geo_stats[key][1] += int(np.sum(geo_outcomes == 0))
            geo_stats[key][2] += int(np.sum(geo_outcomes == -1))

    return n_rows


def main():
    print("Streaming baseline analysis from S3")
    print(f"  Source: {S3_PREFIX}")
    print()

    files = list_s3_parquets()
    print(f"  {len(files)} valid parquets (>{MIN_SIZE//1000}KB)")
    print()

    geo_stats = {}  # (T, S) -> [target, stop, horizon]
    day_stats = []  # [(date, rows, target, stop, horizon)]
    total_rows = 0

    with tempfile.TemporaryDirectory() as tmpdir:
        for i, (name, size) in enumerate(files):
            n = download_and_analyze(name, tmpdir, geo_stats, day_stats)
            total_rows += n
            if (i + 1) % 25 == 0 or i == len(files) - 1:
                print(f"  [{i+1}/{len(files)}] {total_rows:,} rows so far")

    print(f"\n{'='*65}")
    print(f"  BASELINE ANALYSIS — {len(day_stats)} trading days, {total_rows:,} total rows")
    print(f"{'='*65}")

    # Per-geometry null hypothesis check
    print(f"\n  Per-Geometry Null Hypothesis Check")
    print(f"  {'Geometry':>10}  {'P(target)':>10}  {'P_null':>10}  {'Delta':>10}  {'N_resolved':>12}")
    print(f"  {'-'*10}  {'-'*10}  {'-'*10}  {'-'*10}  {'-'*12}")

    for (t, s) in sorted(geo_stats.keys()):
        tgt, stp, hor = geo_stats[(t, s)]
        resolved = tgt + stp
        if resolved == 0:
            continue
        p_target = tgt / resolved
        p_null = s / (t + s)
        delta = p_target - p_null
        print(f"  {t:>4}:{s:<4}  {p_target:>10.4f}  {p_null:>10.4f}  {delta:>+10.4f}  {resolved:>12,}")

    # Overall stats
    total_target = sum(v[0] for v in geo_stats.values())
    total_stop = sum(v[1] for v in geo_stats.values())
    total_horizon = sum(v[2] for v in geo_stats.values())
    print(f"\n  Overall: {total_target:,} target, {total_stop:,} stop, {total_horizon:,} horizon")
    print(f"  Horizon rate: {100*total_horizon/total_rows:.1f}%")

    # Day summary stats
    rows_per_day = [d[1] for d in day_stats]
    print(f"\n  Rows/day: min={min(rows_per_day):,}  median={int(np.median(rows_per_day)):,}  max={max(rows_per_day):,}")
    print(f"  Eval points/day (÷11 geometries): min={min(rows_per_day)//11:,}  median={int(np.median(rows_per_day))//11:,}  max={max(rows_per_day)//11:,}")


if __name__ == "__main__":
    main()
