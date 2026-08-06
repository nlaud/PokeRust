//! Solves a fog-of-war position with outcome-sampling MCCFR.
//!
//! [`ismcts::search`](super::ismcts::search) is the fast heuristic baseline. Both
//! of its trees learn against the same sampled opponent, so the pair converges to
//! a self-consistent strategy rather than to an equilibrium.
//!
//! This search is the first equilibrium baseline. It runs counterfactual regret
//! minimization over sampled paths, and it returns an average strategy for each
//! player. It also returns the counterfactual value of each public belief at the
//! depth limit.
//!
//! # The game encoding
//!
//! One turn holds two simultaneous commands. The search treats the turn as two
//! decisions in one node. Neither information set holds the command of the other
//! player, because [`ObservationKey`] chains only the commands of its own player.
//!
//! A node key holds the [`ObservationKey`] of one player, the search depth, and
//! the forced-chain counter. Read [`infoset`](super::infoset) for the node and
//! its action registry.
//!
//! # One iteration
//!
//! Each iteration picks one traverser. Iteration `n` picks P1 when `n` is even,
//! and P2 when `n` is odd. The iteration then draws one world from the belief and
//! samples one path from that world to the horizon.
//!
//! The search carries three reach values down the path:
//!
//! 1. `player_reach` holds the on-policy reach of each player.
//! 2. `chance_reach` holds the trajectory probability of the resolved turns.
//! 3. `sample_reach` holds the probability that the sampler produced the path.
//!
//! The traverser plays a regret-matching strategy mixed with uniform play. The
//! other player plays its regret-matching strategy without that mix. The mix
//! bounds the selection probability of the traverser from below, and the learner
//! update divides by that probability.
//!
//! [`sample_transition`] reports a trajectory probability and a sampling
//! probability. The two differ when a chokepoint renormalizes a branch group.
//! `chance_reach` reads the first figure, and `sample_reach` reads the second.
//! Their ratio removes the bias of the sampler.
//!
//! # The update
//!
//! The recursion returns the leaf utility over `sample_reach`, and the reach of
//! the sampled suffix. The suffix reach holds the strategy of both players and
//! the trajectory probability of each turn below the node. It is not the reach of
//! the traverser alone. The reach of the other player below the node cancels the
//! same factor inside `sample_reach`, and the sampled counterfactual value needs
//! that cancellation.
//!
//! At each node of the traverser, the search adds one regret to each action. The
//! regret of the sampled action is `weight * tail * (1 - probability)`. The
//! regret of every other action is `-weight * tail * probability`. `weight` is
//! the returned utility times the opponent and chance reach at that node. `tail`
//! starts below the node decision. It holds the other command and the turn
//! trajectory probability.
//!
//! At the same node, the other player adds its current strategy to its average.
//! The added weight is that player's reach over `sample_reach`. Alternation gives
//! both players an average strategy.
//!
//! # The horizon
//!
//! A leaf records one entry for the traverser. [`HorizonKey`] holds the public
//! observation key, the information-set key of the traverser, and the player.
//! `mask_events_public` builds the public stream, and the key chains it.
//!
//! Each entry holds a counterfactual value sum, a reach sum, and a visit count.
//! Two hidden worlds share a key only when the traverser cannot distinguish
//! them. A later public-belief solve reads the counterfactual value as input.
//!
//! # The leaf oracle
//!
//! [`search_with_leaves`] takes a [`HorizonLeaves`] map. At the depth limit, the
//! search reads the value for the public stream and the traverser's information
//! set. It reads the configured evaluator when the map holds no such key.
//!
//! [`MccfrResult::leaf_lookups`] counts each hit and each miss. A terminal leaf
//! keeps its exact value, because the oracle covers the depth limit alone.
//!
//! # The continuation belief
//!
//! [`MccfrConfig::horizon_worlds`] is the number of worlds that the search keeps
//! for each public belief at the depth limit.
//! [`MccfrResult::horizon_beliefs`] holds those worlds. Each world carries the
//! importance weight of its path.
//!
//! The set is a sample of the continuation belief, not a complete posterior. The
//! field defaults to zero, so a normal search stores nothing.
//!
//! # Continual solving
//!
//! [`continual_solve`] runs three passes:
//!
//! 1. It solves the root subgame with no oracle. This pass finds the public
//!    beliefs at the depth limit.
//! 2. It solves each of those public beliefs as a continuation subgame.
//! 3. It solves the root subgame again, with the values of pass 2 as the oracle.
//!
//! Pass 2 runs in the sorted order of the raw public key. One seed therefore
//! gives one result.
//!
//! # The reported value
//!
//! Each iteration returns P1's leaf utility. The search corrects that utility for
//! the traverser's uniform mix and for transition sampling. The corrected mean
//! estimates the value of the current strategy pair. The search clamps that mean
//! to `[0, 1]`, because a utility is a win probability.
//!
//! [`MccfrResult::sampling`] reports the iteration count and mean. Its standard
//! error is `None` because each iteration changes the strategy of later samples.
//! [`MccfrResult::effective_sample_size`] reports that second limit beside it.
//!
//! # Reproducibility
//!
//! One seed drives the particle draws, the action selection, and the engine. The
//! same seed and the same configuration give the same result.

use std::collections::HashMap;
use std::fmt;
use std::time::Instant;

use crate::data::pokemon_move::PokemonMove;
use crate::data::species::Species;
use crate::information::determinize::{DeterminizeConfig, DeterminizeWarning};
use crate::information::unknowns::UnknownBattleState;
use crate::meta::MetaDex;
use crate::simulator::generative::{TransitionConfig, sample_transition};
use crate::simulator::{scoped_sample_rng, with_sample_rng};
use crate::state::battle::{BattleState, MatchState, Player, PlayerCommand};
use crate::state::dex_data::{MoveData, PokemonData};

use super::actions::{self, JointActions, Phase};
use super::belief::{BeliefError, ObservationKey, Particle, ParticleBelief};
use super::eval::EvalContext;
use super::infoset::{InfoKey, InfoNode, root_strategy};
use super::mcts::{
    LOSS, MIN_EXPLORATION, MctsConfig, MctsSamplingError, MctsStats, RunningStats, SelectionPolicy,
    WIN, draw_index, terminal_value,
};
use super::{JointActionProb, SolveError, SolveWarning};

use rand::Rng;

/// Everything the equilibrium search needs beyond the belief itself.
///
/// # Which shared knobs the search reads
///
/// The search reads these fields of [`MctsConfig`]: `iterations`, `depth`,
/// `exploration`, `damage_rolls`, `consider_crit`, `eval`,
/// `max_actions_per_player`, and `max_forced_chain`.
///
/// [`MccfrConfig::default`] raises `exploration` to [`DEFAULT_EXPLORATION`]. Read
/// that constant for the reason.
///
/// It ignores every other field. Counterfactual regret minimization needs regret
/// matching, so `policy` cannot select Exp3. The transition must provide event
/// streams, so `transition` cannot select the enumerated mode. The dominance test
/// reads hidden target data, so an information-set search cannot use
/// `prune_dominated_actions` safely.
#[derive(Debug, Clone, Copy)]
pub struct MccfrConfig {
    /// The knobs that this search shares with [`mcts::search`](super::mcts::search).
    pub search: MctsConfig,
    /// Worlds that [`search_belief`] draws from a belief.
    pub particles: usize,
    /// Resample the belief when the effective sample size falls below this share
    /// of the particle count, from 0 through 1.
    ///
    /// Zero never resamples. The search resamples one time, before the first
    /// iteration.
    pub resample_threshold: f64,
    /// Worlds that the search keeps for each public belief at the depth limit.
    ///
    /// Zero keeps nothing, and [`MccfrResult::horizon_beliefs`] is then empty. A
    /// continual solve needs a positive count, because it solves each of those
    /// beliefs as a continuation subgame.
    pub horizon_worlds: usize,
}

/// The uniform mix that the traverser samples with.
///
/// The regret update divides by the selection probability of the played action,
/// so a small mix gives a rare action a large estimate and a large variance. This
/// rate is the value that Lanctot et al. use for the outcome-sampling estimator.
/// It is much larger than the rate that the MCTS learners need.
pub const DEFAULT_EXPLORATION: f64 = 0.6;

impl Default for MccfrConfig {
    fn default() -> Self {
        MccfrConfig {
            search: MctsConfig {
                iterations: 2_000,
                exploration: DEFAULT_EXPLORATION,
                ..MctsConfig::default()
            },
            particles: 16,
            resample_threshold: 0.5,
            horizon_worlds: 0,
        }
    }
}

/// One public belief and one information set at the depth limit.
///
/// Two hidden worlds that produced one public stream share one key when the
/// traverser also cannot tell them apart. A public-belief solve reads the map as
/// its leaf input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HorizonKey {
    /// The chained public event stream of the path.
    pub public: ObservationKey,
    /// What the player of this entry saw on the same path.
    pub infoset: ObservationKey,
    /// The player that this entry belongs to.
    pub player: Player,
}

/// The counterfactual value of one [`HorizonKey`].
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct HorizonValue {
    /// The sum of the counterfactual reach times the utility of each visit.
    pub value_sum: f64,
    /// The sum of the counterfactual reach of each visit.
    pub reach_sum: f64,
    /// Leaves that reached this key.
    pub visits: u64,
}

impl HorizonValue {
    /// `value_sum` over `reach_sum`, as the utility of the player of the key.
    ///
    /// A key with no reach returns `None`. Only a floating-point underflow of
    /// every visit can produce that.
    pub fn counterfactual_value(&self) -> Option<f64> {
        (self.reach_sum > 0.0).then(|| self.value_sum / self.reach_sum)
    }
}

/// One continuation value for each information set at the depth limit.
///
/// The value is the win probability of [`HorizonKey::player`].
///
/// [`search_with_leaves`] converts a P2 value to a P1 value before the search
/// returns it from a leaf.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HorizonLeaves(HashMap<HorizonKey, f64>);

impl HorizonLeaves {
    /// An empty map. Every lookup then falls back to the leaf evaluator.
    pub fn new() -> Self {
        HorizonLeaves(HashMap::new())
    }

    /// Set the value of one information set, and return the earlier value.
    ///
    /// `player_value` is the key player's win probability. The search clamps it
    /// to `[0, 1]`.
    pub fn insert(&mut self, key: HorizonKey, player_value: f64) -> Option<f64> {
        self.0.insert(key, player_value)
    }

    /// The value of one information set, or `None` when the map has no entry.
    pub fn get(&self, key: HorizonKey) -> Option<f64> {
        self.0.get(&key).copied()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// How often the depth limit read a supplied continuation value.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LeafLookups {
    /// Depth-limit leaves that found their public belief in the oracle.
    pub hits: u64,
    /// Depth-limit leaves that fell back to the configured leaf evaluator.
    pub misses: u64,
}

/// An average strategy pair for one fog-of-war position.
#[derive(Debug, Clone)]
pub struct MccfrResult {
    /// The estimated value of the strategy pair: P1's win probability to the
    /// configured depth. Identical to `p1_win_odds`.
    pub value: f64,
    /// P1's odds of winning, in `[0, 1]`.
    pub p1_win_odds: f64,
    /// P2's odds of winning. The game is zero-sum, so this is `1 - p1_win_odds`.
    pub p2_win_odds: f64,
    /// P1's average strategy at the root, over the union of the world actions.
    pub p1_strategy: Vec<JointActionProb>,
    /// P2's average strategy at the root, likewise.
    pub p2_strategy: Vec<JointActionProb>,
    /// The iteration count and mean for `value`.
    ///
    /// `standard_error` is `None` because the iterations adapt the strategy.
    pub sampling: MctsSamplingError,
    /// The counterfactual value of each public belief at the horizon.
    pub horizon: HashMap<HorizonKey, HorizonValue>,
    /// The worlds that each public belief at the horizon reached.
    ///
    /// [`MccfrConfig::horizon_worlds`] bounds the world count of one entry. A
    /// count of zero leaves this map empty.
    ///
    /// This field exposes the states and weights for inspection. It does not
    /// expose the private histories that [`continual_solve`] retains.
    pub horizon_beliefs: HashMap<ObservationKey, ParticleBelief>,
    /// How often the depth limit read the supplied oracle.
    ///
    /// A search with no oracle reports zero hits and one miss for each
    /// depth-limit leaf.
    pub leaf_lookups: LeafLookups,
    /// The effective sample size of the belief that the search used.
    pub effective_sample_size: f64,
    /// The particle count of that belief.
    pub particles: usize,
    pub stats: MctsStats,
    /// Why the answer is approximate beyond the belief sampling limit.
    pub warnings: Vec<SolveWarning>,
    /// What the determinizer reported while it drew the worlds.
    /// Each distinct warning appears one time.
    pub draw_warnings: Vec<DeterminizeWarning>,
    /// The histories of the sampled horizon worlds.
    ///
    /// A continuation solve uses these histories to keep private information
    /// sets separate. The public particle view above does not expose them.
    horizon_roots: HashMap<ObservationKey, Vec<RootWorld>>,
    /// The counterfactual root values of a continuation search.
    root_values: HashMap<HorizonKey, HorizonValue>,
}

/// One sampled world and the information-set histories that reached it.
#[derive(Debug, Clone)]
struct RootWorld {
    state: MatchState,
    histories: [ObservationKey; 2],
    weight: f64,
}

/// A weighted search root that keeps each world's private history.
#[derive(Debug, Clone)]
struct RootBelief {
    worlds: Vec<RootWorld>,
}

impl RootBelief {
    fn from_particles(belief: &ParticleBelief) -> Result<Self, BeliefError> {
        if belief.is_empty() {
            return Err(BeliefError::NoParticles);
        }
        let worlds = belief
            .particles()
            .iter()
            .map(|particle| {
                let histories = match &particle.state {
                    MatchState::BattleState(battle) => [
                        ObservationKey::for_player(battle, Player::P1),
                        ObservationKey::for_player(battle, Player::P2),
                    ],
                    _ => [ObservationKey::ROOT; 2],
                };
                RootWorld {
                    state: particle.state.clone(),
                    histories,
                    weight: particle.weight,
                }
            })
            .collect();
        let mut belief = RootBelief { worlds };
        belief.normalize();
        Ok(belief)
    }

    fn from_horizon(worlds: &[RootWorld]) -> Result<Self, BeliefError> {
        if worlds.is_empty() {
            return Err(BeliefError::NoParticles);
        }
        let mut belief = RootBelief {
            worlds: worlds.to_vec(),
        };
        belief.normalize();
        Ok(belief)
    }

    fn len(&self) -> usize {
        self.worlds.len()
    }

    fn normalize(&mut self) {
        for world in &mut self.worlds {
            if !world.weight.is_finite() || world.weight < 0.0 {
                world.weight = 0.0;
            }
        }
        let total: f64 = self.worlds.iter().map(|world| world.weight).sum();
        if !total.is_finite() || total <= 0.0 {
            let uniform = 1.0 / self.worlds.len() as f64;
            for world in &mut self.worlds {
                world.weight = uniform;
            }
            return;
        }
        for world in &mut self.worlds {
            world.weight /= total;
        }
    }

    fn effective_sample_size(&self) -> f64 {
        let squares: f64 = self
            .worlds
            .iter()
            .map(|world| world.weight * world.weight)
            .sum();
        if squares > 0.0 { 1.0 / squares } else { 0.0 }
    }

    fn resample_if_degenerate(&mut self, threshold: f64) {
        let floor = threshold.clamp(0.0, 1.0) * self.worlds.len() as f64;
        if self.worlds.is_empty() || self.effective_sample_size() >= floor {
            return;
        }
        let count = self.worlds.len();
        let step = 1.0 / count as f64;
        let start = with_sample_rng(|rng| rng.gen_range(0.0..1.0)) * step;
        let mut drawn = Vec::with_capacity(count);
        let mut source = 0usize;
        let mut cumulative = self.worlds[0].weight;
        for member in 0..count {
            let target = start + member as f64 * step;
            while cumulative < target && source + 1 < count {
                source += 1;
                cumulative += self.worlds[source].weight;
            }
            let mut world = self.worlds[source].clone();
            world.weight = step;
            drawn.push(world);
        }
        self.worlds = drawn;
    }

    fn draw(&self) -> Option<&RootWorld> {
        if self.worlds.is_empty() {
            return None;
        }
        let roll = with_sample_rng(|rng| rng.gen_range(0.0..1.0));
        let mut accumulated = 0.0;
        for world in &self.worlds {
            accumulated += world.weight;
            if roll < accumulated {
                return Some(world);
            }
        }
        self.worlds.last()
    }
}

/// Why a fog-of-war position has no answer.
#[derive(Debug, Clone, PartialEq)]
pub enum MccfrError {
    /// A particle is a preview position or a finished battle.
    Position(SolveError),
    /// The belief could not supply the worlds.
    Belief(BeliefError),
}

impl From<SolveError> for MccfrError {
    fn from(error: SolveError) -> Self {
        MccfrError::Position(error)
    }
}

impl From<BeliefError> for MccfrError {
    fn from(error: BeliefError) -> Self {
        MccfrError::Belief(error)
    }
}

impl fmt::Display for MccfrError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MccfrError::Position(error) => write!(f, "{error}"),
            MccfrError::Belief(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for MccfrError {}

/// Draws worlds from `belief` and searches them.
///
/// The determinizer copies the side of [`DeterminizeConfig::observer`] and
/// samples the other side, so only the hidden side changes between worlds.
///
/// The same `seed` covers the draws and the search, so the same inputs always
/// give the same result.
pub fn search_belief(
    seed: u64,
    belief: &UnknownBattleState,
    meta_dex: &MetaDex,
    pokemon_dex: &HashMap<Species, PokemonData>,
    move_dex: &HashMap<PokemonMove, MoveData>,
    config: &MccfrConfig,
    determinize: &DeterminizeConfig,
) -> Result<MccfrResult, MccfrError> {
    let particles = ParticleBelief::from_belief(
        seed,
        belief,
        meta_dex,
        pokemon_dex,
        move_dex,
        config.particles,
        determinize,
    )?;
    search(seed, &particles, pokemon_dex, move_dex, config)
}

/// Searches a particle set that the caller already holds.
///
/// The search resamples the set one time when its effective sample size falls
/// below [`MccfrConfig::resample_threshold`]. It leaves the caller's set alone.
///
/// Every depth-limit leaf reads the configured evaluator.
/// [`search_with_leaves`] reads a supplied continuation value instead.
///
/// Returns an error when a particle is a preview position or a finished battle,
/// as [`solve`](super::solve) does.
pub fn search(
    seed: u64,
    belief: &ParticleBelief,
    pokemon_dex: &HashMap<Species, PokemonData>,
    move_dex: &HashMap<PokemonMove, MoveData>,
    config: &MccfrConfig,
) -> Result<MccfrResult, MccfrError> {
    search_with_leaves(seed, belief, pokemon_dex, move_dex, config, None)
}

/// Searches a particle set against a supplied leaf oracle.
///
/// `leaves` holds one continuation value for each public belief at the depth
/// limit. A leaf whose public belief is missing reads the configured evaluator.
/// `None` gives the behavior of [`search`].
///
/// The oracle covers the depth limit alone. A terminal leaf keeps its exact
/// value.
pub fn search_with_leaves(
    seed: u64,
    belief: &ParticleBelief,
    pokemon_dex: &HashMap<Species, PokemonData>,
    move_dex: &HashMap<PokemonMove, MoveData>,
    config: &MccfrConfig,
    leaves: Option<&HorizonLeaves>,
) -> Result<MccfrResult, MccfrError> {
    search_root_belief(
        seed,
        RootBelief::from_particles(belief)?,
        pokemon_dex,
        move_dex,
        config,
        leaves,
        None,
        belief.warnings().to_vec(),
    )
}

/// Search worlds that already hold their information-set histories.
#[allow(clippy::too_many_arguments)]
fn search_root_belief(
    seed: u64,
    mut worlds: RootBelief,
    pokemon_dex: &HashMap<Species, PokemonData>,
    move_dex: &HashMap<PokemonMove, MoveData>,
    config: &MccfrConfig,
    leaves: Option<&HorizonLeaves>,
    root_public: Option<ObservationKey>,
    draw_warnings: Vec<DeterminizeWarning>,
) -> Result<MccfrResult, MccfrError> {
    for world in &worlds.worlds {
        match &world.state {
            MatchState::TeamPreviewState(_) => {
                return Err(SolveError::TeamPreviewUnsupported.into());
            }
            // A finished world holds no decision. One of them would make the
            // reported strategy an average over fewer worlds than the count
            // claims, so refuse the whole set.
            MatchState::GameOverState { winner, .. } => {
                return Err(SolveError::GameAlreadyOver { winner: *winner }.into());
            }
            MatchState::BattleState(_) => {}
        }
    }

    let started = Instant::now();
    let _guard = scoped_sample_rng(seed);

    // Depth 0 would score the root without a decision, and the strategy would
    // mean nothing. One turn is the minimum.
    let depth = config.search.depth.max(1);
    let iterations = config.search.iterations.max(1);

    // The search owns this set, so a resample does not change the caller's set.
    worlds.resample_if_degenerate(config.resample_threshold);

    let mut ctx = MccfrContext {
        pokemon_dex,
        move_dex,
        cfg: config,
        exploration: config.search.exploration.clamp(MIN_EXPLORATION, 1.0),
        leaves,
        trees: [HashMap::new(), HashMap::new()],
        root_keys: [Vec::new(), Vec::new()],
        horizon: HashMap::new(),
        horizon_worlds: HashMap::new(),
        root_values: HashMap::new(),
        leaf_lookups: LeafLookups::default(),
        stats: MctsStats::default(),
        action_truncations: [None, None],
    };

    let mut values = RunningStats::default();
    for iteration in 0..iterations {
        let root = worlds.draw().expect("the set is not empty").clone();
        let state = root.state;
        let histories = root.histories;
        for (keys, &history) in ctx.root_keys.iter_mut().zip(&histories) {
            let key = (history, depth, 0);
            if !keys.contains(&key) {
                keys.push(key);
            }
        }
        // Alternation gives both players an average strategy: a player builds
        // its average on the iterations that traverse for the other player.
        let walk = Walk {
            traverser: (iteration % 2) as usize,
            player_reach: [1.0, 1.0],
            chance_reach: 1.0,
            sample_reach: 1.0,
        };
        let descent = ctx.iterate(&state, depth, 0, histories, ObservationKey::ROOT, walk);
        if let Some(public) = root_public {
            ctx.record_root_value(public, histories, walk.traverser, &descent);
        }
        values.push(descent.p1_value * descent.importance_weight);
    }

    let p1_strategy = root_strategy(&ctx.trees[0], &ctx.root_keys[0]);
    let p2_strategy = root_strategy(&ctx.trees[1], &ctx.root_keys[1]);

    let mut warnings = Vec::new();
    for (player, truncation) in [
        (Player::P1, ctx.action_truncations[0]),
        (Player::P2, ctx.action_truncations[1]),
    ] {
        if let Some((kept, total)) = truncation {
            warnings.push(SolveWarning::ActionsTruncated {
                player,
                kept,
                total,
            });
        }
    }

    let value = values.mean().clamp(LOSS, WIN);
    let mut stats = ctx.stats;
    stats.iterations = values.count;
    stats.elapsed = started.elapsed();

    // Each list has at least one world, so the builder accepts each one.
    let horizon_beliefs = ctx
        .horizon_worlds
        .iter()
        .filter_map(|(&public, worlds)| {
            let particles = worlds
                .iter()
                .map(|world| Particle {
                    state: world.state.clone(),
                    weight: world.weight,
                })
                .collect();
            ParticleBelief::from_particles(particles)
                .ok()
                .map(|set| (public, set))
        })
        .collect();

    Ok(MccfrResult {
        value,
        p1_win_odds: value,
        p2_win_odds: WIN - value,
        p1_strategy,
        p2_strategy,
        sampling: MctsSamplingError {
            iterations: values.count,
            mean: value,
            standard_error: None,
        },
        horizon: ctx.horizon,
        horizon_beliefs,
        leaf_lookups: ctx.leaf_lookups,
        effective_sample_size: worlds.effective_sample_size(),
        particles: worlds.len(),
        stats,
        warnings,
        draw_warnings,
        horizon_roots: ctx.horizon_worlds,
        root_values: ctx.root_values,
    })
}

// ── Continual solving ───────────────────────────────────────────────────────

/// The two searches of one continual solve.
#[derive(Debug, Clone, Copy)]
pub struct ContinualConfig {
    /// The search of the root subgame. Both root passes use it.
    ///
    /// [`continual_solve`] raises [`MccfrConfig::horizon_worlds`] to one for the
    /// first pass. A first pass that keeps no world has no continuation belief
    /// to solve.
    pub root: MccfrConfig,
    /// The search of each continuation subgame.
    pub continuation: MccfrConfig,
    /// The largest number of continuation subgames to solve.
    ///
    /// `None` solves every public belief of the first pass. A cap keeps the
    /// first beliefs of the sorted key order.
    pub max_subgames: Option<usize>,
}

impl Default for ContinualConfig {
    fn default() -> Self {
        ContinualConfig {
            root: MccfrConfig {
                horizon_worlds: 4,
                ..MccfrConfig::default()
            },
            continuation: MccfrConfig::default(),
            max_subgames: Some(16),
        }
    }
}

/// One solved continuation subgame.
#[derive(Debug, Clone)]
pub struct ContinualStep {
    /// The public belief that this step solved.
    pub public: ObservationKey,
    /// Worlds of the continuation belief.
    pub worlds: usize,
    /// P1's win probability of the subgame.
    pub value: f64,
    /// What the subgame solve cost.
    pub stats: MctsStats,
    /// Why the value of the subgame is approximate.
    pub warnings: Vec<SolveWarning>,
}

/// What one continual solve produced.
#[derive(Debug, Clone)]
pub struct ContinualResult {
    /// The first pass, which used the configured leaf evaluator.
    pub root: MccfrResult,
    /// The last pass, which used the values of the continuation subgames.
    ///
    /// This pass is the answer of the continual solve.
    pub composed: MccfrResult,
    /// One entry for each continuation subgame, in sorted public-key order.
    pub steps: Vec<ContinualStep>,
    /// The information-set oracle that pass 2 built and pass 3 read.
    pub leaves: HorizonLeaves,
}

/// Solves the root subgame, its continuation subgames, and then the root again.
///
/// The first pass finds the public beliefs at the depth limit. Pass 2 solves
/// each of those beliefs, and pass 3 reads their information-set values. The
/// last pass therefore looks one subgame deeper than one search at that depth.
///
/// A continuation belief holds the sampled worlds of one public stream, not a
/// complete posterior. The value of a subgame is an estimate for that reason.
///
/// Returns an error when a particle is a preview position or a finished battle,
/// as [`search`] does.
pub fn continual_solve(
    seed: u64,
    belief: &ParticleBelief,
    pokemon_dex: &HashMap<Species, PokemonData>,
    move_dex: &HashMap<PokemonMove, MoveData>,
    config: &ContinualConfig,
) -> Result<ContinualResult, MccfrError> {
    let first = MccfrConfig {
        horizon_worlds: config.root.horizon_worlds.max(1),
        ..config.root
    };
    let root = search(seed, belief, pokemon_dex, move_dex, &first)?;

    // The raw key order is stable across runs, so the seed of each subgame and
    // the order of the steps are stable too.
    let mut publics: Vec<ObservationKey> = root.horizon_beliefs.keys().copied().collect();
    publics.sort_by_key(|public| public.raw());
    if let Some(cap) = config.max_subgames {
        publics.truncate(cap);
    }

    let mut leaves = HorizonLeaves::new();
    let mut steps = Vec::with_capacity(publics.len());
    for (index, public) in publics.into_iter().enumerate() {
        let worlds = &root.horizon_roots[&public];
        let subgame = search_root_belief(
            seed.wrapping_add(index as u64 + 1),
            RootBelief::from_horizon(worlds)?,
            pokemon_dex,
            move_dex,
            &config.continuation,
            None,
            Some(public),
            root.draw_warnings.clone(),
        )?;
        for (&key, value) in &subgame.root_values {
            if let Some(player_value) = value.counterfactual_value() {
                leaves.insert(key, player_value);
            }
        }
        steps.push(ContinualStep {
            public,
            worlds: worlds.len(),
            value: subgame.value,
            stats: subgame.stats,
            warnings: subgame.warnings,
        });
    }

    let composed = search_with_leaves(
        seed.wrapping_add(steps.len() as u64 + 1),
        belief,
        pokemon_dex,
        move_dex,
        &config.root,
        Some(&leaves),
    )?;

    Ok(ContinualResult {
        root,
        composed,
        steps,
        leaves,
    })
}

// ── The search ──────────────────────────────────────────────────────────────

/// Counterfactual regret minimization needs regret matching. Exp3 holds a reward
/// estimate rather than a regret, so it cannot drive this update.
const POLICY: SelectionPolicy = SelectionPolicy::RegretMatching;

/// Which player updates its regrets, and the reaches of the current path.
#[derive(Debug, Clone, Copy)]
struct Walk {
    /// 0 when P1 updates its regrets, and 1 when P2 does.
    traverser: usize,
    /// The on-policy reach of P1 and P2, in that order.
    player_reach: [f64; 2],
    /// The trajectory probability of the resolved turns.
    chance_reach: f64,
    /// The probability that the sampler produced the path to this node.
    sample_reach: f64,
}

impl Walk {
    fn divisor(self) -> f64 {
        self.sample_reach.max(f64::MIN_POSITIVE)
    }

    /// The importance weight of the current strategy path.
    fn importance_weight(self) -> f64 {
        self.player_reach[0] * self.player_reach[1] * self.chance_reach / self.divisor()
    }

    /// The sampled counterfactual reach for one player.
    fn counterfactual_reach(self, slot: usize) -> f64 {
        self.player_reach[1 - slot] * self.chance_reach / self.divisor()
    }

    /// The sampled average-strategy weight for one player.
    fn average_weight(self, slot: usize) -> f64 {
        self.player_reach[slot] / self.divisor()
    }
}

/// What one sampled path below a node produced.
#[derive(Debug, Clone, Copy)]
struct Descent {
    /// The utility of the traverser at the leaf, over the sampling probability
    /// of the whole path.
    utility: f64,
    /// The reach of the sampled suffix under the strategy of both players and of
    /// chance.
    ///
    /// The sampled counterfactual value needs the reach of every player below
    /// the node, not the reach of the traverser alone. The reach of the other
    /// player below the node cancels the same factor inside `utility`, which
    /// holds one over the sampling probability of the whole path.
    suffix: f64,
    /// P1's raw utility at the leaf.
    p1_value: f64,
    /// The on-policy trajectory reach over its sampling probability.
    importance_weight: f64,
}

/// What one node selection produced.
struct Selection {
    /// The learner index of each action that this world permits.
    allowed: Vec<usize>,
    /// The regret-matching probability of each entry of `allowed`.
    on_policy: Vec<f64>,
    /// The probability that the visit sampled each entry with.
    sampling: Vec<f64>,
    /// The entry of `allowed` that the visit played.
    played: usize,
}

struct MccfrContext<'a> {
    pokemon_dex: &'a HashMap<Species, PokemonData>,
    move_dex: &'a HashMap<PokemonMove, MoveData>,
    cfg: &'a MccfrConfig,
    /// The exploration rate after the zero check.
    exploration: f64,
    /// The continuation value of each public belief at the depth limit.
    leaves: Option<&'a HorizonLeaves>,
    /// The tree of P1 and the tree of P2, in that order.
    trees: [HashMap<InfoKey, InfoNode>; 2],
    /// Root information sets in first-visit order.
    root_keys: [Vec<InfoKey>; 2],
    /// The counterfactual value of each public belief at a leaf.
    horizon: HashMap<HorizonKey, HorizonValue>,
    /// The worlds that each public belief at the depth limit reached.
    horizon_worlds: HashMap<ObservationKey, Vec<RootWorld>>,
    /// The values at the root of a continuation search.
    root_values: HashMap<HorizonKey, HorizonValue>,
    /// How often the depth limit read the oracle.
    leaf_lookups: LeafLookups,
    stats: MctsStats,
    /// Largest action-set truncation for each player anywhere in the trees.
    action_truncations: [Option<(usize, usize)>; 2],
}

impl MccfrContext<'_> {
    /// Scores one position with the configured leaf evaluator.
    fn score(&self, battle: &BattleState) -> f64 {
        (self.cfg.search.eval)(battle, &EvalContext::new(self.pokemon_dex, self.move_dex))
    }

    /// Sample one path from `state`, and update every node on it.
    ///
    /// `histories` holds the observation key of P1 and of P2, in that order.
    /// `public` holds the stream that neither player owns.
    fn iterate(
        &mut self,
        state: &MatchState,
        depth: u8,
        chain: u8,
        histories: [ObservationKey; 2],
        public: ObservationKey,
        walk: Walk,
    ) -> Descent {
        let battle = match state {
            MatchState::GameOverState { winner, .. } => {
                return self.leaf(terminal_value(*winner), histories, public, walk);
            }
            // Not reachable from a battle position; scoring it as even is the
            // only neutral answer.
            MatchState::TeamPreviewState(_) => return self.leaf(0.5, histories, public, walk),
            MatchState::BattleState(battle) => battle,
        };
        if depth == 0 {
            let value = self.horizon_leaf_value(battle, public, histories, walk.traverser);
            self.record_horizon_world(public, state, histories, walk);
            return self.leaf(value, histories, public, walk);
        }

        let phase = actions::phase_of(state);
        let joint = [
            self.joint_actions(battle, Player::P1, phase),
            self.joint_actions(battle, Player::P2, phase),
        ];
        if joint[0].actions.is_empty() || joint[1].actions.is_empty() {
            // No decision exists here, so there is nothing to learn. The static
            // score is the only answer that the node can give.
            let value = self.score(battle);
            return self.leaf(value, histories, public, walk);
        }

        let keys = [(histories[0], depth, chain), (histories[1], depth, chain)];
        let traverser = walk.traverser;
        let opponent = 1 - traverser;
        // The traverser mixes with uniform play, so the regret update can divide
        // by the selection probability of the action that it played.
        let own = self.select(traverser, keys[traverser], &joint[traverser], self.exploration);
        let other = self.select(opponent, keys[opponent], &joint[opponent], 0.0);

        let mut commands = [Vec::new(), Vec::new()];
        commands[traverser] = joint[traverser].actions[own.played].clone();
        commands[opponent] = joint[opponent].actions[other.played].clone();

        self.stats.turns_simulated += 1;
        let sample = sample_transition(
            state,
            &PlayerCommand::Battle(commands[0].clone()),
            &PlayerCommand::Battle(commands[1].clone()),
            self.move_dex,
            self.pokemon_dex,
            TransitionConfig {
                consider_crit: self.cfg.search.consider_crit,
                damage_rolls: self.cfg.search.damage_rolls,
                // The node keys are observation histories, so the search needs
                // the masked streams of the turn.
                observe: true,
            },
        );
        let views = sample
            .observations
            .expect("the config sets the observe flag");
        let next = [
            histories[0].extend(&commands[0], &views.p1),
            histories[1].extend(&commands[1], &views.p2),
        ];
        // No player owns the public stream, so it chains no command.
        let next_public = public.extend(&[], &views.public);

        let other_probability = other.on_policy[other.played];
        let own_probability = own.on_policy[own.played];
        let other_sampling = other.sampling[other.played];
        let mut player_reach = walk.player_reach;
        player_reach[traverser] *= own_probability;
        player_reach[opponent] *= other_probability;
        let child = Walk {
            traverser,
            player_reach,
            chance_reach: walk.chance_reach * sample.trajectory_probability,
            sample_reach: walk.sample_reach
                * own.sampling[own.played]
                * other_sampling
                * sample.sampling_probability,
        };

        let (child_depth, child_chain) = self.descend(&sample.state, depth, chain);
        let below = self.iterate(&sample.state, child_depth, child_chain, next, next_public, child);

        // The reach of everything that follows the decision of the traverser:
        // the command of the other player, the trajectory of the turn, and the
        // sampled suffix below the successor.
        let tail = other_probability * sample.trajectory_probability * below.suffix;
        self.learn_regrets(traverser, keys[traverser], &own, &below, walk, tail);
        self.learn_average(opponent, keys[opponent], &other, walk);

        Descent {
            utility: below.utility,
            suffix: own_probability * tail,
            p1_value: below.p1_value,
            importance_weight: below.importance_weight,
        }
    }

    /// The value of one depth-limit leaf, as P1's win probability.
    ///
    /// The oracle wins over the configured evaluator. A utility is a win
    /// probability, so an oracle value clamps to `[0, 1]`.
    fn horizon_leaf_value(
        &mut self,
        battle: &BattleState,
        public: ObservationKey,
        histories: [ObservationKey; 2],
        traverser: usize,
    ) -> f64 {
        let key = HorizonKey {
            public,
            infoset: histories[traverser],
            player: player_of(traverser),
        };
        match self.leaves.and_then(|leaves| leaves.get(key)) {
            Some(value) => {
                self.leaf_lookups.hits += 1;
                let player_value = if value.is_finite() {
                    value.clamp(LOSS, WIN)
                } else {
                    0.5
                };
                if traverser == 0 {
                    player_value
                } else {
                    WIN - player_value
                }
            }
            None => {
                self.leaf_lookups.misses += 1;
                self.score(battle)
            }
        }
    }

    /// Keep one world of the continuation belief of `public`.
    ///
    /// The search keeps the first [`MccfrConfig::horizon_worlds`] worlds that
    /// reach the key, so one seed gives one set. The weight of a world is the
    /// importance weight of the path that reached it.
    fn record_horizon_world(
        &mut self,
        public: ObservationKey,
        state: &MatchState,
        histories: [ObservationKey; 2],
        walk: Walk,
    ) {
        let capacity = self.cfg.horizon_worlds;
        if capacity == 0 {
            return;
        }
        // A weight of zero carries no belief mass, and the normalization of the
        // set would drop it anyway.
        let weight = walk.importance_weight();
        if !weight.is_finite() || weight <= 0.0 {
            return;
        }
        let worlds = self.horizon_worlds.entry(public).or_default();
        if worlds.len() >= capacity {
            return;
        }
        worlds.push(RootWorld {
            state: state.clone(),
            histories,
            weight,
        });
    }

    /// Record one continuation value at its preserved root information set.
    fn record_root_value(
        &mut self,
        public: ObservationKey,
        histories: [ObservationKey; 2],
        traverser: usize,
        descent: &Descent,
    ) {
        let utility = if traverser == 0 {
            descent.p1_value
        } else {
            WIN - descent.p1_value
        };
        let reach = descent.importance_weight;
        if !reach.is_finite() || reach <= 0.0 || !utility.is_finite() {
            return;
        }
        let key = HorizonKey {
            public,
            infoset: histories[traverser],
            player: player_of(traverser),
        };
        let entry = self.root_values.entry(key).or_default();
        entry.value_sum += reach * utility;
        entry.reach_sum += reach;
        entry.visits += 1;
    }

    /// Record one leaf and start the return path.
    ///
    /// The leaf writes the counterfactual value of the traverser under the public
    /// stream of the path. Two hidden worlds that gave one public stream and one
    /// information set therefore add to one entry.
    fn leaf(
        &mut self,
        p1_value: f64,
        histories: [ObservationKey; 2],
        public: ObservationKey,
        walk: Walk,
    ) -> Descent {
        let utility = if walk.traverser == 0 {
            p1_value
        } else {
            WIN - p1_value
        };
        // A long path can drive the product below the smallest normal number.
        // The floor keeps the division finite.
        let divisor = walk.divisor();
        let reach = walk.counterfactual_reach(walk.traverser);
        let key = HorizonKey {
            public,
            infoset: histories[walk.traverser],
            player: player_of(walk.traverser),
        };
        let entry = self.horizon.entry(key).or_default();
        entry.value_sum += reach * utility;
        entry.reach_sum += reach;
        entry.visits += 1;

        Descent {
            utility: utility / divisor,
            suffix: 1.0,
            p1_value,
            importance_weight: walk.importance_weight(),
        }
    }

    /// Register the world actions in one node, and play its strategy.
    ///
    /// `slot` is 0 for P1 and 1 for P2. `exploration` is the uniform mix that the
    /// sampler adds. The traverser passes its configured rate, and the other
    /// player passes zero.
    fn select(
        &mut self,
        slot: usize,
        key: InfoKey,
        joint: &JointActions,
        exploration: f64,
    ) -> Selection {
        // The entry API does not report whether it built the node, so the count
        // comes from the size of the tree.
        let before = self.trees[slot].len();
        let node = self.trees[slot].entry(key).or_insert_with(InfoNode::new);
        let allowed = node.register(&joint.actions);
        let on_policy = node.learner.strategy_subset(POLICY, 0.0, &allowed);
        let sampling = if exploration > 0.0 {
            node.learner.strategy_subset(POLICY, exploration, &allowed)
        } else {
            on_policy.clone()
        };
        if self.trees[slot].len() > before {
            self.stats.nodes_created += 1;
        }
        let played = draw_index(&sampling);
        Selection {
            allowed,
            on_policy,
            sampling,
            played,
        }
    }

    /// Add the counterfactual regret of one traverser node.
    ///
    /// `tail` is the reach of everything below the decision of this node. `below`
    /// holds the utility of the sampled path.
    fn learn_regrets(
        &mut self,
        slot: usize,
        key: InfoKey,
        pick: &Selection,
        below: &Descent,
        walk: Walk,
        tail: f64,
    ) {
        let Some(node) = self.trees[slot].get_mut(&key) else {
            return;
        };
        // The counterfactual weight of the information set: the sampled utility
        // times the reach of the other player and of chance above this node.
        let opponent = 1 - slot;
        let weight = below.utility * walk.player_reach[opponent] * walk.chance_reach;
        let probability = pick.on_policy[pick.played];
        let regrets: Vec<f64> = (0..pick.allowed.len())
            .map(|entry| {
                let share = if entry == pick.played {
                    1.0 - probability
                } else {
                    -probability
                };
                weight * tail * share
            })
            .collect();
        node.learner.add_regrets_subset(&pick.allowed, &regrets);
    }

    /// Add the current strategy of one node to its average.
    ///
    /// The player of this node is the one that the iteration does not traverse.
    /// Its own reach over the sampling probability of the path is the weight that
    /// counterfactual regret minimization asks for.
    fn learn_average(&mut self, slot: usize, key: InfoKey, pick: &Selection, walk: Walk) {
        let Some(node) = self.trees[slot].get_mut(&key) else {
            return;
        };
        let weight = walk.average_weight(slot);
        node.learner
            .accumulate_subset_scaled(&pick.allowed, &pick.on_policy, weight);
    }

    /// The legal joint actions of one player, with any truncation recorded.
    fn joint_actions(
        &mut self,
        battle: &BattleState,
        player: Player,
        phase: Phase,
    ) -> JointActions {
        let joint = actions::joint_actions(
            battle,
            player,
            phase,
            self.move_dex,
            self.pokemon_dex,
            self.cfg.search.max_actions_per_player,
            // The dominance estimate reads the hidden stats of the target.
            // It can otherwise make an action available only in selected worlds.
            false,
        );
        if joint.was_capped() {
            let slot = match player {
                Player::P1 => 0,
                Player::P2 => 1,
            };
            let candidate = (joint.actions.len(), joint.total);
            let dropped = |(kept, total): (usize, usize)| total.saturating_sub(kept);
            if self.action_truncations[slot]
                .is_none_or(|previous| dropped(candidate) > dropped(previous))
            {
                self.action_truncations[slot] = Some(candidate);
            }
        }
        joint
    }

    /// The depth and forced-chain counter that a successor uses.
    ///
    /// A successor that waits for a replacement or a self-switch pivot is a
    /// decision point but not a new turn, so it does not consume a turn depth.
    fn descend(&self, child: &MatchState, depth: u8, chain: u8) -> (u8, u8) {
        match actions::phase_of(child) {
            Phase::SelfSwitch | Phase::Replacement if chain < self.cfg.search.max_forced_chain => {
                (depth, chain + 1)
            }
            _ => (depth.saturating_sub(1), 0),
        }
    }
}

/// The player of one tree slot.
fn player_of(slot: usize) -> Player {
    if slot == 0 { Player::P1 } else { Player::P2 }
}

#[cfg(test)]
mod tests {
    use super::Walk;

    /// Chance reach must affect values and counterfactual reaches, not averages.
    #[test]
    fn walk_uses_the_correct_reach_for_each_estimator() {
        let walk = Walk {
            traverser: 0,
            player_reach: [0.5, 0.25],
            chance_reach: 0.2,
            sample_reach: 0.05,
        };

        assert!((walk.importance_weight() - 0.5).abs() < 1e-12);
        assert!((walk.counterfactual_reach(0) - 1.0).abs() < 1e-12);
        assert!((walk.average_weight(0) - 10.0).abs() < 1e-12);
    }
}
