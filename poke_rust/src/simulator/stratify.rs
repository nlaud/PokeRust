//! Stratified sampling for a batch of turn transitions.
//!
//! [`generative::sample_transition`](super::generative::sample_transition) draws
//! one successor of a turn. Each draw is independent, so the error of a batch
//! mean falls only with the square root of the sample count.
//!
//! A stratified batch can reduce that error. The batch cuts the unit
//! interval into one stratum per sample, and each sample reads a different
//! stratum. The batch then covers the outcome distribution instead of clustering
//! by chance. A 75-percent accuracy move over 100 stratified samples hits exactly
//! 75 times. The hit count from 100 independent samples has a standard deviation
//! of approximately 4.3.
//!
//! # The plan
//!
//! [`StratifiedPlan`] is a Latin hypercube. It holds one random permutation of
//! `0..samples` for each of the first [`STRATIFIED_DIMENSIONS`] chokepoints of a
//! turn. Sample `i` reads column `i` of every permutation.
//!
//! [`StratifiedPlan::install`] makes one sample the thread-local stream. The
//! stream gives the uniform value of chokepoint `d`:
//!
//! ```text
//! u = (permutation[d][i] + jitter) / samples
//! ```
//!
//! `jitter` is a fresh uniform value in `[0, 1)`. A chokepoint past the last
//! dimension reads no column, and it draws from the normal RNG.
//!
//! # Why one member keeps its law
//!
//! Column `i` of a uniform random permutation is uniform over `0..samples`, so
//! `u` is uniform over `[0, 1)`. Each dimension holds its own permutation, so the
//! coordinates of one sample are independent.
//!
//! One member of a stratified batch therefore has the law of one independent
//! sample. [`TransitionSample::trajectory_probability`] and
//! [`TransitionSample::sampling_probability`] stay correct. Stratification
//! changes the joint law of the batch, not the law of one member. The batch mean
//! stays unbiased. The variance reduction depends on the sampled value.
//!
//! [`TransitionSample::trajectory_probability`]:
//!     super::generative::TransitionSample::trajectory_probability
//! [`TransitionSample::sampling_probability`]:
//!     super::generative::TransitionSample::sampling_probability
//!
//! # Which draws the stream reaches
//!
//! `helpers::sample_one_of` reads the stream only at a recorded chokepoint.
//! Direct turn choices, such as a confusion duration, also read the stream. A
//! weighted draw outside turn resolution passes `record: None`. The determinizer,
//! the team generator, and `solver::chance` keep their independent draws.
//!
//! A chokepoint of one branch is not a decision, and it consumes no dimension. A
//! chokepoint of more than one branch always consumes one dimension, even when
//! its weights are degenerate. The dimension of a chokepoint therefore stays the
//! same across the batch.

use std::cell::RefCell;

use rand::seq::SliceRandom;
use rand::{Rng, SeedableRng, rngs::StdRng};

/// The count of turn chokepoints that a plan covers.
///
/// A turn of a small position passes through far fewer chokepoints than this.
/// A chokepoint past the last dimension keeps the independent draw, so a deep
/// turn stays correct and loses only the variance reduction.
pub const STRATIFIED_DIMENSIONS: usize = 32;

/// Separates the permutations of the plan from the jitter draws of the batch.
const PLAN_SEED_MIX: u64 = 0x9E37_79B9_7F4A_7C15;

thread_local! {
    /// The stratified stream of the sample that resolves now. `None` means that
    /// no batch installed one, which is the normal case.
    static STRATUM_STREAM: RefCell<Option<StratumStream>> = const { RefCell::new(None) };
}

/// The strata that one batch member reads, in chokepoint order.
struct StratumStream {
    /// The stratum index of this member for each dimension.
    strata: Vec<usize>,
    /// The count of samples of the batch.
    samples: usize,
    /// The dimension that the next chokepoint reads.
    cursor: usize,
}

/// One Latin hypercube plan for a batch of `samples` turn resolutions.
///
/// Read the module documentation for the construction and for the reason that
/// the reported probabilities of one member stay correct.
#[derive(Debug, Clone)]
pub struct StratifiedPlan {
    /// One permutation of `0..samples` per dimension.
    columns: Vec<Vec<usize>>,
    /// The count of samples of the batch.
    samples: usize,
}

impl StratifiedPlan {
    /// Build a plan for `samples` members from `seed`.
    ///
    /// One seed always gives one plan. The permutations come from their own RNG
    /// stream, so the plan does not depend on the count of jitter draws.
    pub fn new(samples: usize, seed: u64) -> StratifiedPlan {
        let mut rng = StdRng::seed_from_u64(seed ^ PLAN_SEED_MIX);
        let columns = (0..STRATIFIED_DIMENSIONS)
            .map(|_| {
                let mut column: Vec<usize> = (0..samples).collect();
                column.shuffle(&mut rng);
                column
            })
            .collect();
        StratifiedPlan { columns, samples }
    }

    /// The count of samples of the batch.
    pub fn samples(&self) -> usize {
        self.samples
    }

    /// Make member `sample` the stream of this thread until the guard drops.
    ///
    /// A `sample` value at or above [`StratifiedPlan::samples`] installs an empty
    /// stream. Every chokepoint then falls back to the independent draw.
    pub fn install(&self, sample: usize) -> StratumGuard {
        let strata: Vec<usize> = self
            .columns
            .iter()
            .filter_map(|column| column.get(sample).copied())
            .collect();
        let stream = StratumStream {
            strata,
            samples: self.samples,
            cursor: 0,
        };
        let previous = STRATUM_STREAM.with(|slot| slot.borrow_mut().replace(stream));
        StratumGuard(previous)
    }
}

/// Restores the previous stratified stream when dropped, even when a test
/// unwinds through a simulator panic.
pub struct StratumGuard(Option<StratumStream>);

impl Drop for StratumGuard {
    fn drop(&mut self) {
        STRATUM_STREAM.with(|slot| {
            *slot.borrow_mut() = self.0.take();
        });
    }
}

/// The uniform value of the next chokepoint, or `None` when no stream covers it.
///
/// The call consumes one dimension of the installed stream. `None` means that no
/// batch installed a stream, or that the turn passed the last dimension. The
/// caller then keeps its independent draw.
pub(crate) fn next_uniform() -> Option<f64> {
    let (stratum, samples) = STRATUM_STREAM.with(|slot| {
        let mut slot = slot.borrow_mut();
        let stream = slot.as_mut()?;
        let stratum = *stream.strata.get(stream.cursor)?;
        stream.cursor += 1;
        Some((stratum, stream.samples))
    })?;
    if samples == 0 {
        return None;
    }
    // The borrow above ends before this draw, so the jitter RNG cannot re-enter
    // the stream.
    let jitter: f64 = crate::simulator::with_sample_rng(|rng| rng.gen_range(0.0..1.0));
    Some((stratum as f64 + jitter) / samples as f64)
}

/// The stratified index for a direct uniform choice.
///
/// The function returns `None` when no stream covers this choice. The caller
/// must then use the normal random draw. A choice with fewer than two outcomes
/// consumes no dimension.
pub(crate) fn uniform_index(outcomes: usize) -> Option<usize> {
    if outcomes <= 1 {
        return None;
    }
    next_uniform().map(|uniform| ((uniform * outcomes as f64) as usize).min(outcomes - 1))
}

/// The index of the branch that holds `u` of the cumulative weight.
///
/// Inversion maps one contiguous unit-interval slice to each branch. Only the
/// strata that cross a slice boundary can change its count. The count is exact
/// when every slice boundary is also a stratum boundary.
///
/// `u` must lie in `[0, 1)`. A degenerate weight set returns `None`, and the
/// caller then keeps the first branch.
pub(crate) fn branch_for_uniform(weights: &[f64], u: f64) -> Option<usize> {
    let total: f64 = weights.iter().sum();
    if total <= 0.0 || !total.is_finite() {
        return None;
    }
    let target = u * total;
    let mut cumulative = 0.0;
    for (index, weight) in weights.iter().enumerate() {
        cumulative += weight;
        if target < cumulative {
            return Some(index);
        }
    }
    // Rounding can leave the target above the final sum. Keep the last branch
    // that holds mass.
    weights.iter().rposition(|weight| *weight > 0.0)
}
