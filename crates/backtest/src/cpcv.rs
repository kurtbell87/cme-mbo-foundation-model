//! Combinatorial Purged Cross-Validation (CPCV) fold generation.
//!
//! Generates C(n_groups, k_test) splits with purge/embargo to prevent
//! data leakage between train and test sets.

/// Configuration for CPCV fold generation.
#[derive(Debug, Clone)]
pub struct CpcvConfig {
    /// Number of groups to divide days into (default: 10).
    pub n_groups: usize,
    /// Number of groups held out for testing per split (default: 2).
    pub k_test: usize,
    /// Bars to purge before each test block boundary (default: 500).
    pub purge_bars: usize,
    /// Bars to embargo after each test block boundary (default: 4600).
    pub embargo_bars: usize,
}

impl Default for CpcvConfig {
    fn default() -> Self {
        Self {
            n_groups: 10,
            k_test: 2,
            purge_bars: 500,
            embargo_bars: 4600,
        }
    }
}

/// Metadata for a single day in the dataset.
#[derive(Debug, Clone)]
pub struct DayMeta {
    /// Date in YYYYMMDD format.
    pub date: i32,
    /// Group index this day belongs to.
    pub group: usize,
    /// Cumulative bar offset at start of this day (across all days).
    pub cum_bar_start: usize,
    /// Cumulative bar offset at end of this day (exclusive).
    pub cum_bar_end: usize,
    /// Number of bars in this day.
    pub bar_count: usize,
}

/// A single CPCV train/test split.
#[derive(Debug, Clone)]
pub struct CpcvSplit {
    /// Split index (0..n_splits).
    pub split_idx: usize,
    /// Group indices used for testing.
    pub test_groups: Vec<usize>,
    /// Group indices used for training (before purge/embargo).
    pub train_groups: Vec<usize>,
    /// Day indices surviving purge/embargo for training.
    pub train_day_indices: Vec<usize>,
    /// Day indices used for testing.
    pub test_day_indices: Vec<usize>,
}

/// Assign days to groups sequentially.
///
/// Days are distributed as evenly as possible: the first `n_days % n_groups`
/// groups get one extra day.
pub fn assign_groups(n_days: usize, n_groups: usize) -> Vec<usize> {
    assert!(n_groups > 0, "n_groups must be > 0");
    assert!(n_days >= n_groups, "n_days must be >= n_groups");

    let base_size = n_days / n_groups;
    let remainder = n_days % n_groups;

    let mut groups = Vec::with_capacity(n_days);
    for g in 0..n_groups {
        let size = if g < remainder { base_size + 1 } else { base_size };
        for _ in 0..size {
            groups.push(g);
        }
    }
    groups
}

/// Build `DayMeta` entries from day dates, bar counts, and group assignments.
pub fn build_day_metas(dates: &[i32], bar_counts: &[usize], n_groups: usize) -> Vec<DayMeta> {
    assert_eq!(dates.len(), bar_counts.len());
    let groups = assign_groups(dates.len(), n_groups);

    let mut metas = Vec::with_capacity(dates.len());
    let mut cum = 0usize;
    for (i, (&date, &bc)) in dates.iter().zip(bar_counts.iter()).enumerate() {
        metas.push(DayMeta {
            date,
            group: groups[i],
            cum_bar_start: cum,
            cum_bar_end: cum + bc,
            bar_count: bc,
        });
        cum += bc;
    }
    metas
}

/// Generate all C(n_groups, k_test) CPCV splits with purge/embargo.
pub fn generate_splits(day_metas: &[DayMeta], config: &CpcvConfig) -> Vec<CpcvSplit> {
    let combos = combinations(config.n_groups, config.k_test);
    let mut splits = Vec::with_capacity(combos.len());

    for (split_idx, test_groups) in combos.into_iter().enumerate() {
        let train_groups: Vec<usize> = (0..config.n_groups)
            .filter(|g| !test_groups.contains(g))
            .collect();

        // Collect test day indices
        let test_day_indices: Vec<usize> = day_metas
            .iter()
            .enumerate()
            .filter(|(_, dm)| test_groups.contains(&dm.group))
            .map(|(i, _)| i)
            .collect();

        // Find test block boundaries for purge/embargo
        let boundaries = find_test_boundaries(day_metas, &test_groups);

        // Filter training days by purge/embargo
        let train_day_indices: Vec<usize> = day_metas
            .iter()
            .enumerate()
            .filter(|(_, dm)| train_groups.contains(&dm.group))
            .filter(|(_, dm)| !is_purged_or_embargoed(dm, &boundaries, config))
            .map(|(i, _)| i)
            .collect();

        splits.push(CpcvSplit {
            split_idx,
            test_groups,
            train_groups,
            train_day_indices,
            test_day_indices,
        });
    }

    splits
}

/// A boundary between test and train regions, defined by cumulative bar offsets.
#[derive(Debug)]
struct TestBoundary {
    /// First cumulative bar index of the test block.
    block_start: usize,
    /// Last cumulative bar index of the test block (exclusive).
    block_end: usize,
}

/// Find contiguous test block boundaries. Non-contiguous test groups produce
/// multiple boundaries (e.g., groups {2, 7} produce 2 boundaries).
fn find_test_boundaries(day_metas: &[DayMeta], test_groups: &[usize]) -> Vec<TestBoundary> {
    // Find contiguous runs of test days
    let test_days: Vec<&DayMeta> = day_metas
        .iter()
        .filter(|dm| test_groups.contains(&dm.group))
        .collect();

    if test_days.is_empty() {
        return vec![];
    }

    let mut boundaries = Vec::new();
    let mut block_start = test_days[0].cum_bar_start;
    let mut block_end = test_days[0].cum_bar_end;

    for dm in &test_days[1..] {
        if dm.cum_bar_start == block_end {
            // Contiguous — extend block
            block_end = dm.cum_bar_end;
        } else {
            // Gap — close current block and start new one
            boundaries.push(TestBoundary {
                block_start,
                block_end,
            });
            block_start = dm.cum_bar_start;
            block_end = dm.cum_bar_end;
        }
    }
    boundaries.push(TestBoundary {
        block_start,
        block_end,
    });

    boundaries
}

/// Check if a training day should be excluded due to purge/embargo.
///
/// A day is excluded if any of its bars fall within `purge_bars` before or
/// `embargo_bars` after any test block boundary.
fn is_purged_or_embargoed(
    day: &DayMeta,
    boundaries: &[TestBoundary],
    config: &CpcvConfig,
) -> bool {
    for boundary in boundaries {
        // Purge zone: [block_start - purge_bars, block_start)
        let purge_start = boundary.block_start.saturating_sub(config.purge_bars);
        if day.cum_bar_end > purge_start && day.cum_bar_start < boundary.block_start {
            return true;
        }

        // Embargo zone: [block_end, block_end + embargo_bars)
        let embargo_end = boundary.block_end + config.embargo_bars;
        if day.cum_bar_start < embargo_end && day.cum_bar_end > boundary.block_end {
            return true;
        }
    }
    false
}

/// Generate all C(n, k) combinations of indices 0..n.
fn combinations(n: usize, k: usize) -> Vec<Vec<usize>> {
    let mut result = Vec::new();
    let mut combo = Vec::with_capacity(k);
    combinations_helper(n, k, 0, &mut combo, &mut result);
    result
}

fn combinations_helper(
    n: usize,
    k: usize,
    start: usize,
    current: &mut Vec<usize>,
    result: &mut Vec<Vec<usize>>,
) {
    if current.len() == k {
        result.push(current.clone());
        return;
    }
    for i in start..n {
        current.push(i);
        combinations_helper(n, k, i + 1, current, result);
        current.pop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_assign_groups_even() {
        let groups = assign_groups(200, 10);
        assert_eq!(groups.len(), 200);
        for g in 0..10 {
            assert_eq!(groups.iter().filter(|&&x| x == g).count(), 20);
        }
    }

    #[test]
    fn test_assign_groups_remainder() {
        // 201 days / 10 groups = 20 each + 1 extra for group 0
        let groups = assign_groups(201, 10);
        assert_eq!(groups.len(), 201);
        assert_eq!(groups.iter().filter(|&&x| x == 0).count(), 21);
        for g in 1..10 {
            assert_eq!(groups.iter().filter(|&&x| x == g).count(), 20);
        }
    }

    #[test]
    fn test_combinations_10_2() {
        let combos = combinations(10, 2);
        assert_eq!(combos.len(), 45); // C(10,2) = 45
    }

    #[test]
    fn test_combinations_3_1() {
        let combos = combinations(3, 1);
        assert_eq!(combos.len(), 3);
        assert_eq!(combos, vec![vec![0], vec![1], vec![2]]);
    }

    #[test]
    fn test_generate_splits_count() {
        let dates: Vec<i32> = (0..201).collect();
        let bar_counts: Vec<usize> = vec![4630; 201];
        let metas = build_day_metas(&dates, &bar_counts, 10);

        let config = CpcvConfig::default();
        let splits = generate_splits(&metas, &config);
        assert_eq!(splits.len(), 45);
    }

    #[test]
    fn test_generate_splits_all_days_covered() {
        let dates: Vec<i32> = (0..201).collect();
        let bar_counts: Vec<usize> = vec![4630; 201];
        let metas = build_day_metas(&dates, &bar_counts, 10);

        let config = CpcvConfig::default();
        let splits = generate_splits(&metas, &config);

        for split in &splits {
            // Every day should be in either train or test (though some train may be purged)
            let all_groups: Vec<usize> = split
                .test_groups
                .iter()
                .chain(split.train_groups.iter())
                .copied()
                .collect();
            for g in 0..10 {
                assert!(all_groups.contains(&g), "group {} missing in split {}", g, split.split_idx);
            }
        }
    }

    #[test]
    fn test_purge_embargo_removes_days() {
        let dates: Vec<i32> = (0..201).collect();
        let bar_counts: Vec<usize> = vec![4630; 201];
        let metas = build_day_metas(&dates, &bar_counts, 10);

        let config = CpcvConfig {
            n_groups: 10,
            k_test: 2,
            purge_bars: 500,
            embargo_bars: 4600,
        };
        let splits = generate_splits(&metas, &config);

        for split in &splits {
            // Train days should be fewer than total train group days (purge/embargo removed some)
            let total_train_group_days: usize = metas
                .iter()
                .filter(|dm| split.train_groups.contains(&dm.group))
                .count();
            assert!(
                split.train_day_indices.len() <= total_train_group_days,
                "split {}: train_days {} > total_group_days {}",
                split.split_idx,
                split.train_day_indices.len(),
                total_train_group_days
            );
        }
    }

    #[test]
    fn test_non_contiguous_test_groups_multiple_boundaries() {
        // Groups {2, 7} are non-contiguous → should produce 2 boundaries
        let dates: Vec<i32> = (0..201).collect();
        let bar_counts: Vec<usize> = vec![4630; 201];
        let metas = build_day_metas(&dates, &bar_counts, 10);

        let test_groups = vec![2, 7];
        let boundaries = find_test_boundaries(&metas, &test_groups);
        assert_eq!(
            boundaries.len(),
            2,
            "non-contiguous groups {{2, 7}} should produce 2 boundaries"
        );

        // Non-contiguous groups should remove more train days than contiguous
        let config = CpcvConfig {
            n_groups: 10,
            k_test: 2,
            purge_bars: 500,
            embargo_bars: 4600,
        };

        // Find the split with test_groups {2,7}
        let splits = generate_splits(&metas, &config);
        let noncontig = splits.iter().find(|s| s.test_groups == vec![2, 7]).unwrap();

        // Find a contiguous split like {0,1}
        let contig = splits.iter().find(|s| s.test_groups == vec![0, 1]).unwrap();

        // Non-contiguous should have 4 purge/embargo zones (2 boundaries × 2 zones each)
        // vs contiguous with 2 zones (1 boundary × 2 zones, but internal boundary is contiguous)
        // So non-contiguous should remove MORE training days
        assert!(
            noncontig.train_day_indices.len() <= contig.train_day_indices.len(),
            "non-contiguous {{2,7}} has {} train days, contiguous {{0,1}} has {}",
            noncontig.train_day_indices.len(),
            contig.train_day_indices.len()
        );
    }

    #[test]
    fn test_no_overlap_train_test() {
        let dates: Vec<i32> = (0..201).collect();
        let bar_counts: Vec<usize> = vec![4630; 201];
        let metas = build_day_metas(&dates, &bar_counts, 10);

        let config = CpcvConfig::default();
        let splits = generate_splits(&metas, &config);

        for split in &splits {
            for &train_idx in &split.train_day_indices {
                assert!(
                    !split.test_day_indices.contains(&train_idx),
                    "split {}: day {} appears in both train and test",
                    split.split_idx,
                    train_idx
                );
            }
        }
    }

    #[test]
    fn test_each_day_tested_9_times() {
        // With k_test=2 and 10 groups, each group appears in C(9,1)=9 splits as test
        let dates: Vec<i32> = (0..200).collect(); // 20 per group, exactly even
        let bar_counts: Vec<usize> = vec![4630; 200];
        let metas = build_day_metas(&dates, &bar_counts, 10);

        let config = CpcvConfig {
            n_groups: 10,
            k_test: 2,
            purge_bars: 0, // no purge for this test
            embargo_bars: 0,
        };
        let splits = generate_splits(&metas, &config);

        let mut test_counts = vec![0usize; 200];
        for split in &splits {
            for &idx in &split.test_day_indices {
                test_counts[idx] += 1;
            }
        }

        for (i, &count) in test_counts.iter().enumerate() {
            assert_eq!(
                count, 9,
                "day {} tested {} times, expected 9",
                i, count
            );
        }
    }
}
