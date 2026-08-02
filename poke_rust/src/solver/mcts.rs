//! Samples a simultaneous stochastic game with Monte Carlo tree search.
//!
//! [`search`] trades the exact value for depth.
//! It never builds a payoff matrix, so its cost grows with the iteration count
//! instead of the squared action count.
//! The result reports the sampling error of the value, so a caller can see how
//! far to trust it.
//!
//! # Why this is not a `SolverAlgorithm`
//!
//! [`SolverAlgorithm`](super::SolverAlgorithm) requires one value from every
//! variant.
//! A sampled value differs between seeds, so this search needs its own entry
//! point and its own result type.
//! The exact search stays the oracle of the tests.
//!
//! # Decoupled learners
//!
//! Each node holds one independent learner for each player.
//! A learner sees only its own actions, so the node stores two vectors instead
//! of a matrix.
//! Both learners minimize regret, and the average payoff of two no-regret
//! learners converges to the value of a zero-sum game.
//!
//! [`SelectionPolicy`] names the two learners.
//! Both mix their strategy with the uniform strategy at rate
//! [`MctsConfig::exploration`].
//! The explicit exploration keeps every selection probability above zero, which
//! the importance-weighted updates divide by.
//!
//! # One iteration
//!
//! 1. Select one joint action for each player from the node strategy.
//! 2. Resolve the action pair with `simulate_turn`.
//! 3. Reduce the outcomes with [`ChanceMode`], and draw one successor by weight.
//! 4. Repeat at the successor until the depth limit, a finished battle, or a new
//!    node.
//! 5. Score a leaf with [`MctsConfig::eval`], and a finished battle with 0 or 1.
//! 6. Update each learner on the path with the returned value.
//!
//! A new node ends the descent. The search creates the node, scores the position
//! statically, and plays an action there on the next visit.
//! The search creates the root before the first iteration.
//!
//! # Averages
//!
//! The result holds the average strategy of each root learner, not the last
//! strategy. Only the average strategy converges to an equilibrium.
//!
//! The value is the mean of the root value of every iteration.
//! Explicit exploration biases that mean, because each player plays a uniform
//! action part of the time.
//! A smaller exploration rate lowers the bias and raises the variance.
//!
//! # Reproducibility
//!
//! One seed drives action selection, successor draws, and the engine.
//! The same seed and the same configuration give the same result.

use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};

use rand::Rng;

use crate::data::pokemon_move::PokemonMove;
use crate::data::species::Species;
use crate::simulator::helpers::sample_one_branch;
use crate::simulator::{scoped_sample_rng, simulate_turn, with_sample_rng};
use crate::state::battle::{BattleCommand, BattleState, MatchState, Player, PlayerCommand};
use crate::state::dex_data::{MoveData, PokemonData};

use super::actions::{self, JointActions, Phase};
use super::chance::ChanceMode;
use super::eval::{self, LeafEvaluator};
use super::matrix::EPS;
use super::search::strategy_of;
use super::{JointActionProb, SolveError, SolveWarning};

/// P1's utility when P1 loses, and when P1 wins.
const LOSS: f64 = 0.0;
const WIN: f64 = 1.0;

/// Selects the learner that each node uses.
///
/// Both learners observe one payoff for each iteration, so both divide that
/// payoff by the selection probability of the played action. The division keeps
/// the estimate unbiased.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionPolicy {
    /// Plays each action in proportion to its positive cumulative regret.
    ///
    /// The regret of an action measures the payoff that the action would have
    /// added. Uniform play covers a node whose regrets are all negative.
    RegretMatching,
    /// Plays each action in proportion to its exponential weight.
    ///
    /// The weight grows with the cumulative reward estimate of the action. The
    /// learning rate is [`MctsConfig::exploration`] divided by the action count.
    Exp3,
}

/// Everything the sampling search needs beyond the position itself.
#[derive(Debug, Clone, Copy)]
pub struct MctsConfig {
    /// Sampled paths from the root. Each path costs one `simulate_turn` per
    /// turn that it descends.
    pub iterations: u32,
    /// Turns of lookahead.
    /// Replacement and self-switch choices do not consume depth.
    pub depth: u8,
    pub policy: SelectionPolicy,
    /// Rate of uniform play, from 0 through 1.
    ///
    /// The rate bounds the selection probability of every action from below.
    /// A rate of zero permits a division by zero in the learner update, so
    /// [`search`] raises a zero rate to a small positive rate.
    pub exploration: f64,
    /// Damage rolls per attack.
    pub damage_rolls: u8,
    /// Whether to branch on critical hits.
    pub consider_crit: bool,
    /// How much of each turn's outcome distribution the draw can reach.
    /// A sparse mode removes outcome mass before the draw, and the result then
    /// holds [`SolveWarning::ChanceMassDiscarded`].
    pub chance: ChanceMode,
    /// Scores positions at the depth horizon.
    pub eval: LeafEvaluator,
    /// Maximum joint actions for each player.
    /// `None` keeps the complete action set.
    pub max_actions_per_player: Option<usize>,
    /// Removes an attack that another attack of the same slot beats on both
    /// damage and accuracy.
    pub prune_dominated_actions: bool,
    /// Maximum decision chain that does not consume depth.
    pub max_forced_chain: u8,
}

impl Default for MctsConfig {
    fn default() -> Self {
        MctsConfig {
            iterations: 1_000,
            depth: 2,
            policy: SelectionPolicy::RegretMatching,
            exploration: 0.1,
            damage_rolls: 1,
            consider_crit: false,
            chance: ChanceMode::Enumerate,
            eval: eval::heuristic,
            max_actions_per_player: None,
            prune_dominated_actions: false,
            max_forced_chain: 8,
        }
    }
}

/// The smallest exploration rate that the search accepts.
/// A rate of zero would divide by zero in a learner update.
const MIN_EXPLORATION: f64 = 1e-6;

/// How far the reported value can move between two runs of the same size.
#[derive(Debug, Clone)]
pub struct MctsSamplingError {
    /// The number of completed iterations.
    pub iterations: u64,
    /// The mean root value over those iterations.
    /// This equals [`MctsResult::value`].
    pub mean: f64,
    /// The standard error of `mean`.
    /// This is the sample standard deviation divided by the square root of
    /// `iterations`. One iteration gives `None`.
    pub standard_error: Option<f64>,
}

/// What the sampling search cost.
#[derive(Debug, Clone, Default)]
pub struct MctsStats {
    pub iterations: u64,
    /// Positions that entered the tree.
    pub nodes_created: u64,
    /// Completed `simulate_turn` calls.
    /// This is the main cost metric.
    pub turns_simulated: u64,
    /// Wall-clock time for the whole search.
    pub elapsed: Duration,
}

/// A sampled equilibrium for one position.
#[derive(Debug, Clone)]
pub struct MctsResult {
    /// The estimated game value: P1's win probability to the configured depth.
    /// Identical to `p1_win_odds`.
    pub value: f64,
    /// P1's odds of winning, in `[0, 1]`.
    pub p1_win_odds: f64,
    /// P2's odds of winning. The game is zero-sum, so this is `1 - p1_win_odds`.
    pub p2_win_odds: f64,
    /// P1's average strategy at the root. Actions at probability zero are
    /// omitted.
    pub p1_strategy: Vec<JointActionProb>,
    /// P2's average strategy at the root, likewise.
    pub p2_strategy: Vec<JointActionProb>,
    /// The sampling error of `value`.
    pub sampling: MctsSamplingError,
    pub stats: MctsStats,
    /// Why the answer is approximate beyond the sampling error itself.
    pub warnings: Vec<SolveWarning>,
}

/// Samples `state` to the configured depth.
///
/// The same `seed` and configuration always give the same result. The seed
/// covers the whole search, including the engine, so a sampling
/// [`ChanceMode`] stays reproducible.
///
/// Returns an error for a preview position and for a finished battle, as
/// [`solve`](super::solve) does.
pub fn search(
    seed: u64,
    state: &MatchState,
    pokemon_dex: &HashMap<Species, PokemonData>,
    move_dex: &HashMap<PokemonMove, MoveData>,
    config: &MctsConfig,
) -> Result<MctsResult, SolveError> {
    match state {
        MatchState::TeamPreviewState(_) => return Err(SolveError::TeamPreviewUnsupported),
        MatchState::GameOverState { winner, .. } => {
            return Err(SolveError::GameAlreadyOver { winner: *winner });
        }
        MatchState::BattleState(_) => {}
    }

    let started = Instant::now();
    let _guard = scoped_sample_rng(seed);

    // Depth 0 would score the root without a decision, and the strategy would
    // mean nothing. One turn is the minimum.
    let depth = config.depth.max(1);
    let iterations = config.iterations.max(1);

    let mut ctx = MctsContext {
        pokemon_dex,
        move_dex,
        cfg: config,
        exploration: config.exploration.clamp(MIN_EXPLORATION, 1.0),
        tree: HashMap::new(),
        stats: MctsStats::default(),
        max_discarded: 0.0,
        action_truncations: [None, None],
    };

    // Create the root before the iteration budget starts.
    // Each requested iteration then samples a path from a root action.
    let root_key = (hash_state(state), depth, 0);
    let battle = match state {
        MatchState::BattleState(battle) => battle,
        _ => unreachable!("the search rejected each non-battle state"),
    };
    let root = ctx.new_node(battle, state);
    ctx.tree.insert(root_key, root);
    ctx.stats.nodes_created += 1;

    let mut values = RunningStats::default();
    for _ in 0..iterations {
        values.push(ctx.iterate(state, depth, 0));
    }

    let root = ctx
        .tree
        .get(&root_key)
        .expect("the first iteration always creates the root node");

    let value = values.mean().clamp(LOSS, WIN);
    let mut warnings = Vec::new();
    if ctx.max_discarded > EPS {
        warnings.push(SolveWarning::ChanceMassDiscarded {
            max_fraction: ctx.max_discarded,
        });
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

    // A zero floor, not `EPS`: explicit exploration gives every action a real
    // probability, and a large action set can push that probability below `EPS`.
    let p1_strategy = strategy_of(&root.p1_actions, &root.p1.average_strategy(), 0.0);
    let p2_strategy = strategy_of(&root.p2_actions, &root.p2.average_strategy(), 0.0);

    let mut stats = ctx.stats;
    stats.iterations = values.count;
    stats.elapsed = started.elapsed();

    Ok(MctsResult {
        value,
        p1_win_odds: value,
        p2_win_odds: WIN - value,
        p1_strategy,
        p2_strategy,
        sampling: MctsSamplingError {
            iterations: values.count,
            mean: value,
            standard_error: values.standard_error(),
        },
        stats,
        warnings,
    })
}

/// What one learner pair produced on an explicit payoff matrix.
#[derive(Debug, Clone)]
pub struct LearnedMatrix {
    /// The mean payoff over every iteration.
    pub value: f64,
    /// The average strategy of the row player.
    pub row_strategy: Vec<f64>,
    /// The average strategy of the column player.
    pub col_strategy: Vec<f64>,
}

/// Runs one selection policy on an explicit payoff matrix.
///
/// The matrix holds the payoff of the row player, and the game is zero-sum.
/// Each payoff must be from 0 through 1, because the column player learns from
/// the complement of the payoff.
/// This entry point uses no engine and no tree, so it isolates the policy from
/// the search.
/// [`solve_matrix_game`](super::matrix::solve_matrix_game) supplies the exact
/// value of the same matrix.
pub fn learn_matrix_game(
    seed: u64,
    payoffs: &[Vec<f64>],
    iterations: u32,
    policy: SelectionPolicy,
    exploration: f64,
) -> LearnedMatrix {
    let rows = payoffs.len();
    let cols = payoffs.first().map_or(0, |row| row.len());
    if rows == 0 || cols == 0 {
        return LearnedMatrix {
            value: 0.0,
            row_strategy: vec![0.0; rows],
            col_strategy: vec![0.0; cols],
        };
    }

    let _guard = scoped_sample_rng(seed);
    let exploration = exploration.clamp(MIN_EXPLORATION, 1.0);
    let mut row_learner = Learner::new(rows);
    let mut col_learner = Learner::new(cols);
    let mut values = RunningStats::default();

    for _ in 0..iterations.max(1) {
        let row_strategy = row_learner.strategy(policy, exploration);
        let col_strategy = col_learner.strategy(policy, exploration);
        let row = draw_index(&row_strategy);
        let col = draw_index(&col_strategy);
        let payoff = payoffs[row][col];

        values.push(payoff);
        row_learner.accumulate(&row_strategy);
        col_learner.accumulate(&col_strategy);
        row_learner.update(policy, row, row_strategy[row], payoff);
        col_learner.update(policy, col, col_strategy[col], WIN - payoff);
    }

    LearnedMatrix {
        value: values.mean(),
        row_strategy: row_learner.average_strategy(),
        col_strategy: col_learner.average_strategy(),
    }
}

// ── The learners ────────────────────────────────────────────────────────────

/// One player's independent learner at one node.
struct Learner {
    /// Regret matching holds the cumulative regret of each action.
    /// Exp3 holds the cumulative reward estimate of each action.
    scores: Vec<f64>,
    /// The sum of every played strategy. It gives the average strategy.
    strategy_sum: Vec<f64>,
}

impl Learner {
    fn new(actions: usize) -> Self {
        Learner {
            scores: vec![0.0; actions],
            strategy_sum: vec![0.0; actions],
        }
    }

    /// The strategy to play now, mixed with the uniform strategy.
    fn strategy(&self, policy: SelectionPolicy, exploration: f64) -> Vec<f64> {
        let actions = self.scores.len();
        if actions == 0 {
            return Vec::new();
        }
        let uniform = 1.0 / actions as f64;

        let weights: Vec<f64> = match policy {
            SelectionPolicy::RegretMatching => self.scores.iter().map(|r| r.max(0.0)).collect(),
            SelectionPolicy::Exp3 => {
                // The learning rate of Exp3 over `actions` arms. The subtracted
                // maximum keeps the exponential finite, and it cancels in the
                // normalization below.
                let rate = exploration / actions as f64;
                let highest = self
                    .scores
                    .iter()
                    .copied()
                    .fold(f64::NEG_INFINITY, f64::max);
                self.scores
                    .iter()
                    .map(|s| (rate * (s - highest)).exp())
                    .collect()
            }
        };

        let total: f64 = weights.iter().sum();
        // Regret matching plays uniformly while every regret stays at or below
        // zero. Exp3 weights are always positive, so only regret matching
        // reaches this branch.
        let base: Vec<f64> = if total > EPS {
            weights.iter().map(|w| w / total).collect()
        } else {
            vec![uniform; actions]
        };

        base.iter()
            .map(|p| (1.0 - exploration) * p + exploration * uniform)
            .collect()
    }

    /// Add one played strategy to the average.
    fn accumulate(&mut self, strategy: &[f64]) {
        for (sum, probability) in self.strategy_sum.iter_mut().zip(strategy) {
            *sum += probability;
        }
    }

    /// Learn from the payoff of the played action.
    ///
    /// `played_probability` is the selection probability of that action. It is
    /// never zero, because the explicit exploration bounds it from below.
    fn update(
        &mut self,
        policy: SelectionPolicy,
        played: usize,
        played_probability: f64,
        reward: f64,
    ) {
        // The payoff of the played action alone, scaled so that its expectation
        // over the played strategy is the payoff of that action.
        let estimate = reward / played_probability;
        match policy {
            SelectionPolicy::RegretMatching => {
                for (action, score) in self.scores.iter_mut().enumerate() {
                    let counterfactual = if action == played { estimate } else { 0.0 };
                    *score += counterfactual - reward;
                }
            }
            SelectionPolicy::Exp3 => self.scores[played] += estimate,
        }
    }

    /// The strategy that the node played on average.
    fn average_strategy(&self) -> Vec<f64> {
        let actions = self.strategy_sum.len();
        if actions == 0 {
            return Vec::new();
        }
        let total: f64 = self.strategy_sum.iter().sum();
        if total <= EPS {
            // No iteration reached this node after it entered the tree.
            return vec![1.0 / actions as f64; actions];
        }
        self.strategy_sum.iter().map(|sum| sum / total).collect()
    }
}

// ── The tree ────────────────────────────────────────────────────────────────

/// A position, its search horizon, and its forced-chain counter.
///
/// The key holds a hash of the position rather than the position itself, as the
/// transposition table of the exact search does. A hash collision merges two
/// positions. Each node holds its own action lists, so a merge changes a value
/// and can never index outside a list.
type NodeKey = (u64, u8, u8);

/// One decision point, with one learner for each player.
struct Node {
    p1: Learner,
    p2: Learner,
    p1_actions: JointActions,
    p2_actions: JointActions,
}

struct MctsContext<'a> {
    pokemon_dex: &'a HashMap<Species, PokemonData>,
    move_dex: &'a HashMap<PokemonMove, MoveData>,
    cfg: &'a MctsConfig,
    /// The exploration rate after the zero check.
    exploration: f64,
    tree: HashMap<NodeKey, Node>,
    stats: MctsStats,
    /// Largest fraction of outcome probability dropped at any one chance node.
    max_discarded: f64,
    /// Largest action-set truncation for each player anywhere in the tree.
    action_truncations: [Option<(usize, usize)>; 2],
}

impl MctsContext<'_> {
    /// Sample one path from `state`, and return P1's value of that path.
    fn iterate(&mut self, state: &MatchState, depth: u8, chain: u8) -> f64 {
        let battle = match state {
            MatchState::GameOverState { winner, .. } => return terminal_value(*winner),
            // Not reachable from a battle position; scoring it as even is the
            // only neutral answer.
            MatchState::TeamPreviewState(_) => return 0.5,
            MatchState::BattleState(battle) => battle,
        };
        if depth == 0 {
            return (self.cfg.eval)(battle);
        }

        let key = (hash_state(state), depth, chain);
        if !self.tree.contains_key(&key) {
            let node = self.new_node(battle, state);
            self.tree.insert(key, node);
            self.stats.nodes_created += 1;
            // A new node holds no experience, so it has no strategy to play.
            // The static score goes back to the parent, and the next visit
            // plays an action here.
            return (self.cfg.eval)(battle);
        }

        let (p1_strategy, p2_strategy, p1_index, p2_index, p1_commands, p2_commands) = {
            let node = &self.tree[&key];
            let p1_strategy = node.p1.strategy(self.cfg.policy, self.exploration);
            let p2_strategy = node.p2.strategy(self.cfg.policy, self.exploration);
            let p1_index = draw_index(&p1_strategy);
            let p2_index = draw_index(&p2_strategy);
            (
                p1_strategy,
                p2_strategy,
                p1_index,
                p2_index,
                node.p1_actions.actions[p1_index].clone(),
                node.p2_actions.actions[p2_index].clone(),
            )
        };

        let branches = self.resolve(state, &p1_commands, &p2_commands);
        let value = match draw_successor(branches) {
            Some(child) => {
                let (child_depth, child_chain) = self.descend(&child, depth, chain);
                self.iterate(&child, child_depth, child_chain)
            }
            // The engine returned no outcome. Score the position instead.
            None => (self.cfg.eval)(battle),
        };

        let node = self
            .tree
            .get_mut(&key)
            .expect("the node exists for the whole iteration");
        node.p1.accumulate(&p1_strategy);
        node.p2.accumulate(&p2_strategy);
        node.p1
            .update(self.cfg.policy, p1_index, p1_strategy[p1_index], value);
        // The game is zero-sum over `[0, 1]`, so P2 maximizes the complement.
        node.p2
            .update(self.cfg.policy, p2_index, p2_strategy[p2_index], WIN - value);
        value
    }

    /// Build the node of a position that the tree does not hold yet.
    fn new_node(&mut self, battle: &BattleState, state: &MatchState) -> Node {
        let phase = actions::phase_of(state);
        let p1_actions = self.joint_actions(battle, Player::P1, phase);
        let p2_actions = self.joint_actions(battle, Player::P2, phase);
        Node {
            p1: Learner::new(p1_actions.actions.len()),
            p2: Learner::new(p2_actions.actions.len()),
            p1_actions,
            p2_actions,
        }
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
            self.cfg.max_actions_per_player,
            self.cfg.prune_dominated_actions,
        );
        if joint.was_capped() {
            let slot = match player {
                Player::P1 => 0,
                Player::P2 => 1,
            };
            let candidate = (joint.actions.len(), joint.total);
            let dropped = |(kept, total): (usize, usize)| total - kept;
            if self.action_truncations[slot]
                .is_none_or(|previous| dropped(candidate) > dropped(previous))
            {
                self.action_truncations[slot] = Some(candidate);
            }
        }
        joint
    }

    /// Resolve one joint action into its weighted successors, reduced by the
    /// configured [`ChanceMode`].
    fn resolve(
        &mut self,
        state: &MatchState,
        p1_commands: &[BattleCommand],
        p2_commands: &[BattleCommand],
    ) -> Vec<(MatchState, f64)> {
        self.stats.turns_simulated += 1;
        let raw: Vec<(MatchState, f64)> = simulate_turn(
            state,
            &PlayerCommand::Battle(p1_commands.to_vec()),
            &PlayerCommand::Battle(p2_commands.to_vec()),
            self.move_dex,
            self.pokemon_dex,
            self.cfg.consider_crit,
            self.cfg.damage_rolls,
            // No observer: event collection would cost time and would keep
            // equal states with different event histories apart.
            None,
        )
        .into_iter()
        .map(|(child, _events, probability)| (child, probability))
        .collect();

        // `simulate_turn` drains a `HashMap`, so successors that tie on
        // probability emerge in an order that varies between runs. The draw
        // reads the list in order, so one seed would otherwise give two
        // results. The state hash is a stable content-derived tiebreak.
        let mut keyed: Vec<(u64, MatchState, f64)> = raw
            .into_iter()
            .map(|(child, probability)| (hash_state(&child), child, probability))
            .collect();
        keyed.sort_by(|a, b| b.2.total_cmp(&a.2).then_with(|| a.0.cmp(&b.0)));
        let sorted: Vec<(MatchState, f64)> = keyed
            .into_iter()
            .map(|(_, child, probability)| (child, probability))
            .collect();

        let (kept, discarded) = self.cfg.chance.apply(sorted);
        if discarded > self.max_discarded {
            self.max_discarded = discarded;
        }
        kept
    }

    /// The depth and forced-chain counter that a successor uses.
    ///
    /// A successor that waits for a replacement or a self-switch pivot is a
    /// decision point but not a new turn, so it does not consume a turn depth.
    fn descend(&self, child: &MatchState, depth: u8, chain: u8) -> (u8, u8) {
        match actions::phase_of(child) {
            Phase::SelfSwitch | Phase::Replacement if chain < self.cfg.max_forced_chain => {
                (depth, chain + 1)
            }
            _ => (depth.saturating_sub(1), 0),
        }
    }
}

// ── Shared plumbing ─────────────────────────────────────────────────────────

/// The mean and the standard error of a value stream.
///
/// Welford's method holds three numbers instead of every value, which keeps the
/// memory constant in the iteration count.
#[derive(Debug, Clone, Default)]
struct RunningStats {
    count: u64,
    mean: f64,
    sum_of_squares: f64,
}

impl RunningStats {
    fn push(&mut self, value: f64) {
        self.count += 1;
        let first_delta = value - self.mean;
        self.mean += first_delta / self.count as f64;
        self.sum_of_squares += first_delta * (value - self.mean);
    }

    fn mean(&self) -> f64 {
        self.mean
    }

    /// The standard error of the mean. One sample gives `None`, because the
    /// sample variance divides by the count minus one.
    fn standard_error(&self) -> Option<f64> {
        (self.count > 1).then(|| {
            let variance = self.sum_of_squares / (self.count - 1) as f64;
            (variance / self.count as f64).sqrt()
        })
    }
}

/// Draw one index from a probability vector.
///
/// The vector sums to one up to rounding. The final index covers the rounding
/// remainder.
fn draw_index(strategy: &[f64]) -> usize {
    let roll: f64 = with_sample_rng(|rng| rng.gen_range(0.0..1.0));
    let mut accumulated = 0.0;
    for (index, probability) in strategy.iter().enumerate() {
        accumulated += probability;
        if roll < accumulated {
            return index;
        }
    }
    strategy.len().saturating_sub(1)
}

/// Draw one successor by weight, and drop the rest.
fn draw_successor(branches: Vec<(MatchState, f64)>) -> Option<MatchState> {
    sample_one_branch(branches)
        .pop()
        .map(|(child, _probability)| child)
}

fn terminal_value(winner: Player) -> f64 {
    match winner {
        Player::P1 => WIN,
        Player::P2 => LOSS,
    }
}

/// `MatchState` implements `Hash` by hand, and it excludes the bookkeeping
/// fields that do not affect play. It is therefore a sound key with no separate
/// canonicalization step.
fn hash_state(state: &MatchState) -> u64 {
    let mut hasher = DefaultHasher::new();
    state.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regret_matching_plays_uniformly_before_it_learns() {
        let learner = Learner::new(4);
        let strategy = learner.strategy(SelectionPolicy::RegretMatching, 0.1);
        assert_eq!(strategy.len(), 4);
        for probability in &strategy {
            assert!((probability - 0.25).abs() < 1e-12, "{probability}");
        }
    }

    #[test]
    fn exploration_bounds_every_selection_probability() {
        let mut learner = Learner::new(3);
        learner.scores = vec![100.0, -5.0, -5.0];
        for policy in [SelectionPolicy::RegretMatching, SelectionPolicy::Exp3] {
            let strategy = learner.strategy(policy, 0.3);
            let total: f64 = strategy.iter().sum();
            assert!((total - 1.0).abs() < 1e-9, "{policy:?} sums to {total}");
            for probability in &strategy {
                assert!(*probability >= 0.1 - 1e-9, "{policy:?} gave {probability}");
            }
        }
    }

    /// A large cumulative reward must not overflow the exponential.
    #[test]
    fn exp3_survives_a_large_score() {
        let mut learner = Learner::new(2);
        learner.scores = vec![1e6, 0.0];
        let strategy = learner.strategy(SelectionPolicy::Exp3, 0.1);
        let total: f64 = strategy.iter().sum();
        assert!((total - 1.0).abs() < 1e-9, "sums to {total}");
        assert!(strategy[0] > strategy[1]);
    }

    #[test]
    fn running_stats_report_the_mean_and_the_error() {
        let mut stats = RunningStats::default();
        assert_eq!(stats.standard_error(), None);
        for value in [0.0, 1.0, 0.0, 1.0] {
            stats.push(value);
        }
        assert!((stats.mean() - 0.5).abs() < 1e-12);
        // The sample standard deviation of that stream is 0.57735.
        let error = stats.standard_error().expect("four samples");
        assert!((error - 0.288675).abs() < 1e-6, "{error}");
    }

    #[test]
    fn draw_index_stays_inside_the_strategy() {
        let _guard = scoped_sample_rng(11);
        let strategy = vec![0.0, 1.0];
        for _ in 0..16 {
            assert_eq!(draw_index(&strategy), 1);
        }
    }

    /// A large action set drives each exploration probability below `EPS`. The
    /// reported strategy must still hold every action, and it must still sum to
    /// one.
    #[test]
    fn reported_strategy_keeps_positive_exploration_probabilities() {
        let action_count = 2_001;
        let exploration_probability = MIN_EXPLORATION / action_count as f64;
        let mut probabilities = vec![exploration_probability; action_count];
        probabilities[0] = 1.0 - exploration_probability * (action_count - 1) as f64;
        let joint = JointActions {
            actions: vec![vec![BattleCommand::Pass]; action_count],
            total: action_count,
        };

        let strategy = strategy_of(&joint, &probabilities, 0.0);
        let total: f64 = strategy.iter().map(|action| action.probability).sum();

        assert_eq!(strategy.len(), action_count);
        assert!((total - 1.0).abs() < 1e-12, "strategy sums to {total}");
    }
}
