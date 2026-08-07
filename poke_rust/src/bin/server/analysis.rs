//! The private P2 analysis job of a battle session.
//!
//! A session with a resolved P2 profile starts one search after each resolved
//! turn. The search runs on a blocking thread, so it never holds the session
//! lock. Each job carries the generation of the position that started it.
//!
//! [`AnalysisState::invalidate`] raises the generation and cancels the running
//! job. [`AnalysisState::accept`] then drops any result whose generation is no
//! longer current, so a slow job can never overwrite a newer answer. Both calls
//! keep the last complete checkpoint, so the client always reads the newest
//! complete answer even after a failure or a cancellation.
//!
//! P1 reads `GET /api/battles/{id}/analysis` during a hotseat battle, so the
//! progress view holds no P2 action, no P2 win odds, no count that divides out
//! P2's action-set size, and no engine-written text. A later item shows the
//! sampled P2 action after both commands lock.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use poke_rust::information::determinize::DeterminizeConfig;
use poke_rust::information::inference::InferenceConfig;
use poke_rust::information::unknowns::UnknownMatchState;
use poke_rust::meta::{MetaDex, MetaFormat};
use poke_rust::solver::{self, JointActionProb};
use poke_rust::state::battle::{MatchState, Player};

use crate::bot::{BotSearchConfig, MAX_SAFE_INTEGER};
use crate::dto::{AnalysisCheckpointDto, AnalysisProgressDto};
use crate::session::{BattleSession, Dexes, MetaDexes};

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
    /// battle lets P1 read every endpoint. The later reveal item reads it.
    #[allow(dead_code)]
    pub p2_win_odds: f64,
    /// P2's mixed strategy at the root.
    /// Private for the same reason as `p2_win_odds`.
    #[allow(dead_code)]
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
    /// Every reason that the answer is approximate.
    pub warnings: Vec<String>,
}

/// The job that is running now.
#[derive(Debug)]
struct RunningJob {
    generation: u64,
    started: Instant,
    /// The solver has no cancellation hook, so this flag stops a job before its
    /// search starts. The generation check in [`AnalysisState::accept`] handles
    /// a job that is already inside the search.
    cancel: Arc<AtomicBool>,
}

/// The analysis record of one session.
#[derive(Debug, Default)]
pub struct AnalysisState {
    /// The generation of the current position.
    /// Every state change raises it by one.
    generation: u64,
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
            job.cancel.store(true, Ordering::Relaxed);
        }
    }

    /// Records the job that is starting and returns its cancel flag.
    /// Cancels an earlier job of the same generation, so one job runs at a time.
    fn start(&mut self) -> Arc<AtomicBool> {
        if let Some(job) = self.running.take() {
            job.cancel.store(true, Ordering::Relaxed);
        }
        let cancel = Arc::new(AtomicBool::new(false));
        self.running = Some(RunningJob {
            generation: self.generation,
            started: Instant::now(),
            cancel: Arc::clone(&cancel),
        });
        self.last_error = None;
        cancel
    }

    /// Stores the result of one job.
    ///
    /// Drops a result from an old generation, so a slow job never overwrites a
    /// newer answer. A failure keeps the last complete checkpoint.
    pub fn accept(&mut self, generation: u64, outcome: Result<AnalysisCheckpoint, String>) {
        if generation != self.generation {
            return;
        }
        if self
            .running
            .as_ref()
            .is_some_and(|job| job.generation == generation)
        {
            self.running = None;
        }
        match outcome {
            Ok(checkpoint) => {
                self.checkpoint = Some(checkpoint);
                self.last_error = None;
            }
            Err(message) => self.last_error = Some(message),
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

    /// The last complete checkpoint, for a test or a later reveal endpoint.
    #[cfg(test)]
    fn checkpoint(&self) -> Option<&AnalysisCheckpoint> {
        self.checkpoint.as_ref()
    }
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
        return;
    }

    let search = profile.search;
    let seed = profile.view.seed.unwrap_or_else(random_seed);
    let time_ms = profile.view.time_ms;
    let format = MetaFormat::from_active_per_side(session.config.active_per_side);
    let state = session.state.clone();
    let belief_p2 = session.belief_p2.clone();
    // Only a belief search reads the inference rules, and cloning the learnset
    // dex is not free, so build the copy for those two algorithms alone.
    let inference = match search {
        BotSearchConfig::Ismcts(_) | BotSearchConfig::Mccfr(_) => session
            .inference_config
            .as_ref()
            .map(clone_inference_config),
        _ => None,
    };

    let generation = session.analysis.generation;
    let cancel = session.analysis.start();
    let battle_id = battle_id.to_string();

    tokio::task::spawn_blocking(move || {
        let outcome = if cancel.load(Ordering::Relaxed) {
            Err("the analysis job was cancelled before its search started".to_string())
        } else {
            run_search(
                search,
                seed,
                time_ms,
                generation,
                &state,
                belief_p2.as_ref(),
                &dexes,
                meta.for_format(format),
                inference,
            )
        };
        let mut sessions = sessions.lock().unwrap_or_else(|e| e.into_inner());
        // A deleted battle leaves the task with no target.
        if let Some(session) = sessions.get_mut(&battle_id) {
            session.analysis.accept(generation, outcome);
        }
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
/// copy per job.
fn clone_inference_config(config: &InferenceConfig) -> InferenceConfig {
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
) -> Result<AnalysisCheckpoint, String> {
    let turn_number = match state {
        MatchState::BattleState(battle) => battle.turn_number,
        _ => return Err("the analysis job runs only on a battle position".to_string()),
    };
    let started = Instant::now();

    // A solver panic must not poison the session mutex, so catch it here and
    // report it as an ordinary job failure.
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        one_search(search, seed, state, belief_p2, dexes, meta, inference)
    }));
    let mut checkpoint = match caught {
        Ok(result) => result?,
        Err(payload) => return Err(panic_message(payload)),
    };

    checkpoint.generation = generation;
    checkpoint.turn_number = turn_number;
    checkpoint.seed = seed;
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
fn one_search(
    search: BotSearchConfig,
    seed: u64,
    state: &MatchState,
    belief_p2: Option<&UnknownMatchState>,
    dexes: &Dexes,
    meta: Option<&MetaDex>,
    inference: Option<InferenceConfig>,
) -> Result<AnalysisCheckpoint, String> {
    match search {
        BotSearchConfig::Exact(config) => {
            let result =
                solver::solve_seeded(seed, state, &dexes.pokemon_dex, &dexes.move_dex, &config)
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
            let result =
                solver::mcts::search(seed, state, &dexes.pokemon_dex, &dexes.move_dex, &config)
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
            let result = solver::ismcts::search_belief(
                seed,
                belief,
                meta,
                &dexes.pokemon_dex,
                &dexes.move_dex,
                &config,
                &determinize,
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
            let result = solver::mccfr::search_belief(
                seed,
                belief,
                meta,
                &dexes.pokemon_dex,
                &dexes.move_dex,
                &config,
                &determinize,
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
    let perfect = matches!(search, BotSearchConfig::Exact(_) | BotSearchConfig::Mcts(_));
    (perfect && fogged).then(|| {
        "This algorithm searched the true position, so the answer used data that the fog of \
         war hides. Only ismcts and mccfr search the belief."
            .to_string()
    })
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
        warnings: lines,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use poke_rust::state::battle::BattleCommand;

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
            warnings: vec!["DepthNotReached".to_string()],
        }
    }

    #[test]
    fn invalidate_raises_the_generation_and_keeps_the_checkpoint() {
        let mut state = AnalysisState::default();
        state.accept(0, Ok(checkpoint(0, 3)));
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

        state.accept(1, Ok(checkpoint(1, 5)));

        assert_eq!(state.checkpoint().unwrap().turn_number, 5);
        let view = state.progress();
        assert_eq!(view.phase, "complete");
        assert!(!view.checkpoint.unwrap().stale);
    }

    #[test]
    fn accept_drops_a_result_of_an_old_generation() {
        let mut state = AnalysisState::default();
        state.accept(0, Ok(checkpoint(0, 1)));
        state.invalidate();

        state.accept(0, Ok(checkpoint(0, 99)));

        assert_eq!(state.checkpoint().unwrap().turn_number, 1);
        assert!(state.progress().error.is_none());
    }

    #[test]
    fn a_failed_job_keeps_the_last_complete_checkpoint() {
        let mut state = AnalysisState::default();
        state.accept(0, Ok(checkpoint(0, 2)));

        state.accept(0, Err("the search panicked".to_string()));

        assert_eq!(state.checkpoint().unwrap().turn_number, 2);
        let view = state.progress();
        assert_eq!(view.phase, "failed");
        assert_eq!(view.error.as_deref(), Some("the search panicked"));
        assert_eq!(view.checkpoint.unwrap().turn_number, 2);
    }

    #[test]
    fn invalidate_sets_the_cancel_flag_of_the_running_job() {
        let mut state = AnalysisState::default();
        let cancel = state.start();
        assert!(!cancel.load(Ordering::Relaxed));
        assert_eq!(state.progress().phase, "running");

        state.invalidate();

        assert!(cancel.load(Ordering::Relaxed));
        assert_eq!(state.progress().phase, "idle");
    }

    #[test]
    fn a_second_start_cancels_the_first_job() {
        let mut state = AnalysisState::default();
        let first = state.start();
        let second = state.start();

        assert!(first.load(Ordering::Relaxed));
        assert!(!second.load(Ordering::Relaxed));
    }

    #[test]
    fn a_new_job_clears_the_last_error() {
        let mut state = AnalysisState::default();
        state.accept(0, Err("no usage cache".to_string()));
        assert_eq!(state.progress().phase, "failed");

        state.start();

        assert!(state.progress().error.is_none());
    }

    /// The progress endpoint is private data of the P2 side, so it must never
    /// carry an action or a win probability.
    #[test]
    fn the_progress_view_holds_no_p2_strategy() {
        let mut state = AnalysisState::default();
        state.accept(0, Ok(checkpoint(0, 4)));

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
        state.accept(0, Ok(checkpoint(0, 4)));

        let json = serde_json::to_string(&state.progress()).unwrap();

        assert!(!json.contains("turnsSimulated"), "{json}");
        assert!(!json.contains("nodes"), "{json}");
        // The two counts of the fixture, in case a later field renames them.
        assert!(!json.contains("40"), "{json}");
        assert!(!json.contains("12"), "{json}");
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
