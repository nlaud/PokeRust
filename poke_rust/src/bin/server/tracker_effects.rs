//! Tracker mode: synthesize the "guaranteed" reactions a user should never
//! have to type by hand — anything that follows deterministically from
//! something they *did* type.
//!
//! Four triggers, matching how the real simulator nests these (see
//! `information::information`'s module docs and
//! `simulator::helpers::apply_reaction_self_boost`):
//!
//! - An **`AbilityRevealed`** node (anywhere in the tree — a standalone
//!   `[slot] [ability]` line, an ability named inline on a move line, or one
//!   synthesized by this module itself for Mega Evolution/Trace) gets its
//!   deterministic on-reveal effect appended as a child, exactly the
//!   "nested-reveal convention" the simulator itself uses (Intimidate's
//!   `-1 atk` per opposing active, a weather-setter's field change, …). The
//!   *ability itself* still has to be named by the user for an ordinary
//!   switch-in reveal — that's new information the engine has no way to
//!   guess — only its guaranteed consequence is auto-added.
//! - A **`MoveUsed`** node gets: its move's always-on `self_boost` (Swords
//!   Dance, Nasty Plot, …); and, generically, every 100%-chance,
//!   non-random-choice entry in `MoveData.secondaries`/`self_secondaries` —
//!   these aren't a hand-curated list, they're *already* sitting in the
//!   parsed move dex (Showdown's move data represents a "pure status" move's
//!   entire guaranteed effect — Thunder Wave's Paralysis, Stealth Rock's
//!   hazard, Rain Dance's weather, Leer's -1 Def — as a clean top-level
//!   `status:`/`sideCondition:`/`weather:`/`boosts:` field, and
//!   `state/dex_data.rs::parse_move_entry` already converts every one of
//!   those into a 100%-chance secondary; see this module's design doc for
//!   the research trail). A target already recorded `Missed`/`Immune`/
//!   `Blocked`, or the move recorded `MoveFailed` at all, is skipped.
//! - A **`MegaEvolution`** node gets its mega form's fixed ability (a mega
//!   forme's dex entry always has exactly one ability slot — no hidden
//!   ability — mirroring `state/battle.rs`'s own mega-evolution ability
//!   resolution) appended as a nested `AbilityRevealed`, itself immediately
//!   cascaded through the same ability-reaction table (the outer recursive
//!   walk's children-pass has already finished by the time a node's own kind
//!   is handled, so a newly-synthesized `AbilityRevealed` can't rely on the
//!   recursion to process it — see `ability_revealed_node`).
//! - **Any node** gets a `Faint` sibling synthesized for any
//!   `DamageDealt`/`Healed`/`SetHp` child that lands exactly on 0 HP — the
//!   real simulator always emits `Faint` as an explicit sibling of the
//!   zero-HP event (never implied by it), and Pass 1's own auto-faint
//!   belt-and-braces only covers the `DamageDealt` arm, so this closes the
//!   `Healed`/`SetHp`-to-0 gap and matches real emitted trees either way.
//!
//! # Scope
//!
//! The move-secondary synthesis intentionally skips `slot_condition`
//! payloads (Wish/Future Sight/Doom Desire need extra snapshot data —
//! attacker stats, ability, item at cast time — the tracker doesn't have
//! readily available) — those stay user-typed. The ability table
//! (Intimidate, the four weather/terrain-setter pairs, Intrepid Sword/
//! Dauntless Shield, Download, Trace) is, and will likely remain, hand-
//! curated: unlike moves, an ability's effect lives entirely in unparsed JS
//! with no structured dex equivalent (confirmed against Download/Trace/
//! Frisk/Forewarn) — Frisk/Forewarn are excluded because they reveal
//! genuinely new information the user has to type anyway (revealing it IS
//! the ability's whole purpose). Weather/terrain-setter *abilities* follow
//! the real precedence rule (see `weather_is_strong` and
//! `simulator::helpers::set_weather`, which this mirrors): a standard
//! weather-setting ability always replaces whatever standard weather is
//! currently active (Sand Stream after Drought overrides Sun with
//! Sandstorm), but fails to activate at all against the three strong/primal
//! weathers (Desolate Land/Primordial Sea/Delta Stream) — those can only be
//! replaced by another strong weather. Terrain has no such exception: a
//! terrain-setting ability always replaces whatever terrain is active, full
//! stop (mirrors `simulator::helpers::set_terrain`, which is unconditional).
//! Because two switches/mega evolutions can land in the *same* turn (e.g.
//! each side mega evolving on the turn's first move), `augment_turn` threads
//! a running scratch of just the field-relevant belief state (weather,
//! terrain) between events in one turn — see its doc comment — so the
//! second event's gate sees the first event's synthesized change instead of
//! stale pre-turn state. Intrepid Sword/Dauntless Shield/one-time abilities
//! are gated on `one_time_ability_used` so a Pokemon that already fired one
//! doesn't get boosted again on a later switch-in. Download only fires when
//! the opposing actives' known Def/SpD bounds make the comparison
//! unambiguous; Trace only fires when exactly one opposing active's ability
//! is already `Known`.

use std::collections::{HashMap, HashSet};

use poke_rust::data::ability::Ability;
use poke_rust::data::pokemon_move::PokemonMove;
use poke_rust::data::species::Species;
use poke_rust::information::information::{EventKind, InformationEvent};
use poke_rust::information::unknowns::{
    PokemonHP, Unknown, UnknownBattleState, UnknownPokemonState,
};
use poke_rust::state::battle::{FieldSlot, Player};
use poke_rust::state::dex_data::{
    MoveData, MoveTarget, PokemonData, PokemonType, Terrain, VolatileStatus, Weather,
};
use poke_rust::state::pokemon::VolatileStatusState;

use crate::tracker_parse::opposing_active_slots;

/// Augment every event in one turn, in order, threading a scratch snapshot of
/// field-level belief state (currently: weather, terrain) between them so a
/// LATER event in the same turn correctly observes an EARLIER event's
/// synthesized change. Concretely: two mega evolutions in one turn, one per
/// side, each with a weather-setting ability — the second must see the
/// first's weather already active to decide whether it's blocked (strong
/// weather) or should override it (standard weather always replaces standard
/// weather). `belief` itself is never mutated; `scratch` is a throwaway clone
/// used only to keep `guaranteed_ability_reactions`'s gates accurate as the
/// turn's events are synthesized one at a time.
pub fn augment_turn(
    events: Vec<InformationEvent>,
    belief: &UnknownBattleState,
    move_dex: &HashMap<PokemonMove, MoveData>,
    pokemon_dex: &HashMap<Species, PokemonData>,
) -> Vec<InformationEvent> {
    let mut scratch = belief.clone();
    let mut live_slots: HashSet<FieldSlot> = [Player::P1, Player::P2]
        .into_iter()
        .flat_map(|player| {
            let mons = match player {
                Player::P1 => &belief.p1_active_mons,
                Player::P2 => &belief.p2_active_mons,
            };
            mons.iter()
                .enumerate()
                .filter(|(_, mon)| !mon.fainted && !is_zero_hp(&mon.hp))
                .map(move |(index, _)| FieldSlot {
                    player,
                    slot_index: index as u8,
                })
        })
        .collect();
    events
        .into_iter()
        .flat_map(|e| {
            match &e.kind {
                EventKind::Switch(sw) => {
                    if !is_zero_hp(&sw.hp) {
                        live_slots.insert(sw.slot);
                    } else {
                        live_slots.remove(&sw.slot);
                    }
                }
                EventKind::SimultaneousSwitch { switches } => {
                    for sw in switches {
                        if !is_zero_hp(&sw.hp) {
                            live_slots.insert(sw.slot);
                        } else {
                            live_slots.remove(&sw.slot);
                        }
                    }
                }
                _ => {}
            }
            let mut augmented =
                augment_with_live_slots(e, &scratch, move_dex, pokemon_dex, &live_slots, true);
            fold_event_into_synthesis_scratch(&mut scratch, &augmented, pokemon_dex);
            remove_fainted_slots(&augmented, &mut live_slots);
            // The turn's own events are all folded into `scratch` by this
            // point (we're at the LAST event — `split_into_turns` always
            // appends `EndOfTurn` last) — compare it against the pre-turn
            // `belief` to decide which timers are guaranteed to expire
            // (`Known(1)`, unchanged by anything typed this turn) and attach
            // their clears as this node's own reactions, matching where the
            // real engine's `EndOfTurn` reactions carry them.
            //
            // Skip entirely if this same turn's events leave either side with
            // every active slot down and no CONFIRMED healthy reserve
            // (fuzz-discovered): tracker syntax requires every complete turn
            // to end with an explicit `endofturn` sentinel even when the last
            // action was the game-ending KO, so the parsed-back event stream
            // always carries a `EventKind::EndOfTurn` node regardless of
            // whether the real engine ever ran a genuine end-of-turn pass for
            // it — `step_action_queue` skips `end_turn` entirely once the
            // battle is already decided (see `simulator/mod.rs`), so a
            // synthesized clear here would claim a duration tick that never
            // actually happened. This can't be answered with full certainty
            // from a fog-of-war belief — the opponent's exact bench size may
            // still be ambiguous — so this deliberately checks only `known_
            // back` (a *confirmed* healthy reserve), not `possible_back`:
            // requiring certainty of "the battle continues" before trusting a
            // tick occurred, rather than certainty of "it doesn't" before
            // skipping. Sound: this only ever WITHHOLDS a clear the real
            // engine might still have emitted, never fabricates one. Uses
            // active-wipe (not full elimination) specifically because
            // `end_turn` genuinely DOES still run, and duration timers DO
            // still tick, on an ordinary "fainted but a reserve is waiting"
            // turn — see the wide unit tests below distinguishing the two.
            let side_ambiguously_wiped = |player: Player| -> bool {
                let (active, known_back) = match player {
                    Player::P1 => (&scratch.p1_active_mons, &scratch.p1_known_back_mons),
                    Player::P2 => (&scratch.p2_active_mons, &scratch.p2_known_back_mons),
                };
                !active.is_empty()
                    && active.iter().all(|mon| mon.fainted || is_zero_hp(&mon.hp))
                    && !known_back
                        .iter()
                        .any(|mon| !mon.fainted && !is_zero_hp(&mon.hp))
            };
            let game_might_already_be_over = matches!(augmented.kind, EventKind::EndOfTurn)
                && (side_ambiguously_wiped(Player::P1) || side_ambiguously_wiped(Player::P2));
            if matches!(augmented.kind, EventKind::EndOfTurn) && !game_might_already_be_over {
                augmented
                    .reactions
                    .extend(synthesize_expiry_clears(belief, &scratch));
            }
            let top_level_faint = match &augmented.kind {
                EventKind::DamageDealt { target, new_hp, .. }
                | EventKind::Healed { target, new_hp, .. }
                | EventKind::SetHp { target, new_hp, .. }
                    if is_zero_hp(new_hp) =>
                {
                    Some(*target)
                }
                _ => None,
            };
            let mut out = vec![augmented];
            if let Some(slot) = top_level_faint {
                let faint = leaf(EventKind::Faint { slot });
                fold_event_into_synthesis_scratch(&mut scratch, &faint, pokemon_dex);
                live_slots.remove(&slot);
                out.push(faint);
            }
            out
        })
        .collect()
}

fn remove_fainted_slots(event: &InformationEvent, live_slots: &mut HashSet<FieldSlot>) {
    match &event.kind {
        EventKind::Faint { slot } => {
            live_slots.remove(slot);
        }
        EventKind::DamageDealt {
            target,
            new_hp: PokemonHP::Number(0) | PokemonHP::Percent(0),
            ..
        }
        | EventKind::Healed {
            target,
            new_hp: PokemonHP::Number(0) | PokemonHP::Percent(0),
            ..
        }
        | EventKind::SetHp {
            target,
            new_hp: PokemonHP::Number(0) | PokemonHP::Percent(0),
            ..
        } => {
            live_slots.remove(target);
        }
        _ => {}
    }
    for reaction in &event.reactions {
        remove_fainted_slots(reaction, live_slots);
    }
}

/// Synthesize the field-effect EXPIRIES a user should never have to type.
/// `inference.rs::decrement_unknown_turns` deliberately decrements a belief's
/// weather/terrain/pseudo-weather/side-condition timers WITHOUT clearing the
/// field itself, deferring that to an event — `WeatherChanged{None}`,
/// `TerrainChanged{None}`, `PseudoWeatherEnd`, `SideConditionEnd` — the same
/// way the real simulator's `decrement_effect_timers` (`simulator/helpers.rs`)
/// both decrements AND emits that event once `turns == 1`. Tracker mode has
/// no simulator to emit it, so without this, a belief's field effects tick
/// down internally but never actually clear.
///
/// `belief` is the state as it stood BEFORE this turn's own events (passed
/// into `augment_turn`); `scratch` is the running synthesis snapshot after
/// every one of this turn's own events has been folded in (see
/// `augment_turn`'s doc comment) — comparing the two tells us whether
/// something the user typed THIS turn already changed the effect (a new
/// weather, an explicit `weather none`, a fresh side condition), in which
/// case there's nothing left to synthesize.
///
/// Only ever fires on `Known(1)` — a *guaranteed* expiry this turn:
/// - Fixed-duration effects (Trick Room=5, Tailwind=4, Safeguard=5, …) reach
///   `Known(1)` on their exact last turn, same as the real engine.
/// - Item-extendable effects (weather/terrain's `Possibly([5, 8])` for a
///   rock/Terrain Extender, screens' `Possibly([5, 8])` for Light Clay) only
///   ever COLLAPSE to `Known` once the ambiguous turn-5 branch has already
///   been soundly excluded (see `apply_end_of_turn`'s three-way resolution
///   in `inference.rs`) — so this never fires at the ambiguous turn-5
///   midpoint, only once a genuine `Known(1)` is reached.
/// - `Known(0)` (the permanent/no-countdown sentinel for primordial weather
///   and entry hazards) and any still-`Possibly` timer are left untouched.
fn synthesize_expiry_clears(
    belief: &UnknownBattleState,
    scratch: &UnknownBattleState,
) -> Vec<InformationEvent> {
    let mut clears = Vec::new();

    if scratch.weather == belief.weather && matches!(belief.weather_turns, Some(Unknown::Known(1)))
    {
        clears.push(leaf(EventKind::WeatherChanged { weather: None }));
    }

    if scratch.terrain == belief.terrain && matches!(belief.terrain_turns, Some(Unknown::Known(1)))
    {
        clears.push(leaf(EventKind::TerrainChanged { terrain: None }));
    }

    for (i, pw) in belief.pseudo_weathers.iter().enumerate() {
        if !scratch.pseudo_weathers.contains(pw) {
            continue; // already ended by an explicit event this turn
        }
        if matches!(belief.pseudo_weather_turns.get(i), Some(Unknown::Known(1))) {
            clears.push(leaf(EventKind::PseudoWeatherEnd { effect: pw.clone() }));
        }
    }

    for player in [Player::P1, Player::P2] {
        let (conditions, turns, scratch_conditions) = match player {
            Player::P1 => (
                &belief.p1_side_conditions,
                &belief.p1_side_condition_turns,
                &scratch.p1_side_conditions,
            ),
            Player::P2 => (
                &belief.p2_side_conditions,
                &belief.p2_side_condition_turns,
                &scratch.p2_side_conditions,
            ),
        };
        for (i, sc) in conditions.iter().enumerate() {
            let still_active = scratch_conditions
                .iter()
                .any(|active| std::mem::discriminant(active) == std::mem::discriminant(sc));
            if !still_active {
                continue; // already ended by an explicit event this turn
            }
            if matches!(turns.get(i), Some(Unknown::Known(1))) {
                clears.push(leaf(EventKind::SideConditionEnd {
                    side: player,
                    condition: sc.clone(),
                }));
            }
        }
    }

    clears
}

/// Recursively scan `event` (and its already-synthesized reactions) for
/// `WeatherChanged`/`TerrainChanged` and fold the LAST one found into
/// `scratch` — used by `augment_turn` to keep the gates in
/// `guaranteed_ability_reactions` accurate across a turn's events. Only
/// weather/terrain are threaded: they're the only belief fields the ability
/// table gates a decision on that another event earlier in the SAME turn can
/// change (Intimidate/Download/Trace/one-time boosts don't have this
/// same-turn ordering dependency).
fn active_mon_mut(
    state: &mut UnknownBattleState,
    slot: FieldSlot,
) -> Option<&mut UnknownPokemonState> {
    match slot.player {
        Player::P1 => state.p1_active_mons.get_mut(slot.slot_index as usize),
        Player::P2 => state.p2_active_mons.get_mut(slot.slot_index as usize),
    }
}

fn roster_mon_for_species(
    state: &UnknownBattleState,
    player: Player,
    species: &Species,
) -> Option<UnknownPokemonState> {
    let buckets = match player {
        Player::P1 => [
            &state.p1_active_mons,
            &state.p1_known_back_mons,
            &state.p1_possible_back_mons,
            &state.p1_fainted_mons,
        ],
        Player::P2 => [
            &state.p2_active_mons,
            &state.p2_known_back_mons,
            &state.p2_possible_back_mons,
            &state.p2_fainted_mons,
        ],
    };
    buckets.into_iter().find_map(|bucket| {
        bucket.iter().find_map(|mon| {
            matches!(&mon.possible_species, Unknown::Known(actual) if actual == species)
                .then(|| mon.clone())
        })
    })
}

fn fold_switch_into_synthesis_scratch(
    state: &mut UnknownBattleState,
    sw: &poke_rust::information::information::SwitchState,
    pokemon_dex: &HashMap<Species, PokemonData>,
) {
    let mut incoming =
        roster_mon_for_species(state, sw.slot.player, &sw.species).unwrap_or_else(|| {
            UnknownPokemonState::from_opponent_species(sw.species.clone(), pokemon_dex, sw.level)
        });
    incoming.possible_species = Unknown::Known(sw.species.clone());
    incoming.level = sw.level;
    incoming.hp = sw.hp.clone();
    incoming.status = sw.status.clone();
    incoming.fainted = false;
    incoming.last_used_move = None;
    if let Some(data) = pokemon_dex.get(&sw.species)
        && let [only] = data.abilities.as_slice()
    {
        incoming.possible_abilities = Unknown::Known(only.clone());
    }
    let (active, known_back, possible_back, fainted) = match sw.slot.player {
        Player::P1 => (
            &mut state.p1_active_mons,
            &mut state.p1_known_back_mons,
            &mut state.p1_possible_back_mons,
            &mut state.p1_fainted_mons,
        ),
        Player::P2 => (
            &mut state.p2_active_mons,
            &mut state.p2_known_back_mons,
            &mut state.p2_possible_back_mons,
            &mut state.p2_fainted_mons,
        ),
    };
    let is_incoming = |mon: &UnknownPokemonState| matches!(&mon.possible_species, Unknown::Known(species) if species == &sw.species);
    known_back.retain(|mon| !is_incoming(mon));
    possible_back.retain(|mon| !is_incoming(mon));
    fainted.retain(|mon| !is_incoming(mon));
    let index = sw.slot.slot_index as usize;
    if index < active.len() {
        let outgoing = std::mem::replace(&mut active[index], incoming);
        if !outgoing.fainted
            && !is_zero_hp(&outgoing.hp)
            && !matches!(
                &outgoing.possible_species,
                Unknown::Known(species) if species == &sw.species
            )
        {
            known_back.push(outgoing);
        }
    } else if index == active.len() {
        active.push(incoming);
    }
}

fn fold_event_kind_into_synthesis_scratch(
    state: &mut UnknownBattleState,
    kind: &EventKind,
    pokemon_dex: &HashMap<Species, PokemonData>,
) {
    match kind {
        EventKind::Switch(sw) => fold_switch_into_synthesis_scratch(state, sw, pokemon_dex),
        EventKind::SimultaneousSwitch { switches } => {
            let mut ordered: Vec<_> = switches.iter().collect();
            ordered.sort_by_key(|sw| {
                (
                    match sw.slot.player {
                        Player::P1 => 0u8,
                        Player::P2 => 1u8,
                    },
                    sw.slot.slot_index,
                )
            });
            for sw in ordered {
                fold_switch_into_synthesis_scratch(state, sw, pokemon_dex);
            }
        }
        EventKind::MegaEvolution { slot, into } => {
            if let Some(mon) = active_mon_mut(state, *slot) {
                mon.possible_species = Unknown::Known(into.clone());
                mon.is_mega = true;
                if let Some(ability) = pokemon_dex
                    .get(into)
                    .and_then(|data| data.primary_ability.clone())
                {
                    mon.possible_abilities = Unknown::Known(ability);
                }
            }
        }
        EventKind::AbilityRevealed { slot, ability } => {
            if let Some(mon) = active_mon_mut(state, *slot) {
                mon.possible_abilities = Unknown::Known(ability.clone());
            }
        }
        EventKind::IllusionEnded {
            slot,
            actual_species,
        } => {
            if let Some(mon) = active_mon_mut(state, *slot) {
                mon.possible_species = Unknown::Known(actual_species.clone());
            }
        }
        EventKind::BoostChanged {
            target,
            boost_idx,
            stages,
        } => {
            if let Some(mon) = active_mon_mut(state, *target)
                && let Some(boost) = mon.boosts.get_mut(*boost_idx)
            {
                *boost = (*boost + *stages).clamp(-6, 6);
            }
        }
        EventKind::BoostsInverted { target } => {
            if let Some(mon) = active_mon_mut(state, *target) {
                for boost in &mut mon.boosts {
                    *boost = -*boost;
                }
            }
        }
        EventKind::WeatherChanged { weather } => state.weather = weather.clone(),
        EventKind::TerrainChanged { terrain } => state.terrain = terrain.clone(),
        EventKind::PseudoWeatherStart { effect } => {
            if !state.pseudo_weathers.contains(effect) {
                state.pseudo_weathers.push(effect.clone());
            }
        }
        EventKind::PseudoWeatherEnd { effect } => {
            state.pseudo_weathers.retain(|active| active != effect);
        }
        EventKind::SideConditionStart { side, condition } => {
            let conditions = match side {
                Player::P1 => &mut state.p1_side_conditions,
                Player::P2 => &mut state.p2_side_conditions,
            };
            if !conditions
                .iter()
                .any(|active| std::mem::discriminant(active) == std::mem::discriminant(condition))
            {
                conditions.push(condition.clone());
            }
        }
        EventKind::SideConditionEnd { side, condition } => {
            let conditions = match side {
                Player::P1 => &mut state.p1_side_conditions,
                Player::P2 => &mut state.p2_side_conditions,
            };
            conditions.retain(|active| {
                std::mem::discriminant(active) != std::mem::discriminant(condition)
            });
        }
        EventKind::Faint { slot } => {
            if let Some(mon) = active_mon_mut(state, *slot) {
                mon.fainted = true;
            }
        }
        EventKind::DamageDealt { target, new_hp, .. }
        | EventKind::Healed { target, new_hp, .. }
        | EventKind::SetHp { target, new_hp, .. } => {
            if let Some(mon) = active_mon_mut(state, *target) {
                mon.hp = new_hp.clone();
                if matches!(new_hp, PokemonHP::Number(0) | PokemonHP::Percent(0)) {
                    mon.fainted = true;
                }
            }
        }
        _ => {}
    }
}

pub(crate) fn fold_event_into_synthesis_scratch(
    state: &mut UnknownBattleState,
    event: &InformationEvent,
    pokemon_dex: &HashMap<Species, PokemonData>,
) {
    fold_event_kind_into_synthesis_scratch(state, &event.kind, pokemon_dex);
    if let EventKind::MoveUsed {
        user, move_used, ..
    } = &event.kind
        && !move_failed_at_all(&event.reactions)
    {
        if let Some(mon) = active_mon_mut(state, *user) {
            mon.last_used_move = Some(move_used.clone());
        }
        state.last_move_on_field = Some(move_used.clone());
    }
    for reaction in &event.reactions {
        fold_event_into_synthesis_scratch(state, reaction, pokemon_dex);
    }
}

/// Walk `event`'s tree and append guaranteed reactions in place. `belief` is
/// read-only — it reflects the state *before* this turn's lines are applied,
/// which is all the synthesis below needs (none of it depends on anything
/// else this same turn changes first) — EXCEPT weather/terrain, which is why
/// `augment_turn` (not this function directly) is the entry point tracker.rs
/// should call for a whole turn's events; see its doc comment.
pub fn augment_with_guaranteed_effects(
    event: InformationEvent,
    belief: &UnknownBattleState,
    move_dex: &HashMap<PokemonMove, MoveData>,
    pokemon_dex: &HashMap<Species, PokemonData>,
) -> InformationEvent {
    let live_slots: HashSet<FieldSlot> = [Player::P1, Player::P2]
        .into_iter()
        .flat_map(|player| {
            let mons = match player {
                Player::P1 => &belief.p1_active_mons,
                Player::P2 => &belief.p2_active_mons,
            };
            mons.iter()
                .enumerate()
                .filter(|(_, mon)| !mon.fainted && !is_zero_hp(&mon.hp))
                .map(move |(index, _)| FieldSlot {
                    player,
                    slot_index: index as u8,
                })
        })
        .collect();
    augment_with_live_slots(event, belief, move_dex, pokemon_dex, &live_slots, true)
}

fn augment_with_live_slots(
    mut event: InformationEvent,
    belief: &UnknownBattleState,
    move_dex: &HashMap<PokemonMove, MoveData>,
    pokemon_dex: &HashMap<Species, PokemonData>,
    live_slots: &HashSet<FieldSlot>,
    allow_entry_ability_effect: bool,
) -> InformationEvent {
    // Reactions are ordered. Thread field state between siblings so two entry
    // abilities nested under one simultaneous switch see each other's weather
    // or terrain changes just as two top-level events do.
    let mut reaction_scratch = belief.clone();
    // A switch/mega parent establishes the entering identity before its
    // nested ability reactions fire.
    fold_event_kind_into_synthesis_scratch(&mut reaction_scratch, &event.kind, pokemon_dex);
    let child_allows_entry_ability = matches!(
        event.kind,
        EventKind::Switch(_)
            | EventKind::SimultaneousSwitch { .. }
            | EventKind::MegaEvolution { .. }
    );
    event.reactions = event
        .reactions
        .into_iter()
        .map(|reaction| {
            let augmented = augment_with_live_slots(
                reaction,
                &reaction_scratch,
                move_dex,
                pokemon_dex,
                live_slots,
                child_allows_entry_ability,
            );
            fold_event_into_synthesis_scratch(&mut reaction_scratch, &augmented, pokemon_dex);
            augmented
        })
        .collect();

    match &event.kind {
        EventKind::AbilityRevealed { slot, ability } if allow_entry_ability_effect => {
            let mirror_armor_slots: HashSet<FieldSlot> = event
                .reactions
                .iter()
                .filter_map(|reaction| match &reaction.kind {
                    EventKind::AbilityRevealed {
                        slot,
                        ability: Ability::MirrorArmor,
                    } => Some(*slot),
                    _ => None,
                })
                .collect();
            let mut guaranteed = guaranteed_ability_reactions(*slot, ability, belief, live_slots);
            if *ability == Ability::Intimidate {
                guaranteed.retain(|reaction| {
                    !matches!(
                        reaction.kind,
                        EventKind::BoostChanged { target, .. }
                            if mirror_armor_slots.contains(&target)
                    )
                });
            }
            event.reactions.extend(guaranteed);
        }
        EventKind::MoveUsed {
            user,
            move_used,
            targets,
        } => {
            // A charge turn does none of the move's real work: Geomancy's
            // +2/+2/+2, Sky Attack's flinch, Solar Beam's damage all land on the
            // RELEASE turn. Synthesizing them here fabricated them a turn early and
            // then again on release, double-counting.
            //
            // The trigger is the user having explicitly typed `charging`, NOT the
            // move carrying a `charge` flag — which is what makes this correct for
            // free in every skip-the-charge case: sun skips Solar Beam's charge, rain
            // skips Electro Shot's, and Power Herb skips all but Sky Drop. Those are
            // typed as ordinary one-turn move lines with no `charging` marker, so
            // they keep synthesizing normally and the tracker needs no weather or
            // item modelling of its own.
            if is_charge_turn(&event.reactions, *user) {
                // What the charge turn DOES do: the handful of moves that boost while
                // winding up. Mirrors `simulator/mod.rs`'s
                // `handle_charging_and_semi_invulnerability`, including its
                // suppress-when-already-capped behaviour (via `adjusted_boost_delta`).
                // Geomancy is deliberately absent — its boosts are the release turn's
                // `self_boost`.
                if let Some((boost_idx, stages)) = charge_turn_boost(move_used) {
                    let delta = adjusted_boost_delta(belief, *user, boost_idx, stages);
                    if delta != 0 {
                        event.reactions.insert(
                            0,
                            leaf(EventKind::BoostChanged {
                                target: *user,
                                boost_idx,
                                stages: delta,
                            }),
                        );
                    }
                }
            } else if let Some(md) = move_dex.get(move_used)
                && !move_failed_at_all(&event.reactions)
            {
                // Power Herb, and Electro Shot in rain, skip the visible charge
                // marker but still grant their charge-turn boost. A matching
                // Charging volatile means this is instead the later release.
                if !is_charge_release(belief, *user, move_used)
                    && let Some((boost_idx, stages)) = charge_turn_boost(move_used)
                {
                    let delta = adjusted_boost_delta(belief, *user, boost_idx, stages);
                    if delta != 0 {
                        event.reactions.insert(
                            0,
                            leaf(EventKind::BoostChanged {
                                target: *user,
                                boost_idx,
                                stages: delta,
                            }),
                        );
                    }
                }
                let connected_somewhere = targets.is_empty()
                    || targets.iter().any(|target| {
                        live_slots.contains(target)
                            && !target_has_failed(&event.reactions, *target)
                            && !is_semi_invulnerable(belief, *target)
                    });
                if connected_somewhere {
                    for (idx, &delta) in md.self_boost.iter().enumerate() {
                        let delta = adjusted_boost_delta(belief, *user, idx, delta);
                        if delta != 0 {
                            event.reactions.push(leaf(EventKind::BoostChanged {
                                target: *user,
                                boost_idx: idx,
                                stages: delta,
                            }));
                        }
                    }
                }

                // Field-level payloads (weather/terrain/pseudo-weather/side
                // condition) apply once per move use, regardless of which
                // bucket housed them — see this module's doc comment.
                if connected_somewhere {
                    for sec in md
                        .secondaries
                        .iter()
                        .chain(md.self_secondaries.iter())
                        .filter(|s| s.chance == 100 && s.random_choices.is_empty())
                    {
                        synthesize_field_effects(
                            &mut event.reactions,
                            &sec.effect,
                            *user,
                            &md.target,
                            belief,
                        );
                    }
                }

                // Per-Pokemon payloads (status/volatile/boosts): `secondaries`
                // normally apply to each explicitly-named target, `self_secondaries`
                // to the user. BUT a self-targeting move's own top-level
                // `status:`/`volatileStatus:`/`sideCondition:` effect (Protect's
                // `protect` volatile, Focus Energy, King's Shield, …) is ALSO parsed
                // into `secondaries` — `parse_move_entry` converts a bare top-level
                // field the same way regardless of the move's own `target`, only a
                // `self: { ... }` block earns `self_secondaries` (see this module's
                // doc comment and `state/dex_data.rs::parse_move_entry`). For a
                // `SelfTarget` move, `targets` is always empty (the tracker grammar
                // never lets a target token name the user — see `parse_move_line`),
                // so without this the effect would be silently dropped entirely; the
                // only sound reading of "connected target" for a self-only move is
                // the user itself.
                let secondary_targets: &[FieldSlot] = if md.target == MoveTarget::SelfTarget {
                    std::slice::from_ref(user)
                } else {
                    targets
                };
                for sec in md
                    .secondaries
                    .iter()
                    .filter(|s| s.chance == 100 && s.random_choices.is_empty())
                {
                    for &target in secondary_targets {
                        if live_slots.contains(&target)
                            && !target_has_failed(&event.reactions, target)
                            && !is_semi_invulnerable(belief, target)
                        {
                            synthesize_per_mon_effects(
                                &mut event.reactions,
                                &sec.effect,
                                *user,
                                target,
                                belief,
                            );
                        }
                    }
                }
                if connected_somewhere {
                    for sec in md
                        .self_secondaries
                        .iter()
                        .filter(|s| s.chance == 100 && s.random_choices.is_empty())
                    {
                        if !target_has_failed(&event.reactions, *user) {
                            synthesize_per_mon_effects(
                                &mut event.reactions,
                                &sec.effect,
                                *user,
                                *user,
                                belief,
                            );
                        }
                    }
                }
            }
        }
        EventKind::MegaEvolution { slot, into } => {
            if let Some(ability) = pokemon_dex
                .get(into)
                .and_then(|d| d.primary_ability.clone())
            {
                event
                    .reactions
                    .push(ability_revealed_node(*slot, ability, belief, live_slots));
            }
        }
        _ => {}
    }

    synthesize_guaranteed_faints(&mut event.reactions);
    event
}

/// A move-wide failure (the user typed `fail`/`failed` anywhere on the line)
/// blocks every guaranteed consequence of the move — self-boost, field
/// effects, per-target effects alike — not just the specific slot it was
/// recorded against (the exact slot `MoveFailed.slot` names depends on
/// whichever target was "current" when the user typed it — see
/// `tracker_parse.rs`'s "flat nesting" token-stream convention — so this
/// checks for presence, not a specific slot match).
fn move_failed_at_all(reactions: &[InformationEvent]) -> bool {
    reactions
        .iter()
        .any(|r| matches!(r.kind, EventKind::MoveFailed { .. }))
}

/// Is this `MoveUsed` the CHARGE turn of a two-turn move — i.e. did the user
/// explicitly type `charging` for this slot?
///
/// Deliberately keyed on the typed marker rather than on the move carrying a
/// `charge` flag. Every skip-the-charge case then falls out for free: harsh sun
/// skips Solar Beam's and Solar Blade's charge, rain skips Electro Shot's, and
/// Power Herb skips all of them except Sky Drop. Those are typed as ordinary
/// one-turn move lines with no `charging` token, so they still synthesize the
/// move's real effects — and the tracker needs no weather or item modelling to
/// get that right.
///
/// Presence-checked rather than slot-matched in the same spirit as
/// `move_failed_at_all`, but scoped to the user: `charging` is only ever
/// meaningful for the mon using the move.
fn is_charge_turn(reactions: &[InformationEvent], user: FieldSlot) -> bool {
    reactions
        .iter()
        .any(|r| matches!(r.kind, EventKind::ChargingMove { user: u, .. } if u == user))
}

/// The stat boost a move grants on its CHARGE turn (not its release turn), as
/// `(boost_idx, stages)`.
///
/// Mirrors `simulator/mod.rs::handle_charging_and_semi_invulnerability`. Geomancy is
/// deliberately absent: it charges silently and its +2 SpA/SpD/Spe is the release
/// turn's ordinary `self_boost`, which the normal synthesis path already handles.
fn charge_turn_boost(move_used: &PokemonMove) -> Option<(usize, i8)> {
    match move_used {
        // boosts[2] = Special Attack.
        PokemonMove::ElectroShot | PokemonMove::MeteorBeam => Some((2, 1)),
        // boosts[1] = Defense.
        PokemonMove::SkullBash => Some((1, 1)),
        _ => None,
    }
}

fn is_charge_release(
    belief: &UnknownBattleState,
    user: FieldSlot,
    move_used: &PokemonMove,
) -> bool {
    mon_at(belief, user).is_some_and(|mon| {
        mon.volatiles.iter().any(
            |volatile| matches!(volatile, VolatileStatusState::Charging(active, _) if active == move_used),
        )
    })
}

/// Is `slot` currently off the field mid-charge (Fly, Dig, Dive, Bounce, Phantom
/// Force, Shadow Force, Sky Drop)? Nothing aimed at it connects, so no guaranteed
/// on-hit effect should be synthesized against it.
///
/// The volatile is put on the belief by the inference engine's `ChargingMove`
/// handler and taken off by the matching release `MoveUsed`, so this stays correct
/// across the turn boundary — the takeoff and the landing are different turns, and a
/// per-turn scratch flag could only ever have covered one of them.
fn is_semi_invulnerable(belief: &UnknownBattleState, slot: FieldSlot) -> bool {
    mon_at(belief, slot).is_some_and(|mon| {
        mon.volatiles.iter().any(|v| {
            matches!(
                v,
                VolatileStatusState::MoveStatus(VolatileStatus::SemiInvulnerable(_), _)
            )
        })
    })
}

/// True if `target` already has a recorded `Missed`/`Immune`/`Blocked` among
/// `reactions` — the move didn't actually connect there, so no guaranteed
/// on-hit effect should be synthesized for it.
fn target_has_failed(reactions: &[InformationEvent], target: FieldSlot) -> bool {
    reactions.iter().any(|r| match &r.kind {
        EventKind::Missed { target: t }
        | EventKind::Immune { target: t }
        | EventKind::Blocked { target: t } => *t == target,
        _ => false,
    })
}

/// Weather/terrain/pseudo-weather/side-condition — global or per-side field
/// state, applied once per move use rather than per target.
fn synthesize_field_effects(
    reactions: &mut Vec<InformationEvent>,
    effect: &poke_rust::state::dex_data::HitEffect,
    user: FieldSlot,
    move_target: &MoveTarget,
    belief: &UnknownBattleState,
) {
    if let Some(weather) = &effect.weather
        && belief.weather.as_ref() != Some(weather)
        && !reactions.iter().any(
            |r| matches!(&r.kind, EventKind::WeatherChanged { weather: Some(w) } if w == weather),
        )
    {
        reactions.push(leaf(EventKind::WeatherChanged {
            weather: Some(weather.clone()),
        }));
    }
    if let Some(terrain) = &effect.terrain
        && belief.terrain.as_ref() != Some(terrain)
        && !reactions.iter().any(
            |r| matches!(&r.kind, EventKind::TerrainChanged { terrain: Some(t) } if t == terrain),
        )
    {
        reactions.push(leaf(EventKind::TerrainChanged {
            terrain: Some(terrain.clone()),
        }));
    }
    if let Some(pw) = &effect.pseudo_weather
        && !belief.pseudo_weathers.contains(pw)
        && !reactions
            .iter()
            .any(|r| matches!(&r.kind, EventKind::PseudoWeatherStart { effect: e } if e == pw))
    {
        reactions.push(leaf(EventKind::PseudoWeatherStart { effect: pw.clone() }));
    }
    if let Some(sc) = &effect.side_condition {
        // `FoeSide` lands on the user's opponent; everything else that can
        // carry a side condition (`AllySide`/`AllyTeam`) lands on the user's
        // own side.
        let side = match move_target {
            MoveTarget::FoeSide => user.player.opponent(),
            _ => user.player,
        };
        let current = match side {
            Player::P1 => &belief.p1_side_conditions,
            Player::P2 => &belief.p2_side_conditions,
        };
        let already_active = current
            .iter()
            .any(|condition| std::mem::discriminant(condition) == std::mem::discriminant(sc));
        if !already_active && !reactions.iter().any(|r| {
            matches!(&r.kind, EventKind::SideConditionStart { side: s, condition } if *s == side && condition == sc)
        }) {
            reactions.push(leaf(EventKind::SideConditionStart {
                side,
                condition: sc.clone(),
            }));
        }
    }
}

/// Status/volatile/boosts — per-Pokemon payloads, applied to one specific
/// `target` (a connected foe for `secondaries`, the user for
/// `self_secondaries`).
fn synthesize_per_mon_effects(
    reactions: &mut Vec<InformationEvent>,
    effect: &poke_rust::state::dex_data::HitEffect,
    source: FieldSlot,
    target: FieldSlot,
    belief: &UnknownBattleState,
) {
    if let Some(status) = &effect.status {
        reactions.push(leaf(EventKind::StatusInflicted {
            target,
            status: status.clone(),
        }));
    }
    if let Some(volatile) = &effect.volatile_status {
        // The move dex stores a placeholder move inside Encore/Disable. Their
        // actual payload is the target's last move, and both moves fail when
        // there is no eligible last move (notably immediately after switch-in).
        let resolved = match volatile {
            // These are not guaranteed merely from the move name: they fail
            // for several target-state reasons (PP, existing lock, immunity,
            // Aroma Veil/Mental Herb). A successful payload is entered
            // explicitly as `encoremove <move>` / `disablemove <move>`.
            VolatileStatus::Encore(_)
            | VolatileStatus::Disable(_)
            | VolatileStatus::Stockpile(_) => None,
            VolatileStatus::LeechSeed => {
                let definitely_seedable = mon_at(belief, target).is_some_and(|mon| {
                    matches!(
                        &mon.possible_types,
                        Unknown::Known(types) if !types.contains(&PokemonType::Grass)
                    )
                });
                definitely_seedable.then_some(VolatileStatus::LeechSeed)
            }
            other => {
                let already_active = mon_at(belief, target).is_some_and(|mon| {
                    mon.volatiles.iter().any(|state| match state {
                        VolatileStatusState::TurnStatus(active, _)
                        | VolatileStatusState::MoveStatus(active, _) => {
                            std::mem::discriminant(active) == std::mem::discriminant(other)
                        }
                        VolatileStatusState::Charging(_, _) => false,
                    })
                });
                (!already_active).then(|| other.clone())
            }
        };
        if let Some(volatile) = resolved {
            reactions.push(leaf(EventKind::VolatileStart { target, volatile }));
        }
    }
    reactions.extend(synthesize_boost_changes(
        &effect.boosts,
        source,
        target,
        belief,
    ));
}

/// Mirror the engine's opponent-stat-drop cascade, including one
/// Defiant/Competitive activation for every distinct stat lowered.
fn synthesize_boost_changes(
    raw_boosts: &[i8; 7],
    source: FieldSlot,
    target: FieldSlot,
    belief: &UnknownBattleState,
) -> Vec<InformationEvent> {
    let ability = mon_at(belief, target).and_then(|mon| match &mon.possible_abilities {
        Unknown::Known(ability) => Some(ability),
        _ => None,
    });
    let mirror_armor = matches!(ability, Some(Ability::MirrorArmor));
    let mut actual = [0i8; 7];
    for (idx, &raw) in raw_boosts.iter().enumerate() {
        if mirror_armor && raw < 0 {
            continue;
        }
        actual[idx] = adjusted_boost_delta(belief, target, idx, raw);
    }

    let lowered: Vec<usize> = actual
        .iter()
        .enumerate()
        .filter_map(|(idx, delta)| (*delta < 0).then_some(idx))
        .collect();
    let mut events: Vec<_> = actual
        .iter()
        .enumerate()
        .filter_map(|(boost_idx, &stages)| {
            (stages != 0).then(|| {
                leaf(EventKind::BoostChanged {
                    target,
                    boost_idx,
                    stages,
                })
            })
        })
        .collect();

    if source.player != target.player
        && !lowered.is_empty()
        && let Some(reactive) = ability
        && matches!(reactive, Ability::Defiant | Ability::Competitive)
    {
        let reaction_idx = if *reactive == Ability::Defiant { 0 } else { 2 };
        let current = mon_at(belief, target)
            .and_then(|mon| mon.boosts.get(reaction_idx))
            .copied()
            .unwrap_or(0);
        let post_drop = (current + actual[reaction_idx]).clamp(-6, 6);
        let requested = 2i8.saturating_mul(lowered.len() as i8);
        let stages = (post_drop + requested).clamp(-6, 6) - post_drop;
        if stages != 0 {
            let trigger_idx = *lowered.last().expect("lowered is non-empty");
            if let Some(trigger) = events.iter_mut().find(|event| {
                matches!(
                    event.kind,
                    EventKind::BoostChanged { boost_idx, stages, .. }
                        if boost_idx == trigger_idx && stages < 0
                )
            }) {
                trigger.reactions.push(InformationEvent {
                    kind: EventKind::AbilityRevealed {
                        slot: target,
                        ability: reactive.clone(),
                    },
                    reactions: vec![leaf(EventKind::BoostChanged {
                        target,
                        boost_idx: reaction_idx,
                        stages,
                    })],
                });
            }
        }
    }
    events
}

fn adjusted_boost_delta(
    belief: &UnknownBattleState,
    target: FieldSlot,
    boost_idx: usize,
    stages: i8,
) -> i8 {
    let Some(mon) = mon_at(belief, target) else {
        return stages;
    };
    let mut adjusted = match &mon.possible_abilities {
        Unknown::Known(Ability::Contrary) => -stages,
        Unknown::Known(Ability::Simple) => stages.saturating_mul(2),
        Unknown::Known(_) => stages,
        Unknown::Possibly(abilities) => {
            let mut outcomes = abilities.iter().map(|ability| match ability {
                Ability::Contrary => -stages,
                Ability::Simple => stages.saturating_mul(2),
                _ => stages,
            });
            let Some(first) = outcomes.next() else {
                return 0;
            };
            if outcomes.any(|outcome| outcome != first) {
                return 0;
            }
            first
        }
        Unknown::Not(excluded) => {
            if !excluded.contains(&Ability::Contrary) || !excluded.contains(&Ability::Simple) {
                return 0;
            }
            stages
        }
    };
    if let Some(current) = mon.boosts.get(boost_idx) {
        adjusted = (*current + adjusted).clamp(-6, 6) - *current;
    }
    adjusted
}

/// Any `DamageDealt`/`Healed`/`SetHp` child landing exactly on 0 HP gets a
/// `Faint` sibling — see this module's doc comment for why this can't be left
/// to Pass 1's belt-and-braces alone.
fn synthesize_guaranteed_faints(reactions: &mut Vec<InformationEvent>) {
    let mut faint_slots: Vec<FieldSlot> = Vec::new();
    for r in reactions.iter() {
        let zeroed = match &r.kind {
            EventKind::DamageDealt { target, new_hp, .. }
            | EventKind::Healed { target, new_hp, .. }
            | EventKind::SetHp { target, new_hp, .. } => is_zero_hp(new_hp).then_some(*target),
            _ => None,
        };
        if let Some(target) = zeroed
            && !faint_slots.contains(&target)
            && !reactions
                .iter()
                .any(|r2| matches!(&r2.kind, EventKind::Faint { slot } if *slot == target))
        {
            faint_slots.push(target);
        }
    }
    for slot in faint_slots {
        reactions.push(leaf(EventKind::Faint { slot }));
    }
}

fn is_zero_hp(hp: &PokemonHP) -> bool {
    matches!(hp, PokemonHP::Number(0) | PokemonHP::Percent(0))
}

fn mon_at(belief: &UnknownBattleState, slot: FieldSlot) -> Option<&UnknownPokemonState> {
    let mons = match slot.player {
        Player::P1 => &belief.p1_active_mons,
        Player::P2 => &belief.p2_active_mons,
    };
    mons.get(slot.slot_index as usize)
}

/// Build an `AbilityRevealed` node with its own cascade already populated —
/// use this (not a bare `leaf(...)`) whenever this module synthesizes a NEW
/// ability reveal (Mega Evolution, Trace) rather than translating one the
/// user typed: the outer recursive walk only processes a node's *existing*
/// children before handling its own kind, so a freshly-appended
/// `AbilityRevealed` would never otherwise get its cascade applied.
fn ability_revealed_node(
    slot: FieldSlot,
    ability: Ability,
    belief: &UnknownBattleState,
    live_slots: &HashSet<FieldSlot>,
) -> InformationEvent {
    let reactions = guaranteed_ability_reactions(slot, &ability, belief, live_slots);
    InformationEvent {
        kind: EventKind::AbilityRevealed { slot, ability },
        reactions,
    }
}

fn guaranteed_ability_reactions(
    slot: FieldSlot,
    ability: &Ability,
    belief: &UnknownBattleState,
    live_slots: &HashSet<FieldSlot>,
) -> Vec<InformationEvent> {
    match ability {
        Ability::Intimidate => live_slots
            .iter()
            .copied()
            .filter(|target| target.player != slot.player)
            .flat_map(|target| {
                synthesize_boost_changes(&[-1, 0, 0, 0, 0, 0, 0], slot, target, belief)
            })
            .collect(),
        Ability::Drizzle
            if !weather_is_strong(&belief.weather)
                && !matches!(belief.weather, Some(Weather::Rain)) =>
        {
            vec![weather_event(Weather::Rain)]
        }
        Ability::Drought
            if !weather_is_strong(&belief.weather)
                && !matches!(belief.weather, Some(Weather::Sun)) =>
        {
            vec![weather_event(Weather::Sun)]
        }
        Ability::SandStream
            if !weather_is_strong(&belief.weather)
                && !matches!(belief.weather, Some(Weather::Sandstorm)) =>
        {
            vec![weather_event(Weather::Sandstorm)]
        }
        Ability::SnowWarning
            if !weather_is_strong(&belief.weather)
                && !matches!(belief.weather, Some(Weather::Snow)) =>
        {
            vec![weather_event(Weather::Snow)]
        }
        // Terrain has no "strong terrain" exception — a terrain-setting
        // ability always replaces whatever terrain is active, unconditionally
        // (mirrors `simulator::helpers::set_terrain`).
        Ability::ElectricSurge => vec![terrain_event(Terrain::ElectricTerrain)],
        Ability::GrassySurge => vec![terrain_event(Terrain::GrassyTerrain)],
        Ability::MistySurge => vec![terrain_event(Terrain::MistyTerrain)],
        Ability::PsychicSurge => vec![terrain_event(Terrain::PsychicTerrain)],
        Ability::IntrepidSword
            if !mon_at(belief, slot).is_some_and(|m| m.one_time_ability_used) =>
        {
            vec![leaf(EventKind::BoostChanged {
                target: slot,
                boost_idx: 0,
                stages: 1,
            })]
        }
        Ability::DauntlessShield
            if !mon_at(belief, slot).is_some_and(|m| m.one_time_ability_used) =>
        {
            vec![leaf(EventKind::BoostChanged {
                target: slot,
                boost_idx: 1,
                stages: 1,
            })]
        }
        Ability::Download => download_reaction(slot, belief),
        Ability::Trace => trace_reaction(slot, belief, live_slots),
        _ => Vec::new(),
    }
}

/// Download compares the SUM of opposing actives' Def vs SpD (mirroring the
/// real ability's `for target of pokemon.foes()` loop) and boosts SpA when
/// Def is the (weaker-to-hit) higher stat, Atk when SpD is — but only when
/// the belief's current bounds make the comparison unambiguous regardless of
/// where the true values land; otherwise this is left for the user to type.
fn download_reaction(slot: FieldSlot, belief: &UnknownBattleState) -> Vec<InformationEvent> {
    let opponents = opposing_active_slots(belief, slot);
    let (mut min_def, mut max_def, mut min_spd, mut max_spd) = (0u32, 0u32, 0u32, 0u32);
    for opp in &opponents {
        let Some(mon) = mon_at(belief, *opp) else {
            return Vec::new();
        };
        min_def += mon.min_stats[2] as u32;
        max_def += mon.max_stats[2] as u32;
        min_spd += mon.min_stats[4] as u32;
        max_spd += mon.max_stats[4] as u32;
    }
    if min_def >= max_spd {
        vec![leaf(EventKind::BoostChanged {
            target: slot,
            boost_idx: 2, // SpA
            stages: 1,
        })]
    } else if max_def < min_spd {
        vec![leaf(EventKind::BoostChanged {
            target: slot,
            boost_idx: 0, // Atk
            stages: 1,
        })]
    } else {
        Vec::new()
    }
}

/// Trace copies an opposing active's ability — only synthesizable when
/// exactly one opposing active's ability is already `Known` (otherwise which
/// one the real game copied is new information the user has to type).
fn trace_reaction(
    slot: FieldSlot,
    belief: &UnknownBattleState,
    live_slots: &HashSet<FieldSlot>,
) -> Vec<InformationEvent> {
    let known: Vec<Ability> = opposing_active_slots(belief, slot)
        .into_iter()
        .filter_map(|opp| mon_at(belief, opp))
        .filter_map(|mon| match &mon.possible_abilities {
            Unknown::Known(a) => Some(a.clone()),
            _ => None,
        })
        .collect();
    match known.as_slice() {
        [ability] => vec![ability_revealed_node(
            slot,
            ability.clone(),
            belief,
            live_slots,
        )],
        _ => Vec::new(),
    }
}

/// The three strong/primal weathers (Desolate Land/Primordial Sea/Delta
/// Stream's Extreme Sunlight/Heavy Rain/Strong Winds) block every standard
/// weather-setting ability from activating at all — mirrors
/// `simulator::helpers::set_weather`'s `current_is_strong` check exactly.
fn weather_is_strong(weather: &Option<Weather>) -> bool {
    matches!(
        weather,
        Some(Weather::ExtremeSunlight) | Some(Weather::HeavyRain) | Some(Weather::StrongWinds)
    )
}

fn weather_event(weather: Weather) -> InformationEvent {
    leaf(EventKind::WeatherChanged {
        weather: Some(weather),
    })
}

fn terrain_event(terrain: Terrain) -> InformationEvent {
    leaf(EventKind::TerrainChanged {
        terrain: Some(terrain),
    })
}

fn leaf(kind: EventKind) -> InformationEvent {
    InformationEvent {
        kind,
        reactions: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tracker_parse::{TrackerLine, parse_tracker_text};
    use poke_rust::information::inference::{InferenceConfig, apply_information};
    use poke_rust::information::unknowns::{PokemonHP, UnknownMatchState, UnknownPokemonState};
    use poke_rust::state::battle::Player;
    use poke_rust::state::dex_data::{
        AbilityData, parse_ability_dex, parse_move_dex, parse_pokemon_dex,
    };
    use std::sync::OnceLock;

    static POKEMON_DEX: OnceLock<HashMap<Species, PokemonData>> = OnceLock::new();
    static MOVE_DEX: OnceLock<HashMap<PokemonMove, MoveData>> = OnceLock::new();
    static ABILITY_DEX: OnceLock<HashMap<Ability, AbilityData>> = OnceLock::new();

    fn pokemon_dex() -> &'static HashMap<Species, PokemonData> {
        POKEMON_DEX.get_or_init(|| parse_pokemon_dex("../pokemon_info/showdownDex.txt"))
    }
    fn move_dex() -> &'static HashMap<PokemonMove, MoveData> {
        MOVE_DEX.get_or_init(|| parse_move_dex("../pokemon_info/showdownMoves.txt"))
    }
    fn ability_dex() -> &'static HashMap<Ability, AbilityData> {
        ABILITY_DEX.get_or_init(|| parse_ability_dex("../pokemon_info/showdownAbilities.txt"))
    }

    fn make_active(species: Species, hp: PokemonHP) -> UnknownPokemonState {
        let mut mon = UnknownPokemonState::from_opponent_species(species, pokemon_dex(), 50);
        mon.hp = hp;
        mon
    }

    fn test_belief() -> UnknownBattleState {
        UnknownBattleState {
            active_per_side: 1,
            back_mons_per_side: 0,
            p1_active_mons: vec![make_active(Species::Pikachu, PokemonHP::Number(100))],
            p2_active_mons: vec![make_active(Species::Garchomp, PokemonHP::Percent(100))],
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
        FieldSlot {
            player: Player::P1,
            slot_index: 0,
        }
    }
    fn o1() -> FieldSlot {
        FieldSlot {
            player: Player::P2,
            slot_index: 0,
        }
    }

    /// Parse one line, augment it, and return the resulting event.
    fn parse_and_augment(text: &str, belief: &UnknownBattleState) -> InformationEvent {
        let lines = parse_tracker_text(text, belief, move_dex(), pokemon_dex()).unwrap();
        let TrackerLine::Event(ev) = lines.into_iter().next().unwrap() else {
            panic!("expected an event line")
        };
        augment_with_guaranteed_effects(ev, belief, move_dex(), pokemon_dex())
    }

    #[test]
    fn taunt_synthesizes_its_own_volatile_on_the_explicit_target() {
        // Only reachable once `tracker_parse.rs`'s cant-reason/move-name
        // collision fix lands ("p1 taunt o1" used to get misparsed as
        // `Cant{Taunt}`, dropping the target and the move entirely).
        let belief = test_belief();
        let augmented = parse_and_augment("p1 taunt o1", &belief);
        assert!(augmented.reactions.iter().any(|r| matches!(
            &r.kind,
            EventKind::VolatileStart { target, volatile: poke_rust::state::dex_data::VolatileStatus::Taunt } if *target == o1()
        )));
    }

    #[test]
    fn protect_synthesizes_its_own_volatile() {
        // Regression: Protect's `volatileStatus: 'protect'` is a bare
        // top-level field on a `target: "self"` move — the dex parser puts
        // it in `secondaries` regardless of the move's own target, but the
        // tracker's `targets` list is always empty for a self-target move
        // (the grammar never lets a target token name the user). Before the
        // `MoveTarget::SelfTarget` special-case, this silently dropped
        // Protect's own volatile entirely.
        let belief = test_belief();
        let augmented = parse_and_augment("p1 protect", &belief);
        assert!(augmented.reactions.iter().any(|r| matches!(
            &r.kind,
            EventKind::VolatileStart { target, volatile: poke_rust::state::dex_data::VolatileStatus::Protect } if *target == p1()
        )));
    }

    #[test]
    fn focus_energy_synthesizes_its_own_volatile() {
        // Same bug class as Protect, different move — confirms the fix is
        // general to self-targeting status moves, not special-cased to Protect.
        let belief = test_belief();
        let augmented = parse_and_augment("p1 focusenergy", &belief);
        assert!(augmented.reactions.iter().any(|r| matches!(
            &r.kind,
            EventKind::VolatileStart { target, volatile: poke_rust::state::dex_data::VolatileStatus::FocusEnergy } if *target == p1()
        )));
    }

    #[test]
    fn charge_and_power_trick_synthesize_their_own_volatiles() {
        // Two more self-targeting, bare-`volatileStatus:` moves — part of the
        // volatile-coverage audit alongside Protect/Focus Energy, confirming
        // the `MoveTarget::SelfTarget` fix generalizes across the whole class.
        let belief = test_belief();
        let charge = parse_and_augment("p1 charge", &belief);
        assert!(charge.reactions.iter().any(|r| matches!(
            &r.kind,
            EventKind::VolatileStart { target, volatile: poke_rust::state::dex_data::VolatileStatus::Charge } if *target == p1()
        )));
        let power_trick = parse_and_augment("p1 powertrick", &belief);
        assert!(power_trick.reactions.iter().any(|r| matches!(
            &r.kind,
            EventKind::VolatileStart { target, volatile: poke_rust::state::dex_data::VolatileStatus::PowerTrick } if *target == p1()
        )));
    }

    #[test]
    fn thunder_wave_synthesizes_guaranteed_paralysis() {
        let belief = test_belief();
        let augmented = parse_and_augment("p1 thunderwave o1", &belief);
        assert!(augmented.reactions.iter().any(|r| matches!(
            &r.kind,
            EventKind::StatusInflicted { target, status: poke_rust::state::dex_data::Status::Paralysis } if *target == o1()
        )));
    }

    #[test]
    fn leer_synthesizes_foe_defense_drop() {
        let belief = test_belief();
        let augmented = parse_and_augment("p1 leer o1", &belief);
        assert!(augmented.reactions.iter().any(|r| matches!(
            &r.kind,
            EventKind::BoostChanged { target, boost_idx: 1, stages: -1 } if *target == o1()
        )));
    }

    #[test]
    fn known_defiant_reacts_to_a_guaranteed_opponent_drop() {
        let mut belief = test_belief();
        belief.p2_active_mons[0].possible_abilities = Unknown::Known(Ability::Defiant);
        let augmented = parse_and_augment("p1 leer o1", &belief);
        let drop = augmented
            .reactions
            .iter()
            .find(|reaction| {
                matches!(
                    reaction.kind,
                    EventKind::BoostChanged {
                        target,
                        boost_idx: 1,
                        stages: -1
                    } if target == o1()
                )
            })
            .expect("Leer should lower Defense");
        let reveal = drop
            .reactions
            .iter()
            .find(|reaction| {
                matches!(
                    reaction.kind,
                    EventKind::AbilityRevealed {
                        slot,
                        ability: Ability::Defiant
                    } if slot == o1()
                )
            })
            .expect("Defiant should be nested beneath the triggering drop");
        assert!(reveal.reactions.iter().any(|reaction| matches!(
            reaction.kind,
            EventKind::BoostChanged {
                target,
                boost_idx: 0,
                stages: 2
            } if target == o1()
        )));
    }

    #[test]
    fn competitive_activates_once_for_each_distinct_stat_lowered() {
        let mut belief = test_belief();
        belief.p2_active_mons[0].possible_abilities = Unknown::Known(Ability::Competitive);
        let events =
            synthesize_boost_changes(&[-1, 0, -1, 0, 0, 0, 0], p1(), o1(), &belief);
        let reveal = events
            .iter()
            .flat_map(|event| &event.reactions)
            .find(|reaction| {
                matches!(
                    reaction.kind,
                    EventKind::AbilityRevealed {
                        ability: Ability::Competitive,
                        ..
                    }
                )
            })
            .expect("Competitive should react to the two distinct drops");
        assert!(reveal.reactions.iter().any(|reaction| matches!(
            reaction.kind,
            EventKind::BoostChanged {
                target,
                boost_idx: 2,
                stages: 4
            } if target == o1()
        )));
    }

    #[test]
    fn one_turn_and_charge_turn_meteor_beam_boost_but_release_does_not() {
        let belief = test_belief();
        for line in ["p1 meteorbeam o1", "p1 meteorbeam charging @o1"] {
            let augmented = parse_and_augment(line, &belief);
            assert!(augmented.reactions.iter().any(|reaction| matches!(
                reaction.kind,
                EventKind::BoostChanged {
                    target,
                    boost_idx: 2,
                    stages: 1
                } if target == p1()
            )));
        }

        let mut releasing = belief;
        releasing.p1_active_mons[0]
            .volatiles
            .push(VolatileStatusState::Charging(
                PokemonMove::MeteorBeam,
                vec![o1()],
            ));
        let augmented = parse_and_augment("p1 meteorbeam o1", &releasing);
        assert!(!augmented.reactions.iter().any(|reaction| matches!(
            reaction.kind,
            EventKind::BoostChanged {
                target,
                boost_idx: 2,
                stages: 1
            } if target == p1()
        )));
    }

    #[test]
    fn inference_tracks_pure_charge_until_the_release_move() {
        let mut belief = test_belief();
        belief.p1_active_mons[0] = make_active(Species::Lunala, PokemonHP::Number(100));
        let charge = parse_and_augment("p1 meteorbeam charging @o1", &belief);
        let config = InferenceConfig::default();
        let UnknownMatchState::Battle(next) = apply_information(
            UnknownMatchState::Battle(belief),
            &[charge],
            false,
            pokemon_dex(),
            move_dex(),
            ability_dex(),
            &config,
        ) else {
            panic!("expected Battle variant")
        };
        belief = next;
        assert!(belief.p1_active_mons[0].volatiles.iter().any(
            |volatile| matches!(volatile, VolatileStatusState::Charging(PokemonMove::MeteorBeam, targets) if targets == &vec![o1()])
        ), "{:?}", belief.p1_active_mons[0].volatiles);

        let release = parse_and_augment("p1 meteorbeam o1", &belief);
        let UnknownMatchState::Battle(next) = apply_information(
            UnknownMatchState::Battle(belief),
            &[release],
            false,
            pokemon_dex(),
            move_dex(),
            ability_dex(),
            &config,
        ) else {
            panic!("expected Battle variant")
        };
        assert!(
            !next.p1_active_mons[0]
                .volatiles
                .iter()
                .any(|volatile| matches!(
                    volatile,
                    VolatileStatusState::Charging(PokemonMove::MeteorBeam, _)
                ))
        );
    }

    #[test]
    fn rain_dance_synthesizes_global_weather_with_no_target_token() {
        let belief = test_belief();
        let augmented = parse_and_augment("p1 raindance", &belief);
        assert!(augmented.reactions.iter().any(|r| matches!(
            &r.kind,
            EventKind::WeatherChanged {
                weather: Some(Weather::Rain)
            }
        )));
    }

    #[test]
    fn stealth_rock_synthesizes_side_condition_on_foe_side_with_no_target_token() {
        let belief = test_belief();
        let augmented = parse_and_augment("p1 stealthrock", &belief);
        assert!(augmented.reactions.iter().any(|r| matches!(
            &r.kind,
            EventKind::SideConditionStart {
                side: Player::P2,
                condition: poke_rust::state::dex_data::SideCondition::StealthRock
            }
        )));
    }

    #[test]
    fn missed_target_does_not_get_guaranteed_status() {
        let belief = test_belief();
        let augmented = parse_and_augment("p1 thunderwave o1 miss", &belief);
        assert!(
            !augmented
                .reactions
                .iter()
                .any(|r| matches!(&r.kind, EventKind::StatusInflicted { .. }))
        );
    }

    #[test]
    fn mega_evolution_reveals_fixed_ability_with_its_own_cascade() {
        // Charizard-Mega-Y's fixed ability is Drought, which itself
        // guarantees a Sun weather change — the cascade must appear nested
        // under the synthesized AbilityRevealed, not as a sibling of it.
        let belief = test_belief();
        let event = InformationEvent {
            kind: EventKind::MegaEvolution {
                slot: p1(),
                into: Species::CharizardMegaY,
            },
            reactions: Vec::new(),
        };
        let augmented = augment_with_guaranteed_effects(event, &belief, move_dex(), pokemon_dex());
        let ability_node = augmented
            .reactions
            .iter()
            .find(|r| {
                matches!(
                    &r.kind,
                    EventKind::AbilityRevealed {
                        ability: Ability::Drought,
                        ..
                    }
                )
            })
            .expect("expected a synthesized Drought reveal");
        assert!(ability_node.reactions.iter().any(|r| matches!(
            &r.kind,
            EventKind::WeatherChanged {
                weather: Some(Weather::Sun)
            }
        )));
    }

    #[test]
    fn weather_setter_overrides_existing_standard_weather() {
        // Regression: a second weather-setting ability used to be gated on
        // `belief.weather.is_none()`, so it silently no-op'd whenever ANY
        // weather (not just a strong one) was already active — the bug
        // report's "Sand Stream after Drought: no weather changes happening
        // here." Standard weather always replaces standard weather.
        let mut belief = test_belief();
        belief.weather = Some(Weather::Sun);
        let augmented = parse_and_augment("o1 sandstream", &belief);
        assert!(augmented.reactions.iter().any(|r| matches!(
            &r.kind,
            EventKind::WeatherChanged {
                weather: Some(Weather::Sandstorm)
            }
        )));
    }

    #[test]
    fn strong_weather_blocks_standard_weather_setter() {
        let mut belief = test_belief();
        belief.weather = Some(Weather::HeavyRain);
        let augmented = parse_and_augment("p1 drought", &belief);
        assert!(
            !augmented
                .reactions
                .iter()
                .any(|r| matches!(&r.kind, EventKind::WeatherChanged { .. }))
        );
    }

    #[test]
    fn two_mega_evolutions_in_one_turn_second_weather_setter_overrides_first() {
        // The bug report's exact scenario: Charizard-Mega-Y (Drought) and
        // Tyranitar-Mega (Sand Stream) both mega evolving the SAME turn,
        // starting from no weather at all. `augment_with_guaranteed_effects`
        // alone can't get this right — both events would be augmented against
        // the same pre-turn `belief` (weather: None) — so this must go
        // through `augment_turn`, which threads the first event's synthesized
        // Sun into a scratch snapshot before augmenting the second.
        let belief = test_belief();
        let charizard = InformationEvent {
            kind: EventKind::MegaEvolution {
                slot: o1(),
                into: Species::CharizardMegaY,
            },
            reactions: Vec::new(),
        };
        let tyranitar = InformationEvent {
            kind: EventKind::MegaEvolution {
                slot: p1(),
                into: Species::TyranitarMega,
            },
            reactions: Vec::new(),
        };
        let augmented = augment_turn(
            vec![charizard, tyranitar],
            &belief,
            move_dex(),
            pokemon_dex(),
        );
        let [charizard_aug, tyranitar_aug] = augmented.as_slice() else {
            panic!("expected exactly two augmented events")
        };
        let drought = charizard_aug
            .reactions
            .iter()
            .find(|r| {
                matches!(
                    &r.kind,
                    EventKind::AbilityRevealed {
                        ability: Ability::Drought,
                        ..
                    }
                )
            })
            .expect("expected a synthesized Drought reveal");
        assert!(drought.reactions.iter().any(|r| matches!(
            &r.kind,
            EventKind::WeatherChanged {
                weather: Some(Weather::Sun)
            }
        )));
        let sand_stream = tyranitar_aug
            .reactions
            .iter()
            .find(|r| {
                matches!(
                    &r.kind,
                    EventKind::AbilityRevealed {
                        ability: Ability::SandStream,
                        ..
                    }
                )
            })
            .expect("expected a synthesized Sand Stream reveal");
        assert!(sand_stream.reactions.iter().any(|r| matches!(
            &r.kind,
            EventKind::WeatherChanged {
                weather: Some(Weather::Sandstorm)
            }
        )));
    }

    #[test]
    fn augment_turn_blocks_setter_after_same_turn_strong_weather() {
        // A strong weather set earlier in the SAME turn must still block a
        // later standard setter, even though it isn't in the committed
        // pre-turn `belief` yet — proves `fold_field_changes_into_scratch`
        // actually threads strength, not just presence.
        let belief = test_belief();
        let heavy_rain = leaf(EventKind::WeatherChanged {
            weather: Some(Weather::HeavyRain),
        });
        let drought_line =
            parse_tracker_text("p1 drought", &belief, move_dex(), pokemon_dex()).unwrap();
        let TrackerLine::Event(drought_event) = drought_line.into_iter().next().unwrap() else {
            panic!("expected an event line")
        };
        let augmented = augment_turn(
            vec![heavy_rain, drought_event],
            &belief,
            move_dex(),
            pokemon_dex(),
        );
        let drought_aug = &augmented[1];
        assert!(
            !drought_aug
                .reactions
                .iter()
                .any(|r| matches!(&r.kind, EventKind::WeatherChanged { .. }))
        );
    }

    #[test]
    fn terrain_setter_always_overrides_existing_terrain() {
        let mut belief = test_belief();
        belief.terrain = Some(Terrain::GrassyTerrain);
        let augmented = parse_and_augment("o1 electricsurge", &belief);
        assert!(augmented.reactions.iter().any(|r| matches!(
            &r.kind,
            EventKind::TerrainChanged {
                terrain: Some(Terrain::ElectricTerrain)
            }
        )));
    }

    #[test]
    fn augment_turn_synthesizes_weather_clear_at_guaranteed_expiry() {
        let mut belief = test_belief();
        belief.weather = Some(Weather::Sandstorm);
        belief.weather_turns = Some(Unknown::Known(1));
        let augmented = augment_turn(
            vec![leaf(EventKind::EndOfTurn)],
            &belief,
            move_dex(),
            pokemon_dex(),
        );
        let eot = augmented.last().expect("expected the EndOfTurn event");
        assert!(matches!(eot.kind, EventKind::EndOfTurn));
        assert!(
            eot.reactions
                .iter()
                .any(|r| matches!(&r.kind, EventKind::WeatherChanged { weather: None })),
            "expected a synthesized WeatherChanged{{None}} under EndOfTurn's reactions"
        );
    }

    #[test]
    fn augment_turn_skips_weather_clear_when_a_side_is_ambiguously_wiped() {
        // Fuzz-discovered (doubles wipeout while the opponent's bench was
        // still fog-of-war ambiguous): if this same turn's own events leave
        // every one of a side's active slots fainted with no CONFIRMED
        // healthy reserve, the real engine may never have run a genuine
        // end-of-turn pass at all (`step_action_queue` skips `end_turn` once
        // the battle is already decided) — synthesizing a clear here would
        // claim a duration tick that might never have happened.
        let mut belief = test_belief();
        belief.weather = Some(Weather::Sandstorm);
        belief.weather_turns = Some(Unknown::Known(1));
        // No known/possible back mon for P2 — an ambiguous wipe once o1 faints.
        let augmented = augment_turn(
            vec![
                leaf(EventKind::Faint { slot: o1() }),
                leaf(EventKind::EndOfTurn),
            ],
            &belief,
            move_dex(),
            pokemon_dex(),
        );
        let eot = augmented.last().expect("expected the EndOfTurn event");
        assert!(matches!(eot.kind, EventKind::EndOfTurn));
        assert!(
            !eot.reactions
                .iter()
                .any(|r| matches!(&r.kind, EventKind::WeatherChanged { .. })),
            "must not synthesize a clear while it's ambiguous whether the real \
             engine even ran a genuine end-of-turn this turn, got {:?}",
            eot.reactions
        );
    }

    #[test]
    fn augment_turn_still_synthesizes_weather_clear_with_a_confirmed_reserve() {
        // Contrast case: a side's only active slot fainting is the ORDINARY,
        // common scenario (a KO mid-battle) whenever a confirmed healthy
        // reserve exists — the real engine's `end_turn` genuinely still runs
        // (only a battle-ending wipe skips it), so expiry synthesis must
        // still fire normally here. Guards against over-restricting the
        // feature for the common case while fixing the ambiguous-wipe one.
        let mut belief = test_belief();
        belief.weather = Some(Weather::Sandstorm);
        belief.weather_turns = Some(Unknown::Known(1));
        belief
            .p2_known_back_mons
            .push(make_active(Species::Tyranitar, PokemonHP::Percent(100)));
        let augmented = augment_turn(
            vec![
                leaf(EventKind::Faint { slot: o1() }),
                leaf(EventKind::EndOfTurn),
            ],
            &belief,
            move_dex(),
            pokemon_dex(),
        );
        let eot = augmented.last().expect("expected the EndOfTurn event");
        assert!(
            eot.reactions
                .iter()
                .any(|r| matches!(&r.kind, EventKind::WeatherChanged { weather: None })),
            "a confirmed healthy reserve means the battle demonstrably continues, \
             so the clear must still synthesize normally, got {:?}",
            eot.reactions
        );
    }

    #[test]
    fn augment_turn_does_not_synthesize_weather_clear_while_still_ambiguous() {
        // A Possibly([5, 8]) timer only ever reaches Known(1) once the
        // extension-item branch has already been excluded — this must never
        // fire against the still-ambiguous candidate set.
        let mut belief = test_belief();
        belief.weather = Some(Weather::Sandstorm);
        belief.weather_turns = Some(Unknown::Possibly(vec![5, 8]));
        let augmented = augment_turn(
            vec![leaf(EventKind::EndOfTurn)],
            &belief,
            move_dex(),
            pokemon_dex(),
        );
        let eot = &augmented[0];
        assert!(
            !eot.reactions
                .iter()
                .any(|r| matches!(&r.kind, EventKind::WeatherChanged { .. }))
        );
    }

    #[test]
    fn augment_turn_skips_weather_clear_when_this_turns_own_event_already_changed_it() {
        // Weather was about to naturally expire (Known(1)) but the user also
        // typed a brand-new weather this same turn — the natural-expiry
        // synthesis must not fire a spurious clear on top of the override.
        let mut belief = test_belief();
        belief.weather = Some(Weather::Sandstorm);
        belief.weather_turns = Some(Unknown::Known(1));
        let rain = leaf(EventKind::WeatherChanged {
            weather: Some(Weather::Rain),
        });
        let augmented = augment_turn(
            vec![rain, leaf(EventKind::EndOfTurn)],
            &belief,
            move_dex(),
            pokemon_dex(),
        );
        let eot = augmented.last().unwrap();
        assert!(
            eot.reactions.is_empty(),
            "expiry clear must not fire once the weather was overridden this turn"
        );
    }

    #[test]
    fn augment_turn_synthesizes_side_condition_end_at_guaranteed_expiry() {
        let mut belief = test_belief();
        belief.p1_side_conditions = vec![poke_rust::state::dex_data::SideCondition::Reflect];
        belief.p1_side_condition_turns = vec![Unknown::Known(1)];
        let augmented = augment_turn(
            vec![leaf(EventKind::EndOfTurn)],
            &belief,
            move_dex(),
            pokemon_dex(),
        );
        let eot = &augmented[0];
        assert!(eot.reactions.iter().any(|r| matches!(
            &r.kind,
            EventKind::SideConditionEnd {
                side: Player::P1,
                condition: poke_rust::state::dex_data::SideCondition::Reflect
            }
        )));
    }

    #[test]
    fn augment_turn_synthesizes_pseudo_weather_end_at_guaranteed_expiry() {
        let mut belief = test_belief();
        belief.pseudo_weathers = vec![poke_rust::state::dex_data::PseudoWeather::TrickRoom];
        belief.pseudo_weather_turns = vec![Unknown::Known(1)];
        let augmented = augment_turn(
            vec![leaf(EventKind::EndOfTurn)],
            &belief,
            move_dex(),
            pokemon_dex(),
        );
        let eot = &augmented[0];
        assert!(eot.reactions.iter().any(|r| matches!(
            &r.kind,
            EventKind::PseudoWeatherEnd {
                effect: poke_rust::state::dex_data::PseudoWeather::TrickRoom
            }
        )));
    }

    #[test]
    fn zero_hp_synthesizes_faint_and_applies_through_inference() {
        let belief = test_belief();
        let lines = parse_tracker_text(
            "p1 thunderbolt o1 0%\nendofturn",
            &belief,
            move_dex(),
            pokemon_dex(),
        )
        .unwrap();

        let mut events = Vec::new();
        for line in lines {
            match line {
                TrackerLine::Event(ev) => events.push(augment_with_guaranteed_effects(
                    ev,
                    &belief,
                    move_dex(),
                    pokemon_dex(),
                )),
                TrackerLine::EndOfTurn => events.push(leaf(EventKind::EndOfTurn)),
            }
        }

        // The synthesized Faint sibling should already be present pre-inference.
        let move_event = events
            .iter()
            .find(|e| matches!(e.kind, EventKind::MoveUsed { .. }))
            .unwrap();
        assert!(
            move_event
                .reactions
                .iter()
                .any(|r| matches!(&r.kind, EventKind::Faint { slot } if *slot == o1()))
        );

        let config = InferenceConfig::default();
        let result = apply_information(
            UnknownMatchState::Battle(belief),
            &events,
            false,
            pokemon_dex(),
            move_dex(),
            ability_dex(),
            &config,
        );
        let UnknownMatchState::Battle(next) = result else {
            panic!("expected Battle variant")
        };
        assert!(next.p2_active_mons[0].fainted);
    }
}
