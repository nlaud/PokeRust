//! Axum handlers. All state lives in `AppState`: parsed dexes (shared, immutable)
//! and a mutex-guarded session map (single-user local tool — coarse locking is fine).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use uuid::Uuid;

use poke_rust::simulator;
use poke_rust::state::battle::Player;

use crate::dto::*;
use crate::session::{self, BattleSession, Dexes, SessionConfig};

#[derive(Clone)]
pub struct AppState {
    pub dexes: Arc<Dexes>,
    pub sessions: Arc<Mutex<HashMap<String, BattleSession>>>,
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

    let session = BattleSession {
        state: poke_rust::state::battle::MatchState::TeamPreviewState(preview),
        config: SessionConfig {
            active_per_side: req.active_per_side,
            brought_per_side: req.brought_per_side,
            consider_crit: req.consider_crit,
            damage_rolls: req.damage_rolls,
        },
        log: Vec::new(),
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
