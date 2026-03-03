#!/usr/bin/env python3
"""Analyze validation export parquets — row counts, outcome distributions, null hypothesis check."""

import sys
import os
import pyarrow.parquet as pq
import numpy as np

def analyze_dir(label, data_dir):
    print(f"\n{'='*60}")
    print(f"  {label}")
    print(f"{'='*60}")

    files = sorted(f for f in os.listdir(data_dir) if f.endswith('.parquet'))
    if not files:
        print("  No parquet files found.")
        return

    total_rows = 0
    all_outcomes = []
    all_targets = []
    all_stops = []

    for fname in files:
        path = os.path.join(data_dir, fname)
        table = pq.read_table(path)
        n = table.num_rows
        total_rows += n
        date_str = fname.replace('-events.parquet', '')

        outcome = table.column('outcome').to_numpy()
        target_ticks = table.column('target_ticks').to_numpy()
        stop_ticks = table.column('stop_ticks').to_numpy()

        all_outcomes.append(outcome)
        all_targets.append(target_ticks)
        all_stops.append(stop_ticks)

        n_target = np.sum(outcome == 1)
        n_stop = np.sum(outcome == 0)
        n_horizon = np.sum(outcome == -1)
        n_eval = n // 10  # 10 geometries per eval point

        print(f"\n  {date_str}: {n:,} rows ({n_eval:,} eval points)")
        print(f"    Target: {n_target:,} ({100*n_target/n:.1f}%)  Stop: {n_stop:,} ({100*n_stop/n:.1f}%)  Horizon: {n_horizon:,} ({100*n_horizon/n:.1f}%)")

    print(f"\n  Total: {total_rows:,} rows across {len(files)} days")

    # Per-geometry null hypothesis check
    outcomes = np.concatenate(all_outcomes)
    targets = np.concatenate(all_targets)
    stops = np.concatenate(all_stops)

    print(f"\n  Per-Geometry Null Hypothesis Check")
    print(f"  {'Geometry':>10}  {'P(target)':>10}  {'P_null':>10}  {'Delta':>10}  {'N':>8}")
    print(f"  {'-'*10}  {'-'*10}  {'-'*10}  {'-'*10}  {'-'*8}")

    geometries = sorted(set(zip(targets.tolist(), stops.tolist())))
    for t, s in geometries:
        mask = (targets == t) & (stops == s)
        geo_outcomes = outcomes[mask]
        # Exclude horizon for P(target) calculation
        resolved = geo_outcomes[geo_outcomes != -1]
        if len(resolved) == 0:
            continue
        p_target = np.mean(resolved == 1)
        p_null = s / (t + s)
        delta = p_target - p_null
        print(f"  {t:>4}:{s:<4}  {p_target:>10.4f}  {p_null:>10.4f}  {delta:>+10.4f}  {len(resolved):>8,}")

    # Feature sanity: check for NaN/Inf
    print(f"\n  Feature Sanity Check")
    sample_path = os.path.join(data_dir, files[0])
    table = pq.read_table(sample_path)
    schema = table.schema
    feature_cols = [f.name for f in schema if f.name.startswith(('bid_size_', 'ask_size_', 'imbalance_', 'weighted_imbalance',
                    'spread_ticks', 'hhi_', 'slope_', 'active_levels_', 'cancel_rate_', 'add_rate_',
                    'trade_aggression', 'message_rate', 'cancel_add_ratio', 'bbo_changes',
                    'price_momentum', 'order_flow_toxicity'))]
    n_nan = 0
    n_inf = 0
    for col_name in feature_cols:
        arr = table.column(col_name).to_numpy().astype(np.float64)
        n_nan += np.sum(np.isnan(arr))
        n_inf += np.sum(np.isinf(arr))
    print(f"  {len(feature_cols)} feature columns checked in {files[0]}")
    print(f"  NaN count: {n_nan:,}  Inf count: {n_inf:,}")

    # Schema dump
    print(f"\n  Schema ({len(schema)} columns):")
    for i, field in enumerate(schema):
        if i < 8 or i >= len(schema) - 4:
            print(f"    {field.name}: {field.type}")
        elif i == 8:
            print(f"    ... ({len(schema) - 12} more feature columns) ...")


if __name__ == '__main__':
    base = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    bbo_dir = os.path.join(base, 'events-bbo')
    all_dir = os.path.join(base, 'events-all')

    print("Event-Level Export Validation Analysis")
    print(f"BBO dir: {bbo_dir}")
    print(f"ALL dir: {all_dir}")

    analyze_dir("BBO-Change Events (filtered to BBO price changes)", bbo_dir)

    if os.path.exists(all_dir) and os.listdir(all_dir):
        analyze_dir("All-Commits Events (every committed state)", all_dir)
    else:
        print("\n  No all-commits data available.")
