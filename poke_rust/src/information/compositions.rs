//! Uniform sampling of bounded integer compositions.
//!
//! "Spend exactly `total` points across `k` slots, with slot `j` confined to
//! `lo[j]..=hi[j]`" — the determinizer's fallback for EV spreads. It fires only
//! when the belief has ruled out every spread the usage data lists, which means
//! the observed stats are tight enough that the meta has nothing left to say;
//! uniform over what remains feasible is then the honest maximum-entropy answer.
//!
//! The draw is exact, not a heuristic. Counting compositions by dynamic
//! programming gives, for each slot in turn, the number of ways to complete the
//! allocation for every value that slot could take; drawing proportionally to
//! those counts yields a uniformly random point of the feasible set. Rejection
//! sampling would also be uniform but degrades badly when the bounds are tight,
//! which is exactly the regime this exists for.
//!
//! Be aware that a uniform draw *looks* nothing like a real EV spread: it
//! produces smeared allocations like 11/11/11/11/11/11 where players run
//! 32/0/0/0/2/32. That is intended. Anything lumpier would be inventing a prior
//! the data does not support, and the caller has already established that no
//! authored spread fits.

use crate::simulator::helpers::sample_one_weighted;

/// `table[j][s]` = the number of ways to fill slots `j..k` summing to exactly
/// `s`. Saturating, since only ratios and zero-ness matter.
fn count_table(lo: &[u32], hi: &[u32], total: u32) -> Vec<Vec<u64>> {
    let k = lo.len();
    let t = total as usize;
    let mut table = vec![vec![0u64; t + 1]; k + 1];
    // One way to fill zero slots: spend nothing.
    table[k][0] = 1;

    for j in (0..k).rev() {
        for s in 0..=t {
            let ceiling = hi[j].min(s as u32);
            if lo[j] > ceiling {
                continue;
            }
            let mut acc = 0u64;
            for v in lo[j]..=ceiling {
                acc = acc.saturating_add(table[j + 1][s - v as usize]);
            }
            table[j][s] = acc;
        }
    }
    table
}

/// The number of allocations satisfying the bounds, saturating at `u64::MAX`.
///
/// Not used by the determinizer, which learns infeasibility from
/// `sample_bounded_composition` returning `None`; kept because it is what the
/// tests cross-check the DP against.
#[allow(dead_code)]
pub(crate) fn count_bounded_compositions(lo: &[u32], hi: &[u32], total: u32) -> u64 {
    if lo.len() != hi.len() || lo.iter().zip(hi).any(|(l, h)| l > h) {
        return 0;
    }
    count_table(lo, hi, total)[0][total as usize]
}

/// Draw one allocation uniformly at random, or `None` if the bounds admit none.
///
/// Panics-free: mismatched or inverted bounds simply yield `None`.
pub(crate) fn sample_bounded_composition(lo: &[u32], hi: &[u32], total: u32) -> Option<Vec<u32>> {
    let k = lo.len();
    if k != hi.len() || lo.iter().zip(hi).any(|(l, h)| l > h) {
        return None;
    }
    let table = count_table(lo, hi, total);
    if table[0][total as usize] == 0 {
        return None;
    }

    let mut out = Vec::with_capacity(k);
    let mut remaining = total as usize;
    for j in 0..k {
        let ceiling = hi[j].min(remaining as u32);
        // Weight each candidate value by how many completions it leaves; that
        // ratio is what makes the overall draw uniform rather than merely legal.
        let choices: Vec<(u32, f64)> = (lo[j]..=ceiling)
            .filter_map(|v| {
                let ways = table[j + 1][remaining - v as usize];
                (ways > 0).then_some((v, ways as f64))
            })
            .collect();
        let (value, _) = sample_one_weighted(choices, |(_, ways)| *ways).pop()?;
        out.push(value);
        remaining -= value as usize;
    }
    debug_assert_eq!(remaining, 0, "composition did not spend the full budget");
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::simulator::scoped_sample_rng;
    use std::collections::HashMap;

    /// Brute-force every allocation, for cross-checking the DP.
    fn enumerate(lo: &[u32], hi: &[u32], total: u32) -> Vec<Vec<u32>> {
        fn go(lo: &[u32], hi: &[u32], j: usize, left: u32, cur: &mut Vec<u32>, out: &mut Vec<Vec<u32>>) {
            if j == lo.len() {
                if left == 0 {
                    out.push(cur.clone());
                }
                return;
            }
            for v in lo[j]..=hi[j].min(left) {
                cur.push(v);
                go(lo, hi, j + 1, left - v, cur, out);
                cur.pop();
            }
        }
        let mut out = Vec::new();
        go(lo, hi, 0, total, &mut Vec::new(), &mut out);
        out
    }

    #[test]
    fn counts_match_brute_force() {
        for (lo, hi, total) in [
            (vec![0, 0, 0], vec![3, 3, 3], 4u32),
            (vec![1, 0, 2], vec![4, 5, 6], 7),
            (vec![0; 6], vec![32; 6], 12),
            (vec![0, 0], vec![2, 2], 5), // infeasible: max is 4
        ] {
            let expected = enumerate(&lo, &hi, total).len() as u64;
            assert_eq!(
                count_bounded_compositions(&lo, &hi, total),
                expected,
                "lo={lo:?} hi={hi:?} total={total}"
            );
        }
    }

    #[test]
    fn infeasible_bounds_yield_none() {
        let _guard = scoped_sample_rng(1);
        // Ceiling too low.
        assert_eq!(sample_bounded_composition(&[0, 0], &[2, 2], 5), None);
        // Floor too high.
        assert_eq!(sample_bounded_composition(&[4, 4], &[5, 5], 3), None);
        // Inverted bounds.
        assert_eq!(sample_bounded_composition(&[3], &[1], 3), None);
        // Mismatched lengths.
        assert_eq!(sample_bounded_composition(&[0, 0], &[1], 1), None);
    }

    #[test]
    fn every_draw_respects_the_bounds_and_budget() {
        let lo = [0, 0, 0, 0, 0, 0];
        let hi = [32, 32, 32, 32, 32, 32];
        for seed in 0..500u64 {
            let _guard = scoped_sample_rng(seed);
            let draw = sample_bounded_composition(&lo, &hi, 66).expect("66 points must fit");
            assert_eq!(draw.iter().sum::<u32>(), 66, "seed {seed}: {draw:?}");
            assert!(
                draw.iter().zip(&hi).all(|(v, h)| v <= h),
                "seed {seed}: {draw:?}"
            );
        }
    }

    /// Tight bounds are the case this exists for: the belief has pinned most
    /// stats and only a little slack remains.
    #[test]
    fn honours_tight_asymmetric_bounds() {
        let lo = [2, 30, 0, 0, 0, 30];
        let hi = [4, 32, 1, 0, 2, 32];
        for seed in 0..200u64 {
            let _guard = scoped_sample_rng(seed);
            let draw = sample_bounded_composition(&lo, &hi, 66).expect("feasible");
            assert_eq!(draw.iter().sum::<u32>(), 66);
            for (i, v) in draw.iter().enumerate() {
                assert!(
                    *v >= lo[i] && *v <= hi[i],
                    "seed {seed} slot {i}: {v} outside {}..={}",
                    lo[i],
                    hi[i]
                );
            }
            // A slot pinned to a single value must always take it.
            assert_eq!(draw[3], 0);
        }
    }

    /// The distinguishing property: not merely legal, but *uniform*. A sampler
    /// that filled slots greedily would pass every test above and fail this one.
    #[test]
    fn draws_are_uniform_over_the_feasible_set() {
        let lo = [0, 0, 0];
        let hi = [3, 3, 3];
        let total = 4;
        let all = enumerate(&lo, &hi, total);
        assert_eq!(all.len(), 12);

        const DRAWS: usize = 60_000;
        let mut counts: HashMap<Vec<u32>, usize> = HashMap::new();
        for seed in 0..DRAWS {
            let _guard = scoped_sample_rng(seed as u64);
            let draw = sample_bounded_composition(&lo, &hi, total).unwrap();
            *counts.entry(draw).or_default() += 1;
        }

        assert_eq!(counts.len(), all.len(), "some allocations were unreachable");
        let expected = DRAWS as f64 / all.len() as f64;
        // 4 sigma on a binomial proportion.
        let sigma = (DRAWS as f64 * (1.0 / all.len() as f64) * (1.0 - 1.0 / all.len() as f64)).sqrt();
        for (allocation, count) in &counts {
            assert!(
                (*count as f64 - expected).abs() < 4.0 * sigma,
                "{allocation:?} drawn {count} times, expected ~{expected:.0}"
            );
        }
    }

    #[test]
    fn a_single_feasible_allocation_is_always_returned() {
        let _guard = scoped_sample_rng(3);
        // Bounds admit exactly one answer.
        assert_eq!(count_bounded_compositions(&[2, 3], &[2, 3], 5), 1);
        assert_eq!(sample_bounded_composition(&[2, 3], &[2, 3], 5), Some(vec![2, 3]));
    }

    #[test]
    fn zero_budget_is_legal_when_floors_allow_it() {
        let _guard = scoped_sample_rng(4);
        assert_eq!(
            sample_bounded_composition(&[0, 0, 0], &[5, 5, 5], 0),
            Some(vec![0, 0, 0])
        );
    }

    #[test]
    fn draws_are_reproducible_under_a_seed() {
        let lo = [0; 6];
        let hi = [32; 6];
        let first = {
            let _guard = scoped_sample_rng(99);
            sample_bounded_composition(&lo, &hi, 66)
        };
        let second = {
            let _guard = scoped_sample_rng(99);
            sample_bounded_composition(&lo, &hi, 66)
        };
        assert_eq!(first, second);
    }
}
