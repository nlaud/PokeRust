//! Samples a fog-of-war position with information-set Monte Carlo tree search.
//!
//! [`mcts::search`](super::mcts::search) needs a concrete `MatchState`, and it
//! groups nodes by the state hash. A fog-of-war caller holds a belief instead,
//! and a state hash would tell two hidden worlds apart that the player cannot
//! tell apart.
//!
//! This search takes a [`ParticleBelief`] and keys its nodes by what each player
//! saw. It is the fast heuristic baseline of the fog-of-war solver.
//!
//! # Two trees
//!
//! The search keeps one tree for each player. A node key holds three parts:
//!
//! 1. The [`ObservationKey`] of that player. The key contains its private state,
//!    its own command history, and its masked event history.
//! 2. The search depth.
//! 3. The forced-chain counter.
//!
//! The key never holds a complete `MatchState` hash. Two worlds share a node when
//! the player has the same private state, commands, and events.
//!
//! # One iteration
//!
//! 1. Draw one particle from the belief in proportion to its weight.
//! 2. Read the legal joint actions of both players in that world.
//! 3. Register those actions in the node of each player.
//! 4. Play the strategy over the actions that this world permits.
//! 5. Resolve the pair with
//!    [`sample_transition`](crate::simulator::generative::sample_transition),
//!    with the observation flag set.
//! 6. Extend the observation history of each player with its masked stream.
//! 7. Repeat at the successor until the depth limit or a finished battle.
//! 8. Update both learners on the path with the value of the leaf.
//!
//! The search descends to the depth limit on every iteration. A fresh node plays
//! close to uniformly, because a learner with no experience has no preference,
//! so a fresh node acts as the default policy of a rollout.
//!
//! # Action registries
//!
//! One node covers many worlds, and two worlds can offer different actions. A
//! node therefore holds a registry that maps one joint action to one learner
//! index. A visit plays only the registered actions that its own world permits,
//! so the learner works as a subset-armed bandit. Read
//! [`infoset`](super::infoset) for the node, and `Learner::update_subset` in
//! [`mcts`](super::mcts) for that update.
//!
//! The reported root strategy covers the union of the actions over all root
//! information sets. The report weights each information set by its visits.
//!
//! # One strategy for each player
//!
//! The observer of the belief knows its own side, so every world gives it one
//! root information set. Its reported strategy is therefore the strategy of that
//! set.
//!
//! The other player can hold a different private build in each world, so it can
//! hold several root information sets. Its reported strategy mixes them. That
//! mixture is an observer-side summary, not a strategy that the other player
//! could play. Read each per-set strategy from the tree when the exact object
//! matters.
//!
//! # What the result measures
//!
//! [`IsmctsResult::sampling`] reports the error over the iterations. It does not
//! report the error of the belief itself.
//! [`IsmctsResult::effective_sample_size`] reports that second limit beside it. A
//! belief whose weight sits on a few particles gives a confident value of a
//! narrow set of worlds.
//!
//! # What this search is not
//!
//! Both trees learn against the same sampled opponent, so the pair converges to
//! a self-consistent strategy rather than to an equilibrium of the imperfect
//! information game. Outcome-sampling MCCFR is the equilibrium baseline, and this
//! search is the speed reference that it has to beat.
//!
//! # Reproducibility
//!
//! One seed drives the particle draws, the action selection, and the engine. The
//! same seed and the same configuration give the same result.
//!
//! # Cancellation
//!
//! [`search_cancellable`] and [`search_belief_cancellable`] read a
//! [`CancelFlag`](super::CancelFlag) at the top of the iteration loop.
//! A cancelled search returns the mean and the average strategy of the finished
//! iterations, and it reports
//! [`SolveWarning::Cancelled`](super::SolveWarning::Cancelled).
//! The particle draw itself reads no flag, because a partial particle set is not
//! a belief.

use std::collections::HashMap;
use std::fmt;
use std::time::Instant;

use crate::data::pokemon_move::PokemonMove;
use crate::data::species::Species;
use crate::information::determinize::{DeterminizeConfig, DeterminizeWarning};
use crate::information::unknowns::UnknownBattleState;
use crate::meta::MetaDex;
use crate::simulator::generative::{TransitionConfig, sample_transition};
use crate::simulator::scoped_sample_rng;
use crate::state::battle::{BattleState, MatchState, Player, PlayerCommand};
use crate::state::dex_data::{MoveData, PokemonData};

use super::actions::{self, JointActions, Phase};
use super::belief::{BeliefError, ObservationKey, ParticleBelief};
use super::eval::EvalContext;
use super::infoset::{InfoKey, InfoNode, root_strategy};
use super::mcts::{
    LOSS, MIN_EXPLORATION, MctsConfig, MctsResult, MctsSamplingError, MctsStats, RunningStats, WIN,
    draw_index, terminal_value,
};
use super::{CancelFlag, JointActionProb, SolveError, SolveWarning, cancel_requested};

/// Everything the fog-of-war search needs beyond the belief itself.
///
/// # Which shared knobs the search reads
///
/// The search reads these fields of [`MctsConfig`]: `iterations`, `depth`,
/// `policy`, `exploration`, `damage_rolls`, `consider_crit`, `eval`,
/// `max_actions_per_player`, `max_forced_chain`, and `control_variate`.
///
/// It ignores `transition`, `eval_batch`, `policy_prior`, `widening`,
/// `prune_dominated_actions`, and `common_random_numbers`. The transition must
/// provide event streams. The dominance test reads hidden target data, so an
/// information-set search cannot use it safely.
#[derive(Debug, Clone, Copy)]
pub struct IsmctsConfig {
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
}

impl Default for IsmctsConfig {
    fn default() -> Self {
        IsmctsConfig {
            search: MctsConfig {
                iterations: 200,
                ..MctsConfig::default()
            },
            particles: 16,
            resample_threshold: 0.5,
        }
    }
}

/// A sampled strategy pair for one fog-of-war position.
#[derive(Debug, Clone)]
pub struct IsmctsResult {
    /// The estimated game value: P1's win probability to the configured depth.
    /// Identical to `p1_win_odds`.
    pub value: f64,
    /// P1's odds of winning, in `[0, 1]`.
    pub p1_win_odds: f64,
    /// P2's odds of winning. The game is zero-sum, so this is `1 - p1_win_odds`.
    pub p2_win_odds: f64,
    /// P1's average strategy at the root, over the union of the world actions.
    pub p1_strategy: Vec<JointActionProb>,
    /// P2's average strategy at the root, likewise.
    pub p2_strategy: Vec<JointActionProb>,
    /// The sampling error of `value` over the iterations.
    pub sampling: MctsSamplingError,
    /// The effective sample size of the belief that the search used.
    ///
    /// This measures the second source of error. The iteration error does not
    /// hold it.
    pub effective_sample_size: f64,
    /// The particle count of that belief.
    pub particles: usize,
    pub stats: MctsStats,
    /// Why the answer is approximate beyond the two sampling errors.
    pub warnings: Vec<SolveWarning>,
    /// What the determinizer reported while it drew the worlds.
    /// Each distinct warning appears one time.
    pub draw_warnings: Vec<DeterminizeWarning>,
}

impl IsmctsResult {
    /// The perfect-information view of this result.
    ///
    /// The two searches report the same value, the same strategy pair, the same
    /// iteration error, and the same cost. A caller that compares the baselines
    /// reads this instead of two shapes.
    pub fn as_mcts(&self) -> MctsResult {
        MctsResult {
            value: self.value,
            p1_win_odds: self.p1_win_odds,
            p2_win_odds: self.p2_win_odds,
            p1_strategy: self.p1_strategy.clone(),
            p2_strategy: self.p2_strategy.clone(),
            sampling: self.sampling.clone(),
            stats: self.stats.clone(),
            warnings: self.warnings.clone(),
        }
    }
}

/// Why a fog-of-war position has no answer.
#[derive(Debug, Clone, PartialEq)]
pub enum IsmctsError {
    /// A particle is a preview position or a finished battle.
    Position(SolveError),
    /// The belief could not supply the worlds.
    Belief(BeliefError),
}

impl From<SolveError> for IsmctsError {
    fn from(error: SolveError) -> Self {
        IsmctsError::Position(error)
    }
}

impl From<BeliefError> for IsmctsError {
    fn from(error: BeliefError) -> Self {
        IsmctsError::Belief(error)
    }
}

impl fmt::Display for IsmctsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IsmctsError::Position(error) => write!(f, "{error}"),
            IsmctsError::Belief(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for IsmctsError {}

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
    config: &IsmctsConfig,
    determinize: &DeterminizeConfig,
) -> Result<IsmctsResult, IsmctsError> {
    search_belief_cancellable(
        seed,
        belief,
        meta_dex,
        pokemon_dex,
        move_dex,
        config,
        determinize,
        None,
    )
}

/// [`search_belief`], with a cooperative stop signal.
///
/// The draw runs first, and it reads no flag. A partial particle set is not a
/// belief, so the search cannot answer from one. [`search_cancellable`] then
/// reads the flag between iterations.
///
/// `None` gives the behavior of [`search_belief`].
#[allow(clippy::too_many_arguments)]
pub fn search_belief_cancellable(
    seed: u64,
    belief: &UnknownBattleState,
    meta_dex: &MetaDex,
    pokemon_dex: &HashMap<Species, PokemonData>,
    move_dex: &HashMap<PokemonMove, MoveData>,
    config: &IsmctsConfig,
    determinize: &DeterminizeConfig,
    cancel: Option<&CancelFlag>,
) -> Result<IsmctsResult, IsmctsError> {
    let particles = ParticleBelief::from_belief(
        seed,
        belief,
        meta_dex,
        pokemon_dex,
        move_dex,
        config.particles,
        determinize,
    )?;
    search_cancellable(seed, &particles, pokemon_dex, move_dex, config, cancel)
}

/// Searches a particle set that the caller already holds.
///
/// The search resamples the set one time when its effective sample size falls
/// below [`IsmctsConfig::resample_threshold`]. It leaves the caller's set alone.
///
/// Returns an error when a particle is a preview position or a finished battle,
/// as [`solve`](super::solve) does.
pub fn search(
    seed: u64,
    belief: &ParticleBelief,
    pokemon_dex: &HashMap<Species, PokemonData>,
    move_dex: &HashMap<PokemonMove, MoveData>,
    config: &IsmctsConfig,
) -> Result<IsmctsResult, IsmctsError> {
    search_cancellable(seed, belief, pokemon_dex, move_dex, config, None)
}

/// [`search`], with a cooperative stop signal.
///
/// The search reads `cancel` at the top of the iteration loop. One iteration is
/// the unit of work: it draws one world, it descends one path, it updates both
/// trees, and it pushes one value into the running mean. A set flag ends the
/// loop, and the result then holds the mean, the sampling error, and the average
/// strategy of the finished iterations alone.
///
/// The search always finishes iteration 1, for the reason
/// [`mcts::search_cancellable`](super::mcts::search_cancellable) gives.
///
/// A cancelled result carries [`SolveWarning::Cancelled`], and
/// [`MctsStats::iterations`] holds the finished count. The reported belief size
/// and effective sample size describe the set that the search used, so a cancel
/// does not change them.
///
/// `None` gives the behavior of [`search`].
pub fn search_cancellable(
    seed: u64,
    belief: &ParticleBelief,
    pokemon_dex: &HashMap<Species, PokemonData>,
    move_dex: &HashMap<PokemonMove, MoveData>,
    config: &IsmctsConfig,
    cancel: Option<&CancelFlag>,
) -> Result<IsmctsResult, IsmctsError> {
    if belief.is_empty() {
        return Err(BeliefError::NoParticles.into());
    }
    for particle in belief.particles() {
        match &particle.state {
            MatchState::TeamPreviewState(_) => {
                return Err(SolveError::TeamPreviewUnsupported.into());
            }
            MatchState::GameOverState { .. } => {
                // A finished world holds no decision. One of them would make the
                // reported strategy an average over fewer worlds than the count
                // claims, so refuse the whole set.
                let winner = match &particle.state {
                    MatchState::GameOverState { winner, .. } => *winner,
                    _ => unreachable!("the arm matched a finished battle"),
                };
                return Err(SolveError::GameAlreadyOver { winner }.into());
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

    // The search owns its copy, so a resample never changes the caller's set.
    let mut worlds = belief.clone();
    worlds.resample_if_degenerate(config.resample_threshold);

    let mut ctx = IsmctsContext {
        pokemon_dex,
        move_dex,
        cfg: config,
        exploration: config.search.exploration.clamp(MIN_EXPLORATION, 1.0),
        trees: [HashMap::new(), HashMap::new()],
        root_keys: [Vec::new(), Vec::new()],
        stats: MctsStats::default(),
        action_truncations: [None, None],
        cancel,
    };

    let mut values = RunningStats::default();
    let mut cancelled = false;
    for index in 0..iterations {
        // Iteration 1 always runs. Every later iteration reads the flag first,
        // so the answer covers whole iterations and nothing else.
        if index > 0
            && (cancel_requested(cancel)
                || cancel.is_some_and(CancelFlag::simulation_budget_hit))
        {
            cancelled = true;
            break;
        }
        let state = worlds.draw().expect("the set is not empty").state.clone();
        let MatchState::BattleState(battle) = &state else {
            unreachable!("the search rejected each non-battle state");
        };
        let histories = [
            ObservationKey::for_player(battle, Player::P1),
            ObservationKey::for_player(battle, Player::P2),
        ];
        let (root_depth, root_chain) = super::root_descent(
            actions::phase_of(&state),
            depth,
            config.search.replacement_depth,
        );
        for (keys, &history) in ctx.root_keys.iter_mut().zip(&histories) {
            let key = (history, root_depth, root_chain);
            if !keys.contains(&key) {
                keys.push(key);
            }
        }
        values.push(ctx.iterate(&state, root_depth, root_chain, histories));
    }

    let p1_strategy = root_strategy(&ctx.trees[0], &ctx.root_keys[0]);
    let p2_strategy = root_strategy(&ctx.trees[1], &ctx.root_keys[1]);

    let mut warnings = Vec::new();
    if cancelled && cancel_requested(cancel) {
        warnings.push(SolveWarning::Cancelled);
    }
    if cancel.is_some_and(CancelFlag::simulation_budget_hit)
        && let Some(budget) = cancel.and_then(CancelFlag::simulation_turn_budget)
    {
        warnings.push(SolveWarning::SimulationTurnBudgetExhausted { budget });
    }
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

    Ok(IsmctsResult {
        value,
        p1_win_odds: value,
        p2_win_odds: WIN - value,
        p1_strategy,
        p2_strategy,
        sampling: MctsSamplingError {
            iterations: values.count,
            mean: value,
            // Every iteration draws its own particle and its own universe, so
            // the independent-sample formula holds.
            standard_error: values.standard_error(),
        },
        effective_sample_size: worlds.effective_sample_size(),
        particles: worlds.len(),
        stats,
        warnings,
        draw_warnings: worlds.warnings().to_vec(),
    })
}

// ── The trees ───────────────────────────────────────────────────────────────

struct IsmctsContext<'a> {
    pokemon_dex: &'a HashMap<Species, PokemonData>,
    move_dex: &'a HashMap<PokemonMove, MoveData>,
    cfg: &'a IsmctsConfig,
    /// The exploration rate after the zero check.
    exploration: f64,
    /// The tree of P1 and the tree of P2, in that order.
    trees: [HashMap<InfoKey, InfoNode>; 2],
    /// Root information sets in first-visit order.
    root_keys: [Vec<InfoKey>; 2],
    stats: MctsStats,
    /// Largest action-set truncation for each player anywhere in the trees.
    action_truncations: [Option<(usize, usize)>; 2],
    cancel: Option<&'a CancelFlag>,
}

/// What one node selection produced.
struct Selection {
    /// The learner index of each action that this world permits.
    allowed: Vec<usize>,
    /// The probability of each entry of `allowed`.
    strategy: Vec<f64>,
    /// The entry of `allowed` that the visit played.
    played: usize,
}

impl IsmctsContext<'_> {
    /// Scores one position with the configured leaf evaluator.
    fn score(&self, battle: &BattleState) -> f64 {
        (self.cfg.search.eval)(battle, &EvalContext::new(self.pokemon_dex, self.move_dex))
    }

    /// Sample one path from `state`, and return P1's value of that path.
    ///
    /// `histories` holds the observation key of P1 and of P2, in that order.
    fn iterate(
        &mut self,
        state: &MatchState,
        depth: u8,
        chain: u8,
        histories: [ObservationKey; 2],
    ) -> f64 {
        let battle = match state {
            MatchState::GameOverState { winner, .. } => return terminal_value(*winner),
            // Not reachable from a battle position; scoring it as even is the
            // only neutral answer.
            MatchState::TeamPreviewState(_) => return 0.5,
            MatchState::BattleState(battle) => battle,
        };
        if depth == 0 {
            return self.score(battle);
        }

        let phase = actions::phase_of(state);
        let joint = [
            self.joint_actions(battle, Player::P1, phase),
            self.joint_actions(battle, Player::P2, phase),
        ];
        if joint[0].actions.is_empty() || joint[1].actions.is_empty() {
            // No decision exists here, so there is nothing to learn. The static
            // score is the only answer that the node can give.
            return self.score(battle);
        }

        let keys = [(histories[0], depth, chain), (histories[1], depth, chain)];
        let picks = [
            self.select(0, keys[0], &joint[0]),
            self.select(1, keys[1], &joint[1]),
        ];
        let commands = [
            joint[0].actions[picks[0].played].clone(),
            joint[1].actions[picks[1].played].clone(),
        ];

        if self
            .cancel
            .is_some_and(|control| !control.claim_simulation_turn())
        {
            let value = self.score(battle);
            self.learn(0, keys[0], &picks[0], value);
            self.learn(1, keys[1], &picks[1], WIN - value);
            return value;
        }
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

        let (child_depth, child_chain) = self.descend(&sample.state, depth, chain);
        let value = self.iterate(&sample.state, child_depth, child_chain, next);

        // The game is zero-sum over `[0, 1]`, so P2 learns from the complement.
        self.learn(0, keys[0], &picks[0], value);
        self.learn(1, keys[1], &picks[1], WIN - value);
        value
    }

    /// Register the world actions in one node, and play its strategy.
    ///
    /// `slot` is 0 for P1 and 1 for P2.
    fn select(&mut self, slot: usize, key: InfoKey, joint: &JointActions) -> Selection {
        let policy = self.cfg.search.policy;
        let exploration = self.exploration;
        // The entry API does not report whether it built the node, so the count
        // comes from the size of the tree.
        let before = self.trees[slot].len();
        let discount = self.cfg.search.average_discount;
        let node = self.trees[slot]
            .entry(key)
            .or_insert_with(|| InfoNode::new(discount));
        let allowed = node.register(&joint.actions);
        let strategy = node.learner.strategy_subset(policy, exploration, &allowed);
        if self.trees[slot].len() > before {
            self.stats.nodes_created += 1;
        }
        let played = draw_index(&strategy);
        Selection {
            allowed,
            strategy,
            played,
        }
    }

    /// Teach one node what its played action returned.
    fn learn(&mut self, slot: usize, key: InfoKey, pick: &Selection, reward: f64) {
        let policy = self.cfg.search.policy;
        let control_variate = self.cfg.search.control_variate;
        let Some(node) = self.trees[slot].get_mut(&key) else {
            return;
        };
        node.learner.accumulate_subset(&pick.allowed, &pick.strategy);
        node.learner.update_subset(
            policy,
            pick.played,
            &pick.allowed,
            &pick.strategy,
            reward,
            control_variate,
        );
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
    /// [`super::forced_descent`] holds the rule, and
    /// [`MctsConfig::replacement_depth`] gives such a decision its own depth.
    fn descend(&self, child: &MatchState, depth: u8, chain: u8) -> (u8, u8) {
        super::forced_descent(
            actions::phase_of(child),
            depth,
            chain,
            self.cfg.search.max_forced_chain,
            self.cfg.search.replacement_depth,
        )
    }
}
