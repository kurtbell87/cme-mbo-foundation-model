/// Two-sample t-test (Welch's t-test). Returns t-statistic.
pub fn welch_t_test(xs: &[f32], ys: &[f32]) -> f32 {
    if xs.len() < 2 || ys.len() < 2 {
        return 0.0;
    }
    let (mean_x, var_x) = mean_var(xs);
    let (mean_y, var_y) = mean_var(ys);

    let nx = xs.len() as f32;
    let ny = ys.len() as f32;

    let denom = (var_x / nx + var_y / ny).sqrt();
    if denom < 1e-10 {
        return 0.0;
    }
    (mean_x - mean_y) / denom
}

fn mean_var(data: &[f32]) -> (f32, f32) {
    let n = data.len() as f32;
    let mean = data.iter().sum::<f32>() / n;
    let var = data.iter().map(|&x| (x - mean).powi(2)).sum::<f32>() / (n - 1.0);
    (mean, var)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_same_distribution() {
        let xs = vec![1.0, 2.0, 3.0, 4.0, 5.0];
        let ys = vec![1.0, 2.0, 3.0, 4.0, 5.0];
        let t = welch_t_test(&xs, &ys);
        assert!(t.abs() < 1e-6);
    }

    #[test]
    fn test_different_means() {
        let xs = vec![1.0, 2.0, 3.0, 4.0, 5.0];
        let ys = vec![11.0, 12.0, 13.0, 14.0, 15.0];
        let t = welch_t_test(&xs, &ys);
        assert!(t < -5.0); // xs mean < ys mean, so t is negative
    }
}
