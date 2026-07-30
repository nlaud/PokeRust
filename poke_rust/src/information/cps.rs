//! Samples a fixed-size subset with conditional Poisson sampling.
//!
//! The determinizer uses this method for move sets and bench selections.
//! Usage data gives a marginal rate for each candidate.
//! The sampler selects a fixed number of candidates.
//!
//! ## The model
//!
//! The model uses `P(S) ∝ ∏_{i∈S} w_i` for each subset of size `n`.
//! It gives the maximum-entropy distribution for the specified marginal rates.
//! The model does not include correlations because the source does not provide them.
//!
//! ## Fitting
//!
//! Weights `w` produce the marginal rates but do not equal them.
//! Inclusion uses `π_i(w) = w_i · e_{n-1}(w \ i) / e_n(w)`.
//! Fitting starts with `w_i = t_i / (1 - t_i)`.
//! Each update uses `w_i ← w_i · t_i / π_i(w)`.
//!
//! The fit recalculates `e_{n-1}(w \ i)` for each `i`.
//! This `O(K²·n)` method remains stable when one weight dominates.
//! Candidate pools contain at most 14 entries.
//!
//! ## Drawing
//!
//! Normal pools contain at most 1,001 subsets.
//! The code enumerates these subsets and calls `sample_one_weighted`.
//! This process draws exactly from the model and supports the seeded random generator.
//! Larger pools use approximate successive sampling.

use crate::simulator::helpers::sample_one_weighted;

/// Above this many candidate subsets, fall back to successive sampling rather
/// than enumerating. `C(14,4) = 1001`, so every move-set draw stays exact.
const MAX_ENUMERATED_SUBSETS: usize = 20_000;

/// Iteration cap for the weight fit. Convergence is fast (tens of passes even
/// with a near-certain entry), so this is a safety net, not a working budget.
const MAX_FIT_ITERATIONS: usize = 500;

/// Elementary symmetric polynomials `e_0..=e_n` of `w`.
///
/// `e_k` is the sum over all size-`k` products of distinct entries, built by the
/// standard `O(K·n)` dynamic program.
fn elementary_symmetric(w: &[f64], n: usize) -> Vec<f64> {
    let mut e = vec![0.0; n + 1];
    e[0] = 1.0;
    for &wi in w {
        for k in (1..=n).rev() {
            e[k] += wi * e[k - 1];
        }
    }
    e
}

/// The inclusion probability of each item under conditional Poisson weights `w`.
///
/// Exposed for testing: this is the function `fit_cp_weights` inverts, so
/// asserting `cp_inclusion_probabilities(fit_cp_weights(t, n), n) == t` is the
/// direct check that the fit is correct.
pub(crate) fn cp_inclusion_probabilities(w: &[f64], n: usize) -> Vec<f64> {
    let k = w.len();
    if n == 0 {
        return vec![0.0; k];
    }
    if n >= k {
        return vec![1.0; k];
    }

    let denom = elementary_symmetric(w, n)[n];
    if !denom.is_finite() || denom <= 0.0 {
        // Degenerate weights carry no information; spread the mass evenly.
        return vec![n as f64 / k as f64; k];
    }

    let mut rest = Vec::with_capacity(k - 1);
    (0..k)
        .map(|i| {
            rest.clear();
            rest.extend(w.iter().enumerate().filter(|(j, _)| *j != i).map(|(_, v)| *v));
            let numer = w[i] * elementary_symmetric(&rest, n - 1)[n - 1];
            if numer.is_finite() {
                (numer / denom).clamp(0.0, 1.0)
            } else {
                1.0
            }
        })
        .collect()
}

/// Recover conditional-Poisson weights inducing the given inclusion targets.
///
/// `targets` should already sum to `n` (use `cap_normalize`); if it does not the
/// fit still converges to the closest achievable point, it just will not match
/// the requested marginals.
pub(crate) fn fit_cp_weights(targets: &[f64], n: usize) -> Vec<f64> {
    // Odds initialization: exact for the unconditioned Poisson case, and close
    // enough that the iteration usually needs only a handful of passes.
    let mut w: Vec<f64> = targets
        .iter()
        .map(|t| {
            let t = t.clamp(1e-12, 1.0 - 1e-12);
            t / (1.0 - t)
        })
        .collect();
    if w.len() <= n {
        return w;
    }

    for _ in 0..MAX_FIT_ITERATIONS {
        let pi = cp_inclusion_probabilities(&w, n);
        let mut max_err: f64 = 0.0;
        for i in 0..w.len() {
            max_err = max_err.max((pi[i] - targets[i]).abs());
            if targets[i] <= 0.0 {
                w[i] = 1e-12;
                continue;
            }
            // Match odds rather than probabilities. The plain ratio update
            // `w *= t/π` converges only linearly and stalls around 1e-4 on
            // targets containing a near-certain entry; correcting on the odds
            // scale reaches machine-level agreement in a few dozen passes.
            let t = targets[i].clamp(1e-12, 1.0 - 1e-12);
            let p = pi[i].clamp(1e-12, 1.0 - 1e-12);
            let ratio = (t / (1.0 - t)) * ((1.0 - p) / p);
            w[i] = (w[i] * ratio).clamp(1e-12, 1e12);
        }
        if max_err < 1e-12 {
            break;
        }
    }
    w
}

/// Scale `p` to sum to `n`, pinning anything that would exceed 1.0.
///
/// A pinned entry is deterministically included — no probability model can put
/// more than certainty on an item — and the remaining mass is redistributed over
/// the others. Terminates in at most `p.len()` passes since each one pins at
/// least one new entry. Each pass rescales from the *original* values rather
/// than compounding, so the result does not depend on how many passes it took.
pub(crate) fn cap_normalize(p: &[f64], n: usize) -> Vec<f64> {
    let k = p.len();
    if k == 0 {
        return Vec::new();
    }
    if n >= k {
        return vec![1.0; k];
    }
    if n == 0 {
        return vec![0.0; k];
    }

    let base: Vec<f64> = p
        .iter()
        .map(|v| if v.is_finite() { v.max(0.0) } else { 0.0 })
        .collect();
    let mut capped = vec![false; k];

    loop {
        let capped_count = capped.iter().filter(|c| **c).count();
        let target = n as f64 - capped_count as f64;
        let free: Vec<usize> = (0..k).filter(|i| !capped[*i]).collect();
        if free.is_empty() || target <= 0.0 {
            break;
        }
        let free_sum: f64 = free.iter().map(|i| base[*i]).sum();

        // No signal left to scale: split the remaining slots evenly.
        if free_sum <= 0.0 {
            let share = (target / free.len() as f64).clamp(0.0, 1.0);
            let mut out = vec![0.0; k];
            for i in 0..k {
                out[i] = if capped[i] { 1.0 } else { share };
            }
            return out;
        }

        let scale = target / free_sum;
        let mut newly_capped = false;
        for &i in &free {
            if base[i] * scale >= 1.0 - 1e-12 {
                capped[i] = true;
                newly_capped = true;
            }
        }
        if !newly_capped {
            let mut out = vec![0.0; k];
            for i in 0..k {
                out[i] = if capped[i] {
                    1.0
                } else {
                    (base[i] * scale).clamp(0.0, 1.0)
                };
            }
            return out;
        }
    }

    (0..k).map(|i| if capped[i] { 1.0 } else { 0.0 }).collect()
}

/// All size-`n` index subsets of `0..k`, in lexicographic order.
fn combinations(k: usize, n: usize) -> Vec<Vec<usize>> {
    if n > k {
        return Vec::new();
    }
    if n == 0 {
        return vec![Vec::new()];
    }
    let mut idx: Vec<usize> = (0..n).collect();
    let mut out = vec![idx.clone()];
    loop {
        // Advance the rightmost index that has room, then repack after it.
        let Some(i) = (0..n).rev().find(|i| idx[*i] < k - n + i) else {
            return out;
        };
        idx[i] += 1;
        for j in i + 1..n {
            idx[j] = idx[j - 1] + 1;
        }
        out.push(idx.clone());
    }
}

/// The number of size-`n` subsets of `k` items, saturating rather than
/// overflowing — the value is only ever compared against a threshold.
fn binomial(k: usize, n: usize) -> usize {
    if n > k {
        return 0;
    }
    let n = n.min(k - n);
    let mut acc: usize = 1;
    for i in 0..n {
        acc = acc.saturating_mul(k - i) / (i + 1);
        if acc >= usize::MAX / 2 {
            return usize::MAX;
        }
    }
    acc
}

/// The largest pool from which a size-`n` draw still takes the exact
/// enumeration path in `sample_fixed_size_subset`.
///
/// Callers that pre-truncate their candidate pool to keep the exactness
/// guarantee need this number, and it depends on `MAX_ENUMERATED_SUBSETS` —
/// deriving it at the call site means hardcoding a constant that silently goes
/// wrong if the threshold moves, and it is easy to get wrong per `n` besides
/// (the caps fall off fast: 200 for pairs, 50 for triples, 27 for quads).
pub(crate) fn max_exact_pool(n: usize) -> usize {
    if n <= 1 {
        return usize::MAX;
    }
    let mut k = n;
    while binomial(k + 1, n) <= MAX_ENUMERATED_SUBSETS {
        k += 1;
    }
    k
}

/// Draw a size-`n` subset whose per-item inclusion probabilities match
/// `marginals` as closely as a fixed-size draw allows.
///
/// Returns the chosen indices (ascending) and the probability of that particular
/// subset. `marginals` need not sum to `n` — they are cap-normalized first — but
/// the closer they do, the less they are distorted.
///
/// Exact for pools small enough to enumerate. Beyond `MAX_ENUMERATED_SUBSETS`
/// it falls back to successive sampling (repeated weighted draws without
/// replacement), which respects the ordering of the weights but does *not*
/// reproduce the marginals exactly. Callers wanting the guarantee should
/// pre-truncate their candidate pool.
pub(crate) fn sample_fixed_size_subset(marginals: &[f64], n: usize) -> (Vec<usize>, f64) {
    let k = marginals.len();
    if n == 0 || k == 0 {
        return (Vec::new(), 1.0);
    }
    if n >= k {
        return ((0..k).collect(), 1.0);
    }

    let capped = cap_normalize(marginals, n);
    let forced: Vec<usize> = (0..k).filter(|i| capped[*i] >= 1.0 - 1e-9).collect();
    let free: Vec<usize> = (0..k)
        .filter(|i| capped[*i] < 1.0 - 1e-9 && capped[*i] > 0.0)
        .collect();

    let remaining = n.saturating_sub(forced.len());
    if remaining == 0 {
        let mut chosen = forced;
        chosen.truncate(n);
        chosen.sort_unstable();
        return (chosen, 1.0);
    }
    if remaining >= free.len() {
        let mut chosen = forced;
        chosen.extend(free);
        chosen.sort_unstable();
        return (chosen, 1.0);
    }

    let targets: Vec<f64> = free.iter().map(|i| capped[*i]).collect();
    let weights = fit_cp_weights(&targets, remaining);

    let (picked, probability) = if binomial(free.len(), remaining) <= MAX_ENUMERATED_SUBSETS {
        draw_by_enumeration(&weights, remaining)
    } else {
        draw_successively(&weights, remaining)
    };

    let mut chosen = forced;
    chosen.extend(picked.into_iter().map(|i| free[i]));
    chosen.sort_unstable();
    (chosen, probability)
}

/// Exact conditional-Poisson draw: enumerate every subset, weight it by the
/// product of its items' weights, pick one.
fn draw_by_enumeration(weights: &[f64], n: usize) -> (Vec<usize>, f64) {
    let subsets = combinations(weights.len(), n);
    let scored: Vec<(Vec<usize>, f64)> = subsets
        .into_iter()
        .map(|s| {
            let w: f64 = s.iter().map(|i| weights[*i]).product();
            (s, if w.is_finite() { w.max(0.0) } else { 0.0 })
        })
        .collect();

    let total: f64 = scored.iter().map(|(_, w)| *w).sum();
    let mut drawn = sample_one_weighted(scored, |(_, w)| *w);
    match drawn.pop() {
        Some((subset, w)) if total > 0.0 => (subset, w / total),
        Some((subset, _)) => (subset, 1.0),
        None => (Vec::new(), 1.0),
    }
}

/// Successive sampling: draw `n` times without replacement, each draw weighted
/// by the remaining items' weights. Approximate — the induced marginals drift
/// from the targets — but linear in the pool size.
fn draw_successively(weights: &[f64], n: usize) -> (Vec<usize>, f64) {
    let mut pool: Vec<usize> = (0..weights.len()).collect();
    let mut chosen = Vec::with_capacity(n);
    let mut probability = 1.0;

    for _ in 0..n {
        if pool.is_empty() {
            break;
        }
        let total: f64 = pool.iter().map(|i| weights[*i].max(0.0)).sum();
        let mut drawn = sample_one_weighted(pool.clone(), |i| weights[*i].max(0.0));
        let Some(pick) = drawn.pop() else { break };
        if total > 0.0 {
            probability *= weights[pick].max(0.0) / total;
        }
        pool.retain(|i| *i != pick);
        chosen.push(pick);
    }
    chosen.sort_unstable();
    (chosen, probability)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::simulator::scoped_sample_rng;
    use std::collections::HashMap;

    fn max_abs_diff(a: &[f64], b: &[f64]) -> f64 {
        a.iter()
            .zip(b)
            .map(|(x, y)| (x - y).abs())
            .fold(0.0, f64::max)
    }

    #[test]
    fn elementary_symmetric_matches_the_definition() {
        let w = [2.0, 3.0, 5.0];
        let e = elementary_symmetric(&w, 3);
        assert_eq!(e[0], 1.0);
        assert_eq!(e[1], 10.0); // 2+3+5
        assert_eq!(e[2], 31.0); // 6+10+15
        assert_eq!(e[3], 30.0); // 2*3*5
    }

    #[test]
    fn combinations_are_complete_and_distinct() {
        let c = combinations(5, 3);
        assert_eq!(c.len(), 10);
        assert_eq!(c.len(), binomial(5, 3));
        let unique: std::collections::HashSet<_> = c.iter().cloned().collect();
        assert_eq!(unique.len(), c.len());
        assert!(c.iter().all(|s| s.len() == 3 && s.windows(2).all(|w| w[0] < w[1])));
        assert_eq!(combinations(3, 0), vec![Vec::<usize>::new()]);
        assert!(combinations(2, 3).is_empty());
    }

    /// The core analytic guarantee: fitted weights reproduce the requested
    /// marginals. This is what makes a sampled move set match the usage data.
    #[test]
    fn fitted_weights_reproduce_the_targets() {
        // Deliberately includes a near-certain entry (0.995), which is where a
        // deflation-based e_k would lose precision.
        for (targets, n) in [
            (vec![0.995, 0.8, 0.6, 0.4, 0.205], 3usize),
            (vec![0.891, 0.843, 0.788, 0.730, 0.321, 0.162, 0.086, 0.028, 0.025, 0.023], 3),
            (vec![0.5, 0.5, 0.5, 0.5], 2),
            (vec![0.9, 0.05, 0.05], 1),
        ] {
            let normalized = cap_normalize(&targets, n);
            let w = fit_cp_weights(&normalized, n);
            let got = cp_inclusion_probabilities(&w, n);
            assert!(
                max_abs_diff(&got, &normalized) < 1e-6,
                "targets {normalized:?} -> got {got:?}"
            );
        }
    }

    #[test]
    fn cap_normalize_sums_to_n_and_respects_the_ceiling() {
        for (p, n) in [
            (vec![0.9, 0.8, 0.7, 0.6, 0.5], 3usize),
            (vec![3.0, 0.1, 0.1], 2), // wildly over 1.0 -> must pin
            (vec![0.1, 0.1, 0.1, 0.1], 2),
            (vec![0.0, 0.0, 1.0], 1),
        ] {
            let out = cap_normalize(&p, n);
            let sum: f64 = out.iter().sum();
            assert!((sum - n as f64).abs() < 1e-9, "p={p:?} n={n} -> {out:?}");
            assert!(out.iter().all(|v| (0.0..=1.0).contains(v)), "{out:?}");
        }
    }

    #[test]
    fn cap_normalize_pins_rather_than_exceeding_one() {
        // Item 0 would scale to 1.875 of a slot, which is not a probability;
        // it gets pinned and its surplus is redistributed over the rest.
        let out = cap_normalize(&[3.0, 0.1, 0.1], 2);
        assert_eq!(out[0], 1.0);
        assert!((out[1] - 0.5).abs() < 1e-9, "{out:?}");
        assert!((out[2] - 0.5).abs() < 1e-9, "{out:?}");

        // Pinning is only for entries that would *exceed* certainty. Two items
        // that scale to just under 1.0 are left proportional, not rounded up —
        // the third option keeps its small but real share.
        let out = cap_normalize(&[3.0, 3.0, 0.001], 2);
        assert!(out[0] > 0.999 && out[0] < 1.0, "{out:?}");
        assert!(out[2] > 0.0, "the long shot must stay reachable: {out:?}");
        assert!((out.iter().sum::<f64>() - 2.0).abs() < 1e-9);
    }

    #[test]
    fn cap_normalize_handles_a_zero_signal() {
        let out = cap_normalize(&[0.0, 0.0, 0.0, 0.0], 2);
        assert!((out.iter().sum::<f64>() - 2.0).abs() < 1e-9);
        // With nothing to go on, every item is equally likely.
        assert!(out.windows(2).all(|w| (w[0] - w[1]).abs() < 1e-12));
    }

    /// Monte Carlo: the empirical inclusion rate of each item must match its
    /// requested marginal. This is the end-to-end check that the fit and the
    /// draw agree.
    #[test]
    fn sampled_marginals_match_the_targets() {
        let targets = vec![0.891, 0.843, 0.788, 0.730, 0.321, 0.162, 0.086, 0.028, 0.025, 0.023];
        let n = 3;
        let normalized = cap_normalize(&targets, n);

        const DRAWS: usize = 60_000;
        let mut counts = vec![0usize; targets.len()];
        for seed in 0..DRAWS {
            let _guard = scoped_sample_rng(seed as u64);
            let (subset, _) = sample_fixed_size_subset(&targets, n);
            assert_eq!(subset.len(), n, "every draw must fill exactly {n} slots");
            for i in subset {
                counts[i] += 1;
            }
        }

        for (i, count) in counts.iter().enumerate() {
            let observed = *count as f64 / DRAWS as f64;
            let expected = normalized[i];
            // 4 sigma on a binomial proportion, plus a small floor for the
            // very-low-rate entries.
            let sigma = (expected * (1.0 - expected) / DRAWS as f64).sqrt();
            let tolerance = 4.0 * sigma + 0.002;
            assert!(
                (observed - expected).abs() < tolerance,
                "item {i}: observed {observed:.4}, expected {expected:.4}"
            );
        }
    }

    #[test]
    fn subset_probabilities_form_a_distribution() {
        // Enumerate every subset the sampler could return and check the reported
        // probabilities sum to 1.
        let targets = vec![0.7, 0.6, 0.5, 0.2];
        let n = 2;
        let mut seen: HashMap<Vec<usize>, f64> = HashMap::new();
        for seed in 0..4_000u64 {
            let _guard = scoped_sample_rng(seed);
            let (subset, p) = sample_fixed_size_subset(&targets, n);
            seen.insert(subset, p);
        }
        assert_eq!(seen.len(), binomial(4, 2), "not every subset was reachable");
        let total: f64 = seen.values().sum();
        assert!((total - 1.0).abs() < 1e-9, "subset probabilities sum to {total}");
    }

    #[test]
    fn certain_items_are_always_included() {
        // A marginal at or above 1.0 must be forced, not merely likely.
        for seed in 0..200u64 {
            let _guard = scoped_sample_rng(seed);
            let (subset, _) = sample_fixed_size_subset(&[1.0, 0.5, 0.3, 0.2], 2);
            assert!(subset.contains(&0), "seed {seed} dropped a certain item");
            assert_eq!(subset.len(), 2);
        }
    }

    #[test]
    fn degenerate_sizes_are_handled() {
        let _guard = scoped_sample_rng(1);
        assert_eq!(sample_fixed_size_subset(&[0.5, 0.5], 0).0, Vec::<usize>::new());
        assert_eq!(sample_fixed_size_subset(&[], 3).0, Vec::<usize>::new());
        // Asking for more than exists returns everything rather than looping.
        assert_eq!(sample_fixed_size_subset(&[0.2, 0.2], 5).0, vec![0, 1]);
        assert_eq!(sample_fixed_size_subset(&[0.2, 0.2, 0.2], 3).0, vec![0, 1, 2]);
    }

    /// A pool too large to enumerate must still return a well-formed draw.
    #[test]
    fn large_pools_fall_back_without_panicking() {
        let targets: Vec<f64> = (0..235).map(|i| 1.0 / (i as f64 + 1.0)).collect();
        assert!(binomial(235, 5) > MAX_ENUMERATED_SUBSETS);
        let _guard = scoped_sample_rng(7);
        let (subset, p) = sample_fixed_size_subset(&targets, 5);
        assert_eq!(subset.len(), 5);
        assert!(subset.windows(2).all(|w| w[0] < w[1]), "must be sorted and distinct");
        assert!(p > 0.0 && p <= 1.0);
    }

    #[test]
    fn draws_are_reproducible_under_a_seed() {
        let targets = vec![0.8, 0.6, 0.4, 0.3, 0.2];
        let first = {
            let _guard = scoped_sample_rng(42);
            sample_fixed_size_subset(&targets, 2)
        };
        let second = {
            let _guard = scoped_sample_rng(42);
            sample_fixed_size_subset(&targets, 2)
        };
        assert_eq!(first, second);
    }
}
