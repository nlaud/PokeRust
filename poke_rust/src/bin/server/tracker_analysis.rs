//! The solver job of a tracker session.
//!
//! A tracker session holds a belief, not a concrete `MatchState`, so the job
//! draws one world with [`determinize_seeded`]. That one world is the search
//! position of an exact or sampled search, and it also renders every strategy
//! row. A belief search reads the belief itself and still renders its rows
//! against the drawn world.
//!
//! # The depth ladder
//!
//! The panel must update while the search goes deeper, so the job runs one rung
//! for each depth from one through the configured depth. A rung publishes a
//! complete checkpoint, and the client reads the newest one. A rung that starts
//! runs until it finishes or the client cancels it.
//!
//! # The record
//!
//! [`TrackerAnalysisState`] copies the generation rule of `analysis.rs`. Every
//! committed turn raises the generation and cancels the running job, and
//! [`TrackerAnalysisState::publish`] drops a rung whose generation or job ID is
//! no longer current.
//!
//! # What the response carries
//!
//! The tracker has one user, and that user typed both rosters, so the response
//! carries the strategy and the win odds of both players. The battle endpoints
//! keep their own privacy rules: `analysis.rs` serves a hotseat battle, where
//! Player 1 reads every endpoint.
//!
//! Engine error text still stays on the server console. A
//! [`DeterminizeError`](poke_rust::information::determinize::DeterminizeError)
//! names a species and a belief mon index, which is a guess about the live
//! opponent rather than a fact the user recorded.
//!
//! # Simulation progress
//!
//! One shared simulation-turn budget covers the full depth ladder. The response
//! reports the claimed count and the hard limit. All rungs and preview worlds
//! use the same count.
//!
//! The tracker accepts only `ismcts` and `mccfr`. These algorithms search the
//! belief instead of treating one sampled hidden world as the true state.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use poke_rust::information::describe::describe_unknown;
use poke_rust::information::determinize::{DeterminizeConfig, determinize_seeded};
use poke_rust::information::inference::InferenceConfig;
use poke_rust::information::unknowns::{
    UnknownBattleState, UnknownPokemonState, UnknownTeamPreviewState,
};
use poke_rust::meta::{MetaDex, MetaFormat};
use poke_rust::solver::preview::{
    OpenListConfig, PreviewChoiceProb, PreviewConfig, solve_open_list_preview_cancellable,
};
use poke_rust::solver::{self, CancelFlag, JointActionProb, SolveConfig};
use poke_rust::state::battle::{BattleState, MatchState, Player};

use crate::bot::{BotProfile, BotSearchConfig, MAX_SAFE_INTEGER};
use crate::dto::{
    MAX_STRATEGY_ROWS, PreviewChoiceDto, StrategyRowDto, TrackerAnalysisCheckpointDto,
    TrackerAnalysisDto, TrackerAnalysisRungDto,
};
use crate::mapping;
use crate::session::{Dexes, MetaDexes};
use crate::tracker::TrackerSession;

/// The largest number of worlds that one team-preview rung draws.
///
/// Each world repeats the cell work of the whole preview matrix, so the cost
/// grows with this count. A doubles preview holds 32,400 cells, and the time
/// limit already stops the run.
const MAX_PREVIEW_WORLDS: usize = 8;

/// One bring-and-lead choice, rendered from the roster of one side.
#[derive(Debug, Clone)]
pub struct TrackerPreviewChoice {
    leads: Vec<String>,
    back: Vec<String>,
}

/// One joint action of a strategy, rendered against the drawn world.
#[derive(Debug, Clone)]
pub struct TrackerStrategyRow {
    pub(crate) commands: Vec<crate::dto::CommandOptionDto>,
    /// The bring-and-lead choice of a team-preview row.
    /// `None` in a battle row.
    pub(crate) preview: Option<TrackerPreviewChoice>,
    pub(crate) probability: f64,
}

/// Which question one rung answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PositionKind {
    /// A battle position with both leads on the field.
    Battle,
    /// The bring-and-lead choice, before the first `leads` line.
    TeamPreview,
}

impl PositionKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            PositionKind::Battle => "battle",
            PositionKind::TeamPreview => "teamPreview",
        }
    }
}

/// The complete answer of one ladder rung.
#[derive(Debug, Clone)]
pub struct TrackerAnalysisCheckpoint {
    /// The generation of the position that the search read.
    pub generation: u64,
    pub turn_number: u16,
    /// The question that this rung answers.
    pub position: PositionKind,
    /// The depth of this rung.
    pub depth_reached: u8,
    pub p1_win_odds: f64,
    pub p2_win_odds: f64,
    pub p1_strategy: Vec<TrackerStrategyRow>,
    pub p2_strategy: Vec<TrackerStrategyRow>,
    /// True when the P2 rows form one strategy for one private state.
    pub p2_strategy_is_playable: bool,
    pub elapsed: Duration,
    /// The seed of the draw and the search.
    pub seed: u64,
    pub warnings: Vec<String>,
}

/// The rung that a running job started.
///
#[derive(Debug, Clone, Copy)]
struct RungProgress {
    depth: u8,
}

/// The job that is running now.
#[derive(Debug)]
struct RunningJob {
    id: u64,
    generation: u64,
    started: Instant,
    /// The rung that runs now. `None` before the first rung starts.
    rung: Option<RungProgress>,
    /// Stops the ladder before a rung starts, and inside a running search.
    cancel: CancelFlag,
}

/// Identifies one ladder job and carries its cancel flag.
#[derive(Debug)]
struct JobTicket {
    id: u64,
    generation: u64,
    cancel: CancelFlag,
}

/// The solver record of one tracker session.
#[derive(Debug, Default)]
pub struct TrackerAnalysisState {
    /// The generation of the current position.
    /// Every committed turn raises it by one.
    generation: u64,
    /// The ID for the next job in this session.
    next_job_id: u64,
    running: Option<RunningJob>,
    /// The newest complete rung.
    /// A failure and a cancellation both keep it.
    checkpoint: Option<TrackerAnalysisCheckpoint>,
    /// Player 1's win odds at the position before the current one.
    previous_p1_win_odds: Option<f64>,
    /// Why the last job produced no rung.
    last_error: Option<String>,
}

impl TrackerAnalysisState {
    /// Raises the generation and cancels the running job.
    ///
    /// Keeps the checkpoint, so the panel shows the last complete answer until
    /// a newer rung arrives. The `stale` flag of the view marks it.
    ///
    /// A checkpoint of the position that is ending supplies the comparison win
    /// odds of the next position.
    pub fn invalidate(&mut self, next_turn_number: u16) {
        self.previous_p1_win_odds = self
            .checkpoint
            .as_ref()
            .filter(|c| c.generation == self.generation)
            .filter(|c| c.turn_number.checked_add(1) == Some(next_turn_number))
            .map(|c| c.p1_win_odds);
        self.generation += 1;
        if let Some(job) = self.running.take() {
            job.cancel.cancel();
        }
    }

    /// The generation of the current position.
    ///
    /// `solve.rs` reads it, so a streaming job can tell its own position from a
    /// newer one.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Cancels the running job and clears every stored answer.
    ///
    /// `DELETE /api/tracker/{id}/analysis` calls this, so the panel returns to
    /// the state it had before the user started a search.
    pub fn reset(&mut self) {
        if let Some(job) = self.running.take() {
            job.cancel.cancel();
        }
        self.checkpoint = None;
        self.previous_p1_win_odds = None;
        self.last_error = None;
    }

    /// Cancels the running job and keeps every stored answer.
    ///
    /// A deleted session calls this, so a search stops rather than running on
    /// with no target.
    pub fn cancel_running(&mut self) {
        if let Some(job) = self.running.take() {
            job.cancel.cancel();
        }
    }

    /// Records the job that is starting and returns its ticket.
    /// Cancels an earlier job, so one ladder runs at a time.
    #[cfg(test)]
    fn start(&mut self) -> JobTicket {
        self.start_with_budget(0)
    }

    fn start_with_budget(&mut self, budget: u64) -> JobTicket {
        if let Some(job) = self.running.take() {
            job.cancel.cancel();
        }
        let id = self.next_job_id;
        self.next_job_id = self.next_job_id.wrapping_add(1);
        let cancel = if budget == 0 {
            CancelFlag::new()
        } else {
            CancelFlag::with_simulation_turn_budget(budget)
        };
        self.running = Some(RunningJob {
            id,
            generation: self.generation,
            started: Instant::now(),
            rung: None,
            cancel: cancel.clone(),
        });
        self.last_error = None;
        JobTicket {
            id,
            generation: self.generation,
            cancel,
        }
    }

    /// True when this ticket still names the running job of the current
    /// position.
    fn is_current(&self, job: &JobTicket) -> bool {
        job.generation == self.generation
            && self
                .running
                .as_ref()
                .is_some_and(|running| running.id == job.id && running.generation == job.generation)
    }

    /// Records the rung that is starting.
    ///
    /// The panel reads this record between two complete rungs, so it can show
    /// the depth in progress and the spent part of the budget.
    fn note_rung(&mut self, job: &JobTicket, depth: u8) {
        if !self.is_current(job) {
            return;
        }
        if let Some(running) = self.running.as_mut() {
            running.rung = Some(RungProgress { depth });
        }
    }

    /// Stores one complete rung and leaves the job running.
    ///
    /// Drops a rung of an old generation or of a replaced job. It also drops a
    /// rung that is not deeper than the rung already stored for this position,
    /// so the panel never steps backwards.
    fn publish(&mut self, job: &JobTicket, checkpoint: TrackerAnalysisCheckpoint) {
        if !self.is_current(job) {
            return;
        }
        let already_deeper = self.checkpoint.as_ref().is_some_and(|stored| {
            stored.generation == checkpoint.generation
                && stored.depth_reached >= checkpoint.depth_reached
        });
        if already_deeper {
            return;
        }
        self.checkpoint = Some(checkpoint);
        self.last_error = None;
    }

    /// Ends one job.
    ///
    /// A failure keeps every rung that already published.
    fn finish(&mut self, job: &JobTicket, outcome: Result<(), String>) {
        if !self.is_current(job) {
            return;
        }
        self.running = None;
        if let Err(message) = outcome {
            self.last_error = Some(message);
        }
    }

    /// The record of this session, for the client.
    pub fn view(&self, profile: Option<&BotProfile>) -> TrackerAnalysisDto {
        let phase = if profile.is_none() {
            "off"
        } else if self.running.is_some() {
            "running"
        } else if self.last_error.is_some() {
            "failed"
        } else if self.checkpoint.is_some() {
            "complete"
        } else {
            "idle"
        };
        TrackerAnalysisDto {
            generation: self.generation,
            phase: phase.to_string(),
            running_ms: self
                .running
                .as_ref()
                .map(|job| job.started.elapsed().as_millis() as u64),
            target_depth: profile.map(|p| p.view.depth),
            rung: self.running.as_ref().and_then(|job| {
                job.rung.map(|rung| rung_dto(rung, &job.cancel))
            }),
            checkpoint: self
                .checkpoint
                .as_ref()
                .map(|c| checkpoint_dto(c, self.generation)),
            previous_p1_win_odds: self.previous_p1_win_odds,
            error: self.last_error.clone(),
            profile: profile.map(|p| p.view.clone()),
        }
    }

    /// The last complete rung, stale or not.
    #[cfg(test)]
    fn checkpoint(&self) -> Option<&TrackerAnalysisCheckpoint> {
        self.checkpoint.as_ref()
    }
}

/// Builds the progress row of the rung that runs.
///
/// The fraction compares the claimed simulation turns with the hard limit.
fn rung_dto(rung: RungProgress, control: &CancelFlag) -> TrackerAnalysisRungDto {
    let turns_simulated = control.simulation_turns();
    let simulation_turn_budget = control.simulation_turn_budget().unwrap_or_default();
    TrackerAnalysisRungDto {
        depth: rung.depth,
        turns_simulated,
        simulation_turn_budget,
        fraction: if simulation_turn_budget == 0 {
            0.0
        } else {
            turns_simulated as f64 / simulation_turn_budget as f64
        },
    }
}

/// Builds the wire row of one checkpoint.
fn checkpoint_dto(
    checkpoint: &TrackerAnalysisCheckpoint,
    generation: u64,
) -> TrackerAnalysisCheckpointDto {
    TrackerAnalysisCheckpointDto {
        generation: checkpoint.generation,
        stale: checkpoint.generation != generation,
        turn_number: checkpoint.turn_number,
        position: checkpoint.position.as_str().to_string(),
        depth_reached: checkpoint.depth_reached,
        elapsed_ms: checkpoint.elapsed.as_millis() as u64,
        seed: checkpoint.seed,
        p1_win_odds: checkpoint.p1_win_odds,
        p2_win_odds: checkpoint.p2_win_odds,
        p1_strategy: checkpoint
            .p1_strategy
            .iter()
            .map(strategy_row_dto)
            .collect(),
        p2_strategy: checkpoint
            .p2_strategy
            .iter()
            .map(strategy_row_dto)
            .collect(),
        p2_strategy_is_playable: checkpoint.p2_strategy_is_playable,
        warnings: checkpoint.warnings.clone(),
    }
}

pub(crate) fn strategy_row_dto(row: &TrackerStrategyRow) -> StrategyRowDto {
    StrategyRowDto {
        commands: row.commands.clone(),
        preview: row.preview.as_ref().map(|choice| PreviewChoiceDto {
            leads: choice.leads.clone(),
            back: choice.back.clone(),
        }),
        probability: row.probability,
    }
}

/// Everything one ladder needs after the caller drops the session lock.
struct LadderInputs {
    search: BotSearchConfig,
    seed: u64,
    /// The depth horizon. The ladder runs one rung for each depth up to it.
    target_depth: u8,
    generation: u64,
    turn_number: u16,
    belief: UnknownBattleState,
    /// The team-preview belief of the session.
    /// The ladder searches it while no side has a lead on the field.
    preview_belief: Option<UnknownTeamPreviewState>,
    inference: InferenceConfig,
    dexes: Arc<Dexes>,
    meta: Arc<MetaDexes>,
    format: MetaFormat,
}

/// What one ladder reports back to the session record.
///
/// The task holds no session lock while it searches. Each hook takes the lock,
/// writes one field, and returns.
struct LadderHooks<'a> {
    /// Stores one complete rung.
    publish: &'a dyn Fn(TrackerAnalysisCheckpoint),
    /// Records the depth of the rung that is starting.
    note_rung: &'a dyn Fn(u8),
}

/// Starts one ladder for the current position of a tracker session.
///
/// Does nothing when the session holds no profile. The caller holds the session
/// lock, so this call only spawns the task. The task takes the lock itself, and
/// only between rungs.
pub fn start_job(
    tracker_id: &str,
    session: &mut TrackerSession,
    dexes: Arc<Dexes>,
    meta: Arc<MetaDexes>,
    sessions: Arc<Mutex<HashMap<String, TrackerSession>>>,
) {
    let Some(profile) = session.solver_profile.as_ref() else {
        return;
    };

    let inputs = LadderInputs {
        search: profile.search,
        seed: profile.view.seed.unwrap_or_else(random_seed),
        target_depth: profile.view.depth,
        generation: session.analysis.generation,
        turn_number: session.belief.turn_number,
        belief: session.belief.clone(),
        preview_belief: session.preview_belief.clone(),
        inference: crate::analysis::clone_inference_config(&session.inference_config),
        dexes,
        meta,
        format: MetaFormat::from_active_per_side(session.active_per_side),
    };

    let job = session
        .analysis
        .start_with_budget(profile.view.simulation_turn_budget);
    let tracker_id = tracker_id.to_string();

    tokio::task::spawn_blocking(move || {
        // A cancel before the first rung saves the whole ladder.
        if job.cancel.is_cancelled() {
            return;
        }
        let outcome = {
            let publish = |checkpoint: TrackerAnalysisCheckpoint| {
                let mut guard = sessions.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(session) = guard.get_mut(&tracker_id) {
                    session.analysis.publish(&job, checkpoint);
                }
            };
            let note_rung = |depth: u8| {
                let mut guard = sessions.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(session) = guard.get_mut(&tracker_id) {
                    session.analysis.note_rung(&job, depth);
                }
            };
            let hooks = LadderHooks {
                publish: &publish,
                note_rung: &note_rung,
            };
            run_ladder(&inputs, &job.cancel, &hooks)
        };
        // A cancelled ladder reports nothing at all. The replacement job already
        // owns the record, and `finish` checks the ticket for the final race.
        if job.cancel.is_cancelled() {
            return;
        }
        let mut guard = sessions.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(session) = guard.get_mut(&tracker_id) {
            session.analysis.finish(&job, outcome);
        }
    });
}

/// Draws the seed of one job.
///
/// The checkpoint publishes this number, and the client reads it as a JSON
/// number, so it must survive a round trip through a JavaScript double.
fn random_seed() -> u64 {
    rand::random::<u64>() & MAX_SAFE_INTEGER
}

/// Runs the ladder of the current position and publishes each complete rung.
///
/// A position with no lead on either side is the team preview, and the preview
/// search answers it with one rung. Every other position runs the depth ladder
/// of the battle search.
///
/// Returns an error only when no rung can run at all, or when a rung fails.
/// Every rung that already published stays in the record.
fn run_ladder(
    inputs: &LadderInputs,
    cancel: &CancelFlag,
    hooks: &LadderHooks,
) -> Result<(), String> {
    let started = Instant::now();
    let meta = inputs
        .meta
        .for_format(inputs.format)
        .ok_or_else(no_usage_cache)?;

    if let Some(belief) = preview_position(inputs) {
        return run_preview_rung(inputs, belief, meta, started, cancel, hooks);
    }
    leads_are_on_the_field(&inputs.belief)?;

    // The tracker user is Player 1, so the draw copies their own side and
    // samples the live opponent.
    let determinize = belief_draw_config(&inputs.inference);
    let drawn = determinize_seeded(
        inputs.seed,
        &inputs.belief,
        meta,
        &inputs.dexes.pokemon_dex,
        &inputs.dexes.move_dex,
        &determinize,
    )
    .map_err(engine_error)?;
    let draw_warning_count = drawn.warnings.len();
    let battle = drawn.state;
    let state = MatchState::BattleState(battle.clone());
    let search_inputs = SearchInputs {
        seed: inputs.seed,
        dexes: &inputs.dexes,
        meta,
        belief: Some(&inputs.belief),
        determinize: belief_draw_config(&inputs.inference),
    };

    let first_depth = inputs.search.first_depth(inputs.target_depth);
    for depth in first_depth..=inputs.target_depth {
        if cancel.is_cancelled() {
            return Ok(());
        }
        let search = inputs.search.with_depth(depth);
        (hooks.note_rung)(depth);

        // A solver panic must not poison the session mutex, so catch it here
        // and report it as an ordinary job failure.
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            one_search(search, &search_inputs, &state, None, cancel)
        }));
        let rung = match caught {
            Ok(result) => result?,
            Err(payload) => return Err(panic_message(payload)),
        };
        // A cancelled search returns the work that finished, which is not a
        // complete rung.
        if cancel.is_cancelled() {
            return Ok(());
        }

        let elapsed = started.elapsed();
        let mut warnings = rung.warnings;
        if draw_warning_count > 0 {
            warnings.push(format!(
                "The determinizer reported {draw_warning_count} warning(s) while it drew the \
                 opponent."
            ));
        }
        if let Some(line) = unknown_bring_line(&inputs.belief) {
            warnings.push(line);
        }
        warnings.push(drawn_world_note(inputs.search));

        (hooks.publish)(TrackerAnalysisCheckpoint {
            generation: inputs.generation,
            turn_number: inputs.turn_number,
            position: PositionKind::Battle,
            depth_reached: rung.depth_reached,
            p1_win_odds: rung.p1_win_odds,
            p2_win_odds: rung.p2_win_odds,
            p1_strategy: strategy_rows(&battle, Player::P1, &rung.p1_strategy),
            p2_strategy: strategy_rows(&battle, Player::P2, &rung.p2_strategy),
            p2_strategy_is_playable: p2_strategy_is_playable(inputs.search),
            elapsed,
            seed: inputs.seed,
            warnings,
        });
        if rung.depth_reached < depth || cancel.simulation_budget_hit() {
            return Ok(());
        }
    }
    Ok(())
}

/// The sampling detail of one approximate rung.
///
/// An exact rung has none of this, because it reads every action of the depth
/// horizon rather than a sample of them.
#[derive(Debug, Clone)]
pub(crate) struct RungSampling {
    /// The name of the algorithm, as the profile spells it.
    pub algorithm: &'static str,
    /// The iterations that the search finished.
    pub iterations: u64,
    /// The worlds that a belief search drew. `None` for a concrete search.
    pub particles: Option<usize>,
    /// The seed of the draw and the search.
    pub seed: u64,
    /// The leaf evaluator that scored the depth horizon.
    pub evaluator: &'static str,
}

/// The result of one rung, before the job renders it.
pub(crate) struct RungResult {
    pub depth_reached: u8,
    pub p1_win_odds: f64,
    pub p2_win_odds: f64,
    pub p1_strategy: Vec<JointActionProb>,
    pub p2_strategy: Vec<JointActionProb>,
    pub warnings: Vec<String>,
    /// What the search cost.
    ///
    /// A sampling search fills the node count, the turn count, and the elapsed
    /// time. It leaves every matrix counter at zero, because it builds no
    /// matrix.
    pub stats: solver::SolveStats,
    /// `None` for an exact search.
    pub sampling: Option<RungSampling>,
}

/// Names the leaf evaluator of one search.
///
/// The client shows this name, because the evaluator sets the value of every
/// position at the depth horizon.
fn evaluator_name(eval: poke_rust::solver::eval::LeafEvaluator) -> &'static str {
    use poke_rust::solver::eval;
    if std::ptr::fn_addr_eq(eval, eval::fitted as eval::LeafEvaluator) {
        "fitted"
    } else if std::ptr::fn_addr_eq(eval, eval::fitted_mlp as eval::LeafEvaluator) {
        "fittedMlp"
    } else if std::ptr::fn_addr_eq(eval, eval::heuristic as eval::LeafEvaluator) {
        "heuristic"
    } else {
        "custom"
    }
}

/// Converts an exact solver result without changing its completed depth.
fn exact_rung(result: solver::SolveResult) -> RungResult {
    RungResult {
        depth_reached: result.depth_reached,
        p1_win_odds: result.p1_win_odds,
        p2_win_odds: result.p2_win_odds,
        p1_strategy: result.p1_strategy,
        p2_strategy: result.p2_strategy,
        warnings: warning_lines(&result.warnings),
        stats: result.stats,
        sampling: None,
    }
}

/// Converts the statistics of a sampling search to the shared shape.
fn sampled_stats(stats: &poke_rust::solver::mcts::MctsStats) -> solver::SolveStats {
    solver::SolveStats {
        nodes_expanded: stats.nodes_created,
        turns_simulated: stats.turns_simulated,
        elapsed: stats.elapsed,
        ..solver::SolveStats::default()
    }
}

/// Everything that [`one_search`] needs, for either session kind.
///
/// A tracker session and a battle session build this record differently. The
/// search itself is the same.
pub(crate) struct SearchInputs<'a> {
    /// The seed of the draw and the search.
    pub seed: u64,
    pub dexes: &'a Dexes,
    pub meta: &'a MetaDex,
    /// The belief that `ismcts` and `mccfr` read.
    /// `None` for a session that holds no belief.
    pub belief: Option<&'a UnknownBattleState>,
    /// The draw rules of a belief search.
    pub determinize: DeterminizeConfig,
}

/// Reports that the profile needs a belief that the session does not hold.
fn no_belief() -> String {
    "This algorithm searches a belief, and this position has none. Use an exact algorithm or \
     mcts."
        .to_string()
}

/// Routes one rung to its solver entry point.
///
/// Every arm passes `cancel`, so a raised flag stops the search that runs.
///
/// `progress` reads each double-oracle round of the root position. Only the
/// exact search has rounds, so every other arm ignores it.
pub(crate) fn one_search(
    search: BotSearchConfig,
    inputs: &SearchInputs<'_>,
    state: &MatchState,
    progress: Option<solver::RootProgress<'_>>,
    cancel: &CancelFlag,
) -> Result<RungResult, String> {
    let pokemon_dex = &inputs.dexes.pokemon_dex;
    let move_dex = &inputs.dexes.move_dex;
    let meta = inputs.meta;
    let cancel = Some(cancel);
    match search {
        BotSearchConfig::Exact(config) => {
            let result = solver::solve_seeded_progress_cancellable(
                inputs.seed,
                state,
                pokemon_dex,
                move_dex,
                &config,
                progress,
                cancel,
            )
            .map_err(engine_error)?;
            Ok(exact_rung(result))
        }
        BotSearchConfig::Mcts(config) => {
            let result = solver::mcts::search_cancellable(
                inputs.seed,
                state,
                pokemon_dex,
                move_dex,
                &config,
                cancel,
            )
            .map_err(engine_error)?;
            Ok(RungResult {
                depth_reached: config.depth,
                p1_win_odds: result.p1_win_odds,
                p2_win_odds: result.p2_win_odds,
                p1_strategy: result.p1_strategy,
                p2_strategy: result.p2_strategy,
                warnings: warning_lines(&result.warnings),
                stats: sampled_stats(&result.stats),
                sampling: Some(RungSampling {
                    algorithm: "mcts",
                    iterations: result.stats.iterations,
                    particles: None,
                    seed: inputs.seed,
                    evaluator: evaluator_name(config.eval),
                }),
            })
        }
        BotSearchConfig::Ismcts(config) => {
            let belief = inputs.belief.ok_or_else(no_belief)?;
            let result = solver::ismcts::search_belief_cancellable(
                inputs.seed,
                belief,
                meta,
                pokemon_dex,
                move_dex,
                &config,
                &inputs.determinize,
                cancel,
            )
            .map_err(engine_error)?;
            Ok(RungResult {
                depth_reached: config.search.depth,
                p1_win_odds: result.p1_win_odds,
                p2_win_odds: result.p2_win_odds,
                p1_strategy: result.p1_strategy,
                p2_strategy: result.p2_strategy,
                warnings: warning_lines(&result.warnings),
                stats: sampled_stats(&result.stats),
                sampling: Some(RungSampling {
                    algorithm: "ismcts",
                    iterations: result.stats.iterations,
                    particles: Some(result.particles),
                    seed: inputs.seed,
                    evaluator: evaluator_name(config.search.eval),
                }),
            })
        }
        BotSearchConfig::Mccfr(config) => {
            let belief = inputs.belief.ok_or_else(no_belief)?;
            let result = solver::mccfr::search_belief_cancellable(
                inputs.seed,
                belief,
                meta,
                pokemon_dex,
                move_dex,
                &config,
                &inputs.determinize,
                cancel,
            )
            .map_err(engine_error)?;
            Ok(RungResult {
                depth_reached: config.search.depth,
                p1_win_odds: result.p1_win_odds,
                p2_win_odds: result.p2_win_odds,
                p1_strategy: result.p1_strategy,
                p2_strategy: result.p2_strategy,
                warnings: warning_lines(&result.warnings),
                stats: sampled_stats(&result.stats),
                sampling: Some(RungSampling {
                    algorithm: "mccfr",
                    iterations: result.stats.iterations,
                    particles: Some(config.particles),
                    seed: inputs.seed,
                    evaluator: evaluator_name(config.search.eval),
                }),
            })
        }
    }
}

/// The draw rules of a belief search that Player 1 asks for.
///
/// The draw copies the side of Player 1, and it samples the live opponent.
pub(crate) fn belief_draw_config(inference: &InferenceConfig) -> DeterminizeConfig {
    DeterminizeConfig {
        inference: crate::analysis::clone_inference_config(inference),
        observer: Player::P1,
        ..DeterminizeConfig::default()
    }
}

/// Reports that the tracker has no position to search yet.
///
/// The first `leads` line puts both sides on the field. Before it, one side or
/// both sides have no active Pokemon, and no search can run.
pub(crate) fn leads_are_on_the_field(belief: &UnknownBattleState) -> Result<(), String> {
    leads_error(
        belief.p1_active_mons.is_empty(),
        belief.p2_active_mons.is_empty(),
    )
}

/// The message of a position that has no leads on the field.
fn leads_error(p1_is_empty: bool, p2_is_empty: bool) -> Result<(), String> {
    if p1_is_empty || p2_is_empty {
        return Err(
            "The search needs both leads on the field. Record a 'leads' line first.".to_string(),
        );
    }
    Ok(())
}

// ── The team-preview rung ────────────────────────────────────────────────────

/// Names the way that a preview answer falls short of the true game.
pub(crate) const PREVIEW_MEAN_MATRIX_NOTE: &str = "The search solved the mean matrix of the drawn worlds, so both players play one strategy \
     for every world. A real opponent reads its own hidden stats and can pick another lead.";

/// The belief to search when the position is the team preview.
///
/// The tracker records a real battle, and the first `leads` line puts both
/// sides on the field. Before that line the position is the bring-and-lead
/// choice, and the preview belief is the only description of it.
///
/// A mid-battle double faint also empties both sides, so the turn number
/// guards the branch. Only turn zero is the preview.
fn preview_position(inputs: &LadderInputs) -> Option<&UnknownTeamPreviewState> {
    let is_preview = position_is_team_preview(
        inputs.belief.p1_active_mons.is_empty(),
        inputs.belief.p2_active_mons.is_empty(),
        inputs.turn_number,
    );
    is_preview
        .then_some(inputs.preview_belief.as_ref())
        .flatten()
}

/// True when the position is the team preview.
///
/// Both sides must have no active Pokemon, and no turn can have run.
pub(crate) fn position_is_team_preview(p1_is_empty: bool, p2_is_empty: bool, turn_number: u16) -> bool {
    p1_is_empty && p2_is_empty && turn_number == 0
}

/// The battle configuration of every cell below a preview choice.
///
/// A sampling profile carries no [`SolveConfig`], because the preview search
/// solves each cell with the exact search. The rung therefore builds one from
/// the depth and the physics of that profile.
///
/// `analysis.rs` calls this for the preview position of a bot battle, which
/// takes the same profile and the same solver.
pub(crate) fn preview_battle_config(search: BotSearchConfig) -> SolveConfig {
    let sampled = |mcts: poke_rust::solver::mcts::MctsConfig| SolveConfig {
        depth: mcts.depth,
        damage_rolls: mcts.damage_rolls,
        consider_crit: mcts.consider_crit,
        max_actions_per_player: None,
        deadline: None,
        ..SolveConfig::default()
    };
    match search {
        BotSearchConfig::Exact(config) => SolveConfig {
            deadline: None,
            ..config
        },
        BotSearchConfig::Mcts(config) => sampled(config),
        BotSearchConfig::Ismcts(config) => sampled(config.search),
        BotSearchConfig::Mccfr(config) => sampled(config.search),
    }
}

/// How many worlds the preview rung draws.
///
/// A belief profile already asks for several worlds, so the rung caps that
/// count at [`MAX_PREVIEW_WORLDS`]. Every other profile reads one world.
///
/// `analysis.rs` calls this for the preview position of a bot battle.
pub(crate) fn preview_worlds(search: BotSearchConfig) -> usize {
    match search {
        BotSearchConfig::Ismcts(config) => config.particles.clamp(1, MAX_PREVIEW_WORLDS),
        BotSearchConfig::Mccfr(config) => config.particles.clamp(1, MAX_PREVIEW_WORLDS),
        _ => 1,
    }
}

/// Searches the team preview and publishes one rung.
///
/// The preview search runs double oracle one time over the whole choice
/// matrix, so it has no depth ladder. Each depth would repeat the complete run,
/// and the cell cache cannot carry a value from one depth to another.
fn run_preview_rung(
    inputs: &LadderInputs,
    belief: &UnknownTeamPreviewState,
    meta: &MetaDex,
    started: Instant,
    cancel: &CancelFlag,
    hooks: &LadderHooks,
) -> Result<(), String> {
    let battle = preview_battle_config(inputs.search);
    let depth = battle.depth;
    (hooks.note_rung)(depth);

    let config = OpenListConfig {
        preview: PreviewConfig {
            battle,
            deadline: None,
        },
        worlds: preview_worlds(inputs.search),
        seed: inputs.seed,
    };
    let determinize = belief_draw_config(&inputs.inference);

    // A solver panic must not poison the session mutex, so catch it here and
    // report it as an ordinary job failure.
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        solve_open_list_preview_cancellable(
            belief,
            meta,
            &inputs.dexes.pokemon_dex,
            &inputs.dexes.move_dex,
            &config,
            &determinize,
            Some(cancel),
        )
    }));
    let result = match caught {
        Ok(outcome) => outcome.map_err(engine_error)?,
        Err(payload) => return Err(panic_message(payload)),
    };
    // A cancelled search returns the work that finished, which is not a
    // complete rung.
    if cancel.is_cancelled() {
        return Ok(());
    }

    let elapsed = started.elapsed();
    let mut warnings = warning_lines(&result.warnings);
    warnings.push(PREVIEW_MEAN_MATRIX_NOTE.to_string());
    warnings.push(preview_worlds_note(result.sampling.worlds));
    if let Some(line) = sampling_error_line(result.sampling.worlds, result.sampling.standard_error)
    {
        warnings.push(line);
    }
    if !result.draw_warnings.is_empty() {
        warnings.push(format!(
            "The determinizer reported {} warning(s) while it drew the opponent.",
            result.draw_warnings.len()
        ));
    }
    (hooks.publish)(TrackerAnalysisCheckpoint {
        generation: inputs.generation,
        turn_number: inputs.turn_number,
        position: PositionKind::TeamPreview,
        depth_reached: depth,
        p1_win_odds: result.p1_win_odds,
        p2_win_odds: result.p2_win_odds,
        p1_strategy: preview_rows(&belief.p1_mons, &result.p1_strategy),
        p2_strategy: preview_rows(&belief.p2_mons, &result.p2_strategy),
        // Each row is one bring-and-lead choice of the mean matrix, so the row
        // list is one strategy. `PREVIEW_MEAN_MATRIX_NOTE` names its limit.
        p2_strategy_is_playable: true,
        elapsed,
        seed: inputs.seed,
        warnings,
    });
    Ok(())
}

/// Names the guess that every preview cell rests on.
///
/// A battle rung says this with [`drawn_world_note`], and a preview rung needs
/// its own line. The preview search reads concrete worlds rather than the
/// belief, so no profile makes it a belief search, and a single world gives the
/// complete answer one guess of the opponent's hidden data.
///
/// [`sampling_error_line`] measures the spread of two or more worlds, and one
/// world has no spread, so this line is the only warning of a one-world rung.
pub(crate) fn preview_worlds_note(worlds: usize) -> String {
    if worlds == 1 {
        "The search drew one world of the belief, so the whole answer assumes one guess of the \
         opponent's hidden data. Only ismcts and mccfr draw more than one world."
            .to_string()
    } else {
        format!(
            "The search drew {worlds} worlds of the belief, so each cell is the mean of \
             {worlds} guesses of the opponent's hidden data."
        )
    }
}

/// Reports how much the drawn worlds disagree about the win odds.
///
/// One world gives no spread to measure, so the line appears from two worlds.
pub(crate) fn sampling_error_line(worlds: usize, standard_error: Option<f64>) -> Option<String> {
    standard_error.map(|error| {
        format!(
            "The {worlds} drawn world(s) give a standard error of {:.1} points on the win odds.",
            100.0 * error
        )
    })
}

/// The highest-rate preview choices of one player, rendered from its roster.
pub(crate) fn preview_rows(
    mons: &[UnknownPokemonState],
    strategy: &[PreviewChoiceProb],
) -> Vec<TrackerStrategyRow> {
    let mut ordered: Vec<&PreviewChoiceProb> = strategy.iter().collect();
    ordered.sort_by(|a, b| b.probability.total_cmp(&a.probability));
    ordered.truncate(MAX_STRATEGY_ROWS);
    ordered
        .into_iter()
        .map(|choice| TrackerStrategyRow {
            commands: Vec::new(),
            preview: Some(TrackerPreviewChoice {
                leads: roster_names(mons, &choice.choice.active_indices),
                back: roster_names(mons, &choice.choice.back_indices),
            }),
            probability: choice.probability,
        })
        .collect()
}

/// Names the roster entries that one index list selects.
///
/// The tracker user typed both rosters, so every species is known. An index
/// outside the roster cannot happen, and the fallback keeps the row complete.
fn roster_names(mons: &[UnknownPokemonState], indices: &[usize]) -> Vec<String> {
    indices
        .iter()
        .map(|&index| match mons.get(index) {
            Some(mon) => describe_unknown(&mon.possible_species),
            None => "Unknown".to_string(),
        })
        .collect()
}

pub(crate) fn no_usage_cache() -> String {
    "The search draws the opponent's hidden data from usage data, and no usage cache is loaded \
     (see meta_scraper/README.md)."
        .to_string()
}

/// Names the way that this rung falls short of the true position.
///
/// An exact or sampled search reads one drawn world, so it assumes one guess of
/// every hidden value. A belief search reads the whole belief and unions the
/// actions of several worlds, so its rows still name the moves of one drawn
/// world.
pub(crate) fn drawn_world_note(search: BotSearchConfig) -> String {
    match search {
        BotSearchConfig::Ismcts(_) | BotSearchConfig::Mccfr(_) => {
            "This algorithm searched the belief. Each row names the moves of one drawn world. \
             The opponent rows mix private builds, so they are not one playable strategy."
                .to_string()
        }
        _ => "This algorithm searched one drawn world of the belief, so the answer assumes one \
              guess of the opponent's hidden data. Only ismcts and mccfr search the belief."
            .to_string(),
    }
}

/// Names the cost of a Player 1 bring that the tracker never learned.
///
/// `create_tracker` gives the whole Player 1 sheet to the belief, because the
/// `leads` line arrives later. A `back` clause on the opening `leads` line cuts
/// that sheet to the bring. Without the clause the drawn world gives Player 1 a
/// bench that the real game does not hold, so the rows can name a switch that
/// Player 1 cannot make.
///
/// Returns `None` once the roster matches the bring of the format.
pub(crate) fn unknown_bring_line(belief: &UnknownBattleState) -> Option<String> {
    let roster = belief.p1_active_mons.len()
        + belief.p1_known_back_mons.len()
        + belief.p1_possible_back_mons.len()
        + belief.p1_fainted_mons.len();
    let brought = belief.active_per_side as usize + belief.back_mons_per_side as usize;
    (roster > brought).then(|| {
        format!(
            "The Player 1 roster holds {roster} Pokemon, and this format brings {brought}. The \
             search can therefore read a switch to a Pokemon that Player 1 did not bring. Add a \
             'back' clause to the opening leads line to state the bring."
        )
    })
}

/// True when the P2 rows describe one private state.
pub(crate) fn p2_strategy_is_playable(search: BotSearchConfig) -> bool {
    !matches!(
        search,
        BotSearchConfig::Ismcts(_) | BotSearchConfig::Mccfr(_)
    )
}

/// Renders one solver warning for the panel.
///
/// The tracker user owns both rosters, so every figure of the search is theirs
/// to read.
fn warning_line(warning: &solver::SolveWarning) -> String {
    match warning {
        solver::SolveWarning::BudgetExhausted { budget } => {
            format!("The {budget}-node budget ran out, so deeper nodes took a static score.")
        }
        solver::SolveWarning::SimulationTurnBudgetExhausted { budget } => format!(
            "The search exhausted the {budget}-turn simulation budget. Later positions used static scores."
        ),
        solver::SolveWarning::DeadlineExceeded { budget } => format!(
            "The {} ms limit expired, so deeper nodes took a static score.",
            budget.as_millis()
        ),
        solver::SolveWarning::DepthNotReached { target, reached } => {
            format!("The search completed depth {reached} of the {target} requested.")
        }
        solver::SolveWarning::ChanceMassDiscarded { max_fraction } => format!(
            "One chance node discarded up to {:.1}% of its outcome probability.",
            100.0 * max_fraction
        ),
        solver::SolveWarning::ActionsTruncated {
            player,
            kept,
            total,
        } => format!(
            "The action cap kept {kept} of {player:?}'s {total} actions, so the search can miss \
             an action."
        ),
        solver::SolveWarning::Cancelled => {
            "The search was cancelled, so the answer holds only the work that finished.".to_string()
        }
    }
}

/// Every distinct warning of one rung, in order.
pub(crate) fn warning_lines(warnings: &[solver::SolveWarning]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    warnings
        .iter()
        .map(warning_line)
        .filter(|line| seen.insert(line.clone()))
        .collect()
}

/// The joint actions that the panel shows, highest rate first.
///
/// The list keeps at most [`MAX_STRATEGY_ROWS`] actions. A tie between two
/// equal rates keeps the order that the solver returned, because `sort_by` is
/// stable.
fn top_actions(strategy: &[JointActionProb]) -> Vec<&JointActionProb> {
    let mut ordered: Vec<&JointActionProb> = strategy.iter().collect();
    ordered.sort_by(|a, b| b.probability.total_cmp(&a.probability));
    ordered.truncate(MAX_STRATEGY_ROWS);
    ordered
}

/// The highest-rate joint actions of one player, rendered against `battle`.
pub(crate) fn strategy_rows(
    battle: &BattleState,
    player: Player,
    strategy: &[JointActionProb],
) -> Vec<TrackerStrategyRow> {
    top_actions(strategy)
        .into_iter()
        .map(|action| TrackerStrategyRow {
            commands: action
                .commands
                .iter()
                .enumerate()
                .map(|(slot_idx, command)| {
                    mapping::command_option(battle, player, slot_idx, command)
                })
                .collect(),
            preview: None,
            probability: action.probability,
        })
        .collect()
}

/// Converts an engine error to a job message.
///
/// The engine writes its own text, and that text names a drawn species and a
/// belief mon index. That guess is not something the tracker user recorded, so
/// the detail goes to the server console and the client reads a fixed line.
pub(crate) fn engine_error(error: impl std::fmt::Display) -> String {
    eprintln!("tracker analysis job: the search failed: {error}");
    "The search failed. The server console holds the reason.".to_string()
}

/// Converts a caught panic payload to a job error message.
pub(crate) fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    let detail = payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "no payload".to_string());
    eprintln!("tracker analysis job: the search panicked: {detail}");
    "The search panicked. The server console holds the reason.".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use poke_rust::data::species::Species;
    use poke_rust::solver::SolveWarning;
    use poke_rust::state::battle::{BattleCommand, SwitchCommand, TeamPreviewCommand};
    use std::sync::OnceLock;

    static PREVIEW_ROSTER: OnceLock<Vec<UnknownPokemonState>> = OnceLock::new();

    /// Four known roster entries, in the order that a preview index reads them.
    fn preview_roster() -> Vec<UnknownPokemonState> {
        PREVIEW_ROSTER
            .get_or_init(|| {
                let dex = poke_rust::state::dex_data::parse_pokemon_dex(
                    "../pokemon_info/showdownDex.txt",
                );
                [
                    Species::Pikachu,
                    Species::Gengar,
                    Species::Garchomp,
                    Species::Snorlax,
                ]
                .into_iter()
                .map(|species| UnknownPokemonState::from_opponent_species(species, &dex, 50))
                .collect()
            })
            .clone()
    }

    fn row(probability: f64) -> TrackerStrategyRow {
        TrackerStrategyRow {
            commands: Vec::new(),
            preview: None,
            probability,
        }
    }

    fn checkpoint(generation: u64, depth: u8, p1_win_odds: f64) -> TrackerAnalysisCheckpoint {
        TrackerAnalysisCheckpoint {
            generation,
            turn_number: 3,
            position: PositionKind::Battle,
            depth_reached: depth,
            p1_win_odds,
            p2_win_odds: 1.0 - p1_win_odds,
            p1_strategy: vec![row(1.0)],
            p2_strategy: vec![row(1.0)],
            p2_strategy_is_playable: true,
            elapsed: Duration::from_millis(40),
            seed: 11,
            warnings: Vec::new(),
        }
    }

    fn profile() -> BotProfile {
        crate::bot::resolve("analysis", &crate::bot::BotProfileRequest::default()).unwrap()
    }

    /// Each rung must reach the client with its own depth, so the panel can
    /// update while the search goes deeper.
    #[test]
    fn every_ladder_rung_publishes_its_own_depth() {
        let mut state = TrackerAnalysisState::default();
        let job = state.start();

        for depth in 1..=3 {
            state.publish(&job, checkpoint(0, depth, 0.5));
            assert_eq!(state.checkpoint().unwrap().depth_reached, depth);
            // The ladder is still running, so the job stays open.
            assert_eq!(state.view(Some(&profile())).phase, "running");
        }

        state.finish(&job, Ok(()));
        assert_eq!(state.view(Some(&profile())).phase, "complete");
    }

    /// A rung that is not deeper than the stored rung must not replace it.
    /// Otherwise a late shallow answer would undo a deep one.
    #[test]
    fn a_shallower_rung_of_the_same_position_is_dropped() {
        let mut state = TrackerAnalysisState::default();
        let job = state.start();
        state.publish(&job, checkpoint(0, 3, 0.5));

        state.publish(&job, checkpoint(0, 2, 0.9));

        assert_eq!(state.checkpoint().unwrap().depth_reached, 3);
        assert_eq!(state.checkpoint().unwrap().p1_win_odds, 0.5);
    }

    /// A committed turn raises the generation, cancels the running job, and
    /// keeps the last answer as the comparison value.
    #[test]
    fn a_commit_raises_the_generation_and_cancels_the_running_job() {
        let mut state = TrackerAnalysisState::default();
        let job = state.start();
        state.publish(&job, checkpoint(0, 2, 0.62));
        assert!(!job.cancel.is_cancelled());

        state.invalidate(4);

        assert_eq!(state.generation, 1);
        assert!(job.cancel.is_cancelled());
        let view = state.view(Some(&profile()));
        assert_eq!(view.generation, 1);
        assert!(view.checkpoint.unwrap().stale);
        assert_eq!(view.previous_p1_win_odds, Some(0.62));
        // The cancelled job left no running record, and the stale rung stays
        // on screen until a newer one arrives.
        assert_eq!(view.phase, "complete");
    }

    /// A slow rung of an old position must never replace a newer answer.
    #[test]
    fn a_rung_of_an_old_generation_is_dropped() {
        let mut state = TrackerAnalysisState::default();
        let old = state.start();
        state.publish(&old, checkpoint(0, 1, 0.4));
        state.invalidate(4);
        let fresh = state.start();
        state.publish(&fresh, checkpoint(1, 1, 0.7));

        state.publish(&old, checkpoint(0, 8, 0.1));

        let stored = state.checkpoint().unwrap();
        assert_eq!(stored.generation, 1);
        assert_eq!(stored.p1_win_odds, 0.7);
    }

    /// A replaced job must not store a rung and must not end the new job.
    #[test]
    fn a_replaced_job_publishes_nothing() {
        let mut state = TrackerAnalysisState::default();
        let first = state.start();
        let second = state.start();
        assert!(first.cancel.is_cancelled());

        state.publish(&first, checkpoint(0, 1, 0.3));
        state.finish(&first, Err("stale".to_string()));

        assert!(state.checkpoint().is_none());
        assert_eq!(state.view(Some(&profile())).phase, "running");

        state.publish(&second, checkpoint(0, 1, 0.8));
        state.finish(&second, Ok(()));
        assert_eq!(state.checkpoint().unwrap().p1_win_odds, 0.8);
    }

    /// A failure keeps every rung that already published.
    #[test]
    fn a_failed_ladder_keeps_the_rungs_that_published() {
        let mut state = TrackerAnalysisState::default();
        let job = state.start();
        state.publish(&job, checkpoint(0, 1, 0.55));

        state.finish(&job, Err("The search panicked.".to_string()));

        let view = state.view(Some(&profile()));
        assert_eq!(view.phase, "failed");
        assert_eq!(view.error.as_deref(), Some("The search panicked."));
        assert_eq!(view.checkpoint.unwrap().depth_reached, 1);
    }

    /// A session with no profile reports `off`, so the panel shows its start
    /// control rather than an empty result.
    #[test]
    fn a_session_with_no_profile_reports_off() {
        let state = TrackerAnalysisState::default();
        let view = state.view(None);
        assert_eq!(view.phase, "off");
        assert!(view.profile.is_none());
        assert!(view.target_depth.is_none());
    }

    /// `DELETE` clears every stored answer, so the panel returns to its start
    /// state.
    #[test]
    fn reset_clears_the_record_and_cancels_the_job() {
        let mut state = TrackerAnalysisState::default();
        let job = state.start();
        state.publish(&job, checkpoint(0, 1, 0.5));
        state.invalidate(4);

        state.reset();

        assert!(job.cancel.is_cancelled());
        let view = state.view(None);
        assert!(view.checkpoint.is_none());
        assert!(view.previous_p1_win_odds.is_none());
        assert!(view.error.is_none());
    }

    /// A delta is valid only when the checkpoint is from the prior turn.
    #[test]
    fn a_delta_does_not_survive_a_skipped_or_rewritten_position() {
        let mut state = TrackerAnalysisState::default();
        let first = state.start();
        state.publish(&first, checkpoint(0, 2, 0.62));

        state.invalidate(4);
        assert_eq!(state.previous_p1_win_odds, Some(0.62));

        let second = state.start();
        state.invalidate(5);
        assert!(second.cancel.is_cancelled());
        assert_eq!(state.previous_p1_win_odds, None);

        let third = state.start();
        state.publish(&third, checkpoint(2, 2, 0.70));
        state.invalidate(3);
        assert_eq!(state.previous_p1_win_odds, None);
    }

    /// The tracker user typed both rosters, so the response carries both
    /// strategies and both win odds.
    #[test]
    fn the_response_carries_both_strategies_and_both_win_odds() {
        let mut state = TrackerAnalysisState::default();
        let job = state.start();
        state.publish(&job, checkpoint(0, 2, 0.62));

        let json = serde_json::to_string(&state.view(Some(&profile()))).unwrap();

        assert!(json.contains("\"p1WinOdds\":0.62"), "{json}");
        assert!(json.contains("\"p2WinOdds\":0.38"), "{json}");
        assert!(json.contains("\"p1Strategy\""), "{json}");
        assert!(json.contains("\"p2Strategy\""), "{json}");
        assert!(json.contains("\"p2StrategyIsPlayable\":true"), "{json}");
    }

    /// The tracker cannot search a battle before the first `leads` line, and the
    /// message has to tell the user which line to record.
    #[test]
    fn a_belief_with_no_leads_reports_a_clear_error() {
        for (p1_empty, p2_empty) in [(true, true), (true, false), (false, true)] {
            let message = leads_error(p1_empty, p2_empty).unwrap_err();
            assert!(message.contains("leads"), "{message}");
        }
        assert!(leads_error(false, false).is_ok());
    }

    /// Only the position before the first turn is the team preview. One empty
    /// side keeps the battle branch, and so does a mid-battle double faint.
    #[test]
    fn only_an_empty_field_on_turn_zero_is_the_team_preview() {
        assert!(position_is_team_preview(true, true, 0));
        assert!(!position_is_team_preview(true, false, 0));
        assert!(!position_is_team_preview(false, true, 0));
        assert!(!position_is_team_preview(false, false, 0));
        // A double faint empties both sides part way through a battle.
        assert!(!position_is_team_preview(true, true, 4));
    }

    /// A Player 1 roster that is larger than the bring makes the drawn world
    /// give Player 1 a bench that the real game does not hold, so the panel has
    /// to name that gap. A roster that matches the bring adds no warning.
    #[test]
    fn an_unknown_player_one_bring_adds_a_warning() {
        let (mut belief, _) = preview_beliefs();
        // `preview_beliefs` leaves the whole sheet on the bench, and the format
        // of that helper brings one Pokemon.
        assert!(belief.p1_known_back_mons.len() > 1);
        let line = unknown_bring_line(&belief).expect("the panel must name the unknown bring");
        assert!(line.contains("did not bring"), "{line}");

        belief.p1_active_mons = vec![belief.p1_known_back_mons.remove(0)];
        belief.p1_known_back_mons.clear();
        assert!(unknown_bring_line(&belief).is_none());
    }

    /// The panel shows one bring-and-lead choice for each row, so the row must
    /// name the lead species and the back species of that choice.
    #[test]
    fn a_preview_row_names_the_leads_and_the_back() {
        let roster = preview_roster();
        let strategy = vec![
            PreviewChoiceProb {
                choice: TeamPreviewCommand {
                    active_indices: vec![2, 0],
                    back_indices: vec![1, 3],
                },
                probability: 0.7,
            },
            PreviewChoiceProb {
                choice: TeamPreviewCommand {
                    active_indices: vec![0, 1],
                    back_indices: vec![2, 3],
                },
                probability: 0.3,
            },
        ];

        let rows = preview_rows(&roster, &strategy);

        assert_eq!(rows.len(), 2);
        let first = rows[0].preview.as_ref().expect("a preview row");
        // The lead order is the order of the choice, not the roster order.
        assert_eq!(first.leads, vec!["Garchomp", "Pikachu"]);
        assert_eq!(first.back, vec!["Gengar", "Snorlax"]);
        assert_eq!(rows[0].probability, 0.7);
        assert!(rows[0].commands.is_empty());

        let json = serde_json::to_string(&strategy_row_dto(&rows[0])).unwrap();
        assert!(
            json.contains("\"leads\":[\"Garchomp\",\"Pikachu\"]"),
            "{json}"
        );
    }

    /// A rate cut must keep the choices that the strategy plays most.
    #[test]
    fn the_preview_rows_keep_the_highest_rates() {
        let roster = preview_roster();
        let strategy: Vec<PreviewChoiceProb> = (0..20)
            .map(|index| PreviewChoiceProb {
                choice: TeamPreviewCommand {
                    active_indices: vec![index % 4],
                    back_indices: Vec::new(),
                },
                probability: f64::from(index as u32) / 100.0,
            })
            .collect();

        let rows = preview_rows(&roster, &strategy);

        assert_eq!(rows.len(), MAX_STRATEGY_ROWS);
        assert_eq!(rows[0].probability, 0.19);
    }

    /// The view must report the current depth and simulation count.
    #[test]
    fn the_view_reports_the_rung_that_runs() {
        let mut state = TrackerAnalysisState::default();
        assert!(state.view(Some(&profile())).rung.is_none());

        let job = state.start_with_budget(1_000);
        assert!(
            state.view(Some(&profile())).rung.is_none(),
            "no rung has started yet"
        );

        state.note_rung(&job, 2);
        let rung = state.view(Some(&profile())).rung.expect("a running rung");
        assert_eq!(rung.depth, 2);
        assert_eq!(rung.turns_simulated, 0);
        assert_eq!(rung.simulation_turn_budget, 1_000);
        assert_eq!(rung.fraction, 0.0);

        // A finished job reports no rung, so the panel drops the bar.
        state.finish(&job, Ok(()));
        assert!(state.view(Some(&profile())).rung.is_none());
    }

    /// The progress row reports the shared budget before work starts.
    #[test]
    fn a_running_rung_stays_below_one_hundred_percent() {
        let control = CancelFlag::with_simulation_turn_budget(1);
        let rung = rung_dto(RungProgress { depth: 2 }, &control);

        assert_eq!(rung.simulation_turn_budget, 1);
        assert_eq!(rung.fraction, 0.0);
    }

    /// A replaced job must not move the progress record of the job that owns it.
    #[test]
    fn a_replaced_job_notes_no_rung() {
        let mut state = TrackerAnalysisState::default();
        let first = state.start();
        let second = state.start();

        state.note_rung(&first, 7);
        assert!(state.view(Some(&profile())).rung.is_none());

        state.note_rung(&second, 1);
        assert_eq!(state.view(Some(&profile())).rung.unwrap().depth, 1);
    }

    /// A preview answer must say that both players play one strategy for every
    /// drawn world, and it must report the spread of the drawn values.
    #[test]
    fn a_preview_rung_reports_its_sampling_limits() {
        assert!(PREVIEW_MEAN_MATRIX_NOTE.contains("mean matrix"));
        assert_eq!(sampling_error_line(1, None), None);
        let line = sampling_error_line(8, Some(0.021)).unwrap();
        assert!(line.contains("8 drawn world(s)"), "{line}");
        assert!(line.contains("2.1 points"), "{line}");
    }

    /// A one-world rung has no spread to report, so the world note is the only
    /// warning that the answer rests on a guess of the opponent's hidden data.
    #[test]
    fn a_preview_rung_names_the_number_of_drawn_worlds() {
        let one = preview_worlds_note(1);
        assert!(one.contains("one world"), "{one}");
        assert!(one.contains("one guess"), "{one}");

        let many = preview_worlds_note(8);
        assert!(many.contains("8 worlds"), "{many}");
        assert!(many.contains("8 guesses"), "{many}");
    }

    /// The preview search solves each cell with the exact search, so a sampling
    /// profile must still supply a complete battle configuration.
    #[test]
    fn a_sampling_profile_builds_a_preview_battle_configuration() {
        for name in ["doubleOracle", "mcts", "ismcts", "mccfr"] {
            let request = crate::bot::BotProfileRequest {
                algorithm: Some(name.to_string()),
                depth: Some(1),
                damage_rolls: Some(16),
                consider_crit: Some(true),
                ..Default::default()
            };
            let resolved = crate::bot::resolve("analysis", &request).unwrap();

            let config = preview_battle_config(resolved.search);

            assert_eq!(config.depth, 1, "{name}");
            assert_eq!(config.damage_rolls, 16, "{name}");
            assert!(config.consider_crit, "{name}");
            assert_eq!(config.deadline, None, "{name}");
        }
    }

    /// A belief profile draws several worlds, and the world count must stay
    /// under the cap. Every other profile reads one world.
    #[test]
    fn the_world_count_stays_under_the_cap() {
        for (name, particles, want) in [
            ("doubleOracle", None, 1),
            ("mcts", None, 1),
            ("ismcts", Some(8), 8),
            ("mccfr", Some(MAX_PREVIEW_WORLDS + 4), MAX_PREVIEW_WORLDS),
        ] {
            let request = crate::bot::BotProfileRequest {
                algorithm: Some(name.to_string()),
                particles,
                ..Default::default()
            };
            let resolved = crate::bot::resolve("analysis", &request).unwrap();
            assert_eq!(preview_worlds(resolved.search), want, "{name}");
        }
    }

    // ── The ladder at a real preview position ────────────────────────────────

    static SERVER_DEXES: OnceLock<Arc<Dexes>> = OnceLock::new();
    static SERVER_META: OnceLock<Option<Arc<MetaDexes>>> = OnceLock::new();

    fn server_dexes() -> Arc<Dexes> {
        Arc::clone(SERVER_DEXES.get_or_init(|| {
            Arc::new(Dexes {
                pokemon_dex: poke_rust::state::dex_data::parse_pokemon_dex(
                    "../pokemon_info/showdownDex.txt",
                ),
                move_dex: poke_rust::state::dex_data::parse_move_dex(
                    "../pokemon_info/showdownMoves.txt",
                ),
                ability_dex: poke_rust::state::dex_data::parse_ability_dex(
                    "../pokemon_info/showdownAbilities.txt",
                ),
                learnset_dex: poke_rust::state::dex_data::parse_learnset_dex(
                    "../pokemon_info/showdownLearnsets.txt",
                ),
            })
        }))
    }

    /// The singles usage cache of the repository, or `None` when it is absent.
    fn server_meta() -> Option<Arc<MetaDexes>> {
        SERVER_META
            .get_or_init(|| {
                let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("../meta_scraper/data");
                if !root.is_dir() {
                    return None;
                }
                let singles =
                    poke_rust::meta::MetaDex::load(&root, None, MetaFormat::Singles).ok()?;
                Some(Arc::new(MetaDexes {
                    singles: Some(singles),
                    doubles: None,
                }))
            })
            .clone()
    }

    const P1_TEAM: &str = "\
Pikachu @ Light Ball
Ability: Static
Level: 50
- Thunderbolt

Snorlax @ Leftovers
Ability: Thick Fat
Level: 50
- Body Slam
";

    const P2_TEAM: &str = "\
Garchomp @ Life Orb
Ability: Rough Skin
Level: 50
- Earthquake

Gengar @ Focus Sash
Ability: Cursed Body
Level: 50
- Shadow Ball
";

    /// The two beliefs of a fresh tracker session, built the way
    /// `create_tracker` builds them.
    fn preview_beliefs() -> (UnknownBattleState, UnknownTeamPreviewState) {
        use poke_rust::information::unknowns::{InformationMode, UnknownMatchState};

        let dexes = server_dexes();
        let preview = poke_rust::simulator::team_preview_state_from_team_strings(
            P1_TEAM,
            P2_TEAM,
            &dexes.pokemon_dex,
            &dexes.move_dex,
            1,
            1,
            true,
        );
        let mut belief = UnknownMatchState::team_preview_open_sheet_from_perspective(
            Player::P1,
            &preview.p1_mons,
            &preview.p2_mons,
            &dexes.pokemon_dex,
            1,
            1,
            50,
            InformationMode::OpenTeamSheet,
            false,
        );
        let UnknownMatchState::TeamPreview(preview_belief) = &mut belief else {
            panic!("the constructor returned the wrong variant");
        };
        let all_p1: Vec<usize> = (0..preview.p1_mons.len()).collect();
        let mut battle = preview_belief.into_battle_state(Player::P1, &[], &all_p1, &[], &[]);
        battle.back_mons_per_side = 0;
        (battle, preview_belief.clone())
    }

    /// The ladder inputs of a fresh tracker session, or `None` with no cache.
    fn preview_inputs(algorithm: &str, simulation_turn_budget: u64) -> Option<LadderInputs> {
        let meta = server_meta()?;
        let dexes = server_dexes();
        let request = crate::bot::BotProfileRequest {
            algorithm: Some(algorithm.to_string()),
            simulation_turn_budget: Some(simulation_turn_budget),
            depth: Some(1),
            damage_rolls: Some(4),
            consider_crit: Some(false),
            ..Default::default()
        };
        let resolved = crate::bot::resolve("analysis", &request).unwrap();
        let (battle, preview) = preview_beliefs();
        Some(LadderInputs {
            search: resolved.search,
            seed: 7,
            target_depth: resolved.view.depth,
            generation: 0,
            turn_number: battle.turn_number,
            belief: battle,
            preview_belief: Some(preview),
            inference: InferenceConfig {
                learnset_dex: dexes.learnset_dex.clone(),
                ..InferenceConfig::default()
            },
            dexes,
            meta,
            format: MetaFormat::Singles,
        })
    }

    /// Runs one ladder and reports every rung it published.
    fn run_and_collect(
        inputs: &LadderInputs,
        cancel: &CancelFlag,
    ) -> (Result<(), String>, Vec<TrackerAnalysisCheckpoint>, Vec<u8>) {
        let published = Mutex::new(Vec::new());
        let noted = Mutex::new(Vec::new());
        let outcome = {
            let publish = |checkpoint: TrackerAnalysisCheckpoint| {
                published.lock().unwrap().push(checkpoint);
            };
            let note_rung = |depth: u8| noted.lock().unwrap().push(depth);
            let hooks = LadderHooks {
                publish: &publish,
                note_rung: &note_rung,
            };
            run_ladder(inputs, cancel, &hooks)
        };
        (
            outcome,
            published.into_inner().unwrap(),
            noted.into_inner().unwrap(),
        )
    }

    /// Before the first `leads` line the ladder must answer the bring-and-lead
    /// question rather than report that the field is empty.
    #[test]
    fn the_ladder_answers_the_team_preview_before_the_first_leads_line() {
        let Some(inputs) = preview_inputs("doubleOracle", 8_000) else {
            return;
        };
        let cancel = CancelFlag::new();

        let (outcome, rungs, noted) = run_and_collect(&inputs, &cancel);

        outcome.expect("the preview position is searchable");
        assert_eq!(rungs.len(), 1, "the preview answers with one rung");
        assert_eq!(rungs[0].position, PositionKind::TeamPreview);
        assert_eq!(rungs[0].turn_number, 0);
        assert_eq!(noted, vec![inputs.target_depth]);
        for row in rungs[0].p1_strategy.iter().chain(&rungs[0].p2_strategy) {
            let choice = row.preview.as_ref().expect("a preview row");
            assert_eq!(choice.leads.len(), 1, "{choice:?}");
            assert!(choice.back.is_empty(), "{choice:?}");
        }
        // Each row names its own side's roster, so a swap would show the other
        // side's species.
        let leads = |rows: &[TrackerStrategyRow]| -> Vec<String> {
            rows.iter()
                .map(|row| row.preview.as_ref().unwrap().leads[0].clone())
                .collect()
        };
        for name in leads(&rungs[0].p1_strategy) {
            assert!(["Pikachu", "Snorlax"].contains(&name.as_str()), "{name}");
        }
        for name in leads(&rungs[0].p2_strategy) {
            assert!(["Garchomp", "Gengar"].contains(&name.as_str()), "{name}");
        }
    }

    /// Every rung must say that the opponent's hidden data came from a draw.
    /// An exact profile draws one world and has no spread to report, so without
    /// this line its answer names no guess at all.
    #[test]
    fn a_preview_rung_discloses_its_drawn_worlds() {
        for (algorithm, worlds) in [("doubleOracle", 1usize), ("ismcts", 8)] {
            let Some(inputs) = preview_inputs(algorithm, 8_000) else {
                return;
            };
            let cancel = CancelFlag::new();

            let (outcome, rungs, _) = run_and_collect(&inputs, &cancel);

            outcome.expect("the preview position is searchable");
            let warnings = &rungs[0].warnings;
            assert!(
                warnings.contains(&preview_worlds_note(worlds)),
                "{algorithm} published {warnings:#?}"
            );
            assert!(
                warnings.iter().any(|line| line.contains("hidden data")),
                "{algorithm} published {warnings:#?}"
            );
        }
    }

    /// A committed turn cancels the running preview search, and a cancelled run
    /// publishes nothing.
    #[test]
    fn a_cancelled_preview_ladder_publishes_no_rung() {
        let Some(inputs) = preview_inputs("doubleOracle", 8_000) else {
            return;
        };
        let cancel = CancelFlag::new();
        cancel.cancel();

        let (outcome, rungs, _) = run_and_collect(&inputs, &cancel);

        assert!(outcome.is_ok(), "{outcome:?}");
        assert!(rungs.is_empty(), "a cancelled ladder published a rung");
    }

    /// An engine error names a drawn species, which is a guess about the live
    /// opponent rather than a recorded fact.
    #[test]
    fn an_engine_error_reaches_the_client_without_its_detail() {
        use poke_rust::information::determinize::DeterminizeError;

        let error = DeterminizeError::UnknownSpecies {
            mon_idx: 1,
            species: poke_rust::data::species::Species::Zoroark,
        };
        assert!(error.to_string().contains("Zoroark"));

        let message = engine_error(error);

        assert!(!message.contains("Zoroark"), "{message}");
        assert!(message.contains("server console"), "{message}");
    }

    /// A panic payload is engine text too.
    #[test]
    fn a_panic_reaches_the_client_without_its_payload() {
        let payload = std::panic::catch_unwind(|| panic!("Zoroark: contradiction at mon 1"))
            .expect_err("the closure panics");

        let message = panic_message(payload);

        assert!(!message.contains("Zoroark"), "{message}");
        assert!(message.contains("server console"), "{message}");
    }

    /// The rows keep the highest rates, so a cut never removes the action that
    /// the strategy plays most.
    #[test]
    fn the_rows_keep_the_highest_rates() {
        let strategy: Vec<JointActionProb> = (0..20u32)
            .map(|index| JointActionProb {
                commands: vec![BattleCommand::Switch(SwitchCommand {
                    party_index: index as usize,
                })],
                probability: f64::from(index) / 100.0,
            })
            .collect();

        let rows = top_actions(&strategy);

        assert_eq!(rows.len(), MAX_STRATEGY_ROWS);
        assert_eq!(rows[0].probability, 0.19);
        for pair in rows.windows(2) {
            assert!(pair[0].probability >= pair[1].probability);
        }
        // A short strategy keeps every action.
        assert_eq!(top_actions(&strategy[..3]).len(), 3);
    }

    /// A limited exact search must report the depth that it completed.
    #[test]
    fn an_exact_rung_keeps_the_solver_depth() {
        let rung = exact_rung(solver::SolveResult {
            value: 0.5,
            p1_win_odds: 0.5,
            p2_win_odds: 0.5,
            p1_strategy: Vec::new(),
            p2_strategy: Vec::new(),
            depth_reached: 1,
            stats: Default::default(),
            warnings: vec![SolveWarning::DepthNotReached {
                target: 3,
                reached: 1,
            }],
        });

        assert_eq!(rung.depth_reached, 1);
        assert!(rung.warnings[0].contains("depth 1 of the 3"));
    }

    /// A repeated warning appears one time, and a distinct one survives.
    #[test]
    fn the_warning_list_keeps_each_distinct_line() {
        let lines = warning_lines(&[
            SolveWarning::DepthNotReached {
                target: 3,
                reached: 1,
            },
            SolveWarning::DepthNotReached {
                target: 3,
                reached: 1,
            },
            SolveWarning::ChanceMassDiscarded { max_fraction: 0.02 },
        ]);

        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(lines[0].contains("depth 1 of the 3"), "{lines:?}");
    }

    /// The panel must say which position the answer describes, so a search that
    /// read one drawn world says so.
    #[test]
    fn a_drawn_world_search_says_that_it_read_one_world() {
        let exact = drawn_world_note(BotSearchConfig::Exact(Default::default()));
        assert!(exact.contains("one drawn world"), "{exact}");
        assert!(exact.contains("ismcts"), "{exact}");

        let belief = drawn_world_note(BotSearchConfig::Ismcts(Default::default()));
        assert!(belief.contains("searched the belief"), "{belief}");
        assert!(belief.contains("not one playable strategy"), "{belief}");
        assert!(p2_strategy_is_playable(BotSearchConfig::Mcts(
            Default::default()
        )));
        assert!(!p2_strategy_is_playable(BotSearchConfig::Mccfr(
            Default::default()
        )));
    }
}
