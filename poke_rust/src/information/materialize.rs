//! # materialize — `UnknownState` → concrete state bridge for Pass 3
//!
//! The damage oracle (`calculate_damage_outcomes_for_target_with_options` in
//! `simulator::helpers`) takes concrete `&BattleState` / `&PokemonState` values.
//! The inference engine only has `UnknownBattleState` / `UnknownPokemonState`.
//! This module bridges that gap: given an unknown state and explicit choices for
//! the hidden fields (stats, item, ability), it produces the concrete types the
//! oracle can consume.
//!
//! **Soundness contract**: callers are responsible for enumerating the right
//! set of (stats, item, ability) combinations; this module just does the
//! mechanical field mapping.

use crate::data::ability::Ability;
use crate::data::item::Item;
use crate::data::pokemon_move::PokemonMove;
use crate::data::species::Species;
use crate::information::unknowns::{PokemonHP, Unknown, UnknownBattleState, UnknownPokemonState};
use crate::state::battle::BattleState;
use crate::state::dex_data::{PokemonType, SlotCondition};
use crate::state::pokemon::{Nature, PokemonGender, PokemonState, PokemonStatsTable};

/// Materialize a `PokemonState` from an `UnknownPokemonState` plus concrete
/// choices for the fields the oracle reads that are still uncertain.
///
/// `stats_override` replaces the entire stats array; call the formula
/// `floor(BSV * nature_mod)` per stat on the caller's side.
/// `item` and `ability` must be drawn from the mon's not-excluded sets.
pub fn materialize_pokemon(
    unk: &UnknownPokemonState,
    stats_override: PokemonStatsTable,
    item: Item,
    ability: Ability,
) -> PokemonState {
    // Species: use the known species; fall back to a placeholder when ambiguous
    // (Pass 3 skips mons with non-Known species, so this path is defensive).
    let species = match &unk.possible_species {
        Unknown::Known(s) => s.clone(),
        _ => Species::Garchomp, // fallback; never reached in practice
    };

    // Types: use the known types (always Known after a switch-in reveals them).
    let types = match &unk.possible_types {
        Unknown::Known(t) => t.clone(),
        _ => vec![PokemonType::Normal],
    };

    // Tera type: use known if available; otherwise default to species type.
    let tera_type = match &unk.possible_tera_type {
        Unknown::Known(t) => t.clone(),
        _ => types.first().cloned().unwrap_or(PokemonType::Normal),
    };

    // Mega species / ability: Known once mega evolution has been observed.
    let mega_species = match &unk.mega_species {
        Unknown::Known(s) => s.clone(),
        _ => None,
    };
    let mega_ability = match &unk.mega_ability {
        Unknown::Known(a) => a.clone(),
        _ => None,
    };

    // Current HP: for Multiscale / ShadowShield / TeraShell / Flail BP, use max HP
    // (stats[0]) when the Pokémon is known to be at 100 %.  For any other percent, we
    // substitute `max_hp × 0.5`.
    //
    // **Invariant**: the ×0.5 sentinel is the only full-HP-gated reducers are
    // Multiscale, ShadowShield, and TeraShell (all check `hp == stats[0]`).  A value
    // of `max_hp × 0.5` is strictly less than `max_hp`, so all three gates evaluate to
    // "not full HP" and the reducers deactivate.  This is correct for any
    // `PokemonHP::Percent(p)` with `p ≠ 100`; the exact fraction does not matter — it
    // only needs to be `< max_hp`.
    //
    // **No double-count with the defensive-ability union**: `defensive_damage_abilities`
    // in inference.rs includes Multiscale / ShadowShield / TeraShell and iterates them
    // unconditionally (they are HP-gated, never pruned by E-B).  For a full-HP defender
    // (`Percent(100)`) those abilities are active in the oracle and therefore considered
    // in the union.  For partial-HP defenders (`Percent(p ≠ 100)`) the ×0.5 sentinel
    // ensures they evaluate to *inactive* in the oracle, so no incorrect reduction is
    // applied and the union correctly contributes nothing for those entries.
    let hp = match &unk.hp {
        PokemonHP::Number(n) => *n,
        PokemonHP::Percent(100) => stats_override[0],
        // Any non-100 % → partial HP sentinel: strictly < max_hp, disabling all
        // full-HP-gated reducers (Multiscale, ShadowShield, TeraShell).
        PokemonHP::Percent(_) => (stats_override[0] as f64 * 0.5) as u16,
    };

    // last_*_damage_taken: used by Counter/Mirror Coat/Metal Burst, which Pass 3
    // skips. Use 0 (safe default).
    let last_phys: u16 = match &unk.last_physical_damage_taken {
        PokemonHP::Number(n) => *n,
        PokemonHP::Percent(_) => 0,
    };
    let last_spec: u16 = match &unk.last_special_damage_taken {
        PokemonHP::Number(n) => *n,
        PokemonHP::Percent(_) => 0,
    };
    let last_dmg: u16 = match &unk.last_damage_taken {
        PokemonHP::Number(n) => *n,
        PokemonHP::Percent(_) => 0,
    };

    // Gender: pick any known; fall back to Genderless (doesn't affect damage).
    let gender = match &unk.possible_genders {
        Unknown::Known(g) => *g,
        _ => PokemonGender::Genderless,
    };

    // Weight: use the known weight (1:1 with species).
    let weight_hg = match &unk.possible_weight_hg {
        Unknown::Known(w) => *w,
        _ => 0,
    };

    // Nature: doesn't matter for the oracle (stats are already overridden),
    // but pick something neutral so any nature-dependent code is predictable.
    let nature = match &unk.possible_natures {
        Unknown::Known(n) => *n,
        _ => Nature::Hardy,
    };

    // Moves: fill known slots; None for unknown (only matters for Last Resort).
    let moves: [Option<PokemonMove>; 4] = unk.known_moves.clone();
    let move_pp: [u8; 4] = [
        unk.move_pp[0].max(0) as u8,
        unk.move_pp[1].max(0) as u8,
        unk.move_pp[2].max(0) as u8,
        unk.move_pp[3].max(0) as u8,
    ];
    let max_pp: [u8; 4] = [
        unk.max_pp[0].max(1) as u8,
        unk.max_pp[1].max(1) as u8,
        unk.max_pp[2].max(1) as u8,
        unk.max_pp[3].max(1) as u8,
    ];

    PokemonState {
        mon_id: match &unk.possible_mon_id {
            Unknown::Known(id) => *id,
            _ => 0,
        },
        fainted: unk.fainted,
        species,
        types,
        is_tera: unk.is_tera,
        is_mega: unk.is_mega,
        has_mega_form: mega_species.is_some(),
        level: unk.level,
        hp,
        moves,
        move_pp,
        max_pp,
        item,
        consumed_item: unk.consumed_item.clone(),
        cud_chew_pending: unk.cud_chew_pending.clone(),
        item_lost: unk.item_lost,
        damaged_this_turn: unk.damaged_this_turn,
        damaged_by_this_turn: unk.damaged_by_this_turn.clone(),
        last_physical_damage_taken: last_phys,
        last_physical_attacker: unk.last_physical_attacker,
        last_special_damage_taken: last_spec,
        last_special_attacker: unk.last_special_attacker,
        last_damage_taken: last_dmg,
        last_damage_attacker: unk.last_damage_attacker,
        stats_raised_this_turn: unk.stats_raised_this_turn,
        stats_lowered_this_turn: unk.stats_lowered_this_turn,
        switched_in_this_turn: unk.switched_in_this_turn,
        stall_counter: unk.stall_counter,
        ally_switch_counter: unk.ally_switch_counter,
        nature,
        boosts: unk.boosts,
        stats: stats_override,
        status: unk.status.clone(),
        volatiles: unk.volatiles.clone(),
        ability,
        gender,
        weight_hg,
        tera_type,
        mega_species,
        mega_ability,
        last_move_failed: unk.last_move_failed,
        original_ability: None,
        last_used_move: unk.last_used_move.clone(),
        consecutive_move_count: unk.consecutive_move_count,
        used_moves_this_field: unk.used_moves_this_field,
        one_time_ability_used: unk.one_time_ability_used,
        ate_berry_this_battle: unk.ate_berry_this_battle,
        first_move_on_field: unk.first_move_on_field,
        first_turn_on_field_pending: unk.first_turn_on_field_pending,
        entered_this_turn: unk.entered_this_turn,
        pre_transform: None,
        pre_mimicry_types: unk.pre_mimicry_types.clone(),
        evs: [0u8; 6],
        ivs: [31u8; 6],
        times_hit: unk.times_hit,
        illusion_disguise: None,
    }
}

/// Materialize a minimal `BattleState` sufficient for the damage oracle.
///
/// `p1_active` and `p2_active` are already-materialized `PokemonState`s for the
/// active slots on each side. Field effects (weather, terrain, screens, pseudo-
/// weathers, slot conditions) are copied from the unknown state.
pub fn materialize_battle(
    unk: &UnknownBattleState,
    p1_active: Vec<PokemonState>,
    p2_active: Vec<PokemonState>,
) -> BattleState {
    // Convert Unknown<u8> turn counters to concrete u8.
    //
    // **Invariant — timer fallback is damage-irrelevant**: the damage oracle reads
    // whether an effect is *active* (weather ≠ None, terrain ≠ None, screen present in
    // side_conditions) — it does not branch on the remaining turn count.  Substituting 3
    // for an unknown timer is therefore safe for all oracle calls; no damage formula
    // inspects the timer value directly.
    //
    // **Invariant — Known(0) is the permanent-effect sentinel** (introduced by the S-A
    // per-effect timer model for primordial weather and entry hazards): these use
    // `Known(0)` to signal "present forever".  The `Known(t) => *t` arm MUST remain
    // first so that `Known(0)` passes through to `0` and is never folded into `_ => 3`.
    // A future refactor must not reorder or merge these arms.
    let weather_turns: Option<u8> = unk.weather_turns.as_ref().map(|wu| match wu {
        Unknown::Known(t) => *t,      // preserves Known(0) permanent-effect sentinel
        _ => 3,                       // arbitrary; does not affect oracle output
    });
    let terrain_turns: Option<u8> = unk.terrain_turns.as_ref().map(|tu| match tu {
        Unknown::Known(t) => *t,
        _ => 3,
    });
    let pseudo_weather_turns: Vec<u8> = unk
        .pseudo_weather_turns
        .iter()
        .map(|pu| match pu {
            Unknown::Known(t) => *t,
            _ => 3,
        })
        .collect();
    let p1_sc_turns: Vec<u8> = unk
        .p1_side_condition_turns
        .iter()
        .map(|t| match t {
            Unknown::Known(v) => *v,
            _ => 3,
        })
        .collect();
    let p2_sc_turns: Vec<u8> = unk
        .p2_side_condition_turns
        .iter()
        .map(|t| match t {
            Unknown::Known(v) => *v,
            _ => 3,
        })
        .collect();

    // Slot conditions: copy p1/p2 slot conditions (affect Future Sight damage, etc.)
    let p1_slot_conds: Vec<Vec<SlotCondition>> = unk.p1_slot_conditions.clone();
    let p2_slot_conds: Vec<Vec<SlotCondition>> = unk.p2_slot_conditions.clone();

    let _n = unk.active_per_side as usize;
    BattleState {
        active_per_side: unk.active_per_side,
        p1_active_mons: p1_active,
        p2_active_mons: p2_active,
        p1_back_mons: Vec::new(),
        p2_back_mons: Vec::new(),
        action_queue: Vec::new(),
        turn_number: unk.turn_number,
        turn_started: unk.turn_started,
        turn_ended: unk.turn_ended,
        p1_has_tera: unk.p1_has_tera,
        p2_has_tera: unk.p2_has_tera,
        p1_has_mega: unk.p1_has_mega,
        p2_has_mega: unk.p2_has_mega,
        weather: unk.weather.clone(),
        weather_turns,
        pseudo_weathers: unk.pseudo_weathers.clone(),
        pseudo_weather_turns,
        terrain: unk.terrain.clone(),
        terrain_turns,
        p1_side_conditions: unk.p1_side_conditions.clone(),
        p1_side_condition_turns: p1_sc_turns,
        p2_side_conditions: unk.p2_side_conditions.clone(),
        p2_side_condition_turns: p2_sc_turns,
        p1_slot_conditions: p1_slot_conds,
        p2_slot_conditions: p2_slot_conds,
        self_switch_pending: None,
        items_consumed_this_turn: Vec::new(),
        last_move_on_field: None,
        sub_damage_dealt: 0,
        round_used_this_turn: unk.round_used_this_turn,
        move_was_prevented: false,
        pending_events: vec![],
        event_observer: None,
    }
}
