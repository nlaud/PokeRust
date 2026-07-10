//! Axum handlers. All state lives in `AppState`: parsed dexes (shared, immutable)
//! and a mutex-guarded session map (single-user local tool — coarse locking is fine).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::extract::{Path, Query, State};
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use uuid::Uuid;

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
    }

    let information_mode = match req.information_mode.as_str() {
        "perfect" => InformationMode::PerfectInformation,
        "openSheet" => InformationMode::OpenTeamSheet,
        "openSheetNatures" => InformationMode::OpenTeamSheetNatures,
        other => return unprocessable(format!("unknown informationMode: {other}")),
    };

    // Perfect Information keeps `belief`/`inference_config` at `None` — a true
    // zero-overhead no-op that leaves ground-truth behavior byte-identical to
    // before this feature existed.
    let (belief, inference_config) = if information_mode == InformationMode::PerfectInformation {
        (None, None)
    } else {
        let config = InferenceConfig {
            use_stat_points: req.stat_points,
            learnset_dex: app.dexes.learnset_dex.clone(),
            ..InferenceConfig::default()
        };
        let belief = UnknownMatchState::team_preview_open_sheet_from_perspective(
            Player::P1,
            &preview.p1_mons,
            &preview.p2_mons,
            &app.dexes.pokemon_dex,
            req.active_per_side,
            req.brought_per_side,
            50,
            information_mode,
        );
        (Some(belief), Some(config))
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
        log: Vec::new(),
        belief,
        inference_config,
    };

    let battle_id = Uuid::new_v4().to_string();
    let view = session.view();
    app.sessions
        .lock()
        .unwrap()
        .insert(battle_id.clone(), session);

    Json(CreateBattleResponse {
        battle_id,
        state: view,
    })
    .into_response()
}

pub async fn get_battle(State(app): State<AppState>, Path(id): Path<String>) -> Response {
    let sessions = app.sessions.lock().unwrap();
    let Some(session) = sessions.get(&id) else {
        return not_found();
    };
    Json(GetBattleResponse {
        state: session.view(),
        log: session.log.clone(),
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
    let sessions = app.sessions.lock().unwrap();
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
    let mut sessions = app.sessions.lock().unwrap();
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

    let (events, probability) = session::resolve_turn(session, &app.dexes, &p1_cmd, &p2_cmd);

    Json(TurnResponse {
        state: session.view(),
        events,
        probability,
    })
    .into_response()
}

pub async fn delete_battle(State(app): State<AppState>, Path(id): Path<String>) -> Response {
    let removed = app.sessions.lock().unwrap().remove(&id).is_some();
    if removed {
        StatusCode::NO_CONTENT.into_response()
    } else {
        not_found()
    }
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
    if remote_path.is_empty() || remote_path.split('/').any(|seg| seg.is_empty() || seg == "..") {
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
        Err(_) => return error(StatusCode::BAD_GATEWAY, "failed reading sprite upstream body"),
    };

    if let Some(parent) = cache_path.parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }
    // Best-effort write: a failure to cache (e.g. disk full) shouldn't fail the
    // response — just serve the bytes we already fetched.
    let _ = tokio::fs::write(&cache_path, &bytes).await;

    sprite_bytes_response(bytes.to_vec())
}
