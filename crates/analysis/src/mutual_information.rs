/// Compute mutual information between two continuous variables using histogram binning.
pub fn mutual_information(xs: &[f32], ys: &[f32], bins: usize) -> f32 {
    let n = xs.len();
    if n == 0 || n != ys.len() || bins == 0 {
        return 0.0;
    }

    let x_min = xs.iter().copied().fold(f32::INFINITY, f32::min);
    let x_max = xs.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let y_min = ys.iter().copied().fold(f32::INFINITY, f32::min);
    let y_max = ys.iter().copied().fold(f32::NEG_INFINITY, f32::max);

    let x_range = x_max - x_min + 1e-10;
    let y_range = y_max - y_min + 1e-10;

    let mut joint = vec![vec![0u32; bins]; bins];
    let mut x_hist = vec![0u32; bins];
    let mut y_hist = vec![0u32; bins];

    for i in 0..n {
        let xi = ((xs[i] - x_min) / x_range * bins as f32) as usize;
        let yi = ((ys[i] - y_min) / y_range * bins as f32) as usize;
        let xi = xi.min(bins - 1);
        let yi = yi.min(bins - 1);
        joint[xi][yi] += 1;
        x_hist[xi] += 1;
        y_hist[yi] += 1;
    }

    let nf = n as f32;
    let mut mi = 0.0f32;
    for xi in 0..bins {
        for yi in 0..bins {
            let pxy = joint[xi][yi] as f32 / nf;
            let px = x_hist[xi] as f32 / nf;
            let py = y_hist[yi] as f32 / nf;
            if pxy > 0.0 && px > 0.0 && py > 0.0 {
                mi += pxy * (pxy / (px * py)).ln();
            }
        }
    }
    mi.max(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_identical_high_mi() {
        let xs: Vec<f32> = (0..1000).map(|i| i as f32).collect();
        let mi = mutual_information(&xs, &xs, 20);
        assert!(mi > 2.0, "identical data should have high MI, got {}", mi);
    }

    #[test]
    fn test_independent_low_mi() {
        // Two sequences with different periods (deterministic for reproducibility)
        // With mod arithmetic these aren't perfectly independent, so use a relaxed threshold
        let xs: Vec<f32> = (0..1000).map(|i| (i * 7 % 100) as f32).collect();
        let ys: Vec<f32> = (0..1000).map(|i| (i * 13 % 100) as f32).collect();
        let mi = mutual_information(&xs, &ys, 10);
        // MI of these pseudo-independent sequences should be much less than identical (>2.0)
        assert!(mi < 1.0, "pseudo-independent data should have low MI, got {}", mi);
    }
}
