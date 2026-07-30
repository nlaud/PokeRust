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
        let (j, value) = extremum(a[0].iter().copied(), |x, y| x < y);
        return Some(MatrixSolution {
            value,
            row_strategy: vec![1.0],
            col_strategy: unit_vector(n, j),
            used_lp: false,
        });
    }
    if n == 1 {
        // P1 maximizes down the only column.
        let (i, value) = extremum(a.iter().map(|row| row[0]), |x, y| x > y);
        return Some(MatrixSolution {
            value,
            row_strategy: unit_vector(m, i),
            col_strategy: vec![1.0],
            used_lp: false,
        });
    }
    None
}

/// Returns a pure saddle-point equilibrium when one exists.
fn saddle_point(a: &[Vec<f64>], m: usize, n: usize) -> Option<MatrixSolution> {
    let (best_row, maximin) = extremum(
        a.iter()
            .map(|row| row.iter().copied().fold(f64::INFINITY, f64::min)),
        |x, y| x > y,
    );
    let (best_col, minimax) = extremum(
        (0..n).map(|j| (0..m).map(|i| a[i][j]).fold(f64::NEG_INFINITY, f64::max)),
        |x, y| x < y,
    );

    ((maximin - minimax).abs() <= EPS).then(|| MatrixSolution {
        value: maximin,
        row_strategy: unit_vector(m, best_row),
        col_strategy: unit_vector(n, best_col),
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
                    !rows.iter().any(|&k| {
                        k != i && cols.iter().all(|&j| a[k][j] > a[i][j] + EPS)
                    })
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
                    !cols.iter().any(|&l| {
                        l != j && rows.iter().all(|&i| a[i][l] < a[i][j] - EPS)
                    })
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The defining property of an equilibrium: neither player can improve by
    /// deviating to any pure strategy. Every solved matrix should satisfy this,
    /// whichever code path produced it.
    fn assert_equilibrium(a: &[Vec<f64>], solution: &MatrixSolution, tol: f64) {
        let n = a[0].len();

        let row_sum: f64 = solution.row_strategy.iter().sum();
        let col_sum: f64 = solution.col_strategy.iter().sum();
        assert!((row_sum - 1.0).abs() < tol, "row strategy sums to {row_sum}");
        assert!((col_sum - 1.0).abs() < tol, "col strategy sums to {col_sum}");
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
}
