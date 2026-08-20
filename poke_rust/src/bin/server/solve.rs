//! The streaming solver job of `POST /api/solve`.
//!
//! `analysis.rs` and `tracker_analysis.rs` each answer a poll. A poll gives the
//! client no event order, so the client cannot tell one answer from the next
//! one. This module streams each answer instead, over Server-Sent Events, for
//! both session kinds.
//!
//! # The three requests
//!
//! 1. `POST /api/solve` validates the session and the profile. It registers one
//!    job and returns the job ID.
//! 2. `GET /api/solve/{id}/events` runs that job and streams its answers. One
//!    job runs one time.
//! 3. `DELETE /api/solve/{id}` stops the job.
//!
//! The search starts on request 2, not on request 1. A job that no client reads
//! therefore costs no processor time.
//!
//! # The five events
//!
//! `started` names the position and the profile. Each `update` holds one
//! answer. `done`, `failed`, and `cancelled` each end the stream.
//!
//! # Generation and revision
//!
//! The generation is the position counter of the session. Every committed turn
//! raises it. A raised counter cancels each job of that session, so an answer
//! of an old position never reaches the client.
//!
//! The revision counts the updates of one job. The pair of the two numbers is
//! stable. It never repeats, and it never falls.
//!
//! # What the answers carry
//!
//! The job publishes completed strategy checkpoints while it runs. An exact
//! double-oracle search publishes after both best-response checks of each
//! round. A round answer carries `complete: false`.
//!
//! # Fog of war
//!
//! A tracker session carries the strategy of both players, because the tracker
//! user typed both rosters. A battle session carries the Player 2 strategy only
//! when its profile holds `reveal_strategy`. This is the same rule that
//! `analysis.rs` applies to the battle endpoints.

use std::cell::Cell;
use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use tokio::sync::mpsc::Sender;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::ReceiverStream;
use uuid::Uuid;

use poke_rust::information::determinize::{DeterminizeConfig, determinize_seeded};
use poke_rust::information::inference::InferenceConfig;
use poke_rust::information::unknowns::{
    UnknownBattleState, UnknownMatchState, UnknownTeamPreviewState,
};
use poke_rust::meta::{MetaDex, MetaFormat};
use poke_rust::solver::preview::{
    OpenListConfig, PreviewConfig, PreviewRound, solve_open_list_preview_progress_cancellable,
};
use poke_rust::solver::{self, CancelFlag, RootRound};
use poke_rust::state::battle::{BattleState, MatchState, Player};

use crate::bot::{BotProfile, BotSearchConfig, MAX_SAFE_INTEGER};
use crate::dto::*;
use crate::routes::AppState;
use crate::session::{Dexes, MetaDexes};
use crate::tracker_analysis::{
    PositionKind, RungResult, RungSampling, SearchInputs, TrackerStrategyRow, preview_rows,
    strategy_rows,
};

/// How many events the channel holds before a send waits.
///
/// The same size as the benchmark stream. A round answer uses a lossy send, so
/// a full channel drops that answer rather than slowing the search.
const CHANNEL_EVENTS: usize = 64;

/// Which session one job answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SolveSource {
    Battle,
    Tracker,
}

impl SolveSource {
    fn from_wire(name: &str) -> Result<Self, String> {
        match name {
            "battle" => Ok(SolveSource::Battle),
            "tracker" => Ok(SolveSource::Tracker),
            other => Err(format!("source: unknown source {other:?}")),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            SolveSource::Battle => "battle",
            SolveSource::Tracker => "tracker",
        }
    }
}

/// One registered job.
///
/// The record lives from `POST /api/solve` until the stream ends. `taken` makes
/// the stream run the search one time, so a reconnect cannot start a second
/// search and cannot repeat a revision.
pub struct SolveJob {
    source: SolveSource,
    session_id: String,
    profile: BotProfile,
    cancel: CancelFlag,
    taken: bool,
}

/// Every registered job, keyed by job ID.
pub type SolveJobs = Arc<Mutex<HashMap<String, SolveJob>>>;

/// Recovers the job map after a panic poisons its mutex.
fn lock_jobs(app: &AppState) -> std::sync::MutexGuard<'_, HashMap<String, SolveJob>> {
    app.solve_jobs.lock().unwrap_or_else(|e| e.into_inner())
}

/// Cancels and removes every job of one session.
///
/// A committed turn calls this. The position moved, so every running answer now
/// describes an old question.
pub fn cancel_jobs_for(app: &AppState, source: SolveSource, session_id: &str) {
    let mut jobs = lock_jobs(app);
    jobs.retain(|_, job| {
        let mine = job.source == source && job.session_id == session_id;
        if mine {
            job.cancel.cancel();
        }
        !mine
    });
}

fn error(status: StatusCode, message: impl Into<String>) -> Response {
    (
        status,
        Json(ApiError {
            message: message.into(),
        }),
    )
        .into_response()
}

/// `POST /api/solve` — register one job for the current position.
///
/// Returns 404 for an unknown session, and 422 for an invalid profile. A second
/// call for the same session cancels the earlier job, so one job runs at a time.
pub async fn start_solve(
    State(app): State<AppState>,
    Json(req): Json<SolveRequestDto>,
) -> Response {
    let source = match SolveSource::from_wire(&req.source) {
        Ok(source) => source,
        Err(message) => return error(StatusCode::UNPROCESSABLE_ENTITY, message),
    };

    let session_exists = match source {
        SolveSource::Battle => {
            let sessions = app.sessions.lock().unwrap_or_else(|e| e.into_inner());
            sessions.contains_key(&req.session_id)
        }
        SolveSource::Tracker => {
            let sessions = app
                .tracker_sessions
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            sessions.contains_key(&req.session_id)
        }
    };
    if !session_exists {
        return error(StatusCode::NOT_FOUND, "session not found");
    }

    let profile = match crate::bot::resolve("profile", &req.profile) {
        Ok(profile) => profile,
        Err(message) => return error(StatusCode::UNPROCESSABLE_ENTITY, message),
    };

    // One job for each session. The old answer belongs to the same position,
    // but two searches of one position only compete for the processor.
    cancel_jobs_for(&app, source, &req.session_id);

    let job_id = Uuid::new_v4().to_string();
    let simulation_turn_budget = profile.view.simulation_turn_budget;
    lock_jobs(&app).insert(
        job_id.clone(),
        SolveJob {
            source,
            session_id: req.session_id,
            profile,
            cancel: CancelFlag::with_simulation_turn_budget(simulation_turn_budget),
            taken: false,
        },
    );
    (StatusCode::CREATED, Json(SolveJobDto { job_id })).into_response()
}

/// `DELETE /api/solve/{id}` — stop one job.
///
/// The running stream then sends `cancelled` and ends.
pub async fn cancel_solve(State(app): State<AppState>, Path(id): Path<String>) -> Response {
    let mut jobs = lock_jobs(&app);
    match jobs.remove(&id) {
        Some(job) => {
            job.cancel.cancel();
            StatusCode::NO_CONTENT.into_response()
        }
        None => error(StatusCode::NOT_FOUND, "job not found"),
    }
}

/// `GET /api/solve/{id}/events` — run the job and stream its answers.
///
/// Returns 404 for an unknown job, and 409 for a job that already ran. The
/// search runs in one `spawn_blocking` task, because it is processor-bound Rust
/// with no await point of its own.
pub async fn solve_events(State(app): State<AppState>, Path(id): Path<String>) -> Response {
    let (source, session_id, profile, cancel) = {
        let mut jobs = lock_jobs(&app);
        let Some(job) = jobs.get_mut(&id) else {
            return error(StatusCode::NOT_FOUND, "job not found");
        };
        if job.taken {
            return error(
                StatusCode::CONFLICT,
                "this job already ran; start a new one with POST /api/solve",
            );
        }
        job.taken = true;
        (
            job.source,
            job.session_id.clone(),
            job.profile.clone(),
            job.cancel.clone(),
        )
    };

    let (tx, rx) = tokio::sync::mpsc::channel::<Event>(CHANNEL_EVENTS);

    // `collect_inputs` reads the generation with the position that it copies. A
    // turn can commit between the register request and this request, and the
    // generation of the registration would then name a different position.
    let inputs = match collect_inputs(&app, source, &session_id, &profile) {
        Ok(inputs) => inputs,
        Err(message) => {
            // The stream still opens, so the client reads the reason on the
            // stream rather than as a status code it cannot see.
            let _ = tx.try_send(event(
                "failed",
                SolveFailedDto {
                    job_id: id.clone(),
                    message,
                },
            ));
            lock_jobs(&app).remove(&id);
            return Sse::new(ReceiverStream::new(rx).map(Ok::<Event, Infallible>))
                .keep_alive(KeepAlive::default())
                .into_response();
        }
    };

    let _ = tx.try_send(event(
        "started",
        SolveStartedDto {
            job_id: id.clone(),
            source: source.as_str().to_string(),
            session_id,
            position: inputs.position.as_str().to_string(),
            generation: inputs.generation,
            seed: inputs.seed,
            target_depth: inputs.target_depth,
            profile: profile.view.clone(),
        },
    ));

    let jobs = Arc::clone(&app.solve_jobs);
    let job_id = id.clone();
    let progress_done = Arc::new(AtomicBool::new(false));
    let progress_done_for_task = Arc::clone(&progress_done);
    let progress_control = cancel.clone();
    let progress_tx = tx.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_millis(100));
        loop {
            interval.tick().await;
            if progress_done_for_task.load(Ordering::Acquire) {
                break;
            }
            let _ = progress_tx.try_send(event(
                "progress",
                SolveProgressDto {
                    turns_simulated: progress_control.simulation_turns(),
                    simulation_turn_budget: progress_control
                        .simulation_turn_budget()
                        .unwrap_or_default(),
                },
            ));
        }
    });
    tokio::task::spawn_blocking(move || {
        let outcome = run_job(&inputs, &tx, &job_id, &cancel);
        progress_done.store(true, Ordering::Release);
        let _ = tx.blocking_send(event(
            "progress",
            SolveProgressDto {
                turns_simulated: cancel.simulation_turns(),
                simulation_turn_budget: cancel.simulation_turn_budget().unwrap_or_default(),
            },
        ));
        let ended = if cancel.is_cancelled() {
            event(
                "cancelled",
                SolveCancelledDto {
                    job_id: job_id.clone(),
                    reason: "The request stopped the job, or the position moved.".to_string(),
                },
            )
        } else {
            match outcome {
                Ok(updates) => event(
                    "done",
                    SolveDoneDto {
                        job_id: job_id.clone(),
                        updates,
                    },
                ),
                Err(message) => event(
                    "failed",
                    SolveFailedDto {
                        job_id: job_id.clone(),
                        message,
                    },
                ),
            }
        };
        let _ = tx.blocking_send(ended);
        jobs.lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&job_id);
    });

    Sse::new(ReceiverStream::new(rx).map(Ok::<Event, Infallible>))
        .keep_alive(KeepAlive::default())
        .into_response()
}

/// Builds a named SSE event.
///
/// A serialization failure sends the name alone, so the state machine of the
/// client still advances.
fn event(name: &'static str, payload: impl Serialize) -> Event {
    Event::default()
        .event(name)
        .json_data(payload)
        .unwrap_or_else(|_| Event::default().event(name))
}

// ── The job inputs ───────────────────────────────────────────────────────────

/// Everything one job needs after the handler drops the session lock.
struct JobInputs {
    search: BotSearchConfig,
    reveal_p2: bool,
    seed: u64,
    target_depth: u8,
    generation: u64,
    /// Which question this job answers.
    position: PositionKind,
    dexes: Arc<Dexes>,
    meta: Arc<MetaDexes>,
    format: MetaFormat,
    inference: InferenceConfig,
    /// The belief of a battle position. `None` for a session with no belief.
    belief: Option<UnknownBattleState>,
    /// The concrete position. `None` for a session that draws one from a belief.
    state: Option<MatchState>,
    /// The team-preview belief. `Some` only for a team-preview position.
    preview_belief: Option<UnknownTeamPreviewState>,
}

/// Copies the session data that the job needs.
///
/// Returns the reason when the session holds no position to search.
fn collect_inputs(
    app: &AppState,
    source: SolveSource,
    session_id: &str,
    profile: &BotProfile,
) -> Result<JobInputs, String> {
    let seed = profile.view.seed.unwrap_or_else(random_seed);
    let common =
        |position, generation, inference, belief, state, preview_belief, active_per_side| {
            JobInputs {
                search: profile.search,
                reveal_p2: match source {
                    // The tracker user typed both rosters, so both strategies are
                    // theirs to read.
                    SolveSource::Tracker => true,
                    SolveSource::Battle => profile.view.reveal_strategy,
                },
                seed,
                target_depth: profile.view.depth.max(1),
                generation,
                position,
                dexes: Arc::clone(&app.dexes),
                meta: Arc::clone(&app.meta),
                format: MetaFormat::from_active_per_side(active_per_side),
                inference,
                belief,
                state,
                preview_belief,
            }
        };

    match source {
        SolveSource::Tracker => {
            let sessions = app
                .tracker_sessions
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let session = sessions.get(session_id).ok_or_else(session_gone)?;
            let preview = crate::tracker_analysis::position_is_team_preview(
                session.belief.p1_active_mons.is_empty(),
                session.belief.p2_active_mons.is_empty(),
                session.belief.turn_number,
            );
            if preview {
                let belief = session
                    .preview_belief
                    .clone()
                    .ok_or_else(|| "This session holds no team-preview belief.".to_string())?;
                return Ok(common(
                    PositionKind::TeamPreview,
                    session.analysis.generation(),
                    crate::analysis::clone_inference_config(&session.inference_config),
                    None,
                    None,
                    Some(belief),
                    session.active_per_side,
                ));
            }
            crate::tracker_analysis::leads_are_on_the_field(&session.belief)?;
            Ok(common(
                PositionKind::Battle,
                session.analysis.generation(),
                crate::analysis::clone_inference_config(&session.inference_config),
                Some(session.belief.clone()),
                // The tracker holds no concrete state, so the job draws one.
                None,
                None,
                session.active_per_side,
            ))
        }
        SolveSource::Battle => {
            let sessions = app.sessions.lock().unwrap_or_else(|e| e.into_inner());
            let session = sessions.get(session_id).ok_or_else(session_gone)?;
            match &session.state {
                MatchState::TeamPreviewState(_) => {
                    return Err(
                        "This endpoint answers a battle position. Resolve team preview \
                                first."
                            .to_string(),
                    );
                }
                MatchState::GameOverState { .. } => {
                    return Err("The battle is over, so there is nothing to choose.".to_string());
                }
                MatchState::BattleState(_) => {}
            }
            // Player 1 owns the client, so the job answers Player 1's question
            // and reads Player 1's belief.
            let belief = match &session.belief_p1 {
                Some(UnknownMatchState::Battle(belief)) => Some(belief.clone()),
                _ => None,
            };
            let inference = session
                .inference_config
                .as_ref()
                .map(crate::analysis::clone_inference_config)
                .unwrap_or_default();
            Ok(common(
                PositionKind::Battle,
                session.analysis.generation(),
                inference,
                belief,
                Some(session.state.clone()),
                None,
                session.config.active_per_side,
            ))
        }
    }
}

fn session_gone() -> String {
    "The session is gone.".to_string()
}

/// Draws the seed of one job.
///
/// The client reads this number as a JSON number, so it must survive a round
/// trip through a JavaScript double.
fn random_seed() -> u64 {
    rand::random::<u64>() & MAX_SAFE_INTEGER
}

// ── The publisher ────────────────────────────────────────────────────────────

/// One answer, before the publisher renders it.
struct Answer {
    depth: u8,
    complete: bool,
    value: f64,
    p1_win_odds: f64,
    p2_win_odds: f64,
    p1_strategy: Vec<TrackerStrategyRow>,
    p2_strategy: Vec<TrackerStrategyRow>,
    p2_is_playable: bool,
    stats: solver::SolveStats,
    sampling: Option<RungSampling>,
    warnings: Vec<String>,
}

/// Sends each answer of one job, in order.
///
/// The publisher owns the revision counter and the rate limit. It runs on the
/// search thread alone, so a `Cell` is enough for each counter.
struct Publisher<'a> {
    tx: &'a Sender<Event>,
    generation: u64,
    target_depth: u8,
    started: Instant,
    reveal_p2: bool,
    revision: Cell<u64>,
    /// The deepest complete answer that this job already sent.
    deepest: Cell<u8>,
}

impl Publisher<'_> {
    /// Builds the wire answer.
    ///
    /// The Player 2 rows leave the answer here when the profile hides them.
    /// This is the one place that reads the reveal rule.
    fn update_dto(&self, answer: Answer, revision: u64) -> SolveUpdateDto {
        SolveUpdateDto {
            generation: self.generation,
            revision,
            depth: answer.depth,
            depth_target: self.target_depth,
            elapsed_ms: self.started.elapsed().as_millis() as u64,
            complete: answer.complete,
            value: answer.value,
            p1_win_odds: answer.p1_win_odds,
            p2_win_odds: answer.p2_win_odds,
            p1_strategy: answer
                .p1_strategy
                .iter()
                .map(crate::tracker_analysis::strategy_row_dto)
                .collect(),
            p2_strategy: self.reveal_p2.then(|| {
                answer
                    .p2_strategy
                    .iter()
                    .map(crate::tracker_analysis::strategy_row_dto)
                    .collect()
            }),
            p2_strategy_is_playable: answer.p2_is_playable,
            stats: stats_dto(&answer.stats),
            sampling: answer.sampling.as_ref().map(sampling_dto),
            warnings: answer.warnings,
        }
    }

    /// Sends one answer, and returns true when it went out.
    ///
    /// Every completed checkpoint goes out in revision order.
    fn send(&self, answer: Answer) -> bool {
        if answer.complete {
            // A depth that is not deeper than the deepest complete answer would
            // step the client backwards.
            if answer.depth <= self.deepest.get() && self.deepest.get() > 0 {
                return false;
            }
            self.deepest.set(answer.depth);
        }

        let revision = self.revision.get();
        let sent = event("update", self.update_dto(answer, revision));
        let delivered = self.tx.blocking_send(sent).is_ok();
        if delivered {
            self.revision.set(revision + 1);
        }
        delivered
    }

    /// The count of answers that this job sent.
    fn updates(&self) -> u64 {
        self.revision.get()
    }
}

fn stats_dto(stats: &solver::SolveStats) -> SolveStatsDto {
    SolveStatsDto {
        nodes_expanded: stats.nodes_expanded,
        turns_simulated: stats.turns_simulated,
        matrix_cells_evaluated: stats.matrix_cells_evaluated,
        matrix_cells_total: stats.matrix_cells_total,
        lps_solved: stats.lps_solved,
        ab_cutoffs: stats.ab_cutoffs,
        tt_hits: stats.tt_hits,
        turn_cache_hits: stats.turn_cache_hits,
    }
}

fn sampling_dto(sampling: &RungSampling) -> SolveSamplingDto {
    SolveSamplingDto {
        algorithm: sampling.algorithm.to_string(),
        iterations: sampling.iterations,
        particles: sampling.particles,
        seed: sampling.seed,
        evaluator: sampling.evaluator.to_string(),
    }
}

// ── The ladder ───────────────────────────────────────────────────────────────

/// Runs the job and returns the count of answers that it sent.
fn run_job(
    inputs: &JobInputs,
    tx: &Sender<Event>,
    job_id: &str,
    cancel: &CancelFlag,
) -> Result<u64, String> {
    let publisher = Publisher {
        tx,
        generation: inputs.generation,
        target_depth: inputs.target_depth,
        started: Instant::now(),
        reveal_p2: inputs.reveal_p2,
        revision: Cell::new(0),
        deepest: Cell::new(0),
    };
    let meta = inputs
        .meta
        .for_format(inputs.format)
        .ok_or_else(crate::tracker_analysis::no_usage_cache)?;

    // A solver panic must not end the whole task without a reason, so catch it
    // here and report it as an ordinary job failure.
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match inputs.position {
        PositionKind::TeamPreview => run_preview(inputs, meta, &publisher, cancel),
        PositionKind::Battle => run_battle_ladder(inputs, meta, &publisher, cancel),
    }));
    match caught {
        Ok(Ok(())) => Ok(publisher.updates()),
        Ok(Err(message)) => Err(message),
        Err(payload) => {
            eprintln!("solve job {job_id}: the search panicked");
            Err(crate::tracker_analysis::panic_message(payload))
        }
    }
}

/// Runs the configured search and publishes each completed checkpoint.
fn run_battle_ladder(
    inputs: &JobInputs,
    meta: &MetaDex,
    publisher: &Publisher<'_>,
    cancel: &CancelFlag,
) -> Result<(), String> {
    let determinize = match &inputs.belief {
        Some(_) => crate::tracker_analysis::belief_draw_config(&inputs.inference),
        None => DeterminizeConfig::default(),
    };

    // A tracker session holds no concrete state, so the job draws one world of
    // the belief. A battle session already has the true state.
    let mut draw_warnings = 0;
    let state = match &inputs.state {
        Some(state) => state.clone(),
        None => {
            let belief = inputs
                .belief
                .as_ref()
                .ok_or_else(|| "This position has neither a state nor a belief.".to_string())?;
            let drawn = determinize_seeded(
                inputs.seed,
                belief,
                meta,
                &inputs.dexes.pokemon_dex,
                &inputs.dexes.move_dex,
                &determinize,
            )
            .map_err(crate::tracker_analysis::engine_error)?;
            draw_warnings = drawn.warnings.len();
            MatchState::BattleState(drawn.state)
        }
    };
    let battle = match &state {
        MatchState::BattleState(battle) => battle.clone(),
        _ => return Err("This endpoint answers a battle position.".to_string()),
    };

    let search_inputs = SearchInputs {
        seed: inputs.seed,
        dexes: &inputs.dexes,
        meta,
        belief: inputs.belief.as_ref(),
        determinize,
    };
    let notes = fixed_notes(inputs, draw_warnings);
    let p2_is_playable = crate::tracker_analysis::p2_strategy_is_playable(inputs.search);

    let first_depth = if matches!(inputs.search, BotSearchConfig::Exact(_)) {
        inputs.target_depth
    } else {
        inputs.search.first_depth(inputs.target_depth)
    };
    for depth in first_depth..=inputs.target_depth {
        if cancel.is_cancelled() {
            return Ok(());
        }
        let search = inputs.search.with_depth(depth);
        let round = |round: RootRound| {
            publisher.send(Answer {
                depth: round.depth,
                complete: false,
                value: round.value,
                p1_win_odds: round.value,
                p2_win_odds: 1.0 - round.value,
                p1_strategy: strategy_rows(&battle, Player::P1, &round.p1_strategy),
                p2_strategy: strategy_rows(&battle, Player::P2, &round.p2_strategy),
                p2_is_playable,
                stats: round.stats,
                sampling: None,
                warnings: notes.clone(),
            });
        };
        let sampled = |root: solver::mcts::SampledRoot| {
            publisher.send(Answer {
                depth,
                complete: false,
                value: root.value,
                p1_win_odds: root.value,
                p2_win_odds: 1.0 - root.value,
                p1_strategy: strategy_rows(&battle, Player::P1, &root.p1_strategy),
                p2_strategy: strategy_rows(&battle, Player::P2, &root.p2_strategy),
                p2_is_playable,
                stats: crate::tracker_analysis::sampled_stats(&root.stats),
                sampling: None,
                warnings: notes.clone(),
            });
        };

        let rung = crate::tracker_analysis::one_search(
            search,
            &search_inputs,
            &state,
            Some(&round),
            Some(&sampled),
            cancel,
        )?;
        // A cancelled search returns the work that finished, which is not a
        // complete rung.
        if cancel.is_cancelled() {
            return Ok(());
        }

        let mut warnings = rung.warnings.clone();
        warnings.extend(notes.iter().cloned());
        publisher.send(complete_answer(&rung, &battle, p2_is_playable, warnings));
        // The search stopped short of the depth it was asked for, so a deeper
        // rung would stop in the same place.
        if rung.depth_reached < depth || cancel.simulation_budget_hit() {
            return Ok(());
        }
    }
    Ok(())
}

/// The notes that every answer of one job carries.
fn fixed_notes(inputs: &JobInputs, draw_warnings: usize) -> Vec<String> {
    let mut notes = Vec::new();
    // A concrete session already holds the true position. Only a drawn world
    // rests on a guess of the hidden data, and only a drawn world can give
    // Player 1 a bench that the real game does not hold.
    if inputs.state.is_none()
        && let Some(belief) = inputs.belief.as_ref()
    {
        notes.push(crate::tracker_analysis::drawn_world_note(inputs.search));
        if let Some(line) = crate::tracker_analysis::unknown_bring_line(belief) {
            notes.push(line);
        }
    }
    if draw_warnings > 0 {
        notes.push(format!(
            "The determinizer reported {draw_warnings} warning(s) while it drew the opponent."
        ));
    }
    notes
}

/// Builds the answer of one complete rung.
fn complete_answer(
    rung: &RungResult,
    battle: &BattleState,
    p2_is_playable: bool,
    warnings: Vec<String>,
) -> Answer {
    Answer {
        depth: rung.depth_reached,
        complete: rung.complete,
        value: rung.p1_win_odds,
        p1_win_odds: rung.p1_win_odds,
        p2_win_odds: rung.p2_win_odds,
        p1_strategy: strategy_rows(battle, Player::P1, &rung.p1_strategy),
        p2_strategy: strategy_rows(battle, Player::P2, &rung.p2_strategy),
        p2_is_playable,
        stats: rung.stats.clone(),
        sampling: rung.sampling.clone(),
        warnings,
    }
}

/// Searches the team preview and publishes each completed strategy.
///
/// The preview search completes a lower battle depth before it starts the next
/// depth. Each completed double-oracle round publishes a partial answer. The
/// final answer uses the deepest depth that completed a round.
fn run_preview(
    inputs: &JobInputs,
    meta: &MetaDex,
    publisher: &Publisher<'_>,
    cancel: &CancelFlag,
) -> Result<(), String> {
    let belief = inputs
        .preview_belief
        .as_ref()
        .ok_or_else(|| "This position holds no team-preview belief.".to_string())?;
    let battle = crate::tracker_analysis::preview_battle_config(inputs.search);
    let depth = battle.depth;
    let config = OpenListConfig {
        preview: PreviewConfig {
            battle,
            deadline: None,
        },
        worlds: crate::tracker_analysis::preview_worlds(inputs.search),
        seed: inputs.seed,
    };
    let determinize = crate::tracker_analysis::belief_draw_config(&inputs.inference);

    let round = |round: PreviewRound| {
        publisher.send(Answer {
            depth: round.depth,
            complete: false,
            value: round.value,
            p1_win_odds: round.value,
            p2_win_odds: 1.0 - round.value,
            p1_strategy: preview_rows(&belief.p1_mons, &round.p1_strategy),
            p2_strategy: preview_rows(&belief.p2_mons, &round.p2_strategy),
            p2_is_playable: true,
            stats: solver::SolveStats {
                turns_simulated: round.stats.turns_simulated,
                matrix_cells_evaluated: round.stats.cells_evaluated,
                matrix_cells_total: round.stats.cells_total,
                lps_solved: round.stats.lps_solved,
                elapsed: round.stats.elapsed,
                ..solver::SolveStats::default()
            },
            sampling: None,
            warnings: vec![
                crate::tracker_analysis::PREVIEW_MEAN_MATRIX_NOTE.to_string(),
                crate::tracker_analysis::preview_worlds_note(config.worlds),
            ],
        });
    };
    let result = solve_open_list_preview_progress_cancellable(
        belief,
        meta,
        &inputs.dexes.pokemon_dex,
        &inputs.dexes.move_dex,
        &config,
        &determinize,
        Some(&round),
        Some(cancel),
    )
    .map_err(crate::tracker_analysis::engine_error)?;
    if cancel.is_cancelled() {
        return Ok(());
    }

    let mut warnings = crate::tracker_analysis::warning_lines(&result.warnings);
    warnings.push(crate::tracker_analysis::PREVIEW_MEAN_MATRIX_NOTE.to_string());
    warnings.push(crate::tracker_analysis::preview_worlds_note(
        result.sampling.worlds,
    ));
    if let Some(line) = crate::tracker_analysis::sampling_error_line(
        result.sampling.worlds,
        result.sampling.standard_error,
    ) {
        warnings.push(line);
    }
    if !result.draw_warnings.is_empty() {
        warnings.push(format!(
            "The determinizer reported {} warning(s) while it drew the opponent.",
            result.draw_warnings.len()
        ));
    }

    publisher.send(Answer {
        depth: result.depth_reached,
        // One rule for every endpoint. This test used to omit
        // `BudgetExhausted`, so a preview answer that the node budget cut short
        // reached the client marked complete.
        complete: result.depth_reached == depth
            && solver::warnings_are_complete(&result.warnings),
        value: result.p1_win_odds,
        p1_win_odds: result.p1_win_odds,
        p2_win_odds: result.p2_win_odds,
        p1_strategy: preview_rows(&belief.p1_mons, &result.p1_strategy),
        p2_strategy: preview_rows(&belief.p2_mons, &result.p2_strategy),
        // Each row is one bring-and-lead choice of the mean matrix, so the row
        // list is one strategy.
        p2_is_playable: true,
        stats: solver::SolveStats {
            turns_simulated: result.stats.turns_simulated,
            matrix_cells_evaluated: result.stats.cells_evaluated,
            matrix_cells_total: result.stats.cells_total,
            lps_solved: result.stats.lps_solved,
            elapsed: result.stats.elapsed,
            ..solver::SolveStats::default()
        },
        sampling: None,
        warnings,
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use poke_rust::solver::SolveStats;

    fn publisher(tx: &Sender<Event>, reveal_p2: bool) -> Publisher<'_> {
        Publisher {
            tx,
            generation: 3,
            target_depth: 2,
            started: Instant::now(),
            reveal_p2,
            revision: Cell::new(0),
            deepest: Cell::new(0),
        }
    }

    fn answer(depth: u8, complete: bool) -> Answer {
        Answer {
            depth,
            complete,
            value: 0.62,
            p1_win_odds: 0.62,
            p2_win_odds: 0.38,
            p1_strategy: Vec::new(),
            p2_strategy: Vec::new(),
            p2_is_playable: true,
            stats: SolveStats::default(),
            sampling: None,
            warnings: Vec::new(),
        }
    }

    /// The count of events that the channel holds.
    fn delivered(rx: &mut tokio::sync::mpsc::Receiver<Event>) -> usize {
        let mut count = 0;
        while rx.try_recv().is_ok() {
            count += 1;
        }
        count
    }

    /// The revision must rise by one for each answer that goes out, so the
    /// client can order two answers that arrive together.
    #[test]
    fn the_revision_rises_by_one_for_each_answer() {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<Event>(16);
        let publisher = publisher(&tx, true);

        assert!(publisher.send(answer(1, true)));
        assert!(publisher.send(answer(2, true)));

        assert_eq!(publisher.updates(), 2);
        assert_eq!(delivered(&mut rx), 2);

        // The wire answer carries the number that the counter held.
        let json = serde_json::to_string(&publisher.update_dto(answer(2, true), 7)).unwrap();
        assert!(json.contains("\"revision\":7"), "{json}");
        assert!(json.contains("\"generation\":3"), "{json}");
        assert!(json.contains("\"depthTarget\":2"), "{json}");
    }

    /// The stream sends each completed checkpoint.
    #[test]
    fn each_checkpoint_is_sent() {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<Event>(16);
        let publisher = publisher(&tx, true);

        assert!(publisher.send(answer(1, false)));
        assert!(publisher.send(answer(1, false)));
        assert!(publisher.send(answer(1, true)));

        assert_eq!(publisher.updates(), 3);
        assert_eq!(delivered(&mut rx), 3);
    }

    /// A depth that is not deeper than the deepest complete answer would step
    /// the panel backwards.
    #[test]
    fn a_shallower_complete_answer_is_dropped() {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<Event>(16);
        let publisher = publisher(&tx, true);

        assert!(publisher.send(answer(3, true)));
        assert!(!publisher.send(answer(2, true)));
        assert!(!publisher.send(answer(3, true)));
        assert!(publisher.send(answer(4, true)));

        assert_eq!(publisher.updates(), 2);
        assert_eq!(delivered(&mut rx), 2);
    }

    /// A battle profile without `revealStrategy` must send no Player 2 row. A
    /// tracker job sets the flag, so its rows always go out.
    #[test]
    fn a_hidden_profile_sends_no_player_two_strategy() {
        let (tx, _rx) = tokio::sync::mpsc::channel::<Event>(16);

        let hidden = publisher(&tx, false);
        let json = serde_json::to_string(&hidden.update_dto(answer(1, true), 0)).unwrap();
        assert!(json.contains("\"p2Strategy\":null"), "{json}");
        // The win odds are not a strategy, and the battle view already shows
        // them, so they stay.
        assert!(json.contains("\"p2WinOdds\":0.38"), "{json}");

        let shown = publisher(&tx, true);
        let json = serde_json::to_string(&shown.update_dto(answer(1, true), 0)).unwrap();
        assert!(json.contains("\"p2Strategy\":[]"), "{json}");
    }

    /// The wire names of the two sources must round trip, and an unknown name
    /// must fail rather than fall back to one of them.
    #[test]
    fn the_source_name_round_trips() {
        for source in [SolveSource::Battle, SolveSource::Tracker] {
            assert_eq!(SolveSource::from_wire(source.as_str()).unwrap(), source);
        }
        assert!(SolveSource::from_wire("other").is_err());
    }
}
