//! Axum handlers. All state lives in `AppState`: parsed dexes (shared, immutable)
//! and a mutex-guarded session map (single-user local tool — coarse locking is fine).

use std::collections::HashMap;
use std::convert::Infallible;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::{Stream, StreamExt};
use uuid::Uuid;

use poke_rust::benchmarking;
use poke_rust::data::item::Item;
use poke_rust::information::inference::InferenceConfig;
use poke_rust::information::unknowns::{InformationMode, UnknownMatchState};
use poke_rust::simulator;
use poke_rust::state::battle::Player;

use crate::dto::*;
use crate::session::{self, BattleSession, Dexes, SessionConfig};

#[derive(Clone)]
pub struct AppState {
    pub dexes: Arc<Dexes>,
    pub sessions: Arc<Mutex<HashMap<String, BattleSession>>>,
    /// Tracker-mode sessions — a separate map from `sessions` since a tracker
    /// session has no opponent-simulating `MatchState` (see `crate::tracker`'s
    /// module doc). Keyed by its own UUID space, independent of battle ids.
    pub tracker_sessions: Arc<Mutex<HashMap<String, crate::tracker::TrackerSession>>>,
    /// On-disk sprite cache directory (gitignored) — see `get_sprite`.
    pub sprite_cache_dir: PathBuf,
    /// Shared client for the one-time upstream fetch on a cache miss.
    pub http: reqwest::Client,
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

/// Recovers from a poisoned mutex instead of propagating the poison forever. A panic
/// inside turn resolution is now caught before it can reach here (see
/// `session::resolve_turn`'s `catch_unwind`), but this is defense-in-depth against any
/// other panic while a session lock is held — one bad request must not turn every
/// future request into a 500 for the rest of the process's life. The data behind a
/// poisoned lock is still exactly as consistent as it was the instant before the panic
/// (nothing here mutates a session and then panics mid-mutation), so recovering it is safe.
fn lock_sessions(app: &AppState) -> std::sync::MutexGuard<'_, HashMap<String, BattleSession>> {
    app.sessions.lock().unwrap_or_else(|e| e.into_inner())
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

    let preview = simulator::team_preview_state_from_team_strings(
        &req.p1_team,
        &req.p2_team,
        &app.dexes.pokemon_dex,
        &app.dexes.move_dex,
        req.active_per_side,
        req.brought_per_side,
        req.stat_points,
    );

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
            let (belief_p1, belief_p2) = if information_mode == InformationMode::ClosedTeamSheet {
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

/// Runs the full turn-resolution-speed + fog-of-war-inference-speed sweep
/// (`poke_rust::benchmarking` — the unbounded grid, matching the offline
/// `cargo bench` binaries) and streams progress over Server-Sent Events,
/// ending in one `result` (or `failed`) event. Needs only `app.dexes` — no
/// `sessions` lock is taken, so a benchmark run never blocks battle requests.
/// The sweep is synchronous, CPU-bound Rust with no `.await` points of its
/// own, so it runs inside `spawn_blocking` rather than on the async runtime's
/// worker threads, where it would stall every other in-flight request for
/// the run's (now multi-minute) duration. `GET`, not `POST`: there are no
/// request knobs left to send a body for, and the browser's native
/// `EventSource` — which the frontend uses to consume this — can only issue
/// `GET` requests.
pub async fn run_benchmark(
    State(app): State<AppState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let dexes = app.dexes.clone();
    let (tx, rx) = tokio::sync::mpsc::channel::<Event>(32);

    tokio::task::spawn_blocking(move || {
        // Captures `&tx` (a shared borrow is enough — `blocking_send` only
        // needs `&self`), so `tx` is still free to send the final event below.
        let send_progress = |stage: &'static str, completed: usize, total: usize| {
            let event = Event::default()
                .event("progress")
                .json_data(BenchmarkProgressDto {
                    stage: stage.to_string(),
                    completed,
                    total,
                })
                .unwrap_or_else(|_| Event::default().event("progress"));
            let _ = tx.blocking_send(event);
        };

        let turn_speed = benchmarking::run_turn_speed(
            &dexes.pokemon_dex,
            &dexes.move_dex,
            &mut |completed, total| send_progress("turnSpeed", completed, total),
        );
        let inference = benchmarking::run_inference(
            &dexes.pokemon_dex,
            &dexes.move_dex,
            &dexes.ability_dex,
            &dexes.learnset_dex,
            &mut |completed, total| send_progress("inference", completed, total),
        );

        // Named "failed", not "error" — `EventSource` already has its own
        // built-in connection-level `error` event (a plain `Event`, not a
        // `MessageEvent` with `.data`); reusing that name here would make a
        // real server-reported failure indistinguishable from a dropped
        // connection on the client.
        let final_event = match (turn_speed, inference) {
            (Ok(ts), Ok(inf)) => Event::default().event("result").json_data(BenchmarkResponse {
                turn_speed: ts
                    .into_iter()
                    .map(|r| TurnSpeedRowDto {
                        scenario: r.scenario.to_string(),
                        mode: r.mode.to_string(),
                        rolls: r.rolls,
                        crit: r.crit,
                        avg_time_secs: r.avg_time_secs,
                        avg_branches: r.avg_branches,
                        pairings: r.pairings,
                    })
                    .collect(),
                inference: inf
                    .into_iter()
                    .map(|r| InferenceRowDto {
                        scenario: r.scenario.to_string(),
                        information_mode: r.information_mode.to_string(),
                        calls: r.calls,
                        avg_time_secs: r.avg_time_secs,
                        contradictions: r.contradictions,
                        contradiction_sample: r.contradiction_sample,
                    })
                    .collect(),
            }),
            (Err(message), _) | (_, Err(message)) => {
                Event::default().event("failed").json_data(ApiError { message })
            }
        };
        let _ = tx.blocking_send(
            final_event.unwrap_or_else(|_| Event::default().event("failed")),
        );
    });

    let stream = ReceiverStream::new(rx).map(Ok);
    Sse::new(stream).keep_alive(KeepAlive::default())
}

/// Sprites live outside the repo on GitHub (see `frontend/src/lib/sprites.ts`); nothing
/// is ever bundled here. This is a caching proxy: a disk hit serves straight from
/// `sprite_cache_dir`, a miss fetches the PNG from GitHub exactly once, writes it to
/// disk, and serves it. Only `raw.githubusercontent.com` URLs are accepted — this is
/// not a general-purpose proxy.
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
