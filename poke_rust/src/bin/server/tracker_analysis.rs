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
//! with no time left does not start.
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
//! # The time limit
//!
//! [`SolveConfig::deadline`](poke_rust::solver::SolveConfig::deadline) stops an
//! exact search before it starts another turn simulation. A simulation that
//! already runs still finishes. One full damage-roll enumeration of a multi-hit
//! move branches `(rolls * 2)` ways for each hit, so a five-hit move at 16 rolls
//! reaches about 33 million outcomes. That one cell can run for minutes.
//!
//! A measured example: a drawn Garchomp with Scale Shot made a depth-one
//! `doubleOracle` rung run past 95 seconds under a 2000 ms limit. The same
//! position answered in about 100 ms under `ismcts` and `mcts`, because a
//! sampling search draws one outcome instead of every outcome.
//!
//! [`overrun_line`] reports the overrun on the rung that publishes. The panel
//! selects a sampling algorithm by default for the same reason.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use poke_rust::information::determinize::{DeterminizeConfig, determinize_seeded};
use poke_rust::information::inference::InferenceConfig;
use poke_rust::information::unknowns::UnknownBattleState;
use poke_rust::meta::{MetaDex, MetaFormat};
use poke_rust::solver::{self, CancelFlag, JointActionProb};
use poke_rust::state::battle::{BattleState, MatchState, Player};

use crate::bot::{BotProfile, BotSearchConfig, MAX_SAFE_INTEGER};
use crate::dto::{TrackerAnalysisCheckpointDto, TrackerAnalysisDto, TrackerStrategyRowDto};
use crate::mapping;
use crate::session::{Dexes, MetaDexes};
use crate::tracker::TrackerSession;

/// How many joint actions of one player the checkpoint carries.
///
/// A doubles position can hold hundreds of joint actions, and the panel shows a
/// short list. The rows keep the highest rates, so the cut removes only actions
/// that the strategy rarely plays.
const MAX_STRATEGY_ROWS: usize = 8;

/// One joint action of a strategy, rendered against the drawn world.
#[derive(Debug, Clone)]
pub struct TrackerStrategyRow {
    commands: Vec<crate::dto::CommandOptionDto>,
    probability: f64,
}

/// The complete answer of one ladder rung.
#[derive(Debug, Clone)]
pub struct TrackerAnalysisCheckpoint {
    /// The generation of the position that the search read.
    pub generation: u64,
    pub turn_number: u16,
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

/// The job that is running now.
#[derive(Debug)]
struct RunningJob {
    id: u64,
    generation: u64,
    started: Instant,
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
    fn start(&mut self) -> JobTicket {
        if let Some(job) = self.running.take() {
            job.cancel.cancel();
        }
        let id = self.next_job_id;
        self.next_job_id = self.next_job_id.wrapping_add(1);
        let cancel = CancelFlag::new();
        self.running = Some(RunningJob {
            id,
            generation: self.generation,
            started: Instant::now(),
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

/// Builds the wire row of one checkpoint.
fn checkpoint_dto(
    checkpoint: &TrackerAnalysisCheckpoint,
    generation: u64,
) -> TrackerAnalysisCheckpointDto {
    TrackerAnalysisCheckpointDto {
        generation: checkpoint.generation,
        stale: checkpoint.generation != generation,
        turn_number: checkpoint.turn_number,
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

fn strategy_row_dto(row: &TrackerStrategyRow) -> TrackerStrategyRowDto {
    TrackerStrategyRowDto {
        commands: row.commands.clone(),
        probability: row.probability,
    }
}

/// Everything one ladder needs after the caller drops the session lock.
struct LadderInputs {
    search: BotSearchConfig,
    seed: u64,
    /// The wall-clock limit of the whole ladder.
    time_ms: u64,
    /// The depth horizon. The ladder runs one rung for each depth up to it.
    target_depth: u8,
    generation: u64,
    turn_number: u16,
    belief: UnknownBattleState,
    inference: InferenceConfig,
    dexes: Arc<Dexes>,
    meta: Arc<MetaDexes>,
    format: MetaFormat,
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
        time_ms: profile.view.time_ms.unwrap_or(DEFAULT_TIME_MS),
        target_depth: profile.view.depth,
        generation: session.analysis.generation,
        turn_number: session.belief.turn_number,
        belief: session.belief.clone(),
        inference: crate::analysis::clone_inference_config(&session.inference_config),
        dexes,
        meta,
        format: MetaFormat::from_active_per_side(session.active_per_side),
    };

    let job = session.analysis.start();
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
            run_ladder(&inputs, &job.cancel, &publish)
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

/// The time limit of a profile that carries none.
///
/// `bot::resolve` always sets one, so this value covers only a hand-built
/// profile.
const DEFAULT_TIME_MS: u64 = 10_000;

/// Draws the seed of one job.
///
/// The checkpoint publishes this number, and the client reads it as a JSON
/// number, so it must survive a round trip through a JavaScript double.
fn random_seed() -> u64 {
    rand::random::<u64>() & MAX_SAFE_INTEGER
}

/// Runs the depth ladder and publishes each complete rung.
///
/// Returns an error only when no rung can run at all, or when a rung fails.
/// Every rung that already published stays in the record.
fn run_ladder(
    inputs: &LadderInputs,
    cancel: &CancelFlag,
    publish: &dyn Fn(TrackerAnalysisCheckpoint),
) -> Result<(), String> {
    let started = Instant::now();
    let mut published_rung = false;
    leads_are_on_the_field(&inputs.belief)?;
    let meta = inputs
        .meta
        .for_format(inputs.format)
        .ok_or_else(no_usage_cache)?;

    let determinize = DeterminizeConfig {
        inference: crate::analysis::clone_inference_config(&inputs.inference),
        // The tracker user is Player 1, so the draw copies their own side and
        // samples the live opponent.
        observer: Player::P1,
        ..DeterminizeConfig::default()
    };
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

    for depth in 1..=inputs.target_depth {
        if cancel.is_cancelled() {
            return Ok(());
        }
        let Some(remaining) = remaining_time(inputs.time_ms, started.elapsed()) else {
            return no_time_left(published_rung);
        };
        let search = inputs.search.with_depth(depth).with_deadline(remaining);

        // A solver panic must not poison the session mutex, so catch it here
        // and report it as an ordinary job failure.
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            one_search(search, inputs, meta, &state, cancel)
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
        if let Some(line) = overrun_line(inputs.time_ms, elapsed) {
            warnings.push(line);
        }
        warnings.push(drawn_world_note(inputs.search));

        publish(TrackerAnalysisCheckpoint {
            generation: inputs.generation,
            turn_number: inputs.turn_number,
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
        published_rung = true;
        if rung.depth_reached < depth {
            return Ok(());
        }
    }
    Ok(())
}

/// Stops a ladder after it spends its time limit.
fn no_time_left(published_rung: bool) -> Result<(), String> {
    if published_rung {
        Ok(())
    } else {
        Err("The time limit expired before the first depth could start.".to_string())
    }
}

/// The time that the ladder has left.
///
/// Returns `None` when the limit is already spent, which stops the ladder.
fn remaining_time(time_ms: u64, spent: Duration) -> Option<Duration> {
    Duration::from_millis(time_ms)
        .checked_sub(spent)
        .filter(|left| !left.is_zero())
}

/// Reports a rung that ran past the limit of the profile.
///
/// [`SolveConfig::deadline`](poke_rust::solver::SolveConfig::deadline) stops the
/// solver before it starts another turn simulation. A simulation that already
/// runs still finishes, and one full damage-roll enumeration of a multi-hit move
/// can take much longer than the limit. The panel shows this line so the user
/// knows why the answer was late.
fn overrun_line(time_ms: u64, elapsed: Duration) -> Option<String> {
    (elapsed > Duration::from_millis(time_ms)).then(|| {
        format!(
            "The search took {} ms, which is past the {time_ms} ms limit. One turn simulation \
             that already runs cannot stop.",
            elapsed.as_millis()
        )
    })
}

/// The result of one rung, before the job renders it.
struct RungResult {
    depth_reached: u8,
    p1_win_odds: f64,
    p2_win_odds: f64,
    p1_strategy: Vec<JointActionProb>,
    p2_strategy: Vec<JointActionProb>,
    warnings: Vec<String>,
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
    }
}

/// Routes one rung to its solver entry point.
///
/// Every arm passes `cancel`, so a raised flag stops the search that runs.
fn one_search(
    search: BotSearchConfig,
    inputs: &LadderInputs,
    meta: &MetaDex,
    state: &MatchState,
    cancel: &CancelFlag,
) -> Result<RungResult, String> {
    let pokemon_dex = &inputs.dexes.pokemon_dex;
    let move_dex = &inputs.dexes.move_dex;
    let cancel = Some(cancel);
    match search {
        BotSearchConfig::Exact(config) => {
            let result = solver::solve_seeded_cancellable(
                inputs.seed,
                state,
                pokemon_dex,
                move_dex,
                &config,
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
            })
        }
        BotSearchConfig::Ismcts(config) => {
            let determinize = belief_draw_config(inputs);
            let result = solver::ismcts::search_belief_cancellable(
                inputs.seed,
                &inputs.belief,
                meta,
                pokemon_dex,
                move_dex,
                &config,
                &determinize,
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
            })
        }
        BotSearchConfig::Mccfr(config) => {
            let determinize = belief_draw_config(inputs);
            let result = solver::mccfr::search_belief_cancellable(
                inputs.seed,
                &inputs.belief,
                meta,
                pokemon_dex,
                move_dex,
                &config,
                &determinize,
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
            })
        }
    }
}

/// The draw rules of a belief search.
fn belief_draw_config(inputs: &LadderInputs) -> DeterminizeConfig {
    DeterminizeConfig {
        inference: crate::analysis::clone_inference_config(&inputs.inference),
        observer: Player::P1,
        ..DeterminizeConfig::default()
    }
}

/// Reports that the tracker has no position to search yet.
///
/// The first `leads` line puts both sides on the field. Before it, one side or
/// both sides have no active Pokemon, and no search can run.
fn leads_are_on_the_field(belief: &UnknownBattleState) -> Result<(), String> {
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

fn no_usage_cache() -> String {
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
fn drawn_world_note(search: BotSearchConfig) -> String {
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

/// True when the P2 rows describe one private state.
fn p2_strategy_is_playable(search: BotSearchConfig) -> bool {
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
fn warning_lines(warnings: &[solver::SolveWarning]) -> Vec<String> {
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
fn strategy_rows(
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
            probability: action.probability,
        })
        .collect()
}

/// Converts an engine error to a job message.
///
/// The engine writes its own text, and that text names a drawn species and a
/// belief mon index. That guess is not something the tracker user recorded, so
/// the detail goes to the server console and the client reads a fixed line.
fn engine_error(error: impl std::fmt::Display) -> String {
    eprintln!("tracker analysis job: the search failed: {error}");
    "The search failed. The server console holds the reason.".to_string()
}

/// Converts a caught panic payload to a job error message.
fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
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
    use poke_rust::solver::SolveWarning;
    use poke_rust::state::battle::{BattleCommand, SwitchCommand};

    fn row(probability: f64) -> TrackerStrategyRow {
        TrackerStrategyRow {
            commands: Vec::new(),
            probability,
        }
    }

    fn checkpoint(generation: u64, depth: u8, p1_win_odds: f64) -> TrackerAnalysisCheckpoint {
        TrackerAnalysisCheckpoint {
            generation,
            turn_number: 3,
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
        crate::bot::resolve(
            "analysis",
            &crate::bot::BotProfileRequest::default(),
            16,
            true,
        )
        .unwrap()
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

    /// The tracker cannot search before the first `leads` line, and the message
    /// has to tell the user which line to record.
    #[test]
    fn a_belief_with_no_leads_reports_a_clear_error() {
        for (p1_empty, p2_empty) in [(true, true), (true, false), (false, true)] {
            let message = leads_error(p1_empty, p2_empty).unwrap_err();
            assert!(message.contains("leads"), "{message}");
        }
        assert!(leads_error(false, false).is_ok());
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

    /// The ladder must stop rather than start a rung that has no time to run.
    #[test]
    fn a_spent_time_limit_stops_the_ladder() {
        assert!(remaining_time(1_000, Duration::from_millis(200)).is_some());
        assert_eq!(
            remaining_time(1_000, Duration::from_millis(200)),
            Some(Duration::from_millis(800))
        );
        assert!(remaining_time(1_000, Duration::from_millis(1_000)).is_none());
        assert!(remaining_time(1_000, Duration::from_millis(4_000)).is_none());

        assert!(no_time_left(true).is_ok());
        let message = no_time_left(false).unwrap_err();
        assert!(message.contains("before the first depth"), "{message}");
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

    /// A turn simulation that already runs cannot stop, so a rung can finish
    /// past its limit. The panel has to say so.
    #[test]
    fn a_rung_past_the_limit_reports_the_overrun() {
        assert_eq!(overrun_line(1_000, Duration::from_millis(900)), None);
        assert_eq!(overrun_line(1_000, Duration::from_millis(1_000)), None);

        let line = overrun_line(1_000, Duration::from_millis(4_500)).unwrap();
        assert!(line.contains("4500 ms"), "{line}");
        assert!(line.contains("1000 ms limit"), "{line}");
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
