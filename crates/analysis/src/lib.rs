pub mod mutual_information;
pub mod spearman;
pub mod statistical_tests;

pub use mutual_information::mutual_information;
pub use spearman::spearman_correlation;

/// Pearson correlation coefficient.
pub fn pearson_corr(xs: &[f32], ys: &[f32]) -> f32 {
    let n = xs.len();
    if n < 2 {
        return 0.0;
    }
    let nf = n as f32;
    let mut sx = 0.0f32;
    let mut sy = 0.0f32;
    let mut sxy = 0.0f32;
    let mut sxx = 0.0f32;
    let mut syy = 0.0f32;
    for i in 0..n {
        sx += xs[i];
        sy += ys[i];
        sxy += xs[i] * ys[i];
        sxx += xs[i] * xs[i];
        syy += ys[i] * ys[i];
    }
    let cov = nf * sxy - sx * sy;
    let vx = nf * sxx - sx * sx;
    let vy = nf * syy - sy * sy;
    let denom = (vx * vy).sqrt();
    if denom < 1e-8 {
        return 0.0;
    }
    (cov / denom).clamp(-1.0, 1.0)
}
