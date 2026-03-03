#!/usr/bin/env python3
"""Generate XGBoost test model fixtures for xgboost-ffi crate tests.

Run from the project root:
    python tests/fixtures/generate_test_fixtures.py

Prerequisites:
    pip install xgboost numpy

Outputs (in tests/fixtures/):
    test_model_2f.json          — Small 2-feature binary classifier (10 trees)
    test_stage1.json            — 20-feature directional filter model
    test_stage2.json            — 20-feature long-vs-short model
    expected_predictions.json   — Known predictions for test vectors
"""

import json
import os
import sys

import numpy as np
import xgboost as xgb

FIXTURE_DIR = os.path.dirname(os.path.abspath(__file__))
np.random.seed(42)


def generate_2f_model():
    """Train a tiny 2-feature binary classifier on synthetic data."""
    X = np.random.randn(1000, 2).astype(np.float32)
    y = (X[:, 0] + X[:, 1] > 0).astype(int)

    model = xgb.XGBClassifier(
        n_estimators=10,
        max_depth=3,
        objective="binary:logistic",
        random_state=42,
        use_label_encoder=False,
        eval_metric="logloss",
    )
    model.fit(X, y)

    path = os.path.join(FIXTURE_DIR, "test_model_2f.json")
    model.save_model(path)
    print(f"  Saved {path}")

    # Known test vectors
    test_vectors = np.array(
        [[0.5, 0.5], [-1.0, 1.0], [0.0, 0.0]], dtype=np.float32
    )
    predictions = model.predict_proba(test_vectors)[:, 1].tolist()
    return test_vectors.tolist(), predictions


def generate_20f_models():
    """Train two 20-feature models: stage1 (directional) and stage2 (long/short).

    Stage 1 target: sum of first 5 features > 0 → directional (1)
    Stage 2 target: feature[0] > 0 → long (1)

    These simple rules let us construct test vectors with known signal outcomes.
    """
    X = np.random.randn(2000, 20).astype(np.float32)

    # Stage 1: directional filter
    y_dir = (np.sum(X[:, :5], axis=1) > 0).astype(int)
    stage1 = xgb.XGBClassifier(
        n_estimators=10,
        max_depth=3,
        objective="binary:logistic",
        random_state=42,
        use_label_encoder=False,
        eval_metric="logloss",
    )
    stage1.fit(X, y_dir)
    path1 = os.path.join(FIXTURE_DIR, "test_stage1.json")
    stage1.save_model(path1)
    print(f"  Saved {path1}")

    # Stage 2: long vs short
    y_long = (X[:, 0] > 0).astype(int)
    stage2 = xgb.XGBClassifier(
        n_estimators=10,
        max_depth=3,
        objective="binary:logistic",
        random_state=42,
        use_label_encoder=False,
        eval_metric="logloss",
    )
    stage2.fit(X, y_long)
    path2 = os.path.join(FIXTURE_DIR, "test_stage2.json")
    stage2.save_model(path2)
    print(f"  Saved {path2}")

    # --- Build test vectors that produce each signal outcome ---
    # These are PRE-NORMALIZATION raw features.
    # The TwoStageModel will normalize them before passing to the models.
    #
    # norm_stats.json:
    #   mean = [1.0, -0.5, 0.0, 0.25, -1.0, 0.5, 0, 0, ..., 3.0]
    #   std  = [2.0,  0.5, 1.0, 0.10,  3.0, 1.5, 1, 1, ..., 0.0]
    #
    # After normalization: z[i] = (raw[i] - mean[i]) / std[i]

    # Load the norm stats to compute what the models actually see
    norm_path = os.path.join(FIXTURE_DIR, "norm_stats.json")
    with open(norm_path) as f:
        norms = json.load(f)
    mean = np.array(norms["mean"], dtype=np.float32)
    std_arr = np.array(norms["std"], dtype=np.float32)
    std_floored = np.where(std_arr < 1e-10, 1.0, std_arr)

    def normalize(raw):
        z = (np.array(raw, dtype=np.float32) - mean) / std_floored
        z = np.where(np.isnan(z), 0.0, z)
        return z

    # HOLD: need stage1 P(directional) < 0.50
    # Strong negative sum of first 5 features after normalization
    # raw values chosen so that z[0:5] are all very negative
    hold_raw = [
        -5.0, -2.0, -5.0, -0.75, -10.0,  # Features 0-4: large negative z-scores
        0.0, 0.0, 0.0, 0.0, 0.0,
        0.0, 0.0, 0.0, 0.0, 0.0,
        0.0, 0.0, 0.0, 0.0, 0.0,
    ]

    # LONG: need stage1 P(directional) > 0.50 AND stage2 P(long) > 0.50
    # Large positive sum of first 5 features + feature[0] strongly positive
    long_raw = [
        9.0, 1.5, 5.0, 1.25, 8.0,  # Features 0-4: large positive z-scores; f[0]>0
        0.0, 0.0, 0.0, 0.0, 0.0,
        0.0, 0.0, 0.0, 0.0, 0.0,
        0.0, 0.0, 0.0, 0.0, 0.0,
    ]

    # SHORT: need stage1 P(directional) > 0.50 AND stage2 P(long) <= 0.50
    # Large positive sum of first 5 features BUT feature[0] strongly negative
    # Tricky: feature[0] contributes to both stage1 sum and stage2.
    # Use very large values for features 1-4 to overwhelm feature[0] in stage1.
    short_raw = [
        -9.0, 4.5, 15.0, 5.25, 20.0,  # f[0] very negative, but f[1:5] overwhelm
        0.0, 0.0, 0.0, 0.0, 0.0,
        0.0, 0.0, 0.0, 0.0, 0.0,
        0.0, 0.0, 0.0, 0.0, 0.0,
    ]

    # Verify signal outcomes in Python
    signal_vectors = {"hold": hold_raw, "long": long_raw, "short": short_raw}
    signals = {}

    for name, raw in signal_vectors.items():
        z = normalize(raw)
        p_dir = float(stage1.predict_proba(z.reshape(1, -1))[0, 1])
        p_long = float(stage2.predict_proba(z.reshape(1, -1))[0, 1])

        if p_dir < 0.50:
            signal = 0
            p_long_val = None
        elif p_long > 0.50:
            signal = 1
            p_long_val = p_long
        else:
            signal = -1
            p_long_val = p_long

        signals[name] = {
            "raw_features": raw,
            "normalized": z.tolist(),
            "p_directional": p_dir,
            "p_long": p_long_val,
            "signal": signal,
        }

        expected_signal = {"hold": 0, "long": 1, "short": -1}[name]
        status = "OK" if signal == expected_signal else "MISMATCH"
        print(f"  {name}: p_dir={p_dir:.4f}, p_long={p_long:.4f}, signal={signal} [{status}]")

        if signal != expected_signal:
            print(
                f"  WARNING: {name} vector does not produce expected signal "
                f"({expected_signal}). Adjust raw feature values.",
                file=sys.stderr,
            )

    # Stage1/stage2 predictions for the 2F test vectors (for cross-reference)
    test_20f_vectors = [
        [2.0] * 5 + [0.0] * 15,
        [-2.0] * 5 + [0.0] * 15,
        [0.0] * 20,
    ]
    X_test = np.array(test_20f_vectors, dtype=np.float32)
    s1_preds = stage1.predict_proba(X_test)[:, 1].tolist()
    s2_preds = stage2.predict_proba(X_test)[:, 1].tolist()

    return {
        "stage1_predictions": s1_preds,
        "stage2_predictions": s2_preds,
        "test_vectors_20f": test_20f_vectors,
        "signals": signals,
    }


def main():
    print("Generating XGBoost test fixtures...")

    vectors_2f, preds_2f = generate_2f_model()
    data_20f = generate_20f_models()

    expected = {
        "2f_model": {
            "test_vectors": vectors_2f,
            "predictions": preds_2f,
        },
        "20f_stage1": {
            "test_vectors": data_20f["test_vectors_20f"],
            "predictions": data_20f["stage1_predictions"],
        },
        "20f_stage2": {
            "test_vectors": data_20f["test_vectors_20f"],
            "predictions": data_20f["stage2_predictions"],
        },
        "signals": data_20f["signals"],
    }

    out_path = os.path.join(FIXTURE_DIR, "expected_predictions.json")
    with open(out_path, "w") as f:
        json.dump(expected, f, indent=2)
    print(f"  Saved {out_path}")

    print("\nDone! Fixture files ready in tests/fixtures/")
    print("2F predictions:", preds_2f)


if __name__ == "__main__":
    main()
