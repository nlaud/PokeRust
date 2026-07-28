//! Game-tree solver for perfect-information battle positions.
//!
//! Given a concrete `MatchState` — both teams fully known — this computes each
//! player's optimal *mixed* strategy over their legal joint commands and the
//! resulting win odds, by searching forward to a fixed depth and solving the
//! game exactly at every node.
//!
//! # Why not minimax
//!
//! A Pokemon turn is a **simultaneous-move stochastic game**: both players
//! commit an action without seeing the other's, and the engine then resolves a
//! probability distribution over successor states. Ordinary minimax models this
//! wrongly — it lets one player see the other's commitment — and the "safest
//! move" policy it produces is deterministic and therefore exploitable by anyone
//! who models it. The correct value of a node is the **Nash equilibrium value**
//! of the payoff matrix whose cells are the joint actions' expected values, and
//! its solution is genuinely mixed. That is why the output here is a probability
//! per action rather than a single best move.
//!
//! # Algorithms
//!
//! Implements the family described in Bošanský, Lisý, Lanctot, Čermák &
//! Winands, *Algorithms for computing strategies in two-player simultaneous move
//! games*, Artificial Intelligence 237:1–40 (2016) — see
//! [`SolverAlgorithm`] for the three variants and what each buys.
//!
//! The paper's chance-node model maps onto this engine exactly: its transition
//! weight `P*(s, r, c, s')` is precisely the probability that `simulate_turn`
//! already attaches to each successor.
//!
//! # Utilities are win probabilities
//!
//! Every value in this module is P1's win probability, in `[0, 1]`; P1
//! maximizes and P2 minimizes. That choice is load-bearing rather than
//! cosmetic — it hands the search globally valid bounds `L = 0`, `U = 1` with no
//! tuning, which is what star1 pruning at chance nodes and the serialized
//! alpha-beta windows both require.
//!
//! # Cost
//!
//! The dominant cost is `simulate_turn`, at hundreds of microseconds per call,
//! against microseconds for a matrix LP. So the metric that decides whether a
//! configuration is affordable is [`SolveStats::turns_simulated`], not the LP
//! count — and the lever that matters is [`ChanceMode`], which bounds how much
//! of each turn's outcome distribution the search descends into.
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
pub mod chance;
pub mod eval;
pub mod matrix;
pub mod search;

use std::collections::HashMap;
use std::fmt;
use std::time::Duration;

use crate::data::pokemon_move::PokemonMove;
use crate::data::species::Species;
use crate::simulator::scoped_sample_rng;
use crate::state::battle::{BattleCommand, MatchState, Player};
use crate::state::dex_data::{MoveData, PokemonData};

use chance::ChanceMode;
use eval::LeafEvaluator;

/// Which of the paper's algorithms to run. All three compute the same value —
/// that equivalence is the search's central regression test — and differ only in
/// how much work they do to get there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SolverAlgorithm {
    /// Algorithm 1. Evaluate every cell of every matrix, solve every LP. The
    /// reference implementation: no pruning, nothing to be subtly wrong.
    BackwardInduction,
    /// Algorithm 2 (BIαβ). Before recursing into a successor, bound its value
    /// with alpha-beta search on the two *serializations* of the subgame — the
    /// variants where one player moves first and the other second. Letting a
    /// player move second can only help them, so those searches bracket the true
    /// simultaneous value; when the brackets meet, the subgame has a pure
    /// equilibrium and needs no recursion at all.
    ///
    /// This trades LP solves for extra turn simulations, which is the wrong
    /// direction given where this engine's cost actually sits. Included so the
    /// benchmark can measure that rather than assume it.
    SerializedBounds,
    /// Algorithm 3 (DOαβ), the default. Rather than filling the whole matrix,
    /// grow a *restricted* game from one action per player: solve the small LP,
    /// find each player's best response over the full action set, add those, and
    /// repeat until neither player can improve. Equilibria have small support in
    /// practice, so this typically touches a small fraction of the cells — and a
    /// cell is a `simulate_turn` call, which is the whole cost of the search.
    DoubleOracle,
}

/// Everything the search needs beyond the position itself.
#[derive(Debug, Clone, Copy)]
pub struct SolveConfig {
    /// Turns of lookahead. Mid-turn decision points — replacements after a
    /// faint, self-switch pivots — do not consume a ply; see
    /// [`SolveConfig::max_forced_chain`].
    pub depth: u8,
    /// Damage rolls per attack, passed through to `simulate_turn`. The single
    /// biggest influence on both fidelity and cost.
    pub damage_rolls: u8,
    /// Whether to branch on critical hits, likewise passed through.
    pub consider_crit: bool,
    /// How much of each turn's outcome distribution to descend into.
    pub chance: ChanceMode,
    pub algorithm: SolverAlgorithm,
    /// Scores positions at the depth horizon; see [`eval::LeafEvaluator`].
    pub eval: LeafEvaluator,
    /// Whether [`SolverAlgorithm::DoubleOracle`] should also compute serialized
    /// alpha-beta bounds. They sharpen its best-response pruning but cost a full
    /// auxiliary search per successor. Off by default, and
    /// [`SolverAlgorithm::SerializedBounds`] computes them regardless.
    pub use_serialized_bounds: bool,
    /// Cap on each player's joint-action count. `None` means no cap, which is
    /// right for singles; doubles reaches a few hundred joint actions and tens
    /// of thousands of matrix cells, where a cap is the difference between
    /// tractable and not. Capping makes the equilibrium approximate and is
    /// reported as [`SolveWarning::ActionsTruncated`].
    pub max_actions_per_player: Option<usize>,
    /// Stop expanding after this many nodes and fall back to evaluating
    /// statically. Exhausting the budget is reported as
    /// [`SolveWarning::BudgetExhausted`] and never panics.
    pub node_budget: Option<u64>,
    /// Transposition-table size in entries, rounded up to a power of two. Cheap
    /// — an entry is a hash, a depth and a bounded value — so this defaults
    /// generously. Zero disables it.
    pub tt_capacity: usize,
    /// Turn-resolution cache size, in *successor states* rather than entries.
    ///
    /// Defaults to zero (disabled), because entries hold whole `MatchState`s and
    /// a single turn at high damage-roll counts can produce hundreds of them.
    /// It pays for itself only under [`SolverAlgorithm::SerializedBounds`] or
    /// `use_serialized_bounds`, where the auxiliary searches revisit the same
    /// `(state, action, action)` triples the main search already resolved.
    pub turn_cache_capacity: usize,
    /// How many consecutive mid-turn decision points may pass without consuming
    /// a ply, before one is charged anyway. A safety valve: replacements always
    /// make progress, so this should never bind.
    pub max_forced_chain: u8,
}

impl Default for SolveConfig {
    fn default() -> Self {
        SolveConfig {
            depth: 2,
            damage_rolls: 1,
            consider_crit: false,
            chance: ChanceMode::Enumerate,
            algorithm: SolverAlgorithm::DoubleOracle,
            eval: eval::heuristic,
            use_serialized_bounds: false,
            max_actions_per_player: None,
            node_budget: Some(2_000_000),
            tt_capacity: 1 << 18,
            turn_cache_capacity: 0,
            max_forced_chain: 8,
        }
    }
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
    /// `simulate_turn` calls that actually ran. **The cost metric that matters**
    /// — everything else in this struct is noise beside it.
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

/// A non-fatal degradation: the answer is still returned, but is approximate in
/// the stated way.
#[derive(Debug, Clone, PartialEq)]
pub enum SolveWarning {
    /// The node budget ran out; nodes past that point were scored statically
    /// rather than searched.
    BudgetExhausted { budget: u64 },
    /// [`ChanceMode`] discarded outcome probability mass. The figure is the
    /// largest fraction dropped at any single chance node, not a total.
    ChanceMassDiscarded { max_fraction: f64 },
    /// A player's joint-action set was capped, so the reported equilibrium is
    /// over a subset of their real options.
    ActionsTruncated {
        player: Player,
        kept: usize,
        total: usize,
    },
}

impl fmt::Display for SolveWarning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SolveWarning::BudgetExhausted { budget } => {
                write!(f, "node budget of {budget} exhausted; deeper nodes were evaluated statically")
            }
            SolveWarning::ChanceMassDiscarded { max_fraction } => write!(
                f,
                "up to {:.1}% of outcome probability was discarded at a chance node",
                100.0 * max_fraction
            ),
            SolveWarning::ActionsTruncated {
                player,
                kept,
                total,
            } => write!(f, "{player:?}'s action set was capped at {kept} of {total}"),
        }
    }
}

/// Why a position could not be solved at all.
#[derive(Debug, Clone, PartialEq)]
pub enum SolveError {
    /// Team preview is a simultaneous move too, but over lead selections rather
    /// than battle commands, and its action space is a different combinatorial
    /// problem. Resolve the preview first, then solve the battle.
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
    pub stats: SolveStats,
    pub warnings: Vec<SolveWarning>,
}

impl SolveResult {
    /// The single likeliest joint action for `player`, or `None` in a position
    /// with no choices. Convenient for a bot that will not mix; note that
    /// playing it deterministically forfeits the unexploitability that made the
    /// equilibrium worth computing.
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

/// Solve `state` to `config.depth` turns of lookahead.
///
/// Uses whatever RNG is ambient, which only matters for
/// [`ChanceMode::Sample`]; every other mode is deterministic. Use
/// [`solve_seeded`] when reproducibility is required.
///
/// Set `VERBOSITY` to 0 before calling. The engine's tracing is keyed off that
/// global, and a search performs thousands of turn resolutions — leaving it high
/// floods stdout and dominates the runtime.
pub fn solve(
    state: &MatchState,
    pokemon_dex: &HashMap<Species, PokemonData>,
    move_dex: &HashMap<PokemonMove, MoveData>,
    config: &SolveConfig,
) -> Result<SolveResult, SolveError> {
    search::run(state, pokemon_dex, move_dex, config)
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
    let _guard = scoped_sample_rng(seed);
    solve(state, pokemon_dex, move_dex, config)
}
