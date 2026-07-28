//! The recursion: backward induction over simultaneous-move stochastic nodes.
//!
//! A node is a position where both players choose at once. Its payoff matrix has
//! P1's joint actions as rows and P2's as columns, and each cell is the *expected*
//! value of that pair of choices — expected because the engine answers a joint
//! action with a probability distribution over successor states, not one state.
//! Solving the matrix (`super::matrix`) gives the node's value and both players'
//! mixed strategies; the value propagates upward.
//!
//! Three algorithms compute the same answer with different amounts of work; see
//! [`SolverAlgorithm`]. All are from Bošanský et al. (AIJ 237, 2016), Algorithms
//! 1–4.
//!
//! # Where bounds are and are not allowed
//!
//! This is the one thing in the module that is easy to get quietly wrong.
//!
//! Alpha-beta style pruning returns a *bound* on a value rather than the value.
//! A matrix cell fed to the LP must be **exact** — a linear program over
//! interval-valued cells does not compute an equilibrium, it computes nonsense,
//! and it does so without complaining. So cells are always evaluated over the
//! full `[0, 1]` window.
//!
//! Bounds are legitimate in exactly two places:
//!
//! - Inside [`SearchContext::serial_ab`], the *serialized* search, which is an
//!   ordinary alternating-move alpha-beta search where cutoffs are sound, and
//!   where star1 pruning at chance nodes is likewise sound.
//! - As the `(α, β)` window handed to a recursive simultaneous solve, when that
//!   window came from serialized bounds. Those brackets are *valid* — they
//!   provably contain the true value — so narrowing to them never excludes the
//!   answer, and the value returned is still exact.
//!
//! Because every simultaneous window either is `[0, 1]` or provably contains the
//! true value, every value the simultaneous search produces is exact. That is
//! what lets the transposition table store bare numbers with no bound flags.
//!
//! # Mid-turn decision points do not consume a ply
//!
//! One `simulate_turn` call does not always advance the turn counter: a faint
//! leaves the battle in a replacement phase, and a self-switch move (U-turn,
//! Baton Pass) leaves it waiting for a pivot. Both are real simultaneous-move
//! decision nodes and are searched as such — but charging them a ply would mean
//! a depth-3 search that hit two faints was really looking one turn ahead. They
//! recurse at the same depth, bounded by
//! [`SolveConfig::max_forced_chain`](super::SolveConfig::max_forced_chain).

use std::collections::HashMap;
use std::collections::VecDeque;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::time::Instant;

use crate::data::pokemon_move::PokemonMove;
use crate::data::species::Species;
use crate::simulator::simulate_turn;
use crate::state::battle::{BattleCommand, BattleState, MatchState, Player, PlayerCommand};
use crate::state::dex_data::{MoveData, PokemonData};

use super::actions::{self, JointActions, Phase};
use super::matrix::{self, EPS, MatrixSolution};
use super::{
    JointActionProb, SolveConfig, SolveError, SolveResult, SolveStats, SolveWarning,
    SolverAlgorithm,
};

/// P1's utility when P1 loses, and when P1 wins. Every value in the search lies
/// between these, which is what makes them usable as the `L` and `U` that star1
/// pruning needs.
const LOSS: f64 = 0.0;
const WIN: f64 = 1.0;

/// Distinguishes the three searches that visit the same positions — the
/// simultaneous game and its two serializations — so their values cannot be
/// confused for one another in the shared cache keyspace.
const SALT_SIMULTANEOUS: u64 = 0x243F_6A88_85A3_08D3;

/// Entry point behind [`super::solve`].
pub(super) fn run(
    state: &MatchState,
    pokemon_dex: &HashMap<Species, PokemonData>,
    move_dex: &HashMap<PokemonMove, MoveData>,
    config: &SolveConfig,
) -> Result<SolveResult, SolveError> {
    match state {
        MatchState::TeamPreviewState(_) => return Err(SolveError::TeamPreviewUnsupported),
        MatchState::GameOverState { winner, .. } => {
            return Err(SolveError::GameAlreadyOver { winner: *winner });
        }
        MatchState::BattleState(_) => {}
    }

    let started = Instant::now();
    let mut ctx = SearchContext::new(pokemon_dex, move_dex, config);

    // Depth 0 would make every cell an evaluation of the same position, so the
    // matrix would be constant and the "equilibrium" arbitrary. One ply is the
    // minimum that means anything.
    let depth = config.depth.max(1);
    let position = ctx.solve_position(state, depth, 0, LOSS, WIN);

    let value = position.value.clamp(LOSS, WIN);
    let mut warnings = Vec::new();
    if let (true, Some(budget)) = (ctx.budget_hit, config.node_budget) {
        warnings.push(SolveWarning::BudgetExhausted { budget });
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
    stats.elapsed = started.elapsed();

    Ok(SolveResult {
        value,
        p1_win_odds: value,
        p2_win_odds: WIN - value,
        p1_strategy: strategy_of(&position.p1, &position.row_strategy),
        p2_strategy: strategy_of(&position.p2, &position.col_strategy),
        stats,
        warnings,
    })
}

/// Pair actions with their probabilities, dropping the ones never played.
fn strategy_of(joint: &JointActions, probabilities: &[f64]) -> Vec<JointActionProb> {
    let mut strategy: Vec<JointActionProb> = joint
        .actions
        .iter()
        .zip(probabilities)
        .filter(|&(_, &p)| p > EPS)
        .map(|(commands, &probability)| JointActionProb {
            commands: commands.clone(),
            probability,
        })
        .collect();
    strategy.sort_by(|a, b| b.probability.total_cmp(&a.probability));
    strategy
}

/// A solved position: its value, both players' action sets, and their strategies
/// over those sets.
struct Position {
    value: f64,
    p1: JointActions,
    p2: JointActions,
    row_strategy: Vec<f64>,
    col_strategy: Vec<f64>,
}

struct SearchContext<'a> {
    pokemon_dex: &'a HashMap<Species, PokemonData>,
    move_dex: &'a HashMap<PokemonMove, MoveData>,
    cfg: &'a SolveConfig,
    stats: SolveStats,
    tt: TranspositionTable,
    turn_cache: TurnCache,
    /// Largest fraction of outcome probability dropped at any one chance node.
    max_discarded: f64,
    /// Largest action-set truncation encountered for each player anywhere in
    /// the search. Child nodes matter just as much as the root: once any node is
    /// capped, the returned equilibrium is approximate.
    action_truncations: [Option<(usize, usize)>; 2],
    budget_hit: bool,
}

impl<'a> SearchContext<'a> {
    fn new(
        pokemon_dex: &'a HashMap<Species, PokemonData>,
        move_dex: &'a HashMap<PokemonMove, MoveData>,
        cfg: &'a SolveConfig,
    ) -> Self {
        SearchContext {
            pokemon_dex,
            move_dex,
            cfg,
            stats: SolveStats::default(),
            tt: TranspositionTable::new(cfg.tt_capacity),
            turn_cache: TurnCache::new(cfg.turn_cache_capacity),
            max_discarded: 0.0,
            action_truncations: [None, None],
            budget_hit: false,
        }
    }

    /// Whether serialized alpha-beta bounds should be computed at each node.
    fn serial_bounds_enabled(&self) -> bool {
        self.cfg.algorithm == SolverAlgorithm::SerializedBounds || self.cfg.use_serialized_bounds
    }

    /// Whether the node budget is spent. Latches `budget_hit` so the caller can
    /// report it once, rather than the search failing loudly mid-flight.
    fn over_budget(&mut self) -> bool {
        match self.cfg.node_budget {
            Some(budget) if self.stats.nodes_expanded >= budget => {
                self.budget_hit = true;
                true
            }
            _ => false,
        }
    }

    // ── The simultaneous-move search ────────────────────────────────────────

    /// The value of `state` to P1, searching `depth` further turns.
    ///
    /// `alpha`/`beta` must be a window that provably contains the true value —
    /// either the full `[LOSS, WIN]` or a pair of serialized bounds. Under that
    /// precondition the returned value is exact, which is what the transposition
    /// table relies on.
    fn node_value(
        &mut self,
        state: &MatchState,
        depth: u8,
        chain: u8,
        mut alpha: f64,
        mut beta: f64,
    ) -> f64 {
        let battle = match state {
            MatchState::GameOverState { winner, .. } => return terminal_value(*winner),
            // Not reachable from a battle position; scoring it as even is the
            // only neutral answer.
            MatchState::TeamPreviewState(_) => return 0.5,
            MatchState::BattleState(battle) => battle,
        };

        if depth == 0 || self.over_budget() {
            return (self.cfg.eval)(battle);
        }

        let key = hash_state(state) ^ SALT_SIMULTANEOUS;
        if let Some(value) = self.tt.probe(key, depth, chain) {
            self.stats.tt_hits += 1;
            return value;
        }

        if self.serial_bounds_enabled() {
            // Letting a player move second can only help them, so a serialized
            // search in which P1 moves second over-estimates P1's value and one
            // in which P2 moves second under-estimates it (Lemmas 4.1/4.2).
            let lower = self.serial_ab(state, depth, chain, LOSS, WIN, Player::P2);
            let upper = self.serial_ab(state, depth, chain, LOSS, WIN, Player::P1);
            if (upper - lower).abs() <= EPS {
                // The brackets met: this subgame has a pure equilibrium and the
                // matrix never has to be built at all.
                self.tt.store(key, depth, chain, lower);
                return lower;
            }
            alpha = alpha.max(lower);
            beta = beta.min(upper);
        }

        let value = self.solve_position(state, depth, chain, alpha, beta).value;
        self.tt.store(key, depth, chain, value);
        value
    }

    /// Build and solve the matrix game at `state`.
    fn solve_position(
        &mut self,
        state: &MatchState,
        depth: u8,
        chain: u8,
        alpha: f64,
        beta: f64,
    ) -> Position {
        let battle = as_battle(state).expect("solve_position requires a battle position");
        let phase = actions::phase_of(state);
        let p1 = self.joint_actions(battle, Player::P1, phase);
        let p2 = self.joint_actions(battle, Player::P2, phase);

        self.stats.nodes_expanded += 1;
        self.stats.matrix_cells_total += (p1.actions.len() * p2.actions.len()) as u64;

        // Nobody has a choice: no game, no LP, just the one outcome. Common,
        // because a replacement or self-switch usually forces one side entirely.
        if p1.actions.len() == 1 && p2.actions.len() == 1 {
            let value = self.cell_value(state, &p1.actions[0], &p2.actions[0], depth, chain);
            return Position {
                value,
                p1,
                p2,
                row_strategy: vec![1.0],
                col_strategy: vec![1.0],
            };
        }

        let solution = match self.cfg.algorithm {
            SolverAlgorithm::DoubleOracle => {
                self.double_oracle(state, depth, chain, alpha, beta, &p1.actions, &p2.actions)
            }
            SolverAlgorithm::BackwardInduction | SolverAlgorithm::SerializedBounds => {
                self.full_matrix(state, depth, chain, &p1.actions, &p2.actions)
            }
        };

        Position {
            value: solution.value,
            p1,
            p2,
            row_strategy: solution.row_strategy,
            col_strategy: solution.col_strategy,
        }
    }

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

    /// Algorithm 1: evaluate every cell, then solve.
    fn full_matrix(
        &mut self,
        state: &MatchState,
        depth: u8,
        chain: u8,
        rows: &[Vec<BattleCommand>],
        cols: &[Vec<BattleCommand>],
    ) -> MatrixSolution {
        let payoffs: Vec<Vec<f64>> = rows
            .iter()
            .map(|row| {
                cols.iter()
                    .map(|col| self.cell_value(state, row, col, depth, chain))
                    .collect::<Vec<f64>>()
            })
            .collect();
        let solution = matrix::solve_matrix_game(&payoffs);
        if solution.used_lp {
            self.stats.lps_solved += 1;
        }
        solution
    }

    /// Algorithm 3: solve the matrix without building all of it.
    ///
    /// Start from a one-by-one restricted game. Solve it, then ask each player
    /// for their best response *over the full action set* — if neither can
    /// improve on the restricted equilibrium, it is an equilibrium of the whole
    /// game and the remaining cells never mattered. Otherwise add the two best
    /// responses as a new row and column and repeat.
    ///
    /// The two best-response values bracket the true value from above and below,
    /// which is both the termination test and the source of the bound tightening
    /// that makes the next round's pruning sharper.
    #[allow(clippy::too_many_arguments)]
    fn double_oracle(
        &mut self,
        state: &MatchState,
        depth: u8,
        chain: u8,
        mut alpha: f64,
        mut beta: f64,
        rows: &[Vec<BattleCommand>],
        cols: &[Vec<BattleCommand>],
    ) -> MatrixSolution {
        let m = rows.len();
        let n = cols.len();

        // Lazily filled; shared across best-response calls so no cell is ever
        // computed twice within this node. That sharing is most of the win.
        let mut cells: Vec<Vec<Option<f64>>> = vec![vec![None; n]; m];
        let mut restricted_rows: Vec<usize> = vec![0];
        let mut restricted_cols: Vec<usize> = vec![0];

        let mut best = MatrixSolution {
            value: 0.5,
            row_strategy: vec![0.0; m],
            col_strategy: vec![0.0; n],
            used_lp: false,
        };

        // Each round adds at least one action to at least one side or stops, so
        // this can only be reached if something is badly wrong.
        for _ in 0..(m + n + 2) {
            for &i in &restricted_rows {
                for &j in &restricted_cols {
                    if cells[i][j].is_none() {
                        cells[i][j] =
                            Some(self.cell_value(state, &rows[i], &cols[j], depth, chain));
                    }
                }
            }

            let sub: Vec<Vec<f64>> = restricted_rows
                .iter()
                .map(|&i| {
                    restricted_cols
                        .iter()
                        .map(|&j| cells[i][j].expect("restricted cell was just filled"))
                        .collect()
                })
                .collect();
            let solution = matrix::solve_matrix_game(&sub);
            if solution.used_lp {
                self.stats.lps_solved += 1;
            }

            let row_strategy = scatter(&solution.row_strategy, &restricted_rows, m);
            let col_strategy = scatter(&solution.col_strategy, &restricted_cols, n);
            best = MatrixSolution {
                value: solution.value,
                row_strategy: row_strategy.clone(),
                col_strategy: col_strategy.clone(),
                used_lp: solution.used_lp,
            };

            let (row_br_value, row_br) =
                self.best_response_row(state, depth, chain, &mut cells, rows, cols, &col_strategy);
            let (col_br_value, col_br) =
                self.best_response_col(state, depth, chain, &mut cells, rows, cols, &row_strategy);

            // P2 can hold P1 to `row_br_value` by playing `col_strategy`, and P1
            // can guarantee `col_br_value` by playing `row_strategy` — both are
            // full-game strategies, so these bracket the true value.
            beta = beta.min(row_br_value);
            alpha = alpha.max(col_br_value);
            if beta - alpha <= EPS {
                break;
            }

            let mut grew = false;
            if !restricted_rows.contains(&row_br) {
                restricted_rows.push(row_br);
                grew = true;
            }
            if !restricted_cols.contains(&col_br) {
                restricted_cols.push(col_br);
                grew = true;
            }
            // Both best responses are already in the restricted game: neither
            // player has anywhere better to go.
            if !grew {
                break;
            }
        }

        // `best.value` is the *restricted* game's value, which equals the true
        // value once the loop has converged. It can differ if the loop instead
        // stopped because an incoming serialized-bounds window closed the
        // bracket first. Both `alpha` and `beta` are always valid bounds on the
        // true value, so clamping into them is a no-op in the converged case and
        // caps the error at the bracket width otherwise.
        //
        // Ordered explicitly rather than with `clamp`, which panics on an
        // inverted range: at convergence the two best-response values are the
        // same quantity summed in different orders — row-major against the
        // column strategy, column-major against the row strategy — so they can
        // cross by one ulp.
        let (low, high) = (alpha.min(beta), alpha.max(beta));
        best.value = best.value.clamp(low, high);
        best
    }

    /// P1's best pure response to `col_strategy`, and its value.
    ///
    /// Rows are abandoned as soon as they cannot catch the best row found so
    /// far, judging unevaluated cells at their upper bound. This is the paper's
    /// λ test rearranged: rather than deriving the payoff a cell would have to
    /// deliver and comparing it against that cell's bound, compare the row's
    /// optimistic completion against the incumbent directly. Both skip exactly
    /// the same rows, and the bound tightens mid-row as cells become known.
    #[allow(clippy::too_many_arguments)]
    fn best_response_row(
        &mut self,
        state: &MatchState,
        depth: u8,
        chain: u8,
        cells: &mut [Vec<Option<f64>>],
        rows: &[Vec<BattleCommand>],
        cols: &[Vec<BattleCommand>],
        col_strategy: &[f64],
    ) -> (f64, usize) {
        let support: Vec<usize> = (0..col_strategy.len())
            .filter(|&j| col_strategy[j] > EPS)
            .collect();

        let mut best_value = f64::NEG_INFINITY;
        let mut best_row = 0;

        for i in 0..rows.len() {
            let mut accumulated = 0.0;
            let mut abandoned = false;

            for (k, &j) in support.iter().enumerate() {
                let optimistic = accumulated
                    + support[k..]
                        .iter()
                        .map(|&jj| col_strategy[jj] * cells[i][jj].unwrap_or(WIN))
                        .sum::<f64>();
                if optimistic < best_value - EPS {
                    self.stats.ab_cutoffs += 1;
                    abandoned = true;
                    break;
                }

                let value = match cells[i][j] {
                    Some(value) => value,
                    None => {
                        let value = self.cell_value(state, &rows[i], &cols[j], depth, chain);
                        cells[i][j] = Some(value);
                        value
                    }
                };
                accumulated += col_strategy[j] * value;
            }

            if !abandoned && accumulated > best_value {
                best_value = accumulated;
                best_row = i;
            }
        }

        (best_value, best_row)
    }

    /// P2's best pure response to `row_strategy`, and its value in P1's terms —
    /// so P2 is looking for the *smallest* number. The mirror of
    /// [`Self::best_response_row`], judging unevaluated cells at their lower
    /// bound.
    #[allow(clippy::too_many_arguments)]
    fn best_response_col(
        &mut self,
        state: &MatchState,
        depth: u8,
        chain: u8,
        cells: &mut [Vec<Option<f64>>],
        rows: &[Vec<BattleCommand>],
        cols: &[Vec<BattleCommand>],
        row_strategy: &[f64],
    ) -> (f64, usize) {
        let support: Vec<usize> = (0..row_strategy.len())
            .filter(|&i| row_strategy[i] > EPS)
            .collect();

        let mut best_value = f64::INFINITY;
        let mut best_col = 0;

        for j in 0..cols.len() {
            let mut accumulated = 0.0;
            let mut abandoned = false;

            for (k, &i) in support.iter().enumerate() {
                let pessimistic = accumulated
                    + support[k..]
                        .iter()
                        .map(|&ii| row_strategy[ii] * cells[ii][j].unwrap_or(LOSS))
                        .sum::<f64>();
                if pessimistic > best_value + EPS {
                    self.stats.ab_cutoffs += 1;
                    abandoned = true;
                    break;
                }

                let value = match cells[i][j] {
                    Some(value) => value,
                    None => {
                        let value = self.cell_value(state, &rows[i], &cols[j], depth, chain);
                        cells[i][j] = Some(value);
                        value
                    }
                };
                accumulated += row_strategy[i] * value;
            }

            if !abandoned && accumulated < best_value {
                best_value = accumulated;
                best_col = j;
            }
        }

        (best_value, best_col)
    }

    /// One matrix cell: the expected value over everything the engine might do
    /// in response to this pair of commands.
    ///
    /// Always evaluated over the full window. See the module documentation for
    /// why a cell may never be a bound.
    fn cell_value(
        &mut self,
        state: &MatchState,
        p1_commands: &[BattleCommand],
        p2_commands: &[BattleCommand],
        depth: u8,
        chain: u8,
    ) -> f64 {
        self.stats.matrix_cells_evaluated += 1;
        let branches = self.resolve(state, p1_commands, p2_commands);

        let mut expected = 0.0;
        // Consumed rather than borrowed, so each successor's own subtree is
        // dropped before the next one is expanded — this is what keeps peak
        // memory proportional to depth instead of to the whole tree.
        for (child, probability) in branches {
            if probability <= 0.0 {
                continue;
            }
            let (child_depth, child_chain) = self.descend(&child, depth, chain);
            expected += probability * self.node_value(&child, child_depth, child_chain, LOSS, WIN);
        }
        expected
    }

    // ── The serialized search ───────────────────────────────────────────────

    /// Alpha-beta over the *serialization* in which `second` moves knowing what
    /// the other player chose.
    ///
    /// The extra information can only help `second`, so the result over-estimates
    /// `second`'s prospects: with `second == P1` the result is an upper bound on
    /// the position's true value, and with `second == P2` a lower bound. This is
    /// an ordinary alternating-move search, so cutoffs here are sound in the way
    /// they are not in the simultaneous search.
    fn serial_ab(
        &mut self,
        state: &MatchState,
        depth: u8,
        chain: u8,
        mut alpha: f64,
        mut beta: f64,
        second: Player,
    ) -> f64 {
        let battle = match state {
            MatchState::GameOverState { winner, .. } => return terminal_value(*winner),
            MatchState::TeamPreviewState(_) => return 0.5,
            MatchState::BattleState(battle) => battle,
        };
        if depth == 0 || self.over_budget() {
            return (self.cfg.eval)(battle);
        }

        let phase = actions::phase_of(state);
        let p1 = self.joint_actions(battle, Player::P1, phase);
        let p2 = self.joint_actions(battle, Player::P2, phase);

        if second == Player::P2 {
            // P1 commits first and maximizes; P2 answers and minimizes.
            let mut best = LOSS;
            for p1_action in &p1.actions {
                let mut answer = WIN;
                let mut answer_beta = beta;
                for p2_action in &p2.actions {
                    let value = self.serial_cell(
                        state, p1_action, p2_action, depth, chain, alpha, answer_beta, second,
                    );
                    answer = answer.min(value);
                    answer_beta = answer_beta.min(answer);
                    if answer <= alpha {
                        self.stats.ab_cutoffs += 1;
                        break;
                    }
                }
                best = best.max(answer);
                alpha = alpha.max(best);
                if alpha >= beta {
                    self.stats.ab_cutoffs += 1;
                    break;
                }
            }
            best
        } else {
            // P2 commits first and minimizes; P1 answers and maximizes.
            let mut best = WIN;
            for p2_action in &p2.actions {
                let mut answer = LOSS;
                let mut answer_alpha = alpha;
                for p1_action in &p1.actions {
                    let value = self.serial_cell(
                        state, p1_action, p2_action, depth, chain, answer_alpha, beta, second,
                    );
                    answer = answer.max(value);
                    answer_alpha = answer_alpha.max(answer);
                    if answer >= beta {
                        self.stats.ab_cutoffs += 1;
                        break;
                    }
                }
                best = best.min(answer);
                beta = beta.min(best);
                if alpha >= beta {
                    self.stats.ab_cutoffs += 1;
                    break;
                }
            }
            best
        }
    }

    /// A chance node inside the serialized search, with star1 pruning.
    ///
    /// Ballard's insight: partway through a weighted average, the unprocessed
    /// mass can contribute at most `WIN` and at least `LOSS` each, so the final
    /// value is already bracketed. If that bracket is entirely outside the
    /// `(alpha, beta)` window the remaining successors cannot change the caller's
    /// decision and need never be searched. The same accounting also narrows the
    /// window passed to each successor, which is where most of the saving
    /// actually comes from.
    #[allow(clippy::too_many_arguments)]
    fn serial_cell(
        &mut self,
        state: &MatchState,
        p1_commands: &[BattleCommand],
        p2_commands: &[BattleCommand],
        depth: u8,
        chain: u8,
        alpha: f64,
        beta: f64,
        second: Player,
    ) -> f64 {
        let branches = self.resolve(state, p1_commands, p2_commands);

        let mut accumulated = 0.0;
        let mut remaining = 1.0;

        for (child, probability) in branches {
            if probability <= 0.0 {
                continue;
            }

            // Best and worst the average can still end up at.
            let optimistic = accumulated + remaining * WIN;
            if optimistic <= alpha {
                self.stats.ab_cutoffs += 1;
                return optimistic;
            }
            let pessimistic = accumulated + remaining * LOSS;
            if pessimistic >= beta {
                self.stats.ab_cutoffs += 1;
                return pessimistic;
            }

            // What this successor would have to return for the average to reach
            // each end of the window, assuming the rest lands at its bound.
            let child_alpha =
                ((alpha - accumulated - (remaining - probability) * WIN) / probability).max(LOSS);
            let child_beta =
                ((beta - accumulated - (remaining - probability) * LOSS) / probability).min(WIN);

            let (child_depth, child_chain) = self.descend(&child, depth, chain);
            let value = self.serial_ab(
                &child,
                child_depth,
                child_chain,
                child_alpha,
                child_beta.max(child_alpha),
                second,
            );

            accumulated += probability * value;
            remaining -= probability;
        }

        accumulated
    }

    // ── Shared plumbing ─────────────────────────────────────────────────────

    /// Resolve one joint action into its weighted successors, reduced by the
    /// configured [`ChanceMode`](super::chance::ChanceMode).
    ///
    /// Always full enumeration rather than the engine's sample mode: sampling
    /// inside `simulate_turn` would collapse the distribution at every internal
    /// expansion point in a way the search cannot see or account for, whereas
    /// `ChanceMode` reduces the final distribution explicitly and reports what
    /// it dropped.
    fn resolve(
        &mut self,
        state: &MatchState,
        p1_commands: &[BattleCommand],
        p2_commands: &[BattleCommand],
    ) -> Vec<(MatchState, f64)> {
        let cache_key = self.turn_cache.enabled().then(|| {
            (
                hash_state(state),
                command_key(p1_commands),
                command_key(p2_commands),
            )
        });

        if let Some(hit) = cache_key.and_then(|key| self.turn_cache.get(&key)) {
            self.stats.turn_cache_hits += 1;
            return hit;
        }

        self.stats.turns_simulated += 1;
        let mut raw: Vec<(MatchState, f64)> = simulate_turn(
            state,
            &PlayerCommand::Battle(p1_commands.to_vec()),
            &PlayerCommand::Battle(p2_commands.to_vec()),
            self.move_dex,
            self.pokemon_dex,
            self.cfg.consider_crit,
            self.cfg.damage_rolls,
            // No observer: skips event collection entirely, and stops branches
            // with identical states but different event histories from being
            // kept apart, which would inflate the branching factor for nothing.
            None,
        )
        .into_iter()
        .map(|(child, _events, probability)| (child, probability))
        .collect();

        // `simulate_turn` sorts its branches by descending probability, but it
        // builds them by draining a `HashMap` first, so successors that *tie* on
        // probability emerge in an order that varies from run to run. Sorting on
        // the state hash is a stable, content-derived tiebreak.
        //
        // Applied unconditionally, including under `Enumerate` where it looks
        // unnecessary. It is not: a reducing `ChanceMode` picks successors by
        // position, but even exact enumeration sums them in list order, and
        // floating-point addition is not associative. Without the tiebreak two
        // runs of the same solve can differ in the last bits of a cell, which is
        // enough to flip a best-response argmax, change how the restricted game
        // grows, and move the reported work counts. One state hash per successor
        // is cheap next to having produced it, and buys a bit-stable value and
        // reproducible statistics.
        let mut keyed: Vec<(u64, MatchState, f64)> = raw
            .into_iter()
            .map(|(child, probability)| (hash_state(&child), child, probability))
            .collect();
        keyed.sort_by(|a, b| b.2.total_cmp(&a.2).then_with(|| a.0.cmp(&b.0)));
        raw = keyed
            .into_iter()
            .map(|(_, child, probability)| (child, probability))
            .collect();

        let (kept, discarded) = self.cfg.chance.apply(raw);
        if discarded > self.max_discarded {
            self.max_discarded = discarded;
        }

        if let Some(key) = cache_key {
            self.turn_cache.insert(key, &kept);
        }
        kept
    }

    /// The depth and forced-chain counter a successor should be searched at.
    ///
    /// A successor that is still mid-turn — waiting for a replacement or a
    /// self-switch pivot — is a decision point but not a new turn, so it does
    /// not consume a ply. `max_forced_chain` bounds how long that can go on.
    fn descend(&self, child: &MatchState, depth: u8, chain: u8) -> (u8, u8) {
        match actions::phase_of(child) {
            Phase::SelfSwitch | Phase::Replacement if chain < self.cfg.max_forced_chain => {
                (depth, chain + 1)
            }
            _ => (depth.saturating_sub(1), 0),
        }
    }
}

fn terminal_value(winner: Player) -> f64 {
    match winner {
        Player::P1 => WIN,
        Player::P2 => LOSS,
    }
}

fn as_battle(state: &MatchState) -> Option<&BattleState> {
    match state {
        MatchState::BattleState(battle) => Some(battle),
        _ => None,
    }
}

/// Lift a restricted game's strategy back onto the full action indices.
fn scatter(restricted: &[f64], indices: &[usize], full_len: usize) -> Vec<f64> {
    let mut full = vec![0.0; full_len];
    for (slot, &index) in indices.iter().enumerate() {
        full[index] = restricted[slot];
    }
    full
}

/// `MatchState` implements `Hash` by hand, deliberately excluding the ephemeral
/// bookkeeping fields that do not affect play, so it is already a sound memo key
/// with no separate canonicalization step needed.
fn hash_state(state: &MatchState) -> u64 {
    let mut hasher = DefaultHasher::new();
    state.hash(&mut hasher);
    hasher.finish()
}

/// A hash of a joint action's content.
///
/// Written by hand because `BattleCommand` implements neither `Hash` nor `Eq`.
/// Only the turn cache uses this, and only to distinguish actions taken from the
/// same position.
fn command_key(commands: &[BattleCommand]) -> u64 {
    let mut hasher = DefaultHasher::new();
    commands.len().hash(&mut hasher);
    for command in commands {
        match command {
            BattleCommand::Pass => 0u8.hash(&mut hasher),
            BattleCommand::Struggle { target } => {
                1u8.hash(&mut hasher);
                target.hash(&mut hasher);
            }
            BattleCommand::Switch(switch) => {
                2u8.hash(&mut hasher);
                switch.party_index.hash(&mut hasher);
            }
            BattleCommand::Attack(attack) => {
                3u8.hash(&mut hasher);
                attack.move_slot.hash(&mut hasher);
                attack.target.hash(&mut hasher);
                attack.terastallize.hash(&mut hasher);
                attack.mega_evolve.hash(&mut hasher);
            }
        }
    }
    hasher.finish()
}

/// Direct-mapped, always-replace memo of position values.
///
/// Only the simultaneous search uses it, and only ever with exact values — see
/// the module documentation. The serialized search deliberately does not share
/// it: its values are alpha-beta bounds for a *different* game, and mixing the
/// two would be silently wrong.
///
/// Stores a 64-bit hash rather than the position itself. `MatchState` is large
/// and expensive to clone, and a full-width hash comparison makes a false hit
/// vanishingly unlikely.
struct TranspositionTable {
    slots: Vec<Option<TtEntry>>,
    mask: u64,
}

#[derive(Clone, Copy)]
struct TtEntry {
    key: u64,
    depth: u8,
    chain: u8,
    value: f64,
}

impl TranspositionTable {
    fn new(capacity: usize) -> Self {
        if capacity == 0 {
            return TranspositionTable {
                slots: Vec::new(),
                mask: 0,
            };
        }
        let size = capacity.next_power_of_two();
        TranspositionTable {
            slots: vec![None; size],
            mask: (size - 1) as u64,
        }
    }

    /// A value is only reusable at the same search horizon. Besides ordinary
    /// depth, the forced-phase chain matters because reaching its configured
    /// limit makes the next replacement or pivot consume a ply.
    fn probe(&self, key: u64, depth: u8, chain: u8) -> Option<f64> {
        let entry = (*self.slots.get((key & self.mask) as usize)?)?;
        (entry.key == key && entry.depth == depth && entry.chain == chain).then_some(entry.value)
    }

    fn store(&mut self, key: u64, depth: u8, chain: u8, value: f64) {
        if self.slots.is_empty() {
            return;
        }
        let index = (key & self.mask) as usize;
        self.slots[index] = Some(TtEntry {
            key,
            depth,
            chain,
            value,
        });
    }
}

/// Memo of `simulate_turn` results, bounded by total stored successors rather
/// than by entry count.
///
/// Entries hold whole `MatchState`s and one turn can produce hundreds of them,
/// so counting entries would leave the real memory cost unbounded — a thousand
/// entries is a few megabytes or a couple of gigabytes depending entirely on the
/// damage-roll setting. Counting successors makes the ceiling predictable.
struct TurnCache {
    entries: HashMap<(u64, u64, u64), Vec<(MatchState, f64)>>,
    order: VecDeque<(u64, u64, u64)>,
    stored: usize,
    capacity: usize,
}

impl TurnCache {
    fn new(capacity: usize) -> Self {
        TurnCache {
            entries: HashMap::new(),
            order: VecDeque::new(),
            stored: 0,
            capacity,
        }
    }

    fn enabled(&self) -> bool {
        self.capacity > 0
    }

    fn get(&self, key: &(u64, u64, u64)) -> Option<Vec<(MatchState, f64)>> {
        self.entries.get(key).cloned()
    }

    fn insert(&mut self, key: (u64, u64, u64), branches: &[(MatchState, f64)]) {
        if !self.enabled() || branches.len() > self.capacity || self.entries.contains_key(&key) {
            return;
        }
        // First in, first out: no recency tracking, because the access pattern
        // here is a depth-first sweep rather than a working set.
        while self.stored + branches.len() > self.capacity {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            if let Some(evicted) = self.entries.remove(&oldest) {
                self.stored -= evicted.len();
            }
        }
        self.stored += branches.len();
        self.order.push_back(key);
        self.entries.insert(key, branches.to_vec());
    }
}

#[cfg(test)]
mod tests {
    use super::TranspositionTable;

    #[test]
    fn transposition_entries_distinguish_forced_chain_depth() {
        let mut table = TranspositionTable::new(8);
        table.store(17, 2, 1, 0.25);

        assert_eq!(table.probe(17, 2, 1), Some(0.25));
        assert_eq!(table.probe(17, 2, 0), None);
        assert_eq!(table.probe(17, 2, 2), None);
    }
}
