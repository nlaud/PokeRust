//! Solves perfect-information battle positions.
//!
//! The input is a concrete `MatchState` with two known teams.
//! The solver searches to a fixed depth.
//! It returns each optimal mixed strategy and P1's win probability.
//!
//! # Iterative deepening
//!
//! [`SolveConfig::iterative_deepening`] searches depth 1 first, then each depth
//! up to [`SolveConfig::depth`].
//! Each pass completes before the next pass starts.
//! The solver returns the last complete pass, and reports its depth in
//! [`SolveResult::depth_reached`].
//! A partial pass is returned only when no pass finished.
//!
//! # Team preview
//!
//! [`solve`] refuses a preview state.
//! [`preview::solve_team_preview`] solves that state instead.
//! It runs double oracle over the bring-and-lead choices, and it uses [`solve`]
//! for each battle below a preview cell.
//!
//! # Open-list team preview
//!
//! An open-list tournament hides the numeric stats of both teams.
//! [`preview::solve_open_list_preview`] takes that belief instead of a state.
//! It draws concrete worlds with the determinizer, and it solves the mean payoff
//! matrix of those worlds.
//! The result holds one strategy pair and the sampling error of the value.
//!
//! # Why not minimax
//!
//! Both players select commands without seeing the other commands.
//! The engine then returns a distribution of successor states.
//! Minimax incorrectly lets one player respond to the other player's command.
//! The solver instead computes each node's Nash equilibrium.
//! Thus, the result gives a probability for each command.
//!
//! # Algorithms
//!
//! The implementation follows Bošanský et al., Artificial Intelligence 237, 2016.
//! [`SolverAlgorithm`] describes the three variants.
//!
//! `simulate_turn` supplies the transition probability for each successor.
//!
//! # Exact search and sampling search
//!
//! [`solve`] is exact for its depth and its [`ChanceMode`].
//! Every [`SolverAlgorithm`] variant returns the same value, and the tests
//! compare them.
//!
//! [`mcts::search`] samples instead.
//! It reaches a greater depth, and it returns an estimate with a sampling error.
//! A sampled value depends on the seed, so this search is not a
//! [`SolverAlgorithm`] variant.
//! The exact search stays the oracle of the sampling tests.
//!
//! # Fog of war
//!
//! [`solve`] and [`mcts::search`] both need a concrete `MatchState`.
//! [`ismcts::search_belief`] takes an `UnknownBattleState` instead.
//! It is the fast heuristic baseline of the fog-of-war solver.
//!
//! [`belief::ParticleBelief`] holds the worlds that the belief permits, one
//! weight for each world, the posterior update, the effective sample-size check,
//! and the resampling step.
//! [`belief::ObservationKey`] is the observation model. It hashes one player's
//! private Pokemon state, commands, and the masked event stream that
//! [`mask_events_for`](crate::information::information::mask_events_for) built
//! for that player.
//!
//! [`ismcts::search`] keys its nodes by that type, never by a `MatchState` hash.
//! Two hidden worlds that one player cannot tell apart therefore share one node.
//! [`infoset`] holds the node itself, and both fog-of-war searches read it.
//!
//! # The equilibrium baseline
//!
//! Both ISMCTS trees learn against the same sampled opponent, so the pair
//! converges to a self-consistent strategy rather than to an equilibrium.
//!
//! [`mccfr::search`] runs outcome-sampling counterfactual regret minimization
//! over the same information sets.
//! It alternates the traverser, and it returns an average strategy for each
//! player.
//! [`mccfr::MccfrResult::horizon`] also holds the counterfactual value of each
//! public belief at the depth limit, which a later public-belief solve reads as
//! its leaf input.
//!
//! [`mccfr::search_with_leaves`] reads supplied information-set values at the
//! depth limit instead of the leaf evaluator.
//! [`mccfr::MccfrConfig::horizon_worlds`] keeps the worlds of each public belief
//! at that limit. The continual solver retains their private histories.
//! [`mccfr::continual_solve`] runs both halves: it solves each public belief of
//! a first pass, and it then solves the root again against those values.
//!
//! # Exploitability
//!
//! [`MctsConfig::widening`](mcts::MctsConfig::widening) lets a node play a
//! prefix of its action list, so its strategy can lose to an action that the
//! node never played.
//! [`exploit::exploitability`] measures that loss.
//! It builds the complete action set of both players, and it answers each
//! strategy with an exact best response over that set.
//! A test of an approximate search compares gaps, never the subset that the
//! search itself played.
//!
//! # Opponent exploitation
//!
//! [`solve`] returns a Nash strategy, and that stays the default of every
//! search.
//! A Nash strategy holds its value against every opponent, and it takes nothing
//! extra from a weak opponent.
//!
//! [`exploit::respond`] answers a known opponent model instead.
//! The opponent plays that model with a supplied confidence, and plays freely
//! with the rest of the mass.
//! A confidence of zero returns the Nash strategy.
//! A confidence of one returns a pure best response.
//!
//! The answer loses the Nash guarantee, and
//! [`exploit::ResponseReport::budget_spent`] names that price.
//! [`exploit::respond_within_budget`] holds the price under a supplied limit.
//! Neither function changes [`SolveConfig`], and neither one changes the result
//! of [`solve`].
//!
//! # Cancellation
//!
//! [`CancelFlag`] stops a search that already runs.
//! Every search reads the flag during the search, and a set flag ends the work
//! at the next safe point.
//! A cancelled search returns an answer, never an error.
//!
//! [`solve_seeded_cancellable`] returns the last complete deepening pass.
//! [`mcts::search_cancellable`], [`ismcts::search_cancellable`], and
//! [`mccfr::search_cancellable`] return the mean and the strategy of the
//! finished iterations alone.
//! Each cancelled answer carries [`SolveWarning::Cancelled`].
//!
//! The flag is a separate argument, not a [`SolveConfig`] field.
//! `SolveConfig` is `Copy`, and an `Arc` field would remove that.
//!
//! The exact search also carries the flag and the deadline into the simulator.
//! One turn simulation is the largest unit of work in a solve, so a stop between
//! two units is not enough on its own.
//! `search::resolve` installs a simulator abort signal for each turn, and a cell
//! whose simulation aborts takes a static score.
//! Read the `search` module documentation for the rule.
//!
//! # Utilities are win probabilities
//!
//! Each utility is P1's win probability in `[0, 1]`.
//! P1 maximizes the value, and P2 minimizes it.
//! This range supplies valid bounds for star1 and serialized alpha-beta pruning.
//!
//! # Cost
//!
//! `simulate_turn` causes most search cost.
//! [`SolveStats::turns_simulated`] measures this work.
//! [`ChanceMode`] limits the outcomes that the search examines.
//!
//! ```no_run
//! # use poke_rust::data::pokemon_move::PokemonMove;
//! # use poke_rust::data::species::Species;
//! # use poke_rust::solver::{solve, SolveConfig, chance::ChanceMode};
//! # use poke_rust::state::battle::MatchState;
//! # use poke_rust::state::dex_data::{MoveData, PokemonData};
//! # use std::collections::HashMap;
//! # fn demo(
//! #     state: &MatchState,
//! #     pokemon_dex: &HashMap<Species, PokemonData>,
//! #     move_dex: &HashMap<PokemonMove, MoveData>,
//! # ) {
//! let config = SolveConfig {
//!     depth: 2,
//!     chance: ChanceMode::TopK(4),
//!     ..SolveConfig::default()
//! };
//! let result = solve(state, pokemon_dex, move_dex, &config).expect("solvable position");
//! println!("P1 wins {:.1}% of the time", 100.0 * result.p1_win_odds);
//! for action in &result.p1_strategy {
//!     println!("  {:.1}%  {:?}", 100.0 * action.probability, action.commands);
//! }
//! # }
//! ```

pub mod actions;
pub mod belief;
pub mod chance;
pub mod eval;
pub mod exploit;
pub mod infoset;
pub mod ismcts;
pub mod matrix;
pub mod mccfr;
pub mod mcts;
pub mod preview;
pub mod search;
pub mod train;

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::data::pokemon_move::PokemonMove;
use crate::data::species::Species;
use crate::simulator::scoped_sample_rng;
use crate::state::battle::{BattleCommand, MatchState, Player};
use crate::state::dex_data::{MoveData, PokemonData};

use chance::ChanceMode;
use eval::{BatchEvaluator, LeafEvaluator};

/// Selects one solver algorithm.
/// All algorithms must compute the same value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SolverAlgorithm {
    /// Evaluates every matrix cell and solves every linear program.
    BackwardInduction,
    /// Bounds successors with both serialized move orders.
    /// Equal bounds identify a pure equilibrium.
    /// This method replaces some matrix solves with more turn simulations.
    SerializedBounds,
    /// Grows a restricted game from one action per player.
    /// Adds each player's best response until neither player can improve.
    DoubleOracle,
}

/// A cooperative stop signal that one thread raises and a search reads.
///
/// A clone shares the flag of the original, so the caller keeps a handle while
/// the search holds its own. The flag only moves from clear to set, so a search
/// that reads it one time never has to read it again.
///
/// The search reads the flag at a point where it holds a complete answer. It
/// then returns that answer with [`SolveWarning::Cancelled`]. A cancel is
/// therefore never an error.
///
/// The flag travels as an argument of a `_cancellable` entry point.
/// [`SolveConfig`] is `Copy`, so it cannot hold this type.
#[derive(Debug, Clone, Default)]
pub struct CancelFlag(Arc<AtomicBool>);

impl CancelFlag {
    /// A new flag, clear.
    pub fn new() -> Self {
        CancelFlag(Arc::new(AtomicBool::new(false)))
    }

    /// Asks every search that holds this flag to stop.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    /// Whether some caller asked the search to stop.
    ///
    /// `Relaxed` is enough. The flag carries no other data, and a search that
    /// reads a stale `false` reads the flag again at the next check point.
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }

    /// The shared cell of this flag.
    ///
    /// `search::resolve` hands this cell to the simulator, which stops one turn
    /// simulation on a raised flag. The simulator takes the cell rather than this
    /// type, so it needs no name from the solver.
    pub(crate) fn shared(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.0)
    }
}

/// Whether an optional flag is set.
///
/// Each search takes `Option<&CancelFlag>`, and `None` means that the caller
/// asked for no cancellation.
pub(crate) fn cancel_requested(flag: Option<&CancelFlag>) -> bool {
    flag.is_some_and(CancelFlag::is_cancelled)
}

/// Everything the search needs beyond the position itself.
#[derive(Debug, Clone, Copy)]
pub struct SolveConfig {
    /// Turns of lookahead.
    /// Replacement and self-switch choices do not consume depth.
    pub depth: u8,
    /// Searches depth 1 first, then each depth up to `depth`.
    /// The result holds the last complete depth.
    /// A pass that reaches a stop limit never replaces a complete pass.
    pub iterative_deepening: bool,
    /// Damage rolls per attack.
    pub damage_rolls: u8,
    /// Whether to branch on critical hits, likewise passed through.
    pub consider_crit: bool,
    /// How much of each turn's outcome distribution to descend into.
    pub chance: ChanceMode,
    pub algorithm: SolverAlgorithm,
    /// Scores positions at the depth horizon; see [`eval::LeafEvaluator`].
    pub eval: LeafEvaluator,
    /// Scores a slice of positions in one call; see [`eval::BatchEvaluator`].
    ///
    /// The search is depth first, so it reaches one leaf at a time and never
    /// calls this pointer. A model evaluator and a parallel search need the
    /// entry point, and [`eval::score_batch`] routes to it.
    pub eval_batch: Option<BatchEvaluator>,
    /// Enables serialized bounds during double-oracle search.
    /// Each bound requires an auxiliary search.
    pub use_serialized_bounds: bool,
    /// Maximum joint actions for each player.
    /// `None` keeps the complete action set.
    /// A cap makes the result approximate.
    pub max_actions_per_player: Option<usize>,
    /// Removes an attack that another attack of the same slot beats on both
    /// damage and accuracy.
    /// The filter reads the current position, so a partner command that changes
    /// the weather or the terrain can invert the comparison.
    /// The filter therefore makes the result approximate.
    pub prune_dominated_actions: bool,
    /// Maximum expanded nodes.
    /// Static evaluation replaces later search.
    pub node_budget: Option<u64>,
    /// Wall-clock limit from the start of the solve.
    /// Static evaluation replaces later search, as for a spent node budget.
    /// The solver does not start a new turn simulation after the limit expires.
    /// The limit also stops a turn simulation that is already active: the
    /// simulator reads an abort signal at each branch loop, and the cell of an
    /// aborted simulation takes a static score.
    /// `None` keeps the solve exact.
    pub deadline: Option<Duration>,
    /// Transposition-table capacity.
    /// Zero disables the table.
    pub tt_capacity: usize,
    /// Turn-cache capacity in successor states.
    /// Zero disables the cache.
    /// Serialized searches can reuse these results.
    pub turn_cache_capacity: usize,
    /// Maximum decision chain that does not consume depth.
    pub max_forced_chain: u8,
    /// Turns of lookahead below a replacement or a self-switch pivot.
    /// `None` gives a forced decision the remaining turn budget, as a turn gets.
    /// Read [`forced_descent`] for the rule and for its termination bound.
    pub replacement_depth: Option<u8>,
}

impl Default for SolveConfig {
    fn default() -> Self {
        SolveConfig {
            depth: 2,
            iterative_deepening: false,
            damage_rolls: 1,
            consider_crit: false,
            chance: ChanceMode::Enumerate,
            algorithm: SolverAlgorithm::DoubleOracle,
            eval: eval::fitted,
            eval_batch: None,
            use_serialized_bounds: false,
            max_actions_per_player: None,
            prune_dominated_actions: false,
            node_budget: Some(2_000_000),
            deadline: None,
            tt_capacity: 1 << 18,
            turn_cache_capacity: 0,
            max_forced_chain: 8,
            replacement_depth: None,
        }
    }
}

/// The high bit of a forced-chain counter. It marks a path that already
/// extended its horizon. See [`forced_descent`].
pub(crate) const EXTENDED_FLAG: u8 = 0b1000_0000;

/// The low seven bits of a forced-chain counter. They hold the count.
pub(crate) const CHAIN_MASK: u8 = !EXTENDED_FLAG;

/// The depth and the forced-chain counter for a root position.
///
/// A search can start at a replacement or self-switch pivot. In that case,
/// apply `replacement_depth` before the search expands the root. Mark a depth
/// increase as the one permitted horizon extension for the path.
pub(crate) fn root_descent(
    phase: actions::Phase,
    depth: u8,
    replacement_depth: Option<u8>,
) -> (u8, u8) {
    let forced = matches!(phase, actions::Phase::SelfSwitch | actions::Phase::Replacement);
    if !forced {
        return (depth, 0);
    }
    let Some(value) = replacement_depth else {
        return (depth, 0);
    };
    let value = value.max(1);
    let chain = if value > depth { EXTENDED_FLAG } else { 0 };
    (value, chain)
}

/// The depth and the forced-chain counter that a successor uses.
///
/// All four searches call this function. `phase` is the phase of the successor,
/// and `depth` and `chain` belong to the parent.
///
/// A replacement or a self-switch pivot is a forced decision. It resolves inside
/// the same turn, so it consumes no depth. `max_forced_chain` bounds how long
/// one chain of such decisions can go on.
///
/// `replacement_depth` sets the depth of a forced child:
///
/// - `None` keeps the remaining depth. This is the depth that a turn would get.
/// - `Some(value)` uses `value`, and `value` clamps to a minimum of 1. A value
///   below the remaining depth lowers the child depth. A value above it raises
///   the child depth, so the search looks past the turn budget of the root.
///
/// # Termination
///
/// A raise extends the horizon, so an unlimited count of raises could make one
/// path grow without a bound. One path therefore raises one time. After that, a
/// forced child takes the lower of the value and the remaining depth.
///
/// The counter carries that fact in [`EXTENDED_FLAG`], which keeps the node
/// state at one pair. Each search keys its cache on the pair, so a cached value
/// cannot cross an extension boundary.
///
/// Read this measure of a node, in this order:
///
/// 1. `0` when the path already extended, and `1` when it did not.
/// 2. The remaining depth.
/// 3. The room left in the forced chain.
///
/// A raise lowers item 1. Another forced child keeps item 1 and lowers item 3. A
/// normal turn keeps item 1 and lowers item 2. A node at depth 0 takes a static
/// score and expands no child. Every edge therefore lowers the measure, and the
/// measure has a lower bound.
pub(crate) fn forced_descent(
    phase: actions::Phase,
    depth: u8,
    chain: u8,
    max_forced_chain: u8,
    replacement_depth: Option<u8>,
) -> (u8, u8) {
    let extended = chain & EXTENDED_FLAG;
    let count = chain & CHAIN_MASK;
    // The flag owns the high bit, so the count cannot use it.
    let limit = max_forced_chain.min(CHAIN_MASK);
    let forced = matches!(phase, actions::Phase::SelfSwitch | actions::Phase::Replacement);
    if !forced || count >= limit {
        return (depth.saturating_sub(1), extended);
    }
    let Some(value) = replacement_depth else {
        return (depth, (count + 1) | extended);
    };
    // Depth 0 would score a replacement position with no decision at all.
    let value = value.max(1);
    if value > depth && extended == 0 {
        return (value, (count + 1) | EXTENDED_FLAG);
    }
    (value.min(depth), (count + 1) | extended)
}

/// One joint action — a command per active slot — and how often to play it.
#[derive(Debug, Clone)]
pub struct JointActionProb {
    /// One `BattleCommand` per active slot, in slot order. Submit as
    /// `PlayerCommand::Battle(commands)`.
    pub commands: Vec<BattleCommand>,
    /// Probability of choosing this joint action. Sums to 1 across a strategy.
    pub probability: f64,
}

/// What the search cost and how much pruning it managed.
#[derive(Debug, Clone, Default)]
pub struct SolveStats {
    /// Decision nodes whose matrix was constructed.
    pub nodes_expanded: u64,
    /// Completed `simulate_turn` calls.
    /// This is the main cost metric.
    pub turns_simulated: u64,
    /// Matrix cells whose value was computed.
    pub matrix_cells_evaluated: u64,
    /// Matrix cells that existed. The ratio against `matrix_cells_evaluated` is
    /// exactly what double-oracle pruning bought.
    pub matrix_cells_total: u64,
    /// Matrix games that reached the simplex, as opposed to a fast path.
    pub lps_solved: u64,
    /// Alpha-beta and star1 cutoffs taken in the serialized searches.
    pub ab_cutoffs: u64,
    /// Positions answered from the transposition table.
    pub tt_hits: u64,
    /// Turn resolutions answered from the turn cache.
    pub turn_cache_hits: u64,
    /// Wall-clock time for the whole solve.
    pub elapsed: Duration,
}

/// Describes why a returned answer is approximate.
#[derive(Debug, Clone, PartialEq)]
pub enum SolveWarning {
    /// The node budget ran out; nodes past that point were scored statically
    /// rather than searched. Under iterative deepening this appears only when no
    /// pass finished, because a complete shallower pass is returned in
    /// preference to a partial deep one.
    BudgetExhausted { budget: u64 },
    /// The wall-clock deadline expired; nodes past that point were scored
    /// statically rather than searched. Under iterative deepening this appears
    /// only when no pass finished, for the same reason as `BudgetExhausted`.
    DeadlineExceeded { budget: Duration },
    /// The search stopped short of the requested depth. Only iterative deepening
    /// reports this: a single-pass search always returns the depth it was asked
    /// for, however much of it was scored statically.
    DepthNotReached { target: u8, reached: u8 },
    /// [`ChanceMode`] discarded outcome probability mass. The figure is the
    /// largest fraction dropped at any single chance node, not a total.
    ChanceMassDiscarded { max_fraction: f64 },
    /// The search used only part of a player's joint-action set.
    ActionsTruncated {
        player: Player,
        kept: usize,
        total: usize,
    },
    /// A [`CancelFlag`] stopped the search before it finished.
    ///
    /// The answer holds the work that finished. An exact search returns its last
    /// complete deepening pass, and a sampling search returns the mean and the
    /// strategy of its finished iterations. This warning describes the whole
    /// search, so it does not depend on which pass the search returned.
    Cancelled,
}

impl fmt::Display for SolveWarning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SolveWarning::BudgetExhausted { budget } => {
                write!(f, "node budget of {budget} exhausted; deeper nodes were evaluated statically")
            }
            SolveWarning::DeadlineExceeded { budget } => {
                write!(
                    f,
                    "the deadline of {budget:?} expired; deeper nodes were evaluated statically"
                )
            }
            SolveWarning::DepthNotReached { target, reached } => write!(
                f,
                "the search completed depth {reached} of the {target} requested"
            ),
            SolveWarning::ChanceMassDiscarded { max_fraction } => write!(
                f,
                "up to {:.1}% of outcome probability was discarded at a chance node",
                100.0 * max_fraction
            ),
            SolveWarning::ActionsTruncated {
                player,
                kept,
                total,
            } => write!(f, "{player:?}'s action set was limited to {kept} of {total}"),
            SolveWarning::Cancelled => write!(
                f,
                "the search was cancelled; the answer holds the work that finished"
            ),
        }
    }
}

/// Why a position could not be solved at all.
#[derive(Debug, Clone, PartialEq)]
pub enum SolveError {
    /// Team preview has a different action space.
    /// Resolve it before the battle.
    TeamPreviewUnsupported,
    /// The battle is already decided; there is nothing to choose.
    GameAlreadyOver { winner: Player },
}

impl fmt::Display for SolveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SolveError::TeamPreviewUnsupported => {
                write!(f, "cannot solve a team-preview state; resolve leads first")
            }
            SolveError::GameAlreadyOver { winner } => {
                write!(f, "the battle is already over; {winner:?} won")
            }
        }
    }
}

impl std::error::Error for SolveError {}

/// An equilibrium for one position.
#[derive(Debug, Clone)]
pub struct SolveResult {
    /// The game's value: P1's win probability under optimal play from both
    /// sides, to the configured depth. Identical to `p1_win_odds`.
    pub value: f64,
    /// P1's odds of winning, in `[0, 1]`.
    pub p1_win_odds: f64,
    /// P2's odds of winning. The game is zero-sum, so this is `1 - p1_win_odds`.
    pub p2_win_odds: f64,
    /// P1's optimal mixed strategy. Actions at probability zero are omitted.
    pub p1_strategy: Vec<JointActionProb>,
    /// P2's optimal mixed strategy, likewise.
    pub p2_strategy: Vec<JointActionProb>,
    /// The depth of the pass that produced this result. Equal to the requested
    /// depth unless iterative deepening stopped early, in which case
    /// [`SolveWarning::DepthNotReached`] accompanies it.
    pub depth_reached: u8,
    pub stats: SolveStats,
    pub warnings: Vec<SolveWarning>,
}

impl SolveResult {
    /// Returns the most probable joint action for one player.
    /// Returns `None` when the player has no choices.
    pub fn most_likely_action(&self, player: Player) -> Option<&JointActionProb> {
        let strategy = match player {
            Player::P1 => &self.p1_strategy,
            Player::P2 => &self.p2_strategy,
        };
        strategy
            .iter()
            .max_by(|a, b| a.probability.total_cmp(&b.probability))
    }
}

/// Solves `state` to the configured depth.
/// Only sample chance mode uses the current random generator.
/// Use [`solve_seeded`] for reproducible samples.
/// Set `VERBOSITY` to zero before a large search.
pub fn solve(
    state: &MatchState,
    pokemon_dex: &HashMap<Species, PokemonData>,
    move_dex: &HashMap<PokemonMove, MoveData>,
    config: &SolveConfig,
) -> Result<SolveResult, SolveError> {
    search::run(state, pokemon_dex, move_dex, config, None)
}

/// [`solve`], made deterministic in `seed`.
///
/// The same inputs always produce the same result. Only relevant under
/// [`ChanceMode::Sample`]; the seed is inert otherwise.
pub fn solve_seeded(
    seed: u64,
    state: &MatchState,
    pokemon_dex: &HashMap<Species, PokemonData>,
    move_dex: &HashMap<PokemonMove, MoveData>,
    config: &SolveConfig,
) -> Result<SolveResult, SolveError> {
    solve_seeded_cancellable(seed, state, pokemon_dex, move_dex, config, None)
}

/// [`solve_seeded`], with a cooperative stop signal.
///
/// The search reads `cancel` between nodes, between matrix cells, and between
/// chance successors. A set flag ends the search, and the result holds the last
/// complete deepening pass with [`SolveWarning::Cancelled`].
///
/// A single-pass search returns the pass that ran. Each point that the cancel
/// reached holds a static score, so every chance-node average stays complete.
///
/// `None` gives the behavior of [`solve_seeded`].
pub fn solve_seeded_cancellable(
    seed: u64,
    state: &MatchState,
    pokemon_dex: &HashMap<Species, PokemonData>,
    move_dex: &HashMap<PokemonMove, MoveData>,
    config: &SolveConfig,
    cancel: Option<&CancelFlag>,
) -> Result<SolveResult, SolveError> {
    let _guard = scoped_sample_rng(seed);
    search::run(state, pokemon_dex, move_dex, config, cancel)
}
