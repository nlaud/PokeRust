//! Defines the Axum handlers.
//! `AppState` stores shared dexes and a mutex-protected session map.

use std::collections::HashMap;
use std::convert::Infallible;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::{Stream, StreamExt};
use uuid::Uuid;

use poke_rust::benchmarking;
use poke_rust::data::item::Item;
use poke_rust::information::inference::InferenceConfig;
use poke_rust::information::unknowns::{InformationMode, UnknownMatchState};
use poke_rust::meta::MetaFormat;
use poke_rust::simulator;
use poke_rust::state::battle::{BattleMechanics, Player};

use crate::dto::*;
use crate::session::{self, BattleSession, Dexes, MetaDexes, SessionConfig};

#[derive(Clone)]
pub struct AppState {
    pub dexes: Arc<Dexes>,
    /// Optional usage caches for generated teams.
    pub meta: Arc<MetaDexes>,
    pub sessions: Arc<Mutex<HashMap<String, BattleSession>>>,
    /// Tracker sessions, keyed by tracker UUID.
    pub tracker_sessions: Arc<Mutex<HashMap<String, crate::tracker::TrackerSession>>>,
    /// On-disk sprite cache directory (gitignored) — see `get_sprite`.
    pub sprite_cache_dir: PathBuf,
    /// Shared client for the one-time upstream fetch on a cache miss.
    pub http: reqwest::Client,
    /// True while one benchmark run is active.
    /// Closing the client stream does not stop a blocking sweep.
    pub benchmark_running: Arc<AtomicBool>,
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

fn unprocessable(message: impl Into<String>) -> Response {
    error(StatusCode::UNPROCESSABLE_ENTITY, message)
}

fn not_found() -> Response {
    error(StatusCode::NOT_FOUND, "battle not found")
}

fn internal_error(message: impl Into<String>) -> Response {
    error(StatusCode::INTERNAL_SERVER_ERROR, message)
}

/// Recovers the session map after a panic poisons its mutex.
/// Session changes are atomic, so the stored data remains consistent.
fn lock_sessions(app: &AppState) -> std::sync::MutexGuard<'_, HashMap<String, BattleSession>> {
    app.sessions.lock().unwrap_or_else(|e| e.into_inner())
}

/// Returns pasted teamsheet text or generates a teamsheet.
/// Generated teams use the same parser and validation as pasted teams.
fn resolve_team_text(
    label: &str,
    mode: &str,
    sheet: &str,
    app: &AppState,
    format: MetaFormat,
    brought_per_side: u8,
    seed: u64,
) -> Result<String, String> {
    if mode != "meta" {
        return Ok(sheet.to_string());
    }
    let Some(meta_dex) = app.meta.for_format(format) else {
        return Err(format!(
            "{label}: meta team requested but usage data is unavailable for this format \
             (see meta_scraper/README.md to populate it)"
        ));
    };
    let team = poke_rust::meta::generate_meta_team(
        meta_dex,
        &app.dexes.pokemon_dex,
        &app.dexes.learnset_dex,
        brought_per_side as usize,
        seed,
    )
    .map_err(|e| format!("{label}: {e}"))?;
    Ok(poke_rust::meta::render_teamsheet(&team))
}

pub async fn create_battle(
    State(app): State<AppState>,
    Json(req): Json<CreateBattleRequest>,
) -> Response {
    if req.active_per_side == 0 || req.brought_per_side < req.active_per_side {
        return unprocessable("activePerSide must be >= 1 and <= broughtPerSide");
    }
    if !(1..=16).contains(&req.damage_rolls) {
        return unprocessable("damageRolls must be between 1 and 16");
    }

    // `req.legal_items` is the selected format's resolved catalog-minus-banned slug
    // list (see `CreateBattleRequest::legal_items`'s doc comment); empty means no
    // restriction. Reject up front on an unresolvable slug (stale frontend catalog,
    // typo) rather than letting it surface later as a confusing mid-battle
    // `inference_contradiction!` panic the first time that "item" would be revealed.
    let legal_items: Option<std::collections::HashSet<Item>> = if req.legal_items.is_empty() {
        None
    } else {
        let mut set = std::collections::HashSet::with_capacity(req.legal_items.len());
        for slug in &req.legal_items {
            let item = Item::from_str(slug);
            if matches!(item, Item::Unknown(_)) {
                return unprocessable(format!("legalItems: unrecognized item {slug:?}"));
            }
            set.insert(item);
        }
        Some(set)
    };

    // Synthesize a teamsheet from usage data for any side whose `TeamMode` asks
    // for one, then fall through the exact same parse path a pasted team takes —
    // see `resolve_team_text`'s doc comment.
    let format = MetaFormat::from_active_per_side(req.active_per_side);
    let meta_seed = req.meta_seed.unwrap_or_else(rand::random::<u64>);
    let p1_team = match resolve_team_text(
        "p1Team",
        &req.p1_team_mode,
        &req.p1_team,
        &app,
        format,
        req.brought_per_side,
        meta_seed,
    ) {
        Ok(text) => text,
        Err(msg) => return unprocessable(msg),
    };
    let p2_team = match resolve_team_text(
        "p2Team",
        &req.p2_team_mode,
        &req.p2_team,
        &app,
        format,
        req.brought_per_side,
        // Distinct from p1's seed so two "meta" sides in one request don't
        // draw the same team; `StdRng` decorrelates adjacent seeds fine, no
        // fancier mixing needed.
        meta_seed.wrapping_add(1),
    ) {
        Ok(text) => text,
        Err(msg) => return unprocessable(msg),
    };

    let mut preview = simulator::team_preview_state_from_team_strings(
        &p1_team,
        &p2_team,
        &app.dexes.pokemon_dex,
        &app.dexes.move_dex,
        req.active_per_side,
        req.brought_per_side,
        req.stat_points,
    );
    preview.mechanics = BattleMechanics {
        tera_enabled: req.tera_enabled,
        mega_enabled: req.mega_enabled,
    };

    for (label, mons) in [("p1Team", &preview.p1_mons), ("p2Team", &preview.p2_mons)] {
        if mons.is_empty() {
            return unprocessable(format!("{}: no valid Pokemon parsed from teamsheet", label));
        }
        if mons.len() < req.brought_per_side as usize {
            return unprocessable(format!(
                "{}: team has {} Pokemon but the format brings {}",
                label,
                mons.len(),
                req.brought_per_side
            ));
        }
        // Reject up front rather than letting the first in-battle reveal of this
        // item panic deep inside `apply_information` (`inference_contradiction!` —
        // see `EventKind::ItemRevealed`'s legal-whitelist check). A team's own held
        // item is seeded as `Known` from turn 0, but the SAME unconditional check
        // also runs when a later event re-confirms it (e.g. eating a held Berry),
        // so an out-of-format item on your OWN team is just as fatal there as an
        // opponent's — validating here turns that into a clean 422 instead.
        if let Some(legal) = &legal_items {
            for mon in mons.iter() {
                if mon.item != Item::None && !legal.contains(&mon.item) {
                    return unprocessable(format!(
                        "{}: {:?} holds {:?}, which is not legal in this format",
                        label, mon.species, mon.item
                    ));
                }
            }
        }
    }

    let information_mode = match req.information_mode.as_str() {
        "perfect" => InformationMode::PerfectInformation,
        "closedSheet" => InformationMode::ClosedTeamSheet,
        "openSheet" => InformationMode::OpenTeamSheet,
        "openSheetNatures" => InformationMode::OpenTeamSheetNatures,
        other => return unprocessable(format!("unknown informationMode: {other}")),
    };

    // Perfect Information keeps both beliefs/`inference_config` at `None` — a true
    // zero-overhead no-op that leaves ground-truth behavior byte-identical to
    // before this feature existed.
    let (belief_p1, belief_p2, inference_config) =
        if information_mode == InformationMode::PerfectInformation {
            (None, None, None)
        } else {
            let config = InferenceConfig {
                use_stat_points: req.stat_points,
                force_max_ivs: req.force_max_ivs,
                legal_items,
                learnset_dex: app.dexes.learnset_dex.clone(),
                ..InferenceConfig::default()
            };
            let (mut belief_p1, mut belief_p2) =
                if information_mode == InformationMode::ClosedTeamSheet {
                    let belief_p1 = UnknownMatchState::team_preview_closed_sheet_from_perspective(
                        Player::P1,
                        &preview.p1_mons,
                        &preview.p2_mons,
                        &app.dexes.pokemon_dex,
                        req.active_per_side,
                        req.brought_per_side,
                        50,
                        req.force_max_ivs,
                    );
                    // P2's mirror-image belief: viewer=P2, so P2's own team is the known side
                    // and P1's the fogged one — note the my/opp argument swap vs. belief_p1.
                    let belief_p2 = UnknownMatchState::team_preview_closed_sheet_from_perspective(
                        Player::P2,
                        &preview.p2_mons,
                        &preview.p1_mons,
                        &app.dexes.pokemon_dex,
                        req.active_per_side,
                        req.brought_per_side,
                        50,
                        req.force_max_ivs,
                    );
                    (belief_p1, belief_p2)
                } else {
                    let belief_p1 = UnknownMatchState::team_preview_open_sheet_from_perspective(
                        Player::P1,
                        &preview.p1_mons,
                        &preview.p2_mons,
                        &app.dexes.pokemon_dex,
                        req.active_per_side,
                        req.brought_per_side,
                        50,
                        information_mode,
                        req.force_max_ivs,
                    );
                    // P2's mirror-image belief: viewer=P2, so P2's own team is the known side
                    // and P1's the fogged one — note the my/opp argument swap vs. belief_p1.
                    let belief_p2 = UnknownMatchState::team_preview_open_sheet_from_perspective(
                        Player::P2,
                        &preview.p2_mons,
                        &preview.p1_mons,
                        &app.dexes.pokemon_dex,
                        req.active_per_side,
                        req.brought_per_side,
                        50,
                        information_mode,
                        req.force_max_ivs,
                    );
                    (belief_p1, belief_p2)
                };
            for belief in [&mut belief_p1, &mut belief_p2] {
                let UnknownMatchState::TeamPreview(fog_preview) = belief else {
                    unreachable!("team-preview belief constructor returned a battle state");
                };
                fog_preview.mechanics = preview.mechanics;
            }
            (Some(belief_p1), Some(belief_p2), Some(config))
        };

    let session = BattleSession {
        state: poke_rust::state::battle::MatchState::TeamPreviewState(preview),
        config: SessionConfig {
            active_per_side: req.active_per_side,
            brought_per_side: req.brought_per_side,
            consider_crit: req.consider_crit,
            damage_rolls: req.damage_rolls,
            information_mode,
        },
        log_p1: Vec::new(),
        log_p2: Vec::new(),
        belief_p1,
        belief_p2,
        inference_config,
    };

    let battle_id = Uuid::new_v4().to_string();
    let view = session.view(Player::P1);
    let view_p2 = session.view(Player::P2);
    lock_sessions(&app).insert(battle_id.clone(), session);

    Json(CreateBattleResponse {
        battle_id,
        state: view,
        state_p2: view_p2,
    })
    .into_response()
}

pub async fn get_battle(State(app): State<AppState>, Path(id): Path<String>) -> Response {
    let sessions = lock_sessions(&app);
    let Some(session) = sessions.get(&id) else {
        return not_found();
    };
    Json(GetBattleResponse {
        state: session.view(Player::P1),
        state_p2: session.view(Player::P2),
        log: session.log_p1.clone(),
        log_p2: session.log_p2.clone(),
    })
    .into_response()
}

#[derive(Deserialize)]
pub struct CommandsQuery {
    player: PlayerDto,
}

pub async fn get_commands(
    State(app): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<CommandsQuery>,
) -> Response {
    let sessions = lock_sessions(&app);
    let Some(session) = sessions.get(&id) else {
        return not_found();
    };
    let player = match query.player {
        PlayerDto::P1 => Player::P1,
        PlayerDto::P2 => Player::P2,
    };
    Json(session::legal_commands(session, &app.dexes, player)).into_response()
}

pub async fn submit_turn(
    State(app): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<TurnRequest>,
) -> Response {
    let mut sessions = lock_sessions(&app);
    let Some(session) = sessions.get_mut(&id) else {
        return not_found();
    };

    let p1_cmd = match session::reconstruct_player_command(session, &app.dexes, Player::P1, &req.p1)
    {
        Ok(cmd) => cmd,
        Err(message) => return unprocessable(message),
    };
    let p2_cmd = match session::reconstruct_player_command(session, &app.dexes, Player::P2, &req.p2)
    {
        Ok(cmd) => cmd,
        Err(message) => return unprocessable(message),
    };

    // A contradiction in the fog-of-war inference engine is caught inside
    // `resolve_turn` and surfaced here as an ordinary error response — the session
    // (both ground-truth state and belief) is left untouched on failure, so the
    // battle is still there to retry or continue against on the next request. See
    // `resolve_turn`'s doc comment for the full atomicity argument.
    let (events, events_p2, probability) =
        match session::resolve_turn(session, &app.dexes, &p1_cmd, &p2_cmd) {
            Ok(result) => result,
            Err(message) => return internal_error(message),
        };

    Json(TurnResponse {
        state: session.view(Player::P1),
        state_p2: session.view(Player::P2),
        events,
        events_p2,
        probability,
    })
    .into_response()
}

pub async fn delete_battle(State(app): State<AppState>, Path(id): Path<String>) -> Response {
    let removed = lock_sessions(&app).remove(&id).is_some();
    if removed {
        StatusCode::NO_CONTENT.into_response()
    } else {
        not_found()
    }
}

/// `GET /api/dex/species` — every teamsheet-legal species, alphabetically, as
/// display names.
///
/// Session-free by necessity: this backs the tracker setup form's opponent
/// picker, which runs before any session exists, so
/// `GET /api/tracker/{id}/completions` (roster-scoped, and the right answer for
/// the in-battle input bar) can't serve it. Mirrors that handler's humanization
/// via `humanize_identifier`, so the names it returns round-trip back through
/// `Species::from_str` when the picker submits them as a comma-separated list.
///
/// Three families of forme are excluded, because none of them can be written on
/// a sheet: `battleOnly` formes (Ash-Greninja, Palafin-Hero, Aegislash-Blade —
/// they only exist mid-battle), Mega formes (reached by holding the stone, so
/// the sheet names the base species), and Gigantamax formes. Alternate formes
/// that ARE sheet-legal stay in, including the item-bound ones — Arceus-Dragon,
/// Silvally-Steel and Genesect-Douse all carry a `requiredItem` too, which is
/// why that field is deliberately NOT the filter.
pub async fn get_species_list(State(app): State<AppState>) -> Response {
    let mut species: Vec<String> = app
        .dexes
        .pokemon_dex
        .iter()
        .filter(|(key, data)| {
            data.battle_only.is_none()
                && !poke_rust::state::pokemon::is_mega_dex_entry(key, data)
                && !is_gigantamax_dex_entry(key)
        })
        .map(|(key, _)| poke_rust::user::humanize_identifier(format!("{key:?}")))
        .collect();
    species.sort();
    species.dedup();
    Json(SpeciesListDto { species }).into_response()
}

/// Gigantamax formes carry neither `battleOnly` nor `requiredItem` in the dex —
/// only `forme: "Gmax"` and a `changesFrom` back-pointer the parser doesn't
/// read — so they're identified by the enum variant's own name, the same way
/// `is_mega_dex_entry` falls back to for Megas.
fn is_gigantamax_dex_entry(species_key: &poke_rust::data::species::Species) -> bool {
    format!("{species_key:?}").to_lowercase().ends_with("gmax")
}

/// Keep one panicking benchmark sweep from aborting the later sequential
/// sweeps. Panics become ordinary per-sweep failures so the SSE contract still
/// delivers exactly one `result` or `failed` event for every card.
fn catch_benchmark_panic<T>(run: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(run)) {
        Ok(outcome) => outcome,
        Err(payload) => {
            let message = if let Some(message) = payload.downcast_ref::<String>() {
                message.clone()
            } else if let Some(message) = payload.downcast_ref::<&str>() {
                (*message).to_string()
            } else {
                "non-string panic payload".to_string()
            };
            Err(format!("benchmark sweep panicked: {message}"))
        }
    }
}

/// Runs all three benchmark sweeps — turn resolution, fog-of-war inference and
/// the game-tree solver (`poke_rust::benchmarking`, the unbounded grid each
/// offline `cargo bench` binary runs) — and streams them over Server-Sent
/// Events.
///
/// **The sweeps run one at a time, deliberately.** Running them concurrently
/// finishes sooner, but three CPU-bound sweeps sharing a machine — on a hybrid
/// CPU, across different core types — report times that no longer reproduce
/// `poke_rust/benches/RESULTS.md`. A benchmark whose numbers cannot be compared
/// to the recorded ones is not worth the wall-clock it saves. Sequential keeps
/// this endpoint's output directly comparable to `cargo bench`.
///
/// Streaming is what makes that affordable to watch. Everything runs in one
/// `spawn_blocking` task, but each sweep emits its own tagged `progress` events
/// and its own `result` the instant it finishes, so the page renders each chart
/// as it lands instead of waiting on the solver sweep at the end. A sweep that
/// fails emits `failed` and does not stop the ones after it; `done` is the one
/// event that ends the stream.
///
/// Needs only `app.dexes` — no `sessions` lock is taken, so a benchmark run
/// never blocks battle requests. The sweeps are synchronous, CPU-bound Rust with
/// no `.await` points of their own, hence `spawn_blocking` rather than the async
/// runtime's worker threads, where they would stall every other in-flight
/// request for the run's duration. `GET`, not `POST`: there are no request knobs
/// to send a body for, and the browser's native `EventSource` — which the
/// frontend uses to consume this — can only issue `GET`.
pub async fn run_benchmark(
    State(app): State<AppState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    // Room for every sweep to have progress in flight without any of them
    // blocking on a browser that is slow to drain.
    let (tx, rx) = tokio::sync::mpsc::channel::<Event>(64);

    // One run at a time. The sweeps cannot be cancelled once started, so
    // without this a reload mid-run would double the CPU-bound work and make
    // every reported time meaningless.
    if app
        .benchmark_running
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        for sweep in [
            BenchmarkSweepDto::TurnSpeed,
            BenchmarkSweepDto::Inference,
            BenchmarkSweepDto::Solver,
        ] {
            let _ = tx.try_send(
                Event::default()
                    .event("failed")
                    .json_data(BenchmarkSweepErrorDto {
                        sweep,
                        message: "A benchmark is already running; wait for it to finish."
                            .to_string(),
                    })
                    .unwrap_or_else(|_| Event::default().event("failed")),
            );
        }
        let _ = tx.try_send(Event::default().event("done").data("{}"));
        return Sse::new(ReceiverStream::new(rx).map(Ok)).keep_alive(KeepAlive::default());
    }

    /// Build a named SSE event, degrading to a payload-less event of the same
    /// name if serialization somehow fails, so the client's state machine still
    /// advances rather than hanging on a sweep that never reports.
    fn event(name: &'static str, payload: impl Serialize) -> Event {
        Event::default()
            .event(name)
            .json_data(payload)
            .unwrap_or_else(|_| Event::default().event(name))
    }

    /// One sweep's plumbing: stream tagged progress, then emit exactly one
    /// `result` or `failed`. Returns once the sweep has reported.
    fn report<T>(
        tx: &tokio::sync::mpsc::Sender<Event>,
        sweep: BenchmarkSweepDto,
        outcome: Result<T, String>,
        into_result: impl FnOnce(T) -> BenchmarkResultDto,
    ) {
        // Named "failed", not "error" — `EventSource` has its own built-in
        // connection-level `error` event (a plain `Event`, not a `MessageEvent`
        // with `.data`); reusing that name would make a server-reported failure
        // indistinguishable from a dropped connection.
        let reported = match outcome {
            Ok(rows) => event("result", into_result(rows)),
            Err(message) => event("failed", BenchmarkSweepErrorDto { sweep, message }),
        };
        let _ = tx.blocking_send(reported);
    }

    // `try_send`, not `blocking_send`: progress is lossy by nature — only the
    // latest value means anything — and these calls sit on the benchmark's own
    // worker thread. Blocking one on a browser that is slow to drain would
    // inflate the very wall-clock the sweep is there to measure. `result` and
    // `failed` still block, since losing one would strand a chart forever.
    let progress_sender = |tx: tokio::sync::mpsc::Sender<Event>, sweep: BenchmarkSweepDto| {
        move |completed: usize, total: usize| {
            let _ = tx.try_send(event(
                "progress",
                BenchmarkProgressDto {
                    stage: sweep,
                    completed,
                    total,
                },
            ));
        }
    };

    // Held outside the sweep task so `done` is still sent if that task panics —
    // otherwise a panic would leave every chart stuck on its skeleton forever.
    let done_tx = tx.clone();

    // One task, three sweeps in sequence — cheapest sweep first, so the page has
    // something on screen early and the multi-minute solver sweep lands last.
    let dexes = app.dexes.clone();
    let running = app.benchmark_running.clone();
    let sweeps = tokio::task::spawn_blocking(move || {
        let mut on_progress = progress_sender(tx.clone(), BenchmarkSweepDto::TurnSpeed);
        let rows = catch_benchmark_panic(|| {
            benchmarking::run_turn_speed(&dexes.pokemon_dex, &dexes.move_dex, &mut on_progress)
        });
        report(&tx, BenchmarkSweepDto::TurnSpeed, rows, |rows| {
            BenchmarkResultDto::TurnSpeed {
                rows: rows.into_iter().map(turn_speed_row_dto).collect(),
            }
        });

        let mut on_progress = progress_sender(tx.clone(), BenchmarkSweepDto::Inference);
        let rows = catch_benchmark_panic(|| {
            benchmarking::run_inference(
                &dexes.pokemon_dex,
                &dexes.move_dex,
                &dexes.ability_dex,
                &dexes.learnset_dex,
                &mut on_progress,
            )
        });
        report(&tx, BenchmarkSweepDto::Inference, rows, |rows| {
            BenchmarkResultDto::Inference {
                rows: rows.into_iter().map(inference_row_dto).collect(),
            }
        });

        let mut on_progress = progress_sender(tx.clone(), BenchmarkSweepDto::Solver);
        let rows = catch_benchmark_panic(|| {
            benchmarking::run_solver(&dexes.pokemon_dex, &dexes.move_dex, &mut on_progress)
        });
        report(&tx, BenchmarkSweepDto::Solver, rows, |rows| {
            BenchmarkResultDto::Solver {
                rows: rows.into_iter().map(solver_row_dto).collect(),
            }
        });
    });

    // `done` is what ends the stream. Sent even if the task panicked, so a
    // client is never left waiting on a sweep that will never report — and the
    // in-flight flag is cleared on that same path, so a panic cannot wedge the
    // endpoint into permanently refusing new runs.
    tokio::spawn(async move {
        let _ = sweeps.await;
        running.store(false, Ordering::SeqCst);
        let _ = done_tx
            .send(Event::default().event("done").data("{}"))
            .await;
    });

    let stream = ReceiverStream::new(rx).map(Ok);
    Sse::new(stream).keep_alive(KeepAlive::default())
}

fn turn_speed_row_dto(row: benchmarking::TurnSpeedRow) -> TurnSpeedRowDto {
    TurnSpeedRowDto {
        scenario: row.scenario.to_string(),
        mode: row.mode.to_string(),
        rolls: row.rolls,
        crit: row.crit,
        avg_time_secs: row.avg_time_secs,
        avg_branches: row.avg_branches,
        pairings: row.pairings,
    }
}

fn inference_row_dto(row: benchmarking::InferenceRow) -> InferenceRowDto {
    InferenceRowDto {
        scenario: row.scenario.to_string(),
        information_mode: row.information_mode.to_string(),
        calls: row.calls,
        avg_time_secs: row.avg_time_secs,
        contradictions: row.contradictions,
        contradiction_sample: row.contradiction_sample,
    }
}

fn solver_row_dto(row: benchmarking::SolverRow) -> SolverRowDto {
    SolverRowDto {
        scenario: row.scenario.to_string(),
        algorithm: row.algorithm.to_string(),
        depth: row.depth,
        rolls: row.rolls,
        chance: row.chance.to_string(),
        action_cap: row.action_cap,
        avg_time_secs: row.avg_time_secs,
        avg_nodes: row.avg_nodes,
        avg_turns_simulated: row.avg_turns_simulated,
        avg_cells_evaluated: row.avg_cells_evaluated,
        avg_cells_total: row.avg_cells_total,
        avg_lps: row.avg_lps,
        pairings: row.pairings,
        skipped: row.skipped.map(str::to_string),
    }
}

/// Serves sprites through a local disk cache.
/// A cache miss downloads one PNG from GitHub.
/// Accepts only `raw.githubusercontent.com` URLs.
const ALLOWED_SPRITE_HOST_PREFIX: &str = "https://raw.githubusercontent.com/";

#[derive(Deserialize)]
pub struct SpriteQuery {
    url: String,
}

fn sprite_bytes_response(bytes: Vec<u8>) -> Response {
    (
        StatusCode::OK,
        [
            (CONTENT_TYPE, "image/png"),
            // Sprite bytes are content-addressed by the upstream URL and never change
            // once cached, so both the browser and any intermediary can cache forever.
            (CACHE_CONTROL, "public, max-age=31536000, immutable"),
        ],
        bytes,
    )
        .into_response()
}

pub async fn get_sprite(State(app): State<AppState>, Query(query): Query<SpriteQuery>) -> Response {
    let url = query.url;
    if !url.starts_with(ALLOWED_SPRITE_HOST_PREFIX) {
        return error(
            StatusCode::FORBIDDEN,
            "only raw.githubusercontent.com sprite URLs are proxied",
        );
    }
    let remote_path = &url[ALLOWED_SPRITE_HOST_PREFIX.len()..];
    // Reject path traversal / empty segments (e.g. "..", "a//b") before ever joining
    // this onto a filesystem path.
    if remote_path.is_empty()
        || remote_path
            .split('/')
            .any(|seg| seg.is_empty() || seg == "..")
    {
        return error(StatusCode::BAD_REQUEST, "invalid sprite path");
    }

    let cache_path = app.sprite_cache_dir.join(remote_path);

    if let Ok(bytes) = tokio::fs::read(&cache_path).await {
        return sprite_bytes_response(bytes);
    }

    let upstream = match app.http.get(&url).send().await {
        Ok(resp) => resp,
        Err(_) => return error(StatusCode::BAD_GATEWAY, "failed to reach sprite upstream"),
    };
    if !upstream.status().is_success() {
        // e.g. a genuine 404 for a species/form GitHub doesn't have — don't cache
        // this, let the frontend's own fallback chain (see sprites.ts) handle it.
        return upstream.status().into_response();
    }
    let bytes = match upstream.bytes().await {
        Ok(b) => b,
        Err(_) => {
            return error(
                StatusCode::BAD_GATEWAY,
                "failed reading sprite upstream body",
            );
        }
    };

    if let Some(parent) = cache_path.parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }
    // Best-effort write: a failure to cache (e.g. disk full) shouldn't fail the
    // response — just serve the bytes we already fetched.
    let _ = tokio::fs::write(&cache_path, &bytes).await;

    sprite_bytes_response(bytes.to_vec())
}

#[cfg(test)]
mod tests {
    use super::catch_benchmark_panic;

    #[test]
    fn benchmark_panics_become_sweep_failures() {
        let result = catch_benchmark_panic::<()>(|| panic!("synthetic sweep failure"));
        assert_eq!(
            result,
            Err("benchmark sweep panicked: synthetic sweep failure".to_string())
        );
    }
}
