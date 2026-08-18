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
//! 2. Resolve the action pair with [`MctsConfig::transition`].
//! 3. Take the one successor that the mode produced.
//! 4. Repeat at the successor until the depth limit, a finished battle, or a new
//!    node.
//! 5. Score a leaf with [`MctsConfig::eval`], and a finished battle with 0 or 1.
//! 6. Update each learner on the path with the returned value.
//!
//! A new node ends the descent. The search creates the node, scores the position
//! statically, and plays an action there on the next visit.
//! The search creates the root before the first iteration.
//!
//! # Transition modes
//!
//! [`TransitionMode::Enumerated`] builds the complete outcome distribution of the
//! turn, reduces it with a [`ChanceMode`], and draws one successor by weight.
//! Every node then pays for the whole distribution.
//!
//! [`TransitionMode::Generative`] samples inside turn resolution with
//! [`simulator::generative`](crate::simulator::generative). The engine keeps one
//! branch at each chokepoint, so a node costs one trajectory. The draw stays
//! unbiased, because each chokepoint picks a branch in proportion to its weight,
//! and this mode discards no outcome mass.
//!
//! # Stratified batches
//!
//! A chance node is one position, one P1 action, and one P2 action. The search
//! visits that node once for every iteration that reaches it, and each visit
//! resolves the same turn again.
//!
//! [`TransitionMode::Generative`] can spread those visits over a stratified
//! batch. The `batch` field holds the member count.
//!
//! The search keeps one cursor for each chance node. The cursor names the plan
//! seed of the current batch and the members that the node already used. A visit
//! builds the plan, installs its member, and resolves the turn. The cursor draws
//! a new seed after the last member, so the next group of visits forms a new
//! batch.
//!
//! The batch therefore covers each random dimension of the turn across the unit
//! interval instead of clustering by chance. Read
//! [`simulator::stratify`](crate::simulator::stratify) for the construction and
//! for the proof that one member keeps the law of one independent draw.
//!
//! The cursor key holds both action indexes, so every member of one batch
//! resolves one position under one command pair. The engine gives one dimension
//! to each chokepoint of more than one branch, so the dimension of a chokepoint
//! stays the same across the batch. A cursor keyed by the position alone would
//! mix members of different command pairs and lose that alignment.
//!
//! The search rebuilds the plan on each visit instead of storing it. A stored
//! plan costs [`STRATIFIED_DIMENSIONS`] times the member count in indexes for
//! every chance node. The rebuild costs that many permutations, which is small
//! beside one `simulate_turn` call.
//!
//! [`STRATIFIED_DIMENSIONS`]:
//!     crate::simulator::stratify::STRATIFIED_DIMENSIONS
//!
//! The search limits the batch to its iteration count. A larger plan has members
//! that the search cannot use.
//!
//! A `batch` of one or zero installs no plan, so the search keeps the
//! independent draw.
//!
//! # Common random numbers
//!
//! Two visits of one node draw two different random universes. A learner that
//! compares two actions across those visits sees the noise of the universe on
//! top of the difference between the actions.
//!
//! [`MctsConfig::common_random_numbers`] removes that noise. Each node holds a
//! pool of `k` universe seeds. Resolution `v` of each action pair uses universe
//! `v % k`. Thus, all action pairs use the same seed for their first resolution.
//! They also use the same seed for each later resolution index.
//!
//! The pool comes from the seeded stream of the search, so one seed still gives
//! one result. A uniform seed gives a correct draw of the transition, so one
//! visit keeps the law of one independent draw. The pool makes the visits of one
//! node dependent, as a stratified batch does, and the result therefore reports
//! no standard error while a pool is active.
//!
//! A pool of `k` universes gives one action pair at most `k` distinct
//! successors. A small pool reduces outcome coverage but gives a cleaner action
//! comparison.
//!
//! That trade has a measured cost. The reported value is the mean root value
//! over the iterations, and a pool leaves fewer distinct universes in that mean,
//! so the error of the value grows. The gain is a lower exploitability gap of
//! the root strategy. `common_random_numbers_lower_the_exploitability_gap` holds
//! the measurement.
//!
//! # Control variates
//!
//! Both learners divide the payoff of the played action by its selection
//! probability. That estimate is unbiased, and its variance grows as the
//! probability falls.
//!
//! [`MctsConfig::control_variate`] subtracts a baseline before the division:
//!
//! ```text
//! value(a) = baseline(a) + [a == played] * (reward - baseline(a)) / p(a)
//! ```
//!
//! The expectation over the draw is the true value of the action for any
//! baseline, so the estimate stays unbiased. The variance falls when the
//! baseline is close to that value.
//!
//! Each learner keeps the running mean reward of each action as its baseline.
//! The learner reads the baseline before it adds the new sample, so the baseline
//! holds no part of the draw that it corrects.
//!
//! The baseline lowers the error of
//! [`learn_matrix_game`] on a short run, and it lowers the exploitability gap of
//! the root strategy of the search. It does not lower the error of the reported
//! value, which the explicit exploration biases.
//! `control_variates_lower_the_matrix_error` and
//! `control_variates_lower_the_exploitability_gap` hold the measurements.
//!
//! # Progressive widening
//!
//! [`MctsConfig::widening`] limits a node to a prefix of its action list.
//! The prefix grows with the visit count of the node, so the early iterations
//! spread over few actions instead of over hundreds.
//! [`Widening::allowed`] holds the growth rule.
//!
//! A node in this mode stores its actions in the coverage order of
//! [`actions::coverage_order`], so a prefix of `k` actions holds as many
//! distinct slot commands, targets, and resource choices as `k` permits.
//!
//! Each learner sees only the allowed prefix. A hidden action keeps a score of
//! zero, so it starts level with the visible actions when the prefix reaches it.
//!
//! A root that never reaches its complete set reports
//! [`SolveWarning::ActionsTruncated`], because its strategy then covers a subset
//! of the legal actions. [`exploit`](super::exploit) measures what that subset
//! costs.
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
//!
//! # Cancellation
//!
//! [`search_cancellable`] reads a [`CancelFlag`](super::CancelFlag) at the top
//! of the iteration loop.
//! A cancelled search returns the mean and the average strategy of the finished
//! iterations, and it reports
//! [`SolveWarning::Cancelled`](super::SolveWarning::Cancelled).
//! A cancel never returns an error.

use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};

use rand::Rng;

use crate::data::pokemon_move::PokemonMove;
use crate::data::species::Species;
use crate::simulator::generative::{TransitionConfig, TransitionSample, sample_transition};
use crate::simulator::helpers::sample_one_branch;
use crate::simulator::stratify::StratifiedPlan;
use crate::simulator::{SampleRngGuard, scoped_sample_rng, simulate_turn, with_sample_rng};
use crate::state::battle::{BattleCommand, BattleState, MatchState, Player, PlayerCommand};
use crate::state::dex_data::{MoveData, PokemonData};

use super::actions::{self, JointActions, Phase};
use super::chance::ChanceMode;
use super::eval::{self, BatchEvaluator, EvalContext, LeafEvaluator, PolicyFeatures};
use super::matrix::EPS;
use super::search::strategy_of;
use super::{CancelFlag, JointActionProb, SolveError, SolveWarning, cancel_requested};

/// P1's utility when P1 loses, and when P1 wins.
pub(super) const LOSS: f64 = 0.0;
pub(super) const WIN: f64 = 1.0;

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
    /// How the search turns one joint action into one successor.
    pub transition: TransitionMode,
    /// Scores positions at the depth horizon.
    pub eval: LeafEvaluator,
    /// Scores a slice of positions in one call; see [`eval::BatchEvaluator`].
    ///
    /// This search is depth first, so it reaches one leaf at a time and never
    /// calls this pointer. A model evaluator and a parallel search need the
    /// entry point.
    pub eval_batch: Option<BatchEvaluator>,
    /// Orders each action list by the policy score of
    /// [`eval::policy_scores`] before progressive widening takes its prefix.
    ///
    /// `false` keeps [`actions::coverage_order`]. The flag changes only which
    /// actions a narrow prefix reaches, because no other part of the search
    /// reads the policy.
    pub policy_prior: bool,
    /// Maximum joint actions for each player.
    /// `None` keeps the complete action set.
    pub max_actions_per_player: Option<usize>,
    /// Removes an attack that another attack of the same slot beats on both
    /// damage and accuracy.
    pub prune_dominated_actions: bool,
    /// Maximum decision chain that does not consume depth.
    pub max_forced_chain: u8,
    /// Turns of lookahead below a replacement or a self-switch pivot.
    /// `None` gives a forced decision the remaining turn budget, as a turn gets.
    /// Read [`super::forced_descent`] for the rule and its termination bound.
    pub replacement_depth: Option<u8>,
    /// Grows the action set of a node with its visit count.
    /// `None` gives every node its complete action set from the first visit.
    pub widening: Option<Widening>,
    /// Universe seeds that each node reuses across its visits.
    ///
    /// `None` and `Some(0)` keep independent draws. Each action pair uses the
    /// same seed for the same resolution index. The pool size limits the distinct
    /// successors of one action pair.
    ///
    /// The search limits the pool size to [`MctsConfig::iterations`].
    ///
    /// A pool makes the visits of one node dependent, so
    /// [`MctsSamplingError::standard_error`] then reports `None`.
    pub common_random_numbers: Option<usize>,
    /// Subtracts the running mean reward of an action before the learner
    /// divides by its selection probability.
    ///
    /// The estimate stays unbiased, and its variance falls as the mean
    /// approaches the value of the action.
    pub control_variate: bool,
}

/// How the search produces the successor of one joint action.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TransitionMode {
    /// Enumerate the outcome distribution of the turn, reduce it with the
    /// [`ChanceMode`], and draw one successor by weight.
    ///
    /// A sparse [`ChanceMode`] removes outcome mass before the draw, and the
    /// result then holds [`SolveWarning::ChanceMassDiscarded`].
    Enumerated(ChanceMode),
    /// Sample one successor inside turn resolution.
    ///
    /// The engine never builds the outcome list, so a node costs one trajectory
    /// instead of one distribution. This mode discards no outcome mass.
    Generative {
        /// Members of the stratified batch that one chance node draws.
        ///
        /// The search spreads one batch over consecutive visits of the node, so
        /// this field costs no extra turn resolution. A value of one or zero
        /// keeps the independent draw.
        ///
        /// A larger batch covers each random dimension more evenly, and it also
        /// delays the point at which the node starts a fresh batch.
        /// The search limits this value to [`MctsConfig::iterations`].
        batch: usize,
    },
}

/// The growth rule of progressive widening.
///
/// A node with `visits` visits and `total` actions may play
/// `max(initial, floor(coefficient * visits ^ exponent))`, clamped to
/// `1..=total`. The count never falls, so a node never loses an action that it
/// already played.
///
/// A small `exponent` widens slowly and gives each action more samples. An
/// `exponent` of 1 widens in proportion to the visit count.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Widening {
    /// Actions of the first visit.
    pub initial: usize,
    /// The multiplier of the growth term.
    pub coefficient: f64,
    /// The exponent of the growth term, from 0 through 1.
    pub exponent: f64,
}

impl Default for Widening {
    fn default() -> Self {
        Widening {
            initial: 4,
            coefficient: 2.0,
            exponent: 0.5,
        }
    }
}

impl Widening {
    /// The actions that a node with `visits` visits may play.
    ///
    /// The result is never above `total` and never below one, so a caller can
    /// index the action list with it. A node with no action returns zero.
    pub fn allowed(&self, visits: u64, total: usize) -> usize {
        if total == 0 {
            return 0;
        }
        // Both fields come from a caller, so both need a sane range. A negative
        // exponent would shrink the count as the node grows.
        let exponent = self.exponent.clamp(0.0, 1.0);
        let coefficient = self.coefficient.max(0.0);
        let grown = coefficient * (visits as f64).powf(exponent);
        let grown = if grown.is_finite() {
            grown.floor().max(0.0).min(total as f64) as usize
        } else {
            total
        };
        grown.max(self.initial).clamp(1, total)
    }
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
            transition: TransitionMode::Enumerated(ChanceMode::Enumerate),
            eval: eval::fitted,
            eval_batch: None,
            policy_prior: false,
            max_actions_per_player: None,
            prune_dominated_actions: false,
            max_forced_chain: 8,
            replacement_depth: None,
            widening: None,
            common_random_numbers: None,
            control_variate: false,
        }
    }
}

/// The smallest exploration rate that the search accepts.
/// A rate of zero would divide by zero in a learner update.
pub(super) const MIN_EXPLORATION: f64 = 1e-6;

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
    /// `iterations`.
    ///
    /// One iteration gives `None`. A stratified search and a search with common
    /// random numbers also give `None`, because both make the visits of one node
    /// dependent.
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
    search_cancellable(seed, state, pokemon_dex, move_dex, config, None)
}

/// [`search`], with a cooperative stop signal.
///
/// The search reads `cancel` at the top of the iteration loop. One iteration is
/// the unit of work: it descends one path, it updates the tree, and it pushes
/// one value into the running mean. A set flag ends the loop, and the result
/// then holds the mean, the sampling error, and the average strategy of the
/// finished iterations alone.
///
/// The search always finishes iteration 1. A mean over zero iterations has no
/// value, and a tree with no visit has no strategy.
///
/// A cancelled result carries [`SolveWarning::Cancelled`], and
/// [`MctsStats::iterations`] holds the finished count.
///
/// `None` gives the behavior of [`search`].
pub fn search_cancellable(
    seed: u64,
    state: &MatchState,
    pokemon_dex: &HashMap<Species, PokemonData>,
    move_dex: &HashMap<PokemonMove, MoveData>,
    config: &MctsConfig,
    cancel: Option<&CancelFlag>,
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
    let (root_depth, root_chain) =
        super::root_descent(actions::phase_of(state), depth, config.replacement_depth);
    let iterations = config.iterations.max(1);
    let batch = effective_batch(config.transition, iterations);
    let universes = effective_universes(config, iterations);

    let mut ctx = MctsContext {
        pokemon_dex,
        move_dex,
        cfg: config,
        exploration: config.exploration.clamp(MIN_EXPLORATION, 1.0),
        batch,
        universes,
        tree: HashMap::new(),
        chance_cursors: BatchCursors::default(),
        stats: MctsStats::default(),
        max_discarded: 0.0,
        action_truncations: [None, None],
        cancel,
    };

    // Create the root before the iteration budget starts.
    // Each requested iteration then samples a path from a root action.
    let root_key = (hash_state(state), root_depth, root_chain);
    let battle = match state {
        MatchState::BattleState(battle) => battle,
        _ => unreachable!("the search rejected each non-battle state"),
    };
    let root = ctx.new_node(battle, state);
    ctx.tree.insert(root_key, root);
    ctx.stats.nodes_created += 1;

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
        values.push(ctx.iterate(state, root_depth, root_chain));
    }

    // Every field that the report needs, read before the truncation record
    // borrows the context mutably.
    let (root_visits, root_counts, p1_strategy, p2_strategy) = {
        let root = ctx
            .tree
            .get(&root_key)
            .expect("the first iteration always creates the root node");
        (
            root.visits,
            [
                (root.p1_actions.actions.len(), root.p1_actions.total),
                (root.p2_actions.actions.len(), root.p2_actions.total),
            ],
            // A zero floor, not `EPS`: explicit exploration gives every action a
            // real probability, and a large action set can push that probability
            // below `EPS`.
            strategy_of(&root.p1_actions, &root.p1.average_strategy(), 0.0),
            strategy_of(&root.p2_actions, &root.p2.average_strategy(), 0.0),
        )
    };

    // A root that never widened to its complete set played a subset, exactly as
    // a cap does, so it reports the same warning.
    if let Some(widening) = config.widening {
        // The search chose the last strategy before it incremented `visits`.
        // Use that count so the warning matches the reported average strategy.
        let last_visit = root_visits.saturating_sub(1);
        for (player, (kept, total)) in [
            (Player::P1, root_counts[0]),
            (Player::P2, root_counts[1]),
        ] {
            let allowed = widening.allowed(last_visit, kept);
            if allowed < total {
                ctx.record_truncation(player, allowed, total);
            }
        }
    }

    let value = values.mean().clamp(LOSS, WIN);
    let mut warnings = Vec::new();
    if cancelled && cancel_requested(cancel) {
        warnings.push(SolveWarning::Cancelled);
    }
    if cancel.is_some_and(CancelFlag::simulation_budget_hit)
        && let Some(budget) = cancel.and_then(CancelFlag::simulation_turn_budget)
    {
        warnings.push(SolveWarning::SimulationTurnBudgetExhausted { budget });
    }
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
            // Dependent visits break the independent-sample formula. A batch of
            // two or more members and a universe pool both create that
            // dependence.
            standard_error: match (batch, universes) {
                (Some(2..), _) | (_, Some(_)) => None,
                _ => values.standard_error(),
            },
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
///
/// `control_variate` is [`MctsConfig::control_variate`] for these two learners,
/// so a caller can measure the baseline without the engine.
pub fn learn_matrix_game(
    seed: u64,
    payoffs: &[Vec<f64>],
    iterations: u32,
    policy: SelectionPolicy,
    exploration: f64,
    control_variate: bool,
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

    // This entry point plays the complete matrix, so both learners always see
    // every action.
    for _ in 0..iterations.max(1) {
        let row_strategy = row_learner.strategy(policy, exploration, rows);
        let col_strategy = col_learner.strategy(policy, exploration, cols);
        let row = draw_index(&row_strategy);
        let col = draw_index(&col_strategy);
        let payoff = payoffs[row][col];

        values.push(payoff);
        row_learner.accumulate(&row_strategy);
        col_learner.accumulate(&col_strategy);
        row_learner.update(policy, row, &row_strategy, payoff, control_variate);
        col_learner.update(policy, col, &col_strategy, WIN - payoff, control_variate);
    }

    LearnedMatrix {
        value: values.mean(),
        row_strategy: row_learner.average_strategy(),
        col_strategy: col_learner.average_strategy(),
    }
}

// ── The learners ────────────────────────────────────────────────────────────

/// One player's independent learner at one node.
///
/// [`ismcts`](super::ismcts) reuses this type. A fog-of-war node registers the
/// actions of every world that reached it, and one visit sees only the actions
/// that its own world permits. The `_subset` methods take that index list, so
/// one learner covers the union while each visit plays a subset.
pub(super) struct Learner {
    /// Regret matching holds the cumulative regret of each action.
    /// Exp3 holds the cumulative reward estimate of each action.
    scores: Vec<f64>,
    /// The sum of every played strategy. It gives the average strategy.
    strategy_sum: Vec<f64>,
    /// The running mean reward of each action, and the samples behind each mean.
    ///
    /// The control variate reads them. A learner without the control variate
    /// never writes them, so it keeps its current result.
    baselines: Vec<f64>,
    baseline_counts: Vec<u64>,
}

impl Learner {
    pub(super) fn new(actions: usize) -> Self {
        Learner {
            scores: vec![0.0; actions],
            strategy_sum: vec![0.0; actions],
            baselines: vec![0.0; actions],
            baseline_counts: vec![0; actions],
        }
    }

    /// Add entries until the learner holds `actions` of them.
    ///
    /// A new entry starts at a score of zero, so it starts level with the
    /// entries that the learner already played. A fog-of-war node calls this
    /// when a new world offers an action that no earlier world offered.
    pub(super) fn grow_to(&mut self, actions: usize) {
        if actions <= self.scores.len() {
            return;
        }
        self.scores.resize(actions, 0.0);
        self.strategy_sum.resize(actions, 0.0);
        self.baselines.resize(actions, 0.0);
        self.baseline_counts.resize(actions, 0);
    }

    /// The strategy to play now, mixed with the uniform strategy.
    ///
    /// `allowed` is the action prefix that progressive widening permits. The
    /// returned vector holds that many entries, so a draw from it never returns
    /// a hidden action and never returns a probability of zero.
    fn strategy(&self, policy: SelectionPolicy, exploration: f64, allowed: usize) -> Vec<f64> {
        let actions = allowed.min(self.scores.len());
        if actions == 0 {
            return Vec::new();
        }
        mix_strategy(policy, exploration, &self.scores[..actions])
    }

    /// The strategy over one subset of the actions.
    ///
    /// Entry `i` of the result is the probability of action `allowed[i]`. The
    /// subset is a bandit of its own, so the probabilities total one over the
    /// subset and the explicit exploration bounds each one from below.
    ///
    /// An index outside the learner is a caller error, so this method drops it
    /// rather than growing the learner behind the caller.
    pub(super) fn strategy_subset(
        &self,
        policy: SelectionPolicy,
        exploration: f64,
        allowed: &[usize],
    ) -> Vec<f64> {
        let scores: Vec<f64> = allowed
            .iter()
            .filter_map(|&action| self.scores.get(action).copied())
            .collect();
        if scores.is_empty() {
            return Vec::new();
        }
        mix_strategy(policy, exploration, &scores)
    }

    /// Add one played strategy to the average.
    fn accumulate(&mut self, strategy: &[f64]) {
        for (sum, probability) in self.strategy_sum.iter_mut().zip(strategy) {
            *sum += probability;
        }
    }

    /// Add one played subset strategy to the average.
    ///
    /// An action that the current world did not offer adds nothing, so its share
    /// of the average falls while other worlds play. This is the price of one
    /// strategy over worlds with different action sets.
    pub(super) fn accumulate_subset(&mut self, allowed: &[usize], strategy: &[f64]) {
        self.accumulate_subset_scaled(allowed, strategy, 1.0);
    }

    /// Add one played subset strategy to the average, at a chosen weight.
    ///
    /// [`mccfr`](super::mccfr) weights each visit by the counterfactual reach of
    /// the information set over the sampling probability of the path. A caller
    /// that gives every visit the same weight uses [`Learner::accumulate_subset`]
    /// instead.
    pub(super) fn accumulate_subset_scaled(
        &mut self,
        allowed: &[usize],
        strategy: &[f64],
        weight: f64,
    ) {
        for (slot, &action) in allowed.iter().enumerate() {
            if let (Some(sum), Some(probability)) =
                (self.strategy_sum.get_mut(action), strategy.get(slot))
            {
                *sum += weight * probability;
            }
        }
    }

    /// Add one regret to each action of a subset.
    ///
    /// Entry `i` of `regrets` belongs to action `allowed[i]`. An index outside
    /// the learner is a caller error, so this method drops it rather than growing
    /// the learner behind the caller.
    ///
    /// Counterfactual regret minimization computes the complete regret vector of
    /// a node before it writes anything, so it needs this entry point instead of
    /// [`Learner::update_subset`].
    pub(super) fn add_regrets_subset(&mut self, allowed: &[usize], regrets: &[f64]) {
        for (slot, &action) in allowed.iter().enumerate() {
            if let (Some(score), Some(regret)) = (self.scores.get_mut(action), regrets.get(slot)) {
                *score += regret;
            }
        }
    }

    /// Learn from the payoff of the played action.
    ///
    /// `strategy` is the strategy that this learner played, so its length is the
    /// action prefix that progressive widening permitted. An action outside that
    /// prefix keeps a score of zero, so it starts level with the played actions
    /// once the prefix reaches it.
    ///
    /// The selection probability of the played action is never zero, because the
    /// explicit exploration bounds it from below.
    ///
    /// `control_variate` subtracts the running mean of an action before the
    /// division and adds it back outside. Read the module documentation for the
    /// estimator and for the reason that it stays unbiased.
    fn update(
        &mut self,
        policy: SelectionPolicy,
        played: usize,
        strategy: &[f64],
        reward: f64,
        control_variate: bool,
    ) {
        let actions = strategy.len().min(self.scores.len());
        if played >= actions {
            return;
        }
        // The complete prefix is the identity subset, so this call runs the same
        // arithmetic in the same order that this method ran before the subset
        // form existed.
        let allowed: Vec<usize> = (0..actions).collect();
        self.update_subset(
            policy,
            played,
            &allowed,
            &strategy[..actions],
            reward,
            control_variate,
        );
    }

    /// Learn from the payoff of the played action, over one action subset.
    ///
    /// `allowed` names the actions that this visit could play, and `strategy`
    /// holds their probabilities in the same order. `played` is the position in
    /// `allowed` of the action that the visit played.
    ///
    /// An action outside `allowed` keeps its score, so it starts level with the
    /// played actions the next time a world offers it.
    ///
    /// Read [`Learner::update`] for the estimator itself. The two methods run
    /// the same arithmetic, and this one reads every score through `allowed`.
    pub(super) fn update_subset(
        &mut self,
        policy: SelectionPolicy,
        played: usize,
        allowed: &[usize],
        strategy: &[f64],
        reward: f64,
        control_variate: bool,
    ) {
        let actions = strategy.len().min(allowed.len());
        if played >= actions || allowed.iter().any(|&action| action >= self.scores.len()) {
            return;
        }
        let played_action = allowed[played];
        // The baseline of the played action, read before this sample enters it.
        // A baseline that held the current sample would correlate with it and
        // would bias the estimate.
        let baseline = if control_variate {
            self.baselines[played_action]
        } else {
            0.0
        };
        // The payoff of the played action alone, scaled so that its expectation
        // over the played strategy is the payoff of that action.
        let played_value = baseline + (reward - baseline) / strategy[played];
        // The value of an action that this iteration did not play. Without a
        // baseline it is zero, and the estimate of the played action then carries
        // the whole payoff.
        let values: Vec<f64> = if control_variate {
            (0..actions)
                .map(|slot| {
                    if slot == played {
                        played_value
                    } else {
                        self.baselines[allowed[slot]]
                    }
                })
                .collect()
        } else {
            Vec::new()
        };
        let value_of = |slot: usize| match (control_variate, slot == played) {
            (true, _) => values[slot],
            (false, true) => played_value,
            (false, false) => 0.0,
        };

        match policy {
            SelectionPolicy::RegretMatching => {
                // The value of the node under the played strategy. Without a
                // baseline the weighted sum is the reward itself, so this branch
                // keeps the arithmetic that the search already used.
                let node_value: f64 = if control_variate {
                    (0..actions).map(|slot| values[slot] * strategy[slot]).sum()
                } else {
                    reward
                };
                for (slot, &action) in allowed[..actions].iter().enumerate() {
                    self.scores[action] += value_of(slot) - node_value;
                }
            }
            // Without a baseline every unplayed action adds zero, so this policy
            // only has to touch the played action.
            SelectionPolicy::Exp3 if !control_variate => self.scores[played_action] += played_value,
            SelectionPolicy::Exp3 => {
                for (slot, &action) in allowed[..actions].iter().enumerate() {
                    self.scores[action] += value_of(slot);
                }
            }
        }

        if control_variate {
            // The next visit reads this sample as part of the baseline.
            self.baseline_counts[played_action] += 1;
            let count = self.baseline_counts[played_action] as f64;
            self.baselines[played_action] += (reward - self.baselines[played_action]) / count;
        }
    }

    /// The strategy that the node played on average.
    pub(super) fn average_strategy(&self) -> Vec<f64> {
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

    /// The total weight of the strategies in the average.
    pub(super) fn average_weight(&self) -> f64 {
        self.strategy_sum.iter().sum()
    }
}

/// Turn one score list into a strategy, mixed with the uniform strategy.
///
/// The result holds one probability for each score, and the probabilities total
/// one. The explicit exploration bounds every probability from below, which the
/// importance-weighted updates divide by.
fn mix_strategy(policy: SelectionPolicy, exploration: f64, scores: &[f64]) -> Vec<f64> {
    let actions = scores.len();
    if actions == 0 {
        return Vec::new();
    }
    let uniform = 1.0 / actions as f64;

    let weights: Vec<f64> = match policy {
        SelectionPolicy::RegretMatching => scores.iter().map(|r| r.max(0.0)).collect(),
        SelectionPolicy::Exp3 => {
            // The learning rate of Exp3 over `actions` arms. The subtracted
            // maximum keeps the exponential finite, and it cancels in the
            // normalization below.
            let rate = exploration / actions as f64;
            let highest = scores.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            scores.iter().map(|s| (rate * (s - highest)).exp()).collect()
        }
    };

    let total: f64 = weights.iter().sum();
    // Regret matching plays uniformly while every regret stays at or below
    // zero. Exp3 weights are always positive, so only regret matching reaches
    // this branch.
    let base: Vec<f64> = if total > EPS {
        weights.iter().map(|w| w / total).collect()
    } else {
        vec![uniform; actions]
    };

    base.iter()
        .map(|p| (1.0 - exploration) * p + exploration * uniform)
        .collect()
}

// ── The tree ────────────────────────────────────────────────────────────────

/// A position, its search horizon, and its forced-chain counter.
///
/// The key holds a hash of the position rather than the position itself, as the
/// transposition table of the exact search does. A hash collision merges two
/// positions. Each node holds its own action lists, so a merge changes a value
/// and can never index outside a list.
type NodeKey = (u64, u8, u8);

/// One chance node: a decision point and one joint action of each player.
///
/// The two indexes address the action lists of the node that [`NodeKey`] names,
/// so one key stands for one turn under one command pair.
type ChanceKey = (NodeKey, usize, usize);

/// Where one chance node stands in its stratified batch.
struct BatchCursor {
    /// The seed of the plan of the current batch.
    seed: u64,
    /// The members that this node already resolved, from zero.
    used: usize,
}

/// The batch position of every chance node that the search reached.
///
/// The enumerated transition mode leaves this empty.
#[derive(Default)]
struct BatchCursors(HashMap<ChanceKey, BatchCursor>);

impl BatchCursors {
    /// The plan seed and the member index that this visit of `key` uses.
    ///
    /// The cursor advances by one member for each call. A cursor that used every
    /// member of its batch draws a new seed. The next group of visits then covers
    /// the dimensions again from a fresh plan.
    ///
    /// Each seed comes from the seeded stream of the search, so one search seed
    /// still gives one result.
    fn next_member(&mut self, key: ChanceKey, batch: usize) -> (u64, usize) {
        let cursor = self.0.entry(key).or_insert_with(|| BatchCursor {
            seed: draw_seed(),
            used: 0,
        });
        let member = (cursor.seed, cursor.used);
        cursor.used += 1;
        // The last member closes the batch. A `batch` of zero cannot reach this
        // type, because the caller keeps the independent draw below two members.
        if cursor.used >= batch {
            cursor.used = 0;
            cursor.seed = draw_seed();
        }
        member
    }
}

/// One decision point, with one learner for each player.
struct Node {
    p1: Learner,
    p2: Learner,
    p1_actions: JointActions,
    p2_actions: JointActions,
    /// Iterations that played an action here.
    /// Progressive widening reads it, and each player derives its own allowed
    /// count from it.
    visits: u64,
    /// The common random numbers for this node.
    universes: UniversePool,
}

/// The common universe seeds and the resolution index of each action pair.
struct UniversePool {
    seeds: Vec<u64>,
    pair_visits: HashMap<(usize, usize), u64>,
}

impl UniversePool {
    /// Create a pool from the seeded search stream.
    fn new(count: Option<usize>) -> Self {
        let seeds = count.map_or_else(Vec::new, |count| (0..count).map(|_| draw_seed()).collect());
        UniversePool {
            seeds,
            pair_visits: HashMap::new(),
        }
    }

    /// Get the universe for the next resolution of one action pair.
    fn next_seed(&mut self, pair: (usize, usize)) -> Option<u64> {
        if self.seeds.is_empty() {
            return None;
        }
        let visits = self.pair_visits.entry(pair).or_default();
        let index = (*visits % self.seeds.len() as u64) as usize;
        *visits += 1;
        Some(self.seeds[index])
    }
}

struct MctsContext<'a> {
    pokemon_dex: &'a HashMap<Species, PokemonData>,
    move_dex: &'a HashMap<PokemonMove, MoveData>,
    cfg: &'a MctsConfig,
    /// The exploration rate after the zero check.
    exploration: f64,
    /// The stratified batch after the iteration-count limit.
    batch: Option<usize>,
    /// The universe pool of each node. `None` keeps the independent draw.
    universes: Option<usize>,
    tree: HashMap<NodeKey, Node>,
    chance_cursors: BatchCursors,
    stats: MctsStats,
    /// Largest fraction of outcome probability dropped at any one chance node.
    max_discarded: f64,
    /// Largest action-set truncation for each player anywhere in the tree.
    action_truncations: [Option<(usize, usize)>; 2],
    cancel: Option<&'a CancelFlag>,
}

impl MctsContext<'_> {
    /// Scores one position with the configured leaf evaluator.
    ///
    /// The evaluator reads the move dex, so every call site builds the same
    /// context here instead of assembling one of its own.
    fn score(&self, battle: &BattleState) -> f64 {
        (self.cfg.eval)(battle, &self.eval_context())
    }

    /// The context that the evaluator and the policy both read.
    fn eval_context(&self) -> EvalContext<'_> {
        EvalContext::new(self.pokemon_dex, self.move_dex)
    }

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
            return self.score(battle);
        }

        let key = (hash_state(state), depth, chain);
        if !self.tree.contains_key(&key) {
            let node = self.new_node(battle, state);
            self.tree.insert(key, node);
            self.stats.nodes_created += 1;
            // A new node holds no experience, so it has no strategy to play.
            // The static score goes back to the parent, and the next visit
            // plays an action here.
            return self.score(battle);
        }

        let (p1_strategy, p2_strategy, p1_index, p2_index, p1_commands, p2_commands) = {
            let node = &self.tree[&key];
            // The visit count of this node before this visit. A first visit
            // therefore plays the initial prefix.
            let p1_allowed = self.allowed(node.visits, node.p1_actions.actions.len());
            let p2_allowed = self.allowed(node.visits, node.p2_actions.actions.len());
            let p1_strategy = node.p1.strategy(self.cfg.policy, self.exploration, p1_allowed);
            let p2_strategy = node.p2.strategy(self.cfg.policy, self.exploration, p2_allowed);
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

        // The resolution uses this action pair's next common universe. The guard
        // drops before the descent and restores the search stream.
        let successor = {
            let _universe = self.universe_stream(&key, (p1_index, p2_index));
            let branches =
                self.resolve(state, (key, p1_index, p2_index), &p1_commands, &p2_commands);
            draw_successor(branches)
        };
        let value = match successor {
            Some(child) => {
                let (child_depth, child_chain) = self.descend(&child, depth, chain);
                self.iterate(&child, child_depth, child_chain)
            }
            // The engine returned no outcome. Score the position instead.
            None => self.score(battle),
        };

        let node = self
            .tree
            .get_mut(&key)
            .expect("the node exists for the whole iteration");
        node.visits += 1;
        node.p1.accumulate(&p1_strategy);
        node.p2.accumulate(&p2_strategy);
        node.p1.update(
            self.cfg.policy,
            p1_index,
            &p1_strategy,
            value,
            self.cfg.control_variate,
        );
        // The game is zero-sum over `[0, 1]`, so P2 maximizes the complement.
        node.p2.update(
            self.cfg.policy,
            p2_index,
            &p2_strategy,
            WIN - value,
            self.cfg.control_variate,
        );
        value
    }

    /// Install the next common universe for one action pair.
    ///
    /// `None` means that the node holds no universe pool, so the visit keeps the
    /// stream of the search itself.
    fn universe_stream(&mut self, key: &NodeKey, pair: (usize, usize)) -> Option<SampleRngGuard> {
        let seed = self.tree.get_mut(key)?.universes.next_seed(pair)?;
        Some(scoped_sample_rng(seed))
    }

    /// The actions that a node with `visits` visits may play.
    ///
    /// A configuration without widening returns the complete count, so every
    /// learner sees every action from the first visit.
    fn allowed(&self, visits: u64, total: usize) -> usize {
        match self.cfg.widening {
            Some(widening) => widening.allowed(visits, total),
            None => total,
        }
    }

    /// Build the node of a position that the tree does not hold yet.
    fn new_node(&mut self, battle: &BattleState, state: &MatchState) -> Node {
        let phase = actions::phase_of(state);
        let mut p1_actions = self.joint_actions(battle, Player::P1, phase);
        let mut p2_actions = self.joint_actions(battle, Player::P2, phase);
        // The allowed set is a prefix of the list, so the order decides which
        // choices a narrow prefix holds. A search without widening keeps the
        // generated order, and therefore keeps its current results.
        if self.cfg.widening.is_some() {
            if self.cfg.policy_prior {
                let ctx = self.eval_context();
                let weights = eval::fitted_policy_weights();
                reorder_by_policy(&mut p1_actions, battle, Player::P1, &ctx, weights);
                reorder_by_policy(&mut p2_actions, battle, Player::P2, &ctx, weights);
            } else {
                reorder_by_coverage(&mut p1_actions);
                reorder_by_coverage(&mut p2_actions);
            }
        }
        // The pool uses the search stream. Thus, one search seed gives one result.
        let universes = UniversePool::new(self.universes);
        Node {
            p1: Learner::new(p1_actions.actions.len()),
            p2: Learner::new(p2_actions.actions.len()),
            p1_actions,
            p2_actions,
            visits: 0,
            universes,
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
            self.record_truncation(player, joint.actions.len(), joint.total);
        }
        joint
    }

    /// Record the largest action-set reduction of one player.
    ///
    /// A cap and progressive widening both reach this method, because both make
    /// a strategy a distribution over a subset of the legal actions.
    fn record_truncation(&mut self, player: Player, kept: usize, total: usize) {
        let slot = match player {
            Player::P1 => 0,
            Player::P2 => 1,
        };
        let candidate = (kept, total);
        let dropped = |(kept, total): (usize, usize)| total.saturating_sub(kept);
        if self.action_truncations[slot].is_none_or(|previous| dropped(candidate) > dropped(previous))
        {
            self.action_truncations[slot] = Some(candidate);
        }
    }

    /// Resolve one joint action into the successors that the configured
    /// [`TransitionMode`] offers the draw.
    ///
    /// `chance_key` names the chance node of this resolution. Only the generative
    /// mode reads it, and it uses the key to place the resolution in a batch.
    fn resolve(
        &mut self,
        state: &MatchState,
        chance_key: ChanceKey,
        p1_commands: &[BattleCommand],
        p2_commands: &[BattleCommand],
    ) -> Vec<(MatchState, f64)> {
        if self
            .cancel
            .is_some_and(|control| !control.claim_simulation_turn())
        {
            return Vec::new();
        }
        self.stats.turns_simulated += 1;
        let chance = match self.cfg.transition {
            TransitionMode::Enumerated(chance) => chance,
            TransitionMode::Generative { .. } => {
                let batch = self.batch.expect("the generative mode sets a batch");
                return self.resolve_generative(state, chance_key, batch, p1_commands, p2_commands);
            }
        };
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

        let (kept, discarded) = chance.apply(sorted);
        if discarded > self.max_discarded {
            self.max_discarded = discarded;
        }
        kept
    }

    /// Sample one successor inside turn resolution.
    ///
    /// The single returned branch is the successor itself, so the caller's draw
    /// is a no-op. Its weight is one because the branch set holds the whole
    /// outcome mass: the trajectory probability of the sample describes the path
    /// that produced the successor, not the share of the successors that it
    /// stands for.
    ///
    /// The engine draws from the seeded RNG of the search, so one seed still
    /// gives one result. The successor sort of the enumerated mode is unneeded
    /// here, because no `HashMap` drain decides the order of a single branch.
    ///
    /// A `batch` above one places the resolution in the stratified batch of
    /// `chance_key`. One member keeps the law of one independent draw, so the
    /// single returned branch keeps its weight of one.
    fn resolve_generative(
        &mut self,
        state: &MatchState,
        chance_key: ChanceKey,
        batch: usize,
        p1_commands: &[BattleCommand],
        p2_commands: &[BattleCommand],
    ) -> Vec<(MatchState, f64)> {
        let sample = if batch > 1 {
            let (seed, member) = self.chance_cursors.next_member(chance_key, batch);
            let plan = StratifiedPlan::new(batch, seed);
            // The guard must outlive the resolution, and it restores the
            // previous stream when this scope ends.
            let _stream = plan.install(member);
            self.sample_one(state, p1_commands, p2_commands)
        } else {
            self.sample_one(state, p1_commands, p2_commands)
        };
        vec![(sample.state, 1.0)]
    }

    /// Resolve one turn with the generative model, under the installed stream.
    fn sample_one(
        &self,
        state: &MatchState,
        p1_commands: &[BattleCommand],
        p2_commands: &[BattleCommand],
    ) -> TransitionSample {
        sample_transition(
            state,
            &PlayerCommand::Battle(p1_commands.to_vec()),
            &PlayerCommand::Battle(p2_commands.to_vec()),
            self.move_dex,
            self.pokemon_dex,
            TransitionConfig {
                consider_crit: self.cfg.consider_crit,
                damage_rolls: self.cfg.damage_rolls,
                // The search reads no events, and tracking them would cost time.
                observe: false,
            },
        )
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
            self.cfg.max_forced_chain,
            self.cfg.replacement_depth,
        )
    }
}

// ── Shared plumbing ─────────────────────────────────────────────────────────

/// Put the actions of one player in the coverage order.
///
/// Progressive widening plays a prefix of the list, so the order decides what a
/// narrow prefix holds. [`actions::coverage_order`] returns a permutation, so
/// this rewrite drops no action and `total` stays correct.
fn reorder_by_coverage(joint: &mut JointActions) {
    let order = actions::coverage_order(&joint.actions);
    apply_order(joint, &order);
}

/// Put the actions of one player in the policy order.
///
/// The highest policy score comes first, so a narrow prefix holds the actions
/// that the policy likes. A score tie keeps the generated order, which keeps
/// the result stable inside one process.
///
/// This rewrite is a permutation, so it drops no action and `total` stays
/// correct.
fn reorder_by_policy(
    joint: &mut JointActions,
    battle: &BattleState,
    player: Player,
    ctx: &EvalContext<'_>,
    weights: &PolicyFeatures,
) {
    let scores: Vec<f64> = joint
        .actions
        .iter()
        .map(|action| eval::policy_score(battle, player, action, ctx, weights))
        .collect();
    let mut order: Vec<usize> = (0..joint.actions.len()).collect();
    order.sort_by(|&left, &right| {
        scores[right]
            .total_cmp(&scores[left])
            .then_with(|| left.cmp(&right))
    });
    apply_order(joint, &order);
}

/// Rewrite an action list into the given permutation.
fn apply_order(joint: &mut JointActions, order: &[usize]) {
    let reordered: Vec<Vec<BattleCommand>> = order
        .iter()
        .map(|&index| joint.actions[index].clone())
        .collect();
    joint.actions = reordered;
}

/// The mean and the standard error of a value stream.
///
/// Welford's method holds three numbers instead of every value, which keeps the
/// memory constant in the iteration count.
#[derive(Debug, Clone, Default)]
pub(super) struct RunningStats {
    pub(super) count: u64,
    mean: f64,
    sum_of_squares: f64,
}

impl RunningStats {
    pub(super) fn push(&mut self, value: f64) {
        self.count += 1;
        let first_delta = value - self.mean;
        self.mean += first_delta / self.count as f64;
        self.sum_of_squares += first_delta * (value - self.mean);
    }

    pub(super) fn mean(&self) -> f64 {
        self.mean
    }

    /// The standard error of the mean. One sample gives `None`, because the
    /// sample variance divides by the count minus one.
    pub(super) fn standard_error(&self) -> Option<f64> {
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
pub(super) fn draw_index(strategy: &[f64]) -> usize {
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

/// Draw one plan seed from the seeded stream of the search.
fn draw_seed() -> u64 {
    with_sample_rng(|rng| rng.r#gen::<u64>())
}

/// Get the batch that the search can use.
///
/// No chance node can have more visits than the complete search. This limit
/// prevents plans from allocating members that no visit can use.
fn effective_batch(transition: TransitionMode, iterations: u32) -> Option<usize> {
    match transition {
        TransitionMode::Enumerated(_) => None,
        TransitionMode::Generative { batch } => Some(batch.min(iterations as usize)),
    }
}

/// Get the universe pool that each node holds.
///
/// A node cannot use more universes than the search has iterations.
/// A pool of zero keeps independent draws.
fn effective_universes(config: &MctsConfig, iterations: u32) -> Option<usize> {
    config
        .common_random_numbers
        .map(|count| count.min(iterations as usize))
        .filter(|&count| count > 0)
}

/// Draw one successor by weight, and drop the rest.
fn draw_successor(branches: Vec<(MatchState, f64)>) -> Option<MatchState> {
    sample_one_branch(branches)
        .pop()
        .map(|(child, _probability)| child)
}

pub(super) fn terminal_value(winner: Player) -> f64 {
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
        let strategy = learner.strategy(SelectionPolicy::RegretMatching, 0.1, 4);
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
            let strategy = learner.strategy(policy, 0.3, 3);
            let total: f64 = strategy.iter().sum();
            assert!((total - 1.0).abs() < 1e-9, "{policy:?} sums to {total}");
            for probability in &strategy {
                assert!(*probability >= 0.1 - 1e-9, "{policy:?} gave {probability}");
            }
        }
    }

    /// A widened learner plays a prefix. The hidden entries must keep their
    /// score, so they start level with the played entries later.
    #[test]
    fn a_widened_update_leaves_the_hidden_scores_alone() {
        let mut learner = Learner::new(4);
        let strategy = learner.strategy(SelectionPolicy::RegretMatching, 0.1, 2);
        assert_eq!(strategy.len(), 2);

        learner.update(SelectionPolicy::RegretMatching, 0, &strategy, 1.0, false);

        assert!(learner.scores[0] > 0.0, "{:?}", learner.scores);
        assert_eq!(learner.scores[2], 0.0);
        assert_eq!(learner.scores[3], 0.0);
    }

    /// A fog-of-war node registers every action of every world, and one visit
    /// plays a subset. The actions outside that subset must keep their score,
    /// so they start level when a later world offers them.
    #[test]
    fn a_subset_update_leaves_the_unavailable_scores_alone() {
        let mut learner = Learner::new(2);
        learner.grow_to(4);
        // This world offers actions 1 and 3 only.
        let allowed = vec![1, 3];
        let strategy = learner.strategy_subset(SelectionPolicy::RegretMatching, 0.1, &allowed);
        assert_eq!(strategy.len(), 2);

        learner.update_subset(
            SelectionPolicy::RegretMatching,
            0,
            &allowed,
            &strategy,
            1.0,
            false,
        );

        assert_eq!(learner.scores[0], 0.0, "{:?}", learner.scores);
        assert_eq!(learner.scores[2], 0.0, "{:?}", learner.scores);
        assert!(learner.scores[1] > 0.0, "{:?}", learner.scores);
        assert!(learner.scores[3] < 0.0, "{:?}", learner.scores);
    }

    /// `grow_to` must keep what the learner already learned, and it must start
    /// each new entry level with the others.
    #[test]
    fn growing_a_learner_keeps_the_earlier_scores() {
        let mut learner = Learner::new(2);
        learner.scores = vec![3.0, -1.0];
        learner.strategy_sum = vec![5.0, 2.0];
        learner.grow_to(4);

        assert_eq!(learner.scores, vec![3.0, -1.0, 0.0, 0.0]);
        assert_eq!(learner.strategy_sum, vec![5.0, 2.0, 0.0, 0.0]);
        // A smaller request must never shrink the learner.
        learner.grow_to(1);
        assert_eq!(learner.scores.len(), 4);
    }

    /// The subset form and the complete form must agree on the identity subset.
    /// The complete form calls the subset form, and this test holds that
    /// contract.
    #[test]
    fn a_complete_subset_matches_the_plain_update() {
        let strategy = vec![0.25, 0.75];
        for policy in [SelectionPolicy::RegretMatching, SelectionPolicy::Exp3] {
            for control_variate in [false, true] {
                let mut plain = Learner::new(2);
                let mut subset = Learner::new(2);
                plain.update(policy, 1, &strategy, 0.5, control_variate);
                subset.update_subset(policy, 1, &[0, 1], &strategy, 0.5, control_variate);
                assert_eq!(plain.scores, subset.scores, "{policy:?} {control_variate}");
                assert_eq!(plain.baselines, subset.baselines);
            }
        }
    }

    /// The baseline must hold no part of the sample that it corrects. A learner
    /// that read the updated mean would subtract the current draw from itself.
    #[test]
    fn a_baseline_holds_only_the_earlier_samples() {
        let mut learner = Learner::new(2);
        let strategy = vec![0.5, 0.5];

        learner.update(SelectionPolicy::RegretMatching, 0, &strategy, 1.0, true);
        // The first sample met a baseline of zero, so the estimate is the plain
        // importance weight.
        assert!((learner.scores[0] - 1.0).abs() < 1e-12, "{:?}", learner.scores);
        assert!((learner.baselines[0] - 1.0).abs() < 1e-12);

        let first_score = learner.scores[0];
        learner.update(SelectionPolicy::RegretMatching, 0, &strategy, 0.0, true);
        // The second sample met a baseline of one: 1 + (0 - 1) / 0.5 = -1. The
        // unplayed action never held a reward, so its baseline is still zero.
        let played_value = 1.0 + (0.0 - 1.0) / 0.5;
        let node_value = 0.5 * played_value + 0.5 * 0.0;
        assert!(
            (learner.scores[0] - (first_score + played_value - node_value)).abs() < 1e-12,
            "{:?}",
            learner.scores
        );
        assert!((learner.baselines[0] - 0.5).abs() < 1e-12);
    }

    /// A learner without the control variate must keep every current result, so
    /// its update has to stay the plain importance weight.
    #[test]
    fn an_update_without_a_baseline_keeps_the_importance_weight() {
        let strategy = vec![0.25, 0.75];
        // The estimate of the played action is 0.5 / 0.25. Regret matching then
        // subtracts the node value, which is the reward itself.
        let expected = [
            (SelectionPolicy::RegretMatching, [2.0 - 0.5, -0.5]),
            (SelectionPolicy::Exp3, [2.0, 0.0]),
        ];
        for (policy, scores) in expected {
            let mut learner = Learner::new(2);
            learner.update(policy, 0, &strategy, 0.5, false);
            assert_eq!(learner.scores, scores, "{policy:?}");
            assert_eq!(learner.baselines, vec![0.0, 0.0], "{policy:?}");
        }
    }

    #[test]
    fn a_universe_pool_aligns_action_pairs_by_resolution_index() {
        let mut pool = UniversePool {
            seeds: vec![11, 22],
            pair_visits: HashMap::new(),
        };

        assert_eq!(pool.next_seed((0, 0)), Some(11));
        assert_eq!(pool.next_seed((0, 0)), Some(22));
        assert_eq!(pool.next_seed((1, 0)), Some(11));
        assert_eq!(pool.next_seed((1, 0)), Some(22));
        assert_eq!(pool.next_seed((0, 0)), Some(11));
    }

    #[test]
    fn a_universe_pool_cannot_exceed_the_iteration_count() {
        let off = MctsConfig::default();
        assert_eq!(effective_universes(&off, 4), None);
        assert_eq!(
            effective_universes(
                &MctsConfig {
                    common_random_numbers: Some(0),
                    ..off
                },
                4
            ),
            None
        );
        assert_eq!(
            effective_universes(
                &MctsConfig {
                    common_random_numbers: Some(usize::MAX),
                    ..off
                },
                4
            ),
            Some(4)
        );
    }

    /// A large cumulative reward must not overflow the exponential.
    #[test]
    fn exp3_survives_a_large_score() {
        let mut learner = Learner::new(2);
        learner.scores = vec![1e6, 0.0];
        let strategy = learner.strategy(SelectionPolicy::Exp3, 0.1, 2);
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
    fn a_batch_cannot_exceed_the_iteration_count() {
        assert_eq!(
            effective_batch(TransitionMode::Generative { batch: usize::MAX }, 7),
            Some(7)
        );
        assert_eq!(
            effective_batch(TransitionMode::Generative { batch: 0 }, 7),
            Some(0)
        );
        assert_eq!(
            effective_batch(TransitionMode::Enumerated(ChanceMode::Enumerate), 7),
            None
        );
    }

    /// One batch spans consecutive visits of one chance node. Every member must
    /// therefore read the same plan. The visit after the last member must start
    /// a new plan.
    #[test]
    fn a_chance_cursor_walks_one_batch_before_it_reseeds() {
        let _guard = scoped_sample_rng(5);
        let mut cursors = BatchCursors::default();
        let key = ((7, 1, 0), 0, 0);

        let batch: Vec<(u64, usize)> = (0..3).map(|_| cursors.next_member(key, 3)).collect();
        let members: Vec<usize> = batch.iter().map(|(_, member)| *member).collect();
        assert_eq!(members, vec![0, 1, 2]);
        assert!(
            batch.iter().all(|(seed, _)| *seed == batch[0].0),
            "one batch used two plans: {batch:?}"
        );

        let next = cursors.next_member(key, 3);
        assert_eq!(next.1, 0, "the fourth visit must open a new batch");
        assert_ne!(next.0, batch[0].0, "the new batch must use a new plan");
    }

    /// The dimension of a chokepoint depends on the command pair, so two command
    /// pairs of one position must not share a batch.
    #[test]
    fn two_action_pairs_hold_two_cursors() {
        let _guard = scoped_sample_rng(5);
        let mut cursors = BatchCursors::default();
        let position = (7, 1, 0);

        let left = cursors.next_member((position, 0, 0), 4);
        let right = cursors.next_member((position, 0, 1), 4);

        assert_eq!((left.1, right.1), (0, 0), "both pairs start their own batch");
        assert_ne!(left.0, right.0, "both pairs drew one plan seed");
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
