/// Compute Spearman rank correlation coefficient.
pub fn spearman_correlation(xs: &[f32], ys: &[f32]) -> f32 {
    let n = xs.len();
    if n < 2 || n != ys.len() {
        return 0.0;
    }

    let x_ranks = compute_ranks(xs);
    let y_ranks = compute_ranks(ys);

    // Pearson on ranks
    crate::pearson_corr(&x_ranks, &y_ranks)
}

fn compute_ranks(values: &[f32]) -> Vec<f32> {
    let n = values.len();
    let mut indexed: Vec<(usize, f32)> = values.iter().copied().enumerate().collect();
    indexed.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap());

    let mut ranks = vec![0.0f32; n];
    let mut i = 0;
    while i < n {
        let mut j = i;
        while j < n && indexed[j].1 == indexed[i].1 {
            j += 1;
        }
        // Average rank for ties
        let avg_rank = (i + j - 1) as f32 / 2.0 + 1.0;
        for k in i..j {
            ranks[indexed[k].0] = avg_rank;
        }
        i = j;
    }
    ranks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_perfect_positive() {
        let xs = vec![1.0, 2.0, 3.0, 4.0, 5.0];
        let ys = vec![10.0, 20.0, 30.0, 40.0, 50.0];
        let r = spearman_correlation(&xs, &ys);
        assert!((r - 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_perfect_negative() {
        let xs = vec![1.0, 2.0, 3.0, 4.0, 5.0];
        let ys = vec![50.0, 40.0, 30.0, 20.0, 10.0];
        let r = spearman_correlation(&xs, &ys);
        assert!((r - (-1.0)).abs() < 1e-6);
    }
}
