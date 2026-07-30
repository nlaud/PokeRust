//! Solves perfect-information battle positions.
//!
//! The input is a concrete `MatchState` with two known teams.
//! The solver searches to a fixed depth.
//! It returns each optimal mixed strategy and P1's win probability.
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

/// Everything the search needs beyond the position itself.
#[derive(Debug, Clone, Copy)]
pub struct SolveConfig {
    /// Turns of lookahead.
    /// Replacement and self-switch choices do not consume depth.
    pub depth: u8,
    /// Damage rolls per attack.
    pub damage_rolls: u8,
    /// Whether to branch on critical hits, likewise passed through.
    pub consider_crit: bool,
    /// How much of each turn's outcome distribution to descend into.
    pub chance: ChanceMode,
    pub algorithm: SolverAlgorithm,
    /// Scores positions at the depth horizon; see [`eval::LeafEvaluator`].
    pub eval: LeafEvaluator,
    /// Enables serialized bounds during double-oracle search.
    /// Each bound requires an auxiliary search.
    pub use_serialized_bounds: bool,
    /// Maximum joint actions for each player.
    /// `None` keeps the complete action set.
    /// A cap makes the result approximate.
    pub max_actions_per_player: Option<usize>,
    /// Maximum expanded nodes.
    /// Static evaluation replaces later search.
    pub node_budget: Option<u64>,
    /// Transposition-table capacity.
    /// Zero disables the table.
    pub tt_capacity: usize,
    /// Turn-cache capacity in successor states.
    /// Zero disables the cache.
    /// Serialized searches can reuse these results.
    pub turn_cache_capacity: usize,
    /// Maximum decision chain that does not consume depth.
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
