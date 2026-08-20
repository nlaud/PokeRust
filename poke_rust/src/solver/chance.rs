//! Selects the turn outcomes that the search examines.
//!
//! `simulate_turn` returns each outcome and its probability.
//! Each search depth multiplies the tree by the outcome count.
//! One 16-roll singles turn can return more than 500 outcomes.
//!
//! `Enumerate` keeps the exact distribution but limits practical depth.
//! Other modes discard outcomes to permit more depth.
//! The search reports the discarded probability.
//!
//! `simulate_turn` sorts outcomes by decreasing probability.
//! `TopK` takes the first outcomes without another sort.

use crate::simulator::helpers::sample_indices_weighted;

/// How much of a chance node's successor distribution to keep.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ChanceMode {
    /// Keeps every successor at its true probability.
    Enumerate,
    /// The `k` most likely successors, renormalized to sum to 1.
    ///
    /// Favors common outcomes and can remove important tail outcomes.
    /// Always keeps at least one branch.
    TopK(usize),
    /// Every successor at or above probability `t`, renormalized.
    ///
    /// Adapts the kept count to the probability distribution.
    /// Always keeps at least one branch.
    Threshold(f64),
    /// `n` successors drawn by weight, with replacement, each weighted by how
    /// often it came up.
    ///
    /// Preserves tail outcomes in expectation.
    /// Adds variance and requires `solve_seeded` for reproducibility.
    Sample(usize),
}

impl ChanceMode {
    /// Reduces a weighted successor list.
    /// Returns normalized branches and the removed probability mass.
    /// Input branches must use descending probability order.
    pub fn apply<T>(&self, mut branches: Vec<(T, f64)>) -> (Vec<(T, f64)>, f64) {
        if branches.len() <= 1 {
            return (renormalize(branches), 0.0);
        }

        match *self {
            ChanceMode::Enumerate => (renormalize(branches), 0.0),

            ChanceMode::TopK(k) => {
                let keep = k.max(1);
                if keep >= branches.len() {
                    return (renormalize(branches), 0.0);
                }
                let total: f64 = branches.iter().map(|(_, p)| p).sum();
                branches.truncate(keep);
                let kept: f64 = branches.iter().map(|(_, p)| p).sum();
                (renormalize(branches), discarded(total, kept))
            }

            ChanceMode::Threshold(t) => {
                let total: f64 = branches.iter().map(|(_, p)| p).sum();
                // Sorted descending, so the survivors are a prefix — and the
                // floor of one keeps a node whose branches are all individually
                // unlikely from evaporating entirely.
                let keep = branches.iter().take_while(|(_, p)| *p >= t).count().max(1);
                branches.truncate(keep);
                let kept: f64 = branches.iter().map(|(_, p)| p).sum();
                (renormalize(branches), discarded(total, kept))
            }

            ChanceMode::Sample(n) => {
                let draws = n.max(1);
                if draws >= branches.len() {
                    return (renormalize(branches), 0.0);
                }
                (sample_with_replacement(branches, draws), 0.0)
            }
        }
    }
}

/// Draws branches with replacement.
/// Weights each distinct result by its draw count.
fn sample_with_replacement<T>(branches: Vec<(T, f64)>, draws: usize) -> Vec<(T, f64)> {
    let weights: Vec<f64> = branches.iter().map(|(_, p)| *p).collect();

    // One distribution for the whole batch. Drawing one index at a time built
    // an index vector and a fresh distribution for each draw, which is
    // `O(draws * branches)` of work for the same index sequence.
    let mut counts = vec![0usize; branches.len()];
    for picked in sample_indices_weighted(&weights, draws) {
        counts[picked] += 1;
    }

    let share = 1.0 / draws as f64;
    let kept: Vec<(T, f64)> = branches
        .into_iter()
        .zip(&counts)
        .filter(|&(_, &count)| count > 0)
        .map(|((state, _), &count)| (state, count as f64 * share))
        .collect();

    // Only if every draw somehow failed; renormalize handles the empty case.
    renormalize(kept)
}

/// Rescales branch weights to sum to one.
fn renormalize<T>(mut branches: Vec<(T, f64)>) -> Vec<(T, f64)> {
    let total: f64 = branches.iter().map(|(_, p)| p).sum();
    if total <= 0.0 {
        let share = if branches.is_empty() {
            0.0
        } else {
            1.0 / branches.len() as f64
        };
        for (_, p) in branches.iter_mut() {
            *p = share;
        }
        return branches;
    }
    for (_, p) in branches.iter_mut() {
        *p /= total;
    }
    branches
}

/// Returns the discarded probability fraction without negative rounding error.
fn discarded(total: f64, kept: f64) -> f64 {
    if total <= 0.0 {
        0.0
    } else {
        (1.0 - kept / total).max(0.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::simulator::scoped_sample_rng;

    /// Descending, as `simulate_turn` returns them.
    fn branches() -> Vec<(char, f64)> {
        vec![('a', 0.5), ('b', 0.3), ('c', 0.15), ('d', 0.05)]
    }

    fn total(branches: &[(char, f64)]) -> f64 {
        branches.iter().map(|(_, p)| p).sum()
    }

    #[test]
    fn enumerate_keeps_everything() {
        let (kept, dropped) = ChanceMode::Enumerate.apply(branches());
        assert_eq!(kept.len(), 4);
        assert_eq!(dropped, 0.0);
        assert!((total(&kept) - 1.0).abs() < 1e-12);
    }

    #[test]
    fn top_k_keeps_the_likeliest_and_renormalizes() {
        let (kept, dropped) = ChanceMode::TopK(2).apply(branches());
        assert_eq!(kept.iter().map(|(c, _)| *c).collect::<Vec<_>>(), ['a', 'b']);
        assert!((total(&kept) - 1.0).abs() < 1e-12);
        assert!((dropped - 0.2).abs() < 1e-12, "dropped {dropped}");
        // 0.5 : 0.3 renormalized is 0.625 : 0.375.
        assert!((kept[0].1 - 0.625).abs() < 1e-12);
    }

    /// The property the search relies on when it wants an exact answer without
    /// special-casing the mode.
    #[test]
    fn top_k_of_max_equals_enumerate() {
        let (unbounded, dropped) = ChanceMode::TopK(usize::MAX).apply(branches());
        let (enumerated, _) = ChanceMode::Enumerate.apply(branches());
        assert_eq!(unbounded, enumerated);
        assert_eq!(dropped, 0.0);
    }

    #[test]
    fn top_k_never_empties_a_node() {
        let (kept, _) = ChanceMode::TopK(0).apply(branches());
        assert_eq!(kept.len(), 1);
        assert!((kept[0].1 - 1.0).abs() < 1e-12);
    }

    #[test]
    fn threshold_keeps_the_qualifying_prefix() {
        let (kept, dropped) = ChanceMode::Threshold(0.15).apply(branches());
        assert_eq!(
            kept.iter().map(|(c, _)| *c).collect::<Vec<_>>(),
            ['a', 'b', 'c']
        );
        assert!((dropped - 0.05).abs() < 1e-12);
        assert!((total(&kept) - 1.0).abs() < 1e-12);
    }

    /// A node smeared across many individually-unlikely branches must not
    /// collapse to nothing just because none of them clears the bar.
    #[test]
    fn threshold_floors_at_one_branch() {
        let smeared: Vec<(usize, f64)> = (0..100).map(|i| (i, 0.01)).collect();
        let (kept, dropped) = ChanceMode::Threshold(0.5).apply(smeared);
        assert_eq!(kept.len(), 1);
        assert!((kept[0].1 - 1.0).abs() < 1e-12);
        assert!((dropped - 0.99).abs() < 1e-9, "dropped {dropped}");
    }

    #[test]
    fn sample_returns_a_distribution_and_is_seed_reproducible() {
        let first = {
            let _guard = scoped_sample_rng(7);
            ChanceMode::Sample(3).apply(branches()).0
        };
        let second = {
            let _guard = scoped_sample_rng(7);
            ChanceMode::Sample(3).apply(branches()).0
        };
        assert_eq!(first, second);
        assert!((total(&first) - 1.0).abs() < 1e-12);
        assert!(!first.is_empty() && first.len() <= 3);
    }

    /// Asking for at least as many draws as there are branches is pointless
    /// extra RNG for a strictly worse answer, so it falls through to exact.
    #[test]
    fn sample_beyond_the_branch_count_is_exact() {
        let _guard = scoped_sample_rng(1);
        let (kept, dropped) = ChanceMode::Sample(10).apply(branches());
        assert_eq!(kept, ChanceMode::Enumerate.apply(branches()).0);
        assert_eq!(dropped, 0.0);
    }

    #[test]
    fn a_single_branch_passes_through_every_mode() {
        for mode in [
            ChanceMode::Enumerate,
            ChanceMode::TopK(1),
            ChanceMode::Threshold(0.99),
            ChanceMode::Sample(4),
        ] {
            let (kept, dropped) = mode.apply(vec![('x', 0.25)]);
            assert_eq!(kept, vec![('x', 1.0)], "mode {mode:?}");
            assert_eq!(dropped, 0.0);
        }
    }

    #[test]
    fn empty_input_is_not_a_panic() {
        let (kept, dropped) = ChanceMode::TopK(4).apply(Vec::<(char, f64)>::new());
        assert!(kept.is_empty());
        assert_eq!(dropped, 0.0);
    }
}
