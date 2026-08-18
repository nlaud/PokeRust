//! Searches simultaneous stochastic game nodes by backward induction.
//!
//! Both players choose at each node.
//! Matrix rows contain P1 joint commands, and columns contain P2 joint commands.
//! Each cell contains the expected value of its command pair.
//! The matrix solution supplies the node value and both mixed strategies.
//!
//! [`SolverAlgorithm`] describes three algorithms that return the same answer.
//! They follow Algorithms 1 through 4 from Bošanský et al., AIJ 237, 2016.
//!
//! # Bounds
//!
//! Alpha-beta pruning can return a bound instead of an exact value.
//! Every matrix cell must contain an exact value.
//! Therefore, simultaneous search evaluates cells in the full `[0, 1]` window.
//!
//! Bounds are valid in two places:
//!
//! - [`SearchContext::serial_ab`] uses bounds during serialized search.
//! - A recursive simultaneous search can use bounds from serialized search.
//!
//! These windows contain the exact value.
//! Thus, the transposition table does not need bound flags.
//!
//! # Iterative deepening
//!
//! [`SolveConfig::iterative_deepening`](super::SolveConfig::iterative_deepening)
//! makes [`run`] search depth 1, then depth 2, and so on to the requested depth.
//! Each pass finishes before the next pass starts.
//!
//! One context serves every pass, so three things carry forward:
//!
//! 1. The turn cache. It holds resolved transitions and has no depth in its key.
//! 2. The transposition table. Its key holds the depth, so a deeper pass reuses
//!    a shallower value only where the horizons agree.
//! 3. The root support, through [`RootSeed`].
//!
//! The node budget and the deadline span the whole solve, not one pass. `run`
//! returns the last pass that neither limit stopped.
//!
//! # Cancellation
//!
//! [`CancelFlag`](super::CancelFlag) is the third stop reason, beside the node
//! budget and the deadline. It behaves as those two do: the search stops at the
//! next check point, it scores the rest of the tree statically, and it returns
//! the last complete deepening pass.
//!
//! The search reads the flag at four places:
//!
//! 1. Each node, through [`SearchContext::should_stop`].
//! 2. Each matrix cell, before the cell resolves a turn.
//! 3. Each chance successor, before the search descends into it.
//! 4. Inside one turn simulation, through the simulator abort signal.
//!
//! A point that the cancel reached takes a static score. Every chance node
//! therefore keeps a complete weighted average over its successors, because each
//! successor keeps its own probability and its own value.
//!
//! # Stopping inside one turn simulation
//!
//! Places 1 through 3 stop the search between two units of work. They cannot stop
//! a turn simulation that already runs, and one exact turn of a multi-hit or
//! spread move can be the largest unit in the whole solve.
//!
//! [`SearchContext::resolve`] therefore installs a simulator abort signal around
//! each `simulate_turn` call. It carries the time that remains before the
//! deadline and the cancel flag of the caller. The simulator reads the signal at
//! the branch loops that multiply a set, and a raised signal ends the expansion.
//!
//! A stopped simulation can return a partial branch set. The final signal check
//! can also reject a complete result that finished too late. `resolve` discards
//! the result in both cases. The cell then takes a static score, and the result
//! never enters the turn cache.
//!
//! # The worker pool
//!
//! [`SolveConfig::workers`](super::SolveConfig::workers) asks for more than one
//! worker. The root position then evaluates matrix cells in batches, and it
//! holds one [`SearchContext`] for each worker that a batch used.
//! `solver::pool` holds the permits, the batch runner, and the job seed.
//!
//! A worker context appears only when a batch gets a permit for it. A context
//! holds a transposition table, so a solve that never gets a permit pays for no
//! table. Read [`WorkerSeed`] for the rule.
//!
//! Three rules keep the answer the same as the answer of a serial solve:
//!
//! 1. A cell value is exact. A cache changes the speed of a cell, not its value.
//! 2. Only the root position makes a batch. One solve therefore has one level of
//!    parallelism, and it cannot oversubscribe the machine.
//! 3. The matrix solver and the double-oracle control loop stay serial. A worker
//!    answers cells, and it makes no decision.
//!
//! The double-oracle round uses a batch in two places. The first is the fill of
//! the missing restricted cells. The second is a prefetch before each
//! best-response check, because that check reads its cells one at a time.
//!
//! [`matrix::CellOracle::batch_limit`] bounds the prefetch. Without the bound
//! the prefetch would fill the whole matrix, which is the work that double
//! oracle exists to avoid.
//!
//! # What the pool leaves out
//!
//! The pool runs only under an exact chance mode.
//! [`ChanceMode::Sample`](super::chance::ChanceMode::Sample) draws from the
//! random generator, and a sampled cell value also depends on the caches of the
//! worker that ran it. A parallel sample could not repeat a serial answer, so a
//! sampling solve keeps the serial path.
//!
//! Each job still installs its own seeded generator, from
//! [`pool::job_seed`](super::pool::job_seed). An extra thread starts with no
//! override, so a job that reaches the generator stays deterministic.
//!
//! [`SearchContext::serial_ab`] also stays serial. Its extra turn simulations
//! cost more than the cells that it saves.
//!
//! The cost counters do depend on the thread schedule, because a cache hit
//! depends on the job order of one worker. A solve that hits the node budget or
//! the deadline depends on the schedule for the same reason. The value and both
//! strategies do not.
//!
//! Two workers write verbose output at the same time, and the lines interleave.
//! Set `VERBOSITY` to zero before a large search.
//!
//! # Midturn decisions
//!
//! A faint can require a replacement before the turn ends.
//! A self-switch move can require a pivot choice.
//! The search does not charge these decisions as another depth.
//! [`SolveConfig::max_forced_chain`](super::SolveConfig::max_forced_chain) limits the decision chain.

use std::collections::HashMap;
use std::collections::VecDeque;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use crate::data::pokemon_move::PokemonMove;
use crate::data::species::Species;
use crate::simulator::{AbortGuard, scoped_abort_signal, scoped_sample_rng, simulate_turn};
use crate::state::battle::{BattleCommand, BattleState, MatchState, Player, PlayerCommand};
use crate::state::dex_data::{MoveData, PokemonData};

use super::actions::{self, JointActions, Phase};
use super::chance::ChanceMode;
use super::eval::EvalContext;
use super::matrix::{self, CellJob, EPS, MatrixSolution};
use super::pool;
use super::{
    CancelFlag, JointActionProb, SolveConfig, SolveError, SolveResult, SolveStats, SolveWarning,
    SolverAlgorithm, cancel_requested,
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
///
/// [`SolveConfig::iterative_deepening`](super::SolveConfig::iterative_deepening)
/// turns the single search into a sequence of passes at depth 1, 2, and so on.
/// One `SearchContext` serves every pass, so the passes share the transposition
/// table, the turn cache, and the statistics.
///
/// `cancel` stops the search from another thread. See the module documentation.
///
/// `progress` reads each double-oracle round of the root position. See
/// [`super::solve_seeded_progress_cancellable`].
pub(super) fn run(
    state: &MatchState,
    pokemon_dex: &HashMap<Species, PokemonData>,
    move_dex: &HashMap<PokemonMove, MoveData>,
    config: &SolveConfig,
    progress: Option<super::RootProgress<'_>>,
    cancel: Option<&CancelFlag>,
) -> Result<SolveResult, SolveError> {
    match state {
        MatchState::TeamPreviewState(_) => return Err(SolveError::TeamPreviewUnsupported),
        MatchState::GameOverState { winner, .. } => {
            return Err(SolveError::GameAlreadyOver { winner: *winner });
        }
        MatchState::BattleState(_) => {}
    }

    let started = Instant::now();
    let mut ctx = SearchContext::new(pokemon_dex, move_dex, config, started, cancel);
    // One slot for each extra worker. A batch fills a slot when it gets a permit
    // for that worker. A filled slot lives for the whole solve, so a worker
    // keeps its tables across rounds and across deepening passes. Only the root
    // position hands the slots out.
    let mut helpers: Vec<Option<SearchContext<'_>>> =
        (1..worker_count(config)).map(|_| None).collect();

    // Depth 0 would make every cell an evaluation of the same position, so the
    // matrix would be constant and the "equilibrium" arbitrary. One ply is the
    // minimum that means anything.
    let target = config.depth.max(1);
    let first = if config.iterative_deepening { 1 } else { target };

    let mut reached = first;
    let mut solved: Option<Position> = None;
    let mut returned_pass_is_partial = false;

    for depth in first..=target {
        // The first pass must produce a strategy. Later passes must not exceed
        // the node budget or the deadline only to find that no search work
        // remains.
        if solved.is_some() && ctx.should_stop() {
            break;
        }

        // Taken rather than borrowed: the seed lives in the context so it
        // survives between passes, but `solve_position` needs the context
        // mutably. The pass writes a fresh seed back at the end.
        let seed = ctx.root_seed.take();
        let max_discarded = ctx.max_discarded;
        let action_truncations = ctx.action_truncations;
        let (root_depth, root_chain) =
            super::root_descent(actions::phase_of(state), depth, config.replacement_depth);
        // The first pass must return one strategy, including when the budget is
        // zero. Later passes reach this line only when the budget has space.
        ctx.record_node();
        // Only this call is the root, so only this call reports its rounds.
        let pass = ctx.solve_position(
            state,
            root_depth,
            root_chain,
            LOSS,
            WIN,
            seed.as_ref(),
            progress.map(|hook| RootReport { hook, depth }),
            &mut helpers,
        );

        // `budget_hit`, `deadline_hit`, and `cancel_hit` latch, and the loop
        // stops at the first pass that sets one, so the flags describe this pass
        // alone.
        if ctx.stopped() {
            // A complete shallower answer beats a partial deeper one. The
            // partial pass is kept only when no pass ever finished, because
            // something has to be returned.
            if solved.is_none() {
                reached = depth;
                solved = Some(pass);
                returned_pass_is_partial = true;
            } else {
                // The warnings must describe the returned pass. Discard
                // approximation metadata from the incomplete pass.
                ctx.max_discarded = max_discarded;
                ctx.action_truncations = action_truncations;
            }
            break;
        }

        ctx.root_seed = Some(root_seed_of(&pass));
        reached = depth;
        solved = Some(pass);
    }

    let position = solved.expect("the depth range is never empty, so one pass always runs");

    // Each batch already took the stop flags and the approximation metadata of
    // its workers, so only the counters remain. Worker index order keeps the
    // sum independent of the thread schedule.
    for helper in helpers.iter().flatten() {
        ctx.add_counters(helper);
    }

    let value = position.value.clamp(LOSS, WIN);
    let mut warnings = Vec::new();
    if returned_pass_is_partial {
        if let (true, Some(budget)) = (ctx.budget_hit, config.node_budget) {
            warnings.push(SolveWarning::BudgetExhausted { budget });
        }
        if let (true, Some(budget)) = (ctx.deadline_hit, config.deadline) {
            warnings.push(SolveWarning::DeadlineExceeded { budget });
        }
    }
    if ctx
        .cancel
        .is_some_and(CancelFlag::simulation_budget_hit)
        && let Some(budget) = ctx.cancel.and_then(CancelFlag::simulation_turn_budget)
    {
        warnings.push(SolveWarning::SimulationTurnBudgetExhausted { budget });
    }
    // The other two warnings say that the returned answer is part static, so
    // they apply to the returned pass alone. A cancel describes the whole
    // search, so it applies whether or not the returned pass is complete.
    if ctx.cancel_hit {
        warnings.push(SolveWarning::Cancelled);
    }
    if reached < target {
        warnings.push(SolveWarning::DepthNotReached { target, reached });
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
        p1_strategy: strategy_of(&position.p1, &position.row_strategy, EPS),
        p2_strategy: strategy_of(&position.p2, &position.col_strategy, EPS),
        depth_reached: reached,
        stats,
        warnings,
    })
}

/// Cells that one worker can prefetch before a best-response check.
///
/// A larger value fills more of the matrix, and it gives each worker more work.
/// Two cells for each worker keep the pool busy through one check without a
/// large amount of speculative work.
const PREFETCH_JOBS_PER_WORKER: usize = 2;

/// Adds one worker's counters to a statistics snapshot.
fn add_counters(stats: &mut SolveStats, other: &SolveStats) {
    stats.nodes_expanded += other.nodes_expanded;
    stats.turns_simulated += other.turns_simulated;
    stats.matrix_cells_evaluated += other.matrix_cells_evaluated;
    stats.matrix_cells_total += other.matrix_cells_total;
    stats.lps_solved += other.lps_solved;
    stats.ab_cutoffs += other.ab_cutoffs;
    stats.tt_hits += other.tt_hits;
    stats.turn_cache_hits += other.turn_cache_hits;
}

/// Atomically takes one slot from the shared node budget.
fn claim_node(nodes: &AtomicU64, budget: Option<u64>) -> bool {
    match budget {
        Some(budget) => nodes
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |count| {
                (count < budget).then_some(count + 1)
            })
            .is_ok(),
        None => {
            nodes.fetch_add(1, Ordering::Relaxed);
            true
        }
    }
}

/// The workers that one solve may use.
///
/// Four conditions hold the count at 1:
///
/// 1. The configuration asks for 1 worker or fewer.
/// 2. The algorithm is not double oracle. Only that algorithm has a batch.
/// 3. The chance mode samples. Read the module documentation for the reason.
/// 4. The process pool has a capacity of 1.
fn worker_count(cfg: &SolveConfig) -> usize {
    if cfg.workers <= 1
        || cfg.algorithm != SolverAlgorithm::DoubleOracle
        || matches!(cfg.chance, ChanceMode::Sample(_))
    {
        return 1;
    }
    cfg.workers.min(pool::shared().capacity()).max(1)
}

/// The root actions that a completed pass played with positive probability.
fn root_seed_of(position: &Position) -> RootSeed {
    RootSeed {
        rows: support_of(&position.row_strategy),
        cols: support_of(&position.col_strategy),
    }
}

/// The indices a mixed strategy actually plays, in ascending order and without
/// repeats.
fn support_of(strategy: &[f64]) -> Vec<usize> {
    (0..strategy.len())
        .filter(|&index| strategy[index] > EPS)
        .collect()
}

/// Pair actions with their probabilities, dropping the ones never played.
///
/// `floor` is the largest probability that still counts as never played. The
/// exact search passes [`EPS`], because a linear program leaves numerical dust
/// on an action that it does not play. The sampling search passes zero, because
/// explicit exploration gives every action a real probability, and that
/// probability can fall below [`EPS`] over a large action set.
pub(super) fn strategy_of(
    joint: &JointActions,
    probabilities: &[f64],
    floor: f64,
) -> Vec<JointActionProb> {
    let mut strategy: Vec<JointActionProb> = joint
        .actions
        .iter()
        .zip(probabilities)
        .filter(|&(_, &p)| p > floor)
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

/// The root support carried from one deepening pass to the next.
///
/// Every pass derives the root action set from the same position with the same
/// deterministic generator, so an index means the same action in each pass. The
/// next pass therefore opens its restricted game on the actions the previous
/// pass settled on, rather than on action 0.
///
/// This is an ordering heuristic and nothing more. Double oracle terminates on a
/// best response over the *full* action set, so it converges to the same value
/// from any starting set — a good seed only reduces the number of rounds.
struct RootSeed {
    rows: Vec<usize>,
    cols: Vec<usize>,
}

/// Where one deepening pass sends its root rounds.
///
/// The pass depth belongs here rather than in the hook, because one hook serves
/// every pass of one solve.
#[derive(Clone, Copy)]
struct RootReport<'a> {
    hook: super::RootProgress<'a>,
    depth: u8,
}

/// The cell oracle of one matrix game, and the reporter of its rounds.
///
/// [`matrix::double_oracle_with`] takes one value for both jobs. That is why
/// this type exists: a cell closure and a round closure would both borrow the
/// search context, and only one of them can.
struct SearchOracle<'ctx, 'cfg, 'pos> {
    ctx: &'ctx mut SearchContext<'cfg>,
    /// One slot for each extra worker. Empty below the root, and empty when the
    /// configuration asks for a serial solve. A slot holds no context until a
    /// batch gets a permit for that worker.
    helpers: &'ctx mut [Option<SearchContext<'cfg>>],
    /// Builds the context of an empty slot.
    seed: WorkerSeed<'cfg>,
    state: &'pos MatchState,
    p1: &'pos JointActions,
    p2: &'pos JointActions,
    depth: u8,
    chain: u8,
    /// The state hash of this position. It names the position inside a job seed.
    /// Zero when the oracle has no worker, because no job then needs a seed.
    root: u64,
    /// `None` below the root, and `None` when the caller wants no progress.
    report: Option<RootReport<'pos>>,
}

impl matrix::CellOracle for SearchOracle<'_, '_, '_> {
    fn cell(&mut self, row: usize, col: usize) -> f64 {
        self.ctx.cell_value(
            self.state,
            &self.p1.actions[row],
            &self.p2.actions[col],
            self.depth,
            self.chain,
        )
    }

    /// Evaluates one batch of cells over the workers of this oracle.
    ///
    /// The batch takes permits from the process pool. A batch with no permit
    /// runs every job on the calling thread, which is the serial path.
    ///
    /// Each job installs a seeded generator from its own identity. Read the
    /// module documentation for the rule that this protects.
    fn cells(&mut self, jobs: &[CellJob]) -> Vec<f64> {
        if self.helpers.is_empty() || jobs.len() <= 1 {
            return jobs.iter().map(|job| self.cell(job.row, job.col)).collect();
        }

        // Copied out of `self`, because the job closure runs on another thread
        // and `report` is not thread safe.
        let state = self.state;
        let p1 = self.p1;
        let p2 = self.p2;
        let depth = self.depth;
        let chain = self.chain;
        let root = self.root;
        let seed = &self.seed;

        // The calling thread runs the batch as worker 0, so it needs no permit.
        // A worker also costs a transposition table, so the request asks for one
        // worker for each `PREFETCH_JOBS_PER_WORKER` jobs. A small batch then
        // starts no thread that it cannot keep busy.
        let wanted = self
            .helpers
            .len()
            .min(jobs.len() / PREFETCH_JOBS_PER_WORKER);
        let permits = pool::shared().acquire(wanted);

        let values = {
            let mut workers: Vec<&mut SearchContext<'_>> = Vec::with_capacity(1 + permits.count());
            workers.push(&mut *self.ctx);
            for slot in self.helpers.iter_mut().take(permits.count()) {
                workers.push(slot.get_or_insert_with(|| seed.build()));
            }
            pool::run_jobs(&mut workers, jobs.len(), |ctx, index| {
                let job = jobs[index];
                let _rng =
                    scoped_sample_rng(pool::job_seed(root, depth, job.round, job.row, job.col));
                ctx.cell_value(
                    state,
                    &p1.actions[job.row],
                    &p2.actions[job.col],
                    depth,
                    chain,
                )
            })
        };
        drop(permits);

        // Worker index order, so the warnings of the answer do not depend on the
        // thread schedule. The counters merge one time, at the end of the solve.
        for helper in self.helpers.iter().flatten() {
            self.ctx.adopt(helper);
        }
        values
    }

    fn batch_limit(&self) -> usize {
        if self.helpers.is_empty() {
            return 1;
        }
        (self.helpers.len() + 1) * PREFETCH_JOBS_PER_WORKER
    }

    fn round(&mut self, solution: &MatrixSolution, oracle: &matrix::OracleStats) {
        let Some(report) = self.report else {
            return;
        };
        // The run reports its own counters only when it returns, so add the
        // work of this matrix by hand. A complete answer of the same pass then
        // holds the same numbers.
        let mut stats = self.ctx.stats.clone();
        for helper in self.helpers.iter().flatten() {
            add_counters(&mut stats, &helper.stats);
        }
        stats.lps_solved += oracle.lps_solved;
        stats.ab_cutoffs += oracle.cutoffs;
        stats.elapsed = self.ctx.started.elapsed();
        (report.hook)(super::RootRound {
            depth: report.depth,
            value: solution.value,
            p1_strategy: strategy_of(self.p1, &solution.row_strategy, EPS),
            p2_strategy: strategy_of(self.p2, &solution.col_strategy, EPS),
            stats,
        });
    }
}

/// The parts that build one extra worker context.
///
/// A worker holds its own transposition table, and that table costs several
/// megabytes at the default capacity. The root position therefore builds a
/// worker only when a batch gets a permit for it. A small solve then pays for no
/// worker at all.
#[derive(Clone)]
struct WorkerSeed<'a> {
    pokemon_dex: &'a HashMap<Species, PokemonData>,
    move_dex: &'a HashMap<PokemonMove, MoveData>,
    cfg: &'a SolveConfig,
    cancel: Option<&'a CancelFlag>,
    started: Instant,
    nodes: Arc<AtomicU64>,
}

impl<'a> WorkerSeed<'a> {
    /// One worker context.
    ///
    /// The worker gets its own transposition table, turn cache, counters, and
    /// stop flags. It shares the node counter, the cancel flag, the start time,
    /// and the configuration.
    ///
    /// The worker takes no root seed. Only the control thread reads that seed,
    /// and only at the root position.
    fn build(&self) -> SearchContext<'a> {
        SearchContext {
            pokemon_dex: self.pokemon_dex,
            move_dex: self.move_dex,
            cfg: self.cfg,
            stats: SolveStats::default(),
            tt: TranspositionTable::new(self.cfg.tt_capacity),
            turn_cache: TurnCache::new(self.cfg.turn_cache_capacity),
            max_discarded: 0.0,
            action_truncations: [None, None],
            budget_hit: false,
            deadline_hit: false,
            cancel_hit: false,
            cancel: self.cancel,
            started: self.started,
            root_seed: None,
            nodes: Arc::clone(&self.nodes),
        }
    }
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
    deadline_hit: bool,
    /// Set when the search read a raised [`CancelFlag`].
    /// It latches for the same reason the other two do.
    cancel_hit: bool,
    /// The stop signal of the caller, if the caller supplied one.
    cancel: Option<&'a CancelFlag>,
    /// When the solve began. Every deepening pass shares it, so the deadline
    /// bounds the whole solve rather than one pass.
    started: Instant,
    /// The previous deepening pass's root support, if there was one.
    root_seed: Option<RootSeed>,
    /// The expanded nodes of every worker of this solve.
    ///
    /// The node budget bounds the whole solve, not one worker. A serial solve
    /// has one context, so this counter equals `stats.nodes_expanded` and the
    /// budget behaves as it did before the pool existed.
    nodes: Arc<AtomicU64>,
}

impl<'a> SearchContext<'a> {
    fn new(
        pokemon_dex: &'a HashMap<Species, PokemonData>,
        move_dex: &'a HashMap<PokemonMove, MoveData>,
        cfg: &'a SolveConfig,
        started: Instant,
        cancel: Option<&'a CancelFlag>,
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
            deadline_hit: false,
            cancel_hit: false,
            cancel,
            started,
            root_seed: None,
            nodes: Arc::new(AtomicU64::new(0)),
        }
    }

    /// The parts that build one extra worker of this solve.
    fn worker_seed(&self) -> WorkerSeed<'a> {
        WorkerSeed {
            pokemon_dex: self.pokemon_dex,
            move_dex: self.move_dex,
            cfg: self.cfg,
            cancel: self.cancel,
            started: self.started,
            nodes: Arc::clone(&self.nodes),
        }
    }

    /// Takes the stop flags and the approximation metadata of one worker.
    ///
    /// A batch calls this after every job ends. The operations are an OR and two
    /// maximums, so a repeated call changes nothing.
    fn adopt(&mut self, other: &SearchContext<'_>) {
        self.budget_hit |= other.budget_hit;
        self.deadline_hit |= other.deadline_hit;
        self.cancel_hit |= other.cancel_hit;
        if other.max_discarded > self.max_discarded {
            self.max_discarded = other.max_discarded;
        }
        let dropped = |(kept, total): (usize, usize)| total - kept;
        for slot in 0..self.action_truncations.len() {
            let Some(candidate) = other.action_truncations[slot] else {
                continue;
            };
            if self.action_truncations[slot]
                .is_none_or(|previous| dropped(candidate) > dropped(previous))
            {
                self.action_truncations[slot] = Some(candidate);
            }
        }
    }

    /// Adds the counters of one worker to this context.
    ///
    /// The solve calls this one time for each worker, after the last pass. An
    /// earlier call would count the same work twice.
    fn add_counters(&mut self, other: &SearchContext<'_>) {
        add_counters(&mut self.stats, &other.stats);
    }

    /// Records one node without a budget check.
    fn record_node(&mut self) {
        self.stats.nodes_expanded += 1;
        self.nodes.fetch_add(1, Ordering::Relaxed);
    }

    /// Records one node if the shared budget has space.
    fn try_record_node(&mut self) -> bool {
        if !claim_node(&self.nodes, self.cfg.node_budget) {
            self.budget_hit = true;
            return false;
        }
        self.stats.nodes_expanded += 1;
        true
    }

    /// Scores one position with the configured leaf evaluator.
    ///
    /// The evaluator reads the move dex, so every call site builds the same
    /// context here instead of assembling one of its own.
    fn score(&self, battle: &BattleState) -> f64 {
        (self.cfg.eval)(battle, &EvalContext::new(self.pokemon_dex, self.move_dex))
    }

    /// Whether serialized alpha-beta bounds should be computed at each node.
    fn serial_bounds_enabled(&self) -> bool {
        self.cfg.algorithm == SolverAlgorithm::SerializedBounds || self.cfg.use_serialized_bounds
    }

    /// Whether the search must stop: the node budget is spent, the deadline has
    /// passed, or a caller raised the cancel flag. Latches the reason so the
    /// caller can report it once, rather than the search failing loudly
    /// mid-flight. A stopped search evaluates the rest of the tree statically;
    /// it never abandons a node half-solved, so every matrix cell keeps an exact
    /// value.
    ///
    /// All three tests read a monotone quantity, so a stop never becomes a
    /// resume.
    fn should_stop(&mut self) -> bool {
        let budget_hit = if let Some(budget) = self.cfg.node_budget
            && self.nodes.load(Ordering::Relaxed) >= budget
        {
            self.budget_hit = true;
            true
        } else {
            false
        };
        let deadline_hit = self.deadline_expired();
        let cancel_hit = self.cancel_requested();
        let simulation_budget_hit = self
            .cancel
            .is_some_and(CancelFlag::simulation_budget_hit);
        budget_hit || simulation_budget_hit || deadline_hit || cancel_hit
    }

    /// Check the deadline and save the result for the solve warning.
    fn deadline_expired(&mut self) -> bool {
        if let Some(deadline) = self.cfg.deadline
            && self.started.elapsed() >= deadline
        {
            self.deadline_hit = true;
            return true;
        }
        false
    }

    /// Check the cancel flag and save the result for the solve warning.
    ///
    /// The latch makes every later check cheap, and it also keeps one search
    /// consistent: a flag that another thread raises mid-pass cannot make one
    /// half of that pass stop and the other half continue.
    fn cancel_requested(&mut self) -> bool {
        if self.cancel_hit {
            return true;
        }
        if cancel_requested(self.cancel) {
            self.cancel_hit = true;
            return true;
        }
        false
    }

    /// Whether a stop reason has latched.
    fn stopped(&self) -> bool {
        self.budget_hit
            || self
                .cancel
                .is_some_and(CancelFlag::simulation_budget_hit)
            || self.deadline_hit
            || self.cancel_hit
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

        if depth == 0 || self.should_stop() {
            return self.score(battle);
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

        // No seed below the root: the seed indexes the root action set, and it
        // means nothing at any other position. No report below the root either:
        // a round of a child position answers a different question.
        // No worker below the root: one solve has one level of parallelism.
        if !self.try_record_node() {
            return self.score(battle);
        }
        let value = self
            .solve_position(state, depth, chain, alpha, beta, None, None, &mut [])
            .value;
        self.tt.store(key, depth, chain, value);
        value
    }

    /// Build and solve the matrix game at `state`.
    ///
    /// `seed` is the previous deepening pass's root support. Only the root call
    /// supplies it, and only double oracle reads it.
    ///
    /// `report` publishes each double-oracle round. Only the root call supplies
    /// it, for the same reason.
    ///
    /// `helpers` holds the extra worker contexts. Only the root call supplies
    /// them, so a child position always solves its matrix on one thread.
    #[allow(clippy::too_many_arguments)]
    fn solve_position(
        &mut self,
        state: &MatchState,
        depth: u8,
        chain: u8,
        alpha: f64,
        beta: f64,
        seed: Option<&RootSeed>,
        report: Option<RootReport<'_>>,
        helpers: &mut [Option<SearchContext<'a>>],
    ) -> Position {
        let battle = as_battle(state).expect("solve_position requires a battle position");
        let phase = actions::phase_of(state);
        let p1 = self.joint_actions(battle, Player::P1, phase);
        let p2 = self.joint_actions(battle, Player::P2, phase);

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
            SolverAlgorithm::DoubleOracle => self.double_oracle(
                state, depth, chain, alpha, beta, &p1, &p2, seed, report, helpers,
            ),
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
    /// [`matrix::double_oracle`] holds the algorithm. This method supplies the
    /// cell oracle and folds the returned counters into the search statistics.
    ///
    /// `seed`, when present, replaces the one-by-one start with the previous
    /// deepening pass's root support. See [`RootSeed`] for why that cannot move
    /// the value.
    #[allow(clippy::too_many_arguments)]
    fn double_oracle(
        &mut self,
        state: &MatchState,
        depth: u8,
        chain: u8,
        alpha: f64,
        beta: f64,
        p1: &JointActions,
        p2: &JointActions,
        seed: Option<&RootSeed>,
        report: Option<RootReport<'_>>,
        helpers: &mut [Option<SearchContext<'a>>],
    ) -> MatrixSolution {
        let limits = matrix::OracleLimits {
            alpha,
            beta,
            low: LOSS,
            high: WIN,
        };
        let seed = matrix::OracleSeed {
            rows: seed.map(|seed| seed.rows.as_slice()),
            cols: seed.map(|seed| seed.cols.as_slice()),
        };
        // The oracle owns the context for the whole run, so one type serves the
        // cell calls and the round calls. Two closures cannot, because both
        // would borrow the context.
        // The hash names this position inside a job seed. A serial oracle needs
        // no seed, so it also needs no hash.
        let root = if helpers.is_empty() {
            0
        } else {
            hash_state(state)
        };
        let (solution, oracle) = matrix::double_oracle_with(
            p1.actions.len(),
            p2.actions.len(),
            seed,
            limits,
            SearchOracle {
                seed: self.worker_seed(),
                ctx: self,
                helpers,
                state,
                p1,
                p2,
                depth,
                chain,
                root,
                report,
            },
        );
        // `cell_value` counts the evaluated cells itself, so `cells_requested`
        // would double count them.
        self.stats.lps_solved += oracle.lps_solved;
        self.stats.ab_cutoffs += oracle.cutoffs;
        solution
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
        // The root does not enter node_value before it resolves a matrix cell.
        // Check here to stop new turn simulations after the deadline, and after
        // a cancel.
        let deadline_hit = self.deadline_expired();
        let cancel_hit = self.cancel_requested();
        if deadline_hit || cancel_hit {
            let battle = as_battle(state).expect("cell_value requires a battle position");
            return self.score(battle);
        }
        let Some(branches) = self.resolve(state, p1_commands, p2_commands) else {
            // The turn simulation stopped part way, so its branch set is not a
            // distribution. Score the position as a stop between two cells does.
            self.latch_abort_reason();
            let battle = as_battle(state).expect("cell_value requires a battle position");
            return self.score(battle);
        };

        let mut expected = 0.0;
        // Consumed rather than borrowed, so each successor's own subtree is
        // dropped before the next one is expanded — this is what keeps peak
        // memory proportional to depth instead of to the whole tree.
        for (child, probability) in branches {
            if probability <= 0.0 {
                continue;
            }
            let (child_depth, child_chain) = self.descend(&child, depth, chain);
            // A cancel between two successors scores the rest of them
            // statically, which `node_value` does on its own. The average
            // therefore still covers every successor with its own probability.
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
        if depth == 0 || self.should_stop() {
            return self.score(battle);
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
        // A deadline can expire, and a caller can cancel, after serial_ab starts
        // and before it resolves a cell.
        let deadline_hit = self.deadline_expired();
        let cancel_hit = self.cancel_requested();
        if deadline_hit || cancel_hit {
            let battle = as_battle(state).expect("serial_cell requires a battle position");
            return self.score(battle);
        }
        let Some(branches) = self.resolve(state, p1_commands, p2_commands) else {
            // The turn simulation stopped part way, so its branch set is not a
            // distribution. Score the position as a stop between two cells does.
            self.latch_abort_reason();
            let battle = as_battle(state).expect("serial_cell requires a battle position");
            return self.score(battle);
        };

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
            // `serial_ab` reads the stop reasons at its own top, so a cancel
            // between two successors gives the rest a static score. The running
            // average keeps every successor and its probability.
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
    ///
    /// `None` means that the deadline expired, or that a caller cancelled, before
    /// the caller accepted the result. The branch set can be partial or complete.
    /// The caller must discard it and score the position statically.
    fn resolve(
        &mut self,
        state: &MatchState,
        p1_commands: &[BattleCommand],
        p2_commands: &[BattleCommand],
    ) -> Option<Vec<(MatchState, f64)>> {
        let cache_key = self.turn_cache.enabled().then(|| {
            (
                hash_state(state),
                command_key(p1_commands),
                command_key(p2_commands),
            )
        });

        if let Some(hit) = cache_key.and_then(|key| self.turn_cache.get(&key)) {
            self.stats.turn_cache_hits += 1;
            return Some(hit);
        }

        if self
            .cancel
            .is_some_and(|control| !control.claim_simulation_turn())
        {
            return None;
        }
        self.stats.turns_simulated += 1;
        // The simulator stops the expansion of one turn at the same two limits
        // the search itself obeys. Without this the search can only stop between
        // two cells, and one exact turn of a multi-hit or spread move can run far
        // past the deadline.
        let remaining = self
            .cfg
            .deadline
            .map(|deadline| deadline.saturating_sub(self.started.elapsed()));
        let cancel = self.cancel.map(CancelFlag::shared);
        let abort = (remaining.is_some() || cancel.is_some())
            .then(|| scoped_abort_signal(remaining, cancel));

        let simulated = simulate_turn(
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
        );

        // Read the guard before it drops, and drop it before the return so the
        // next turn simulation installs its own signal.
        let aborted = abort.as_ref().is_some_and(AbortGuard::aborted);
        drop(abort);
        if aborted {
            // A stopped result never enters the turn cache. A later cell with the
            // same key must simulate the turn again.
            return None;
        }

        let mut raw: Vec<(MatchState, f64)> = simulated
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
        Some(kept)
    }

    /// Latch the reason that a turn simulation aborted, so the answer carries the
    /// matching warning.
    ///
    /// The abort raises on the same two conditions the search reads between two
    /// cells, so one read of each condition names it.
    fn latch_abort_reason(&mut self) {
        self.deadline_expired();
        self.cancel_requested();
    }

    /// The depth and forced-chain counter a successor should be searched at.
    ///
    /// A successor that is still mid-turn — waiting for a replacement or a
    /// self-switch pivot — is a decision point but not a new turn, so it does
    /// not consume a ply. [`super::forced_descent`] holds the rule, and
    /// [`SolveConfig::replacement_depth`] gives such a decision its own depth.
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

/// Evaluates single matrix cells of one root position.
///
/// [`run`] solves a whole game, and it never exposes one cell.
/// A caller that needs named cells, such as the exploitability check in
/// [`exploit`](super::exploit), uses this wrapper instead.
///
/// One instance holds one [`SearchContext`], so every cell shares the
/// transposition table and the turn cache of that context.
/// A cell is exact for the configured depth and [`ChanceMode`](super::chance::ChanceMode),
/// as a cell of [`run`] is.
pub struct RootCells<'a> {
    ctx: SearchContext<'a>,
    depth: u8,
}

impl<'a> RootCells<'a> {
    pub fn new(
        pokemon_dex: &'a HashMap<Species, PokemonData>,
        move_dex: &'a HashMap<PokemonMove, MoveData>,
        config: &'a SolveConfig,
    ) -> Self {
        RootCells {
            // Depth 0 would score the root without a decision, as it would in
            // `run`. One turn is the minimum.
            depth: config.depth.max(1),
            // No cancel flag: one cell is the unit of work here, and the caller
            // stops between cells rather than inside one.
            ctx: SearchContext::new(pokemon_dex, move_dex, config, Instant::now(), None),
        }
    }

    /// The value of one command pair at `state`, to the configured depth.
    ///
    /// `state` must be a battle position.
    ///
    /// A forced root takes the same start depth that [`run`] gives it. Without
    /// that call a cell of this wrapper would answer a different question than
    /// the same cell of `run`.
    pub fn cell_value(
        &mut self,
        state: &MatchState,
        p1_commands: &[BattleCommand],
        p2_commands: &[BattleCommand],
    ) -> f64 {
        let (depth, chain) = super::root_descent(
            actions::phase_of(state),
            self.depth,
            self.ctx.cfg.replacement_depth,
        );
        self.ctx
            .cell_value(state, p1_commands, p2_commands, depth, chain)
    }

    /// What the evaluated cells cost so far.
    pub fn stats(&self) -> SolveStats {
        let mut stats = self.ctx.stats.clone();
        stats.elapsed = self.ctx.started.elapsed();
        stats
    }

    /// Why the cell values are approximate.
    pub fn warnings(&self) -> Vec<SolveWarning> {
        let mut warnings = Vec::new();
        if self.ctx.budget_hit
            && let Some(budget) = self.ctx.cfg.node_budget
        {
            warnings.push(SolveWarning::BudgetExhausted { budget });
        }
        if self
            .ctx
            .cancel
            .is_some_and(CancelFlag::simulation_budget_hit)
            && let Some(budget) = self
                .ctx
                .cancel
                .and_then(CancelFlag::simulation_turn_budget)
        {
            warnings.push(SolveWarning::SimulationTurnBudgetExhausted { budget });
        }
        if self.ctx.deadline_hit
            && let Some(budget) = self.ctx.cfg.deadline
        {
            warnings.push(SolveWarning::DeadlineExceeded { budget });
        }
        if self.ctx.max_discarded > EPS {
            warnings.push(SolveWarning::ChanceMassDiscarded {
                max_fraction: self.ctx.max_discarded,
            });
        }
        for (player, truncation) in [
            (Player::P1, self.ctx.action_truncations[0]),
            (Player::P2, self.ctx.action_truncations[1]),
        ] {
            if let Some((kept, total)) = truncation {
                warnings.push(SolveWarning::ActionsTruncated {
                    player,
                    kept,
                    total,
                });
            }
        }
        warnings
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
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Barrier};
    use std::thread;

    use super::{TranspositionTable, claim_node};

    #[test]
    fn transposition_entries_distinguish_forced_chain_depth() {
        let mut table = TranspositionTable::new(8);
        table.store(17, 2, 1, 0.25);

        assert_eq!(table.probe(17, 2, 1), Some(0.25));
        assert_eq!(table.probe(17, 2, 0), None);
        assert_eq!(table.probe(17, 2, 2), None);
    }

    #[test]
    fn concurrent_node_claims_stop_at_the_shared_budget() {
        const WORKERS: usize = 8;
        const BUDGET: u64 = 19;
        let nodes = Arc::new(AtomicU64::new(0));
        let barrier = Arc::new(Barrier::new(WORKERS));

        let claims: u64 = thread::scope(|scope| {
            let handles: Vec<_> = (0..WORKERS)
                .map(|_| {
                    let nodes = Arc::clone(&nodes);
                    let barrier = Arc::clone(&barrier);
                    scope.spawn(move || {
                        barrier.wait();
                        (0..BUDGET)
                            .filter(|_| claim_node(&nodes, Some(BUDGET)))
                            .count() as u64
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|handle| handle.join().expect("a budget worker panicked"))
                .sum()
        });

        assert_eq!(claims, BUDGET);
        assert_eq!(nodes.load(Ordering::Relaxed), BUDGET);
    }
}
