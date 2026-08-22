//! Solves each drawn world by itself, and averages the strategies.
//!
//! This is perfect-information Monte Carlo, or PIMC. The literature also calls
//! it determinized search, and bridge calls it double-dummy sampling. It is the
//! labeled baseline of the fog-of-war solver, and it is not an equilibrium
//! method.
//!
//! # One search
//!
//! 1. Draw one world for each particle of the belief.
//! 2. Solve each world with [`solve`](super::solve), which is exact for its
//!    depth.
//! 3. Add each world strategy to a running mixture, at the weight of its
//!    particle.
//! 4. Normalize the mixture, and report it as the strategy of the position.
//!
//! The value is the weighted mean of the world values.
//!
//! # Strategy fusion
//!
//! Each world solve reads the hidden data of that world. The search therefore
//! plays a different action in each world, and no player can do that. The
//! mixture claims knowledge that the position does not supply.
//!
//! This defect is strategy fusion, and it makes the reported value too good for
//! the player that the fog protects. Every answer carries
//! [`SolveWarning::StrategyFusion`] for that reason.
//!
//! [`ismcts`](super::ismcts) and [`mccfr`](super::mccfr) do not have the defect.
//! They key their nodes by what one player saw, so two worlds that a player
//! cannot tell apart share one decision.
//!
//! [`preview::solve_open_list_preview`](super::preview::solve_open_list_preview)
//! also avoids it. That search averages the payoff matrix and solves one time,
//! so one strategy covers every world.
//!
//! # Whose strategy is playable
//!
//! Every world holds the same data for the observer of the belief, because the
//! determinizer copies that side. The observer therefore has one action set, and
//! its mixture is one distribution over its own legal actions.
//!
//! The other player holds a different private build in each world. Its rows mix
//! those builds, and they can name a move that only one world gave it. That
//! report is an observer-side summary, not a strategy that the other player
//! could play.
//!
//! # The simulation budget
//!
//! One job budget covers the whole search. The worlds run in order, so the first
//! world would spend the whole budget and the answer would rest on one world.
//!
//! Each world therefore gets an equal share through
//! [`CancelFlag::child_with_budget`]. A world that does not spend its share
//! leaves the rest to the job, so a later world is never starved by the split.
//!
//! # Reproducibility
//!
//! One seed drives the world draws and each world solve. The same seed and the
//! same configuration give the same result. The search reads each particle one
//! time, in order, so it draws no random number of its own.

use std::collections::HashMap;
use std::fmt;
use std::time::Instant;

use crate::data::pokemon_move::PokemonMove;
use crate::data::species::Species;
use crate::information::determinize::{DeterminizeConfig, DeterminizeWarning};
use crate::information::unknowns::UnknownBattleState;
use crate::meta::MetaDex;
use crate::state::battle::{BattleCommand, MatchState};
use crate::state::dex_data::{MoveData, PokemonData};

use super::actions::JointActions;
use super::belief::{BeliefError, JointActionKey, ParticleBelief};
use super::mcts::{MctsSamplingError, RunningStats};
use super::search::strategy_of;
use super::{
    CancelFlag, JointActionProb, RootProgress, RootRound, SolveConfig, SolveError, SolveResult,
    SolveStats, SolveWarning, cancel_requested, solve_seeded_cancellable,
};

/// Everything the search needs beyond the belief itself.
#[derive(Debug, Clone, Copy)]
pub struct PimcConfig {
    /// The perfect-information search of one world.
    ///
    /// Keep [`SolveConfig::iterative_deepening`] on. A world that runs out of
    /// its share then returns its last complete depth rather than a statically
    /// scored pass.
    pub solve: SolveConfig,
    /// Solves each world by refinement from this base depth.
    ///
    /// `None` gives each world a complete search to `solve.depth`.
    ///
    /// A doubles world cannot finish a depth-2 search in the share of a job
    /// budget, so it returns its depth-1 pass. Refinement spends the same share
    /// on the cells that decide the answer and reaches depth 2 on those.
    /// `super::refine_seeded_cancellable` holds the rule and the defect that it
    /// reports.
    pub refine_base_depth: Option<u8>,
    /// Worlds that [`search_belief`] draws from a belief.
    pub particles: usize,
    /// Resample the belief when the effective sample size falls below this share
    /// of the particle count, from 0 through 1.
    ///
    /// Zero never resamples. The search resamples one time, before the first
    /// world.
    pub resample_threshold: f64,
}

impl Default for PimcConfig {
    fn default() -> Self {
        PimcConfig {
            solve: SolveConfig {
                iterative_deepening: true,
                ..SolveConfig::default()
            },
            refine_base_depth: None,
            particles: 8,
            resample_threshold: 0.5,
        }
    }
}

/// An averaged strategy pair for one fog-of-war position.
#[derive(Debug, Clone)]
pub struct PimcResult {
    /// The estimated game value: P1's win probability to the configured depth.
    /// Identical to `p1_win_odds`.
    pub value: f64,
    /// P1's odds of winning, in `[0, 1]`.
    pub p1_win_odds: f64,
    /// P2's odds of winning. The game is zero-sum, so this is `1 - p1_win_odds`.
    pub p2_win_odds: f64,
    /// P1's mixed strategy, over the union of the world action sets.
    pub p1_strategy: Vec<JointActionProb>,
    /// P2's mixed strategy, likewise.
    pub p2_strategy: Vec<JointActionProb>,
    /// The spread of the world values.
    ///
    /// `iterations` holds the worlds that the search solved. The error covers
    /// the draw of the worlds. It does not cover the depth horizon, and it does
    /// not cover the belief itself.
    pub sampling: MctsSamplingError,
    /// The effective sample size of the belief that the search used.
    pub effective_sample_size: f64,
    /// The particle count of that belief.
    pub particles: usize,
    /// The worlds that the search solved.
    /// A stop limit can leave this below `particles`.
    pub worlds_solved: usize,
    /// The lowest depth that any solved world finished.
    pub depth_reached: u8,
    /// The summed cost of every world solve.
    pub stats: SolveStats,
    /// Why the answer is approximate. Always holds
    /// [`SolveWarning::StrategyFusion`].
    pub warnings: Vec<SolveWarning>,
    /// What the determinizer reported while it drew the worlds.
    /// Each distinct warning appears one time.
    pub draw_warnings: Vec<DeterminizeWarning>,
}

/// Why a fog-of-war position has no answer.
#[derive(Debug, Clone, PartialEq)]
pub enum PimcError {
    /// A particle is a preview position or a finished battle.
    Position(SolveError),
    /// The belief could not supply the worlds.
    Belief(BeliefError),
}

impl From<SolveError> for PimcError {
    fn from(error: SolveError) -> Self {
        PimcError::Position(error)
    }
}

impl From<BeliefError> for PimcError {
    fn from(error: BeliefError) -> Self {
        PimcError::Belief(error)
    }
}

impl fmt::Display for PimcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PimcError::Position(error) => write!(f, "{error}"),
            PimcError::Belief(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for PimcError {}

/// Draws worlds from `belief` and solves each one.
///
/// The determinizer copies the side of [`DeterminizeConfig::observer`] and
/// samples the other side, so only the hidden side changes between worlds.
///
/// The same `seed` covers the draws and each solve, so the same inputs always
/// give the same result.
pub fn search_belief(
    seed: u64,
    belief: &UnknownBattleState,
    meta_dex: &MetaDex,
    pokemon_dex: &HashMap<Species, PokemonData>,
    move_dex: &HashMap<PokemonMove, MoveData>,
    config: &PimcConfig,
    determinize: &DeterminizeConfig,
) -> Result<PimcResult, PimcError> {
    search_belief_cancellable(
        seed,
        belief,
        meta_dex,
        pokemon_dex,
        move_dex,
        config,
        determinize,
        None,
        None,
    )
}

/// [`search_belief`], with a progress hook and a cooperative stop signal.
///
/// The draw runs first, and it reads no flag. A partial particle set is not a
/// belief, so the search cannot answer from one.
/// [`search_progress_cancellable`] then reads the flag between worlds.
///
/// `None` for both gives the behavior of [`search_belief`].
#[allow(clippy::too_many_arguments)]
pub fn search_belief_cancellable(
    seed: u64,
    belief: &UnknownBattleState,
    meta_dex: &MetaDex,
    pokemon_dex: &HashMap<Species, PokemonData>,
    move_dex: &HashMap<PokemonMove, MoveData>,
    config: &PimcConfig,
    determinize: &DeterminizeConfig,
    progress: Option<RootProgress<'_>>,
    cancel: Option<&CancelFlag>,
) -> Result<PimcResult, PimcError> {
    let particles = ParticleBelief::from_belief(
        seed,
        belief,
        meta_dex,
        pokemon_dex,
        move_dex,
        config.particles,
        determinize,
    )?;
    search_progress_cancellable(
        seed,
        &particles,
        pokemon_dex,
        move_dex,
        config,
        progress,
        cancel,
    )
}

/// Solves a particle set that the caller already holds.
///
/// The search resamples the set one time when its effective sample size falls
/// below [`PimcConfig::resample_threshold`]. It leaves the caller's set alone.
///
/// Returns an error when a particle is a preview position or a finished battle,
/// as [`solve`](super::solve) does.
pub fn search(
    seed: u64,
    belief: &ParticleBelief,
    pokemon_dex: &HashMap<Species, PokemonData>,
    move_dex: &HashMap<PokemonMove, MoveData>,
    config: &PimcConfig,
) -> Result<PimcResult, PimcError> {
    search_progress_cancellable(seed, belief, pokemon_dex, move_dex, config, None, None)
}

/// [`search`], with a progress hook and a cooperative stop signal.
///
/// The search reads `cancel` before each world. One world is the unit of work:
/// it solves one position and it adds one strategy pair to the mixture. A set
/// flag ends the loop, and the result then holds the worlds that finished.
///
/// The search always solves world 1, so the answer never rests on an empty
/// mixture.
///
/// `progress` fires after each world with the running mixture, so a caller can
/// publish an answer while the search goes on. The hook runs on the thread of
/// the search, so keep the call short.
///
/// `None` for both gives the behavior of [`search`].
pub fn search_progress_cancellable(
    seed: u64,
    belief: &ParticleBelief,
    pokemon_dex: &HashMap<Species, PokemonData>,
    move_dex: &HashMap<PokemonMove, MoveData>,
    config: &PimcConfig,
    progress: Option<RootProgress<'_>>,
    cancel: Option<&CancelFlag>,
) -> Result<PimcResult, PimcError> {
    check_particles(belief)?;

    let started = Instant::now();
    // The search owns its copy, so a resample never changes the caller's set.
    let mut worlds = belief.clone();
    worlds.resample_if_degenerate(config.resample_threshold);

    let mut quota = first_world_quota(cancel, worlds.len());
    let world_config = SolveConfig {
        // The mixture needs the whole strategy of each world, so no world takes
        // the shortcut of an early stop at the root.
        deadline: None,
        ..config.solve
    };

    let mut mix = [StrategyMix::default(), StrategyMix::default()];
    let mut values = RunningStats::default();
    let mut weighted_value = 0.0;
    let mut weight_solved = 0.0;
    let mut stats = SolveStats::default();
    let mut warnings: Vec<SolveWarning> = Vec::new();
    let mut depth_reached = u8::MAX;
    let mut worlds_solved = 0;
    let mut cancelled = false;
    let mut quota_hit = false;

    for (index, particle) in worlds.particles().iter().enumerate() {
        // World 1 always runs. Every later world reads the flag first, so the
        // answer covers whole worlds and nothing else.
        if index > 0
            && (cancel_requested(cancel) || cancel.is_some_and(CancelFlag::simulation_budget_hit))
        {
            cancelled = cancel_requested(cancel);
            break;
        }

        let share = quota.map(|budget| cancel.expect("a quota needs a flag").child_with_budget(budget));
        let world_seed = seed.wrapping_add(index as u64);
        let world_cancel = share.as_ref().or(cancel);
        let result = match config.refine_base_depth {
            Some(base_depth) => {
                super::refine_seeded_cancellable(
                    world_seed,
                    &particle.state,
                    pokemon_dex,
                    move_dex,
                    &world_config,
                    base_depth,
                    world_cancel,
                )?
                .0
            }
            None => solve_seeded_cancellable(
                world_seed,
                &particle.state,
                pokemon_dex,
                move_dex,
                &world_config,
                world_cancel,
            )?,
        };
        quota_hit |= share.is_some_and(|share| share.simulation_budget_hit());
        // The first world measures what a world of this belief costs. Size the
        // rest from that rather than from an even split.
        if index == 0 {
            quota = later_world_quota(cancel, worlds.len(), result.stats.turns_simulated);
        }

        mix[0].add(&result.p1_strategy, particle.weight);
        mix[1].add(&result.p2_strategy, particle.weight);
        values.push(result.value);
        weighted_value += result.value * particle.weight;
        weight_solved += particle.weight;
        depth_reached = depth_reached.min(result.depth_reached);
        add_stats(&mut stats, &result.stats);
        keep_world_warnings(&mut warnings, &result);
        worlds_solved += 1;

        if let Some(hook) = progress {
            hook(RootRound {
                depth: result.depth_reached,
                value: mean_value(weighted_value, weight_solved),
                p1_strategy: mix[0].strategy(),
                p2_strategy: mix[1].strategy(),
                stats: stats.clone(),
            });
        }
    }

    // Every world of a `from_belief` set carries the same weight, so this is the
    // plain mean there. A caller-supplied set can weight its worlds instead.
    let value = mean_value(weighted_value, weight_solved);
    stats.elapsed = started.elapsed();

    warnings.push(SolveWarning::StrategyFusion {
        worlds: worlds_solved,
    });
    if cancelled {
        warnings.push(SolveWarning::Cancelled);
    }
    // One job budget covers the run, so the answer names that budget rather than
    // the share of one world.
    if (quota_hit || cancel.is_some_and(CancelFlag::simulation_budget_hit))
        && let Some(budget) = cancel.and_then(CancelFlag::simulation_turn_budget)
    {
        warnings.push(SolveWarning::SimulationTurnBudgetExhausted { budget });
    }

    Ok(PimcResult {
        value,
        p1_win_odds: value,
        p2_win_odds: 1.0 - value,
        p1_strategy: mix[0].strategy(),
        p2_strategy: mix[1].strategy(),
        sampling: MctsSamplingError {
            iterations: values.count,
            mean: value,
            // Each world draws its own hidden data, so the independent-sample
            // formula holds over the worlds. The error counts each world one
            // time, which is the draw that `from_belief` makes.
            standard_error: values.standard_error(),
        },
        effective_sample_size: worlds.effective_sample_size(),
        particles: worlds.len(),
        worlds_solved,
        depth_reached: depth_reached.min(config.solve.depth),
        stats,
        warnings,
        draw_warnings: worlds.warnings().to_vec(),
    })
}

/// The weighted mean of the world values.
///
/// A caller-supplied set can weight a world at zero, and
/// [`PimcConfig::resample_threshold`] of zero never normalizes such a set. The
/// division would then be `0.0 / 0.0`, and the NaN would reach the reported win
/// odds and every displayed percentage. An even value is the only neutral
/// answer for a set that carries no weight at all.
fn mean_value(weighted_value: f64, weight_solved: f64) -> f64 {
    if weight_solved > 0.0 {
        weighted_value / weight_solved
    } else {
        0.5
    }
}

/// Refuses a set that holds a position with no decision.
///
/// A finished world or a preview world would make the mixture an average over
/// fewer worlds than the count claims, so the whole set fails.
/// [`ismcts::search_cancellable`](super::ismcts::search_cancellable) applies the
/// same rule.
fn check_particles(belief: &ParticleBelief) -> Result<(), PimcError> {
    if belief.is_empty() {
        return Err(BeliefError::NoParticles.into());
    }
    for particle in belief.particles() {
        match &particle.state {
            MatchState::TeamPreviewState(_) => {
                return Err(SolveError::TeamPreviewUnsupported.into());
            }
            MatchState::GameOverState { winner, .. } => {
                return Err(SolveError::GameAlreadyOver { winner: *winner }.into());
            }
            MatchState::BattleState(_) => {}
        }
    }
    Ok(())
}

/// The worlds that the job budget must always be able to finish.
///
/// An even split gives every world the same share. That share starves every
/// world at once when one solve costs more than it, and the answer is then a
/// mixture of positions that were scored statically rather than searched. Two
/// searched worlds say more than thirty-two unsearched ones, so the first world
/// may take this fraction of the job budget.
const GUARANTEED_WORLDS: u64 = 2;

/// The share of the job budget that the first world may spend.
///
/// `None` means that the caller set no budget, so each world reads the flag of
/// the job itself.
///
/// The first world has no measurement to size itself from, so it takes the
/// larger of the even share and the [`GUARANTEED_WORLDS`] share.
fn first_world_quota(cancel: Option<&CancelFlag>, worlds: usize) -> Option<u64> {
    let budget = cancel.and_then(CancelFlag::simulation_turn_budget)?;
    let even = budget / worlds.max(1) as u64;
    Some(even.max(budget / GUARANTEED_WORLDS).max(1))
}

/// The share of the job budget that a later world may spend.
///
/// `measured` is what the first world cost. A world of the same belief solves a
/// position of the same shape, so that cost predicts the rest far better than an
/// even split does. The margin covers the variation between two draws.
///
/// The job budget still bounds the run. A quota above what the job has left
/// simply lets the parent flag stop the world, and the loop then reports the
/// worlds that finished.
fn later_world_quota(
    cancel: Option<&CancelFlag>,
    worlds: usize,
    measured: u64,
) -> Option<u64> {
    let budget = cancel.and_then(CancelFlag::simulation_turn_budget)?;
    let even = budget / worlds.max(1) as u64;
    Some(even.max(measured.saturating_add(measured / 4)).max(1))
}

/// Adds the cost of one world to the running total.
///
/// `elapsed` stays out. The worlds run in sequence, so the caller measures the
/// whole search one time.
fn add_stats(total: &mut SolveStats, world: &SolveStats) {
    total.nodes_expanded += world.nodes_expanded;
    total.turns_simulated += world.turns_simulated;
    total.matrix_cells_evaluated += world.matrix_cells_evaluated;
    total.matrix_cells_total += world.matrix_cells_total;
    total.lps_solved += world.lps_solved;
    total.ab_cutoffs += world.ab_cutoffs;
    total.tt_hits += world.tt_hits;
    total.turn_cache_hits += world.turn_cache_hits;
}

/// Keeps the distinct warnings of one world.
///
/// The share of a world is not the budget of the job, so its
/// [`SolveWarning::SimulationTurnBudgetExhausted`] would name a number that the
/// caller never sent. The search reports the job budget itself instead.
fn keep_world_warnings(warnings: &mut Vec<SolveWarning>, result: &SolveResult) {
    for warning in &result.warnings {
        if matches!(warning, SolveWarning::SimulationTurnBudgetExhausted { .. }) {
            continue;
        }
        if !warnings.contains(warning) {
            warnings.push(warning.clone());
        }
    }
}

/// One player's running mixture over the worlds.
///
/// Two worlds can offer different actions, so the mixture keys its rows by
/// [`JointActionKey`]. A row that only one world offered keeps the weight of
/// that world alone.
#[derive(Default)]
struct StrategyMix {
    index_of: HashMap<JointActionKey, usize>,
    actions: Vec<Vec<BattleCommand>>,
    sums: Vec<f64>,
}

impl StrategyMix {
    /// Adds one world strategy at the weight of its particle.
    fn add(&mut self, strategy: &[JointActionProb], weight: f64) {
        for row in strategy {
            let key = JointActionKey::new(&row.commands);
            let index = match self.index_of.get(&key) {
                Some(&index) => index,
                None => {
                    let index = self.actions.len();
                    self.index_of.insert(key, index);
                    self.actions.push(row.commands.clone());
                    self.sums.push(0.0);
                    index
                }
            };
            self.sums[index] += row.probability * weight;
        }
    }

    /// The normalized mixture, in descending probability order.
    fn strategy(&self) -> Vec<JointActionProb> {
        let total: f64 = self.sums.iter().sum();
        let mut probabilities = self.sums.clone();
        if total > 0.0 {
            for probability in &mut probabilities {
                *probability /= total;
            }
        }
        let joint = JointActions {
            total: self.actions.len(),
            actions: self.actions.clone(),
        };
        // A zero floor, not `EPS`: a world can hold an action at a probability
        // that the mixture then divides again.
        strategy_of(&joint, &probabilities, 0.0)
    }
}

#[cfg(test)]
mod mix_tests {
    use super::*;
    use crate::state::battle::SwitchCommand;

    fn row(party_index: usize, probability: f64) -> JointActionProb {
        JointActionProb {
            commands: vec![BattleCommand::Switch(SwitchCommand { party_index })],
            probability,
        }
    }

    /// Two worlds that play the same action must add their weights.
    #[test]
    fn a_mixture_adds_the_weight_of_each_world() {
        let mut mix = StrategyMix::default();
        mix.add(&[row(0, 1.0)], 3.0);
        mix.add(&[row(0, 0.5), row(1, 0.5)], 1.0);

        let strategy = mix.strategy();
        assert_eq!(strategy.len(), 2);
        // (3 * 1 + 1 * 0.5) / 4 and (1 * 0.5) / 4.
        assert!((strategy[0].probability - 0.875).abs() < 1e-12, "{strategy:?}");
        assert!((strategy[1].probability - 0.125).abs() < 1e-12, "{strategy:?}");
    }

    /// An action that one world alone offers keeps that world's weight.
    #[test]
    fn a_mixture_keeps_an_action_of_one_world() {
        let mut mix = StrategyMix::default();
        mix.add(&[row(0, 1.0)], 1.0);
        mix.add(&[row(2, 1.0)], 1.0);

        let strategy = mix.strategy();
        assert_eq!(strategy.len(), 2);
        let total: f64 = strategy.iter().map(|row| row.probability).sum();
        assert!((total - 1.0).abs() < 1e-12, "{strategy:?}");
    }
}
