//! Pure-Rust XGBoost model loader and predictor.
//!
//! Parses XGBoost JSON model files (format version [2,1,x]) and performs
//! gradient-boosted tree inference without requiring the XGBoost C library.

use serde::Deserialize;
use std::fs;
use std::path::Path;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON parse error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("Invalid model: {0}")]
    InvalidModel(String),
    #[error("UTF-8 error: {0}")]
    Utf8(#[from] std::str::Utf8Error),
}

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Prediction detail from a two-stage model.
#[derive(Debug, Clone)]
pub struct PredictionDetail {
    pub p_directional: f32,
    pub p_long: Option<f32>,
    pub signal: i32,
}

// ---------------------------------------------------------------------------
// Internal JSON model structures
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct JsonModel {
    learner: Learner,
}

#[derive(Deserialize)]
struct Learner {
    gradient_booster: GradientBooster,
    learner_model_param: LearnerModelParam,
}

#[derive(Deserialize)]
struct GradientBooster {
    model: GbtreeModel,
}

#[derive(Deserialize)]
struct GbtreeModel {
    trees: Vec<JsonTree>,
}

#[derive(Deserialize)]
struct LearnerModelParam {
    base_score: String,
    num_feature: String,
}

#[derive(Deserialize)]
struct JsonTree {
    left_children: Vec<i32>,
    right_children: Vec<i32>,
    split_indices: Vec<u32>,
    split_conditions: Vec<f64>,
    default_left: Vec<u8>,
    base_weights: Vec<f64>,
}

// ---------------------------------------------------------------------------
// Internal tree representation
// ---------------------------------------------------------------------------

struct Tree {
    left_children: Vec<i32>,
    right_children: Vec<i32>,
    split_indices: Vec<u32>,
    split_conditions: Vec<f64>,
    default_left: Vec<bool>,
    base_weights: Vec<f64>,
}

impl Tree {
    fn from_json(jt: &JsonTree) -> Self {
        Self {
            left_children: jt.left_children.clone(),
            right_children: jt.right_children.clone(),
            split_indices: jt.split_indices.clone(),
            split_conditions: jt.split_conditions.clone(),
            default_left: jt.default_left.iter().map(|&v| v != 0).collect(),
            base_weights: jt.base_weights.clone(),
        }
    }

    fn predict(&self, features: &[f32]) -> f64 {
        let mut node: usize = 0;
        loop {
            let left = self.left_children[node];
            if left == -1 {
                // Leaf node
                return self.base_weights[node];
            }

            let feat_idx = self.split_indices[node] as usize;
            let threshold = self.split_conditions[node];
            let feat_val = if feat_idx < features.len() {
                features[feat_idx] as f64
            } else {
                f64::NAN
            };

            node = if feat_val.is_nan() {
                if self.default_left[node] {
                    left as usize
                } else {
                    self.right_children[node] as usize
                }
            } else if feat_val < threshold {
                left as usize
            } else {
                self.right_children[node] as usize
            };
        }
    }
}

// ---------------------------------------------------------------------------
// GbtModel
// ---------------------------------------------------------------------------

/// A gradient-boosted tree model loaded from XGBoost JSON format.
pub struct GbtModel {
    trees: Vec<Tree>,
    base_score_logit: f64,
}

fn sigmoid(x: f64) -> f64 {
    1.0 / (1.0 + (-x).exp())
}

fn logit(p: f64) -> f64 {
    (p / (1.0 - p)).ln()
}

impl GbtModel {
    /// Load a model from a JSON file path.
    pub fn load(path: &Path) -> Result<Self, Error> {
        let data = fs::read_to_string(path)?;
        Self::from_json_str(&data)
    }

    /// Load a model from a byte buffer (JSON content).
    pub fn load_from_buffer(buf: &[u8]) -> Result<Self, Error> {
        if buf.is_empty() {
            return Err(Error::InvalidModel("empty buffer".into()));
        }
        let data = std::str::from_utf8(buf)?;
        Self::from_json_str(data)
    }

    fn from_json_str(data: &str) -> Result<Self, Error> {
        let model: JsonModel = serde_json::from_str(data)
            .map_err(|e| Error::InvalidModel(format!("failed to parse model JSON: {e}")))?;

        let base_score: f64 = model
            .learner
            .learner_model_param
            .base_score
            .parse()
            .map_err(|_| Error::InvalidModel("invalid base_score".into()))?;

        if model.learner.gradient_booster.model.trees.is_empty() {
            return Err(Error::InvalidModel("model has no trees".into()));
        }

        let trees: Vec<Tree> = model
            .learner
            .gradient_booster
            .model
            .trees
            .iter()
            .map(Tree::from_json)
            .collect();

        Ok(Self {
            trees,
            base_score_logit: logit(base_score),
        })
    }

    /// Predict a single probability for the given features.
    pub fn predict(&self, features: &[f32]) -> Result<f32, Error> {
        let mut raw = self.base_score_logit;
        for tree in &self.trees {
            raw += tree.predict(features);
        }
        Ok(sigmoid(raw) as f32)
    }

    /// Predict probabilities for a batch of samples.
    ///
    /// `flat_features` contains `n_samples * n_features` values in row-major order.
    pub fn predict_batch(
        &self,
        flat_features: &[f32],
        n_samples: usize,
        n_features: usize,
    ) -> Result<Vec<f32>, Error> {
        let mut results = Vec::with_capacity(n_samples);
        for i in 0..n_samples {
            let start = i * n_features;
            let end = start + n_features;
            let sample = &flat_features[start..end];
            results.push(self.predict(sample)?);
        }
        Ok(results)
    }
}

// ---------------------------------------------------------------------------
// Normalization stats
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct NormStats {
    mean: Vec<f32>,
    std: Vec<f32>,
}

// ---------------------------------------------------------------------------
// TwoStageModel
// ---------------------------------------------------------------------------

/// Two-stage prediction model: stage1 (directional filter) + stage2 (long/short).
pub struct TwoStageModel {
    stage1: GbtModel,
    stage2: GbtModel,
    mean: Vec<f32>,
    std_floored: Vec<f32>,
}

impl TwoStageModel {
    /// Load a two-stage model from stage1, stage2, and normalization stats paths.
    pub fn load(
        stage1_path: &Path,
        stage2_path: &Path,
        norm_stats_path: &Path,
    ) -> Result<Self, Error> {
        let stage1 = GbtModel::load(stage1_path)?;
        let stage2 = GbtModel::load(stage2_path)?;

        let norm_data = fs::read_to_string(norm_stats_path)?;
        let stats: NormStats = serde_json::from_str(&norm_data)?;

        if stats.mean.len() != 20 || stats.std.len() != 20 {
            return Err(Error::InvalidModel(format!(
                "norm_stats must have 20 elements, got mean={} std={}",
                stats.mean.len(),
                stats.std.len()
            )));
        }

        let std_floored: Vec<f32> = stats
            .std
            .iter()
            .map(|&s| if s.abs() < 1e-10 { 1.0 } else { s })
            .collect();

        Ok(Self {
            stage1,
            stage2,
            mean: stats.mean,
            std_floored,
        })
    }

    /// Z-score normalize raw features.
    ///
    /// Rules:
    /// - z = (x - mean) / std
    /// - If std < 1e-10, use std = 1.0
    /// - If z is NaN, replace with 0.0
    pub fn normalize(&self, raw: &[f32; 20]) -> [f32; 20] {
        let mut z = [0.0f32; 20];
        for i in 0..20 {
            let val = (raw[i] - self.mean[i]) / self.std_floored[i];
            z[i] = if val.is_nan() { 0.0 } else { val };
        }
        z
    }

    /// Predict signal: 0 (HOLD), +1 (LONG), -1 (SHORT).
    ///
    /// Signal rules:
    ///   P(directional) < 0.50 → 0 (HOLD)
    ///   P(directional) >= 0.50 AND P(long) > 0.50 → +1 (LONG)
    ///   P(directional) >= 0.50 AND P(long) <= 0.50 → -1 (SHORT)
    pub fn predict(&self, raw: &[f32; 20]) -> Result<i32, Error> {
        let detail = self.predict_detailed(raw)?;
        Ok(detail.signal)
    }

    /// Predict with full detail including intermediate probabilities.
    pub fn predict_detailed(&self, raw: &[f32; 20]) -> Result<PredictionDetail, Error> {
        let z = self.normalize(raw);
        let p_dir = self.stage1.predict(&z)?;

        if p_dir < 0.50 {
            Ok(PredictionDetail {
                p_directional: p_dir,
                p_long: None,
                signal: 0,
            })
        } else {
            let p_long = self.stage2.predict(&z)?;
            let signal = if p_long > 0.50 { 1 } else { -1 };
            Ok(PredictionDetail {
                p_directional: p_dir,
                p_long: Some(p_long),
                signal,
            })
        }
    }
}
