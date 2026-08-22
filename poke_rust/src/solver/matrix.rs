//! Solves a two-player zero-sum matrix game.
//!
//! Rows contain P1 joint commands. Columns contain P2 joint commands.
//! Each cell contains P1's expected payoff for both commands.
//! The result contains the game value and both optimal mixed strategies.
//!
//! One linear program solves the game exactly.
//! Its dual values provide the other player's strategy from the same tableau.
//! The dense tableau supports the small matrices without another numeric dependency.
//!
//! The solver first uses these faster cases:
//!
//! 1. Solve a row or column with one legal action.
//! 2. Find a pure saddle point.
//! 3. Remove strictly dominated strategies.
//!
//! P1 maximizes each payoff. P2 minimizes it.
//! The search supplies probabilities in `[0, 1]`.
//! An internal shift also permits other payoff ranges.

/// Comparison tolerance. Payoffs are win probabilities in `[0, 1]`, so this is
/// an absolute tolerance on a quantity of order 1, not a relative one.
pub const EPS: f64 = 1e-9;

/// Largest dimension that uses dominance reduction.
const DOMINANCE_MAX_DIM: usize = 64;

/// The equilibrium of one matrix game.
///
/// `row_strategy` and `col_strategy` are probability distributions over the
/// rows and columns respectively, each summing to 1 (up to `EPS`).
#[derive(Debug, Clone, PartialEq)]
pub struct MatrixSolution {
    /// The game's value, in P1's favour.
    pub value: f64,
    /// P1's optimal mixed strategy, one entry per row.
    pub row_strategy: Vec<f64>,
    /// P2's optimal mixed strategy, one entry per column.
    pub col_strategy: Vec<f64>,
    /// Whether the simplex actually ran, as opposed to one of the fast paths.
    /// Reported as `lps_solved` in the search statistics.
    pub used_lp: bool,
}

/// Solves a rectangular zero-sum matrix game.
/// P1 selects a row, and P2 selects a column.
/// An empty dimension returns a zero value and empty strategies.
pub fn solve_matrix_game(a: &[Vec<f64>]) -> MatrixSolution {
    let m = a.len();
    let n = a.first().map_or(0, |row| row.len());
    if m == 0 || n == 0 {
        return MatrixSolution {
            value: 0.0,
            row_strategy: vec![0.0; m],
            col_strategy: vec![0.0; n],
            used_lp: false,
        };
    }
    debug_assert!(
        a.iter().all(|row| row.len() == n),
        "matrix game must be rectangular"
    );

    if let Some(solution) = degenerate(a, m, n) {
        return solution;
    }
    if let Some(solution) = saddle_point(a, m, n) {
        return solution;
    }

    let (rows, cols) = undominated(a, m, n);
    if rows.len() < m || cols.len() < n {
        let reduced = submatrix(a, &rows, &cols);
        let solution = solve_reduced(&reduced);
        return scatter(&solution, m, n, &rows, &cols);
    }

    simplex(a, m, n)
}

/// Solves a game after dominance reduction.
/// Checks simple cases again before the linear program.
fn solve_reduced(a: &[Vec<f64>]) -> MatrixSolution {
    let m = a.len();
    let n = a[0].len();
    degenerate(a, m, n)
        .or_else(|| saddle_point(a, m, n))
        .unwrap_or_else(|| simplex(a, m, n))
}

/// A player with exactly one legal action has no decision: the other player
/// simply optimizes against it.
fn degenerate(a: &[Vec<f64>], m: usize, n: usize) -> Option<MatrixSolution> {
    if m == 1 {
        // P2 minimizes along the only row.
        let value = a[0].iter().copied().fold(f64::INFINITY, f64::min);
        let count = a[0]
            .iter()
            .filter(|&&payoff| (payoff - value).abs() <= EPS)
            .count();
        return Some(MatrixSolution {
            value,
            row_strategy: vec![1.0],
            col_strategy: a[0]
                .iter()
                .map(|&payoff| {
                    if (payoff - value).abs() <= EPS {
                        1.0 / count as f64
                    } else {
                        0.0
                    }
                })
                .collect(),
            used_lp: false,
        });
    }
    if n == 1 {
        // P1 maximizes down the only column.
        let value = a.iter().map(|row| row[0]).fold(f64::NEG_INFINITY, f64::max);
        let count = a.iter().filter(|row| (row[0] - value).abs() <= EPS).count();
        return Some(MatrixSolution {
            value,
            row_strategy: a
                .iter()
                .map(|row| {
                    if (row[0] - value).abs() <= EPS {
                        1.0 / count as f64
                    } else {
                        0.0
                    }
                })
                .collect(),
            col_strategy: vec![1.0],
            used_lp: false,
        });
    }
    None
}

/// Returns a saddle-point equilibrium when one exists.
///
/// The strategy is uniform over all equivalent security actions. This prevents
/// the fast path from selecting one arbitrary action when several actions give
/// the same equilibrium guarantee.
fn saddle_point(a: &[Vec<f64>], m: usize, n: usize) -> Option<MatrixSolution> {
    let row_guarantees: Vec<f64> = a
        .iter()
        .map(|row| row.iter().copied().fold(f64::INFINITY, f64::min))
        .collect();
    let col_limits: Vec<f64> = (0..n)
        .map(|j| (0..m).map(|i| a[i][j]).fold(f64::NEG_INFINITY, f64::max))
        .collect();
    let maximin = row_guarantees
        .iter()
        .copied()
        .fold(f64::NEG_INFINITY, f64::max);
    let minimax = col_limits.iter().copied().fold(f64::INFINITY, f64::min);

    if (maximin - minimax).abs() > EPS {
        return None;
    }

    let row_count = row_guarantees
        .iter()
        .filter(|&&value| (value - maximin).abs() <= EPS)
        .count();
    let col_count = col_limits
        .iter()
        .filter(|&&value| (value - minimax).abs() <= EPS)
        .count();
    Some(MatrixSolution {
        value: maximin,
        row_strategy: row_guarantees
            .iter()
            .map(|&value| {
                if (value - maximin).abs() <= EPS {
                    1.0 / row_count as f64
                } else {
                    0.0
                }
            })
            .collect(),
        col_strategy: col_limits
            .iter()
            .map(|&value| {
                if (value - minimax).abs() <= EPS {
                    1.0 / col_count as f64
                } else {
                    0.0
                }
            })
            .collect(),
        used_lp: false,
    })
}

/// Returns rows and columns that survive strict dominance.
/// Strictly dominated strategies have zero probability in every equilibrium.
/// This function does not remove weakly dominated strategies.
fn undominated(a: &[Vec<f64>], m: usize, n: usize) -> (Vec<usize>, Vec<usize>) {
    let mut rows: Vec<usize> = (0..m).collect();
    let mut cols: Vec<usize> = (0..n).collect();
    if m.max(n) > DOMINANCE_MAX_DIM {
        return (rows, cols);
    }

    loop {
        let mut changed = false;

        if rows.len() > 1 {
            let keep: Vec<usize> = rows
                .iter()
                .copied()
                .filter(|&i| {
                    !rows
                        .iter()
                        .any(|&k| k != i && cols.iter().all(|&j| a[k][j] > a[i][j] + EPS))
                })
                .collect();
            if keep.len() < rows.len() && !keep.is_empty() {
                rows = keep;
                changed = true;
            }
        }

        if cols.len() > 1 {
            let keep: Vec<usize> = cols
                .iter()
                .copied()
                .filter(|&j| {
                    !cols
                        .iter()
                        .any(|&l| l != j && rows.iter().all(|&i| a[i][l] < a[i][j] - EPS))
                })
                .collect();
            if keep.len() < cols.len() && !keep.is_empty() {
                cols = keep;
                changed = true;
            }
        }

        if !changed {
            return (rows, cols);
        }
    }
}

/// Solves the game by linear programming.
///
/// Formulated as P2's LP because it lands in canonical form for free. With every
/// payoff shifted strictly positive, the game value `v` is positive, so P2's
/// guarantee `a·y ≤ v·1` with `Σy = 1` can be rescaled by `y' = y / v` into
///
/// ```text
///     maximize  1ᵀy'   subject to   a·y' ≤ 1,  y' ≥ 0
/// ```
///
/// whose optimum is `Σy' = 1/v`. The right-hand side is all ones, so the
/// all-slack basis is already feasible and no Phase I is required. Its dual is
/// P1's LP, and strong duality hands back P1's strategy as the reduced costs of
/// the slack columns at the optimum — one tableau, both strategies.
fn simplex(a: &[Vec<f64>], m: usize, n: usize) -> MatrixSolution {
    // The rescaling divides by the value, so the value must be known positive
    // first. An additive shift leaves both equilibrium strategies untouched and
    // moves the value by exactly the shift, so it costs nothing to undo.
    let min = a
        .iter()
        .flat_map(|row| row.iter().copied())
        .fold(f64::INFINITY, f64::min);
    let shift = if min <= 1.0 { 1.0 - min } else { 0.0 };

    // Row i is [ shifted payoffs | slack identity | rhs ].
    let width = n + m + 1;
    let rhs = width - 1;
    let mut t = vec![vec![0.0f64; width]; m];
    for (i, row) in t.iter_mut().enumerate() {
        for j in 0..n {
            row[j] = a[i][j] + shift;
        }
        row[n + i] = 1.0;
        row[rhs] = 1.0;
    }

    // Objective row held as reduced costs; optimal once all are non-negative.
    // `obj[rhs]` accumulates the objective value.
    let mut obj = vec![0.0f64; width];
    for slot in obj.iter_mut().take(n) {
        *slot = -1.0;
    }
    let mut basis: Vec<usize> = (0..m).map(|i| n + i).collect();

    // Dantzig's rule (steepest reduced cost) converges in far fewer pivots but
    // can cycle on degenerate vertices; Bland's rule cannot cycle but crawls.
    // Run Dantzig, then fall back to Bland if it has not converged by the point
    // where cycling is the plausible explanation.
    let bland_after = 4 * (m + n) + 64;
    let max_iters = 64 * (m + n) + 4096;

    for iter in 0..max_iters {
        let bland = iter >= bland_after;

        let entering = if bland {
            (0..n + m).find(|&j| obj[j] < -EPS)
        } else {
            let mut best = -EPS;
            let mut pick = None;
            for (j, &cost) in obj.iter().enumerate().take(n + m) {
                if cost < best {
                    best = cost;
                    pick = Some(j);
                }
            }
            pick
        };
        let Some(q) = entering else { break };

        // Ratio test, breaking ties on the smallest leaving-variable index so
        // that Bland's anti-cycling guarantee actually holds.
        let mut leaving: Option<usize> = None;
        let mut best_ratio = f64::INFINITY;
        for i in 0..m {
            if t[i][q] <= EPS {
                continue;
            }
            let ratio = t[i][rhs] / t[i][q];
            let better = match leaving {
                None => true,
                Some(cur) => {
                    ratio < best_ratio - EPS
                        || ((ratio - best_ratio).abs() <= EPS && basis[i] < basis[cur])
                }
            };
            if better {
                leaving = Some(i);
                best_ratio = ratio;
            }
        }
        // Unbounded. Impossible with strictly positive payoffs (every structural
        // column has a positive entry), but bail rather than loop if numerics
        // ever say otherwise.
        let Some(p) = leaving else { break };

        let pivot = t[p][q];
        for value in t[p].iter_mut() {
            *value /= pivot;
        }
        // Cloned once so the pivot row can be subtracted from every other row
        // (and from the objective) without holding two borrows of `t`.
        let pivot_row = t[p].clone();
        for (i, row) in t.iter_mut().enumerate() {
            if i == p {
                continue;
            }
            let factor = row[q];
            if factor != 0.0 {
                for (value, pivot_value) in row.iter_mut().zip(&pivot_row) {
                    *value -= factor * pivot_value;
                }
            }
        }
        let factor = obj[q];
        if factor != 0.0 {
            for (value, pivot_value) in obj.iter_mut().zip(&pivot_row) {
                *value -= factor * pivot_value;
            }
        }
        basis[p] = q;
    }

    let objective = obj[rhs];
    if objective <= EPS {
        // Only reachable if the tableau degenerated numerically; the maximin
        // pair is a correct-by-construction fallback that never lies about
        // being a pure profile.
        let (i, _) = extremum(
            a.iter()
                .map(|row| row.iter().copied().fold(f64::INFINITY, f64::min)),
            |x, y| x > y,
        );
        let (j, _) = extremum(
            (0..n).map(|c| (0..m).map(|r| a[r][c]).fold(f64::NEG_INFINITY, f64::max)),
            |x, y| x < y,
        );
        return MatrixSolution {
            value: a[i][j],
            row_strategy: unit_vector(m, i),
            col_strategy: unit_vector(n, j),
            used_lp: true,
        };
    }

    let shifted_value = 1.0 / objective;

    let mut col_strategy = vec![0.0; n];
    for (i, &var) in basis.iter().enumerate() {
        if var < n {
            col_strategy[var] = shifted_value * t[i][rhs];
        }
    }
    let row_strategy: Vec<f64> = (0..m).map(|i| shifted_value * obj[n + i]).collect();

    MatrixSolution {
        value: shifted_value - shift,
        row_strategy: normalized(row_strategy),
        col_strategy: normalized(col_strategy),
        used_lp: true,
    }
}

/// Clamp away negative dust and rescale to sum 1. A strategy that sums to zero
/// (only reachable through numerical collapse) becomes uniform.
fn normalized(mut v: Vec<f64>) -> Vec<f64> {
    for x in v.iter_mut() {
        if *x < 0.0 {
            *x = 0.0;
        }
    }
    let total: f64 = v.iter().sum();
    if total <= EPS {
        let share = 1.0 / v.len() as f64;
        return vec![share; v.len()];
    }
    for x in v.iter_mut() {
        *x /= total;
    }
    v
}

/// Index and value of the entry that `better` prefers, first one winning ties.
fn extremum(values: impl Iterator<Item = f64>, better: impl Fn(f64, f64) -> bool) -> (usize, f64) {
    let mut best_idx = 0;
    let mut best = f64::NAN;
    for (i, v) in values.enumerate() {
        if i == 0 || better(v, best) {
            best_idx = i;
            best = v;
        }
    }
    (best_idx, best)
}

fn unit_vector(len: usize, at: usize) -> Vec<f64> {
    let mut v = vec![0.0; len];
    v[at] = 1.0;
    v
}

fn submatrix(a: &[Vec<f64>], rows: &[usize], cols: &[usize]) -> Vec<Vec<f64>> {
    rows.iter()
        .map(|&i| cols.iter().map(|&j| a[i][j]).collect())
        .collect()
}

/// Lift a reduced game's solution back onto the full matrix's indices, leaving
/// eliminated strategies at probability zero.
fn scatter(
    solution: &MatrixSolution,
    m: usize,
    n: usize,
    rows: &[usize],
    cols: &[usize],
) -> MatrixSolution {
    let mut row_strategy = vec![0.0; m];
    for (k, &i) in rows.iter().enumerate() {
        row_strategy[i] = solution.row_strategy[k];
    }
    let mut col_strategy = vec![0.0; n];
    for (k, &j) in cols.iter().enumerate() {
        col_strategy[j] = solution.col_strategy[k];
    }
    MatrixSolution {
        value: solution.value,
        row_strategy,
        col_strategy,
        used_lp: solution.used_lp,
    }
}

// ── Double oracle ───────────────────────────────────────────────────────────

/// The counters that one [`double_oracle`] run produced.
///
/// The function returns them instead of writing them, because a cell oracle
/// usually borrows the caller. The caller adds them to its own statistics.
#[derive(Debug, Clone, Copy, Default)]
pub struct OracleStats {
    /// Calls to the cell oracle.
    pub cells_requested: u64,
    /// Restricted games that reached the simplex.
    pub lps_solved: u64,
    /// Best-response rows and columns that a bound test abandoned.
    pub cutoffs: u64,
    /// Rounds that completed both best-response checks.
    pub rounds_completed: u64,
}

/// The value window and the payoff range of one [`double_oracle`] run.
///
/// `alpha` and `beta` must contain the true value. A caller without a proven
/// window passes the payoff range itself.
#[derive(Debug, Clone, Copy)]
pub struct OracleLimits {
    /// A proven lower bound on the true value.
    pub alpha: f64,
    /// A proven upper bound on the true value.
    pub beta: f64,
    /// The smallest payoff that a cell can hold.
    pub low: f64,
    /// The largest payoff that a cell can hold.
    pub high: f64,
}

impl Default for OracleLimits {
    /// The window and the range of a payoff that is a probability.
    fn default() -> Self {
        OracleLimits {
            alpha: 0.0,
            beta: 1.0,
            low: 0.0,
            high: 1.0,
        }
    }
}

/// The rows and columns that a [`double_oracle`] run opens its restricted game
/// with.
///
/// An empty side starts from action 0. An index at or past the action count is
/// dropped, because a stale index names a different action or none at all.
#[derive(Debug, Clone, Copy, Default)]
pub struct OracleSeed<'a> {
    pub rows: Option<&'a [usize]>,
    pub cols: Option<&'a [usize]>,
}

/// The order in which a best-response check reads the actions of each player.
///
/// A check abandons an action as soon as a bound rules it out, and that bound is
/// the best action found so far. Index order therefore starts from a weak
/// incumbent, and it evaluates many actions before a strong one appears. A check
/// that reads a strong action first abandons most of the rest after one cell.
///
/// The order changes the cells that a run reads. It cannot change the best
/// response or its value, because the check still reads every action, and the
/// bound test abandons an action only when even its optimistic completion loses.
/// `None` keeps index order.
///
/// An index at or past the action count is dropped, and a missing index is
/// appended in index order, so a partial or stale list still reads every action
/// exactly one time.
#[derive(Debug, Clone, Copy, Default)]
pub struct OracleOrder<'a> {
    pub rows: Option<&'a [usize]>,
    pub cols: Option<&'a [usize]>,
}

/// A complete scan permutation of `len` actions.
///
/// Every action appears exactly one time, so the caller can iterate the result
/// in place of `0..len`.
fn scan_order(order: Option<&[usize]>, len: usize) -> Vec<usize> {
    let Some(order) = order else {
        return (0..len).collect();
    };
    let mut seen = vec![false; len];
    let mut out = Vec::with_capacity(len);
    for &index in order {
        if index < len && !seen[index] {
            seen[index] = true;
            out.push(index);
        }
    }
    for (index, &taken) in seen.iter().enumerate() {
        if !taken {
            out.push(index);
        }
    }
    out
}

/// One cell that a [`double_oracle_with`] run asks for.
///
/// The three fields name the cell inside one run. A parallel oracle builds the
/// random seed of the job from them, so the seed does not depend on the thread
/// schedule. Read `solver::pool` for the seed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellJob {
    /// The round that asked for this cell, counted from 0.
    pub round: usize,
    pub row: usize,
    pub col: usize,
}

/// Supplies the payoff of one cell, and reads the answer of one round.
///
/// [`double_oracle`] needs one payoff at a time, so a plain closure is enough
/// for most callers. [`ClosureOracle`] wraps such a closure.
///
/// A caller that reports progress needs more. It has to read the restricted
/// equilibrium after both best-response checks of each round. That value and
/// those two strategies are the answer of the round. A second closure cannot do
/// this, because both closures would borrow the same search context. One oracle
/// holds that context one time and serves both methods.
///
/// A caller with a worker pool needs more again. [`CellOracle::cells`] takes a
/// job list rather than one cell, so the oracle can spread the list over its
/// workers. The default implementation calls [`CellOracle::cell`] for each job
/// in order, so a plain closure keeps the behavior of a serial run.
pub trait CellOracle {
    /// The payoff of one row against one column.
    fn cell(&mut self, row: usize, col: usize) -> f64;

    /// The payoff of each job, in job order.
    ///
    /// The answer must hold one value for each job. The run writes those values
    /// into its cell table in the same order.
    fn cells(&mut self, jobs: &[CellJob]) -> Vec<f64> {
        jobs.iter().map(|job| self.cell(job.row, job.col)).collect()
    }

    /// The largest prefetch that this oracle wants before a best-response check.
    ///
    /// A best-response check reads its cells one at a time, and it abandons a
    /// row or a column as soon as a bound rules it out. A parallel oracle has no
    /// work under that pattern, so the run fills some of the missing cells first.
    ///
    /// A value of 1 or less turns the prefetch off. That is the default, and it
    /// keeps the exact cell set of a serial run.
    ///
    /// The limit stops the prefetch from filling the whole matrix. Read the
    /// module documentation of `solver::search` for the reason.
    fn batch_limit(&self) -> usize {
        1
    }

    /// True after the caller can no longer compute an exact cell.
    ///
    /// The run then returns the last round that completed before the stop.
    /// The default keeps an unbounded matrix solve unchanged.
    fn stop_requested(&mut self) -> bool {
        false
    }

    /// The restricted equilibrium after both best-response checks of one round.
    ///
    /// `stats` holds the counters of this run so far. The caller adds them to
    /// its own counters, because the run reports them only when it returns.
    ///
    /// The default implementation reads nothing. Only a progress reporter needs
    /// this method.
    fn round(&mut self, _solution: &MatrixSolution, _stats: &OracleStats) {}
}

/// A cell closure, as an oracle that reports no round.
pub struct ClosureOracle<F>(pub F);

impl<F: FnMut(usize, usize) -> f64> CellOracle for ClosureOracle<F> {
    fn cell(&mut self, row: usize, col: usize) -> f64 {
        (self.0)(row, col)
    }
}

/// Solves a matrix game without building all of it.
///
/// `rows` and `cols` give the size of the full game. `cell_value` returns the
/// payoff of one row and one column. The run calls it only for the cells that it
/// needs, and never twice for the same cell.
///
/// Use [`double_oracle_with`] to also read the answer of each round.
pub fn double_oracle<F>(
    rows: usize,
    cols: usize,
    seed: OracleSeed<'_>,
    limits: OracleLimits,
    cell_value: F,
) -> (MatrixSolution, OracleStats)
where
    F: FnMut(usize, usize) -> f64,
{
    double_oracle_with(
        rows,
        cols,
        seed,
        OracleOrder::default(),
        limits,
        ClosureOracle(cell_value),
    )
}

/// [`double_oracle`], with an oracle that also reads each round.
///
/// Start from a small restricted game. Solve it, then ask each player for their
/// best response over the full action set. Neither player can improve on the
/// restricted equilibrium when both best responses are already in the restricted
/// game. That equilibrium is then an equilibrium of the whole game, and the
/// remaining cells never mattered. Otherwise add both best responses and repeat.
///
/// The two best-response values bracket the true value from above and below.
/// This is both the stop test and the source of the bound tightening that makes
/// the next round's pruning sharper.
///
/// This implements Algorithm 3 of Bošanský et al., Artificial Intelligence 237,
/// 2016.
pub fn double_oracle_with<O>(
    rows: usize,
    cols: usize,
    seed: OracleSeed<'_>,
    order: OracleOrder<'_>,
    limits: OracleLimits,
    mut oracle: O,
) -> (MatrixSolution, OracleStats)
where
    O: CellOracle,
{
    let mut stats = OracleStats::default();
    let mut best = MatrixSolution {
        value: 0.0,
        row_strategy: vec![0.0; rows],
        col_strategy: vec![0.0; cols],
        used_lp: false,
    };
    if rows == 0 || cols == 0 {
        return (best, stats);
    }
    best.value = 0.5 * (limits.low + limits.high);
    best.row_strategy.fill(1.0 / rows as f64);
    best.col_strategy.fill(1.0 / cols as f64);

    let mut alpha = limits.alpha;
    let mut beta = limits.beta;

    // Lazily filled, and shared across the best-response calls. No cell is ever
    // computed twice within this run. That sharing is most of the win.
    let mut cells: Vec<Vec<Option<f64>>> = vec![vec![None; cols]; rows];
    let mut restricted_rows = seeded_start(seed.rows, rows);
    let mut restricted_cols = seeded_start(seed.cols, cols);

    // Each round adds at least one action to at least one side or stops. This
    // limit is therefore only reachable if something is badly wrong.
    'rounds: for round in 0..(rows + cols + 2) {
        if oracle.stop_requested() {
            break;
        }
        // The round needs every restricted cell, so the whole list goes to the
        // oracle in one call. A serial oracle answers the jobs in this order,
        // which is the order that a one-cell-at-a-time run used.
        let mut missing = Vec::new();
        for &i in &restricted_rows {
            for &j in &restricted_cols {
                if cells[i][j].is_none() {
                    missing.push(CellJob {
                        round,
                        row: i,
                        col: j,
                    });
                }
            }
        }
        fill(&mut cells, &missing, &mut stats, &mut oracle);

        let sub: Vec<Vec<f64>> = restricted_rows
            .iter()
            .map(|&i| {
                restricted_cols
                    .iter()
                    .map(|&j| cells[i][j].expect("restricted cell was just filled"))
                    .collect()
            })
            .collect();
        let solution = solve_matrix_game(&sub);
        if solution.used_lp {
            stats.lps_solved += 1;
        }

        let row_strategy = lift(&solution.row_strategy, &restricted_rows, rows);
        let col_strategy = lift(&solution.col_strategy, &restricted_cols, cols);
        let candidate = MatrixSolution {
            value: solution.value,
            row_strategy: row_strategy.clone(),
            col_strategy: col_strategy.clone(),
            used_lp: solution.used_lp,
        };
        if oracle.stop_requested() {
            break;
        }

        let limit = oracle.batch_limit();
        prefetch_rows(
            &mut cells,
            &col_strategy,
            round,
            limit,
            &mut stats,
            &mut oracle,
        );
        if oracle.stop_requested() {
            break;
        }
        let Some((row_br_value, row_br)) = best_response_row(
            &mut cells,
            &col_strategy,
            limits.high,
            order.rows,
            &mut stats,
            &mut oracle,
        ) else {
            break 'rounds;
        };
        prefetch_cols(
            &mut cells,
            &row_strategy,
            round,
            limit,
            &mut stats,
            &mut oracle,
        );
        if oracle.stop_requested() {
            break;
        }
        let Some((col_br_value, col_br)) = best_response_col(
            &mut cells,
            &row_strategy,
            limits.low,
            order.cols,
            &mut stats,
            &mut oracle,
        ) else {
            break 'rounds;
        };

        // Both best-response checks are complete, so this round has an answer.
        // A reporter can publish it now. The loop reads nothing back.
        best = candidate;
        stats.rounds_completed += 1;
        oracle.round(&best, &stats);

        // P2 can hold P1 to `row_br_value` by playing `col_strategy`, and P1 can
        // guarantee `col_br_value` by playing `row_strategy`. Both are full-game
        // strategies, so these bracket the true value.
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
        // Both best responses are already in the restricted game. Neither player
        // has anywhere better to go.
        if !grew {
            break;
        }
    }

    // `best.value` is the *restricted* game's value, which equals the true value
    // once the loop has converged. It can differ if the loop instead stopped
    // because an incoming window closed the bracket first. Both `alpha` and
    // `beta` are always valid bounds on the true value, so clamping into them is
    // a no-op in the converged case and caps the error at the bracket width
    // otherwise.
    //
    // Ordered explicitly rather than with `clamp`, which panics on an inverted
    // range: at convergence the two best-response values are the same quantity
    // summed in different orders — row-major against the column strategy,
    // column-major against the row strategy — so they can cross by one ulp.
    let (low, high) = (alpha.min(beta), alpha.max(beta));
    best.value = best.value.clamp(low, high);
    (best, stats)
}

/// Asks the oracle for each job, and writes the answers into `cells`.
///
/// The counter rises before the call, so a round hook reads the work of the
/// whole batch.
fn fill<O>(
    cells: &mut [Vec<Option<f64>>],
    jobs: &[CellJob],
    stats: &mut OracleStats,
    oracle: &mut O,
) where
    O: CellOracle,
{
    if jobs.is_empty() {
        return;
    }
    stats.cells_requested += jobs.len() as u64;
    let values = oracle.cells(jobs);
    debug_assert_eq!(
        values.len(),
        jobs.len(),
        "a cell oracle must answer every job"
    );
    for (job, value) in jobs.iter().zip(values) {
        cells[job.row][job.col] = Some(value);
    }
}

/// Fills up to `limit` of the cells that [`best_response_row`] can read.
///
/// The check abandons a row as soon as a bound rules it out, so it cannot say in
/// advance which cells it needs. The prefetch takes the missing cells in index
/// order instead, and `limit` bounds the count.
///
/// A prefetched cell cannot move the answer. The check judges a missing cell at
/// its optimistic bound, and it abandons a row only when even that bound loses
/// by more than [`EPS`]. Such a row is never the best response. A larger set of
/// known cells therefore changes which rows the check abandons, and it leaves
/// both the best row and its value the same.
fn prefetch_rows<O>(
    cells: &mut [Vec<Option<f64>>],
    col_strategy: &[f64],
    round: usize,
    limit: usize,
    stats: &mut OracleStats,
    oracle: &mut O,
) where
    O: CellOracle,
{
    if limit <= 1 {
        return;
    }
    let mut jobs = Vec::new();
    'rows: for (i, row) in cells.iter().enumerate() {
        for (j, value) in row.iter().enumerate() {
            if col_strategy[j] <= EPS || value.is_some() {
                continue;
            }
            jobs.push(CellJob {
                round,
                row: i,
                col: j,
            });
            if jobs.len() >= limit {
                break 'rows;
            }
        }
    }
    fill(cells, &jobs, stats, oracle);
}

/// Fills up to `limit` of the cells that [`best_response_col`] can read.
///
/// This is the mirror of [`prefetch_rows`], and the same argument holds for it.
fn prefetch_cols<O>(
    cells: &mut [Vec<Option<f64>>],
    row_strategy: &[f64],
    round: usize,
    limit: usize,
    stats: &mut OracleStats,
    oracle: &mut O,
) where
    O: CellOracle,
{
    if limit <= 1 {
        return;
    }
    let cols = cells.first().map_or(0, |row| row.len());
    let mut jobs = Vec::new();
    'cols: for j in 0..cols {
        for (i, row) in cells.iter().enumerate() {
            if row_strategy[i] <= EPS || row[j].is_some() {
                continue;
            }
            jobs.push(CellJob {
                round,
                row: i,
                col: j,
            });
            if jobs.len() >= limit {
                break 'cols;
            }
        }
    }
    fill(cells, &jobs, stats, oracle);
}

/// P1's best pure response to `col_strategy`, and its value.
///
/// Rows are abandoned as soon as they cannot catch the best row found so far,
/// judging unevaluated cells at `high`. This is the paper's λ test rearranged:
/// rather than deriving the payoff a cell would have to deliver and comparing it
/// against that cell's bound, compare the row's optimistic completion against the
/// incumbent directly. Both skip exactly the same rows, and the bound tightens
/// mid-row as cells become known.
fn best_response_row<O>(
    cells: &mut [Vec<Option<f64>>],
    col_strategy: &[f64],
    high: f64,
    order: Option<&[usize]>,
    stats: &mut OracleStats,
    oracle: &mut O,
) -> Option<(f64, usize)>
where
    O: CellOracle,
{
    let support: Vec<usize> = (0..col_strategy.len())
        .filter(|&j| col_strategy[j] > EPS)
        .collect();

    let mut best_value = f64::NEG_INFINITY;
    let mut best_row = 0;

    // Every row is still read. [`OracleOrder`] changes only which row sets the
    // incumbent first, and therefore how early the bound test fires.
    for i in scan_order(order, cells.len()) {
        let mut accumulated = 0.0;
        let mut abandoned = false;

        for (k, &j) in support.iter().enumerate() {
            let optimistic = accumulated
                + support[k..]
                    .iter()
                    .map(|&jj| col_strategy[jj] * cells[i][jj].unwrap_or(high))
                    .sum::<f64>();
            if optimistic < best_value - EPS {
                stats.cutoffs += 1;
                abandoned = true;
                break;
            }

            let value = match cells[i][j] {
                Some(value) => value,
                None => {
                    stats.cells_requested += 1;
                    let value = oracle.cell(i, j);
                    cells[i][j] = Some(value);
                    if oracle.stop_requested() {
                        return None;
                    }
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

    Some((best_value, best_row))
}

/// P2's best pure response to `row_strategy`, and its value in P1's terms.
///
/// P2 therefore looks for the *smallest* number. This is the mirror of
/// [`best_response_row`], and it judges unevaluated cells at `low`.
fn best_response_col<O>(
    cells: &mut [Vec<Option<f64>>],
    row_strategy: &[f64],
    low: f64,
    order: Option<&[usize]>,
    stats: &mut OracleStats,
    oracle: &mut O,
) -> Option<(f64, usize)>
where
    O: CellOracle,
{
    let support: Vec<usize> = (0..row_strategy.len())
        .filter(|&i| row_strategy[i] > EPS)
        .collect();

    let cols = cells.first().map_or(0, |row| row.len());
    let mut best_value = f64::INFINITY;
    let mut best_col = 0;

    // Every column is still read. See [`best_response_row`] for the order.
    for j in scan_order(order, cols) {
        let mut accumulated = 0.0;
        let mut abandoned = false;

        for (k, &i) in support.iter().enumerate() {
            let pessimistic = accumulated
                + support[k..]
                    .iter()
                    .map(|&ii| row_strategy[ii] * cells[ii][j].unwrap_or(low))
                    .sum::<f64>();
            if pessimistic > best_value + EPS {
                stats.cutoffs += 1;
                abandoned = true;
                break;
            }

            let value = match cells[i][j] {
                Some(value) => value,
                None => {
                    stats.cells_requested += 1;
                    let value = oracle.cell(i, j);
                    cells[i][j] = Some(value);
                    if oracle.stop_requested() {
                        return None;
                    }
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

    Some((best_value, best_col))
}

/// The action indices that the restricted game opens with.
///
/// Falls back to action 0, which is what an unseeded run always uses. An index at
/// or past `len` is dropped: a capped action set can shrink between calls, and a
/// stale index would then name a different action or none at all.
/// The function removes repeated indices.
fn seeded_start(seed: Option<&[usize]>, len: usize) -> Vec<usize> {
    let mut start = Vec::new();
    for &index in seed.unwrap_or(&[]) {
        if index < len && !start.contains(&index) {
            start.push(index);
        }
    }
    if start.is_empty() {
        start.push(0);
    }
    start
}

/// Lift a restricted game's strategy back onto the full action indices.
fn lift(restricted: &[f64], indices: &[usize], full_len: usize) -> Vec<f64> {
    let mut full = vec![0.0; full_len];
    for (slot, &index) in indices.iter().enumerate() {
        full[index] = restricted[slot];
    }
    full
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A partial, stale, or repeated order must still name every action one
    /// time. The best-response check iterates this list in place of `0..len`, so
    /// a dropped action would silently leave a best response unconsidered.
    #[test]
    fn a_scan_order_is_always_a_complete_permutation() {
        let cases: [(Option<&[usize]>, usize); 6] = [
            (None, 4),
            (Some(&[]), 4),
            (Some(&[3, 1]), 4),
            (Some(&[9, 2]), 4),
            (Some(&[1, 1, 1]), 4),
            (Some(&[3, 2, 1, 0]), 4),
        ];
        for (order, len) in cases {
            let scan = scan_order(order, len);
            let mut sorted = scan.clone();
            sorted.sort_unstable();
            assert_eq!(
                sorted,
                (0..len).collect::<Vec<usize>>(),
                "order {order:?} of {len} produced {scan:?}"
            );
        }
    }

    /// The requested actions must come first, in the requested order.
    #[test]
    fn a_scan_order_leads_with_the_requested_actions() {
        assert_eq!(scan_order(Some(&[3, 1]), 5), vec![3, 1, 0, 2, 4]);
    }

    /// No order means index order, which is what an unordered run reads.
    #[test]
    fn no_scan_order_is_index_order() {
        assert_eq!(scan_order(None, 3), vec![0, 1, 2]);
    }

    /// The defining property of an equilibrium: neither player can improve by
    /// deviating to any pure strategy. Every solved matrix should satisfy this,
    /// whichever code path produced it.
    fn assert_equilibrium(a: &[Vec<f64>], solution: &MatrixSolution, tol: f64) {
        let n = a[0].len();

        let row_sum: f64 = solution.row_strategy.iter().sum();
        let col_sum: f64 = solution.col_strategy.iter().sum();
        assert!(
            (row_sum - 1.0).abs() < tol,
            "row strategy sums to {row_sum}"
        );
        assert!(
            (col_sum - 1.0).abs() < tol,
            "col strategy sums to {col_sum}"
        );
        assert!(solution.row_strategy.iter().all(|&p| p >= -tol));
        assert!(solution.col_strategy.iter().all(|&p| p >= -tol));

        // P1's strategy guarantees at least the value against every column.
        for j in 0..n {
            let payoff: f64 = a
                .iter()
                .zip(&solution.row_strategy)
                .map(|(row, probability)| probability * row[j])
                .sum();
            assert!(
                payoff >= solution.value - tol,
                "column {j} exploits P1: {payoff} < {}",
                solution.value
            );
        }
        // P2's strategy concedes at most the value against every row.
        for (i, row) in a.iter().enumerate() {
            let payoff: f64 = (0..n).map(|j| solution.col_strategy[j] * row[j]).sum();
            assert!(
                payoff <= solution.value + tol,
                "row {i} exploits P2: {payoff} > {}",
                solution.value
            );
        }
    }

    #[test]
    fn rock_paper_scissors_is_uniform() {
        let a = vec![
            vec![0.0, -1.0, 1.0],
            vec![1.0, 0.0, -1.0],
            vec![-1.0, 1.0, 0.0],
        ];
        let solution = solve_matrix_game(&a);
        assert!(solution.value.abs() < 1e-9, "value {}", solution.value);
        for p in solution.row_strategy.iter().chain(&solution.col_strategy) {
            assert!((p - 1.0 / 3.0).abs() < 1e-9, "expected uniform, got {p}");
        }
        assert_equilibrium(&a, &solution, 1e-9);
    }

    #[test]
    fn matching_pennies_is_even() {
        let a = vec![vec![1.0, -1.0], vec![-1.0, 1.0]];
        let solution = solve_matrix_game(&a);
        assert!(solution.value.abs() < 1e-9);
        for p in solution.row_strategy.iter().chain(&solution.col_strategy) {
            assert!((p - 0.5).abs() < 1e-9, "expected 50/50, got {p}");
        }
    }

    /// The fast path that matters most in practice: an asymmetric game whose
    /// equilibrium is genuinely mixed and not uniform.
    #[test]
    fn asymmetric_mixed_equilibrium() {
        // Value 1/2, row strategy (1/2, 1/2), column strategy (1/2, 1/2, 0).
        let a = vec![vec![1.0, 0.0, 0.0], vec![0.0, 1.0, 2.0]];
        let solution = solve_matrix_game(&a);
        assert_equilibrium(&a, &solution, 1e-9);
        assert!((solution.value - 0.5).abs() < 1e-9, "{}", solution.value);
    }

    #[test]
    fn pure_saddle_point_skips_the_lp() {
        // Row 1 / column 0 is a saddle at 3: the row minimum and column maximum.
        let a = vec![vec![1.0, 2.0], vec![3.0, 4.0]];
        let solution = solve_matrix_game(&a);
        assert!(!solution.used_lp, "a saddle point should not reach simplex");
        assert!((solution.value - 3.0).abs() < 1e-9);
        assert_eq!(solution.row_strategy, vec![0.0, 1.0]);
        assert_eq!(solution.col_strategy, vec![1.0, 0.0]);
    }

    #[test]
    fn single_row_lets_the_column_player_minimize() {
        let a = vec![vec![0.7, 0.2, 0.9]];
        let solution = solve_matrix_game(&a);
        assert!(!solution.used_lp);
        assert!((solution.value - 0.2).abs() < 1e-9);
        assert_eq!(solution.col_strategy, vec![0.0, 1.0, 0.0]);
        assert_eq!(solution.row_strategy, vec![1.0]);
    }

    #[test]
    fn single_column_lets_the_row_player_maximize() {
        let a = vec![vec![0.7], vec![0.2], vec![0.9]];
        let solution = solve_matrix_game(&a);
        assert!(!solution.used_lp);
        assert!((solution.value - 0.9).abs() < 1e-9);
        assert_eq!(solution.row_strategy, vec![0.0, 0.0, 1.0]);
    }

    /// One-choice games must mix the equivalent choices of the other player.
    #[test]
    fn one_choice_games_mix_equivalent_replies() {
        let one_row = solve_matrix_game(&[vec![0.2, 0.2, 0.7]]);
        assert_eq!(one_row.col_strategy, vec![0.5, 0.5, 0.0]);

        let one_col = solve_matrix_game(&[vec![0.8], vec![0.3], vec![0.8]]);
        assert_eq!(one_col.row_strategy, vec![0.5, 0.0, 0.5]);
    }

    #[test]
    fn single_cell() {
        let solution = solve_matrix_game(&[vec![0.42]]);
        assert!((solution.value - 0.42).abs() < 1e-9);
        assert_eq!(solution.row_strategy, vec![1.0]);
        assert_eq!(solution.col_strategy, vec![1.0]);
    }

    #[test]
    fn empty_matrix_is_not_a_panic() {
        let solution = solve_matrix_game(&[]);
        assert_eq!(solution.value, 0.0);
        assert!(solution.row_strategy.is_empty());
    }

    /// A strictly dominated row must end up with probability zero, and the
    /// value must match the game with that row deleted by hand.
    #[test]
    fn strictly_dominated_row_is_eliminated() {
        let full = vec![
            vec![0.0, -1.0, 1.0],
            vec![1.0, 0.0, -1.0],
            vec![-1.0, 1.0, 0.0],
            // Strictly worse than row 0 against every column.
            vec![-0.5, -1.5, 0.5],
        ];
        let solution = solve_matrix_game(&full);
        assert!(
            solution.row_strategy[3].abs() < 1e-9,
            "dominated row kept mass {}",
            solution.row_strategy[3]
        );
        assert!(solution.value.abs() < 1e-9);
        assert_equilibrium(&full, &solution, 1e-9);
    }

    #[test]
    fn win_probability_range_is_handled() {
        // The range the search actually uses: payoffs in [0, 1].
        let a = vec![vec![0.9, 0.1], vec![0.2, 0.8]];
        let solution = solve_matrix_game(&a);
        assert_equilibrium(&a, &solution, 1e-9);
        assert!(solution.value > 0.0 && solution.value < 1.0);
    }

    /// Constant games are the degenerate case most likely to break a tableau:
    /// every strategy is optimal and the LP is massively degenerate.
    #[test]
    fn constant_matrix_has_constant_value() {
        let a = vec![vec![0.25; 4]; 4];
        let solution = solve_matrix_game(&a);
        assert!((solution.value - 0.25).abs() < 1e-9);
        assert_eq!(solution.row_strategy, vec![0.25; 4]);
        assert_eq!(solution.col_strategy, vec![0.25; 4]);
        assert_equilibrium(&a, &solution, 1e-9);
    }

    /// Equivalent security actions must share the strategy probability.
    #[test]
    fn saddle_point_mixes_equivalent_actions() {
        let a = vec![
            vec![0.5, 0.5, 0.8],
            vec![0.5, 0.5, 0.9],
            vec![0.4, 0.5, 1.0],
        ];
        let solution = solve_matrix_game(&a);

        assert_eq!(solution.row_strategy, vec![0.5, 0.5, 0.0]);
        assert_eq!(solution.col_strategy, vec![0.5, 0.5, 0.0]);
        assert_equilibrium(&a, &solution, 1e-9);
    }

    /// The real safety net: random matrices across a range of shapes, each
    /// checked against the definition of equilibrium rather than a known answer.
    #[test]
    fn random_matrices_yield_equilibria() {
        // xorshift, so the test carries no RNG dependency and never flakes.
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 11) as f64 / (1u64 << 53) as f64
        };

        for m in 1..=6 {
            for n in 1..=6 {
                for _ in 0..25 {
                    let a: Vec<Vec<f64>> =
                        (0..m).map(|_| (0..n).map(|_| next()).collect()).collect();
                    let solution = solve_matrix_game(&a);
                    // Looser than EPS: the tableau accumulates rounding, and the
                    // guarantee being checked is a sum over up to six terms.
                    assert_equilibrium(&a, &solution, 1e-7);
                }
            }
        }
    }

    /// Equilibrium strategies are invariant under an additive shift of the
    /// payoffs, and the value moves by exactly that shift. Worth pinning because
    /// the simplex path shifts internally to force positivity.
    #[test]
    fn additive_shift_moves_only_the_value() {
        let a = vec![vec![0.0, -1.0, 1.0], vec![1.0, 0.0, -1.0]];
        let shifted: Vec<Vec<f64>> = a
            .iter()
            .map(|row| row.iter().map(|x| x + 10.0).collect())
            .collect();

        let base = solve_matrix_game(&a);
        let moved = solve_matrix_game(&shifted);
        assert!((moved.value - base.value - 10.0).abs() < 1e-7);
        for (p, q) in base.row_strategy.iter().zip(&moved.row_strategy) {
            assert!((p - q).abs() < 1e-7);
        }
    }

    /// A repeated seed index must not overwrite its probability during lifting.
    #[test]
    fn double_oracle_removes_duplicate_seed_indices() {
        let payoffs = [vec![0.2, 0.8], vec![0.7, 0.1]];
        let rows = [0, 0];
        let cols = [0, 0];

        let reference = solve_matrix_game(&payoffs);
        let (solution, _) = double_oracle(
            payoffs.len(),
            payoffs[0].len(),
            OracleSeed {
                rows: Some(&rows),
                cols: Some(&cols),
            },
            OracleLimits::default(),
            |row, col| payoffs[row][col],
        );

        assert!((solution.value - reference.value).abs() < 1e-7);
        assert!((solution.row_strategy.iter().sum::<f64>() - 1.0).abs() < EPS);
        assert!((solution.col_strategy.iter().sum::<f64>() - 1.0).abs() < EPS);
        assert_equilibrium(&payoffs, &solution, 1e-7);
    }

    /// What one recorded run reports back to the test.
    #[derive(Default)]
    struct OracleRecord {
        /// The count of cell calls before each round call.
        cells_at_each_round: Vec<usize>,
        cells: usize,
        /// The restricted value of each round.
        values: Vec<f64>,
    }

    /// Records the order of the cell calls and the round calls of one run.
    ///
    /// [`double_oracle`] takes the oracle by value, so the record lives behind a
    /// shared handle. The test reads that handle after the run.
    struct RecordingOracle<'a> {
        payoffs: &'a [Vec<f64>],
        record: std::rc::Rc<std::cell::RefCell<OracleRecord>>,
    }

    impl CellOracle for RecordingOracle<'_> {
        fn cell(&mut self, row: usize, col: usize) -> f64 {
            self.record.borrow_mut().cells += 1;
            self.payoffs[row][col]
        }

        fn round(&mut self, solution: &MatrixSolution, stats: &OracleStats) {
            let mut record = self.record.borrow_mut();
            assert_eq!(
                stats.cells_requested as usize, record.cells,
                "the round must read the counters of this run"
            );
            let cells = record.cells;
            record.cells_at_each_round.push(cells);
            record.values.push(solution.value);
        }
    }

    /// The progress reporter publishes one answer for each round, and it
    /// publishes only after both best-response checks read their cells.
    #[test]
    fn the_round_hook_fires_after_both_best_response_checks() {
        // Rock, paper, scissors needs every row and every column, so the run
        // takes several rounds and each round adds one action to each side.
        let payoffs = vec![
            vec![0.5, 0.0, 1.0],
            vec![1.0, 0.5, 0.0],
            vec![0.0, 1.0, 0.5],
        ];
        let record = std::rc::Rc::new(std::cell::RefCell::new(OracleRecord::default()));
        let oracle = RecordingOracle {
            payoffs: &payoffs,
            record: std::rc::Rc::clone(&record),
        };

        let (solution, _) = double_oracle_with(
            payoffs.len(),
            payoffs[0].len(),
            OracleSeed::default(),
            OracleOrder::default(),
            OracleLimits::default(),
            oracle,
        );

        let record = record.borrow();
        let rounds = &record.cells_at_each_round;
        assert!(
            rounds.len() > 1,
            "this game needs more than one round, so the hook must fire more than one time"
        );
        // The first round solves one cell, and then both checks read the rest of
        // the first row and the first column. A hook that fired before those
        // checks would report only the one seeded cell.
        assert!(
            rounds[0] >= payoffs.len() + payoffs[0].len() - 1,
            "the hook fired before both checks read their cells: {rounds:?}"
        );
        // Each round reads at least one new cell, so the counts rise.
        for pair in rounds.windows(2) {
            assert!(pair[1] > pair[0], "{rounds:?}");
        }
        assert_eq!(record.values.len(), rounds.len());
        // The hook must not change the answer.
        assert!((solution.value - 0.5).abs() < 1e-7);
        assert_equilibrium(&payoffs, &solution, 1e-7);
    }

    /// A stopped round must not replace the last complete strategy.
    #[test]
    fn a_stopped_round_keeps_the_last_complete_strategy() {
        struct StoppingOracle<'a> {
            payoffs: &'a [Vec<f64>],
            calls: std::rc::Rc<std::cell::Cell<usize>>,
            rounds: std::rc::Rc<std::cell::RefCell<Vec<MatrixSolution>>>,
        }

        impl CellOracle for StoppingOracle<'_> {
            fn cell(&mut self, row: usize, col: usize) -> f64 {
                self.calls.set(self.calls.get() + 1);
                self.payoffs[row][col]
            }

            fn stop_requested(&mut self) -> bool {
                self.calls.get() >= 9
            }

            fn round(&mut self, solution: &MatrixSolution, _stats: &OracleStats) {
                self.rounds.borrow_mut().push(solution.clone());
            }
        }

        // The seeded 2-by-2 game has a mixed equilibrium. The first round adds
        // row 2 and column 2. The stop occurs on their missing cross cell.
        let payoffs = vec![
            vec![1.0, 0.0, 0.2],
            vec![0.0, 1.0, 0.2],
            vec![0.8, 0.8, 1.0],
        ];
        let seed = [0, 1];
        let calls = std::rc::Rc::new(std::cell::Cell::new(0));
        let rounds = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let (solution, _) = double_oracle_with(
            3,
            3,
            OracleSeed {
                rows: Some(&seed),
                cols: Some(&seed),
            },
            OracleOrder::default(),
            OracleLimits::default(),
            StoppingOracle {
                payoffs: &payoffs,
                calls: std::rc::Rc::clone(&calls),
                rounds: std::rc::Rc::clone(&rounds),
            },
        );

        let rounds = rounds.borrow();
        assert_eq!(calls.get(), 9);
        assert_eq!(rounds.len(), 1, "the stopped round reached the reporter");
        assert_eq!(solution, rounds[0]);
        assert_eq!(solution.row_strategy, vec![0.5, 0.5, 0.0]);
        assert_eq!(solution.col_strategy, vec![0.5, 0.5, 0.0]);
    }

    /// A stop before the first complete round must not select action zero.
    #[test]
    fn a_stop_before_the_first_round_returns_a_uniform_fallback() {
        struct FirstCellStop {
            calls: usize,
        }

        impl CellOracle for FirstCellStop {
            fn cell(&mut self, _row: usize, _col: usize) -> f64 {
                self.calls += 1;
                0.9
            }

            fn stop_requested(&mut self) -> bool {
                self.calls > 0
            }
        }

        let (solution, stats) = double_oracle_with(
            3,
            2,
            OracleSeed::default(),
            OracleOrder::default(),
            OracleLimits::default(),
            FirstCellStop { calls: 0 },
        );

        assert_eq!(stats.rounds_completed, 0);
        assert_eq!(solution.row_strategy, vec![1.0 / 3.0; 3]);
        assert_eq!(solution.col_strategy, vec![0.5; 2]);
        assert_eq!(solution.value, 0.5);
    }
}
