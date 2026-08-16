//! The private P2 analysis job of a battle session.
//!
//! A session with a resolved P2 profile starts one search after each resolved
//! turn. The search runs on a blocking thread, so it never holds the session
//! lock. Each job carries a generation and a job ID.
//!
//! [`AnalysisState::invalidate`] raises the generation and cancels the running
//! job. [`AnalysisState::accept`] drops a result when its generation or job ID
//! is not current. Thus, a slow job cannot overwrite a newer answer. Both calls
//! keep the last complete checkpoint after a failure or a cancellation.
//!
//! Every search reads the cancel flag during the search, so a raised flag stops
//! the work rather than only the start of it. A cancelled search still returns
//! an answer, and that answer covers only the work that finished. The task
//! therefore drops it: a partial answer must not replace the last complete
//! checkpoint.
//!
//! P1 reads `GET /api/battles/{id}/analysis` during a battle, so the progress
//! view holds no P2 action, no P2 win odds, no count that divides out P2's
//! action-set size, and no engine-written text.
//!
//! [`draw_p2_command`] draws the command that P2 plays. The turn response
//! reveals that one command after the turn resolves, so both commands are
//! already locked. See [`P2Draw`] for what the reveal carries.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

use poke_rust::information::determinize::DeterminizeConfig;
use poke_rust::information::inference::InferenceConfig;
use poke_rust::information::unknowns::UnknownMatchState;
use poke_rust::meta::{MetaDex, MetaFormat};
use poke_rust::solver::{self, CancelFlag, JointActionProb};
use poke_rust::state::battle::{BattleCommand, MatchState, Player, PlayerCommand};

use crate::bot::{BotSearchConfig, MAX_SAFE_INTEGER};
use crate::dto::{
    AnalysisCheckpointDto, AnalysisProgressDto, AnalysisReplayDto, P2RevealDto, PlayerCommandDto,
};
use crate::mapping;
use crate::session::{self, BattleSession, Dexes, MetaDexes};

/// The completed answer of one job.
///
/// The four solver result types share these fields, so one checkpoint covers
/// every algorithm.
#[derive(Debug, Clone)]
pub struct AnalysisCheckpoint {
    /// The generation of the position that the search read.
    pub generation: u64,
    pub turn_number: u16,
    /// P2's odds of winning, in `[0, 1]`.
    ///
    /// No endpoint returns this. The answer is private to P2, and a hotseat
    /// battle lets P1 read every endpoint.
    #[allow(dead_code)]
    pub p2_win_odds: f64,
    /// P2's mixed strategy at the root.
    ///
    /// No endpoint returns this list. [`draw_p2_command`] draws one action from
    /// it, and the turn response reveals that one action alone.
    pub p2_strategy: Vec<JointActionProb>,
    /// The depth that the search reached.
    /// A sampling search reports its configured depth.
    pub depth_reached: u8,
    /// Completed `simulate_turn` calls.
    ///
    /// No endpoint returns this. A depth-one exact search spends one call per
    /// matrix cell, so the figure is about `|A1| * |A2|`, and P1 already holds
    /// `|A1|`. That divides out P2's joint-action count, which is the same
    /// figure [`warning_line`] scrubs out of
    /// [`solver::SolveWarning::ActionsTruncated`]. The later reveal item reads
    /// it.
    #[allow(dead_code)]
    pub turns_simulated: u64,
    /// Decision nodes for an exact search, tree nodes for a sampling search.
    /// Private for the same reason as `turns_simulated`.
    #[allow(dead_code)]
    pub nodes: u64,
    pub elapsed: Duration,
    /// The seed of this search, which makes the result reproducible.
    pub seed: u64,
    /// The data that repeats this search.
    pub replay: AnalysisReplay,
    /// True when the strategy respects the session's fog of war.
    pub strategy_is_playable: bool,
    /// Every reason that the answer is approximate.
    pub warnings: Vec<String>,
}

/// The data that repeats one analysis search.
///
/// Every field is a position identifier, a seed, or a public profile setting.
/// An operator with the session history can run the same search again.
#[derive(Debug, Clone, Default)]
pub struct AnalysisReplay {
    /// The generation and turn identify the position in the session history.
    pub generation: u64,
    pub turn_number: u16,
    /// The seed of the search.
    pub search_seed: u64,
    pub algorithm: String,
    pub preset: String,
    pub time_ms: Option<u64>,
    pub node_budget: Option<u64>,
    pub depth: u8,
    pub workers: u8,
    pub iterations: Option<u32>,
    pub particles: Option<usize>,
    pub max_actions_per_player: Option<usize>,
    pub damage_rolls: u8,
    pub consider_crit: bool,
}

/// The job that is running now.
#[derive(Debug)]
struct RunningJob {
    id: u64,
    generation: u64,
    started: Instant,
    /// Stops the job before its search starts, and during the search.
    ///
    /// Every search reads this flag at its own check points. The ticket check in
    /// [`AnalysisState::accept`] covers the final race.
    cancel: CancelFlag,
}

/// Identifies one analysis job and carries its cancel flag.
#[derive(Debug)]
struct JobTicket {
    id: u64,
    generation: u64,
    cancel: CancelFlag,
}

/// The analysis record of one session.
#[derive(Debug, Default)]
pub struct AnalysisState {
    /// The generation of the current position.
    /// Every state change raises it by one.
    generation: u64,
    /// The ID for the next job in this session.
    next_job_id: u64,
    running: Option<RunningJob>,
    /// The newest complete answer.
    /// A failure and a cancellation both keep it.
    checkpoint: Option<AnalysisCheckpoint>,
    /// Why the last job produced no checkpoint.
    last_error: Option<String>,
}

impl AnalysisState {
    /// Raises the generation and cancels the running job.
    ///
    /// Keeps the checkpoint. The client reads the last complete answer until a
    /// newer job finishes, and the `stale` flag of the view marks it.
    pub fn invalidate(&mut self) {
        self.generation += 1;
        if let Some(job) = self.running.take() {
            job.cancel.cancel();
        }
    }

    /// Records the job that is starting and returns its ticket.
    /// Cancels an earlier job of the same generation, so one job runs at a time.
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

    /// Stores the result of one job.
    ///
    /// Drops a result from an old or replaced job. A failure keeps the last
    /// complete checkpoint.
    ///
    /// Returns the console line of the exit that ran. The caller writes that
    /// line after it releases the session lock. A job that leaves no checkpoint
    /// for the current generation makes the client report "no answer", and the
    /// four lines name which exit ran.
    fn accept(&mut self, job: &JobTicket, outcome: Result<AnalysisCheckpoint, String>) -> String {
        if job.generation != self.generation {
            return format!(
                "analysis job {}: the result was dropped, \
                 because the position moved from generation {} to generation {}",
                job.id, job.generation, self.generation
            );
        }
        if !self
            .running
            .as_ref()
            .is_some_and(|running| running.id == job.id && running.generation == job.generation)
        {
            return format!(
                "analysis job {} (generation {}): the result was dropped, \
                 because another job replaced it",
                job.id, job.generation
            );
        }
        self.running = None;
        match outcome {
            Ok(checkpoint) => {
                let line = complete_line(job, &checkpoint);
                self.checkpoint = Some(checkpoint);
                self.last_error = None;
                line
            }
            Err(message) => {
                let line = format!(
                    "analysis job {} (generation {}): no checkpoint: {message}",
                    job.id, job.generation
                );
                self.last_error = Some(message);
                line
            }
        }
    }

    /// The private progress of this session.
    ///
    /// The response holds no P2 action, no P2 strategy, and no P2 win odds. Only
    /// the cost of the search and the generation of each answer appear.
    pub fn progress(&self) -> AnalysisProgressDto {
        let phase = if self.running.is_some() {
            "running"
        } else if self.last_error.is_some() {
            "failed"
        } else if self.checkpoint.is_some() {
            "complete"
        } else {
            "idle"
        };
        AnalysisProgressDto {
            generation: self.generation,
            phase: phase.to_string(),
            running_ms: self
                .running
                .as_ref()
                .map(|job| job.started.elapsed().as_millis() as u64),
            checkpoint: self
                .checkpoint
                .as_ref()
                .map(|c| checkpoint_dto(c, self.generation)),
            error: self.last_error.clone(),
        }
    }

    /// The generation of the current position.
    fn generation(&self) -> u64 {
        self.generation
    }

    /// The checkpoint of the current position.
    ///
    /// Returns `None` while the newest complete answer belongs to an older
    /// position, because a stale strategy names actions that the current
    /// position can no longer play.
    fn current_checkpoint(&self) -> Option<&AnalysisCheckpoint> {
        self.checkpoint
            .as_ref()
            .filter(|c| c.generation == self.generation)
    }

    /// The last complete checkpoint, stale or not.
    #[cfg(test)]
    fn checkpoint(&self) -> Option<&AnalysisCheckpoint> {
        self.checkpoint.as_ref()
    }
}

/// The console line of one complete job.
///
/// The line names the cost of the search. It never names the number of rows in
/// `p2_strategy`. That number is P2's joint-action count, which
/// [`AnalysisCheckpoint::turns_simulated`] keeps off every response and which
/// [`warning_line`] scrubs out of `ActionsTruncated`. P1 runs the server in a
/// hotseat battle, so the console must hold no more of P2's plan than the
/// response does.
///
/// An empty strategy explains the "no action with a positive weight" draw line,
/// so the line reports that one fact alone.
fn complete_line(job: &JobTicket, checkpoint: &AnalysisCheckpoint) -> String {
    let strategy = if checkpoint.p2_strategy.is_empty() {
        "no action"
    } else {
        "an action"
    };
    format!(
        "analysis job {} (generation {}): complete, depth {}, {} ms, \
         the strategy holds {strategy}, playable {}",
        job.id,
        job.generation,
        checkpoint.depth_reached,
        checkpoint.elapsed.as_millis(),
        checkpoint.strategy_is_playable
    )
}

/// Builds the public progress row of one checkpoint.
///
/// The row carries the wall-clock cost of the search, never the strategy and
/// never a count that divides out an action-set size. See
/// [`AnalysisCheckpoint::turns_simulated`].
fn checkpoint_dto(checkpoint: &AnalysisCheckpoint, generation: u64) -> AnalysisCheckpointDto {
    AnalysisCheckpointDto {
        generation: checkpoint.generation,
        stale: checkpoint.generation != generation,
        turn_number: checkpoint.turn_number,
        depth_reached: checkpoint.depth_reached,
        elapsed_ms: checkpoint.elapsed.as_millis() as u64,
        seed: checkpoint.seed,
        warnings: checkpoint.warnings.clone(),
    }
}

/// Starts one analysis job for the current position.
///
/// Does nothing when the session holds no P2 profile, and nothing outside a
/// battle position. The caller holds the session lock, so this call only spawns
/// the task. The task takes the lock itself after the search returns.
pub fn start_job(
    battle_id: &str,
    session: &mut BattleSession,
    dexes: Arc<Dexes>,
    meta: Arc<MetaDexes>,
    sessions: Arc<Mutex<HashMap<String, BattleSession>>>,
) {
    let Some(profile) = session.bot_p2.as_ref() else {
        return;
    };
    if !matches!(session.state, MatchState::BattleState(_)) {
        // `routes::turn` calls this after the turn resolves, so a finished game
        // is the only position that reaches this exit. That exit is correct,
        // and the client asks for no further turn. Name the two cases apart, so
        // the line does not report a normal end as a skipped job.
        let reason = if matches!(session.state, MatchState::GameOverState { .. }) {
            "the battle is over"
        } else {
            "the position is not a battle"
        };
        eprintln!(
            "analysis: no job started at generation {}, because {reason}",
            session.analysis.generation()
        );
        return;
    }

    let search = profile.search;
    let seed = profile.view.seed.unwrap_or_else(random_seed);
    let time_ms = profile.view.time_ms;
    let format = MetaFormat::from_active_per_side(session.config.active_per_side);
    let state = session.state.clone();
    let belief_p2 = session.belief_p2.clone();
    let generation = session.analysis.generation;
    // Only a belief search reads the inference rules, and cloning the learnset
    // dex is not free, so build the copy for those two algorithms alone.
    let inference = match search {
        BotSearchConfig::Ismcts(_) | BotSearchConfig::Mccfr(_) => session
            .inference_config
            .as_ref()
            .map(clone_inference_config),
        _ => None,
    };

    let replay = AnalysisReplay {
        generation,
        turn_number: 0,
        search_seed: seed,
        algorithm: profile.view.algorithm.clone(),
        preset: profile.view.preset.clone(),
        time_ms: profile.view.time_ms,
        node_budget: profile.view.node_budget,
        depth: profile.view.depth,
        workers: profile.view.workers,
        iterations: profile.view.iterations,
        particles: profile.view.particles,
        max_actions_per_player: profile.view.max_actions_per_player,
        damage_rolls: session.config.damage_rolls,
        consider_crit: session.config.consider_crit,
    };

    let job = session.analysis.start();
    let battle_id = battle_id.to_string();
    eprintln!(
        "analysis job {} (generation {}): start, algorithm {}, preset {}, depth {}, {}",
        job.id,
        job.generation,
        replay.algorithm,
        replay.preset,
        replay.depth,
        // `resolve` fills this field for every profile, and `Debug` would print
        // `Some(20000)` where the line names a count of milliseconds.
        time_ms.map_or_else(
            || "no time limit".to_string(),
            |limit| format!("time {limit} ms")
        )
    );

    tokio::task::spawn_blocking(move || {
        // A cancel before the search saves the whole search.
        if job.cancel.is_cancelled() {
            eprintln!(
                "analysis job {} (generation {}): cancelled before the search started",
                job.id, job.generation
            );
            return;
        }
        let outcome = run_search(
            search,
            seed,
            time_ms,
            generation,
            &state,
            belief_p2.as_ref(),
            &dexes,
            meta.for_format(format),
            inference,
            replay,
            &job.cancel,
        );
        // A cancelled search returns an answer that holds only the work that
        // finished, so this task reports nothing at all. Two rules need that.
        // A partial answer must not replace the last complete checkpoint. A
        // cancelled job must also leave the record of the replacement job.
        // The ticket check in `accept` protects the race after this flag check.
        if job.cancel.is_cancelled() {
            eprintln!(
                "analysis job {} (generation {}): cancelled during the search, \
                 so it reports no result",
                job.id, job.generation
            );
            return;
        }
        let mut sessions = sessions.lock().unwrap_or_else(|e| e.into_inner());
        // A deleted battle leaves the task with no target.
        let line = if let Some(session) = sessions.get_mut(&battle_id) {
            session.analysis.accept(&job, outcome)
        } else {
            format!(
                "analysis job {} (generation {}): the battle is gone, \
                 so it reports no result",
                job.id, job.generation
            )
        };
        // A console write can block. The operator can pause a Windows console,
        // and a redirected stream can fill its pipe. This lock guards every
        // session, so release it before the write.
        drop(sessions);
        eprintln!("{line}");
    });
}

/// Draws the seed of one job.
///
/// The checkpoint publishes this number, and the client reads it as a JSON
/// number, so it must survive a round trip through a JavaScript double.
/// `bot::resolve` rejects a submitted seed above [`MAX_SAFE_INTEGER`] for the
/// same reason, and a seed the server itself draws has to satisfy the rule the
/// server enforces. That constant is `2^53 - 1`, so the mask is uniform.
fn random_seed() -> u64 {
    rand::random::<u64>() & MAX_SAFE_INTEGER
}

/// Copies the inference rules of a session for a belief search.
///
/// `InferenceConfig` lives in the engine, and the determinizer needs an owned
/// copy per job. `tracker_analysis.rs` copies its own session rules the same
/// way.
pub fn clone_inference_config(config: &InferenceConfig) -> InferenceConfig {
    InferenceConfig {
        use_stat_points: config.use_stat_points,
        force_max_ivs: config.force_max_ivs,
        level: config.level,
        legal_items: config.legal_items.clone(),
        allow_repeat_items: config.allow_repeat_items,
        learnset_dex: config.learnset_dex.clone(),
        ev_total_cap: config.ev_total_cap,
    }
}

/// Runs one search and converts its result to a checkpoint.
///
/// A configuration that this session cannot serve returns an error, so the
/// session keeps its last complete checkpoint.
#[allow(clippy::too_many_arguments)]
fn run_search(
    search: BotSearchConfig,
    seed: u64,
    time_ms: Option<u64>,
    generation: u64,
    state: &MatchState,
    belief_p2: Option<&UnknownMatchState>,
    dexes: &Dexes,
    meta: Option<&MetaDex>,
    inference: Option<InferenceConfig>,
    replay: AnalysisReplay,
    cancel: &CancelFlag,
) -> Result<AnalysisCheckpoint, String> {
    let turn_number = match state {
        MatchState::BattleState(battle) => battle.turn_number,
        _ => return Err("the analysis job runs only on a battle position".to_string()),
    };
    let started = Instant::now();

    // A solver panic must not poison the session mutex, so catch it here and
    // report it as an ordinary job failure.
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        one_search(search, seed, state, belief_p2, dexes, meta, inference, cancel)
    }));
    let mut checkpoint = match caught {
        Ok(result) => result?,
        Err(payload) => return Err(panic_message(payload)),
    };

    checkpoint.generation = generation;
    checkpoint.turn_number = turn_number;
    checkpoint.seed = seed;
    checkpoint.replay = AnalysisReplay {
        turn_number,
        ..replay
    };
    checkpoint.strategy_is_playable = strategy_respects_fog(search, belief_p2.is_some());
    if let Some(line) = perfect_information_warning(search, belief_p2.is_some()) {
        checkpoint.warnings.push(line);
    }
    // An exact search maps the limit to `SolveConfig::deadline`, and a sampling
    // search holds no deadline field. Neither one stops on the exact
    // millisecond, so measure the whole job and report the overrun.
    if let Some(limit) = time_ms {
        let elapsed = started.elapsed();
        if elapsed > Duration::from_millis(limit) {
            checkpoint.warnings.push(format!(
                "The search took {} ms, which is past the {limit} ms limit.",
                elapsed.as_millis()
            ));
        }
    }
    Ok(checkpoint)
}

/// Routes one resolved profile to its solver entry point.
///
/// The returned checkpoint carries no generation, no turn number, and no seed.
/// [`run_search`] fills those three fields.
///
/// Every arm passes `cancel`, so a raised flag stops the search that runs.
#[allow(clippy::too_many_arguments)]
fn one_search(
    search: BotSearchConfig,
    seed: u64,
    state: &MatchState,
    belief_p2: Option<&UnknownMatchState>,
    dexes: &Dexes,
    meta: Option<&MetaDex>,
    inference: Option<InferenceConfig>,
    cancel: &CancelFlag,
) -> Result<AnalysisCheckpoint, String> {
    let cancel = Some(cancel);
    match search {
        BotSearchConfig::Exact(config) => {
            let result = solver::solve_seeded_cancellable(
                seed,
                state,
                &dexes.pokemon_dex,
                &dexes.move_dex,
                &config,
                cancel,
            )
            .map_err(engine_error)?;
            Ok(partial_checkpoint(
                result.p2_win_odds,
                result.p2_strategy,
                result.depth_reached,
                result.stats.turns_simulated,
                result.stats.nodes_expanded,
                result.stats.elapsed,
                &result.warnings,
                &[],
            ))
        }
        BotSearchConfig::Mcts(config) => {
            let result = solver::mcts::search_cancellable(
                seed,
                state,
                &dexes.pokemon_dex,
                &dexes.move_dex,
                &config,
                cancel,
            )
            .map_err(engine_error)?;
            Ok(partial_checkpoint(
                result.p2_win_odds,
                result.p2_strategy,
                config.depth,
                result.stats.turns_simulated,
                result.stats.nodes_created,
                result.stats.elapsed,
                &result.warnings,
                &[],
            ))
        }
        BotSearchConfig::Ismcts(config) => {
            let (belief, determinize) = belief_search_inputs(belief_p2, inference)?;
            let meta = meta.ok_or_else(unsupported_meta)?;
            let result = solver::ismcts::search_belief_cancellable(
                seed,
                belief,
                meta,
                &dexes.pokemon_dex,
                &dexes.move_dex,
                &config,
                &determinize,
                cancel,
            )
            .map_err(engine_error)?;
            Ok(partial_checkpoint(
                result.p2_win_odds,
                result.p2_strategy,
                config.search.depth,
                result.stats.turns_simulated,
                result.stats.nodes_created,
                result.stats.elapsed,
                &result.warnings,
                &result.draw_warnings,
            ))
        }
        BotSearchConfig::Mccfr(config) => {
            let (belief, determinize) = belief_search_inputs(belief_p2, inference)?;
            let meta = meta.ok_or_else(unsupported_meta)?;
            let result = solver::mccfr::search_belief_cancellable(
                seed,
                belief,
                meta,
                &dexes.pokemon_dex,
                &dexes.move_dex,
                &config,
                &determinize,
                cancel,
            )
            .map_err(engine_error)?;
            Ok(partial_checkpoint(
                result.p2_win_odds,
                result.p2_strategy,
                config.search.depth,
                result.stats.turns_simulated,
                result.stats.nodes_created,
                result.stats.elapsed,
                &result.warnings,
                &result.draw_warnings,
            ))
        }
    }
}

/// Reports a search that read more than the fog of war allows.
///
/// `Exact` and `Mcts` search the true `MatchState`, so on a fog-of-war session
/// they read P1's unrevealed moves, item, ability, and spread. Only `Ismcts` and
/// `Mccfr` draw their worlds from P2's belief. The profile's `approximations`
/// list every way the answer falls short of exact and no way it overshoots, so
/// name this one on the checkpoint.
///
/// The line names the algorithm, which the client already reads back in
/// `BattleView::bot_p2`, and no state of either side.
fn perfect_information_warning(search: BotSearchConfig, fogged: bool) -> Option<String> {
    (!strategy_respects_fog(search, fogged)).then(|| {
        "This algorithm searched the true position, so the answer used data that the fog of \
         war hides. Only ismcts and mccfr search the belief."
            .to_string()
    })
}

/// True when this search can control P2 without reading hidden P1 data.
fn strategy_respects_fog(search: BotSearchConfig, fogged: bool) -> bool {
    !fogged
        || matches!(
            search,
            BotSearchConfig::Ismcts(_) | BotSearchConfig::Mccfr(_)
        )
}

/// The belief and the draw rules of a belief search.
///
/// P2 is the observer, so the determinizer copies P2's own side and samples
/// P1's hidden data.
fn belief_search_inputs(
    belief_p2: Option<&UnknownMatchState>,
    inference: Option<InferenceConfig>,
) -> Result<
    (
        &poke_rust::information::unknowns::UnknownBattleState,
        DeterminizeConfig,
    ),
    String,
> {
    let message = "botP2: this algorithm needs a fog-of-war battle, and this session has none";
    let Some(UnknownMatchState::Battle(belief)) = belief_p2 else {
        return Err(message.to_string());
    };
    let Some(inference) = inference else {
        return Err(message.to_string());
    };
    let determinize = DeterminizeConfig {
        inference,
        observer: Player::P2,
        ..DeterminizeConfig::default()
    };
    Ok((belief, determinize))
}

fn unsupported_meta() -> String {
    "botP2: this algorithm draws worlds from usage data, and no usage cache is loaded \
     (see meta_scraper/README.md)"
        .to_string()
}

/// Converts an engine error to a job message.
///
/// The engine writes its own text, and that text names hidden data: a
/// [`DeterminizeError`](poke_rust::information::determinize::DeterminizeError)
/// prints a species and a belief mon index, and a belief error wraps one. P1
/// reads `GET /api/battles/{id}/analysis` during a hotseat battle, so the
/// response carries a fixed line and the detail goes to the server console,
/// where only the operator reads it.
///
/// The messages that this module writes itself name no state, so those still
/// reach the client whole.
fn engine_error(error: impl std::fmt::Display) -> String {
    eprintln!("analysis job: the search failed: {error}");
    "The search failed. The server console holds the reason.".to_string()
}

/// Converts a caught panic payload to a job error message.
///
/// A panic payload is engine text too, so it goes to the console for the same
/// reason as [`engine_error`].
fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    let detail = payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "no payload".to_string());
    eprintln!("analysis job: the search panicked: {detail}");
    "The search panicked. The server console holds the reason.".to_string()
}

/// Renders one solver warning for the progress view.
///
/// The line never names a player and never carries an action count. P1 reads
/// the same endpoint, so `ActionsTruncated` would otherwise report the size of
/// P2's action set. Every other figure is either a limit that the client already
/// holds through the profile or a depth that the view already reports.
///
/// Scrubbing the player name makes the two `ActionsTruncated` lines equal, so
/// [`partial_checkpoint`] also drops the repeat. See the note there.
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
        solver::SolveWarning::ActionsTruncated { .. } => {
            "The action cap removed at least one action, so the search can miss it.".to_string()
        }
        // A cancelled job reports no checkpoint, so this line reaches the client
        // only through a search that a caller inside the process cancelled. The
        // line names no state of either side.
        solver::SolveWarning::Cancelled => {
            "The search was cancelled, so the answer holds only the work that finished.".to_string()
        }
    }
}

/// Builds the shared part of a checkpoint from one solver result.
///
/// Repeated lines collapse to one. The solver raises `ActionsTruncated` once
/// per player, and [`warning_line`] scrubs the player name, so both players
/// render the same line. The count of that line would then carry the fact that
/// the scrub removes: P1 holds its own action count and the cap, so two lines
/// say P2 exceeded the cap and one line says P2 did not. One line for each
/// distinct message says only that some action was cut.
#[allow(clippy::too_many_arguments)]
fn partial_checkpoint(
    p2_win_odds: f64,
    p2_strategy: Vec<JointActionProb>,
    depth_reached: u8,
    turns_simulated: u64,
    nodes: u64,
    elapsed: Duration,
    warnings: &[solver::SolveWarning],
    draw_warnings: &[poke_rust::information::determinize::DeterminizeWarning],
) -> AnalysisCheckpoint {
    let mut seen = std::collections::HashSet::new();
    let mut lines: Vec<String> = warnings
        .iter()
        .map(warning_line)
        .filter(|line| seen.insert(line.clone()))
        .collect();
    if !draw_warnings.is_empty() {
        lines.push(format!(
            "The determinizer reported {} warning(s) while it drew the hidden side.",
            draw_warnings.len()
        ));
    }
    AnalysisCheckpoint {
        generation: 0,
        turn_number: 0,
        p2_win_odds,
        p2_strategy,
        depth_reached,
        turns_simulated,
        nodes,
        elapsed,
        seed: 0,
        replay: AnalysisReplay::default(),
        strategy_is_playable: true,
        warnings: lines,
    }
}

// ── The P2 draw ──────────────────────────────────────────────────────────────

/// Which rule produced the drawn P2 command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DrawSource {
    /// The mixed strategy of a checkpoint of the current position.
    Strategy,
    /// A uniform draw over the legal joint actions.
    Uniform,
    /// A uniform draw over the team-preview choices.
    TeamPreview,
}

impl DrawSource {
    fn as_str(self) -> &'static str {
        match self {
            DrawSource::Strategy => "strategy",
            DrawSource::Uniform => "uniform",
            DrawSource::TeamPreview => "teamPreview",
        }
    }
}

/// One drawn P2 command.
///
/// The record holds one action. It holds no probability of that action and no
/// other action of the strategy, so the reveal that [`reveal_dto`] builds shows
/// P2's choice and nothing else of P2's plan.
pub struct P2Draw {
    pub command: PlayerCommand,
    pub source: DrawSource,
    /// The seed of this draw.
    pub seed: u64,
    /// The replay record of the checkpoint that supplied the strategy.
    /// `None` for either uniform draw.
    pub replay: Option<AnalysisReplay>,
}

/// Draws the command that P2 plays this turn.
///
/// The three cases run in this order:
///
/// 1. A team-preview position draws one choice with a uniform weight.
/// 2. A battle position with a checkpoint of the current position draws one
///    joint action from the mixed strategy of that checkpoint.
/// 3. Every other case draws one legal joint action with a uniform weight.
///
/// Case 3 covers a job that has not finished, a job that failed, and a strategy
/// whose action the legality check rejects. [`P2Draw::source`] names the case
/// that ran, so the client can show which one it was.
pub fn draw_p2_command(session: &BattleSession, dexes: &Dexes) -> Result<P2Draw, String> {
    let seed = draw_seed(session);
    let mut rng = StdRng::seed_from_u64(seed);

    if let MatchState::TeamPreviewState(preview) = &session.state {
        let choices = solver::preview::preview_choices(preview, Player::P2);
        if choices.is_empty() {
            return Err("P2 has no legal team-preview choice".to_string());
        }
        let pick = choices[rng.gen_range(0..choices.len())].clone();
        return Ok(P2Draw {
            command: PlayerCommand::TeamPreview(pick),
            source: DrawSource::TeamPreview,
            seed,
            replay: None,
        });
    }

    let MatchState::BattleState(battle) = &session.state else {
        return Err("battle is already over".to_string());
    };

    // Case 2 has four exits, and three of them fall through to the uniform draw
    // of case 3. Name the exit on the console. The reason reads P2's plan, so it
    // never reaches the client. See `engine_error`.
    let generation = session.analysis.generation();
    match session.analysis.current_checkpoint() {
        None => eprintln!(
            "analysis draw (generation {generation}): the draw is uniform, \
             because no checkpoint covers this position"
        ),
        Some(checkpoint) if !checkpoint.strategy_is_playable => eprintln!(
            "analysis draw (generation {generation}): the draw is uniform, \
             because the strategy read data that the fog of war hides"
        ),
        Some(checkpoint) => match sample_strategy(&checkpoint.p2_strategy, &mut rng) {
            None => eprintln!(
                "analysis draw (generation {generation}): the draw is uniform, \
                 because the strategy holds no action with a positive weight"
            ),
            Some(commands) => match accept(session, dexes, commands) {
                None => eprintln!(
                    "analysis draw (generation {generation}): the draw is uniform, \
                     because the legality check rejected the sampled action"
                ),
                Some(command) => {
                    return Ok(P2Draw {
                        command,
                        source: DrawSource::Strategy,
                        seed,
                        replay: Some(checkpoint.replay.clone()),
                    });
                }
            },
        },
    }

    // No cap and no dominance pruning: the draw needs the whole legal set, and
    // either filter would remove an action that P2 may play.
    let joint = solver::actions::joint_actions(
        battle,
        Player::P2,
        solver::actions::phase_of(&session.state),
        &dexes.move_dex,
        &dexes.pokemon_dex,
        None,
        false,
    );
    if joint.actions.is_empty() {
        return Err("P2 has no legal command right now".to_string());
    }
    let start = rng.gen_range(0..joint.actions.len());
    first_legal_from(session, dexes, &joint.actions, start)
        .map(|command| P2Draw {
            command,
            source: DrawSource::Uniform,
            seed,
            replay: None,
        })
        .ok_or_else(|| "P2 has no legal command right now".to_string())
}

/// Renders one drawn P2 command for the turn response.
///
/// `state` must be the position before the turn, so each description names the
/// Pokemon that acted.
pub fn reveal_dto(state: &MatchState, draw: &P2Draw) -> P2RevealDto {
    // Team preview renders nothing: the leads appear on the field of their own
    // accord, and the back picks stay hidden under the fog of war.
    let commands = match (state, &draw.command) {
        (MatchState::BattleState(battle), PlayerCommand::Battle(commands)) => commands
            .iter()
            .enumerate()
            .map(|(slot_idx, command)| {
                mapping::command_option(battle, Player::P2, slot_idx, command)
            })
            .collect(),
        _ => Vec::new(),
    };
    P2RevealDto {
        commands,
        source: draw.source.as_str().to_string(),
        draw_seed: draw.seed,
        replay: draw.replay.as_ref().map(replay_dto),
    }
}

/// Builds the replay row of one reveal.
///
/// Each digest goes over the wire as a decimal string, because a `u64` above
/// `2^53` loses precision in a JavaScript number.
fn replay_dto(replay: &AnalysisReplay) -> AnalysisReplayDto {
    AnalysisReplayDto {
        generation: replay.generation,
        turn_number: replay.turn_number,
        search_seed: replay.search_seed,
        algorithm: replay.algorithm.clone(),
        preset: replay.preset.clone(),
        time_ms: replay.time_ms,
        node_budget: replay.node_budget,
        depth: replay.depth,
        workers: replay.workers,
        iterations: replay.iterations,
        particles: replay.particles,
        max_actions_per_player: replay.max_actions_per_player,
        damage_rolls: replay.damage_rolls,
        consider_crit: replay.consider_crit,
    }
}

/// The seed of one draw.
///
/// A profile seed makes the whole battle reproducible, so the generation mixes
/// into it. Without that mix one seed would select the same index of every
/// strategy, and P2 would repeat one action for the whole battle. A profile
/// with no seed draws a fresh one under the mask that [`random_seed`] explains.
fn draw_seed(session: &BattleSession) -> u64 {
    mix_draw_seed(
        session.bot_p2.as_ref().and_then(|p| p.view.seed),
        session.analysis.generation(),
    )
}

/// Mixes a profile seed with the generation of the position.
///
/// The result stays under [`MAX_SAFE_INTEGER`], because the reveal publishes it
/// and the client reads it as a JavaScript number.
fn mix_draw_seed(profile_seed: Option<u64>, generation: u64) -> u64 {
    match profile_seed {
        Some(seed) => (seed ^ generation.wrapping_mul(0x9E37_79B9_7F4A_7C15)) & MAX_SAFE_INTEGER,
        None => random_seed(),
    }
}

/// Draws one joint action from a mixed strategy.
///
/// Returns `None` for an empty strategy. A negative weight counts as zero, and
/// a total that falls short of one still returns an action, so a rounding loss
/// never drops the draw.
fn sample_strategy<'a>(
    strategy: &'a [JointActionProb],
    rng: &mut StdRng,
) -> Option<&'a [BattleCommand]> {
    let total: f64 = strategy
        .iter()
        .map(|action| action.probability.max(0.0))
        .sum();
    if !total.is_finite() || total <= 0.0 {
        return strategy.first().map(|action| action.commands.as_slice());
    }
    let mut roll = rng.gen_range(0.0..1.0) * total;
    for action in strategy {
        roll -= action.probability.max(0.0);
        if roll <= 0.0 {
            return Some(&action.commands);
        }
    }
    strategy.last().map(|action| action.commands.as_slice())
}

/// The first joint action that the legality check accepts.
///
/// The scan starts at `start` and wraps, so one seed always gives one answer.
fn first_legal_from(
    session: &BattleSession,
    dexes: &Dexes,
    actions: &[Vec<BattleCommand>],
    start: usize,
) -> Option<PlayerCommand> {
    (0..actions.len())
        .map(|offset| &actions[(start + offset) % actions.len()])
        .find_map(|commands| accept(session, dexes, commands))
}

/// Runs one drawn joint action through the legality check of the server.
///
/// The check is the one that a submitted command takes, so a drawn command can
/// never reach the engine on a path that a client command could not.
fn accept(
    session: &BattleSession,
    dexes: &Dexes,
    commands: &[BattleCommand],
) -> Option<PlayerCommand> {
    let dto = PlayerCommandDto::Battle {
        commands: commands.iter().map(mapping::battle_command_dto).collect(),
    };
    session::reconstruct_player_command(session, dexes, Player::P2, &dto).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use poke_rust::state::battle::{BattleCommand, SwitchCommand};

    fn checkpoint(generation: u64, turn_number: u16) -> AnalysisCheckpoint {
        AnalysisCheckpoint {
            generation,
            turn_number,
            p2_win_odds: 0.75,
            p2_strategy: vec![JointActionProb {
                commands: vec![BattleCommand::Pass],
                probability: 1.0,
            }],
            depth_reached: 2,
            turns_simulated: 40,
            nodes: 12,
            elapsed: Duration::from_millis(30),
            seed: 7,
            replay: AnalysisReplay {
                generation,
                turn_number,
                search_seed: 7,
                algorithm: "doubleOracle".to_string(),
                preset: "balanced".to_string(),
                time_ms: Some(10_000),
                node_budget: Some(500_000),
                depth: 2,
                workers: 1,
                iterations: None,
                particles: None,
                max_actions_per_player: Some(12),
                damage_rolls: 16,
                consider_crit: true,
            },
            strategy_is_playable: true,
            warnings: vec!["DepthNotReached".to_string()],
        }
    }

    #[test]
    fn invalidate_raises_the_generation_and_keeps_the_checkpoint() {
        let mut state = AnalysisState::default();
        let job = state.start();
        state.accept(&job, Ok(checkpoint(0, 3)));
        assert_eq!(state.checkpoint().unwrap().turn_number, 3);

        state.invalidate();

        assert_eq!(state.generation, 1);
        assert_eq!(state.checkpoint().unwrap().turn_number, 3);
        let view = state.progress();
        assert_eq!(view.generation, 1);
        assert!(view.checkpoint.unwrap().stale);
    }

    #[test]
    fn accept_stores_a_result_of_the_current_generation() {
        let mut state = AnalysisState::default();
        state.invalidate();
        let job = state.start();

        state.accept(&job, Ok(checkpoint(1, 5)));

        assert_eq!(state.checkpoint().unwrap().turn_number, 5);
        let view = state.progress();
        assert_eq!(view.phase, "complete");
        assert!(!view.checkpoint.unwrap().stale);
    }

    #[test]
    fn accept_drops_a_result_of_an_old_generation() {
        let mut state = AnalysisState::default();
        let job = state.start();
        state.accept(&job, Ok(checkpoint(0, 1)));
        state.invalidate();

        state.accept(&job, Ok(checkpoint(0, 99)));

        assert_eq!(state.checkpoint().unwrap().turn_number, 1);
        assert!(state.progress().error.is_none());
    }

    #[test]
    fn a_failed_job_keeps_the_last_complete_checkpoint() {
        let mut state = AnalysisState::default();
        let complete = state.start();
        state.accept(&complete, Ok(checkpoint(0, 2)));
        let failed = state.start();

        state.accept(&failed, Err("the search panicked".to_string()));

        assert_eq!(state.checkpoint().unwrap().turn_number, 2);
        let view = state.progress();
        assert_eq!(view.phase, "failed");
        assert_eq!(view.error.as_deref(), Some("the search panicked"));
        assert_eq!(view.checkpoint.unwrap().turn_number, 2);
    }

    #[test]
    fn invalidate_sets_the_cancel_flag_of_the_running_job() {
        let mut state = AnalysisState::default();
        let job = state.start();
        assert!(!job.cancel.is_cancelled());
        assert_eq!(state.progress().phase, "running");

        state.invalidate();

        assert!(job.cancel.is_cancelled());
        assert_eq!(state.progress().phase, "idle");
    }

    #[test]
    fn a_second_start_cancels_the_first_job() {
        let mut state = AnalysisState::default();
        let first = state.start();
        let second = state.start();

        assert!(first.cancel.is_cancelled());
        assert!(!second.cancel.is_cancelled());
    }

    /// A replaced job must not store a result or clear the new running job.
    #[test]
    fn accept_drops_a_result_from_a_replaced_job() {
        let mut state = AnalysisState::default();
        let first = state.start();
        let second = state.start();

        state.accept(&first, Ok(checkpoint(0, 1)));

        assert!(state.checkpoint().is_none());
        assert_eq!(state.progress().phase, "running");

        state.accept(&second, Ok(checkpoint(0, 2)));

        assert_eq!(state.checkpoint().unwrap().turn_number, 2);
        assert_eq!(state.progress().phase, "complete");
    }

    /// A clone of the flag shares the signal, which is what lets the session
    /// keep one handle while the search thread holds another.
    #[test]
    fn a_cloned_cancel_flag_shares_the_signal() {
        let flag = CancelFlag::new();
        let copy = flag.clone();

        assert!(!copy.is_cancelled());
        flag.cancel();

        assert!(copy.is_cancelled());
    }

    #[test]
    fn a_new_job_clears_the_last_error() {
        let mut state = AnalysisState::default();
        let failed = state.start();
        state.accept(&failed, Err("no usage cache".to_string()));
        assert_eq!(state.progress().phase, "failed");

        state.start();

        assert!(state.progress().error.is_none());
    }

    /// The progress endpoint is private data of the P2 side, so it must never
    /// carry an action or a win probability.
    #[test]
    fn the_progress_view_holds_no_p2_strategy() {
        let mut state = AnalysisState::default();
        let job = state.start();
        state.accept(&job, Ok(checkpoint(0, 4)));

        let view = state.progress();
        let json = serde_json::to_string(&view).unwrap();

        assert!(!json.contains("Pass"), "{json}");
        assert!(!json.contains("strategy"), "{json}");
        assert!(!json.contains("0.75"), "{json}");
        assert!(!json.to_lowercase().contains("odds"), "{json}");
        assert!(!json.to_lowercase().contains("command"), "{json}");
        // Wall-clock cost is public.
        assert!(json.contains("elapsedMs"), "{json}");
    }

    /// A node count and a turn-simulation count both divide by P1's own action
    /// count to give P2's, which is the figure `warning_line` scrubs out of
    /// `ActionsTruncated`. Neither may reach the wire.
    #[test]
    fn the_progress_view_holds_no_search_node_count() {
        let mut state = AnalysisState::default();
        let job = state.start();
        state.accept(&job, Ok(checkpoint(0, 4)));

        let json = serde_json::to_string(&state.progress()).unwrap();

        assert!(!json.contains("turnsSimulated"), "{json}");
        assert!(!json.contains("nodes"), "{json}");
        // The two counts of the fixture, in case a later field renames them.
        assert!(!json.contains("40"), "{json}");
        assert!(!json.contains("12"), "{json}");
    }

    /// The console line of a complete job must hold no action count either.
    ///
    /// P1 starts the server of a hotseat battle, so P1 can read that console.
    /// `p2_strategy.len()` is P2's joint-action count, which is the figure
    /// `warning_line` scrubs and `turns_simulated` withholds. Two strategies of
    /// different size must therefore write the same line.
    #[test]
    fn the_complete_line_holds_no_action_count() {
        let mut narrow = AnalysisState::default();
        let job = narrow.start();
        let short = narrow.accept(&job, Ok(checkpoint(0, 4)));

        let mut wide = AnalysisState::default();
        let job = wide.start();
        let mut many = checkpoint(0, 4);
        many.p2_strategy = vec![many.p2_strategy[0].clone(); 12];
        let long = wide.accept(&job, Ok(many));

        assert_eq!(short, long, "the line counts P2's actions");
        assert!(!short.contains("12"), "{short}");
        assert!(short.contains("the strategy holds an action"), "{short}");
    }

    /// An empty strategy explains the uniform draw, so the line names it.
    /// This one bit is not a count.
    #[test]
    fn the_complete_line_names_an_empty_strategy() {
        let mut state = AnalysisState::default();
        let job = state.start();
        let mut empty = checkpoint(0, 4);
        empty.p2_strategy.clear();

        let line = state.accept(&job, Ok(empty));

        assert!(line.contains("the strategy holds no action"), "{line}");
    }

    /// A dropped result and a failure both return their own line, so the caller
    /// writes the console line after it releases the session lock.
    #[test]
    fn each_accept_exit_returns_its_own_line() {
        let mut state = AnalysisState::default();
        let stale = state.start();
        state.invalidate();
        let dropped = state.accept(&stale, Ok(checkpoint(0, 1)));
        assert!(dropped.contains("the position moved"), "{dropped}");

        let first = state.start();
        let second = state.start();
        let replaced = state.accept(&first, Ok(checkpoint(1, 1)));
        assert!(replaced.contains("another job replaced it"), "{replaced}");

        let failed = state.accept(&second, Err("the search panicked".to_string()));
        assert!(failed.contains("no checkpoint"), "{failed}");
    }

    /// The published seed goes back over the wire as a JSON number, and
    /// `bot::resolve` rejects a submitted seed above `MAX_SAFE_INTEGER`. A seed
    /// the server draws itself has to pass the rule the server enforces.
    #[test]
    fn a_drawn_seed_survives_a_javascript_number() {
        for _ in 0..1_000 {
            let seed = random_seed();
            assert!(seed <= MAX_SAFE_INTEGER, "{seed}");
            assert_eq!(seed as f64 as u64, seed, "{seed}");
        }
    }

    /// `Exact` and `Mcts` search the true position, so on a fog-of-war session
    /// they read data that P2 cannot legally hold.
    #[test]
    fn a_perfect_information_search_on_a_fogged_session_says_so() {
        use poke_rust::solver::SolveConfig;
        use poke_rust::solver::mcts::MctsConfig;

        let exact = BotSearchConfig::Exact(SolveConfig::default());
        let mcts = BotSearchConfig::Mcts(MctsConfig::default());

        for search in [exact, mcts] {
            let line = perfect_information_warning(search, true)
                .expect("a fogged session must report the perfect-information read");
            assert!(line.contains("fog of war"), "{line}");
            // A perfect-information session hides nothing, so there is nothing
            // to report.
            assert_eq!(perfect_information_warning(search, false), None);
        }
    }

    /// A belief search reads only the belief, so it never earns the line.
    #[test]
    fn a_belief_search_reports_no_perfect_information_read() {
        use poke_rust::solver::ismcts::IsmctsConfig;
        use poke_rust::solver::mccfr::MccfrConfig;

        for search in [
            BotSearchConfig::Ismcts(IsmctsConfig::default()),
            BotSearchConfig::Mccfr(MccfrConfig::default()),
        ] {
            assert_eq!(perfect_information_warning(search, true), None);
        }
    }

    /// A true-state search can report progress in a fogged session, but its
    /// strategy must not control P2.
    #[test]
    fn a_true_state_strategy_cannot_control_a_fogged_bot() {
        let exact = BotSearchConfig::Exact(Default::default());
        let mcts = BotSearchConfig::Mcts(Default::default());
        let ismcts = BotSearchConfig::Ismcts(Default::default());
        let mccfr = BotSearchConfig::Mccfr(Default::default());

        assert!(!strategy_respects_fog(exact, true));
        assert!(!strategy_respects_fog(mcts, true));
        assert!(strategy_respects_fog(ismcts, true));
        assert!(strategy_respects_fog(mccfr, true));
        assert!(strategy_respects_fog(exact, false));
    }

    /// The engine writes a species and a belief mon index into its own error
    /// text, and P1 reads this endpoint during a hotseat battle.
    #[test]
    fn an_engine_error_reaches_the_client_without_its_detail() {
        use poke_rust::information::determinize::DeterminizeError;
        use poke_rust::solver::belief::BeliefError;
        use poke_rust::solver::ismcts::IsmctsError;

        let error = IsmctsError::Belief(BeliefError::Draw {
            world: 0,
            error: DeterminizeError::UnknownSpecies {
                mon_idx: 1,
                species: poke_rust::data::species::Species::Zoroark,
            },
        });
        let raw = error.to_string();
        assert!(raw.contains("Zoroark"), "{raw}");

        let message = engine_error(error);

        assert!(!message.contains("Zoroark"), "{message}");
        assert!(!message.contains("mon 1"), "{message}");
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

    /// `ActionsTruncated` names a player and its action count, and a
    /// determinizer warning names a Pokemon slot. Neither figure may reach P1.
    #[test]
    fn a_warning_line_holds_no_player_and_no_action_count() {
        let checkpoint = partial_checkpoint(
            0.5,
            Vec::new(),
            1,
            0,
            0,
            Duration::ZERO,
            &[
                solver::SolveWarning::ActionsTruncated {
                    player: Player::P2,
                    kept: 6,
                    total: 37,
                },
                solver::SolveWarning::DepthNotReached {
                    target: 3,
                    reached: 1,
                },
            ],
            &[
                poke_rust::information::determinize::DeterminizeWarning::LearnsetTopUp {
                    mon_idx: 2,
                    moves: vec![poke_rust::data::pokemon_move::PokemonMove::Protect],
                },
            ],
        );
        let joined = checkpoint.warnings.join("\n");

        assert!(!joined.contains("P2"), "{joined}");
        assert!(!joined.contains("37"), "{joined}");
        assert!(!joined.contains("Protect"), "{joined}");
        assert_eq!(checkpoint.warnings.len(), 3);
        assert!(joined.contains("action cap"), "{joined}");
        assert!(joined.contains("depth 1 of the 3"), "{joined}");
        assert!(
            joined.contains("determinizer reported 1 warning"),
            "{joined}"
        );
    }

    /// The solver raises `ActionsTruncated` once per player, and the scrub makes
    /// both lines equal, so the count of that line replaces the player name that
    /// the scrub removed. P1 holds its own action count and the cap, so two
    /// lines would tell P1 that P2 exceeded the cap.
    #[test]
    fn a_repeated_warning_line_appears_one_time() {
        let both = |player_a, player_b| {
            partial_checkpoint(
                0.5,
                Vec::new(),
                1,
                0,
                0,
                Duration::ZERO,
                &[
                    solver::SolveWarning::ActionsTruncated {
                        player: player_a,
                        kept: 6,
                        total: 37,
                    },
                    solver::SolveWarning::ActionsTruncated {
                        player: player_b,
                        kept: 6,
                        total: 12,
                    },
                ],
                &[],
            )
            .warnings
        };

        // Both players truncated.
        let two = both(Player::P1, Player::P2);
        assert_eq!(two.len(), 1, "{two:?}");

        // P1 alone truncated. The view must not tell the two cases apart.
        let one = partial_checkpoint(
            0.5,
            Vec::new(),
            1,
            0,
            0,
            Duration::ZERO,
            &[solver::SolveWarning::ActionsTruncated {
                player: Player::P1,
                kept: 6,
                total: 37,
            }],
            &[],
        )
        .warnings;
        assert_eq!(one, two);
    }

    // ── The P2 draw ─────────────────────────────────────────────────────────

    fn strategy(weights: &[f64]) -> Vec<JointActionProb> {
        weights
            .iter()
            .enumerate()
            .map(|(idx, probability)| JointActionProb {
                commands: vec![BattleCommand::Switch(SwitchCommand { party_index: idx })],
                probability: *probability,
            })
            .collect()
    }

    fn drawn_index(commands: &[BattleCommand]) -> usize {
        match &commands[0] {
            BattleCommand::Switch(switch) => switch.party_index,
            other => panic!("the fixture draws a switch, got {other:?}"),
        }
    }

    /// One seed must give one answer, so an operator can repeat a battle.
    #[test]
    fn one_seed_draws_the_same_action_every_time() {
        let strategy = strategy(&[0.3, 0.5, 0.2]);

        let first = {
            let mut rng = StdRng::seed_from_u64(4_242);
            drawn_index(sample_strategy(&strategy, &mut rng).unwrap())
        };
        for _ in 0..20 {
            let mut rng = StdRng::seed_from_u64(4_242);
            let again = drawn_index(sample_strategy(&strategy, &mut rng).unwrap());
            assert_eq!(again, first);
        }
    }

    /// The draw must follow the mixed strategy. A solver that plays its second
    /// action at 25% has to reach that rate at the table.
    #[test]
    fn the_draw_follows_the_strategy_weights() {
        let strategy = strategy(&[0.75, 0.25]);
        let mut rng = StdRng::seed_from_u64(1);
        let mut counts = [0u32; 2];

        for _ in 0..20_000 {
            counts[drawn_index(sample_strategy(&strategy, &mut rng).unwrap())] += 1;
        }

        let second = f64::from(counts[1]) / 20_000.0;
        assert!((second - 0.25).abs() < 0.02, "{counts:?}");
    }

    /// A solver can return weights that do not total one, and a rounding loss
    /// must never drop the draw.
    #[test]
    fn a_short_or_broken_weight_total_still_draws() {
        let mut rng = StdRng::seed_from_u64(9);

        // Weights that fall short of one.
        let short = strategy(&[0.4, 0.4]);
        assert!(sample_strategy(&short, &mut rng).is_some());

        // Every weight zero.
        let zeroed = strategy(&[0.0, 0.0]);
        assert_eq!(drawn_index(sample_strategy(&zeroed, &mut rng).unwrap()), 0);

        // A negative weight counts as zero, so the positive action wins.
        let negative = strategy(&[-1.0, 1.0]);
        for _ in 0..50 {
            assert_eq!(
                drawn_index(sample_strategy(&negative, &mut rng).unwrap()),
                1
            );
        }

        // An empty strategy has nothing to draw, which sends the caller to the
        // uniform case.
        assert!(sample_strategy(&[], &mut rng).is_none());
    }

    /// A stale checkpoint names actions of an older position, so the draw must
    /// not read it. The uniform case then runs.
    #[test]
    fn only_a_checkpoint_of_the_current_position_supplies_a_strategy() {
        let mut state = AnalysisState::default();
        let job = state.start();
        state.accept(&job, Ok(checkpoint(0, 1)));
        assert!(state.current_checkpoint().is_some());

        state.invalidate();

        assert!(state.checkpoint().is_some());
        assert!(state.current_checkpoint().is_none());
    }

    /// A job that never finished leaves no checkpoint at all.
    #[test]
    fn a_missing_checkpoint_supplies_no_strategy() {
        let mut state = AnalysisState::default();
        assert!(state.current_checkpoint().is_none());
        let job = state.start();

        state.accept(&job, Err("the search failed".to_string()));

        assert!(state.current_checkpoint().is_none());
    }

    /// A fixed profile seed must stay reproducible and must still move between
    /// turns. Without the mix, one seed would select the same index of every
    /// strategy for the whole battle.
    #[test]
    fn the_draw_seed_mixes_the_generation_in() {
        let first = mix_draw_seed(Some(77), 3);

        assert_eq!(first, mix_draw_seed(Some(77), 3));
        assert_ne!(first, mix_draw_seed(Some(77), 4));
        assert_ne!(first, mix_draw_seed(Some(78), 3));

        // The reveal publishes the seed as a JavaScript number.
        for generation in 0..1_000 {
            let seed = mix_draw_seed(Some(u64::MAX), generation);
            assert!(seed <= MAX_SAFE_INTEGER, "{seed}");
            assert_eq!(seed as f64 as u64, seed, "{seed}");
        }
    }

    /// Replay metadata must contain every resolved search setting. It must not
    /// contain a digest of hidden state.
    #[test]
    fn replay_metadata_is_complete_and_holds_no_hidden_state_digest() {
        let replay = AnalysisReplay {
            generation: 9,
            turn_number: 4,
            search_seed: 12,
            algorithm: "ismcts".to_string(),
            preset: "strong".to_string(),
            time_ms: Some(40_000),
            node_budget: None,
            depth: 3,
            workers: 1,
            iterations: Some(20_000),
            particles: Some(32),
            max_actions_per_player: None,
            damage_rolls: 16,
            consider_crit: true,
        };

        let json = serde_json::to_string(&replay_dto(&replay)).unwrap();

        assert!(json.contains("\"generation\":9"), "{json}");
        assert!(json.contains("\"turnNumber\":4"), "{json}");
        assert!(json.contains("\"iterations\":20000"), "{json}");
        assert!(json.contains("\"particles\":32"), "{json}");
        assert!(json.contains("\"timeMs\":40000"), "{json}");
        assert!(!json.to_lowercase().contains("hash"), "{json}");
        assert!(!json.to_lowercase().contains("belief"), "{json}");
        assert!(!json.to_lowercase().contains("state"), "{json}");
    }

    /// The reveal shows P2's one action. It must never show the odds of that
    /// action or any other action of the strategy.
    #[test]
    fn the_reveal_holds_no_probability_and_no_second_action() {
        let checkpoint = checkpoint(0, 3);
        let draw = P2Draw {
            command: PlayerCommand::Battle(vec![BattleCommand::Pass]),
            source: DrawSource::Strategy,
            seed: 31,
            replay: Some(checkpoint.replay.clone()),
        };
        // The fixture strategy holds one action; a real one holds more. Build
        // the reveal from the draw alone, which is the whole point.
        let reveal = P2RevealDto {
            commands: Vec::new(),
            source: draw.source.as_str().to_string(),
            draw_seed: draw.seed,
            replay: draw.replay.as_ref().map(replay_dto),
        };

        let json = serde_json::to_string(&reveal).unwrap();

        assert!(!json.contains("probability"), "{json}");
        assert!(!json.contains("strategy\":"), "{json}");
        assert!(!json.to_lowercase().contains("odds"), "{json}");
        assert!(!json.contains("0.75"), "{json}");
        // The cost counts stay private for the reason `warning_line` explains.
        assert!(!json.contains("turnsSimulated"), "{json}");
        assert!(!json.contains("nodes"), "{json}");
        assert_eq!(reveal.source, "strategy");
    }

    /// The collapse must keep every distinct line, so a real second reason
    /// still reaches the client.
    #[test]
    fn the_collapse_keeps_each_distinct_warning() {
        let checkpoint = partial_checkpoint(
            0.5,
            Vec::new(),
            1,
            0,
            0,
            Duration::ZERO,
            &[
                solver::SolveWarning::ActionsTruncated {
                    player: Player::P1,
                    kept: 6,
                    total: 37,
                },
                solver::SolveWarning::ActionsTruncated {
                    player: Player::P2,
                    kept: 6,
                    total: 12,
                },
                solver::SolveWarning::DepthNotReached {
                    target: 3,
                    reached: 1,
                },
            ],
            &[],
        );

        assert_eq!(checkpoint.warnings.len(), 2, "{:?}", checkpoint.warnings);
        let joined = checkpoint.warnings.join("\n");
        assert!(joined.contains("action cap"), "{joined}");
        assert!(joined.contains("depth 1 of the 3"), "{joined}");
    }
}
