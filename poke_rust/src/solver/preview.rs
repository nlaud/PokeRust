//! Solves a perfect-information team preview.
//!
//! The input is a concrete [`TeamPreviewState`] with two known teams.
//! Each player selects a bring set and a lead order without seeing the other
//! selection.
//! The result is a mixed strategy over preview choices for each player, plus
//! P1's win probability.
//!
//! # Choice count
//!
//! Official doubles brings four Pokemon and leads with two of them.
//! One team of six gives 15 bring sets and 12 ordered lead pairs.
//! This makes 180 choices per side and 32,400 matrix cells.
//!
//! Each cell needs a full battle solve, so the search must not build the whole
//! matrix.
//! [`matrix::double_oracle`](super::matrix::double_oracle) therefore supplies
//! the equilibrium, and it reads only the cells that it needs.
//!
//! # Cell value
//!
//! One cell is one pair of preview choices.
//! `simulate_turn` applies both choices and returns weighted battle states.
//! Send-out abilities and speed ties create more than one branch.
//!
//! The cell value is the probability-weighted mean of the branch values.
//! A battle branch uses [`solve`](super::solve) with the caller's
//! [`SolveConfig`].
//! A game-over branch uses the terminal value.
//!
//! # Scope
//!
//! Both teams are known here, so the game is perfect information.
//! Open-list preview needs a belief over hidden numeric stats, a determinizer,
//! and a particle model.
//! That work is a separate task.
//!
//! ```no_run
//! # use poke_rust::data::pokemon_move::PokemonMove;
//! # use poke_rust::data::species::Species;
//! # use poke_rust::solver::preview::{PreviewConfig, solve_team_preview};
//! # use poke_rust::state::battle::TeamPreviewState;
//! # use poke_rust::state::dex_data::{MoveData, PokemonData};
//! # use std::collections::HashMap;
//! # fn demo(
//! #     state: &TeamPreviewState,
//! #     pokemon_dex: &HashMap<Species, PokemonData>,
//! #     move_dex: &HashMap<PokemonMove, MoveData>,
//! # ) {
//! let config = PreviewConfig::default();
//! let result = solve_team_preview(state, pokemon_dex, move_dex, &config)
//!     .expect("the preview state is well formed");
//! println!("P1 wins {:.1}% of the time", 100.0 * result.p1_win_odds);
//! for choice in &result.p1_strategy {
//!     println!("  {:.1}%  {:?}", 100.0 * choice.probability, choice.choice);
//! }
//! # }
//! ```

use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};

use crate::data::pokemon_move::PokemonMove;
use crate::data::species::Species;
use crate::simulator::simulate_turn;
use crate::state::battle::{
    MatchState, Player, PlayerCommand, TeamPreviewCommand, TeamPreviewState,
};
use crate::state::dex_data::{MoveData, PokemonData};

use super::chance::ChanceMode;
use super::matrix::{self, EPS, OracleLimits, OracleSeed};
use super::{SolveConfig, SolveWarning, solve};

/// P1's utility when P1 loses, and when P1 wins.
const LOSS: f64 = 0.0;
const WIN: f64 = 1.0;

/// The value of a position that the solver cannot score.
const EVEN: f64 = 0.5;

/// The official team-preview limit is 90 seconds.
/// The default leaves time for the caller to send the choice.
const DEFAULT_DEADLINE: Duration = Duration::from_secs(80);

// ── Choice enumeration ──────────────────────────────────────────────────────

/// Every legal bring-and-lead choice for one player, in a fixed order.
///
/// The enumeration takes each combination of `brought_per_side` team indices.
/// For each combination it takes each ordered arrangement of `active_per_side`
/// of those indices. The remaining brought indices become `back_indices` in
/// ascending order.
///
/// Lead order matters, because the slot index changes targets and speed-tie
/// order. Bench order does not change legal play, so the enumeration keeps one
/// ascending bench order.
///
/// The order is deterministic. An index therefore names the same choice in every
/// call for the same preview state.
///
/// Returns an empty list when the state asks for a choice that the team cannot
/// supply.
pub fn preview_choices(state: &TeamPreviewState, player: Player) -> Vec<TeamPreviewCommand> {
    let team = match player {
        Player::P1 => state.p1_mons.len(),
        Player::P2 => state.p2_mons.len(),
    };
    let brought = state.brought_per_side as usize;
    let active = state.active_per_side as usize;
    if active == 0 || active > brought || brought > team {
        return Vec::new();
    }

    let mut choices = Vec::new();
    for brought_set in combinations(team, brought) {
        for active_indices in arrangements(&brought_set, active) {
            let back_indices: Vec<usize> = brought_set
                .iter()
                .copied()
                .filter(|index| !active_indices.contains(index))
                .collect();
            choices.push(TeamPreviewCommand {
                active_indices,
                back_indices,
            });
        }
    }
    choices
}

/// Every ascending index set of size `choose` from `0..len`.
fn combinations(len: usize, choose: usize) -> Vec<Vec<usize>> {
    let mut out = Vec::new();
    if choose > len {
        return out;
    }
    let mut current: Vec<usize> = (0..choose).collect();
    loop {
        out.push(current.clone());
        // Advance the rightmost index that is not already at its ceiling, then
        // reset every index to its right.
        let mut slot = choose;
        loop {
            if slot == 0 {
                return out;
            }
            slot -= 1;
            if current[slot] != slot + len - choose {
                current[slot] += 1;
                for later in slot + 1..choose {
                    current[later] = current[later - 1] + 1;
                }
                break;
            }
        }
    }
}

/// Every ordered selection of `take` items from `items`.
fn arrangements(items: &[usize], take: usize) -> Vec<Vec<usize>> {
    if take == 0 {
        return vec![Vec::new()];
    }
    let mut out = Vec::new();
    for (position, &item) in items.iter().enumerate() {
        let mut rest = items.to_vec();
        rest.remove(position);
        for tail in arrangements(&rest, take - 1) {
            let mut one = Vec::with_capacity(take);
            one.push(item);
            one.extend(tail);
            out.push(one);
        }
    }
    out
}

// ── Configuration ───────────────────────────────────────────────────────────

/// Everything a preview solve needs beyond the position itself.
#[derive(Debug, Clone, Copy)]
pub struct PreviewConfig {
    /// The configuration of each battle solve below a preview cell.
    pub battle: SolveConfig,
    /// Wall-clock limit for the whole preview solve.
    /// After the limit the solver scores a cell with `battle.eval` instead of a
    /// battle search, and the result carries
    /// [`SolveWarning::DeadlineExceeded`].
    /// The solver does not start a new battle solve after the limit expires.
    /// A battle solve that is already active can finish after the limit.
    /// `None` keeps the solve exact.
    pub deadline: Option<Duration>,
}

impl Default for PreviewConfig {
    fn default() -> Self {
        PreviewConfig {
            battle: SolveConfig::default(),
            deadline: Some(DEFAULT_DEADLINE),
        }
    }
}

// ── Results ─────────────────────────────────────────────────────────────────

/// One preview choice and how often to play it.
#[derive(Debug, Clone)]
pub struct PreviewChoiceProb {
    /// The bring set and the lead order. Submit as
    /// `PlayerCommand::TeamPreview(choice)`.
    pub choice: TeamPreviewCommand,
    /// Probability of playing this choice. Sums to 1 across a strategy.
    pub probability: f64,
}

/// What a preview solve cost.
#[derive(Debug, Clone, Default)]
pub struct PreviewStats {
    /// Cells that the full matrix holds.
    pub cells_total: u64,
    /// Cells whose value the solver computed.
    /// The ratio against `cells_total` is what double-oracle pruning bought.
    pub cells_evaluated: u64,
    /// Cells that the cache answered.
    pub cell_cache_hits: u64,
    /// Battle branches that reached [`solve`](super::solve).
    pub battles_solved: u64,
    /// Completed `simulate_turn` calls, including the calls inside each battle
    /// solve.
    pub turns_simulated: u64,
    /// Matrix games that reached the simplex, in the preview game and in every
    /// battle solve.
    pub lps_solved: u64,
    /// Wall-clock time for the whole preview solve.
    pub elapsed: Duration,
}

/// An equilibrium over preview choices.
#[derive(Debug, Clone)]
pub struct PreviewResult {
    /// The game's value: P1's win probability under optimal play from both
    /// sides. Identical to `p1_win_odds`.
    pub value: f64,
    /// P1's odds of winning, in `[0, 1]`.
    pub p1_win_odds: f64,
    /// P2's odds of winning. The game is zero-sum, so this is `1 - p1_win_odds`.
    pub p2_win_odds: f64,
    /// P1's optimal mixed strategy. Choices at probability zero are omitted.
    pub p1_strategy: Vec<PreviewChoiceProb>,
    /// P2's optimal mixed strategy, likewise.
    pub p2_strategy: Vec<PreviewChoiceProb>,
    pub stats: PreviewStats,
    /// Why the answer is approximate. Holds the preview deadline warning and
    /// every distinct warning that a battle solve reported.
    pub warnings: Vec<SolveWarning>,
}

/// Why a preview position has no equilibrium.
#[derive(Debug, Clone, PartialEq)]
pub enum PreviewError {
    /// The player cannot form a legal bring-and-lead choice.
    /// The team is too small, or `active_per_side` is zero or larger than
    /// `brought_per_side`.
    NoLegalChoice { player: Player },
}

impl fmt::Display for PreviewError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PreviewError::NoLegalChoice { player } => write!(
                f,
                "{player:?} has no legal bring-and-lead choice in this preview state"
            ),
        }
    }
}

impl std::error::Error for PreviewError {}

// ── Cell cache ──────────────────────────────────────────────────────────────

/// Values of preview cells that the solver already computed.
///
/// The key holds hashes of the preview state, configuration, and dex data.
/// It also holds the row index and the column index.
///
/// The cache does not hold sampled cells or cells with a battle deadline.
/// A cell that the solver scores after the preview deadline does not enter it.
///
/// A caller can pass the same cache to a later solve of the same preview state.
#[derive(Debug, Clone, Default)]
pub struct PreviewCellCache {
    values: HashMap<CellKey, CachedCell>,
}

type CellKey = (u64, u64, u64, usize, usize);

#[derive(Debug, Clone)]
struct CachedCell {
    value: f64,
    warnings: Vec<SolveWarning>,
}

impl PreviewCellCache {
    pub fn new() -> Self {
        PreviewCellCache::default()
    }

    /// The number of stored cells.
    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// Removes every stored cell.
    pub fn clear(&mut self) {
        self.values.clear();
    }
}

/// A hash of the preview position.
fn preview_key(state: &TeamPreviewState) -> u64 {
    let mut hasher = DefaultHasher::new();
    state.hash(&mut hasher);
    hasher.finish()
}

/// A hash of every configuration field that changes a cell value.
///
/// `tt_capacity` and `turn_cache_capacity` are memo sizes and cannot change a
/// value, so the key leaves them out.
fn config_key(config: &PreviewConfig) -> u64 {
    let battle = &config.battle;
    let mut hasher = DefaultHasher::new();
    battle.depth.hash(&mut hasher);
    battle.iterative_deepening.hash(&mut hasher);
    battle.damage_rolls.hash(&mut hasher);
    battle.consider_crit.hash(&mut hasher);
    chance_key(&battle.chance).hash(&mut hasher);
    (battle.algorithm as u8).hash(&mut hasher);
    (battle.eval as usize).hash(&mut hasher);
    battle.use_serialized_bounds.hash(&mut hasher);
    battle.max_actions_per_player.hash(&mut hasher);
    battle.node_budget.hash(&mut hasher);
    battle.deadline.hash(&mut hasher);
    battle.max_forced_chain.hash(&mut hasher);
    hasher.finish()
}

/// A hash of the data that can change a battle result.
fn dex_key(
    pokemon_dex: &HashMap<Species, PokemonData>,
    move_dex: &HashMap<PokemonMove, MoveData>,
) -> u64 {
    let mut hasher = DefaultHasher::new();

    let mut pokemon: Vec<_> = pokemon_dex.iter().collect();
    pokemon.sort_unstable_by(|(left, _), (right, _)| left.cmp(right));
    for (species, data) in pokemon {
        species.hash(&mut hasher);
        format_args!("{data:?}").to_string().hash(&mut hasher);
    }

    let mut moves: Vec<_> = move_dex.iter().collect();
    moves.sort_unstable_by(|(left, _), (right, _)| left.cmp(right));
    for (name, data) in moves {
        name.hash(&mut hasher);
        format_args!("{data:?}").to_string().hash(&mut hasher);
    }

    hasher.finish()
}

/// True when a second call with the same inputs must return the same cell.
fn cells_are_cacheable(config: &PreviewConfig) -> bool {
    !matches!(config.battle.chance, ChanceMode::Sample(_)) && config.battle.deadline.is_none()
}

/// A hashable form of [`ChanceMode`], which holds a float.
fn chance_key(chance: &ChanceMode) -> (u8, u64) {
    match *chance {
        ChanceMode::Enumerate => (0, 0),
        ChanceMode::TopK(k) => (1, k as u64),
        ChanceMode::Threshold(t) => (2, t.to_bits()),
        ChanceMode::Sample(n) => (3, n as u64),
    }
}

// ── Entry points ────────────────────────────────────────────────────────────

/// Solves `state` as a simultaneous choice of a bring set and a lead order.
///
/// Set `VERBOSITY` to zero before a large search.
pub fn solve_team_preview(
    state: &TeamPreviewState,
    pokemon_dex: &HashMap<Species, PokemonData>,
    move_dex: &HashMap<PokemonMove, MoveData>,
    config: &PreviewConfig,
) -> Result<PreviewResult, PreviewError> {
    let mut cache = PreviewCellCache::new();
    solve_team_preview_cached(state, pokemon_dex, move_dex, config, &mut cache)
}

/// [`solve_team_preview`], reading and writing `cache`.
///
/// The solve reads every cell that `cache` already holds. It writes every exact
/// cell that it computes.
pub fn solve_team_preview_cached(
    state: &TeamPreviewState,
    pokemon_dex: &HashMap<Species, PokemonData>,
    move_dex: &HashMap<PokemonMove, MoveData>,
    config: &PreviewConfig,
    cache: &mut PreviewCellCache,
) -> Result<PreviewResult, PreviewError> {
    let mut ctx = PreviewContext::new(state, pokemon_dex, move_dex, config, cache)?;
    let rows = ctx.p1.len();
    let cols = ctx.p2.len();

    let (solution, oracle) = matrix::double_oracle(
        rows,
        cols,
        OracleSeed::default(),
        OracleLimits {
            alpha: LOSS,
            beta: WIN,
            low: LOSS,
            high: WIN,
        },
        |row, col| ctx.cell_value(row, col),
    );
    ctx.stats.lps_solved += oracle.lps_solved;
    ctx.stats.cells_total = (rows * cols) as u64;
    ctx.stats.elapsed = ctx.started.elapsed();

    let value = solution.value.clamp(LOSS, WIN);
    let mut warnings = ctx.warnings;
    if let (true, Some(budget)) = (ctx.deadline_hit, config.deadline) {
        warnings.insert(0, SolveWarning::DeadlineExceeded { budget });
    }

    Ok(PreviewResult {
        value,
        p1_win_odds: value,
        p2_win_odds: WIN - value,
        p1_strategy: strategy_of(&ctx.p1, &solution.row_strategy),
        p2_strategy: strategy_of(&ctx.p2, &solution.col_strategy),
        stats: ctx.stats,
        warnings,
    })
}

/// Computes the requested cells and writes them into `cache`.
///
/// A caller can run this before an interactive solve, so that the solve reads
/// the cells instead of computing them.
///
/// Each cell is a row index into P1's choices and a column index into P2's
/// choices. [`preview_choices`] returns those choices in the same order. The
/// function skips an index that is out of range.
pub fn precompute_preview_cells(
    state: &TeamPreviewState,
    pokemon_dex: &HashMap<Species, PokemonData>,
    move_dex: &HashMap<PokemonMove, MoveData>,
    config: &PreviewConfig,
    cells: &[(usize, usize)],
    cache: &mut PreviewCellCache,
) -> Result<PreviewStats, PreviewError> {
    let mut ctx = PreviewContext::new(state, pokemon_dex, move_dex, config, cache)?;
    let rows = ctx.p1.len();
    let cols = ctx.p2.len();

    for &(row, col) in cells {
        if row >= rows || col >= cols {
            continue;
        }
        ctx.cell_value(row, col);
    }

    ctx.stats.cells_total = (rows * cols) as u64;
    ctx.stats.elapsed = ctx.started.elapsed();
    Ok(ctx.stats)
}

/// The value of one preview cell.
///
/// This uses a private cache, so it never reads or writes a caller's cache.
/// Tests use it to check the double-oracle result against the full matrix.
pub fn preview_cell_value(
    state: &TeamPreviewState,
    pokemon_dex: &HashMap<Species, PokemonData>,
    move_dex: &HashMap<PokemonMove, MoveData>,
    config: &PreviewConfig,
    p1_choice: &TeamPreviewCommand,
    p2_choice: &TeamPreviewCommand,
) -> f64 {
    let mut cache = PreviewCellCache::new();
    let mut ctx = PreviewContext {
        pokemon_dex,
        move_dex,
        config,
        match_state: MatchState::TeamPreviewState(state.clone()),
        p1: vec![p1_choice.clone()],
        p2: vec![p2_choice.clone()],
        preview_key: preview_key(state),
        config_key: config_key(config),
        dex_key: dex_key(pokemon_dex, move_dex),
        cacheable: cells_are_cacheable(config),
        cache: &mut cache,
        stats: PreviewStats::default(),
        started: Instant::now(),
        deadline_hit: false,
        warnings: Vec::new(),
    };
    ctx.cell_value(0, 0)
}

/// Pair choices with their probabilities, dropping the ones never played.
fn strategy_of(choices: &[TeamPreviewCommand], probabilities: &[f64]) -> Vec<PreviewChoiceProb> {
    let mut strategy: Vec<PreviewChoiceProb> = choices
        .iter()
        .zip(probabilities)
        .filter(|&(_, &probability)| probability > EPS)
        .map(|(choice, &probability)| PreviewChoiceProb {
            choice: choice.clone(),
            probability,
        })
        .collect();
    strategy.sort_by(|a, b| b.probability.total_cmp(&a.probability));
    strategy
}

// ── The cell oracle ─────────────────────────────────────────────────────────

/// Everything one preview solve needs while it runs.
struct PreviewContext<'a> {
    pokemon_dex: &'a HashMap<Species, PokemonData>,
    move_dex: &'a HashMap<PokemonMove, MoveData>,
    config: &'a PreviewConfig,
    /// The preview position, held as the type that `simulate_turn` takes.
    match_state: MatchState,
    p1: Vec<TeamPreviewCommand>,
    p2: Vec<TeamPreviewCommand>,
    preview_key: u64,
    config_key: u64,
    dex_key: u64,
    cacheable: bool,
    cache: &'a mut PreviewCellCache,
    stats: PreviewStats,
    started: Instant,
    deadline_hit: bool,
    warnings: Vec<SolveWarning>,
}

impl<'a> PreviewContext<'a> {
    fn new(
        state: &TeamPreviewState,
        pokemon_dex: &'a HashMap<Species, PokemonData>,
        move_dex: &'a HashMap<PokemonMove, MoveData>,
        config: &'a PreviewConfig,
        cache: &'a mut PreviewCellCache,
    ) -> Result<Self, PreviewError> {
        let p1 = preview_choices(state, Player::P1);
        if p1.is_empty() {
            return Err(PreviewError::NoLegalChoice { player: Player::P1 });
        }
        let p2 = preview_choices(state, Player::P2);
        if p2.is_empty() {
            return Err(PreviewError::NoLegalChoice { player: Player::P2 });
        }

        Ok(PreviewContext {
            pokemon_dex,
            move_dex,
            config,
            match_state: MatchState::TeamPreviewState(state.clone()),
            p1,
            p2,
            preview_key: preview_key(state),
            config_key: config_key(config),
            dex_key: dex_key(pokemon_dex, move_dex),
            cacheable: cells_are_cacheable(config),
            cache,
            stats: PreviewStats::default(),
            started: Instant::now(),
            deadline_hit: false,
            warnings: Vec::new(),
        })
    }

    /// The value of one cell, from the cache when the cache holds it.
    fn cell_value(&mut self, row: usize, col: usize) -> f64 {
        let key = (self.preview_key, self.config_key, self.dex_key, row, col);
        if self.cacheable
            && let Some(cell) = self.cache.values.get(&key).cloned()
        {
            self.stats.cell_cache_hits += 1;
            for warning in cell.warnings {
                merge_warning(&mut self.warnings, warning);
            }
            return cell.value;
        }

        // Read once, so that the whole cell uses one answer. A cell that mixes a
        // searched branch with an evaluated branch would be neither exact nor
        // reproducible.
        let expired = self.deadline_expired();
        let (value, cell_warnings) = self.evaluate(row, col, expired);
        self.stats.cells_evaluated += 1;
        for warning in &cell_warnings {
            merge_warning(&mut self.warnings, warning.clone());
        }
        if self.cacheable && !expired {
            self.cache.values.insert(
                key,
                CachedCell {
                    value,
                    warnings: cell_warnings,
                },
            );
        }
        value
    }

    /// Applies both preview choices and averages the branch values.
    fn evaluate(&mut self, row: usize, col: usize, expired: bool) -> (f64, Vec<SolveWarning>) {
        let p1_command = PlayerCommand::TeamPreview(self.p1[row].clone());
        let p2_command = PlayerCommand::TeamPreview(self.p2[col].clone());
        let branches = simulate_turn(
            &self.match_state,
            &p1_command,
            &p2_command,
            self.move_dex,
            self.pokemon_dex,
            self.config.battle.consider_crit,
            self.config.battle.damage_rolls,
            None,
        );
        self.stats.turns_simulated += 1;

        let total: f64 = branches.iter().map(|(_, _, probability)| probability).sum();
        if total <= 0.0 {
            return (EVEN, Vec::new());
        }

        let mut expected = 0.0;
        let mut warnings = Vec::new();
        for (child, _, probability) in branches {
            if probability <= 0.0 {
                continue;
            }
            expected += probability * self.branch_value(&child, expired, &mut warnings);
        }
        // The branch probabilities already sum to 1. The division protects the
        // range of the result against rounding in a long branch list.
        (expected / total, warnings)
    }

    /// The value of one battle that a preview choice pair produced.
    fn branch_value(
        &mut self,
        child: &MatchState,
        expired: bool,
        warnings: &mut Vec<SolveWarning>,
    ) -> f64 {
        let battle = match child {
            MatchState::GameOverState { winner, .. } => {
                return match winner {
                    Player::P1 => WIN,
                    Player::P2 => LOSS,
                };
            }
            // `simulate_turn` never returns a preview state from a preview
            // state. Scoring it as even is the only neutral answer.
            MatchState::TeamPreviewState(_) => return EVEN,
            MatchState::BattleState(battle) => battle,
        };

        if expired {
            return (self.config.battle.eval)(battle);
        }

        match solve(child, self.pokemon_dex, self.move_dex, &self.config.battle) {
            Ok(result) => {
                self.stats.battles_solved += 1;
                self.stats.turns_simulated += result.stats.turns_simulated;
                self.stats.lps_solved += result.stats.lps_solved;
                for warning in result.warnings {
                    merge_warning(warnings, warning);
                }
                result.value
            }
            // A finished battle arrives as a game-over state, and a preview state
            // cannot reach this line. Score the position instead of failing.
            Err(_) => (self.config.battle.eval)(battle),
        }
    }

    /// Checks the deadline and saves the result for the solve warning.
    fn deadline_expired(&mut self) -> bool {
        if let Some(deadline) = self.config.deadline
            && self.started.elapsed() >= deadline
        {
            self.deadline_hit = true;
            return true;
        }
        false
    }
}

/// Adds one warning without repeating it.
fn merge_warning(warnings: &mut Vec<SolveWarning>, warning: SolveWarning) {
    if let SolveWarning::ChanceMassDiscarded { max_fraction } = warning {
        for existing in warnings.iter_mut() {
            if let SolveWarning::ChanceMassDiscarded {
                max_fraction: current,
            } = existing
            {
                *current = current.max(max_fraction);
                return;
            }
        }
        warnings.push(warning);
        return;
    }
    if !warnings.contains(&warning) {
        warnings.push(warning);
    }
}
