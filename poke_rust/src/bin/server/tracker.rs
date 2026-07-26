//! Tracker mode: sessions for following a real battle by typing what happened,
//! rather than simulating an opponent. See `tracker_parse.rs` for the text
//! grammar and `tracker_effects.rs` for guaranteed-effect synthesis; this
//! module owns the session type and its four HTTP handlers.
//!
//! # Why there's no concrete `MatchState` here
//!
//! Battle mode's `BattleSession` holds an authoritative `MatchState` plus a
//! *belief about the opponent* derived from it. Tracker mode has no opponent to
//! simulate — the belief IS the state, for both sides: the viewer's own side is
//! seeded fully `Known` (see `create_tracker`) exactly like `from_known_pokemon`
//! already seeds a battle-mode belief's own side, and the opponent's side
//! narrows exactly like an ordinary fog-of-war belief always has. Neither side
//! starts with anyone active — a session begins fully benched on both sides,
//! and the first `leads p … o …` tracker-text event (see `tracker_parse.rs`)
//! sends out both sides' actual leads together, symmetrically for the viewer
//! and the opponent.
//! `apply_information` already knows how to fold events into a belief with no
//! separate ground truth backing it — that's exactly what a battle-mode belief
//! already is; `mapping::battle_view_from_belief` renders a `BattleView`
//! straight from it, the same way `bench_pokemon_view_from_belief` already
//! renders bench mons with no concrete pairing.
//!
//! # Turn contract (Phase 1)
//!
//! `POST /events` must submit one or more *complete* turns, each ending with an
//! explicit `endofturn` line — see `split_into_turns`. Every turn is applied to
//! a scratch clone of the belief first; the session only commits once every
//! turn in the submission has succeeded, mirroring `session::resolve_turn`'s
//! all-or-nothing discipline (a contradiction must never leave the belief
//! half-updated).

use std::collections::{HashMap, HashSet};

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use uuid::Uuid;

use poke_rust::data::item::Item;
use poke_rust::data::species::Species;
use poke_rust::information::inference::{InferenceConfig, apply_information, apply_structural_preview};
use poke_rust::information::information::{EventKind, InformationEvent};
use poke_rust::information::unknowns::{InformationMode, UnknownBattleState, UnknownMatchState};
use poke_rust::simulator;
use poke_rust::state::battle::{FieldSlot, Player};
use poke_rust::user::{humanize_identifier, move_name};

use crate::dto::*;
use crate::mapping;
use crate::routes::AppState;
use crate::session::Dexes;
use crate::tracker_effects::augment_turn;
use crate::tracker_parse::{ParseError, TrackerLine, fold_leads_and_entry_abilities, parse_tracker_text};

pub struct TrackerSession {
    pub belief: UnknownBattleState,
    /// Snapshot of `belief` taken in `create_tracker`, before any tracker-text
    /// event has been applied — nobody active on either side yet (see this
    /// module's doc comment). The rebuild path (`rebuild_tracker_history`)
    /// re-applies `script` on top of THIS, never `belief`, since `belief` is
    /// the already-mutated cumulative state a rebuild needs to discard and
    /// recompute from scratch.
    pub initial_belief: UnknownBattleState,
    /// Raw tracker-text of each `/events` submission (or the single full
    /// script from the last `/history` rebuild), concatenated with a blank
    /// line between entries to reproduce the exact input to `PUT /history`.
    /// Exists so `GET /tracker/{id}` can hand the editor back its authored
    /// text after a page reload — `log`/`belief` alone can't be reversed into
    /// tracker syntax (see `apply_turns_from`'s doc comment).
    pub script: Vec<String>,
    pub active_per_side: u8,
    pub brought_per_side: u8,
    pub inference_config: InferenceConfig,
    pub log: Vec<TurnLogEntry>,
    pub turn_count: u16,
    /// Every species on either roster, as parsed from the teamsheets at
    /// `create_tracker` time — independent of fog-of-war, since the tracker
    /// completions endpoint (`get_tracker_completions`) needs the ground-truth
    /// match roster, not a belief that may still be narrowing the opponent's
    /// species. Deduplicated; order not significant.
    pub roster_species: Vec<Species>,
}

#[derive(Debug)]
pub(crate) enum SubmitTrackerError {
    Parse(ParseError),
    Unprocessable(String),
    Internal(String),
}

fn lock(app: &AppState) -> std::sync::MutexGuard<'_, HashMap<String, TrackerSession>> {
    app.tracker_sessions.lock().unwrap_or_else(|e| e.into_inner())
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
    error(StatusCode::NOT_FOUND, "tracker not found")
}
fn internal_error(message: impl Into<String>) -> Response {
    error(StatusCode::INTERNAL_SERVER_ERROR, message)
}

/// A parse failure gets its own typed body (line + message) rather than being
/// flattened into `ApiError.message` — the frontend can point the cursor at
/// the offending line instead of just displaying prose.
fn parse_error_response(e: ParseError) -> Response {
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        Json(TrackerParseErrorDto {
            line: e.line,
            message: e.message,
        }),
    )
        .into_response()
}

fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        s.to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "internal error (inference engine panicked)".to_string()
    }
}

/// Turn a single-line comma-separated species list into the blank-line-
/// separated blocks `parse_team_sheet_str` (via
/// `simulator::team_preview_state_from_team_strings`) expects. A real teamsheet
/// paste always has newlines of its own (moves, EVs, etc. each on their own
/// line), so it's never mistaken for this shorthand.
pub fn normalize_opponent_text(input: &str) -> String {
    let trimmed = input.trim();
    if !trimmed.contains('\n') && trimmed.contains(',') {
        trimmed
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n")
    } else {
        trimmed.to_string()
    }
}

pub async fn create_tracker(
    State(app): State<AppState>,
    Json(req): Json<CreateTrackerRequest>,
) -> Response {
    if req.active_per_side == 0 || req.brought_per_side < req.active_per_side {
        return unprocessable("activePerSide must be >= 1 and <= broughtPerSide");
    }

    let legal_items: Option<HashSet<Item>> = if req.legal_items.is_empty() {
        None
    } else {
        let mut set = HashSet::with_capacity(req.legal_items.len());
        for slug in &req.legal_items {
            let item = Item::from_str(slug);
            if matches!(item, Item::Unknown(_)) {
                return unprocessable(format!("legalItems: unrecognized item {slug:?}"));
            }
            set.insert(item);
        }
        Some(set)
    };

    let opponent_text = normalize_opponent_text(&req.opponent);
    let preview = simulator::team_preview_state_from_team_strings(
        &req.my_team,
        &opponent_text,
        &app.dexes.pokemon_dex,
        &app.dexes.move_dex,
        req.active_per_side,
        req.brought_per_side,
        req.stat_points,
    );
    for (label, mons) in [("myTeam", &preview.p1_mons), ("opponent", &preview.p2_mons)] {
        if mons.is_empty() {
            return unprocessable(format!("{label}: no valid Pokemon parsed"));
        }
        if mons.len() < req.brought_per_side as usize {
            return unprocessable(format!(
                "{}: {} Pokemon parsed but the format brings {}",
                label,
                mons.len(),
                req.brought_per_side
            ));
        }
    }
    let information_mode = match req.information_mode.as_str() {
        "closedSheet" => InformationMode::ClosedTeamSheet,
        "openSheet" => InformationMode::OpenTeamSheet,
        "openSheetNatures" => InformationMode::OpenTeamSheetNatures,
        "perfect" => {
            return unprocessable("informationMode 'perfect' has no meaning in tracker mode");
        }
        other => return unprocessable(format!("unknown informationMode: {other}")),
    };

    let inference_config = InferenceConfig {
        use_stat_points: req.stat_points,
        force_max_ivs: req.force_max_ivs,
        legal_items,
        learnset_dex: app.dexes.learnset_dex.clone(),
        ..InferenceConfig::default()
    };

    let team_preview_belief = if information_mode == InformationMode::ClosedTeamSheet {
        UnknownMatchState::team_preview_closed_sheet_from_perspective(
            Player::P1,
            &preview.p1_mons,
            &preview.p2_mons,
            &app.dexes.pokemon_dex,
            req.active_per_side,
            req.brought_per_side,
            50,
            req.force_max_ivs,
        )
    } else {
        UnknownMatchState::team_preview_open_sheet_from_perspective(
            Player::P1,
            &preview.p1_mons,
            &preview.p2_mons,
            &app.dexes.pokemon_dex,
            req.active_per_side,
            req.brought_per_side,
            50,
            information_mode,
            req.force_max_ivs,
        )
    };
    let UnknownMatchState::TeamPreview(team_preview_belief) = team_preview_belief else {
        return internal_error("team preview belief constructor returned the wrong variant");
    };

    // Nobody is sent out yet on EITHER side — leads are conveyed by the first
    // `leads p … o …` tracker-text event, not chosen up front (see this
    // module's doc comment). `into_battle_state` already gives the
    // opponent's whole roster to `possible_back` unconditionally when
    // `viewer == P1`; passing every one of the viewer's own indices as
    // "back" (none "active") does the identical thing for their own side —
    // fully `Known`, just not on the field yet.
    let all_p1_indices: Vec<usize> = (0..preview.p1_mons.len()).collect();
    let battle_belief =
        team_preview_belief.into_battle_state(Player::P1, &[], &all_p1_indices, &[], &[]);

    let mut roster_species: Vec<Species> = Vec::new();
    for mon in preview.p1_mons.iter().chain(preview.p2_mons.iter()) {
        if !roster_species.contains(&mon.species) {
            roster_species.push(mon.species.clone());
        }
    }

    let session = TrackerSession {
        initial_belief: battle_belief.clone(),
        belief: battle_belief,
        script: Vec::new(),
        active_per_side: req.active_per_side,
        brought_per_side: req.brought_per_side,
        inference_config,
        log: Vec::new(),
        turn_count: 0,
        roster_species,
    };
    let view = mapping::battle_view_from_belief(
        &session.belief,
        session.active_per_side,
        session.brought_per_side,
        session.inference_config.legal_items.as_ref(),
    );
    let tracker_id = Uuid::new_v4().to_string();
    lock(&app).insert(tracker_id.clone(), session);

    Json(CreateTrackerResponse {
        tracker_id,
        state: view,
    })
    .into_response()
}

pub async fn get_tracker(State(app): State<AppState>, Path(id): Path<String>) -> Response {
    let sessions = lock(&app);
    let Some(session) = sessions.get(&id) else {
        return not_found();
    };
    let view = mapping::battle_view_from_belief(
        &session.belief,
        session.active_per_side,
        session.brought_per_side,
        session.inference_config.legal_items.as_ref(),
    );
    Json(GetTrackerResponse {
        state: view,
        log: session.log.clone(),
        script: session.script.join("\n"),
    })
    .into_response()
}

pub async fn delete_tracker(State(app): State<AppState>, Path(id): Path<String>) -> Response {
    lock(&app).remove(&id);
    StatusCode::NO_CONTENT.into_response()
}

/// Split parsed lines into complete, `EndOfTurn`-terminated batches, appending
/// the terminal `EventKind::EndOfTurn` node itself (Pass 1 uses it to trigger
/// per-turn bookkeeping — see `EventKind::EndOfTurn`'s doc comment) — the user
/// never types it as a literal event node, just the `endofturn` sentinel line.
/// Rejects a trailing partial turn rather than silently applying it early; see
/// this module's doc comment on the Phase-1 "submit whole turns" contract.
fn split_into_turns(lines: Vec<TrackerLine>) -> Result<Vec<Vec<InformationEvent>>, String> {
    let mut turns = Vec::new();
    let mut current: Vec<InformationEvent> = Vec::new();
    for line in lines {
        match line {
            TrackerLine::Event(ev) => current.push(ev),
            TrackerLine::EndOfTurn => {
                current.push(InformationEvent {
                    kind: EventKind::EndOfTurn,
                    reactions: Vec::new(),
                });
                turns.push(std::mem::take(&mut current));
            }
        }
    }
    if !current.is_empty() {
        return Err(format!(
            "{} event(s) after the last 'endofturn' — every turn must end with an explicit endofturn line",
            current.len()
        ));
    }
    if turns.is_empty() {
        return Err("no complete turn submitted (need at least one 'endofturn' line)".to_string());
    }
    Ok(turns)
}

/// Every active slot on both sides must show *some* recorded action this
/// turn — a move, a switch (voluntary or a faint replacement), or an explicit
/// reason it couldn't act (`Cant`/`MustRecharge`) — checked against `belief`
/// as it stood *before* this turn's events apply (the same "current
/// occupant/reserves" question a real turn would ask). A slot with no
/// recorded action at all almost always means the user forgot to type it,
/// not that nothing happened — so this is rejected the same way a
/// contradiction is, before any mutation. The one legitimate exception: a
/// slot whose occupant is already fainted with no healthy reserve left on
/// that side genuinely has nothing to record.
fn validate_turn_completeness(
    events: &[InformationEvent],
    belief: &UnknownBattleState,
    active_per_side: u8,
) -> Result<(), String> {
    let has_regular_action = events.iter().any(|event| {
        matches!(
            event.kind,
            EventKind::MoveUsed { .. }
                | EventKind::MustRecharge { .. }
                | EventKind::ChargingMove { .. }
        ) || matches!(
                event.kind,
                EventKind::Cant { ref reason, .. } if *reason != poke_rust::information::information::CantReason::Other
            )
    });
    let replacement_batch = !has_regular_action
        && [Player::P1, Player::P2].into_iter().any(|player| {
            (0..active_per_side).any(|slot_index| {
                slot_needs_replacement(belief, FieldSlot { player, slot_index })
            })
        });
    let battle_ended = [Player::P1, Player::P2]
        .into_iter()
        .any(|player| side_was_eliminated(events, belief, player, active_per_side));

    for player in [Player::P1, Player::P2] {
        for slot_index in 0..active_per_side {
            let slot = FieldSlot { player, slot_index };
            if slot_acted(events, slot)
                || slot_was_zeroed(events, slot)
                || slot_can_be_skipped(belief, slot)
                || (replacement_batch && !slot_needs_replacement(belief, slot))
                || battle_ended
            {
                continue;
            }
            return Err(format!(
                "{player:?} slot {} did nothing this turn (no move, switch, or recorded reason)",
                slot_index + 1
            ));
        }
    }
    Ok(())
}

fn side_was_eliminated(
    events: &[InformationEvent],
    belief: &UnknownBattleState,
    player: Player,
    active_per_side: u8,
) -> bool {
    let active = match player {
        Player::P1 => &belief.p1_active_mons,
        Player::P2 => &belief.p2_active_mons,
    };
    let all_active_fainted = (0..active_per_side).all(|slot_index| {
        let slot = FieldSlot { player, slot_index };
        active
            .get(slot_index as usize)
            .is_some_and(|mon| mon.fainted)
            || slot_was_zeroed(events, slot)
    });
    all_active_fainted && side_has_no_healthy_reserve(belief, player)
}

/// Does `player`'s bench hold not a single non-fainted reserve — neither a
/// revealed (`known_back`) nor a still-unrevealed-but-possible
/// (`possible_back`) one? Sound under fog: a `possible_back` mon that
/// *might* still be alive keeps this `false`, the same soundness discipline
/// every other belief-only predicate in this module follows. Shared by
/// `side_was_eliminated` (this turn's not-yet-committed events may zero a
/// slot the belief doesn't reflect yet) and [`side_eliminated`] (the
/// already-committed belief alone, for rendering game-over after the fact).
fn side_has_no_healthy_reserve(belief: &UnknownBattleState, player: Player) -> bool {
    let (known_back, possible_back) = match player {
        Player::P1 => (&belief.p1_known_back_mons, &belief.p1_possible_back_mons),
        Player::P2 => (&belief.p2_known_back_mons, &belief.p2_possible_back_mons),
    };
    !known_back.iter().any(|mon| !mon.fainted) && !possible_back.iter().any(|mon| !mon.fainted)
}

/// Belief-only counterpart to [`side_was_eliminated`], for rendering: true
/// when `player`'s active slots are ALL fainted (per the already-committed
/// belief — no in-flight `events` to cross-check, unlike the turn-validation
/// caller above) and the bench holds no possible survivor either. Used by
/// `mapping::battle_view_from_belief` to decide `PhaseDto::GameOver`.
pub(crate) fn side_eliminated(belief: &UnknownBattleState, player: Player, active_per_side: u8) -> bool {
    let active = match player {
        Player::P1 => &belief.p1_active_mons,
        Player::P2 => &belief.p2_active_mons,
    };
    let all_active_fainted = (0..active_per_side)
        .all(|slot_index| active.get(slot_index as usize).is_some_and(|mon| mon.fainted));
    all_active_fainted && side_has_no_healthy_reserve(belief, player)
}

fn slot_was_zeroed(events: &[InformationEvent], slot: FieldSlot) -> bool {
    events.iter().any(|event| {
        let zeroed = match &event.kind {
            EventKind::DamageDealt { target, new_hp, .. }
            | EventKind::Healed { target, new_hp, .. }
            | EventKind::SetHp { target, new_hp, .. } => {
                *target == slot
                    && matches!(
                        new_hp,
                        poke_rust::information::unknowns::PokemonHP::Number(0)
                            | poke_rust::information::unknowns::PokemonHP::Percent(0)
                    )
            }
            EventKind::Faint { slot: fainted } => *fainted == slot,
            _ => false,
        };
        zeroed || slot_was_zeroed(&event.reactions, slot)
    })
}

fn slot_needs_replacement(belief: &UnknownBattleState, slot: FieldSlot) -> bool {
    let active = match slot.player {
        Player::P1 => &belief.p1_active_mons,
        Player::P2 => &belief.p2_active_mons,
    };
    match active.get(slot.slot_index as usize) {
        None => true,
        Some(mon) if !mon.fainted => false,
        Some(_) => {
            let (known_back, possible_back) = match slot.player {
                Player::P1 => (&belief.p1_known_back_mons, &belief.p1_possible_back_mons),
                Player::P2 => (&belief.p2_known_back_mons, &belief.p2_possible_back_mons),
            };
            known_back.iter().any(|mon| !mon.fainted)
                || possible_back.iter().any(|mon| !mon.fainted)
        }
    }
}

/// Does `slot` have a top-level event this turn establishing it acted?
fn slot_acted(events: &[InformationEvent], slot: FieldSlot) -> bool {
    events.iter().any(|e| match &e.kind {
        EventKind::MoveUsed { user, .. } => *user == slot,
        EventKind::Switch(sw) => sw.slot == slot,
        EventKind::SimultaneousSwitch { switches } => switches.iter().any(|sw| sw.slot == slot),
        EventKind::Cant { slot: s, .. } => *s == slot,
        EventKind::MustRecharge { slot: s } => *s == slot,
        // Spending the turn charging (Solar Beam, Fly, …) is acting. In practice the
        // grammar always nests this under a `MoveUsed`, which the first arm already
        // catches; this covers a hand-built or engine-sourced tree that carries a
        // bare top-level one.
        EventKind::ChargingMove { user, .. } => *user == slot,
        _ => false,
    })
}

/// True only when `slot` genuinely has nothing possible to record: its
/// pre-turn occupant is already fainted and neither known- nor
/// possible-back holds a single non-fainted reserve for that side. A slot
/// that was never filled at all (still pre-leads) is NOT skippable — that
/// side is expected to send out a lead this same turn.
fn slot_can_be_skipped(belief: &UnknownBattleState, slot: FieldSlot) -> bool {
    let active = match slot.player {
        Player::P1 => &belief.p1_active_mons,
        Player::P2 => &belief.p2_active_mons,
    };
    match active.get(slot.slot_index as usize) {
        None => false,
        Some(mon) if !mon.fainted => false,
        Some(_) => {
            let (known_back, possible_back) = match slot.player {
                Player::P1 => (&belief.p1_known_back_mons, &belief.p1_possible_back_mons),
                Player::P2 => (&belief.p2_known_back_mons, &belief.p2_possible_back_mons),
            };
            !known_back.iter().any(|m| !m.fainted) && !possible_back.iter().any(|m| !m.fainted)
        }
    }
}

pub async fn submit_tracker_events(
    State(app): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<TrackerEventsRequest>,
) -> Response {
    let mut sessions = lock(&app);
    let Some(session) = sessions.get_mut(&id) else {
        return not_found();
    };

    match apply_tracker_text(session, &req.text, &app.dexes) {
        Ok(response) => Json(response).into_response(),
        Err(SubmitTrackerError::Parse(error)) => parse_error_response(error),
        Err(SubmitTrackerError::Unprocessable(message)) => unprocessable(message),
        Err(SubmitTrackerError::Internal(message)) => internal_error(message),
    }
}

/// The result of successfully applying a batch of complete turns starting
/// from some base belief — see `apply_turns_from`.
struct AppliedTurns {
    belief: UnknownBattleState,
    log: Vec<TurnLogEntry>,
}

/// Parses `text` (one or more complete, `endofturn`-terminated turns) and
/// applies them in order starting from `base`, all-or-nothing: `base` itself
/// is never mutated, and any failure — parse, turn-completeness, or an
/// inference-engine panic caught via `catch_unwind` — discards everything
/// applied so far in this call, returning the base belief's caller untouched.
/// This is the exact production pipeline both tracker-mutating endpoints
/// share:
/// - `submit_tracker_events` (`POST /events`) calls this with `base =
///   &session.belief` and `turn_offset = session.turn_count`, then EXTENDS the
///   session (append semantics — existing turns are untouched).
/// - `rebuild_tracker_history` (`PUT /history`) calls this with `base =
///   &session.initial_belief` and `turn_offset = 0`, then REPLACES the session
///   wholesale — the only way to make an edit to an already-committed turn
///   take effect, since a committed belief cannot be reverse-applied (there is
///   no `EventNode → InformationEvent` inverse — see `TrackerSession::script`'s
///   doc comment).
///
/// `turn_offset` only affects `TurnLogEntry` labels ("Turn N") and the turn
/// number reported in an error message; it does not affect what's valid.
fn apply_turns_from(
    base: &UnknownBattleState,
    text: &str,
    dexes: &Dexes,
    config: &InferenceConfig,
    active_per_side: u8,
    turn_offset: u16,
) -> Result<AppliedTurns, SubmitTrackerError> {
    // Parse against `base` (HP-direction classification reads it) — a parse
    // error must never mutate anything.
    let lines = match parse_tracker_text(text, base, &dexes.move_dex, &dexes.pokemon_dex) {
        Ok(l) => l,
        Err(e) => return Err(SubmitTrackerError::Parse(e)),
    };
    let turns = match split_into_turns(lines) {
        Ok(t) => t,
        Err(message) => return Err(SubmitTrackerError::Unprocessable(message)),
    };

    let mut working = base.clone();
    let mut log: Vec<TurnLogEntry> = Vec::new();
    let mut turn_count = turn_offset;
    for events in turns {
        // A combined `leads p … o …` line already parses to one event; this
        // also merges the (still-accepted) two-separate-lines form and folds
        // immediately-following entry-ability reveals into its `reactions` —
        // see `fold_leads_and_entry_abilities`'s doc comment. Must run before
        // completeness validation / synthesis so both see the final nested
        // shape.
        let events = fold_leads_and_entry_abilities(events);

        // An incomplete turn (some active slot recorded nothing at all) is
        // rejected the same way a contradiction is — before any mutation.
        if let Err(message) = validate_turn_completeness(&events, &working, active_per_side) {
            return Err(SubmitTrackerError::Unprocessable(format!(
                "turn {}: {message}",
                turn_count + 1
            )));
        }

        // Guaranteed-effect synthesis reads `working` as it stands right before
        // THIS turn's own events apply, threading a weather/terrain scratch
        // across the turn's own events as it goes — see `augment_turn`'s doc
        // comment in `tracker_effects`.
        let events: Vec<InformationEvent> =
            augment_turn(events, &working, &dexes.move_dex, &dexes.pokemon_dex);

        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            apply_information(
                UnknownMatchState::Battle(working.clone()),
                &events,
                false,
                &dexes.pokemon_dex,
                &dexes.move_dex,
                &dexes.ability_dex,
                config,
            )
        }));
        match caught {
            Ok(UnknownMatchState::Battle(next)) => working = next,
            Ok(_) => {
                return Err(SubmitTrackerError::Internal(
                    "belief left the Battle phase mid-turn".to_string(),
                ));
            }
            Err(payload) => {
                return Err(SubmitTrackerError::Unprocessable(format!(
                    "turn {}: {}",
                    turn_count + 1,
                    panic_message(payload)
                )));
            }
        }
        turn_count += 1;
        log.push(TurnLogEntry {
            label: format!("Turn {turn_count}"),
            events: events.iter().map(mapping::event_node).collect(),
        });
    }

    Ok(AppliedTurns { belief: working, log })
}

/// Apply a tracker-text request through the exact production pipeline. Work is
/// performed on local clones and committed only after every submitted turn
/// succeeds, preserving the endpoint's all-or-nothing contract.
pub(crate) fn apply_tracker_text(
    session: &mut TrackerSession,
    text: &str,
    dexes: &Dexes,
) -> Result<TrackerEventsResponse, SubmitTrackerError> {
    let applied = apply_turns_from(
        &session.belief,
        text,
        dexes,
        &session.inference_config,
        session.active_per_side,
        session.turn_count,
    )?;

    session.belief = applied.belief;
    session.turn_count += applied.log.len() as u16;
    session.log.extend(applied.log.iter().cloned());
    session.script.push(text.to_string());
    let view = mapping::battle_view_from_belief(
        &session.belief,
        session.active_per_side,
        session.brought_per_side,
        session.inference_config.legal_items.as_ref(),
    );

    Ok(TrackerEventsResponse {
        state: view,
        log_delta: applied.log,
    })
}

/// `POST /api/tracker/{id}/preview` — per-event structural feedback for the
/// in-progress (possibly incomplete) turn the user is currently typing.
/// Unlike `submit_tracker_events`, this NEVER mutates the session: it clones
/// the committed belief, runs the same parse/fold/augment steps, then applies
/// only `apply_structural_preview`'s Pass-1 subset (see its doc comment for
/// why that's the only pass sound on a partial turn) and returns a disposable
/// provisional view. `text` need not be a complete turn and need not end with
/// `endofturn` — turn-completeness is deliberately NOT checked here.
pub async fn preview_tracker_events(
    State(app): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<TrackerPreviewRequest>,
) -> Response {
    let sessions = lock(&app);
    let Some(session) = sessions.get(&id) else {
        return not_found();
    };

    let lines = match parse_tracker_text(
        &req.text,
        &session.belief,
        &app.dexes.move_dex,
        &app.dexes.pokemon_dex,
    ) {
        Ok(l) => l,
        Err(e) => return parse_error_response(e),
    };
    let events: Vec<InformationEvent> = lines
        .into_iter()
        .filter_map(|line| match line {
            TrackerLine::Event(ev) => Some(ev),
            TrackerLine::EndOfTurn => None,
        })
        .collect();
    let events = fold_leads_and_entry_abilities(events);

    let mut working = session.belief.clone();
    let events: Vec<InformationEvent> =
        augment_turn(events, &working, &app.dexes.move_dex, &app.dexes.pokemon_dex);
    apply_structural_preview(
        &mut working,
        &events,
        &app.dexes.pokemon_dex,
        &app.dexes.move_dex,
        &app.dexes.ability_dex,
        &session.inference_config,
    );

    let view = mapping::battle_view_from_belief(
        &working,
        session.active_per_side,
        session.brought_per_side,
        session.inference_config.legal_items.as_ref(),
    );
    Json(TrackerPreviewResponse {
        state: view,
        events: events.iter().map(mapping::event_node).collect(),
    })
    .into_response()
}

/// `PUT /api/tracker/{id}/history` — rebuild the whole session from
/// `session.initial_belief` using a corrected/edited FULL script (every turn,
/// not just new ones). This is how editing a past event or removing a
/// committed turn takes effect: the frontend owns the authored script (see
/// `TrackerSession::script`'s doc comment) and always resubmits it in full
/// here rather than trying to patch a single turn in place, since parsing and
/// validation are both belief-dependent and must replay in order from
/// scratch. All-or-nothing exactly like `POST /events` — on any failure the
/// session is untouched. An empty (or all-whitespace) script is accepted as
/// "reset to zero committed turns" rather than the usual "no complete turn
/// submitted" parse error, so popping the very first turn back into the draft
/// and never recommitting it is a valid end state.
pub async fn rebuild_tracker_history(
    State(app): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<TrackerEventsRequest>,
) -> Response {
    let mut sessions = lock(&app);
    let Some(session) = sessions.get_mut(&id) else {
        return not_found();
    };

    let applied = if req.text.trim().is_empty() {
        AppliedTurns { belief: session.initial_belief.clone(), log: Vec::new() }
    } else {
        match apply_turns_from(
            &session.initial_belief,
            &req.text,
            &app.dexes,
            &session.inference_config,
            session.active_per_side,
            0,
        ) {
            Ok(applied) => applied,
            Err(SubmitTrackerError::Parse(error)) => return parse_error_response(error),
            Err(SubmitTrackerError::Unprocessable(message)) => return unprocessable(message),
            Err(SubmitTrackerError::Internal(message)) => return internal_error(message),
        }
    };

    session.belief = applied.belief;
    session.turn_count = applied.log.len() as u16;
    session.log = applied.log;
    session.script = if req.text.trim().is_empty() { Vec::new() } else { vec![req.text] };

    let view = mapping::battle_view_from_belief(
        &session.belief,
        session.active_per_side,
        session.brought_per_side,
        session.inference_config.legal_items.as_ref(),
    );
    Json(GetTrackerResponse {
        state: view,
        log: session.log.clone(),
        script: session.script.join("\n"),
    })
    .into_response()
}

/// `GET /api/tracker/{id}/completions` — name pools for the tracker input
/// bar's autocomplete, scoped to this specific match rather than the entire
/// dex (see `TrackerCompletionsDto`'s doc comment for why: a full move/ability
/// dex dump would suggest moves no Pokemon in this battle could ever use).
/// Held items are deliberately NOT returned here — they aren't
/// species-constrained, and the frontend already has the full item catalog
/// (`frontend/src/lib/items.ts`).
pub async fn get_tracker_completions(State(app): State<AppState>, Path(id): Path<String>) -> Response {
    let sessions = lock(&app);
    let Some(session) = sessions.get(&id) else {
        return not_found();
    };

    let mut moves: Vec<String> = Vec::new();
    let mut abilities: Vec<String> = Vec::new();
    for species in &session.roster_species {
        if let Some(learnable) = app.dexes.learnset_dex.get(species) {
            for mv in learnable {
                let name = move_name(mv);
                if !moves.contains(&name) {
                    moves.push(name);
                }
            }
        }
        if let Some(data) = app.dexes.pokemon_dex.get(species) {
            for ability in &data.abilities {
                let name = humanize_identifier(format!("{ability:?}"));
                if !abilities.contains(&name) {
                    abilities.push(name);
                }
            }
        }
    }
    let species: Vec<String> = session
        .roster_species
        .iter()
        .map(|s| humanize_identifier(format!("{s:?}")))
        .collect();
    moves.sort();
    abilities.sort();

    Json(TrackerCompletionsDto { species, moves, abilities }).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use poke_rust::data::pokemon_move::PokemonMove;
    use poke_rust::data::species::Species;
    use poke_rust::information::information::{CantReason, SwitchState};
    use poke_rust::information::unknowns::{PokemonHP, UnknownPokemonState};
    use poke_rust::state::dex_data::PokemonData;
    use std::sync::OnceLock;

    static POKEMON_DEX: OnceLock<HashMap<Species, PokemonData>> = OnceLock::new();
    fn pokemon_dex() -> &'static HashMap<Species, PokemonData> {
        POKEMON_DEX.get_or_init(|| poke_rust::state::dex_data::parse_pokemon_dex("../pokemon_info/showdownDex.txt"))
    }

    static DEXES: OnceLock<Dexes> = OnceLock::new();
    fn dexes() -> &'static Dexes {
        DEXES.get_or_init(|| Dexes {
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
    }

    fn tracker_session_1v1() -> TrackerSession {
        TrackerSession {
            belief: belief_1v1(),
            initial_belief: belief_1v1(),
            script: Vec::new(),
            active_per_side: 1,
            brought_per_side: 1,
            inference_config: InferenceConfig::default(),
            log: Vec::new(),
            turn_count: 0,
            roster_species: vec![Species::Pikachu, Species::Garchomp],
        }
    }

    fn make_active(species: Species, fainted: bool) -> UnknownPokemonState {
        let mut mon = UnknownPokemonState::from_opponent_species(species, pokemon_dex(), 50);
        mon.fainted = fainted;
        mon
    }

    /// A 1v1 belief with both actives alive and no bench.
    fn belief_1v1() -> UnknownBattleState {
        UnknownBattleState {
            active_per_side: 1,
            back_mons_per_side: 0,
            p1_active_mons: vec![make_active(Species::Pikachu, false)],
            p2_active_mons: vec![make_active(Species::Garchomp, false)],
            p1_known_back_mons: Vec::new(),
            p2_known_back_mons: Vec::new(),
            p1_possible_back_mons: Vec::new(),
            p2_possible_back_mons: Vec::new(),
            p1_fainted_mons: Vec::new(),
            p2_fainted_mons: Vec::new(),
            p1_unresolved_zoroark_count: 0,
            p2_unresolved_zoroark_count: 0,
            p1_roster_templates: Vec::new(),
            p2_roster_templates: Vec::new(),
            turn_number: 1,
            turn_started: false,
            turn_ended: false,
            p1_has_tera: true,
            p2_has_tera: true,
            p1_has_mega: true,
            p2_has_mega: true,
            weather: None,
            weather_turns: None,
            weather_setter_mon_idx: None,
            pseudo_weathers: Vec::new(),
            pseudo_weather_turns: Vec::new(),
            terrain: None,
            terrain_turns: None,
            terrain_setter_mon_idx: None,
            p1_side_conditions: Vec::new(),
            p1_side_condition_turns: Vec::new(),
            p1_side_condition_setters: Vec::new(),
            p2_side_conditions: Vec::new(),
            p2_side_condition_turns: Vec::new(),
            p2_side_condition_setters: Vec::new(),
            p1_slot_conditions: vec![Vec::new()],
            p2_slot_conditions: vec![Vec::new()],
            self_switch_pending: None,
            items_consumed_this_turn: Vec::new(),
            last_move_on_field: None,
            sub_damage_dealt: 0,
            round_used_this_turn: false,
            predicates: Vec::new(),
        }
    }

    fn p1() -> FieldSlot {
        FieldSlot { player: Player::P1, slot_index: 0 }
    }
    fn o1() -> FieldSlot {
        FieldSlot { player: Player::P2, slot_index: 0 }
    }
    fn leaf(kind: EventKind) -> InformationEvent {
        InformationEvent { kind, reactions: Vec::new() }
    }

    #[test]
    fn rejects_turn_where_a_slot_did_nothing() {
        let belief = belief_1v1();
        // Only P1 acted; P2's slot recorded nothing at all.
        let events = vec![leaf(EventKind::MoveUsed {
            user: p1(),
            move_used: PokemonMove::Thunderbolt,
            targets: vec![o1()],
        })];
        let err = validate_turn_completeness(&events, &belief, 1).unwrap_err();
        assert!(err.contains("P2"), "{err}");
    }

    #[test]
    fn accepts_turn_where_every_slot_acted() {
        let belief = belief_1v1();
        let events = vec![
            leaf(EventKind::MoveUsed {
                user: p1(),
                move_used: PokemonMove::Thunderbolt,
                targets: vec![o1()],
            }),
            leaf(EventKind::Cant {
                slot: o1(),
                reason: CantReason::Paralysis,
            }),
        ];
        assert!(validate_turn_completeness(&events, &belief, 1).is_ok());
    }

    #[test]
    fn accepts_switch_as_the_slots_action() {
        let belief = belief_1v1();
        let events = vec![
            leaf(EventKind::MoveUsed {
                user: p1(),
                move_used: PokemonMove::Thunderbolt,
                targets: vec![o1()],
            }),
            leaf(EventKind::Switch(SwitchState {
                slot: o1(),
                species: Species::Tyranitar,
                level: 50,
                hp: PokemonHP::Percent(100),
                status: None,
                tera_type: None,
                disguise_species: None,
                max_hp: 0,
            })),
        ];
        assert!(validate_turn_completeness(&events, &belief, 1).is_ok());
    }

    #[test]
    fn self_switch_move_and_its_own_switch_both_count_for_the_same_slot() {
        // U-turn/Volt Switch: the acting slot both used a move AND recorded a
        // switch of itself in the same turn — `slot_acted` must match on
        // either event kind, not require exactly one.
        let belief = belief_1v1();
        let events = vec![
            leaf(EventKind::MoveUsed {
                user: p1(),
                move_used: PokemonMove::VoltSwitch,
                targets: vec![o1()],
            }),
            leaf(EventKind::Switch(SwitchState {
                slot: p1(),
                species: Species::Charizard,
                level: 50,
                hp: PokemonHP::Percent(100),
                status: None,
                tera_type: None,
                disguise_species: None,
                max_hp: 0,
            })),
            leaf(EventKind::MoveUsed {
                user: o1(),
                move_used: PokemonMove::Protect,
                targets: vec![],
            }),
        ];
        assert!(validate_turn_completeness(&events, &belief, 1).is_ok());
    }

    #[test]
    fn rejects_unfilled_lead_slot_on_the_first_turn() {
        // Nobody active yet (fresh session) — P2 never sends out a lead.
        let mut belief = belief_1v1();
        belief.p1_active_mons.clear();
        belief.p2_active_mons.clear();
        let events = vec![leaf(EventKind::SimultaneousSwitch {
            switches: vec![SwitchState {
                slot: p1(),
                species: Species::Pikachu,
                level: 50,
                hp: PokemonHP::Number(100),
                status: None,
                tera_type: None,
                disguise_species: None,
                max_hp: 100,
            }],
        })];
        let err = validate_turn_completeness(&events, &belief, 1).unwrap_err();
        assert!(err.contains("P2"), "{err}");
    }

    #[test]
    fn skips_fainted_slot_with_no_healthy_reserves() {
        let mut belief = belief_1v1();
        belief.p2_active_mons[0].fainted = true;
        // P1 acts; P2's fainted slot has no bench at all, so nothing to record.
        let events = vec![leaf(EventKind::MoveUsed {
            user: p1(),
            move_used: PokemonMove::Protect,
            targets: vec![],
        })];
        assert!(validate_turn_completeness(&events, &belief, 1).is_ok());
    }

    #[test]
    fn does_not_skip_fainted_slot_with_a_healthy_reserve_available() {
        let mut belief = belief_1v1();
        belief.p2_active_mons[0].fainted = true;
        belief.p2_known_back_mons.push(make_active(Species::Tyranitar, false));
        let events = vec![leaf(EventKind::MoveUsed {
            user: p1(),
            move_used: PokemonMove::Protect,
            targets: vec![],
        })];
        let err = validate_turn_completeness(&events, &belief, 1).unwrap_err();
        assert!(err.contains("P2"), "{err}");
    }

    #[test]
    fn replacement_only_batch_does_not_require_healthy_slots_to_act() {
        let mut belief = belief_1v1();
        belief.p1_active_mons[0].fainted = true;
        belief
            .p1_known_back_mons
            .push(make_active(Species::Charizard, false));
        let events = vec![leaf(EventKind::Switch(SwitchState {
            slot: p1(),
            species: Species::Charizard,
            level: 50,
            hp: PokemonHP::Number(153),
            status: None,
            tera_type: None,
            disguise_species: None,
            max_hp: 153,
        }))];
        assert!(validate_turn_completeness(&events, &belief, 1).is_ok());
    }

    #[test]
    fn slot_knocked_out_before_its_action_does_not_need_a_separate_action_event() {
        let belief = belief_1v1();
        let events = vec![InformationEvent {
            kind: EventKind::MoveUsed {
                user: o1(),
                move_used: PokemonMove::Earthquake,
                targets: vec![p1()],
            },
            reactions: vec![leaf(EventKind::DamageDealt {
                target: p1(),
                new_hp: PokemonHP::Number(0),
                max_hp: 100,
            })],
        }];
        assert!(validate_turn_completeness(&events, &belief, 1).is_ok());
    }

    #[test]
    fn side_eliminated_true_once_last_active_faints_with_no_reserve() {
        let mut belief = belief_1v1();
        belief.p2_active_mons[0].fainted = true;
        assert!(side_eliminated(&belief, Player::P2, 1));
        // The other side, still alive, must not be reported eliminated.
        assert!(!side_eliminated(&belief, Player::P1, 1));
    }

    #[test]
    fn side_eliminated_false_with_a_known_healthy_reserve() {
        let mut belief = belief_1v1();
        belief.p2_active_mons[0].fainted = true;
        belief
            .p2_known_back_mons
            .push(make_active(Species::Tyranitar, false));
        assert!(!side_eliminated(&belief, Player::P2, 1));
    }

    #[test]
    fn side_eliminated_false_with_only_a_possible_healthy_reserve() {
        // Soundness: an unrevealed bench mon that MIGHT still be alive must
        // keep this false — never claim elimination is certain when it isn't.
        let mut belief = belief_1v1();
        belief.p2_active_mons[0].fainted = true;
        belief
            .p2_possible_back_mons
            .push(make_active(Species::Tyranitar, false));
        assert!(!side_eliminated(&belief, Player::P2, 1));
    }

    #[test]
    fn side_eliminated_false_before_a_lead_has_been_sent_out() {
        // Fresh session, nobody active yet — an empty active Vec must never
        // read as "every active mon fainted."
        let mut belief = belief_1v1();
        belief.p2_active_mons.clear();
        assert!(!side_eliminated(&belief, Player::P2, 1));
    }

    #[test]
    fn apply_tracker_text_commits_multiple_complete_turns() {
        let mut session = tracker_session_1v1();
        let response = apply_tracker_text(
            &mut session,
            "p1 protect\no1 protect\nendofturn\np1 protect\no1 protect\nendofturn",
            dexes(),
        )
        .expect("two complete turns should commit");

        assert_eq!(session.turn_count, 2);
        assert_eq!(session.log.len(), 2);
        assert_eq!(response.log_delta.len(), 2);
        assert_eq!(response.log_delta[0].label, "Turn 1");
        assert_eq!(response.log_delta[1].label, "Turn 2");
    }

    #[test]
    fn apply_tracker_text_rolls_back_all_turns_when_a_later_turn_is_incomplete() {
        let mut session = tracker_session_1v1();
        let belief_before = format!("{:?}", session.belief);
        let result = apply_tracker_text(
            &mut session,
            "p1 protect\no1 protect\nendofturn\np1 protect\nendofturn",
            dexes(),
        );

        assert!(matches!(result, Err(SubmitTrackerError::Unprocessable(_))));
        assert_eq!(session.turn_count, 0);
        assert!(session.log.is_empty());
        assert_eq!(format!("{:?}", session.belief), belief_before);
    }

    #[test]
    fn randomized_invalid_later_lines_report_their_line_and_never_commit() {
        let iterations = std::env::var("POKERUST_TRACKER_INVALID_FUZZ_ITERS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(128);
        for seed in 0..iterations {
            let mut session = tracker_session_1v1();
            let belief_before = format!("{:?}", session.belief);
            let text = format!(
                "p1 protect\no1 protect\nendofturn\n# seed {seed}\np1 definitely_not_a_tracker_token_{seed}\no1 protect\nendofturn"
            );
            let result = apply_tracker_text(&mut session, &text, dexes());
            match result {
                Err(SubmitTrackerError::Parse(error)) => assert_eq!(error.line, 5, "seed={seed}"),
                Err(other) => panic!("seed={seed}: expected a parse failure, got {other:?}"),
                Ok(_) => panic!("seed={seed}: invalid tracker text unexpectedly committed"),
            }
            assert_eq!(session.turn_count, 0, "seed={seed}");
            assert!(session.log.is_empty(), "seed={seed}");
            assert_eq!(format!("{:?}", session.belief), belief_before, "seed={seed}");
        }
    }

    /// `PUT /history`'s whole reason for existing is that it must reproduce
    /// exactly what the append path (`POST /events`) would have produced, had
    /// the same text been submitted turn-by-turn from a fresh session — that's
    /// what makes "edit a past event, resubmit the full corrected script" sound
    /// rather than just a different (and possibly diverging) code path.
    #[test]
    fn rebuild_from_initial_belief_reproduces_the_equivalent_append_sequence() {
        let text = "p1 protect\no1 protect\nendofturn\np1 thunderbolt o1 62%\no1 protect\nendofturn";

        let mut appended = tracker_session_1v1();
        apply_tracker_text(&mut appended, text, dexes()).expect("append path should succeed");

        let rebuilt = apply_turns_from(
            &tracker_session_1v1().initial_belief,
            text,
            dexes(),
            &InferenceConfig::default(),
            1,
            0,
        )
        .expect("rebuild path should succeed");

        assert_eq!(format!("{:?}", appended.belief), format!("{:?}", rebuilt.belief));
        assert_eq!(appended.log.len(), rebuilt.log.len());
        assert_eq!(appended.turn_count, rebuilt.log.len() as u16);
    }

    /// `apply_turns_from` itself still rejects an empty/all-whitespace script
    /// the same way it always rejected "no endofturn line at all" — this pins
    /// that behavior so the empty-script special case added to
    /// `rebuild_tracker_history` (reset to `initial_belief` with zero turns,
    /// exercised over real HTTP in the Playwright suite) is visibly a
    /// deliberate carve-out at the call site, not something that crept into
    /// the shared turn-application core.
    #[test]
    fn apply_turns_from_rejects_an_empty_script_as_no_complete_turn() {
        let belief = belief_1v1();
        let result = apply_turns_from(&belief, "   \n", dexes(), &InferenceConfig::default(), 1, 0);
        assert!(matches!(result, Err(SubmitTrackerError::Unprocessable(_))));
    }

    /// Soundness guard for `apply_structural_preview`: the facts it establishes
    /// from a PARTIAL turn (here, only P1's move — P2 hasn't acted yet and
    /// there's no `endofturn`) must already agree with what the full six-pass
    /// pipeline confirms once the very same turn is completed for real. If a
    /// future change made the Pass-1-only preview diverge from ground truth
    /// (rather than just being a strict under-approximation of it), this would
    /// mean the live input bar shows the user a fact the eventual commit then
    /// contradicts — exactly the failure mode `apply_structural_preview`'s doc
    /// comment promises can't happen.
    #[test]
    fn structural_preview_of_a_partial_turn_matches_the_eventual_full_turn_end_belief() {
        let move_used = InformationEvent {
            kind: EventKind::MoveUsed {
                user: p1(),
                move_used: PokemonMove::Thunderbolt,
                targets: vec![o1()],
            },
            reactions: vec![leaf(EventKind::DamageDealt {
                target: o1(),
                new_hp: PokemonHP::Percent(50),
                max_hp: 100,
            })],
        };

        // Only P1 has acted so far — exactly what a live `/preview` call sees
        // mid-turn, before P2's action or `endofturn` have been typed.
        let mut preview_belief = belief_1v1();
        apply_structural_preview(
            &mut preview_belief,
            std::slice::from_ref(&move_used),
            pokemon_dex(),
            &dexes().move_dex,
            &dexes().ability_dex,
            &InferenceConfig::default(),
        );

        // The same turn, completed for real through the full pipeline.
        let complete_events = vec![
            move_used.clone(),
            leaf(EventKind::Cant { slot: o1(), reason: CantReason::Other }),
            leaf(EventKind::EndOfTurn),
        ];
        let full_belief = match apply_information(
            UnknownMatchState::Battle(belief_1v1()),
            &complete_events,
            false,
            pokemon_dex(),
            &dexes().move_dex,
            &dexes().ability_dex,
            &InferenceConfig::default(),
        ) {
            UnknownMatchState::Battle(b) => b,
            other => panic!("expected Battle, got {other:?}"),
        };

        assert_eq!(preview_belief.p2_active_mons[0].hp, full_belief.p2_active_mons[0].hp);
        assert!(
            preview_belief.p1_active_mons[0]
                .known_moves
                .contains(&Some(PokemonMove::Thunderbolt)),
            "the preview should already have revealed the move Pass 1 applies directly"
        );
        assert_eq!(
            preview_belief.p1_active_mons[0].known_moves,
            full_belief.p1_active_mons[0].known_moves,
            "a fact Pass 1 confirms from a partial turn must never be contradicted by the full turn"
        );
    }
}
