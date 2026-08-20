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
//! # The determinized baseline
//!
//! [`pimc::search`] solves each drawn world with [`solve`] and averages the
//! world strategies. This is perfect-information Monte Carlo.
//!
//! Each world solve reads the hidden data of that world, so the mix plays a
//! different action in each world. No player can do that. This defect is
//! strategy fusion, and every answer carries [`SolveWarning::StrategyFusion`].
//!
//! Use this search as a labeled baseline of the two searches above. Do not use
//! it as the main fog-of-war solver.
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
//! # Parallel search
//!
//! [`SolveConfig::workers`] asks double oracle for more than one worker.
//! The root position evaluates its matrix cells in batches.
//! Every other part of the search stays serial.
//!
//! [`pool`] holds the permits of the process. It bounds the extra threads across
//! every concurrent solve, and it uses neither the Tokio runtime nor the
//! benchmark threads.
//!
//! A parallel solve returns the value and the strategies of a serial solve. Read
//! the `search` module documentation for the rules that give this property, and
//! for what the pool leaves serial.
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
pub mod pimc;
pub mod pool;
pub mod preview;
pub mod search;
pub mod train;

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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
#[derive(Debug, Default)]
struct SearchControl {
    cancelled: Arc<AtomicBool>,
    simulation_turn_budget: AtomicU64,
    simulation_turns: AtomicU64,
    simulation_budget_hit: AtomicBool,
    /// The control that also counts every turn of this one.
    ///
    /// [`CancelFlag::child_with_budget`] sets this field. A claim then passes
    /// through to the parent, so one job budget still bounds the whole run while
    /// the child holds its own share. See that method for the rule.
    parent: Option<Arc<SearchControl>>,
}

impl SearchControl {
    /// Claims one turn from this control and from every control above it.
    fn claim(&self) -> bool {
        if !self.claim_own() {
            return false;
        }
        match &self.parent {
            Some(parent) => parent.claim(),
            None => true,
        }
    }

    /// Claims one turn from this control alone.
    ///
    /// A zero budget has no limit, and it counts nothing.
    fn claim_own(&self) -> bool {
        let budget = self.simulation_turn_budget.load(Ordering::Relaxed);
        if budget == 0 {
            return true;
        }
        let claimed =
            self.simulation_turns
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                    (value < budget).then_some(value + 1)
                });
        if claimed.is_err() {
            self.simulation_budget_hit.store(true, Ordering::Relaxed);
            return false;
        }
        true
    }
}

#[derive(Debug, Clone, Default)]
pub struct CancelFlag(Arc<SearchControl>);

impl CancelFlag {
    /// A new flag, clear.
    pub fn new() -> Self {
        CancelFlag(Arc::new(SearchControl::default()))
    }

    /// Makes a flag with one shared simulation-turn budget.
    pub fn with_simulation_turn_budget(budget: u64) -> Self {
        let flag = Self::new();
        flag.0.simulation_turn_budget.store(budget, Ordering::Relaxed);
        flag
    }

    /// Asks every search that holds this flag to stop.
    pub fn cancel(&self) {
        self.0.cancelled.store(true, Ordering::Relaxed);
    }

    /// Whether some caller asked the search to stop.
    ///
    /// `Relaxed` is enough. The flag carries no other data, and a search that
    /// reads a stale `false` reads the flag again at the next check point.
    pub fn is_cancelled(&self) -> bool {
        self.0.cancelled.load(Ordering::Relaxed)
    }

    /// A flag with its own budget that still spends the budget of this one.
    ///
    /// The child shares the cancel signal of the parent, so one `cancel` call
    /// stops both. Each claim takes one turn from the child and one turn from
    /// the parent, so the parent counter moves while the child runs. A caller
    /// that reports progress therefore reads the parent as before.
    ///
    /// Either budget can stop a claim, and the two mean different things:
    ///
    /// - The child ran out. Its own share is spent, and the caller starts the
    ///   next child.
    /// - The parent ran out. The whole job is spent.
    ///
    /// Read [`CancelFlag::simulation_budget_hit`] on each flag to tell them
    /// apart. A parent refusal leaves the child counter one turn high, because
    /// the child claims first. No simulation runs on that claim, so the figure
    /// only describes a child that already stopped.
    ///
    /// A search must not read that method, because it answers for one flag
    /// alone. [`CancelFlag::simulation_budget_exhausted`] walks the chain, and
    /// that is the question a search asks: whether any later claim can succeed.
    ///
    /// [`pimc`] uses this to give each drawn world an equal share of one job
    /// budget.
    pub(crate) fn child_with_budget(&self, budget: u64) -> CancelFlag {
        CancelFlag(Arc::new(SearchControl {
            cancelled: Arc::clone(&self.0.cancelled),
            simulation_turn_budget: AtomicU64::new(budget),
            simulation_turns: AtomicU64::new(0),
            simulation_budget_hit: AtomicBool::new(false),
            parent: Some(Arc::clone(&self.0)),
        }))
    }

    /// Claims one turn simulation from the shared budget.
    ///
    /// A flag with a zero budget has no simulation limit.
    ///
    /// A child flag claims from itself and then from every flag above it. See
    /// [`CancelFlag::child_with_budget`].
    pub(crate) fn claim_simulation_turn(&self) -> bool {
        self.0.claim()
    }

    /// Returns true after a search tries to pass the simulation-turn budget of
    /// this flag alone.
    ///
    /// A child flag reports its own share here, and it reports `false` while
    /// the share holds and an ancestor is spent. A search reads
    /// [`CancelFlag::simulation_budget_exhausted`] instead, because a refused
    /// claim stops the search whichever flag refused it.
    pub fn simulation_budget_hit(&self) -> bool {
        self.0.simulation_budget_hit.load(Ordering::Relaxed)
    }

    /// Returns true after this flag or any flag above it refused a claim.
    ///
    /// [`CancelFlag::claim_simulation_turn`] claims from this flag and then from
    /// every flag above it, so an ancestor can refuse a claim that this flag
    /// permitted. The refusal stops the search either way: no later claim can
    /// succeed, so every later turn would take a static score.
    ///
    /// A search therefore reads this method rather than
    /// [`CancelFlag::simulation_budget_hit`]. Without it a `pimc` world whose
    /// job budget ran out would run its whole depth on static scores and then
    /// report the answer as complete.
    pub fn simulation_budget_exhausted(&self) -> bool {
        let mut control = &self.0;
        loop {
            if control.simulation_budget_hit.load(Ordering::Relaxed) {
                return true;
            }
            match &control.parent {
                Some(parent) => control = parent,
                None => return false,
            }
        }
    }

    /// Returns the simulation turns that this flag claimed.
    pub fn simulation_turns(&self) -> u64 {
        self.0.simulation_turns.load(Ordering::Relaxed)
    }

    /// Returns the configured simulation-turn budget.
    pub fn simulation_turn_budget(&self) -> Option<u64> {
        match self.0.simulation_turn_budget.load(Ordering::Relaxed) {
            0 => None,
            budget => Some(budget),
        }
    }

    /// The shared cell of this flag.
    ///
    /// `search::resolve` hands this cell to the simulator, which stops one turn
    /// simulation on a raised flag. The simulator takes the cell rather than this
    /// type, so it needs no name from the solver.
    pub(crate) fn shared(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.0.cancelled)
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
    /// The workers that double oracle asks for.
    ///
    /// A value of 0 or 1 keeps the serial search. A larger value lets the root
    /// position evaluate matrix cells in batches. [`pool`] bounds the extra
    /// threads across every solve of the process, so a solve can get fewer
    /// workers than it asks for.
    ///
    /// The pool does not change the value or either strategy. It does change the
    /// cost counters, and it does change which nodes a stopped search reached.
    /// Read the `search` module documentation for the rules.
    ///
    /// One worker holds its own transposition table, so a large `tt_capacity`
    /// multiplies the memory of a solve by this count.
    pub workers: usize,
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
            workers: 1,
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
    /// The search exhausted the simulation-turn budget.
    /// The search used static scores after this point.
    SimulationTurnBudgetExhausted { budget: u64 },
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
    /// The root double-oracle run completed no round, so both strategies are
    /// the uniform placeholder rather than an equilibrium.
    ///
    /// Double oracle publishes an answer only after both best-response checks
    /// of a round. A stop reason that arrives before the first round leaves the
    /// run with nothing to publish, and it returns the uniform strategy that it
    /// started from. That answer says only that the search learned nothing.
    ///
    /// A uniform strategy is also a legitimate equilibrium of some positions,
    /// and the two look identical on the wire. This warning is what tells them
    /// apart.
    NoCompletedRound,
    /// The answer mixed one strategy for each drawn world.
    ///
    /// [`pimc::search`] solves each world as a perfect-information game, so each
    /// world plays the hidden data that it drew. The mix therefore claims
    /// knowledge that no player holds. This defect is strategy fusion, and every
    /// answer of that search carries this warning.
    StrategyFusion { worlds: usize },
}

impl SolveWarning {
    /// True when this warning says that the search stopped short of the work it
    /// was configured to do.
    ///
    /// The three stop limits and a short depth all say the same thing: part of
    /// the returned answer is a static score rather than a searched value.
    /// [`SolveWarning::ChanceMassDiscarded`], [`SolveWarning::ActionsTruncated`],
    /// and [`SolveWarning::StrategyFusion`] describe the configured method
    /// instead, so they leave a finished search finished.
    pub fn stopped_configured_work(&self) -> bool {
        matches!(
            self,
            SolveWarning::BudgetExhausted { .. }
                | SolveWarning::SimulationTurnBudgetExhausted { .. }
                | SolveWarning::DeadlineExceeded { .. }
                | SolveWarning::DepthNotReached { .. }
                | SolveWarning::Cancelled
                | SolveWarning::NoCompletedRound
        )
    }
}

/// True when no warning says that configured work stopped early.
///
/// One authority for the whole project. The simulate, tracker, and streaming
/// endpoints each used to hold their own copy of this rule, and the copies
/// disagreed about [`SolveWarning::BudgetExhausted`].
pub fn warnings_are_complete(warnings: &[SolveWarning]) -> bool {
    !warnings.iter().any(SolveWarning::stopped_configured_work)
}

/// [`warnings_are_complete`], for a search that samples.
///
/// The simulation-turn budget is the terminal limit of a sampling search rather
/// than an interruption of it, so a spent budget leaves such an answer complete.
/// Every other stop reason still cuts the answer short.
pub fn sampling_warnings_are_complete(warnings: &[SolveWarning]) -> bool {
    !warnings.iter().any(|warning| {
        warning.stopped_configured_work()
            && !matches!(
                warning,
                SolveWarning::SimulationTurnBudgetExhausted { .. }
            )
    })
}

impl fmt::Display for SolveWarning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SolveWarning::BudgetExhausted { budget } => {
                write!(f, "node budget of {budget} exhausted; deeper nodes were evaluated statically")
            }
            SolveWarning::SimulationTurnBudgetExhausted { budget } => write!(
                f,
                "the search exhausted the simulation-turn budget of {budget}. Later positions used static scores"
            ),
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
            SolveWarning::NoCompletedRound => write!(
                f,
                "the search completed no double-oracle round, so both strategies are the uniform placeholder rather than an equilibrium"
            ),
            SolveWarning::Cancelled => write!(
                f,
                "the search was cancelled; the answer holds the work that finished"
            ),
            SolveWarning::StrategyFusion { worlds } => write!(
                f,
                "the search solved {worlds} world(s) separately and averaged the strategies, so it played each world as if the hidden data were known (strategy fusion)"
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

/// The answer of one double-oracle round at the root position.
///
/// [`double_oracle`](matrix::double_oracle) adds actions until neither player
/// has a better response. Each round therefore holds a complete answer over the
/// actions that the round reached. A caller that reports progress can publish
/// that answer before the whole search finishes.
///
/// The round is not an equilibrium of the whole game. Only the last round of a
/// converged run is.
#[derive(Debug, Clone)]
pub struct RootRound {
    /// The depth of the deepening pass that this round belongs to.
    pub depth: u8,
    /// The value of the restricted game, as P1's win probability.
    pub value: f64,
    /// P1's strategy over the actions that this round reached.
    pub p1_strategy: Vec<JointActionProb>,
    /// P2's strategy, likewise.
    pub p2_strategy: Vec<JointActionProb>,
    /// The search statistics up to this round.
    pub stats: SolveStats,
}

/// Reads each root round while the search runs.
///
/// The search calls this pointer on its own thread, between two matrix cells. A
/// slow call therefore slows the search. Keep the call short.
pub type RootProgress<'a> = &'a dyn Fn(RootRound);

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
    search::run(state, pokemon_dex, move_dex, config, None, None)
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
    solve_seeded_progress_cancellable(seed, state, pokemon_dex, move_dex, config, None, cancel)
}

/// [`solve_seeded_cancellable`], with a progress hook for the root rounds.
///
/// The hook fires after both full best-response checks of each double-oracle
/// round at the root position. It does not fire below the root, and it does not
/// fire for the other two algorithms, because only double oracle has rounds.
///
/// The hook cannot change the search. It reads one [`RootRound`] and returns.
///
/// `None` gives the behavior of [`solve_seeded_cancellable`].
pub fn solve_seeded_progress_cancellable(
    seed: u64,
    state: &MatchState,
    pokemon_dex: &HashMap<Species, PokemonData>,
    move_dex: &HashMap<PokemonMove, MoveData>,
    config: &SolveConfig,
    progress: Option<RootProgress<'_>>,
    cancel: Option<&CancelFlag>,
) -> Result<SolveResult, SolveError> {
    let _guard = scoped_sample_rng(seed);
    search::run(state, pokemon_dex, move_dex, config, progress, cancel)
}

#[cfg(test)]
mod budget_tests {
    use super::CancelFlag;

    #[test]
    fn cloned_flags_share_one_simulation_budget() {
        let control = CancelFlag::with_simulation_turn_budget(2);
        let clone = control.clone();

        assert!(control.claim_simulation_turn());
        assert!(clone.claim_simulation_turn());
        assert!(!control.claim_simulation_turn());
        assert_eq!(clone.simulation_turns(), 2);
        assert!(clone.simulation_budget_hit());
    }

    #[test]
    fn concurrent_claims_do_not_pass_the_budget() {
        let control = CancelFlag::with_simulation_turn_budget(37);
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let worker = control.clone();
                std::thread::spawn(move || {
                    (0..20)
                        .filter(|_| worker.claim_simulation_turn())
                        .count()
                })
            })
            .collect();
        let claimed: usize = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .sum();

        assert_eq!(claimed, 37);
        assert_eq!(control.simulation_turns(), 37);
        assert!(control.simulation_budget_hit());
    }

    /// A child spends its own share, and the parent counts every turn of it.
    #[test]
    fn a_child_stops_at_its_share_and_the_parent_counts_it() {
        let job = CancelFlag::with_simulation_turn_budget(10);
        let world = job.child_with_budget(3);

        assert!(world.claim_simulation_turn());
        assert!(world.claim_simulation_turn());
        assert!(world.claim_simulation_turn());
        assert!(!world.claim_simulation_turn());
        assert!(world.simulation_budget_hit());
        // The parent counted the three turns, and its own budget still holds.
        assert_eq!(job.simulation_turns(), 3);
        assert!(!job.simulation_budget_hit());

        // A second world takes its own share of the same job budget.
        let next = job.child_with_budget(3);
        assert!(next.claim_simulation_turn());
        assert_eq!(job.simulation_turns(), 4);
    }

    /// A spent job budget stops a child that still holds its own share.
    #[test]
    fn a_spent_parent_stops_a_child_with_room_left() {
        let job = CancelFlag::with_simulation_turn_budget(2);
        let world = job.child_with_budget(100);

        assert!(world.claim_simulation_turn());
        assert!(world.claim_simulation_turn());
        assert!(!world.claim_simulation_turn());
        assert!(job.simulation_budget_hit());
        // The child claims first, so its counter holds the refused claim too.
        assert!(!world.simulation_budget_hit());
        assert_eq!(world.simulation_turns(), 3);
        // The child's own share still holds, but no later claim can succeed, so
        // the search that reads this flag must stop.
        assert!(world.simulation_budget_exhausted());
    }

    /// A child with room left, under a job with room left, stops for nothing.
    #[test]
    fn a_child_with_room_left_reports_no_exhausted_budget() {
        let job = CancelFlag::with_simulation_turn_budget(10);
        let world = job.child_with_budget(5);

        assert!(world.claim_simulation_turn());
        assert!(!world.simulation_budget_exhausted());
        assert!(!job.simulation_budget_exhausted());
    }

    /// One cancel must stop the parent and every child.
    #[test]
    fn a_cancelled_parent_cancels_its_child() {
        let job = CancelFlag::with_simulation_turn_budget(10);
        let world = job.child_with_budget(5);

        assert!(!world.is_cancelled());
        job.cancel();
        assert!(world.is_cancelled());
    }
}
