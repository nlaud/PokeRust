//! Defines the Axum handlers.
//! `AppState` stores shared dexes and a mutex-protected session map.

use std::collections::{HashMap, HashSet};
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
use poke_rust::data::pokemon_move::PokemonMove;
use poke_rust::data::species::Species;
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
    /// Every registered streaming solver job — see `solve.rs`.
    pub solve_jobs: crate::solve::SolveJobs,
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
///
/// `roster_size` is the roster of the format, not the brought count. A meta
/// side must own a full roster, because team preview then picks the brought
/// Pokemon out of it. A roster equal to the brought count removes that choice
/// and leaves the side with no reserve.
fn resolve_team_text(
    label: &str,
    mode: &str,
    sheet: &str,
    app: &AppState,
    format: MetaFormat,
    roster_size: u8,
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
        roster_size as usize,
        seed,
    )
    .map_err(|e| format!("{label}: {e}"))?;
    Ok(poke_rust::meta::render_teamsheet(&team))
}

/// Checks the three per-side counts of a create-battle request.
/// Returns the error message of the first broken rule.
///
/// A roster below the brought count cannot fill team preview. A generated team
/// would then fail the roster-length check in `create_battle`, and that message
/// blames the generator instead of the counts.
fn side_count_error(
    active_per_side: u8,
    brought_per_side: u8,
    total_per_side: u8,
) -> Option<&'static str> {
    if active_per_side == 0 || brought_per_side < active_per_side {
        return Some("activePerSide must be >= 1 and <= broughtPerSide");
    }
    if total_per_side < brought_per_side {
        return Some("totalPerSide must be >= broughtPerSide");
    }
    if total_per_side > 6 {
        return Some("totalPerSide must be <= 6");
    }
    None
}

pub(crate) fn learnset_data_error(
    learnset_dex: &HashMap<Species, HashSet<PokemonMove>>,
) -> Option<&'static str> {
    learnset_dex
        .is_empty()
        .then_some("Champions learnset data is unavailable")
}

/// Checks that the Champions learnset dex holds every species of one side.
/// Returns the message of the first species that it does not hold, or `None`.
///
/// `showdownDex.txt` holds 1516 species, but Champions gives a learnset to only
/// a small subset. A species outside that set still plays for its own owner,
/// because the teamsheet supplies its moves. It breaks for the opponent:
/// `determinize::sample_moves` finds no move source and returns
/// `NoLegalMoves`, which fails the analysis job mid-battle. `analysis::
/// engine_error` then replaces the engine text with a fixed line that protects
/// hidden player-two data, so the player never learns the cause. Reject the
/// team here instead, while the player can still repair the sheet.
///
/// The check runs in every information mode. Such a species is not legal in
/// Champions, and Perfect Information does not make it legal.
///
/// `parse_pokemon_header` maps a mega entry back to its base species before the
/// preview state stores it, so a `Tyranitar-Mega` line checks `tyranitar`.
pub(crate) fn roster_legality_error<'a>(
    label: &str,
    roster: impl IntoIterator<Item = &'a Species>,
    learnset_dex: &HashMap<Species, HashSet<PokemonMove>>,
) -> Option<String> {
    for species in roster {
        if !learnset_dex.contains_key(species) {
            return Some(format!(
                "{}: {} has no Champions learnset and cannot be used",
                label,
                poke_rust::user::humanize_identifier(format!("{species:?}"))
            ));
        }
    }
    None
}

/// True when this search can control P2 under this information mode.
///
/// A search reads the true position, or it draws worlds from a belief. A
/// session hides Player 1 data, or it hides nothing. The pair plays only when
/// the two answers agree.
///
/// Perfect Information builds no belief, so a belief search fails every
/// analysis job and leaves no answer at all. A search of the true position runs
/// in a fog-of-war session, but it reads data that the fog hides, so
/// `analysis::draw_p2_command` drops its strategy. Both pairs give P2 a uniform
/// draw on every turn, so `create_battle` refuses them.
///
/// `SetupPanel.tsx` disables the same pairs in the picker.
fn bot_algorithm_fits_mode(search: crate::bot::BotSearchConfig, mode: InformationMode) -> bool {
    let searches_belief = matches!(
        search,
        crate::bot::BotSearchConfig::Ismcts(_) | crate::bot::BotSearchConfig::Mccfr(_)
    );
    searches_belief == (mode != InformationMode::PerfectInformation)
}

/// The search of a request that names no algorithm.
///
/// `BotProfileRequest::algorithm` is optional, and `bot::resolve` fills an
/// absent field with `doubleOracle`. That search reads the true position, so it
/// cannot control P2 in a fog-of-war session. Every other mode hides data. An
/// empty profile would therefore fail [`bot_algorithm_fits_mode`].
///
/// The absent field takes the search that fits the mode instead. The response
/// carries the resolved name in `botP2.algorithm`, so the client reads which
/// search it got. `defaultAlgorithmFor` in `SetupPanel.tsx` makes the same
/// choice.
fn default_bot_algorithm(mode: InformationMode) -> &'static str {
    if mode == InformationMode::PerfectInformation {
        "doubleOracle"
    } else {
        "ismcts"
    }
}

/// The 422 message for a pair that cannot play.
///
/// `mode_name` is the wire name of the request, so the message names the field
/// that the client sent. The line names the algorithm and the mode, and no
/// state of either side.
fn bot_algorithm_mismatch(algorithm: &str, mode_name: &str, mode: InformationMode) -> String {
    if mode == InformationMode::PerfectInformation {
        format!(
            "botP2.algorithm: {algorithm} searches a belief, and informationMode \
             {mode_name:?} builds none. Use another algorithm, or use a fog-of-war mode."
        )
    } else {
        format!(
            "botP2.algorithm: {algorithm} reads the true position, and informationMode \
             {mode_name:?} hides that position. Use ismcts or mccfr, or use \"perfect\"."
        )
    }
}

/// Resolves the P2 bot profile of one create request.
///
/// An absent algorithm takes [`default_bot_algorithm`]. A resolve error wins
/// over the pair check, so the client reads the reason of its own field first.
/// `mode_name` is the wire name that the request sent.
///
/// `bot::resolve` refuses a limit that the algorithm does not read. A request
/// that names no algorithm never saw the algorithm that made that rule, so the
/// message ends with the name that this mode chose. Without that name the
/// client reads "use an exact algorithm" for a fog-of-war mode, which the pair
/// check then refuses.
fn resolve_bot_p2(
    request: &crate::bot::BotProfileRequest,
    mode: InformationMode,
    mode_name: &str,
) -> Result<crate::bot::BotProfile, String> {
    let mut request = request.clone();
    let chosen = request.algorithm.is_none().then(|| {
        let name = default_bot_algorithm(mode);
        request.algorithm = Some(name.to_string());
        name
    });
    let profile = crate::bot::resolve("botP2", &request).map_err(|message| {
            match chosen {
                Some(name) => format!(
                    "{message}. botP2.algorithm named no algorithm, \
                 so informationMode {mode_name:?} chose {name}"
                ),
                None => message,
            }
        })?;
    if !bot_algorithm_fits_mode(profile.search, mode) {
        return Err(bot_algorithm_mismatch(
            &profile.view.algorithm,
            mode_name,
            mode,
        ));
    }
    Ok(profile)
}

pub async fn create_battle(
    State(app): State<AppState>,
    Json(req): Json<CreateBattleRequest>,
) -> Response {
    if let Some(message) = side_count_error(
        req.active_per_side,
        req.brought_per_side,
        req.total_per_side,
    ) {
        return unprocessable(message);
    }
    if !(1..=16).contains(&req.damage_rolls) {
        return unprocessable("damageRolls must be between 1 and 16");
    }
    if let Some(message) = learnset_data_error(&app.dexes.learnset_dex) {
        return internal_error(message);
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
        req.total_per_side,
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
        req.total_per_side,
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
        // Roster legality comes before item legality: a species with no
        // Champions learnset cannot hold a legal item either.
        if let Some(message) = roster_legality_error(
            label,
            mons.iter().map(|mon| &mon.species),
            &app.dexes.learnset_dex,
        ) {
            return unprocessable(message);
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

    // Resolve the optional P2 bot profile last. A team or item error must still
    // win the 422. The profile uses the same physics as the battle.
    let bot_p2 = match &req.bot_p2 {
        Some(request) => match resolve_bot_p2(
            request,
            information_mode,
            &req.information_mode,
        ) {
            Ok(profile) => Some(profile),
            Err(message) => return unprocessable(message),
        },
        None => None,
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
        bot_p2,
        analysis: crate::analysis::AnalysisState::default(),
    };

    let battle_id = Uuid::new_v4().to_string();
    let view = session.view(Player::P1);
    let view_p2 = session.view(Player::P2);
    let bot_view = session.bot_p2.as_ref().map(|p| p.view.clone());
    {
        // The first position is the team preview, and a bot session searches it
        // as it searches every later turn. The route inserts the session first,
        // because the task reads the session back by its ID. `start_job` only
        // spawns the search, so this lock does not hold the search.
        let mut sessions = lock_sessions(&app);
        sessions.insert(battle_id.clone(), session);
        if let Some(session) = sessions.get_mut(&battle_id) {
            crate::analysis::start_job(
                &battle_id,
                session,
                Arc::clone(&app.dexes),
                Arc::clone(&app.meta),
                Arc::clone(&app.sessions),
            );
        }
    }

    Json(CreateBattleResponse {
        battle_id,
        state: view,
        state_p2: view_p2,
        bot_p2: bot_view,
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
        bot_p2: session.bot_p2.as_ref().map(|p| p.view.clone()),
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

    // A session with a P2 bot draws P2's command itself, so the request must
    // carry none. A hotseat session must still carry one.
    let (p2_cmd, p2_reveal) = match (session.bot_p2.is_some(), &req.p2) {
        (true, Some(_)) => {
            return unprocessable(
                "p2: this battle runs a P2 bot, so the request must not carry a p2 command"
                    .to_string(),
            );
        }
        (false, None) => {
            return unprocessable(
                "p2: this battle runs no P2 bot, so the request must carry a p2 command"
                    .to_string(),
            );
        }
        (false, Some(dto)) => {
            match session::reconstruct_player_command(session, &app.dexes, Player::P2, dto) {
                Ok(cmd) => (cmd, None),
                Err(message) => return unprocessable(message),
            }
        }
        (true, None) => match crate::analysis::draw_p2_command(session, &app.dexes) {
            // The reveal renders against the position before the turn, so each
            // description names the Pokemon that acted.
            Ok(draw) => {
                let reveal = crate::analysis::reveal_dto(&session.state, &draw);
                (draw.command, Some(reveal))
            }
            Err(message) => return unprocessable(message),
        },
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

    // The position moved, so every streaming solver job of this battle now
    // answers an old question. Each such job ends with a `cancelled` event.
    crate::solve::cancel_jobs_for(&app, crate::solve::SolveSource::Battle, &id);

    // `resolve_turn` already raised the analysis generation, so this job carries
    // the new position. The search runs off the lock, and its result reaches the
    // session only while the generation still matches.
    crate::analysis::start_job(
        &id,
        session,
        Arc::clone(&app.dexes),
        Arc::clone(&app.meta),
        Arc::clone(&app.sessions),
    );

    Json(TurnResponse {
        state: session.view(Player::P1),
        state_p2: session.view(Player::P2),
        events,
        events_p2,
        probability,
        p2_reveal,
    })
    .into_response()
}

/// `GET /api/battles/{id}/analysis` — the private progress of the P2 analysis
/// job.
///
/// The response is progress alone. It never holds a P2 action, a P2 strategy,
/// or the P2 win odds, because P1 reads the same endpoint during a hotseat
/// battle. The turn response reveals the sampled P2 action after both commands
/// lock.
///
/// One session breaks that rule on purpose: a profile with `revealStrategy`
/// asks for P2's strategy, and `analysis::progress_dto` adds it. A hotseat
/// session holds no profile, so it never reaches that path.
pub async fn get_analysis(State(app): State<AppState>, Path(id): Path<String>) -> Response {
    let sessions = lock_sessions(&app);
    let Some(session) = sessions.get(&id) else {
        return not_found();
    };
    Json(crate::analysis::progress_dto(session)).into_response()
}

/// `POST /api/battles/{id}/analysis` keeps the strategy found so far.
pub async fn finish_analysis(State(app): State<AppState>, Path(id): Path<String>) -> Response {
    let mut sessions = lock_sessions(&app);
    let Some(session) = sessions.get_mut(&id) else {
        return not_found();
    };
    if let Err(message) = session.analysis.finish_now() {
        return error(StatusCode::CONFLICT, message);
    }
    Json(crate::analysis::progress_dto(session)).into_response()
}

pub async fn delete_battle(State(app): State<AppState>, Path(id): Path<String>) -> Response {
    let removed = lock_sessions(&app).remove(&id).is_some();
    // A removed battle leaves every streaming job with no target.
    crate::solve::cancel_jobs_for(&app, crate::solve::SolveSource::Battle, &id);
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
    if let Some(message) = learnset_data_error(&app.dexes.learnset_dex) {
        return internal_error(message);
    }
    let mut species: Vec<String> = app
        .dexes
        .pokemon_dex
        .iter()
        .filter(|(key, data)| is_champions_teamsheet_species(key, data, &app.dexes.learnset_dex))
        .map(|(key, _)| poke_rust::user::humanize_identifier(format!("{key:?}")))
        .collect();
    species.sort();
    species.dedup();
    Json(SpeciesListDto { species }).into_response()
}

fn is_champions_teamsheet_species(
    species: &Species,
    data: &poke_rust::state::dex_data::PokemonData,
    learnset_dex: &HashMap<Species, HashSet<PokemonMove>>,
) -> bool {
    data.battle_only.is_none()
        && !poke_rust::state::pokemon::is_mega_dex_entry(species, data)
        && !is_gigantamax_dex_entry(species)
        && learnset_dex.contains_key(species)
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
    use std::collections::{HashMap, HashSet};

    use poke_rust::data::pokemon_move::PokemonMove;
    use poke_rust::data::species::Species;

    use poke_rust::information::unknowns::InformationMode;

    use super::bot_algorithm_fits_mode;
    use super::bot_algorithm_mismatch;
    use super::catch_benchmark_panic;
    use super::default_bot_algorithm;
    use super::is_champions_teamsheet_species;
    use super::learnset_data_error;
    use super::resolve_bot_p2;
    use super::roster_legality_error;
    use super::side_count_error;
    use crate::dto::{CreateBattleRequest, TurnRequest};

    /// Holds the two species that the tests treat as Champions species.
    fn learnset_dex() -> HashMap<Species, HashSet<PokemonMove>> {
        HashMap::from([
            (Species::Garchomp, HashSet::from([PokemonMove::Earthquake])),
            (Species::RotomWash, HashSet::from([PokemonMove::HydroPump])),
        ])
    }

    /// The first species outside the dex names itself, not the whole roster.
    #[test]
    fn a_species_outside_the_learnset_dex_is_rejected() {
        let message = roster_legality_error(
            "p2Team",
            &[Species::Garchomp, Species::Landorus],
            &learnset_dex(),
        );

        assert_eq!(
            message,
            Some("p2Team: Landorus has no Champions learnset and cannot be used".to_string())
        );
    }

    /// The message must carry the display name, not the enum name.
    #[test]
    fn the_message_holds_the_side_label_and_the_display_name() {
        let message = roster_legality_error("myTeam", &[Species::RotomHeat], &learnset_dex())
            .expect("a species outside the dex must produce a message");

        assert!(message.starts_with("myTeam: "), "{message}");
        assert!(message.contains("Rotom Heat"), "{message}");
    }

    #[test]
    fn a_roster_of_learnset_species_is_accepted() {
        assert_eq!(
            roster_legality_error(
                "p1Team",
                &[Species::Garchomp, Species::RotomWash],
                &learnset_dex()
            ),
            None
        );
    }

    #[test]
    fn an_empty_roster_is_accepted() {
        let empty: &[Species] = &[];

        assert_eq!(
            roster_legality_error("p1Team", empty, &learnset_dex()),
            None
        );
    }

    #[test]
    fn an_empty_learnset_dex_reports_a_server_data_error() {
        assert_eq!(
            learnset_data_error(&HashMap::new()),
            Some("Champions learnset data is unavailable")
        );
        assert_eq!(learnset_data_error(&learnset_dex()), None);
    }

    /// The real dex must hold the species that the shipped teamsheets use, and
    /// must not hold a species that Champions leaves out.
    #[test]
    fn the_shipped_learnset_dex_matches_the_check() {
        let dex =
            poke_rust::state::dex_data::parse_learnset_dex("../pokemon_info/showdownLearnsets.txt");
        assert!(!dex.is_empty(), "the learnset dex must load");

        for species in [
            Species::Pikachu,
            Species::Venusaur,
            Species::Incineroar,
            Species::Floette,
            Species::GourgeistSmall,
            Species::GourgeistLarge,
            Species::GourgeistSuper,
            Species::MausholdFour,
            Species::VivillonFancy,
        ] {
            assert_eq!(
                roster_legality_error("p1Team", std::slice::from_ref(&species), &dex),
                None,
                "{species:?} must use its effective Champions learnset"
            );
        }
        assert_eq!(
            roster_legality_error("p2Team", &[Species::Rillaboom], &dex),
            Some("p2Team: Rillaboom has no Champions learnset and cannot be used".to_string())
        );
    }

    #[test]
    fn the_species_catalog_matches_roster_legality() {
        let pokemon_dex =
            poke_rust::state::dex_data::parse_pokemon_dex("../pokemon_info/showdownDex.txt");
        let learnset_dex =
            poke_rust::state::dex_data::parse_learnset_dex("../pokemon_info/showdownLearnsets.txt");
        let listed = |species: &Species| {
            let data = pokemon_dex
                .get(species)
                .expect("the species must have dex data");
            is_champions_teamsheet_species(species, data, &learnset_dex)
        };

        assert!(listed(&Species::Garchomp));
        assert!(listed(&Species::GourgeistSuper));
        assert!(!listed(&Species::Rillaboom));
    }

    /// A session with a P2 bot sends no `p2` field, and `submit_turn` reads the
    /// absent field as the request for a draw. An empty field must therefore
    /// parse, not fail.
    #[test]
    fn a_turn_request_without_a_p2_command_parses() {
        let body = r#"{ "p1": { "kind": "pass" } }"#;

        let req: TurnRequest = serde_json::from_str(body).unwrap();

        assert!(req.p2.is_none());
    }

    /// A hotseat session still sends both commands.
    #[test]
    fn a_turn_request_with_a_p2_command_parses() {
        let body = r#"{ "p1": { "kind": "pass" }, "p2": { "kind": "pass" } }"#;

        let req: TurnRequest = serde_json::from_str(body).unwrap();

        assert!(req.p2.is_some());
    }

    #[test]
    fn benchmark_panics_become_sweep_failures() {
        let result = catch_benchmark_panic::<()>(|| panic!("synthetic sweep failure"));
        assert_eq!(
            result,
            Err("benchmark sweep panicked: synthetic sweep failure".to_string())
        );
    }

    #[test]
    fn a_full_roster_with_a_smaller_brought_count_is_valid() {
        assert_eq!(side_count_error(2, 4, 6), None);
        assert_eq!(side_count_error(1, 3, 6), None);
        assert_eq!(side_count_error(2, 6, 6), None);
    }

    #[test]
    fn a_roster_below_the_brought_count_is_rejected() {
        assert_eq!(
            side_count_error(2, 4, 3),
            Some("totalPerSide must be >= broughtPerSide")
        );
    }

    #[test]
    fn a_roster_above_six_is_rejected() {
        assert_eq!(side_count_error(2, 4, 7), Some("totalPerSide must be <= 6"));
    }

    #[test]
    fn the_active_count_rule_still_runs_first() {
        assert_eq!(
            side_count_error(0, 4, 6),
            Some("activePerSide must be >= 1 and <= broughtPerSide")
        );
        assert_eq!(
            side_count_error(4, 2, 6),
            Some("activePerSide must be >= 1 and <= broughtPerSide")
        );
    }

    /// An older client sends no `totalPerSide`, and it must still get a roster
    /// of 6.
    #[test]
    fn an_absent_roster_size_defaults_to_six() {
        let body = r#"{
            "p1Team": "",
            "p2Team": "",
            "activePerSide": 2,
            "broughtPerSide": 4
        }"#;
        let req: CreateBattleRequest = serde_json::from_str(body).unwrap();
        assert_eq!(req.total_per_side, 6);
    }

    #[test]
    fn a_sent_roster_size_wins_over_the_default() {
        let body = r#"{
            "p1Team": "",
            "p2Team": "",
            "activePerSide": 2,
            "broughtPerSide": 4,
            "totalPerSide": 5
        }"#;
        let req: CreateBattleRequest = serde_json::from_str(body).unwrap();
        assert_eq!(req.total_per_side, 5);
    }

    #[test]
    fn an_absent_bot_profile_stays_absent() {
        let body = r#"{
            "p1Team": "",
            "p2Team": "",
            "activePerSide": 2,
            "broughtPerSide": 4
        }"#;
        let req: CreateBattleRequest = serde_json::from_str(body).unwrap();
        assert!(req.bot_p2.is_none());
    }

    #[test]
    fn a_sent_bot_profile_uses_solver_physics() {
        let body = r#"{
            "p1Team": "",
            "p2Team": "",
            "activePerSide": 2,
            "broughtPerSide": 4,
            "damageRolls": 16,
            "considerCrit": true,
            "botP2": { "algorithm": "ismcts", "damageRolls": 4, "considerCrit": false }
        }"#;
        let req: CreateBattleRequest = serde_json::from_str(body).unwrap();
        let profile = crate::bot::resolve("botP2", req.bot_p2.as_ref().unwrap()).unwrap();
        assert_eq!(profile.view.algorithm, "ismcts");
        assert!(!profile.view.exact);
        let crate::bot::BotSearchConfig::Ismcts(config) = profile.search else {
            panic!("ismcts must build an ismcts configuration");
        };
        assert_eq!(config.search.damage_rolls, 4);
        assert!(!config.search.consider_crit);
    }

    /// The search of one algorithm name with server defaults.
    fn search_of(algorithm: &str) -> crate::bot::BotSearchConfig {
        let request = crate::bot::BotProfileRequest {
            algorithm: Some(algorithm.to_string()),
            ..crate::bot::BotProfileRequest::default()
        };
        crate::bot::resolve("botP2", &request)
            .unwrap()
            .search
    }

    #[test]
    fn a_belief_search_fits_only_a_fog_of_war_mode() {
        for name in ["ismcts", "mccfr"] {
            let search = search_of(name);
            assert!(
                !bot_algorithm_fits_mode(search, InformationMode::PerfectInformation),
                "{name} needs a belief"
            );
            for mode in [
                InformationMode::ClosedTeamSheet,
                InformationMode::OpenTeamSheet,
                InformationMode::OpenTeamSheetNatures,
            ] {
                assert!(bot_algorithm_fits_mode(search, mode), "{name} in {mode:?}");
            }
        }
    }

    #[test]
    fn a_true_position_search_fits_only_perfect_information() {
        for name in [
            "doubleOracle",
            "serializedBounds",
            "backwardInduction",
            "mcts",
        ] {
            let search = search_of(name);
            assert!(
                bot_algorithm_fits_mode(search, InformationMode::PerfectInformation),
                "{name} reads the true position"
            );
            for mode in [
                InformationMode::ClosedTeamSheet,
                InformationMode::OpenTeamSheet,
                InformationMode::OpenTeamSheetNatures,
            ] {
                assert!(
                    !bot_algorithm_fits_mode(search, mode),
                    "{name} in {mode:?} reads hidden data"
                );
            }
        }
    }

    #[test]
    fn each_mismatch_message_names_the_algorithm_and_the_mode() {
        let belief =
            bot_algorithm_mismatch("mccfr", "perfect", InformationMode::PerfectInformation);
        assert!(belief.starts_with("botP2.algorithm: mccfr"), "{belief}");
        assert!(belief.contains("\"perfect\""), "{belief}");
        assert!(belief.contains("searches a belief"), "{belief}");

        let exact = bot_algorithm_mismatch(
            "doubleOracle",
            "closedSheet",
            InformationMode::ClosedTeamSheet,
        );
        assert!(
            exact.starts_with("botP2.algorithm: doubleOracle"),
            "{exact}"
        );
        assert!(exact.contains("\"closedSheet\""), "{exact}");
        assert!(exact.contains("reads the true position"), "{exact}");
    }

    /// The four wire names of the information mode, with the mode of each one.
    const WIRE_MODES: [(&str, InformationMode); 4] = [
        ("perfect", InformationMode::PerfectInformation),
        ("closedSheet", InformationMode::ClosedTeamSheet),
        ("openSheet", InformationMode::OpenTeamSheet),
        ("openSheetNatures", InformationMode::OpenTeamSheetNatures),
    ];

    /// An empty profile must still build a bot that plays.
    ///
    /// `bot::resolve` fills an absent algorithm with `doubleOracle`, and that
    /// search reads the true position, so the fixed default alone would refuse
    /// every fog-of-war session.
    #[test]
    fn an_absent_bot_algorithm_takes_a_search_that_fits_the_mode() {
        let body = r#"{
            "p1Team": "",
            "p2Team": "",
            "activePerSide": 2,
            "broughtPerSide": 4,
            "botP2": {}
        }"#;
        let req: CreateBattleRequest = serde_json::from_str(body).unwrap();
        let request = req.bot_p2.as_ref().unwrap();
        assert!(request.algorithm.is_none(), "the body names no algorithm");

        for (name, mode) in WIRE_MODES {
            let profile = resolve_bot_p2(request, mode, name)
                .unwrap_or_else(|message| panic!("informationMode {name:?}: {message}"));
            assert!(
                bot_algorithm_fits_mode(profile.search, mode),
                "informationMode {name:?} resolved {}, which cannot control P2",
                profile.view.algorithm
            );
            // A mode that resolves a sampled search takes the derived budget,
            // which reads the depth and the particle count.
            let expected = match profile.view.particles {
                Some(particles) => {
                    crate::bot::DEFAULT_ROLLOUTS_PER_PARTICLE
                        * particles as u64
                        * profile.view.depth as u64
                }
                None => crate::bot::DEFAULT_SIMULATION_TURN_BUDGET,
            };
            assert_eq!(
                profile.view.simulation_turn_budget, expected,
                "informationMode {name:?}"
            );
        }
    }

    /// The default of the request body is the default of the picker.
    #[test]
    fn the_absent_algorithm_matches_the_picker_default() {
        assert_eq!(
            default_bot_algorithm(InformationMode::PerfectInformation),
            "doubleOracle"
        );
        for (name, mode) in WIRE_MODES {
            if mode == InformationMode::PerfectInformation {
                continue;
            }
            assert_eq!(default_bot_algorithm(mode), "ismcts", "mode {name:?}");
        }
    }

    /// A named pair that cannot play still fails, and the message names it.
    #[test]
    fn a_named_pair_that_cannot_play_is_refused() {
        let exact = crate::bot::BotProfileRequest {
            algorithm: Some("doubleOracle".to_string()),
            ..crate::bot::BotProfileRequest::default()
        };
        let message = resolve_bot_p2(&exact, InformationMode::ClosedTeamSheet, "closedSheet")
        .expect_err("an exact search cannot control P2 under the fog of war");
        assert!(message.contains("reads the true position"), "{message}");

        let belief = crate::bot::BotProfileRequest {
            algorithm: Some("mccfr".to_string()),
            ..crate::bot::BotProfileRequest::default()
        };
        let message = resolve_bot_p2(&belief, InformationMode::PerfectInformation, "perfect")
        .expect_err("a belief search needs a belief");
        assert!(message.contains("searches a belief"), "{message}");
    }

}
