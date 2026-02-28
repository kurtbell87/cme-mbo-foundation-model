# TDD Spec: XGBoost FFI Wrapper

**Phase:** 1 (Parallel with Phases 0, 2)
**Crate:** `crates/xgboost-ffi/` (new library crate)
**Priority:** HIGH — required for inference in live pipeline.

---

## Context

The validated MBO-DL model is a two-stage XGBoost binary classifier:
- **Stage 1:** Directional filter — P(directional) from 20 features
- **Stage 2:** Long vs short — P(long) from 20 features, only on bars where P(directional) > 0.50

XGBoost models trained in Python can be loaded via the XGBoost C API (`libxgboost.dylib`). This crate wraps that API for safe Rust usage.

---

## What to Build

### 1. Raw FFI Bindings

Wrap the minimal XGBoost C API functions needed for inference:

```c
// From xgboost/c_api.h
int XGBoosterCreate(const DMatrixHandle dmats[], bst_ulong len, BoosterHandle *out);
int XGBoosterFree(BoosterHandle handle);
int XGBoosterLoadModel(BoosterHandle handle, const char *fname);
int XGBoosterLoadModelFromBuffer(BoosterHandle handle, const void *buf, bst_ulong len);

int XGDMatrixCreateFromMat(const float *data, bst_ulong nrow, bst_ulong ncol, float missing, DMatrixHandle *out);
int XGDMatrixFree(DMatrixHandle handle);

int XGBoosterPredict(BoosterHandle handle, DMatrixHandle dmat, int option_mask, unsigned ntree_limit, int training, bst_ulong *out_len, const float **out_result);
```

**Type aliases:**
- `BoosterHandle = *mut c_void`
- `DMatrixHandle = *mut c_void`
- `bst_ulong = u64`

### 2. Safe Wrapper: `GbtModel`

```rust
pub struct GbtModel {
    handle: BoosterHandle,
}

impl GbtModel {
    /// Load a model from a file path (.json or .ubj format)
    pub fn load(path: &Path) -> Result<Self>;

    /// Load a model from an in-memory buffer
    pub fn load_from_buffer(buf: &[u8]) -> Result<Self>;

    /// Predict probability for a single sample (20 features)
    /// Returns the raw prediction (probability for binary:logistic)
    pub fn predict(&self, features: &[f32]) -> Result<f32>;

    /// Predict probabilities for a batch of samples
    /// features: row-major [n_samples × n_features]
    pub fn predict_batch(&self, features: &[f32], n_samples: usize, n_features: usize) -> Result<Vec<f32>>;
}

impl Drop for GbtModel {
    fn drop(&mut self) { /* XGBoosterFree */ }
}
```

### 3. Two-Stage Model: `TwoStageModel`

```rust
pub struct TwoStageModel {
    stage1: GbtModel,         // directional filter
    stage2: GbtModel,         // long vs short
    threshold: f32,           // P(directional) threshold (default 0.50)
    mean: Vec<f32>,           // training mean for z-scoring (20 elements)
    std: Vec<f32>,            // training std for z-scoring (20 elements)
}

impl TwoStageModel {
    /// Load both models + normalization stats
    pub fn load(
        stage1_path: &Path,
        stage2_path: &Path,
        norm_stats_path: &Path,  // JSON with mean/std arrays
    ) -> Result<Self>;

    /// Normalize raw features using saved training statistics
    /// z = (x - mean) / std, NaN -> 0.0, std < 1e-10 -> 1.0
    pub fn normalize(&self, raw_features: &[f32; 20]) -> [f32; 20];

    /// Full inference: normalize -> Stage 1 -> threshold -> Stage 2 -> signal
    /// Returns: -1 (short), 0 (hold), +1 (long)
    pub fn predict(&self, raw_features: &[f32; 20]) -> Result<i8>;

    /// Detailed inference returning intermediate values
    pub fn predict_detailed(&self, raw_features: &[f32; 20]) -> Result<PredictionDetail>;
}

pub struct PredictionDetail {
    pub p_directional: f32,
    pub p_long: Option<f32>,  // None if filtered by Stage 1
    pub signal: i8,           // -1, 0, +1
}
```

### 4. Normalization Stats Format

JSON file with mean and std arrays:

```json
{
    "mean": [0.123, -0.456, ...],  // 20 elements
    "std": [1.234, 0.567, ...],    // 20 elements
    "feature_names": ["weighted_imbalance", "spread", ...]  // 20 elements
}
```

**Normalization rules:**
- `z = (x - mean) / std`
- If `std < 1e-10`, use `std = 1.0`
- If result is NaN, replace with `0.0`

---

## XGBoost Installation

XGBoost C library must be installed on the system. On macOS:

```bash
brew install xgboost
```

This provides `libxgboost.dylib` and headers. The crate should use:

```toml
# Cargo.toml
[build-dependencies]
# None needed if linking dynamically

[dependencies]
# None for FFI — just link

# In build.rs or lib.rs:
# #[link(name = "xgboost")]
```

Alternatively, use `pkg-config` to find the library:

```toml
[build-dependencies]
pkg-config = "0.3"
```

If XGBoost isn't available via pkg-config on macOS, use direct path:
- Library: `/opt/homebrew/lib/libxgboost.dylib`
- Headers: `/opt/homebrew/include/xgboost/`

---

## Model Export Format

The Python-trained models should be exported as JSON for maximum portability:

```python
import xgboost as xgb
model = xgb.XGBClassifier(...)
model.fit(X_train, y_train)
model.save_model("stage1.json")  # JSON format
```

The C API `XGBoosterLoadModel` can load `.json`, `.ubj` (Universal Binary JSON), and legacy `.bin` formats.

---

## XGBoost Tuned Hyperparameters (for reference)

Both stages use identical params:
- `max_depth=6, learning_rate=0.0134, min_child_weight=20`
- `subsample=0.561, colsample_bytree=0.748`
- `reg_alpha=0.0014, reg_lambda=6.586`
- `n_estimators=2000, early_stopping_rounds=50`
- `objective=binary:logistic, eval_metric=logloss`

---

## Exit Criteria

- [ ] `crates/xgboost-ffi/` crate added to workspace, compiles
- [ ] FFI bindings for `XGBoosterCreate`, `XGBoosterFree`, `XGBoosterLoadModel`, `XGBoosterPredict`, `XGDMatrixCreateFromMat`, `XGDMatrixFree`
- [ ] `GbtModel::load()` loads a `.json` model file
- [ ] `GbtModel::predict()` returns prediction for a single sample
- [ ] `GbtModel::predict_batch()` handles multiple samples
- [ ] `Drop` impl frees booster handle (no leaks)
- [ ] `TwoStageModel::normalize()` matches Python z-scoring (NaN→0, std floor at 1e-10)
- [ ] `TwoStageModel::predict()` returns correct {-1, 0, +1} signal
- [ ] All tests pass

---

## Test Plan

### RED Phase Tests

**T1: GbtModel creation and drop** — Create a booster, verify handle is non-null, drop without crash.

**T2: GbtModel::load from JSON** — Load a small test model (2-feature, 10-tree XGBoost binary classifier trained on synthetic data). The test should include a pre-generated `.json` model file checked into `tests/fixtures/`.

**T3: GbtModel::predict single sample** — Given known features, predict matches expected probability (within 1e-6 of Python `xgb.predict()`).

**T4: GbtModel::predict_batch** — Predict for 3 samples at once, results match individual predictions.

**T5: TwoStageModel::normalize** — Test z-scoring:
- Normal case: `(x - mean) / std` matches expected
- Zero-std case: when `std < 1e-10`, output = `(x - mean) / 1.0`
- NaN case: if input produces NaN, output = 0.0

**T6: TwoStageModel::predict signal logic** — Test the three outcomes:
- P(directional) < 0.50 → signal = 0
- P(directional) > 0.50, P(long) > 0.50 → signal = +1
- P(directional) > 0.50, P(long) ≤ 0.50 → signal = -1

**T7: Error handling** — Loading a non-existent model returns Err, not panic.

### GREEN Phase Implementation

1. Create `crates/xgboost-ffi/Cargo.toml`
2. Add to workspace members
3. Create `build.rs` to find XGBoost library (`pkg-config` or explicit path)
4. Implement FFI extern declarations in `src/ffi.rs`
5. Implement `GbtModel` in `src/model.rs`
6. Implement `TwoStageModel` in `src/two_stage.rs`
7. Generate test fixture: small XGBoost model trained on synthetic data (can use a Python script in `tests/fixtures/generate_test_model.py`)
8. Implement all tests

### Test Fixture Generation

To create the test model, write a small Python script:

```python
import xgboost as xgb
import numpy as np

np.random.seed(42)
X = np.random.randn(1000, 2).astype(np.float32)
y = (X[:, 0] + X[:, 1] > 0).astype(int)
model = xgb.XGBClassifier(n_estimators=10, max_depth=3, objective="binary:logistic")
model.fit(X, y)
model.save_model("tests/fixtures/test_model.json")

# Save known predictions for test vectors
test_vectors = np.array([[0.5, 0.5], [-1.0, 1.0], [0.0, 0.0]], dtype=np.float32)
predictions = model.predict_proba(test_vectors)[:, 1]
np.save("tests/fixtures/test_predictions.npy", predictions)
```

Check in both the `.json` model and the expected predictions.
