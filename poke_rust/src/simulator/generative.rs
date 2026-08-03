//! Resolves one turn as a generative transition.
//!
//! A sampling search needs one successor, not the outcome distribution.
//! [`sample_transition`] gives it one: the engine keeps a single branch at each
//! chokepoint, so the cost of a node is one trajectory instead of one
//! distribution.
//!
//! # What the caller gets back
//!
//! [`TransitionSample`] holds the next state, the observations of the turn, and
//! two probabilities:
//!
//! - [`TransitionSample::trajectory_probability`] is the chance that the game
//!   produces this trajectory.
//! - [`TransitionSample::sampling_probability`] is the chance that this function
//!   produced it.
//!
//! The two differ because the engine renormalizes some chokepoints. A chokepoint
//! whose collapsed result later joins sibling branches additively must report the
//! weight of its whole branch set, not the weight of the branch that survived —
//! `helpers::sample_one_branch_renormalized` explains why. The reported figure of
//! `sample_turn_raw` therefore overstates the trajectory at those chokepoints.
//! This module records both accumulators and reports both numbers.
//!
//! A few choices never build a branch set at all. Confusion duration and the
//! Starf Berry stat pick are the examples. Each one draws one uniform result and
//! writes it straight into the state, so no chokepoint sees it. The engine
//! records those draws on the branch that made them, in
//! `PokemonState::direct_choice_log_probability`, and this module reads the
//! total from the branch that survived. A thread-wide count would be wrong,
//! because it would also hold the draws of every branch that a later chokepoint
//! discarded.
//!
//! [`TransitionSample::importance_weight`] is their ratio. A caller that
//! averages a value over many samples multiplies each sample by that weight to
//! remove the bias of the sampler.
//!
//! # What the probabilities describe
//!
//! Both numbers describe a trajectory, not a state. Several trajectories can
//! reach one state, and full enumeration would report their sum, so
//! `trajectory_probability` is a lower bound on the probability of the resulting
//! state. [`sample_turn`](super::sample_turn) states the same limit.
//!
//! The draw itself is unbiased in the state: every chokepoint picks a branch in
//! proportion to its weight, so the state marginal of the sampled distribution is
//! the exact successor distribution. A search that only needs a successor draw
//! can therefore ignore both probabilities.
//!
//! # Observations
//!
//! Resolution tracks the raw trajectory once, and masking is a pure function of
//! it. [`TurnObservations`] holds all three views of that one resolution: the
//! stream each player sees, and the public stream that neither player owns.
//! Resolving the turn a second time for a second view would follow a different
//! random universe — `information::mask_events_for` explains the hazard.
//!
//! A search does not read events, so [`TransitionConfig::observe`] turns event
//! tracking off. The observations are then `None`.
//!
//! # Batches
//!
//! [`sample_transition_batch`] draws many successors of one position as a
//! stratified batch. Each member keeps the law of an independent draw, so both
//! reported probabilities keep their meaning. Stratification can reduce the
//! variance of a batch mean. Read [`super::stratify`] for the construction.

use std::collections::HashMap;

use crate::data::pokemon_move::PokemonMove;
use crate::data::species::Species;
use crate::information::information::{
    InformationEvent, mask_events_for, mask_events_public,
};
use crate::state::battle::{MatchState, Player, PlayerCommand};
use crate::state::dex_data::{MoveData, PokemonData};

use super::stratify::StratifiedPlan;
use super::{sample_turn_raw, scoped_chokepoint_log, scoped_sample_rng, take_direct_choice_log};

/// The three views of one resolved turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnObservations {
    /// What a spectator sees: a percentage for every HP, and the disguise
    /// species of every Illusion user.
    pub public: Vec<InformationEvent>,
    /// What P1 sees.
    pub p1: Vec<InformationEvent>,
    /// What P2 sees.
    pub p2: Vec<InformationEvent>,
}

/// One sampled turn.
#[derive(Debug, Clone)]
pub struct TransitionSample {
    /// The state after the turn.
    pub state: MatchState,
    /// The three views of the turn, or `None` when
    /// [`TransitionConfig::observe`] is false.
    pub observations: Option<TurnObservations>,
    /// The probability that the game produces this trajectory.
    pub trajectory_probability: f64,
    /// The probability that [`sample_transition`] produces this trajectory.
    pub sampling_probability: f64,
}

impl TransitionSample {
    /// `trajectory_probability` divided by `sampling_probability`.
    ///
    /// Multiply an estimate from this sample by this weight to remove the bias of
    /// the sampler. A sampler that drew the trajectory at its true rate returns
    /// one.
    ///
    /// A sampling probability of zero cannot happen for a drawn trajectory, so
    /// the guard against it returns the neutral weight of one.
    pub fn importance_weight(&self) -> f64 {
        if self.sampling_probability > 0.0 {
            self.trajectory_probability / self.sampling_probability
        } else {
            1.0
        }
    }
}

/// How to resolve the turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransitionConfig {
    /// Whether to branch on critical hits.
    pub consider_crit: bool,
    /// Damage rolls per attack, from 1 through 16.
    pub damage_rolls: u8,
    /// Whether to build [`TransitionSample::observations`].
    ///
    /// A search leaves this false and pays no event-tracking cost. A fog-of-war
    /// caller sets it and reads the three streams.
    pub observe: bool,
}

impl Default for TransitionConfig {
    fn default() -> Self {
        TransitionConfig {
            consider_crit: true,
            damage_rolls: 16,
            observe: false,
        }
    }
}

/// Resolve `state` under both commands, and return one sampled successor.
///
/// The turn resolves once. Read the module documentation for what the two
/// probabilities mean and for why they differ.
pub fn sample_transition(
    state: &MatchState,
    p1_cmd: &PlayerCommand,
    p2_cmd: &PlayerCommand,
    move_dex: &HashMap<PokemonMove, MoveData>,
    pokemon_dex: &HashMap<Species, PokemonData>,
    config: TransitionConfig,
) -> TransitionSample {
    // The guard must outlive the resolution: every chokepoint reports into it.
    let recorder = scoped_chokepoint_log();
    // `sample_turn_raw` reads the observer only as an on/off switch for event
    // tracking; the stream it returns is raw, and masking happens below.
    let observer = config.observe.then_some(Player::P1);
    let (mut next_state, raw_events, reported_probability) = sample_turn_raw(
        state,
        p1_cmd,
        p2_cmd,
        move_dex,
        pokemon_dex,
        config.consider_crit,
        config.damage_rolls,
        observer,
    );
    let log = recorder.read();
    drop(recorder);

    // A direct draw makes no branch set, so no chokepoint saw it and the
    // reported figure does not hold it. The sampler drew it at its true rate, so
    // it enters both accumulators and leaves the importance weight alone.
    let direct = take_direct_choice_log(&mut next_state);

    let observations = raw_events.map(|raw| TurnObservations {
        public: mask_events_public(&raw),
        p1: mask_events_for(Player::P1, &raw),
        p2: mask_events_for(Player::P2, &raw),
    });

    // The correction replaces each renormalized chokepoint's group weight with
    // the weight of the branch that survived it. Both clamps only absorb
    // floating-point drift; the exact values already lie in `[0, 1]`.
    let trajectory_probability =
        (reported_probability * (log.correction + direct).exp()).clamp(0.0, 1.0);
    let sampling_probability = (log.sampling + direct).exp().clamp(0.0, 1.0);

    TransitionSample {
        state: next_state,
        observations,
        trajectory_probability,
        sampling_probability,
    }
}

/// Deterministic counterpart to [`sample_transition`].
///
/// Every random choice of the turn comes from `seed`, so one seed and one
/// position always give one transition. Reproducible searches and tests use this
/// entry point; a search that seeds its whole run through
/// `simulator::scoped_sample_rng` uses [`sample_transition`] instead, because a
/// per-turn seed here would override the seed of the run.
#[allow(clippy::too_many_arguments)]
pub fn sample_transition_seeded(
    seed: u64,
    state: &MatchState,
    p1_cmd: &PlayerCommand,
    p2_cmd: &PlayerCommand,
    move_dex: &HashMap<PokemonMove, MoveData>,
    pokemon_dex: &HashMap<Species, PokemonData>,
    config: TransitionConfig,
) -> TransitionSample {
    let _guard = scoped_sample_rng(seed);
    sample_transition(state, p1_cmd, p2_cmd, move_dex, pokemon_dex, config)
}

/// Resolve `state` under both commands `samples` times, as one stratified batch.
///
/// The batch draws every random choice of a turn from a Latin hypercube. The
/// members cover each random dimension across the unit interval. This coverage
/// can reduce the variance of a mean over the successor distribution.
///
/// Each member keeps the law of one independent draw, so
/// [`TransitionSample::trajectory_probability`] and
/// [`TransitionSample::sampling_probability`] keep their meaning. Read
/// [`super::stratify`] for the construction and for the proof of that law.
///
/// One seed and one position always give one batch. A caller that wants its own
/// loop builds a [`StratifiedPlan`] and installs each member itself.
#[allow(clippy::too_many_arguments)]
pub fn sample_transition_batch(
    seed: u64,
    samples: usize,
    state: &MatchState,
    p1_cmd: &PlayerCommand,
    p2_cmd: &PlayerCommand,
    move_dex: &HashMap<PokemonMove, MoveData>,
    pokemon_dex: &HashMap<Species, PokemonData>,
    config: TransitionConfig,
) -> Vec<TransitionSample> {
    // One guard covers the whole batch: the jitter of every member and every
    // fallback draw comes from this one stream.
    let _guard = scoped_sample_rng(seed);
    let plan = StratifiedPlan::new(samples, seed);
    (0..samples)
        .map(|index| {
            let _stream = plan.install(index);
            sample_transition(state, p1_cmd, p2_cmd, move_dex, pokemon_dex, config)
        })
        .collect()
}
