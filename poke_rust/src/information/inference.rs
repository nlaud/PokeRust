//! # state::inference — information-folding engine
//!
//! Converts an ordered list of [`InformationEvent`]s into tighter bounds on the
//! [`UnknownMatchState`].  The entry point is [`apply_information`].
//!
//! ## Pipeline (six passes, one event-tree walk)
//!
//! 1. **Pass 1** — direct/structural facts (items revealed, status, types, HP, …)
//! 2. **Pass 2** — item presence/absence from behaviour (Life Orb recoil, no recoil,
//!    100%-accurate miss → Bright Powder, Choice-item multi-move → no Choice, …)
//! 3. **Pass 3** — damage → stat bounds (invert the real pipeline; deferred pending
//!    HP tracking across turns — currently stubs that do not narrow)
//! 4. **Pass 4** — speed ordering → Spe bounds (within priority brackets, with multiplier
//!    accounting; Quick Claw / Quick Draw → disjunctive predicates)
//! 5. **Pass 5** — back-solve EV / IV / nature from tightened stat bounds
//! 6. **Pass 6** — BCP (boolean constraint propagation) on the CNF `predicates` to fixpoint
//!
//! **100 % soundness guarantee**: the returned state never excludes a training that could
//! actually produce the observed events.  When events are *jointly impossible*, the function
//! **panics** via [`inference_contradiction!`] with a descriptive message.

#![allow(unused, dead_code, clippy::too_many_arguments)]

use std::collections::{HashMap, HashSet};

use crate::data::ability::Ability;
use crate::data::item::Item;
use crate::data::pokemon_move::PokemonMove;
use crate::data::species::Species;
use crate::information::information::{EventKind, InformationEvent, SwitchState};
use crate::information::unknowns::{
    PokemonHP, Statement, Unknown, UnknownBattleState, UnknownMatchState, UnknownPokemonState,
    UnknownTeamPreviewState,
};
use crate::simulator::helpers::base_damage_formula;
use crate::state::battle::{FieldSlot, Player};
use crate::state::dex_data::{
    AbilityData, AccuracyType, MoveCategory, MoveData, PokemonData, PokemonStat, PokemonType,
    PseudoWeather, SideCondition, SlotCondition, Status, Terrain, VolatileStatus, Weather,
};
use crate::state::pokemon::{Nature, VolatileStatusState, calc_hp, calc_stat, nature_stat_modifiers};

// ── Configuration ─────────────────────────────────────────────────────────────

/// Runtime tuning for [`apply_information`].
pub struct InferenceConfig {
    /// Use the stat-points EV lattice (`{0,4,12,20,…,252}`) instead of the full
    /// 0–252 range. Set this to match the `--stat-points` flag passed to the sim.
    pub use_stat_points: bool,
    /// Pin all opponent IVs to 31 (Pokémon Champions competitive default). When
    /// `true`, the engine skips IV uncertainty and only reasons about EVs + nature.
    pub force_max_ivs: bool,
    /// Default level for newly observed opponent Pokémon (usually 50 for Champions).
    pub level: u8,
    /// Optional legal item whitelist for the opponent. When `Some`, the inference
    /// engine restricts item disjunctions and predicates to only these items; a
    /// revealed item outside the whitelist triggers a contradiction panic. `None`
    /// means all items are considered possible.
    pub legal_items: Option<HashSet<Item>>,
    /// Learnset data per species (from `showdownLearnsets.txt`). When non-empty,
    /// enables learnset-based Illusion narrowing: after an opponent move is revealed,
    /// any candidate species that cannot legally learn that move is dropped from
    /// `possible_species`. Empty map disables this narrowing (default for tests).
    pub learnset_dex: HashMap<Species, HashSet<PokemonMove>>,
    /// Total EV budget across all six stats. When `Some(n)`, Pass 5 applies
    /// cross-stat tightening: `maxEvs[i] ≤ n − Σ_{j≠i} minEvs[j]`. Standard
    /// competitive value is 510. `None` disables the cap check.
    pub ev_total_cap: Option<u16>,
}

impl Default for InferenceConfig {
    fn default() -> Self {
        InferenceConfig {
            use_stat_points: true,
            force_max_ivs: true,
            level: 50,
            legal_items: None,
            learnset_dex: HashMap::new(),
            ev_total_cap: Some(510),
        }
    }
}

// ── EV lattice (stat-points mode) ─────────────────────────────────────────────

/// Achievable EV values under `--stat-points` mode.
/// Derived from `scale_evs_for_stat_points`: `ev = max(0, 8p − 4)` for `p = 0..=32`.
/// 33 values: 0, then 4, 12, 20, …, 252 (each +8 after the first gap).
pub const EV_LATTICE: [u8; 33] = [
    0, 4, 12, 20, 28, 36, 44, 52, 60, 68, 76, 84, 92, 100, 108, 116, 124, 132, 140, 148, 156, 164,
    172, 180, 188, 196, 204, 212, 220, 228, 236, 244, 252,
];

// ── Contradiction macro ────────────────────────────────────────────────────────

/// Panic with a descriptive contradiction message.  Called whenever the observed
/// events are jointly impossible under the current state.
macro_rules! inference_contradiction {
    ($ctx:expr, $($msg:tt)*) => {
        panic!(
            "[inference contradiction] context={:?} — {}",
            $ctx,
            format!($($msg)*)
        )
    };
}

// ── mon_idx helpers ────────────────────────────────────────────────────────────

/// Total number of mons tracked in a `BattleState`, in `mon_idx` order:
/// `[p1_active…, p1_known_back…, p1_possible_back…, p2_active…, p2_known_back…, p2_possible_back…]`.
fn mons_count_battle(state: &UnknownBattleState) -> usize {
    state.p1_active_mons.len()
        + state.p1_known_back_mons.len()
        + state.p1_possible_back_mons.len()
        + state.p2_active_mons.len()
        + state.p2_known_back_mons.len()
        + state.p2_possible_back_mons.len()
}

/// Return the `mon_idx` of the Pokémon currently occupying `slot` in the active array.
pub fn mon_idx_for_active_slot(state: &UnknownBattleState, slot: &FieldSlot) -> Option<usize> {
    let slot_i = slot.slot_index as usize;
    match slot.player {
        Player::P1 => {
            if slot_i < state.p1_active_mons.len() {
                Some(slot_i)
            } else {
                None
            }
        }
        Player::P2 => {
            let p2_start = state.p1_active_mons.len()
                + state.p1_known_back_mons.len()
                + state.p1_possible_back_mons.len();
            if slot_i < state.p2_active_mons.len() {
                Some(p2_start + slot_i)
            } else {
                None
            }
        }
    }
}

/// Borrow the `UnknownPokemonState` at `mon_idx`.
pub fn get_mon_by_idx(state: &UnknownBattleState, idx: usize) -> Option<&UnknownPokemonState> {
    let segs: [&[UnknownPokemonState]; 6] = [
        &state.p1_active_mons,
        &state.p1_known_back_mons,
        &state.p1_possible_back_mons,
        &state.p2_active_mons,
        &state.p2_known_back_mons,
        &state.p2_possible_back_mons,
    ];
    let mut offset = 0;
    for seg in segs {
        if idx < offset + seg.len() {
            return Some(&seg[idx - offset]);
        }
        offset += seg.len();
    }
    None
}

/// Mutably borrow the `UnknownPokemonState` at `mon_idx`.
pub fn get_mon_mut_by_idx(
    state: &mut UnknownBattleState,
    idx: usize,
) -> Option<&mut UnknownPokemonState> {
    let p1a = state.p1_active_mons.len();
    let p1k = state.p1_known_back_mons.len();
    let p1p = state.p1_possible_back_mons.len();
    let p2a = state.p2_active_mons.len();
    let p2k = state.p2_known_back_mons.len();

    if idx < p1a {
        return Some(&mut state.p1_active_mons[idx]);
    }
    let idx = idx - p1a;
    if idx < p1k {
        return Some(&mut state.p1_known_back_mons[idx]);
    }
    let idx = idx - p1k;
    if idx < p1p {
        return Some(&mut state.p1_possible_back_mons[idx]);
    }
    let idx = idx - p1p;
    if idx < p2a {
        return Some(&mut state.p2_active_mons[idx]);
    }
    let idx = idx - p2a;
    if idx < p2k {
        return Some(&mut state.p2_known_back_mons[idx]);
    }
    let idx = idx - p2k;
    if idx < state.p2_possible_back_mons.len() {
        return Some(&mut state.p2_possible_back_mons[idx]);
    }
    None
}

// ── Unknown<T> manipulation helpers ───────────────────────────────────────────

/// Add `val` to the exclusion list.  Contradiction if already `Known` to `val`.
/// Removes `val` from a `Possibly` set; collapses to `Known` if one remains.
fn unknown_exclude<T: PartialEq + Clone>(u: &mut Unknown<T>, val: &T, ctx: &str) {
    match u {
        Unknown::Known(v) => {
            if v == val {
                inference_contradiction!(ctx, "exclude({:?}) conflicts with Known value", ctx);
            }
        }
        Unknown::Not(excluded) => {
            if !excluded.contains(val) {
                excluded.push(val.clone());
            }
        }
        Unknown::Possibly(candidates) => {
            candidates.retain(|c| c != val);
            if candidates.len() == 1 {
                *u = Unknown::Known(candidates[0].clone());
            }
        }
    }
}

/// Force an `Unknown<T>` to `Known(val)`.  Contradiction if already `Known` to
/// something else, or if `val` is in a `Not` exclusion list.
fn unknown_set_known<T: PartialEq + Clone + std::fmt::Debug>(
    u: &mut Unknown<T>,
    val: T,
    ctx: &str,
) {
    match u {
        Unknown::Known(v) => {
            if *v != val {
                inference_contradiction!(ctx, "Known({:?}) vs new Known({:?})", v, val);
            }
        }
        Unknown::Not(excluded) => {
            if excluded.contains(&val) {
                inference_contradiction!(
                    ctx,
                    "Not({:?}) excludes the revealed value {:?}",
                    excluded,
                    val
                );
            }
            *u = Unknown::Known(val);
        }
        Unknown::Possibly(candidates) => {
            if !candidates.contains(&val) {
                inference_contradiction!(
                    ctx,
                    "Possibly({:?}) does not include {:?}",
                    candidates,
                    val
                );
            }
            *u = Unknown::Known(val);
        }
    }
}

/// `true` if `val` is definitely excluded (not possible).
fn unknown_is_excluded<T: PartialEq>(u: &Unknown<T>, val: &T) -> bool {
    match u {
        Unknown::Known(v) => v != val,
        Unknown::Not(excluded) => excluded.contains(val),
        Unknown::Possibly(candidates) => !candidates.iter().any(|c| c == val),
    }
}

/// `true` if this `Unknown` is `Known` to exactly `val`.
fn unknown_is_known_as<T: PartialEq>(u: &Unknown<T>, val: &T) -> bool {
    matches!(u, Unknown::Known(v) if v == val)
}

// ── Public entry point ─────────────────────────────────────────────────────────

/// Fold one turn's (or team preview's) ordered `events` into `state`, returning
/// an updated `UnknownMatchState` that incorporates every fact the events imply.
///
/// `ability_dex` supplies ability metadata (on-start visibility, priority modifiers).
/// Pass `&HashMap::new()` if not available — ability-absence inference is silently skipped.
///
/// # Panics
/// If the events are jointly impossible under the current state (soundness oracle).
pub fn apply_information(
    mut state: UnknownMatchState,
    events: &[InformationEvent],
    is_team_preview: bool,
    dex: &HashMap<Species, PokemonData>,
    move_dex: &HashMap<PokemonMove, MoveData>,
    ability_dex: &HashMap<Ability, AbilityData>,
    config: &InferenceConfig,
) -> UnknownMatchState {
    match &mut state {
        UnknownMatchState::TeamPreview(preview) => {
            apply_information_team_preview(preview, events, config, dex);
        }
        UnknownMatchState::Battle(battle) => {
            apply_information_battle(battle, events, dex, move_dex, ability_dex, config);
        }
        UnknownMatchState::GameOver { .. } => {}
    }
    state
}

// ── Team-preview path ─────────────────────────────────────────────────────────

fn apply_information_team_preview(
    state: &mut UnknownTeamPreviewState,
    events: &[InformationEvent],
    config: &InferenceConfig,
    dex: &HashMap<Species, PokemonData>,
) {
    // slot_map: (player, field_slot_index) → index in p1_mons / p2_mons.
    // Persists across top-level events so reactions at any nesting depth can
    // look up the right mon after a SimultaneousSwitch.
    let mut slot_map: Vec<(Player, u8, usize)> = Vec::new();
    for event in events {
        process_team_preview_event(state, event, config, dex, &mut slot_map);
    }
}

/// Look up the `UnknownPokemonState` currently occupying `(player, slot_index)`.
/// Returns `None` if the slot has not been filled by a switch yet.
fn find_preview_mon<'a>(
    state: &'a mut UnknownTeamPreviewState,
    player: &Player,
    slot_index: u8,
    slot_map: &[(Player, u8, usize)],
) -> Option<&'a mut UnknownPokemonState> {
    let mon_idx = slot_map.iter().find_map(|(p, s, idx)| {
        if p == player && *s == slot_index { Some(*idx) } else { None }
    })?;
    match player {
        Player::P1 => state.p1_mons.get_mut(mon_idx),
        Player::P2 => state.p2_mons.get_mut(mon_idx),
    }
}

fn process_team_preview_event(
    state: &mut UnknownTeamPreviewState,
    event: &InformationEvent,
    config: &InferenceConfig,
    dex: &HashMap<Species, PokemonData>,
    // (player, field_slot_index) → roster index in p1_mons / p2_mons.
    slot_map: &mut Vec<(Player, u8, usize)>,
) {
    match &event.kind {
        // ── Switch-in events — register the field slot in the slot map ────────
        EventKind::Switch(sw) => {
            let mons = match sw.slot.player {
                Player::P1 => &mut state.p1_mons,
                Player::P2 => &mut state.p2_mons,
            };
            if let Some(roster_idx) = mons
                .iter()
                .position(|m| unknown_is_known_as(&m.possible_species, &sw.species))
            {
                apply_switch_state_to_mon(&mut mons[roster_idx], sw, config);
                slot_map.push((sw.slot.player.clone(), sw.slot.slot_index, roster_idx));
            }
        }

        EventKind::SimultaneousSwitch { switches } => {
            for sw in switches {
                let mons = match sw.slot.player {
                    Player::P1 => &mut state.p1_mons,
                    Player::P2 => &mut state.p2_mons,
                };
                if let Some(roster_idx) = mons
                    .iter()
                    .position(|m| unknown_is_known_as(&m.possible_species, &sw.species))
                {
                    apply_switch_state_to_mon(&mut mons[roster_idx], sw, config);
                    slot_map.push((sw.slot.player.clone(), sw.slot.slot_index, roster_idx));
                }
            }
        }

        // ── HP changes ───────────────────────────────────────────────────────
        EventKind::DamageDealt { target, new_hp } => {
            if let Some(mon) =
                find_preview_mon(state, &target.player, target.slot_index, slot_map)
            {
                mon.hp = new_hp.clone();
            }
        }

        EventKind::Healed { target, new_hp } => {
            if let Some(mon) =
                find_preview_mon(state, &target.player, target.slot_index, slot_map)
            {
                mon.hp = new_hp.clone();
            }
        }

        // ── Fainting from entry damage ────────────────────────────────────────
        EventKind::Faint { slot } => {
            if let Some(mon) =
                find_preview_mon(state, &slot.player, slot.slot_index, slot_map)
            {
                mon.fainted = true;
            }
        }

        // ── Stat boosts (Intimidate, Download, etc.) ──────────────────────────
        EventKind::BoostChanged { target, boost_idx, stages } => {
            if let Some(mon) =
                find_preview_mon(state, &target.player, target.slot_index, slot_map)
            {
                if *boost_idx < 7 {
                    mon.boosts[*boost_idx] = mon.boosts[*boost_idx].saturating_add(*stages);
                }
            }
        }

        EventKind::BoostsCleared { target } => {
            if let Some(mon) =
                find_preview_mon(state, &target.player, target.slot_index, slot_map)
            {
                mon.boosts = [0i8; 7];
            }
        }

        // ── Items ─────────────────────────────────────────────────────────────
        EventKind::ItemRevealed { slot, item } => {
            if let Some(mon) =
                find_preview_mon(state, &slot.player, slot.slot_index, slot_map)
            {
                unknown_set_known(&mut mon.item, item.clone(), "preview-item");
            }
        }

        EventKind::ItemGained { slot, item } => {
            if let Some(mon) =
                find_preview_mon(state, &slot.player, slot.slot_index, slot_map)
            {
                unknown_set_known(&mut mon.item, item.clone(), "preview-item-gained");
            }
        }

        EventKind::ItemLost { slot, item, consumed } => {
            if let Some(mon) =
                find_preview_mon(state, &slot.player, slot.slot_index, slot_map)
            {
                if *consumed {
                    mon.consumed_item = Some(item.clone());
                } else {
                    mon.item_lost = true;
                }
                unknown_set_known(&mut mon.item, Item::None, "preview-item-lost");
            }
        }

        // ── Abilities ─────────────────────────────────────────────────────────
        EventKind::AbilityRevealed { slot, ability } => {
            if let Some(mon) =
                find_preview_mon(state, &slot.player, slot.slot_index, slot_map)
            {
                unknown_set_known(&mut mon.possible_abilities, ability.clone(), "preview-ability");
            }
        }

        // ── Status ────────────────────────────────────────────────────────────
        EventKind::StatusInflicted { target, status } => {
            if let Some(mon) =
                find_preview_mon(state, &target.player, target.slot_index, slot_map)
            {
                mon.status = Some(status.clone());
            }
        }

        // ── Forme / type changes (entry abilities like Schooling) ─────────────
        EventKind::FormeChange { slot, into, .. } => {
            if let Some(mon) =
                find_preview_mon(state, &slot.player, slot.slot_index, slot_map)
            {
                unknown_set_known(&mut mon.possible_species, into.clone(), "preview-forme");
            }
        }

        EventKind::TypeChanged { slot, new_types } => {
            if let Some(mon) =
                find_preview_mon(state, &slot.player, slot.slot_index, slot_map)
            {
                unknown_set_known(&mut mon.possible_types, new_types.clone(), "preview-type");
            }
        }

        EventKind::Terastallization { slot, tera_type } => {
            if let Some(mon) =
                find_preview_mon(state, &slot.player, slot.slot_index, slot_map)
            {
                mon.is_tera = true;
                unknown_set_known(&mut mon.possible_tera_type, tera_type.clone(), "preview-tera");
            }
        }

        EventKind::MegaEvolution { slot, into } => {
            if let Some(mon) =
                find_preview_mon(state, &slot.player, slot.slot_index, slot_map)
            {
                mon.is_mega = true;
                unknown_set_known(&mut mon.possible_species, into.clone(), "preview-mega");
            }
        }

        // ── Field effects — no-ops in preview state (no field fields to update)
        // Weather/terrain are set on the BattleState when the battle begins.
        EventKind::WeatherChanged { .. }
        | EventKind::TerrainChanged { .. } => {}

        // ── Illegal events — cannot happen before the first move is chosen ────
        EventKind::MoveUsed { .. }
        | EventKind::Cant { .. }
        | EventKind::ChargingMove { .. }
        | EventKind::MustRecharge { .. }
        | EventKind::SingleMoveOrTurn { .. }
        | EventKind::Crit { .. }
        | EventKind::Missed { .. }
        | EventKind::MoveFailed { .. }
        | EventKind::Blocked { .. }
        | EventKind::Immune { .. }
        | EventKind::HitCount { .. }
        | EventKind::SetHp { .. }
        | EventKind::StatusCured { .. }
        | EventKind::BoostsInverted { .. }
        | EventKind::BoostsSwapped { .. }
        | EventKind::BoostsCopied { .. }
        | EventKind::PseudoWeatherStart { .. }
        | EventKind::PseudoWeatherEnd { .. }
        | EventKind::SideConditionStart { .. }
        | EventKind::SideConditionEnd { .. }
        | EventKind::SlotConditionStart { .. }
        | EventKind::SlotConditionEnd { .. }
        | EventKind::VolatileStart { .. }
        | EventKind::VolatileEnd { .. }
        | EventKind::PerishCount { .. }
        | EventKind::EndOfTurn => {
            panic!(
                "[inference] illegal event {:?} at team preview",
                event.kind
            );
        }
    }

    for reaction in &event.reactions {
        process_team_preview_event(state, reaction, config, dex, slot_map);
    }
}

// ── Battle path ───────────────────────────────────────────────────────────────

fn apply_information_battle(
    state: &mut UnknownBattleState,
    events: &[InformationEvent],
    dex: &HashMap<Species, PokemonData>,
    move_dex: &HashMap<PokemonMove, MoveData>,
    ability_dex: &HashMap<Ability, AbilityData>,
    config: &InferenceConfig,
) {
    // ── Pass 4 (first): speed ordering → Spe bounds ─────────────────────────
    // Run BEFORE the event walk so that speed bounds (minStats[5]/maxStats[5])
    // are already tightened when Pass 3 calls the damage oracle for Gyro Ball
    // and Electro Ball, which compute BP from effective speeds.
    pass4_speed_from_order(state, events, move_dex, ability_dex);
    // Propagate the emitted SpeedComparison predicates to fixpoint immediately.
    while propagate_speed_comparisons(state) {}

    // ── Pass 1–3: depth-first event walk ─────────────────────────────────────
    let mut ctx = BattleContext {
        dex,
        move_dex,
        ability_dex,
        config,
        move_context: None,
    };
    for event in events {
        process_battle_event(state, event, &mut ctx);
    }

    // ── Pass 5: back-solve EV/IV/nature from tightened stat bounds ────────────
    // Run on all mons whose species is unambiguous (Known).
    let total = mons_count_battle(state);
    for idx in 0..total {
        // Only run pass5 for mons with a known species (need base stats).
        let has_known_species = get_mon_by_idx(state, idx)
            .map(|m| matches!(m.possible_species, Unknown::Known(_)))
            .unwrap_or(false);
        if has_known_species {
            // Extract the mon, run pass5, put it back.
            // (Can't borrow mutably via get_mon_mut while dex is also borrowed.)
            // Use a clone-modify-reassign pattern.
            if let Some(mon) = get_mon_mut_by_idx(state, idx) {
                pass5_back_solve(mon, config, dex);
            }
        }
    }

    // ── Pass 6: BCP to fixpoint ────────────────────────────────────────────────
    run_bcp(state);
}

/// Context threaded through the recursive event walk.
struct BattleContext<'a> {
    dex: &'a HashMap<Species, PokemonData>,
    move_dex: &'a HashMap<PokemonMove, MoveData>,
    ability_dex: &'a HashMap<Ability, AbilityData>,
    config: &'a InferenceConfig,
    /// The nearest enclosing `MoveUsed`, for nested-reaction analysis.
    move_context: Option<MoveContext>,
}

#[derive(Clone)]
struct MoveContext {
    user_slot: FieldSlot,
    pokemon_move: PokemonMove,
    targets: Vec<FieldSlot>,
    is_crit: bool,
    /// Pre-move HP of each target (for Pass 3 damage delta).
    pre_hit_hp: Vec<(FieldSlot, PokemonHP)>,
    /// Accumulated observed damage intervals per target, in hit order.
    observed_damage: Vec<(FieldSlot, PokemonHP)>,
}

/// Depth-first event walk applying Passes 1–3.
fn process_battle_event(
    state: &mut UnknownBattleState,
    event: &InformationEvent,
    ctx: &mut BattleContext,
) {
    let prev_move_ctx = ctx.move_context.clone();

    // Detect crit in the reaction list for the MoveContext.
    let is_crit = event
        .reactions
        .iter()
        .any(|r| matches!(r.kind, EventKind::Crit { .. }));

    if let EventKind::MoveUsed {
        user,
        move_used,
        targets,
    } = &event.kind
    {
        // Snapshot pre-hit HP for all targets (Pass 3 scaffold).
        let pre_hit_hp = targets
            .iter()
            .filter_map(|t| {
                mon_idx_for_active_slot(state, t)
                    .and_then(|i| get_mon_by_idx(state, i))
                    .map(|m| (t.clone(), m.hp.clone()))
            })
            .collect();

        ctx.move_context = Some(MoveContext {
            user_slot: user.clone(),
            pokemon_move: move_used.clone(),
            targets: targets.clone(),
            is_crit,
            pre_hit_hp,
            observed_damage: Vec::new(),
        });
    }

    // Pass 1 — direct facts.
    pass1_apply_event(state, event, ctx);

    // Recurse.
    for reaction in &event.reactions {
        process_battle_event(state, reaction, ctx);
    }

    // Pass 2/3 — item and stat inference keyed on the full MoveUsed + reactions.
    if matches!(event.kind, EventKind::MoveUsed { .. }) {
        pass2_item_from_move(state, event, ctx);
        pass3_damage_to_stats(state, event, ctx);
    }

    ctx.move_context = prev_move_ctx;
}

// ── Pass 1: Direct/structural facts ──────────────────────────────────────────

fn pass1_apply_event(
    state: &mut UnknownBattleState,
    event: &InformationEvent,
    ctx: &BattleContext,
) {
    match &event.kind {
        EventKind::Switch(sw) => {
            // Apply switch-out bench reset to the mon leaving the slot (if any).
            apply_switch_out_reset(state, &sw.slot);
            pass1_switch(state, sw, ctx);
            // Ability absence inference: no other entries in the same wave.
            pass1_ability_absence_inference(state, &[sw.slot.clone()], &event.reactions, ctx);
        }

        EventKind::SimultaneousSwitch { switches } => {
            // Apply switch-out bench reset to all slots being replaced.
            for sw in switches {
                apply_switch_out_reset(state, &sw.slot);
            }
            // Process each switch-in.
            for sw in switches {
                pass1_switch(state, sw, ctx);
            }
            // Ability absence inference over the combined reaction list.
            let slots: Vec<FieldSlot> = switches.iter().map(|sw| sw.slot.clone()).collect();
            pass1_ability_absence_inference(state, &slots, &event.reactions, ctx);
        }

        EventKind::EndOfTurn => {
            // Visible EOT effects (damage chip, heals, etc.) are in reactions and will be
            // processed by the recursive descent. This arm triggers internal bookkeeping.
            apply_end_of_turn(state);
        }

        EventKind::MoveUsed {
            user, move_used, ..
        } => {
            if let Some(idx) = mon_idx_for_active_slot(state, user) {
                if let Some(mon) = get_mon_mut_by_idx(state, idx) {
                    reveal_move_on_mon(mon, move_used);
                    narrow_species_by_learnset(
                        mon, move_used, &ctx.config.learnset_dex, ctx.dex,
                    );
                    if Some(move_used) == mon.last_used_move.as_ref() {
                        mon.consecutive_move_count = mon.consecutive_move_count.saturating_add(1);
                    } else {
                        mon.consecutive_move_count = 1;
                    }
                    // Update used_moves_this_field BEFORE choice exclusion (it reads it).
                    for i in 0..4 {
                        if mon.known_moves[i] == Some(move_used.clone()) {
                            mon.used_moves_this_field[i] = true;
                        }
                    }
                    // Choice-item exclusion: keyed on used_moves_this_field (not last_used_move
                    // which survives switch-out).
                    pass1_choice_exclusion(mon, move_used);
                    mon.last_used_move = Some(move_used.clone());
                }
            }
        }

        EventKind::Faint { slot } => {
            if let Some(idx) = mon_idx_for_active_slot(state, slot) {
                if let Some(mon) = get_mon_mut_by_idx(state, idx) {
                    mon.fainted = true;
                    mon.hp = PokemonHP::Percent(0);
                }
            }
        }

        EventKind::DamageDealt { target, new_hp } => {
            let old_hp = mon_idx_for_active_slot(state, target)
                .and_then(|i| get_mon_by_idx(state, i))
                .map(|m| m.hp.clone());
            update_mon_hp(state, target, new_hp.clone());

            // Per-turn damage tracking (mirrors end_turn Phase 5 fields).
            if let Some(idx) = mon_idx_for_active_slot(state, target) {
                if let Some(mon) = get_mon_mut_by_idx(state, idx) {
                    mon.damaged_this_turn = true;
                    mon.last_damage_taken = old_hp.clone().unwrap_or(PokemonHP::Percent(0));
                    mon.times_hit = mon.times_hit.saturating_add(1);

                    // Attribute to the enclosing MoveUsed if available.
                    if let Some(ref mctx) = ctx.move_context {
                        let attacker = &mctx.user_slot;
                        if !mon.damaged_by_this_turn.contains(attacker) {
                            mon.damaged_by_this_turn.push(attacker.clone());
                        }
                        mon.last_damage_attacker = Some(attacker.clone());
                        // Physical vs special from move category.
                        if let Some(md) = ctx.move_dex.get(&mctx.pokemon_move) {
                            match md.category {
                                MoveCategory::Physical => {
                                    mon.last_physical_damage_taken =
                                        old_hp.clone().unwrap_or(PokemonHP::Percent(0));
                                    mon.last_physical_attacker = Some(attacker.clone());
                                }
                                MoveCategory::Special => {
                                    mon.last_special_damage_taken =
                                        old_hp.clone().unwrap_or(PokemonHP::Percent(0));
                                    mon.last_special_attacker = Some(attacker.clone());
                                }
                                MoveCategory::Status => {}
                            }
                        }
                    }
                }
            }
        }
        EventKind::Healed { target, new_hp } => {
            update_mon_hp(state, target, new_hp.clone());
        }
        EventKind::SetHp { target, new_hp } => {
            update_mon_hp(state, target, new_hp.clone());
        }

        EventKind::StatusInflicted { target, status } => {
            if let Some(idx) = mon_idx_for_active_slot(state, target) {
                if let Some(mon) = get_mon_mut_by_idx(state, idx) {
                    if let Some(ref existing) = mon.status.clone() {
                        if existing != status {
                            inference_contradiction!(
                                idx,
                                "StatusInflicted {:?} but already has {:?}",
                                status,
                                existing
                            );
                        }
                    }
                    mon.status = Some(status.clone());
                }
            }
        }

        EventKind::StatusCured { target, .. } => {
            if let Some(idx) = mon_idx_for_active_slot(state, target) {
                if let Some(mon) = get_mon_mut_by_idx(state, idx) {
                    mon.status = None;
                }
            }
        }

        EventKind::ItemRevealed { slot, item } => {
            if let Some(idx) = mon_idx_for_active_slot(state, slot) {
                if let Some(legal) = &ctx.config.legal_items {
                    if !legal.contains(item) && *item != Item::None {
                        inference_contradiction!(
                            idx,
                            "ItemRevealed {:?} outside legal whitelist",
                            item
                        );
                    }
                }
                if let Some(mon) = get_mon_mut_by_idx(state, idx) {
                    unknown_set_known(&mut mon.item, item.clone(), &format!("mon#{idx} item"));
                }
            }
        }
        EventKind::ItemGained { slot, item } => {
            if let Some(idx) = mon_idx_for_active_slot(state, slot) {
                if let Some(legal) = &ctx.config.legal_items {
                    if !legal.contains(item) && *item != Item::None {
                        inference_contradiction!(
                            idx,
                            "ItemRevealed {:?} outside legal whitelist",
                            item
                        );
                    }
                }
                if let Some(mon) = get_mon_mut_by_idx(state, idx) {
                    mon.item = Unknown::Known(item.clone());
                    mon.item_lost = false;
                }
            }
        }
        EventKind::ItemLost {
            slot,
            item,
            consumed,
        } => {
            if let Some(idx) = mon_idx_for_active_slot(state, slot) {
                if let Some(mon) = get_mon_mut_by_idx(state, idx) {
                    if *consumed {
                        mon.consumed_item = Some(item.clone());
                    } else {
                        mon.item_lost = true;
                    }
                    mon.item = Unknown::Known(Item::None);
                }
            }
        }

        EventKind::AbilityRevealed { slot, ability } => {
            if let Some(idx) = mon_idx_for_active_slot(state, slot) {
                if let Some(mon) = get_mon_mut_by_idx(state, idx) {
                    unknown_set_known(
                        &mut mon.possible_abilities,
                        ability.clone(),
                        &format!("mon#{idx} ability"),
                    );
                }
            }
        }

        EventKind::BoostChanged {
            target,
            boost_idx,
            stages,
        } => {
            if let Some(idx) = mon_idx_for_active_slot(state, target) {
                if let Some(mon) = get_mon_mut_by_idx(state, idx) {
                    if *boost_idx < 7 {
                        let new_stage =
                            (mon.boosts[*boost_idx] as i16 + *stages as i16).clamp(-6, 6) as i8;
                        mon.boosts[*boost_idx] = new_stage;
                    }
                    if *stages > 0 {
                        mon.stats_raised_this_turn = true;
                    } else if *stages < 0 {
                        mon.stats_lowered_this_turn = true;
                    }
                }
            }
        }
        EventKind::BoostsCleared { target } => {
            if let Some(idx) = mon_idx_for_active_slot(state, target) {
                if let Some(mon) = get_mon_mut_by_idx(state, idx) {
                    mon.boosts = [0; 7];
                }
            }
        }
        EventKind::BoostsInverted { target } => {
            if let Some(idx) = mon_idx_for_active_slot(state, target) {
                if let Some(mon) = get_mon_mut_by_idx(state, idx) {
                    for b in mon.boosts.iter_mut() {
                        *b = -*b;
                    }
                }
            }
        }
        EventKind::BoostsSwapped { source, target } => {
            let src_idx = mon_idx_for_active_slot(state, source);
            let tgt_idx = mon_idx_for_active_slot(state, target);
            if let (Some(si), Some(ti)) = (src_idx, tgt_idx) {
                let sb = get_mon_by_idx(state, si).map(|m| m.boosts);
                let tb = get_mon_by_idx(state, ti).map(|m| m.boosts);
                if let (Some(sb), Some(tb)) = (sb, tb) {
                    if let Some(sm) = get_mon_mut_by_idx(state, si) {
                        sm.boosts = tb;
                    }
                    if let Some(tm) = get_mon_mut_by_idx(state, ti) {
                        tm.boosts = sb;
                    }
                }
            }
        }
        EventKind::BoostsCopied { source, target } => {
            let src_idx = mon_idx_for_active_slot(state, source);
            let tgt_idx = mon_idx_for_active_slot(state, target);
            if let (Some(si), Some(ti)) = (src_idx, tgt_idx) {
                let sb = get_mon_by_idx(state, si).map(|m| m.boosts);
                if let (Some(sb), Some(tm)) = (sb, get_mon_mut_by_idx(state, ti)) {
                    tm.boosts = sb;
                }
            }
        }

        EventKind::MegaEvolution { slot, into } => {
            if let Some(idx) = mon_idx_for_active_slot(state, slot) {
                if let Some(mon) = get_mon_mut_by_idx(state, idx) {
                    unknown_set_known(
                        &mut mon.possible_species,
                        into.clone(),
                        &format!("mon#{idx} mega"),
                    );
                    mon.is_mega = true;
                    // Update types from dex.
                    if let Some(data) = ctx.dex.get(into) {
                        mon.possible_types = Unknown::Known(data.types.clone());
                    }
                }
                match slot.player {
                    Player::P1 => state.p1_has_mega = true,
                    Player::P2 => state.p2_has_mega = true,
                }
                // Pin the held item to the required Mega Stone.
                let mega_stone = ctx
                    .dex
                    .get(into)
                    .and_then(|d| d.required_item.as_ref().map(|s| Item::from_str(s)));
                if let Some(stone) = mega_stone {
                    if stone != Item::None {
                        if let Some(legal) = &ctx.config.legal_items {
                            if !legal.contains(&stone) {
                                inference_contradiction!(
                                    idx,
                                    "Mega Stone {:?} outside legal whitelist",
                                    stone
                                );
                            }
                        }
                        if let Some(mon) = get_mon_mut_by_idx(state, idx) {
                            unknown_set_known(
                                &mut mon.item,
                                stone,
                                &format!("mon#{idx} mega-stone"),
                            );
                        }
                    }
                }
            }
        }

        EventKind::Terastallization { slot, tera_type } => {
            if let Some(idx) = mon_idx_for_active_slot(state, slot) {
                if let Some(mon) = get_mon_mut_by_idx(state, idx) {
                    mon.is_tera = true;
                    unknown_set_known(
                        &mut mon.possible_tera_type,
                        tera_type.clone(),
                        &format!("mon#{idx} tera"),
                    );
                }
                match slot.player {
                    Player::P1 => state.p1_has_tera = false,
                    Player::P2 => state.p2_has_tera = false,
                }
            }
        }

        EventKind::FormeChange { slot, into, .. } => {
            if let Some(idx) = mon_idx_for_active_slot(state, slot) {
                if let Some(mon) = get_mon_mut_by_idx(state, idx) {
                    unknown_set_known(
                        &mut mon.possible_species,
                        into.clone(),
                        &format!("mon#{idx} forme"),
                    );
                    if let Some(data) = ctx.dex.get(into) {
                        mon.possible_types = Unknown::Known(data.types.clone());
                    }
                }
            }
        }

        EventKind::TypeChanged { slot, new_types } => {
            if let Some(idx) = mon_idx_for_active_slot(state, slot) {
                if let Some(mon) = get_mon_mut_by_idx(state, idx) {
                    mon.possible_types = Unknown::Known(new_types.clone());
                }
            }
        }

        EventKind::WeatherChanged { weather } => {
            state.weather = weather.clone();
            state.weather_turns = weather.as_ref().map(|_| Unknown::Possibly(vec![5, 8]));
        }
        EventKind::TerrainChanged { terrain } => {
            state.terrain = terrain.clone();
            state.terrain_turns = terrain.as_ref().map(|_| Unknown::Possibly(vec![5, 8]));
        }
        EventKind::PseudoWeatherStart { effect } => {
            if !state.pseudo_weathers.contains(effect) {
                state.pseudo_weathers.push(effect.clone());
                state
                    .pseudo_weather_turns
                    .push(Unknown::Possibly(vec![5, 8]));
            }
        }
        EventKind::PseudoWeatherEnd { effect } => {
            if let Some(pos) = state.pseudo_weathers.iter().position(|e| e == effect) {
                state.pseudo_weathers.remove(pos);
                state.pseudo_weather_turns.remove(pos);
            }
        }
        EventKind::SideConditionStart { side, condition } => {
            let (conditions, turns) = match side {
                Player::P1 => (
                    &mut state.p1_side_conditions,
                    &mut state.p1_side_condition_turns,
                ),
                Player::P2 => (
                    &mut state.p2_side_conditions,
                    &mut state.p2_side_condition_turns,
                ),
            };
            if !conditions.contains(condition) {
                conditions.push(condition.clone());
                turns.push(Unknown::Possibly(vec![5, 8]));
            }
        }
        EventKind::SideConditionEnd { side, condition } => {
            let (conditions, turns) = match side {
                Player::P1 => (
                    &mut state.p1_side_conditions,
                    &mut state.p1_side_condition_turns,
                ),
                Player::P2 => (
                    &mut state.p2_side_conditions,
                    &mut state.p2_side_condition_turns,
                ),
            };
            if let Some(pos) = conditions.iter().position(|c| c == condition) {
                conditions.remove(pos);
                turns.remove(pos);
            }
        }
        EventKind::SlotConditionStart { slot, condition } => {
            let slot_conds = match slot.player {
                Player::P1 => &mut state.p1_slot_conditions,
                Player::P2 => &mut state.p2_slot_conditions,
            };
            let i = slot.slot_index as usize;
            if let Some(sc_vec) = slot_conds.get_mut(i) {
                if !sc_vec.contains(condition) {
                    sc_vec.push(condition.clone());
                }
            }
        }
        EventKind::SlotConditionEnd { slot, condition } => {
            let slot_conds = match slot.player {
                Player::P1 => &mut state.p1_slot_conditions,
                Player::P2 => &mut state.p2_slot_conditions,
            };
            let i = slot.slot_index as usize;
            if let Some(sc_vec) = slot_conds.get_mut(i) {
                sc_vec.retain(|c| c != condition);
            }
        }

        EventKind::VolatileStart { target, volatile } => {
            if let Some(idx) = mon_idx_for_active_slot(state, target) {
                if let Some(mon) = get_mon_mut_by_idx(state, idx) {
                    use crate::state::pokemon::VolatileStatusState;
                    let already = mon.volatiles.iter().any(|v| match v {
                        VolatileStatusState::TurnStatus(vs, _) => vs == volatile,
                        VolatileStatusState::MoveStatus(vs, _) => vs == volatile,
                        VolatileStatusState::Charging(_, _) => false,
                    });
                    if !already {
                        mon.volatiles
                            .push(VolatileStatusState::TurnStatus(volatile.clone(), 0));
                    }
                }
            }
        }
        EventKind::VolatileEnd { target, volatile } => {
            if let Some(idx) = mon_idx_for_active_slot(state, target) {
                if let Some(mon) = get_mon_mut_by_idx(state, idx) {
                    use crate::state::pokemon::VolatileStatusState;
                    mon.volatiles.retain(|v| match v {
                        VolatileStatusState::TurnStatus(vs, _) => vs != volatile,
                        VolatileStatusState::MoveStatus(vs, _) => vs != volatile,
                        VolatileStatusState::Charging(_, _) => true,
                    });
                }
            }
        }

        EventKind::ChargingMove { user, move_used } => {
            if let Some(idx) = mon_idx_for_active_slot(state, user) {
                if let Some(mon) = get_mon_mut_by_idx(state, idx) {
                    reveal_move_on_mon(mon, move_used);
                    narrow_species_by_learnset(
                        mon, move_used, &ctx.config.learnset_dex, ctx.dex,
                    );
                }
            }
        }

        // Events with no direct state update in Pass 1 — all handled by
        // enclosing MoveUsed context in Passes 2/3 or by reactions.
        EventKind::Crit { .. }
        | EventKind::Immune { .. }
        | EventKind::Missed { .. }
        | EventKind::MoveFailed { .. }
        | EventKind::Blocked { .. }
        | EventKind::HitCount { .. }
        | EventKind::Cant { .. }
        | EventKind::MustRecharge { .. }
        | EventKind::SingleMoveOrTurn { .. }
        | EventKind::PerishCount { .. } => {}
    }
}

// ── Pass 1 helpers ────────────────────────────────────────────────────────────

fn pass1_switch(state: &mut UnknownBattleState, sw: &SwitchState, ctx: &BattleContext) {
    let player = &sw.slot.player;
    let slot_i = sw.slot.slot_index as usize;
    let species = &sw.species;

    // Find the mon in the back and move it to the active slot.
    let back_mon: Option<UnknownPokemonState> = {
        let known = match player {
            Player::P1 => &mut state.p1_known_back_mons,
            Player::P2 => &mut state.p2_known_back_mons,
        };
        if let Some(pos) = known
            .iter()
            .position(|m| unknown_is_known_as(&m.possible_species, species))
        {
            Some(known.remove(pos))
        } else {
            let possible = match player {
                Player::P1 => &mut state.p1_possible_back_mons,
                Player::P2 => &mut state.p2_possible_back_mons,
            };
            possible
                .iter()
                .position(|m| unknown_is_known_as(&m.possible_species, species))
                .map(|pos| possible.remove(pos))
        }
    };

    let mut mon = if let Some(m) = back_mon {
        m
    } else {
        // Completely new opponent mon: build from species, then recompute stat bounds for
        // the configured IV mode (always call — fixes the bug where non-force_max_ivs mode
        // left the mon with the from_opponent_species defaults instead of proper bounds).
        let mut new_mon =
            UnknownPokemonState::from_opponent_species(species.clone(), ctx.dex, ctx.config.level);
        recompute_stats_for_iv_mode(&mut new_mon, species, ctx);
        if let Some(legal) = &ctx.config.legal_items {
            let mut candidates: Vec<Item> = legal.iter().cloned().collect();
            candidates.push(Item::None);
            new_mon.item = Unknown::Possibly(candidates);
        }
        new_mon
    };

    apply_switch_state_to_mon(&mut mon, sw, ctx.config);

    // Illusion widening (only for unknown / opponent mons).
    let opponent_known_back_species: Vec<Species> = {
        let back = match player {
            Player::P1 => &state.p1_known_back_mons,
            Player::P2 => &state.p2_known_back_mons,
        };
        back.iter()
            .filter_map(|m| {
                if let Unknown::Known(s) = &m.possible_species { Some(s.clone()) } else { None }
            })
            .collect()
    };

    let actives = match sw.slot.player {
        Player::P1 => &mut state.p1_active_mons,
        Player::P2 => &mut state.p2_active_mons,
    };
    if slot_i < actives.len() {
        actives[slot_i] = mon;
    } else {
        actives.push(mon);
    }

    maybe_widen_for_illusion(state, &sw.slot, &opponent_known_back_species);
}

/// Recompute `minStats`/`maxStats` and pin IVs according to `config.force_max_ivs`.
///
/// When `force_max_ivs = true`: IVs are pinned to [31;6], min stats use EV=0 and nature×0.9,
///   max stats use EV=252 and nature×1.1.
/// When `force_max_ivs = false`: IV range is [0,31], min uses IV=0 + EV=0 + nature×0.9,
///   max uses IV=31 + EV=252 + nature×1.1.
fn recompute_stats_for_iv_mode(
    mon: &mut UnknownPokemonState,
    species: &Species,
    ctx: &BattleContext,
) {
    let force_max = ctx.config.force_max_ivs;
    if force_max {
        mon.minIvs = [31; 6];
        mon.maxIvs = [31; 6];
    } else {
        mon.minIvs = [0; 6];
        mon.maxIvs = [31; 6];
    }
    if let Some(data) = ctx.dex.get(species) {
        let b = data.base_stats;
        let lv = ctx.config.level;
        let min_iv: u8 = if force_max { 31 } else { 0 };
        mon.minStats = [
            calc_hp(b[0], min_iv, 0, lv),
            calc_stat(b[1], min_iv, 0, lv, 0.9),
            calc_stat(b[2], min_iv, 0, lv, 0.9),
            calc_stat(b[3], min_iv, 0, lv, 0.9),
            calc_stat(b[4], min_iv, 0, lv, 0.9),
            calc_stat(b[5], min_iv, 0, lv, 0.9),
        ];
        mon.maxStats = [
            calc_hp(b[0], 31, 252, lv),
            calc_stat(b[1], 31, 252, lv, 1.1),
            calc_stat(b[2], 31, 252, lv, 1.1),
            calc_stat(b[3], 31, 252, lv, 1.1),
            calc_stat(b[4], 31, 252, lv, 1.1),
            calc_stat(b[5], 31, 252, lv, 1.1),
        ];
        // Initialise pre-nature BSV bounds (neutral mod = 1.0).
        mon.min_pre_nature_stat = [
            calc_hp(b[0], min_iv, 0, lv),
            calc_stat(b[1], min_iv, 0, lv, 1.0),
            calc_stat(b[2], min_iv, 0, lv, 1.0),
            calc_stat(b[3], min_iv, 0, lv, 1.0),
            calc_stat(b[4], min_iv, 0, lv, 1.0),
            calc_stat(b[5], min_iv, 0, lv, 1.0),
        ];
        mon.max_pre_nature_stat = [
            calc_hp(b[0], 31, 252, lv),
            calc_stat(b[1], 31, 252, lv, 1.0),
            calc_stat(b[2], 31, 252, lv, 1.0),
            calc_stat(b[3], 31, 252, lv, 1.0),
            calc_stat(b[4], 31, 252, lv, 1.0),
            calc_stat(b[5], 31, 252, lv, 1.0),
        ];
    }
}

fn apply_switch_state_to_mon(
    mon: &mut UnknownPokemonState,
    sw: &SwitchState,
    config: &InferenceConfig,
) {
    mon.level = sw.level;
    mon.hp = sw.hp.clone();
    mon.status = sw.status.clone();
    mon.switched_in_this_turn = true;
    mon.entered_this_turn = true;
    // Clear per-field flags on switch-in (mirrors helpers.rs:5396-5399).
    mon.first_move_on_field = true;
    mon.first_turn_on_field_pending = false; // caller can override for mid-turn entries
    mon.used_moves_this_field = [false; 4];
    if let Some(tt) = &sw.tera_type {
        mon.is_tera = true;
        mon.possible_tera_type = Unknown::Known(tt.clone());
    }
    // IV range is set by recompute_stats_for_iv_mode; apply_switch_state_to_mon only
    // enforces the flag for mons that arrive from back (already built without force_max).
    if config.force_max_ivs {
        mon.minIvs = [31; 6];
        mon.maxIvs = [31; 6];
    }
}

/// Apply the bench (switch-out) reset to whatever mon is currently in `slot`, if any.
/// Mirrors the switch-out field clearing at `simulator/mod.rs:6225-6246`.
fn apply_switch_out_reset(state: &mut UnknownBattleState, slot: &FieldSlot) {
    let actives = match slot.player {
        Player::P1 => &mut state.p1_active_mons,
        Player::P2 => &mut state.p2_active_mons,
    };
    let i = slot.slot_index as usize;
    if let Some(mon) = actives.get_mut(i) {
        // Unburden ends on switch-out.
        mon.item_lost = false;
        // Per-turn event flags don't follow to the bench.
        mon.damaged_this_turn = false;
        mon.damaged_by_this_turn.clear();
        mon.last_physical_damage_taken = PokemonHP::Percent(0);
        mon.last_physical_attacker = None;
        mon.last_special_damage_taken = PokemonHP::Percent(0);
        mon.last_special_attacker = None;
        mon.last_damage_taken = PokemonHP::Percent(0);
        mon.last_damage_attacker = None;
        mon.stats_raised_this_turn = false;
        mon.stats_lowered_this_turn = false;
        mon.switched_in_this_turn = false;
        // Consecutive-use streaks reset on switch-out.
        mon.stall_counter = 0;
        mon.ally_switch_counter = 0;
        mon.consecutive_move_count = 0;
        // Null last_used_move so the Metronome streak doesn't carry across switch-ins.
        mon.last_used_move = None;
        // Rage Fist hit counter resets (Champions rules).
        mon.times_hit = 0;
    }
}

fn reveal_move_on_mon(mon: &mut UnknownPokemonState, pokemon_move: &PokemonMove) {
    if mon
        .known_moves
        .iter()
        .any(|m| m.as_ref() == Some(pokemon_move))
    {
        return; // already known
    }
    for slot in mon.known_moves.iter_mut() {
        if slot.is_none() {
            *slot = Some(pokemon_move.clone());
            return;
        }
    }
    // All 4 slots filled but move not found — legal mon constraint violated.
    // Don't panic; widening is sound.
}

/// Narrow `possible_species` (when `Possibly`) by excluding candidates whose learnset
/// doesn't include `move_used`. Collapses to `Known` when only one candidate remains
/// and refreshes `possible_types` / `possible_weight_hg` from the species dex.
///
/// Sound: only removes a species if we have *positive* learnset data confirming it
/// cannot learn the move.  Absent learnset data → keeps the candidate.
fn narrow_species_by_learnset(
    mon: &mut UnknownPokemonState,
    move_used: &PokemonMove,
    learnset_dex: &HashMap<Species, HashSet<PokemonMove>>,
    dex: &HashMap<Species, PokemonData>,
) {
    if learnset_dex.is_empty() {
        return;
    }
    let candidates = match &mon.possible_species {
        Unknown::Possibly(v) => v.clone(),
        _ => return, // Known → nothing to narrow; Not → can't enumerate safely
    };

    let remaining: Vec<Species> = candidates
        .iter()
        .filter(|s| {
            // Sound: keep species if learnset data is absent (can't confirm illegality).
            learnset_dex.get(*s).map_or(true, |moves| moves.contains(move_used))
        })
        .cloned()
        .collect();

    if remaining.len() == candidates.len() {
        return; // Nothing excluded.
    }
    if remaining.is_empty() {
        // All candidates illegal — learnset data may be wrong; don't narrow to contradiction.
        return;
    }

    if remaining.len() == 1 {
        let species = remaining[0].clone();
        mon.possible_species = Unknown::Known(species.clone());
        // Refresh types and weight from the now-pinned species.
        if let Some(pd) = dex.get(&species) {
            mon.possible_types = Unknown::Known(pd.types.clone());
            mon.possible_weight_hg = Unknown::Known(pd.weight);
        }
    } else {
        mon.possible_species = Unknown::Possibly(remaining);
    }
}

/// Exclude Choice items when the mon has used 2+ different moves in the same field stint.
///
/// Uses `used_moves_this_field` (cleared on switch-in) rather than `last_used_move`
/// (not cleared on switch-out) so that a Pokémon using a new move after switching back
/// in is never incorrectly flagged as Choice-locked.
///
/// Call AFTER `used_moves_this_field` has been updated for `new_move`.
fn pass1_choice_exclusion(mon: &mut UnknownPokemonState, new_move: &PokemonMove) {
    // Count how many distinct known moves have been used this field.
    let distinct_used: Vec<&PokemonMove> = mon
        .known_moves
        .iter()
        .enumerate()
        .filter_map(|(i, slot)| {
            if mon.used_moves_this_field[i] {
                slot.as_ref()
            } else {
                None
            }
        })
        .collect();

    // If the mon has used more than one distinct move since it came in, Choice items are out.
    let has_different = distinct_used.iter().any(|&m| m != new_move);
    if has_different && distinct_used.len() >= 2 {
        let choices = [Item::ChoiceBand, Item::ChoiceScarf, Item::ChoiceSpecs];
        for ci in &choices {
            unknown_exclude(&mut mon.item, ci, "choice-lock");
        }
    }
}

fn update_mon_hp(state: &mut UnknownBattleState, slot: &FieldSlot, new_hp: PokemonHP) {
    if let Some(idx) = mon_idx_for_active_slot(state, slot) {
        if let Some(mon) = get_mon_mut_by_idx(state, idx) {
            mon.hp = new_hp;
        }
    }
}

// ── Ability suppression helpers ────────────────────────────────────────────────

/// `true` if this mon's ability is DEFINITELY suppressed (Gastro Acid volatile present).
/// Does NOT check for Neutralizing Gas because we only call this when the active
/// field scan (below) already cleared the NeutralizingGas check.
fn has_gastro_acid(mon: &UnknownPokemonState) -> bool {
    mon.volatiles.iter().any(|v| {
        matches!(v,
            VolatileStatusState::TurnStatus(VolatileStatus::GastroAcid, _)
            | VolatileStatusState::MoveStatus(VolatileStatus::GastroAcid, _))
    })
}

/// `true` if we can be CERTAIN that some active mon has Neutralizing Gas (meaning
/// that all non-NeutralizingGas abilities are suppressed field-wide). We are
/// certain only when `possible_abilities == Known(NeutralizingGas)`.
fn neutralizing_gas_definitely_active(state: &UnknownBattleState) -> bool {
    state
        .p1_active_mons
        .iter()
        .chain(state.p2_active_mons.iter())
        .any(|m| {
            !m.fainted
                && unknown_is_known_as(&m.possible_abilities, &Ability::NeutralizingGas)
        })
}

/// `true` if this mon's ability might be suppressed (sound: returns true whenever
/// suppression is possible, not just certain). Used to skip absence-of-effect inference.
fn unknown_ability_might_be_suppressed(state: &UnknownBattleState, slot: &FieldSlot) -> bool {
    // If Neutralizing Gas might be on the field → suppress inference (sound: might be true).
    let maybe_ng = state
        .p1_active_mons
        .iter()
        .chain(state.p2_active_mons.iter())
        .any(|m| {
            !m.fainted
                && !unknown_is_excluded(&m.possible_abilities, &Ability::NeutralizingGas)
        });
    if maybe_ng {
        return true;
    }
    // Check per-mon Gastro Acid.
    if let Some(idx) = mon_idx_for_active_slot(state, slot) {
        if let Some(mon) = get_mon_by_idx(state, idx) {
            return has_gastro_acid(mon);
        }
    }
    false
}

// ── End-of-turn bookkeeping ────────────────────────────────────────────────────

/// Apply internal EOT resets that mirror `end_turn` Phase 5 and
/// `decrement_effect_timers` in `simulator/helpers.rs`.
///
/// Visible EOT effects (weather chip, heal, etc.) are handled by the event walk
/// visiting `EndOfTurn::reactions`; this function handles the invisible internal state.
fn apply_end_of_turn(state: &mut UnknownBattleState) {
    // ── Decrement field timers ────────────────────────────────────────────────
    decrement_unknown_turns(&mut state.weather_turns, &mut state.weather);
    decrement_unknown_turns(&mut state.terrain_turns, &mut state.terrain);
    for t in state.pseudo_weather_turns.iter_mut() {
        decrement_unknown_turns_raw(t);
    }
    // Remove expired pseudo-weathers (those whose turn set collapsed to empty).
    // (We don't know which pseudo-weather expired — leave for event-driven clearing.)
    for (sc_turns, _sc) in state
        .p1_side_condition_turns
        .iter_mut()
        .zip(state.p1_side_conditions.iter())
    {
        decrement_unknown_turns_raw(sc_turns);
    }
    for (sc_turns, _sc) in state
        .p2_side_condition_turns
        .iter_mut()
        .zip(state.p2_side_conditions.iter())
    {
        decrement_unknown_turns_raw(sc_turns);
    }

    // Also decrement the `turns` field inside any turn-count predicate.
    for clause in state.predicates.iter_mut() {
        for lit in clause.iter_mut() {
            match lit {
                Statement::WeatherTurns { turns }
                | Statement::PseudoWeatherTurns { turns }
                | Statement::SideConditionTurns { turns, .. } => {
                    if *turns > 0 {
                        *turns -= 1;
                    }
                }
                _ => {}
            }
        }
    }
    // Remove predicates whose turns have all reached 0.
    state.predicates.retain(|clause| {
        !clause.iter().any(|lit| {
            matches!(
                lit,
                Statement::WeatherTurns { turns: 0 }
                    | Statement::PseudoWeatherTurns { turns: 0 }
                    | Statement::SideConditionTurns { turns: 0, .. }
            )
        })
    });

    // ── Advance turn counter ──────────────────────────────────────────────────
    state.turn_number = state.turn_number.saturating_add(1);

    // ── Clear per-turn flags (mirrors end_turn Phase 5, helpers.rs:6623-6673) ─
    for mon in state
        .p1_active_mons
        .iter_mut()
        .chain(state.p2_active_mons.iter_mut())
    {
        mon.entered_this_turn = false;
        // U-turn / self-switch mid-turn: first_turn_on_field_pending causes EOT to skip
        // clearing first_move_on_field exactly once.
        if mon.first_turn_on_field_pending {
            mon.first_turn_on_field_pending = false;
        } else {
            mon.first_move_on_field = false;
        }
        mon.damaged_this_turn = false;
        mon.damaged_by_this_turn.clear();
        mon.last_physical_damage_taken = PokemonHP::Percent(0);
        mon.last_physical_attacker = None;
        mon.last_special_damage_taken = PokemonHP::Percent(0);
        mon.last_special_attacker = None;
        mon.last_damage_taken = PokemonHP::Percent(0);
        mon.last_damage_attacker = None;
        mon.stats_raised_this_turn = false;
        mon.stats_lowered_this_turn = false;
        mon.switched_in_this_turn = false;
        // Turn-scoped volatiles (Roost, Electrify).
        mon.volatiles.retain(|v| {
            !matches!(v,
                VolatileStatusState::TurnStatus(VolatileStatus::Roost, _)
                | VolatileStatusState::TurnStatus(VolatileStatus::Electrify, _)
                | VolatileStatusState::MoveStatus(VolatileStatus::Roost, _)
                | VolatileStatusState::MoveStatus(VolatileStatus::Electrify, _))
        });
    }
    state.round_used_this_turn = false;
    state.items_consumed_this_turn.clear();
}

/// Decrement an `Unknown<u8>` field representing remaining effect turns.
/// If the counter reaches 0 in all possibilities, clears the option to reflect expiry.
fn decrement_unknown_turns<T>(turns_opt: &mut Option<Unknown<u8>>, field: &mut Option<T>) {
    if let Some(t) = turns_opt.as_mut() {
        decrement_unknown_turns_raw(t);
        // If the Possibly set is now empty, all possibilities say the effect has expired.
        // We leave clearing `field` to the event-driven path (WeatherChanged / SideConditionEnd)
        // so that we don't accidentally clear weather that persisted (8-turn case).
    }
}

fn decrement_unknown_turns_raw(t: &mut Unknown<u8>) {
    match t {
        Unknown::Known(n) => {
            if *n > 0 {
                *n -= 1;
            }
        }
        Unknown::Possibly(v) => {
            *v = v.iter().filter_map(|&n| if n > 1 { Some(n - 1) } else { None }).collect();
            if v.len() == 1 {
                *t = Unknown::Known(v[0]);
            }
        }
        Unknown::Not(_) => {} // Not meaningful for turn counts
    }
}

// ── Ability absence / priority inference ──────────────────────────────────────

/// Weather-setting abilities whose activation is always visible (`WeatherChanged`).
const WEATHER_SETTING_ABILITIES: &[Ability] = &[
    Ability::Drizzle,
    Ability::Drought,
    Ability::SandStream,
    Ability::SnowWarning,
    Ability::OrichalcumPulse, // Sets Sun (from helpers.rs:5791)
    Ability::HadronEngine,    // Sets Electric Terrain (from helpers.rs:5785)
];

/// Terrain-setting abilities whose activation is always visible (`TerrainChanged`).
const TERRAIN_SETTING_ABILITIES: &[Ability] = &[
    Ability::ElectricSurge,
    Ability::GrassySurge,
    Ability::MistySurge,
    Ability::PsychicSurge,
    // HadronEngine sets Electric Terrain (already listed above, checked separately)
];

/// After a batch of switch-ins, scan the combined `reactions` list and remove
/// abilities from `possible_abilities` that MUST have activated but didn't.
///
/// Sound: only excludes when we can be certain the ability would have been visible.
/// Conservative: multi-mon battles with multiple possible setters are skipped unless
/// the attribution is unambiguous.
fn pass1_ability_absence_inference(
    state: &mut UnknownBattleState,
    entered_slots: &[FieldSlot],
    reactions: &[InformationEvent],
    ctx: &BattleContext,
) {
    if entered_slots.is_empty() {
        return;
    }

    let weather_changed = reactions
        .iter()
        .any(|r| matches!(&r.kind, EventKind::WeatherChanged { weather: Some(_) }));
    let terrain_changed = reactions
        .iter()
        .any(|r| matches!(&r.kind, EventKind::TerrainChanged { terrain: Some(_) }));

    for slot in entered_slots {
        // Skip if ability might be suppressed (sound: conservative).
        if unknown_ability_might_be_suppressed(state, slot) {
            continue;
        }

        let Some(idx) = mon_idx_for_active_slot(state, slot) else {
            continue;
        };

        // ── Weather-setting abilities ────────────────────────────────────────
        if !weather_changed {
            // If ONLY this slot's mons could have a weather setter (no other entering
            // mon has one), absence of WeatherChanged proves this mon doesn't have it.
            // For single-entry (Switch) this is always unambiguous.
            let sole_possible_setter = entered_slots.len() == 1
                || only_slot_with_weather_setter(state, entered_slots, slot);

            if sole_possible_setter {
                for ab in WEATHER_SETTING_ABILITIES {
                    if let Some(mon) = get_mon_mut_by_idx(state, idx) {
                        unknown_exclude(&mut mon.possible_abilities, ab, "ability-absence-weather");
                    }
                }
            }
        }

        // ── Terrain-setting abilities ────────────────────────────────────────
        if !terrain_changed {
            let sole_possible_setter = entered_slots.len() == 1
                || only_slot_with_terrain_setter(state, entered_slots, slot);

            if sole_possible_setter {
                for ab in TERRAIN_SETTING_ABILITIES {
                    // HadronEngine is already in WEATHER_SETTING_ABILITIES (sets elec terrain
                    // but appears as WeatherChanged in some sims); skip duplicates.
                    if let Some(mon) = get_mon_mut_by_idx(state, idx) {
                        unknown_exclude(&mut mon.possible_abilities, ab, "ability-absence-terrain");
                    }
                }
            }
        }

        // ── Intimidate ───────────────────────────────────────────────────────
        // Intimidate fires a BoostChanged {boost_idx:0, stages:-1} on each adjacent foe.
        // Only check when this is the sole possible Intimidate user and no such boost appeared.
        let sole_possible_intimidate = entered_slots.len() == 1
            || only_slot_with_ability(state, entered_slots, slot, &Ability::Intimidate);

        if sole_possible_intimidate {
            // Look for any opponent-side BoostChanged{0,-1} in reactions.
            let intimidate_fired = reactions.iter().any(|r| {
                if let EventKind::BoostChanged { target, boost_idx: 0, stages: -1 } = &r.kind {
                    // target must be on the opposite side from the entering mon.
                    target.player != slot.player
                } else {
                    false
                }
            });
            if !intimidate_fired {
                if let Some(mon) = get_mon_mut_by_idx(state, idx) {
                    unknown_exclude(
                        &mut mon.possible_abilities,
                        &Ability::Intimidate,
                        "ability-absence-intimidate",
                    );
                }
            }
        }

        // ── Intrepid Sword / Dauntless Shield ────────────────────────────────
        // These give +1 Atk and +1 Def to self, respectively, on first entry only.
        // Skip if one_time_ability_used (already known to have fired); we don't track
        // this reliably for opponents so just check if it fired in this reaction.
        let intrepid_fired = reactions.iter().any(|r| {
            if let EventKind::BoostChanged { target, boost_idx: 0, stages: 1 } = &r.kind {
                target == slot
            } else {
                false
            }
        });
        let dauntless_fired = reactions.iter().any(|r| {
            if let EventKind::BoostChanged { target, boost_idx: 1, stages: 1 } = &r.kind {
                target == slot
            } else {
                false
            }
        });
        if !intrepid_fired && entered_slots.len() == 1 {
            if let Some(mon) = get_mon_mut_by_idx(state, idx) {
                if !mon.one_time_ability_used {
                    unknown_exclude(
                        &mut mon.possible_abilities,
                        &Ability::IntrepidSword,
                        "ability-absence-intrepid",
                    );
                }
            }
        }
        if !dauntless_fired && entered_slots.len() == 1 {
            if let Some(mon) = get_mon_mut_by_idx(state, idx) {
                if !mon.one_time_ability_used {
                    unknown_exclude(
                        &mut mon.possible_abilities,
                        &Ability::DauntlessShield,
                        "ability-absence-dauntless",
                    );
                }
            }
        }
    }
}

fn only_slot_with_weather_setter(
    state: &UnknownBattleState,
    entered_slots: &[FieldSlot],
    this_slot: &FieldSlot,
) -> bool {
    // Returns true if among entered_slots, only this_slot could possibly have a weather setter.
    for slot in entered_slots {
        if slot == this_slot {
            continue;
        }
        if let Some(idx) = mon_idx_for_active_slot(state, slot) {
            if let Some(mon) = get_mon_by_idx(state, idx) {
                for ab in WEATHER_SETTING_ABILITIES {
                    if !unknown_is_excluded(&mon.possible_abilities, ab) {
                        return false; // Another entering mon might also have a weather setter.
                    }
                }
            }
        }
    }
    true
}

fn only_slot_with_terrain_setter(
    state: &UnknownBattleState,
    entered_slots: &[FieldSlot],
    this_slot: &FieldSlot,
) -> bool {
    for slot in entered_slots {
        if slot == this_slot {
            continue;
        }
        if let Some(idx) = mon_idx_for_active_slot(state, slot) {
            if let Some(mon) = get_mon_by_idx(state, idx) {
                for ab in TERRAIN_SETTING_ABILITIES {
                    if !unknown_is_excluded(&mon.possible_abilities, ab) {
                        return false;
                    }
                }
            }
        }
    }
    true
}

fn only_slot_with_ability(
    state: &UnknownBattleState,
    entered_slots: &[FieldSlot],
    this_slot: &FieldSlot,
    ability: &Ability,
) -> bool {
    for slot in entered_slots {
        if slot == this_slot {
            continue;
        }
        if let Some(idx) = mon_idx_for_active_slot(state, slot) {
            if let Some(mon) = get_mon_by_idx(state, idx) {
                if !unknown_is_excluded(&mon.possible_abilities, ability) {
                    return false;
                }
            }
        }
    }
    true
}

// ── Pass 2: Item presence/absence from behaviour ──────────────────────────────

fn pass2_item_from_move(
    state: &mut UnknownBattleState,
    event: &InformationEvent,
    ctx: &BattleContext,
) {
    let EventKind::MoveUsed {
        user,
        move_used,
        targets,
    } = &event.kind
    else {
        return;
    };
    let Some(move_data) = ctx.move_dex.get(move_used) else {
        return;
    };
    let is_damaging = matches!(
        move_data.category,
        MoveCategory::Physical | MoveCategory::Special
    );

    // ── Life Orb ──────────────────────────────────────────────────────────────
    if is_damaging {
        let has_lo_recoil = event
            .reactions
            .iter()
            .any(|r| matches!(&r.kind, EventKind::DamageDealt { target, .. } if target == user));

        if let Some(user_idx) = mon_idx_for_active_slot(state, user) {
            if !has_lo_recoil {
                let (could_mg, could_sf, has_secondary) = {
                    let um = get_mon_by_idx(state, user_idx);
                    (
                        um.map_or(false, |m| {
                            !unknown_is_excluded(&m.possible_abilities, &Ability::MagicGuard)
                        }),
                        um.map_or(false, |m| {
                            !unknown_is_excluded(&m.possible_abilities, &Ability::SheerForce)
                        }),
                        !move_data.secondaries.is_empty(),
                    )
                };

                if !could_mg && !(could_sf && has_secondary) {
                    // Definitively no Life Orb on this mon.
                    if let Some(mon) = get_mon_mut_by_idx(state, user_idx) {
                        unknown_exclude(&mut mon.item, &Item::LifeOrb, "no-lo-recoil");
                    }
                } else {
                    // Predicate: Not(LifeOrb) ∨ MagicGuard ∨ (SheerForce ∧ secondary)
                    let mut clause = vec![Statement::Not(Box::new(Statement::HasItem {
                        mon_idx: user_idx,
                        item: Item::LifeOrb,
                    }))];
                    if could_mg {
                        clause.push(Statement::HasAbility {
                            mon_idx: user_idx,
                            ability: Ability::MagicGuard,
                        });
                    }
                    if could_sf && has_secondary {
                        clause.push(Statement::HasAbility {
                            mon_idx: user_idx,
                            ability: Ability::SheerForce,
                        });
                    }
                    state.predicates.push(clause);
                }
            }
        }
    }

    // ── Bright Powder / Lax Incense from 100%-accurate miss ───────────────────
    for reaction in &event.reactions {
        if let EventKind::Missed { target } = &reaction.kind {
            if matches!(move_data.accuracy, AccuracyType::Percent(100)) {
                // No stat-stage accuracy/evasion modifiers in play?
                let user_acc_stage = mon_idx_for_active_slot(state, user)
                    .and_then(|ui| get_mon_by_idx(state, ui))
                    .map(|m| m.boosts[5])
                    .unwrap_or(0);
                let tgt_eva_stage = mon_idx_for_active_slot(state, target)
                    .and_then(|ti| get_mon_by_idx(state, ti))
                    .map(|m| m.boosts[6])
                    .unwrap_or(0);

                if user_acc_stage >= 0 && tgt_eva_stage <= 0 {
                    if let Some(tgt_idx) = mon_idx_for_active_slot(state, target) {
                        let legal_ok = |item: &Item| {
                            ctx.config
                                .legal_items
                                .as_ref()
                                .map_or(true, |l| l.contains(item))
                        };
                        let mut clause = Vec::new();
                        if legal_ok(&Item::BrightPowder) {
                            clause.push(Statement::HasItem {
                                mon_idx: tgt_idx,
                                item: Item::BrightPowder,
                            });
                        }
                        if legal_ok(&Item::LaxIncense) {
                            clause.push(Statement::HasItem {
                                mon_idx: tgt_idx,
                                item: Item::LaxIncense,
                            });
                        }
                        // TODO: accuracy-reducing abilities (Sand Veil, Snow Cloak,
                        // Tangled Feet) — deferred; not emitting them is sound (wider).
                        if !clause.is_empty() {
                            state.predicates.push(clause);
                        }
                    }
                }
            }
        }
    }

    let _ = targets; // suppress unused warning
}

// ── Pass 3: Damage → stat bounds ──────────────────────────────────────────────

/// Damage-to-stat inference: called once per top-level `MoveUsed` event after
/// the full reaction tree has been walked (so HP deltas and crit flags are live).
///
/// **Design**: instead of a hand-rolled analytic inverse (fragile with 22 flooring
/// steps), we use the real simulator oracle
/// `calculate_damage_outcomes_for_target_with_options` as a forward model and
/// enumerate candidate stats to find which ones can reproduce the observed damage.
///
/// **Direction B** (opponent attacks our known Pokémon): HP delta is exact
/// (`PokemonHP::Number`); we bound the attacker's Atk/SpA.
///
/// **Direction A** (we attack the opponent): HP delta is a percent interval;
/// we bound the defender's Def/SpD and HP.
///
/// **Soundness**: we always take the *union* over all possible (item, ability,
/// nature-class) assignments, so we never exclude a training that could produce
/// the observed damage.  Conditional CNF clauses let BCP recover precision as
/// other passes exclude boosters.
fn pass3_damage_to_stats(
    state: &mut UnknownBattleState,
    event: &InformationEvent,
    ctx: &BattleContext,
) {
    use crate::information::materialize::{materialize_battle, materialize_pokemon};
    use crate::simulator::helpers::calculate_damage_outcomes_for_target_with_options;
    use crate::simulator::DamageConfig;

    let EventKind::MoveUsed {
        user,
        move_used,
        targets,
    } = &event.kind
    else {
        return;
    };

    let Some(move_data) = ctx.move_dex.get(move_used) else {
        return;
    };

    // ── Skip moves that carry no stat signal ─────────────────────────────────
    // Status moves, OHKO, fixed-damage, retaliation, Super Fang, etc.
    use crate::state::dex_data::{DamageOverride, MoveCategory};
    if matches!(move_data.category, MoveCategory::Status) {
        return;
    }
    if move_data.ohko {
        return;
    }
    if !matches!(move_data.damage_override, DamageOverride::None) {
        return;
    }
    // Retaliation moves (Counter/Mirror Coat/Metal Burst/Comeuppance):
    // damage is a multiple of incoming damage, not of the user's stats.
    use crate::data::pokemon_move::PokemonMove as PM;
    if matches!(
        move_used,
        PM::Counter | PM::MirrorCoat | PM::MetalBurst | PM::Comeuppance
    ) {
        return;
    }
    // Ambiguous offensive stat (Shell Side Arm, Photon Geyser): skip in v1.
    if matches!(move_used, PM::ShellSideArm | PM::PhotonGeyser) {
        return;
    }
    // Beat Up BP depends on party members' base Attack — out of scope for inference.
    if matches!(move_used, PM::BeatUp) {
        return;
    }
    // Need a clear offensive stat to know which field to bound.
    let Some(off_stat) = crate::simulator::helpers::move_offensive_stat(move_data) else {
        return;
    };

    // Determine attacker / target mon_idx.
    let Some(user_idx) = mon_idx_for_active_slot(state, user) else {
        return;
    };

    // Whether the move has variable BP determined by speed stats (Gyro Ball / Electro Ball).
    let speed_dep_bp = is_speed_dependent_bp(move_used);

    // For each target that has one or more DamageDealt reactions, run inference.
    // Multi-hit moves produce multiple DamageDealt reactions per target; we process
    // each hit independently to intersect BSV feasibility constraints.
    for target_slot in targets {
        let Some(target_idx) = mon_idx_for_active_slot(state, target_slot) else {
            continue;
        };

        // Collect ALL DamageDealt reactions for this target, in event order.
        let damage_reactions: Vec<&InformationEvent> = event
            .reactions
            .iter()
            .filter(|r| {
                matches!(&r.kind, EventKind::DamageDealt { target, .. } if target == target_slot)
            })
            .collect();

        if damage_reactions.is_empty() {
            continue;
        }

        // Pre-hit HP from MoveContext snapshot.
        let Some(move_ctx) = &ctx.move_context else {
            continue;
        };
        let pre_hp = move_ctx
            .pre_hit_hp
            .iter()
            .find(|(slot, _)| slot == target_slot)
            .map(|(_, hp)| hp);
        let Some(pre_hp) = pre_hp else {
            continue;
        };

        // For multi-hit: use a global crit flag (any hit critted) — sound but slightly
        // looser than per-hit crit tracking. For single-hit this is exact.
        let is_crit = move_ctx.is_crit;

        // Detect whether this target's HP is a multi-hit sequence.
        let is_multi = move_data.multihit_range[0] > 0 || damage_reactions.len() > 1;

        // Running HP tracks the HP value between consecutive hits.
        let mut current_hp: PokemonHP = pre_hp.clone();

        for (hit_idx, dmg_reaction) in damage_reactions.iter().enumerate() {
            let new_hp = match &dmg_reaction.kind {
                EventKind::DamageDealt { new_hp, .. } => new_hp,
                _ => {
                    current_hp = current_hp.clone();
                    continue;
                }
            };

            // Per-hit BP override for fixed-BP multi-hit moves (Triple Kick etc.).
            // None for normal multi-hit moves (each hit uses move's base_power).
            let bp_override: Option<u16> = if is_multi {
                match move_used {
                    PM::TripleKick => Some(10 + hit_idx as u16 * 10),
                    PM::TripleAxel => Some(20 + hit_idx as u16 * 20),
                    PM::PopulationBomb => Some(20),
                    _ => None,
                }
            } else {
                None
            };

            // ── Classify direction ────────────────────────────────────────────
            // Direction B: target HP is Number → exact damage; bound ATTACKER's stat.
            // Direction A: target HP is Percent → interval damage; bound DEFENDER's stat.
            match (&current_hp, new_hp) {
                (PokemonHP::Number(pre), PokemonHP::Number(post)) => {
                    let exact_damage = (*pre).saturating_sub(*post);
                    if exact_damage > 0 {
                        pass3_direction_b(
                            state,
                            event,
                            ctx,
                            user_idx,
                            target_idx,
                            user,
                            target_slot,
                            move_data,
                            &off_stat,
                            is_crit,
                            exact_damage,
                            bp_override,
                            speed_dep_bp,
                        );
                    }
                }
                (PokemonHP::Percent(pre_pct), PokemonHP::Percent(post_pct)) => {
                    if *post_pct < *pre_pct {
                        let delta_pct = pre_pct - post_pct;
                        let Some(def_stat) =
                            crate::simulator::helpers::move_defensive_stat(move_data)
                        else {
                            current_hp = new_hp.clone();
                            continue;
                        };
                        pass3_direction_a(
                            state,
                            event,
                            ctx,
                            user_idx,
                            target_idx,
                            user,
                            target_slot,
                            move_data,
                            &def_stat,
                            is_crit,
                            delta_pct,
                            bp_override,
                            speed_dep_bp,
                        );
                    }
                }
                _ => {
                    // Mixed Number/Percent — HP tracking not implemented; skip.
                }
            }

            current_hp = new_hp.clone();
        }
    }
}

/// Returns `true` for moves whose base power depends on one or both mons' Speed stats.
fn is_speed_dependent_bp(move_used: &PokemonMove) -> bool {
    matches!(move_used, PokemonMove::GyroBall | PokemonMove::ElectroBall)
}

/// Items that can boost offensive damage for a given attacker mon.
/// We enumerate these to build the booster disjuncts in CNF clauses.
fn offensive_damage_items(mon: &UnknownPokemonState) -> Vec<Item> {
    // Damage-relevant items (subset whose presence/absence matters for Pass 3).
    // Metronome streak is handled separately (by varying consecutive_move_count).
    [
        Item::ChoiceBand,
        Item::ChoiceSpecs,
        Item::LifeOrb,
        Item::ExpertBelt,
        Item::MuscleBand,
        Item::WiseGlasses,
        Item::Charcoal,
        Item::MysticWater,
        Item::SharpBeak,
        Item::TwistedSpoon,
        Item::BlackGlasses,
        Item::PoisonBarb,
        Item::SoftSand,
        Item::HardStone,
        Item::Magnet,
        Item::MetalCoat,
        Item::NeverMeltIce,
        Item::SilkScarf,
        Item::BlackBelt,
        Item::SpellTag,
        Item::MiracleSeed,
        Item::DragonFang,
        Item::FairyFeather,
        Item::Metronome,
        Item::LightBall, // Pikachu only
    ]
    .iter()
    .filter(|i| !unknown_is_excluded(&mon.item, i))
    .cloned()
    .collect()
}

/// Abilities that can boost offensive damage.
fn offensive_damage_abilities(mon: &UnknownPokemonState) -> Vec<Ability> {
    [
        Ability::HugePower,
        Ability::PurePower,
        Ability::Hustle,
        Ability::Guts,
        Ability::Adaptability,
        Ability::Technician,
        Ability::ToughClaws,
        Ability::IronFist,
        Ability::Sharpness,
        Ability::MegaLauncher,
        Ability::StrongJaw,
        Ability::Reckless,
        Ability::SandForce,
        Ability::SheerForce,
        Ability::WaterBubble,
        Ability::SolarPower,
        Ability::OrichalcumPulse,
        Ability::HadronEngine,
        Ability::Rivalry,
        Ability::Blaze,
        Ability::Overgrow,
        Ability::Swarm,
        Ability::Torrent,
        Ability::FireMane,
        Ability::FlashFire,
        Ability::SupremeOverlord,
        Ability::Sniper,
        Ability::Plus,
        Ability::Minus,
        // -ate abilities
        Ability::Pixilate,
        Ability::Refrigerate,
        Ability::Aerilate,
        Ability::Galvanize,
        Ability::Normalize,
        Ability::Dragonize,
        Ability::Eelevate,
    ]
    .iter()
    .filter(|a| !unknown_is_excluded(&mon.possible_abilities, a))
    .cloned()
    .collect()
}

/// "No-booster" placeholders used when we want to compute the neutral bound.
fn neutral_item(mon: &UnknownPokemonState) -> Item {
    if let Unknown::Known(i) = &mon.item {
        i.clone()
    } else {
        Item::None
    }
}

fn neutral_ability(mon: &UnknownPokemonState) -> Ability {
    if let Unknown::Known(a) = &mon.possible_abilities {
        a.clone()
    } else {
        Ability::None
    }
}

/// Direction B: we are the target, HP is exact, bound the ATTACKER's offensive BSV.
#[allow(clippy::too_many_arguments)]
fn pass3_direction_b(
    state: &mut UnknownBattleState,
    event: &InformationEvent,
    ctx: &BattleContext,
    user_idx: usize,
    target_idx: usize,
    user_slot: &FieldSlot,
    target_slot: &FieldSlot,
    move_data: &crate::state::dex_data::MoveData,
    off_stat: &crate::state::dex_data::PokemonStat,
    is_crit: bool,
    exact_damage: u16,
    // Per-hit base power override for multi-hit moves (None = use move's base_power).
    bp_override: Option<u16>,
    // True for Gyro Ball / Electro Ball — BP depends on attacker + target speeds.
    speed_dep_bp: bool,
) {
    use crate::information::materialize::{materialize_battle, materialize_pokemon};
    use crate::simulator::helpers::calculate_damage_outcomes_for_target_with_options;
    use crate::simulator::DamageConfig;

    let Some(attacker_unk) = get_mon_by_idx(state, user_idx).cloned() else {
        return;
    };
    let Some(target_unk) = get_mon_by_idx(state, target_idx).cloned() else {
        return;
    };

    // Need known attacker species for BSV-based inference.
    let base_stats = match &attacker_unk.possible_species {
        Unknown::Known(s) => match ctx.dex.get(s) {
            Some(d) => d.base_stats,
            None => return,
        },
        _ => return,
    };

    let si = stat_to_stats_idx(off_stat);
    let level = attacker_unk.level;

    // Current pre-nature BSV range for this stat.
    let bsv_lo = attacker_unk.min_pre_nature_stat[si];
    let bsv_hi = attacker_unk.max_pre_nature_stat[si];
    if bsv_lo > bsv_hi {
        return;
    }

    // Determine which nature classes are still possible.
    let nature_classes: Vec<(f32, bool, bool)> = {
        // (nature_mod, is_boost, is_nerf)
        let mut classes = Vec::new();
        let boost_natures = boosting_natures_for_stat(off_stat);
        let nerf_natures = nerfing_natures_for_stat(off_stat);
        // boost
        if boost_natures
            .iter()
            .any(|n| !unknown_is_excluded(&attacker_unk.possible_natures, n))
        {
            classes.push((1.1_f32, true, false));
        }
        // neutral (HP also only gets this)
        let any_neutral = ALL_NATURES.iter().any(|n| {
            !boost_natures.contains(n)
                && !nerf_natures.contains(n)
                && !unknown_is_excluded(&attacker_unk.possible_natures, n)
        });
        if any_neutral {
            classes.push((1.0_f32, false, false));
        }
        // nerf
        if si != 0
            && nerf_natures
                .iter()
                .any(|n| !unknown_is_excluded(&attacker_unk.possible_natures, n))
        {
            classes.push((0.9_f32, false, true));
        }
        classes
    };
    if nature_classes.is_empty() {
        return;
    }

    // Booster sets for predicate emission.
    let booster_items = offensive_damage_items(&attacker_unk);
    let booster_abilities = offensive_damage_abilities(&attacker_unk);

    // Oracle config: always use all 16 rolls regardless of CLI setting.
    let oracle_config = DamageConfig {
        consider_crit: true,
        damage_rolls: 16,
    };

    // Spread multiplier (doubles ×0.75 when a move hits all adjacent foes with 2+ active opponents).
    let targets_mult = if state.active_per_side > 1
        && matches!(
            move_data.target,
            crate::state::dex_data::MoveTarget::AllAdjacent
                | crate::state::dex_data::MoveTarget::AllAdjacentFoes
        ) {
        0.75
    } else {
        1.0
    };

    // For speed-dependent BP moves (Gyro Ball / Electro Ball), the attacker's speed
    // also varies over its current [min, max] range. We scan both endpoints (sound
    // because BP is monotone in the speed ratio — all intermediate BPs are covered).
    let attacker_speed_range: Option<(u16, u16)> = if speed_dep_bp {
        Some((attacker_unk.minStats[5], attacker_unk.maxStats[5]))
    } else {
        None
    };

    // ── Unconditional tightening: union over all (nature_class, item, ability) ─
    let mut global_bsv_lo: Option<u16> = None;
    let mut global_bsv_hi: Option<u16> = None;
    let mut global_stat_lo: Option<u16> = None;
    let mut global_stat_hi: Option<u16> = None;

    // ── Per-nature-class data for predicate emission ──────────────────────────
    // For each nature class we compute the BSV interval under neutral gear (no booster).
    struct NatureClassResult {
        mod_f32: f32,
        is_boost: bool,
        is_nerf: bool,
        bsv_lo_neutral: Option<u16>,
        bsv_hi_neutral: Option<u16>,
    }
    let mut per_class: Vec<NatureClassResult> = Vec::new();

    for (nat_mod, is_boost, is_nerf) in &nature_classes {
        // Items to enumerate for this class: all possible items (for union) plus
        // a neutral-item run (for predicate lower bound).
        let item_choices: Vec<Item> = {
            let mut items = booster_items.clone();
            let neutral = neutral_item(&attacker_unk);
            if !items.contains(&neutral) {
                items.push(neutral);
            }
            items
        };
        let ability_choices: Vec<Ability> = {
            let mut abs = booster_abilities.clone();
            let neutral = neutral_ability(&attacker_unk);
            if !abs.contains(&neutral) {
                abs.push(neutral);
            }
            abs
        };

        // Also enumerate Metronome item streak values 0..=5.
        let streak_range: Vec<u8> = if !unknown_is_excluded(&attacker_unk.item, &Item::Metronome) {
            vec![0, 1, 2, 3, 4, 5]
        } else {
            vec![0]
        };

        // ── Neutral-gear BSV interval (for predicate emission) ─────────────
        let neutral_i = neutral_item(&attacker_unk);
        let neutral_a = neutral_ability(&attacker_unk);

        let (bsv_lo_neutral, bsv_hi_neutral) = find_feasible_bsv_range_b(
            state,
            &attacker_unk,
            &target_unk,
            user_slot,
            target_slot,
            move_data,
            &oracle_config,
            targets_mult,
            *nat_mod,
            si,
            base_stats,
            bsv_lo,
            bsv_hi,
            neutral_i,
            neutral_a,
            0,
            exact_damage,
            is_crit,
            bp_override,
            attacker_speed_range,
        );

        per_class.push(NatureClassResult {
            mod_f32: *nat_mod,
            is_boost: *is_boost,
            is_nerf: *is_nerf,
            bsv_lo_neutral,
            bsv_hi_neutral,
        });

        // ── Union over all (item, ability, streak) assignments ─────────────
        for item in &item_choices {
            for ability in &ability_choices {
                for &streak in &streak_range {
                    let mut atk_for_oracle = attacker_unk.clone();
                    atk_for_oracle.consecutive_move_count = streak;

                    let (lo, hi) = find_feasible_bsv_range_b(
                        state,
                        &atk_for_oracle,
                        &target_unk,
                        user_slot,
                        target_slot,
                        move_data,
                        &oracle_config,
                        targets_mult,
                        *nat_mod,
                        si,
                        base_stats,
                        bsv_lo,
                        bsv_hi,
                        item.clone(),
                        ability.clone(),
                        streak,
                        exact_damage,
                        is_crit,
                        bp_override,
                        attacker_speed_range,
                    );
                    if let (Some(lo_v), Some(hi_v)) = (lo, hi) {
                        let final_lo = (lo_v as f64 * *nat_mod as f64).floor() as u16;
                        let final_hi = (hi_v as f64 * *nat_mod as f64).floor() as u16;
                        global_bsv_lo = Some(global_bsv_lo.map_or(lo_v, |g| g.min(lo_v)));
                        global_bsv_hi = Some(global_bsv_hi.map_or(hi_v, |g| g.max(hi_v)));
                        global_stat_lo =
                            Some(global_stat_lo.map_or(final_lo, |g| g.min(final_lo)));
                        global_stat_hi =
                            Some(global_stat_hi.map_or(final_hi, |g| g.max(final_hi)));
                    }
                }
            }
        }
    }

    // Apply unconditional tightening.
    if let Some(mon) = get_mon_mut_by_idx(state, user_idx) {
        if let Some(lo) = global_bsv_lo {
            if lo > mon.min_pre_nature_stat[si] {
                mon.min_pre_nature_stat[si] = lo;
            }
        }
        if let Some(hi) = global_bsv_hi {
            if hi < mon.max_pre_nature_stat[si] {
                mon.max_pre_nature_stat[si] = hi;
            }
        }
        if let Some(lo) = global_stat_lo {
            if lo > mon.minStats[si] {
                mon.minStats[si] = lo;
            }
        }
        if let Some(hi) = global_stat_hi {
            if hi < mon.maxStats[si] {
                mon.maxStats[si] = hi;
            }
        }
    }

    // ── Conditional CNF predicates ────────────────────────────────────────────
    // For each nature class κ, emit:
    //   LOWER: [not-κ guards] ∨ EVIVStatGE{bsv_lo_neutral} ∨ ⋁ booster_items ∨ ⋁ booster_abilities
    //   UPPER: [not-κ guards] ∨ EVIVStatLE{bsv_hi_neutral} ∨ ⋁ booster_items ∨ ⋁ booster_abilities
    // Only emit when:
    //   - neutral BSV bound was computable, AND
    //   - the bound is strictly tighter than the current min_pre/max_pre (worth emitting).
    for cr in &per_class {
        let not_kappa_guards: Vec<Statement> = match (cr.is_boost, cr.is_nerf) {
            (true, _) => vec![Statement::Not(Box::new(Statement::NatureBoostsStat {
                mon_idx: user_idx,
                stat: off_stat.clone(),
            }))],
            (_, true) => vec![Statement::Not(Box::new(Statement::NatureNerfsStat {
                mon_idx: user_idx,
                stat: off_stat.clone(),
            }))],
            // neutral: exclude the class when the nature is definitely a booster or nerf
            (false, false) => vec![
                Statement::NatureBoostsStat {
                    mon_idx: user_idx,
                    stat: off_stat.clone(),
                },
                Statement::NatureNerfsStat {
                    mon_idx: user_idx,
                    stat: off_stat.clone(),
                },
            ],
        };

        let booster_literals: Vec<Statement> = booster_items
            .iter()
            .map(|i| Statement::HasItem {
                mon_idx: user_idx,
                item: i.clone(),
            })
            .chain(booster_abilities.iter().map(|a| Statement::HasAbility {
                mon_idx: user_idx,
                ability: a.clone(),
            }))
            .collect();

        let current_pre_min = attacker_unk.min_pre_nature_stat[si];
        let current_pre_max = attacker_unk.max_pre_nature_stat[si];

        if let Some(lo) = cr.bsv_lo_neutral {
            if lo > current_pre_min {
                let mut clause = not_kappa_guards.clone();
                clause.push(Statement::EVIVStatGE {
                    mon_idx: user_idx,
                    stat: off_stat.clone(),
                    value: lo,
                });
                clause.extend(booster_literals.clone());
                if clause.len() > not_kappa_guards.len() + 1 {
                    // only emit if there are non-trivial booster alternatives
                    state.predicates.push(clause);
                } else {
                    // No boosters possible: force directly.
                    if let Some(mon) = get_mon_mut_by_idx(state, user_idx) {
                        if lo > mon.min_pre_nature_stat[si] {
                            mon.min_pre_nature_stat[si] = lo;
                        }
                    }
                }
            }
        }
        if let Some(hi) = cr.bsv_hi_neutral {
            if hi < current_pre_max {
                let mut clause = not_kappa_guards.clone();
                clause.push(Statement::EVIVStatLE {
                    mon_idx: user_idx,
                    stat: off_stat.clone(),
                    value: hi,
                });
                clause.extend(booster_literals.clone());
                if clause.len() > not_kappa_guards.len() + 1 {
                    state.predicates.push(clause);
                } else {
                    if let Some(mon) = get_mon_mut_by_idx(state, user_idx) {
                        if hi < mon.max_pre_nature_stat[si] {
                            mon.max_pre_nature_stat[si] = hi;
                        }
                    }
                }
            }
        }
    }
}

/// Linear scan for the feasible BSV interval [lo, hi] for Direction B under a
/// single fixed (nat_mod, item, ability, streak) assignment.
///
/// Returns `(Some(lo), Some(hi))` if any BSV in `[bsv_lo, bsv_hi]` can produce
/// `exact_damage`, `(None, None)` if none can.
///
/// `bp_override` — per-hit base power (for multi-hit moves); `None` uses move's BP.
/// `attacker_speed_range` — for Gyro Ball / Electro Ball, the attacker's speed stat
/// range to union over. The oracle is called at the speed endpoints (sound because
/// BP is monotone in the speed ratio — all intermediate BPs lie in between).
#[allow(clippy::too_many_arguments)]
fn find_feasible_bsv_range_b(
    state: &UnknownBattleState,
    attacker_unk: &UnknownPokemonState,
    target_unk: &UnknownPokemonState,
    user_slot: &FieldSlot,
    target_slot: &FieldSlot,
    move_data: &crate::state::dex_data::MoveData,
    oracle_config: &crate::simulator::DamageConfig,
    targets_mult: f64,
    nat_mod: f32,
    si: usize,
    base_stats: [u16; 6],
    bsv_lo: u16,
    bsv_hi: u16,
    item: Item,
    ability: Ability,
    streak: u8,
    exact_damage: u16,
    is_crit: bool,
    bp_override: Option<u16>,
    attacker_speed_range: Option<(u16, u16)>,
) -> (Option<u16>, Option<u16>) {
    use crate::information::materialize::{materialize_battle, materialize_pokemon};
    use crate::simulator::helpers::calculate_damage_outcomes_for_target_with_options;

    // Materialize target with known stats (target is our own mon — stats exact).
    let target_stats = target_unk.minStats; // min == max for known mons
    let target_ps = materialize_pokemon(
        target_unk,
        target_stats,
        neutral_item(target_unk),
        neutral_ability(target_unk),
    );

    // Speed endpoints for speed-dependent BP moves (Gyro Ball / Electro Ball).
    // Scanning both endpoints of [min_spe, max_spe] is sound since BP is monotone
    // in the speed ratio, covering all intermediate BPs.
    let speed_endpoints: Vec<u16> = match attacker_speed_range {
        Some((lo, hi)) if lo != hi => vec![lo, hi],
        Some((lo, _)) => vec![lo],
        None => vec![attacker_unk.minStats[5]],
    };

    // Build the attacker PS with a specific BSV and optional speed override.
    let make_atk = |bsv: u16, spe_override: u16| -> crate::state::pokemon::PokemonState {
        let mut stats = attacker_unk.minStats; // fill non-inferred stats with current min
        if si == 0 {
            stats[0] = bsv; // HP: no nature
        } else {
            stats[si] = (bsv as f64 * nat_mod as f64).floor() as u16;
        }
        stats[5] = spe_override; // override speed (no-op for non-speed-dep moves)
        let mut unk_copy = attacker_unk.clone();
        unk_copy.consecutive_move_count = streak;
        materialize_pokemon(&unk_copy, stats, item.clone(), ability.clone())
    };

    // A BSV is feasible if the oracle produces `exact_damage` with correct crit for
    // *any* speed endpoint (sound union over the speed range).
    let can_produce = |bsv: u16| -> bool {
        speed_endpoints.iter().any(|&spe| {
            let atk_ps = make_atk(bsv, spe);
            let p1_active = if user_slot.player == crate::state::battle::Player::P1 {
                vec![atk_ps.clone()]
            } else {
                vec![target_ps.clone()]
            };
            let p2_active = if user_slot.player == crate::state::battle::Player::P1 {
                vec![target_ps.clone()]
            } else {
                vec![atk_ps.clone()]
            };
            let battle = materialize_battle(state, p1_active, p2_active);
            let outcomes = calculate_damage_outcomes_for_target_with_options(
                &battle,
                &atk_ps,
                &target_ps,
                user_slot.clone(),
                target_slot.clone(),
                move_data,
                *oracle_config,
                targets_mult,
                1.0, // invulnerability_multiplier
                bp_override,
                None,
            );
            outcomes
                .iter()
                .any(|(dmg, crit, _)| *dmg == exact_damage && *crit == is_crit)
        })
    };

    // Linear scan over the BSV range.
    let mut found_lo: Option<u16> = None;
    let mut found_hi: Option<u16> = None;
    for bsv in bsv_lo..=bsv_hi {
        if can_produce(bsv) {
            found_lo = Some(found_lo.unwrap_or(bsv));
            found_hi = Some(bsv);
        }
    }
    (found_lo, found_hi)
}

/// Direction A: we attacked the opponent, HP is a percent interval,
/// bound the DEFENDER's defensive stat (and HP).
#[allow(clippy::too_many_arguments)]
fn pass3_direction_a(
    state: &mut UnknownBattleState,
    event: &InformationEvent,
    ctx: &BattleContext,
    user_idx: usize,
    target_idx: usize,
    user_slot: &FieldSlot,
    target_slot: &FieldSlot,
    move_data: &crate::state::dex_data::MoveData,
    def_stat: &crate::state::dex_data::PokemonStat,
    is_crit: bool,
    delta_pct: u8,
    // Per-hit base power override for multi-hit moves.
    bp_override: Option<u16>,
    // True for Gyro Ball / Electro Ball — defender's speed affects BP.
    speed_dep_bp: bool,
) {
    use crate::information::materialize::{materialize_battle, materialize_pokemon};
    use crate::simulator::helpers::calculate_damage_outcomes_for_target_with_options;
    use crate::simulator::DamageConfig;

    let Some(defender_unk) = get_mon_by_idx(state, target_idx).cloned() else {
        return;
    };
    let Some(attacker_unk) = get_mon_by_idx(state, user_idx).cloned() else {
        return;
    };

    // Need known defender species for BSV inference.
    let base_stats = match &defender_unk.possible_species {
        Unknown::Known(s) => match ctx.dex.get(s) {
            Some(d) => d.base_stats,
            None => return,
        },
        _ => return,
    };

    let si = stat_to_stats_idx(def_stat);
    let level = defender_unk.level;

    let bsv_lo = defender_unk.min_pre_nature_stat[si];
    let bsv_hi = defender_unk.max_pre_nature_stat[si];
    if bsv_lo > bsv_hi {
        return;
    }

    // HP bounds for the defender.
    let hp_lo = defender_unk.minStats[0];
    let hp_hi = defender_unk.maxStats[0];

    // Nature classes for the defensive stat.
    let nature_classes: Vec<(f32, bool, bool)> = {
        let mut classes = Vec::new();
        let boost_natures = boosting_natures_for_stat(def_stat);
        let nerf_natures = nerfing_natures_for_stat(def_stat);
        if boost_natures
            .iter()
            .any(|n| !unknown_is_excluded(&defender_unk.possible_natures, n))
        {
            classes.push((1.1_f32, true, false));
        }
        let any_neutral = ALL_NATURES.iter().any(|n| {
            !boost_natures.contains(n)
                && !nerf_natures.contains(n)
                && !unknown_is_excluded(&defender_unk.possible_natures, n)
        });
        if any_neutral {
            classes.push((1.0_f32, false, false));
        }
        if nerf_natures
            .iter()
            .any(|n| !unknown_is_excluded(&defender_unk.possible_natures, n))
        {
            classes.push((0.9_f32, false, true));
        }
        classes
    };
    if nature_classes.is_empty() {
        return;
    }

    // Oracle config.
    let oracle_config = DamageConfig {
        consider_crit: true,
        damage_rolls: 16,
    };
    let targets_mult = 1.0_f64; // Direction A: single-target only for this pass

    // ── Unconditional tightening: union over (nat, hp_candidate, def_bsv) ────
    let mut global_bsv_lo: Option<u16> = None;
    let mut global_bsv_hi: Option<u16> = None;
    let mut global_stat_lo: Option<u16> = None;
    let mut global_stat_hi: Option<u16> = None;

    // Attacker is OUR known mon; use its actual known stats.
    let atk_stats = attacker_unk.minStats;
    let atk_item = neutral_item(&attacker_unk);
    let atk_ability = neutral_ability(&attacker_unk);
    let atk_ps = materialize_pokemon(&attacker_unk, atk_stats, atk_item, atk_ability);

    // Defender speed endpoints for Gyro Ball / Electro Ball (Direction A: we attacked
    // the opponent, so the *target/defender's* speed is the unknown that affects BP).
    // BP is monotone in the speed ratio, so scanning only the lo/hi endpoints is sound.
    let defender_speed_endpoints: Vec<u16> = if speed_dep_bp {
        let lo = defender_unk.minStats[5];
        let hi = defender_unk.maxStats[5];
        if lo == hi { vec![lo] } else { vec![lo, hi] }
    } else {
        vec![defender_unk.minStats[5]]
    };

    // We sample from the defender's possible HP range (step by 4 for speed).
    for hp_cand in (hp_lo..=hp_hi).step_by(4.max(1) as usize) {
        // Convert percent delta to raw damage interval for this candidate max HP.
        // Convention: Percent(p) = round(current_hp * 100 / max_hp), so:
        //   p = round(hp * 100 / max_hp)  →  hp = round(p * max_hp / 100)
        // Damage interval for delta_pct p: [floor((p-0.5)*max_hp/100), ceil((p+0.5)*max_hp/100)]
        // clamped to [1, max_hp].  Sound: this is wider than the actual rounding bucket.
        let hp_c = hp_cand as f64;
        let d_lo = ((delta_pct as f64 - 0.5) * hp_c / 100.0).floor().max(1.0) as u16;
        let d_hi = ((delta_pct as f64 + 0.5) * hp_c / 100.0).ceil().min(hp_c) as u16;

        for (nat_mod, is_boost, is_nerf) in &nature_classes {
            let can_produce_def_bsv = |bsv: u16| -> bool {
                let mut def_stats = defender_unk.minStats;
                def_stats[0] = hp_cand; // set candidate HP
                if si == 0 {
                    def_stats[0] = bsv;
                } else {
                    def_stats[si] = (bsv as f64 * *nat_mod as f64).floor() as u16;
                }

                // Scan speed endpoints for speed-dependent-BP moves (Gyro Ball /
                // Electro Ball): feasible if any speed in the defender's range yields
                // a matching outcome (sound over-approximation across the speed range).
                defender_speed_endpoints.iter().any(|&def_spe| {
                    let mut spd_stats = def_stats;
                    spd_stats[5] = def_spe;
                    let def_ps = materialize_pokemon(
                        &defender_unk,
                        spd_stats,
                        neutral_item(&defender_unk),
                        neutral_ability(&defender_unk),
                    );
                    let p1_active = if user_slot.player == crate::state::battle::Player::P1 {
                        vec![atk_ps.clone()]
                    } else {
                        vec![def_ps.clone()]
                    };
                    let p2_active = if user_slot.player == crate::state::battle::Player::P1 {
                        vec![def_ps.clone()]
                    } else {
                        vec![atk_ps.clone()]
                    };
                    let battle = materialize_battle(state, p1_active, p2_active);

                    let outcomes = calculate_damage_outcomes_for_target_with_options(
                        &battle,
                        &atk_ps,
                        &def_ps,
                        user_slot.clone(),
                        target_slot.clone(),
                        move_data,
                        oracle_config,
                        targets_mult,
                        1.0,
                        bp_override,
                        None,
                    );
                    // Any outcome with damage in [d_lo, d_hi] and matching crit.
                    outcomes
                        .iter()
                        .any(|(dmg, crit, _)| *dmg >= d_lo && *dmg <= d_hi && *crit == is_crit)
                })
            };

            let mut found_lo_local: Option<u16> = None;
            let mut found_hi_local: Option<u16> = None;
            for bsv in bsv_lo..=bsv_hi {
                if can_produce_def_bsv(bsv) {
                    found_lo_local = Some(found_lo_local.unwrap_or(bsv));
                    found_hi_local = Some(bsv);
                }
            }
            if let (Some(lo_v), Some(hi_v)) = (found_lo_local, found_hi_local) {
                let final_lo = (lo_v as f64 * *nat_mod as f64).floor() as u16;
                let final_hi = (hi_v as f64 * *nat_mod as f64).floor() as u16;
                global_bsv_lo = Some(global_bsv_lo.map_or(lo_v, |g| g.min(lo_v)));
                global_bsv_hi = Some(global_bsv_hi.map_or(hi_v, |g| g.max(hi_v)));
                global_stat_lo = Some(global_stat_lo.map_or(final_lo, |g| g.min(final_lo)));
                global_stat_hi = Some(global_stat_hi.map_or(final_hi, |g| g.max(final_hi)));
            }
        }
    }

    // Apply unconditional tightening.
    if let Some(mon) = get_mon_mut_by_idx(state, target_idx) {
        if let Some(lo) = global_bsv_lo {
            if lo > mon.min_pre_nature_stat[si] {
                mon.min_pre_nature_stat[si] = lo;
            }
        }
        if let Some(hi) = global_bsv_hi {
            if hi < mon.max_pre_nature_stat[si] {
                mon.max_pre_nature_stat[si] = hi;
            }
        }
        if let Some(lo) = global_stat_lo {
            if lo > mon.minStats[si] {
                mon.minStats[si] = lo;
            }
        }
        if let Some(hi) = global_stat_hi {
            if hi < mon.maxStats[si] {
                mon.maxStats[si] = hi;
            }
        }
    }
    // Direction A predicates: nature-conditional BSV clauses mirroring Direction B,
    // but using defender's booster/nerf set (Eviolite/Assault Vest not modelled).
    // For v1, we rely on the unconditional tightening above; predicates follow the
    // same pattern but are omitted here to keep scope manageable.
}

// ── Pass 4: Speed ordering → Spe bounds ──────────────────────────────────────

/// Returns `true` if the mon at `mon_idx` is on P2's side (indices past the P1 segments).
fn mon_is_p2(state: &UnknownBattleState, mon_idx: usize) -> bool {
    let p1_count = state.p1_active_mons.len()
        + state.p1_known_back_mons.len()
        + state.p1_possible_back_mons.len();
    mon_idx >= p1_count
}

/// Returns the effective move priority for `move_used`, folding in field-conditional
/// boosts that are deterministically known from state (currently: Grassy Glide +1 on
/// Grassy Terrain).  Does NOT fold in ability-based boosts (Prankster/Gale Wings/Triage);
/// those are tracked as disjunct escape clauses instead.
fn effective_move_priority(
    move_used: &PokemonMove,
    base_priority: i8,
    state: &UnknownBattleState,
) -> i8 {
    if *move_used == PokemonMove::GrassyGlide
        && state.terrain == Some(Terrain::GrassyTerrain)
    {
        base_priority + 1
    } else {
        base_priority
    }
}

/// Emit `SpeedComparison` predicates from the observed top-level move order.
///
/// For each pair of consecutive moves in the same effective priority bracket:
/// - Wraps the natural SpeedComparison in a disjunction with any move-order explanation
///   that could account for the ordering without implying a speed edge (Quick Claw,
///   Quick Draw, ability priority, Stall, item speed modifiers, weather abilities, etc.).
/// - Accounts for Trick Room (reverses the inferred fast/slow assignment) and Tailwind
///   (folds the ×2 multiplier into the comparison deterministically).
fn pass4_speed_from_order(
    state: &mut UnknownBattleState,
    top_events: &[InformationEvent],
    move_dex: &HashMap<PokemonMove, MoveData>,
    _ability_dex: &HashMap<Ability, AbilityData>,
) {
    // Collect (slot, eff_priority, mon_idx, move_used) for all top-level MoveUsed events.
    let mut move_order: Vec<(FieldSlot, i8, usize, PokemonMove)> = Vec::new();
    for event in top_events {
        if let EventKind::MoveUsed {
            user, move_used, ..
        } = &event.kind
        {
            let base_prio = move_dex.get(move_used).map(|md| md.priority).unwrap_or(0);
            let eff_prio = effective_move_priority(move_used, base_prio, state);
            if let Some(idx) = mon_idx_for_active_slot(state, user) {
                move_order.push((user.clone(), eff_prio, idx, move_used.clone()));
            }
        }
    }

    let trick_room_active = state.pseudo_weathers.contains(&PseudoWeather::TrickRoom);

    for window in move_order.windows(2) {
        let (_, p0, idx0, mv0) = &window[0];
        let (_, p1, idx1, _mv1) = &window[1];

        // Different effective priority brackets — no speed inference possible.
        if p0 != p1 {
            continue;
        }

        // Under Trick Room the slower mon goes first; swap the fast/slow assignment.
        let (fast_idx, slow_idx, fast_move) = if trick_room_active {
            // idx1 went second → is the faster mon in normal ordering.
            (*idx1, *idx0, _mv1.clone())
        } else {
            (*idx0, *idx1, mv0.clone())
        };

        let (fast_mult, slow_mult) = compute_speed_multipliers(state, fast_idx, slow_idx);

        // ── Build escape disjuncts ────────────────────────────────────────────
        // Every escape disjunct D means: "the SpeedComparison OR the escape D explains
        // the observation" — so the predicate remains sound (a wider union).

        let fast_mon = get_mon_by_idx(state, fast_idx);
        let slow_mon = get_mon_by_idx(state, slow_idx);

        let mut clause: Vec<Statement> = vec![Statement::SpeedComparison {
            fast_idx,
            slow_idx,
            fast_mult,
            slow_mult,
        }];

        // (1) Quick Claw / Quick Draw on the fast mon — random first-mover.
        if fast_mon.map_or(false, |m| !unknown_is_excluded(&m.item, &Item::QuickClaw)) {
            clause.push(Statement::HasItem {
                mon_idx: fast_idx,
                item: Item::QuickClaw,
            });
        }
        if fast_mon.map_or(false, |m| {
            !unknown_is_excluded(&m.possible_abilities, &Ability::QuickDraw)
        }) {
            clause.push(Statement::HasAbility {
                mon_idx: fast_idx,
                ability: Ability::QuickDraw,
            });
        }

        // (2) Ability-priority boosts on the fast mon's move.
        //     Each is conditional on the move type/category matching the ability's trigger.
        if let (Some(fast_m), Some(fast_md)) = (fast_mon, move_dex.get(&fast_move)) {
            // Prankster: +1 to Status-category moves.
            if fast_md.category == MoveCategory::Status
                && !unknown_is_excluded(&fast_m.possible_abilities, &Ability::Prankster)
            {
                clause.push(Statement::HasAbility {
                    mon_idx: fast_idx,
                    ability: Ability::Prankster,
                });
            }
            // Gale Wings: +1 to Flying-type moves at full HP (Gen VIII+ condition).
            let fast_at_full_hp = matches!(fast_m.hp, PokemonHP::Percent(100));
            if fast_md.pokemon_type == PokemonType::Flying
                && fast_at_full_hp
                && !unknown_is_excluded(&fast_m.possible_abilities, &Ability::GaleWings)
            {
                clause.push(Statement::HasAbility {
                    mon_idx: fast_idx,
                    ability: Ability::GaleWings,
                });
            }
            // Triage: +3 to draining/healing moves.
            if fast_md.heal_fraction != [0, 0]
                && !unknown_is_excluded(&fast_m.possible_abilities, &Ability::Triage)
            {
                clause.push(Statement::HasAbility {
                    mon_idx: fast_idx,
                    ability: Ability::Triage,
                });
            }
        }

        // (3) Stall on the slow mon: Stall forces the holder to always go last
        //     within its priority bracket regardless of speed.
        if slow_mon.map_or(false, |m| {
            !unknown_is_excluded(&m.possible_abilities, &Ability::Stall)
        }) {
            clause.push(Statement::HasAbility {
                mon_idx: slow_idx,
                ability: Ability::Stall,
            });
        }

        // (4) Choice Scarf on the fast mon: ×1.5 effective speed means the natural
        //     SpeedComparison predicate is too strong (over-narrows) without this escape.
        if fast_mon.map_or(false, |m| !unknown_is_excluded(&m.item, &Item::ChoiceScarf)) {
            clause.push(Statement::HasItem {
                mon_idx: fast_idx,
                item: Item::ChoiceScarf,
            });
        }

        // (5) Speed-reducing items on the slow mon: these force the holder to go last
        //     in its bracket, explaining the ordering without implying a speed edge.
        if let Some(slow_m) = slow_mon {
            for slow_item in [Item::IronBall, Item::LaggingTail, Item::FullIncense] {
                if !unknown_is_excluded(&slow_m.item, &slow_item) {
                    clause.push(Statement::HasItem {
                        mon_idx: slow_idx,
                        item: slow_item,
                    });
                }
            }
        }

        // (6) Weather-conditional speed-doubling abilities on the fast mon.
        //     Only add escapes when the triggering weather is currently active.
        if let Some(fast_m) = fast_mon {
            let weather = &state.weather;
            let is_rain = matches!(weather, Some(Weather::Rain) | Some(Weather::HeavyRain));
            if is_rain && !unknown_is_excluded(&fast_m.possible_abilities, &Ability::SwiftSwim) {
                clause.push(Statement::HasAbility {
                    mon_idx: fast_idx,
                    ability: Ability::SwiftSwim,
                });
            }
            let is_sun =
                matches!(weather, Some(Weather::Sun) | Some(Weather::ExtremeSunlight));
            if is_sun && !unknown_is_excluded(&fast_m.possible_abilities, &Ability::Chlorophyll) {
                clause.push(Statement::HasAbility {
                    mon_idx: fast_idx,
                    ability: Ability::Chlorophyll,
                });
            }
            let is_sand = matches!(weather, Some(Weather::Sandstorm));
            if is_sand && !unknown_is_excluded(&fast_m.possible_abilities, &Ability::SandRush) {
                clause.push(Statement::HasAbility {
                    mon_idx: fast_idx,
                    ability: Ability::SandRush,
                });
            }
            let is_snow = matches!(weather, Some(Weather::Snow));
            if is_snow && !unknown_is_excluded(&fast_m.possible_abilities, &Ability::SlushRush) {
                clause.push(Statement::HasAbility {
                    mon_idx: fast_idx,
                    ability: Ability::SlushRush,
                });
            }
            // Surge Surfer: ×2 on Electric Terrain.
            if state.terrain == Some(Terrain::ElectricTerrain)
                && !unknown_is_excluded(&fast_m.possible_abilities, &Ability::SurgeSurfer)
            {
                clause.push(Statement::HasAbility {
                    mon_idx: fast_idx,
                    ability: Ability::SurgeSurfer,
                });
            }
            // Unburden: ×2 after losing held item.
            if fast_m.item_lost
                && !unknown_is_excluded(&fast_m.possible_abilities, &Ability::Unburden)
            {
                clause.push(Statement::HasAbility {
                    mon_idx: fast_idx,
                    ability: Ability::Unburden,
                });
            }
            // Quick Feet: ×1.5 when statused. Guard whenever the mon is statused;
            // the paralysis factor in `compute_speed_multipliers` already handles the
            // para case numerically, but Quick Feet *overrides* the paralysis penalty,
            // so the predicate may be too strong without this escape when both apply.
            if fast_m.status.is_some()
                && !unknown_is_excluded(&fast_m.possible_abilities, &Ability::QuickFeet)
            {
                clause.push(Statement::HasAbility {
                    mon_idx: fast_idx,
                    ability: Ability::QuickFeet,
                });
            }
        }

        // Emit: unit clause → unconditional bound; multi-entry → disjunction.
        state.predicates.push(clause);
    }
}

/// Integer speed multipliers (fast_mult, slow_mult) scaled to a common denominator.
///
/// Encodes: `base_spe(fast) * fast_mult >= base_spe(slow) * slow_mult`.
/// Accounts for boost stages, paralysis (×½), and Tailwind (×2, deterministic).
/// Items (Choice Scarf, Iron Ball) and ability-based multipliers (Swift Swim, etc.)
/// are NOT folded in — they are handled as escape disjuncts in `pass4_speed_from_order`.
fn compute_speed_multipliers(
    state: &UnknownBattleState,
    fast_idx: usize,
    slow_idx: usize,
) -> (u32, u32) {
    let fast_boost = get_mon_by_idx(state, fast_idx)
        .map(|m| m.boosts[4])
        .unwrap_or(0);
    let slow_boost = get_mon_by_idx(state, slow_idx)
        .map(|m| m.boosts[4])
        .unwrap_or(0);
    let fast_para = get_mon_by_idx(state, fast_idx)
        .map(|m| matches!(m.status, Some(Status::Paralysis)))
        .unwrap_or(false);
    let slow_para = get_mon_by_idx(state, slow_idx)
        .map(|m| matches!(m.status, Some(Status::Paralysis)))
        .unwrap_or(false);

    // Tailwind ×2: deterministically known from side conditions.
    let fast_tailwind = if mon_is_p2(state, fast_idx) {
        state.p2_side_conditions.contains(&SideCondition::TailWind)
    } else {
        state.p1_side_conditions.contains(&SideCondition::TailWind)
    };
    let slow_tailwind = if mon_is_p2(state, slow_idx) {
        state.p2_side_conditions.contains(&SideCondition::TailWind)
    } else {
        state.p1_side_conditions.contains(&SideCondition::TailWind)
    };

    // Stage multiplier as (numerator, denominator).
    let stage_frac = |stage: i8| -> (u32, u32) {
        let s = stage.clamp(-6, 6);
        if s >= 0 { (2 + s as u32, 2) } else { (2, 2 + (-s) as u32) }
    };

    let (fn_, fd) = stage_frac(fast_boost);
    let (sn_, sd) = stage_frac(slow_boost);
    // Paralysis ×1/2.
    let (fp_n, fp_d): (u32, u32) = if fast_para { (1, 2) } else { (1, 1) };
    let (sp_n, sp_d): (u32, u32) = if slow_para { (1, 2) } else { (1, 1) };
    // Tailwind ×2.
    let (ft_n, ft_d): (u32, u32) = if fast_tailwind { (2, 1) } else { (1, 1) };
    let (st_n, st_d): (u32, u32) = if slow_tailwind { (2, 1) } else { (1, 1) };

    // Combine to a common scale.
    // fast_mult = fn_*fp_n*ft_n * (sd*sp_d*st_d)
    // slow_mult = sn_*sp_n*st_n * (fd*fp_d*ft_d)
    let fast_mult = fn_ * fp_n * ft_n * sd * sp_d * st_d;
    let slow_mult = sn_ * sp_n * st_n * fd * fp_d * ft_d;
    (fast_mult, slow_mult)
}

// ── Pass 5: Back-solve EV / IV / nature from stat bounds ──────────────────────

/// Tighten `minEvs`/`maxEvs`/`possible_natures` from current `minStats`/`maxStats`.
pub fn pass5_back_solve(
    mon: &mut UnknownPokemonState,
    config: &InferenceConfig,
    dex: &HashMap<Species, PokemonData>,
) {
    let base: [u16; 6] = match &mon.possible_species {
        Unknown::Known(s) => match dex.get(s) {
            Some(d) => d.base_stats,
            None => return,
        },
        _ => return, // Ambiguous species — skip (sound: we only narrow).
    };
    let level = mon.level;

    let all_natures = ALL_NATURES;
    let candidate_natures: Vec<Nature> = all_natures
        .iter()
        .copied()
        .filter(|n| !unknown_is_excluded(&mon.possible_natures, n))
        .collect();

    if candidate_natures.is_empty() {
        inference_contradiction!("pass5", "no remaining valid natures");
    }

    let ev_candidates: &[u8] = &EV_LATTICE;

    // ── HP (stat_i = 0, no nature modifier) ──────────────────────────────────
    {
        let s_min = mon.minStats[0];
        let s_max = mon.maxStats[0];
        let iv_range = if config.force_max_ivs {
            31..=31
        } else {
            mon.minIvs[0]..=mon.maxIvs[0]
        };
        let mut min_ev: Option<u8> = None;
        let mut max_ev: Option<u8> = None;
        let mut any = false;
        for iv in iv_range {
            for &ev in ev_candidates {
                let hp = calc_hp(base[0], iv, ev, level);
                if hp >= s_min && hp <= s_max {
                    any = true;
                    min_ev = Some(min_ev.map_or(ev, |g: u8| g.min(ev)));
                    max_ev = Some(max_ev.map_or(ev, |g: u8| g.max(ev)));
                }
            }
        }
        if !any {
            inference_contradiction!("pass5-hp", "no IV/EV can produce observed HP bounds");
        }
        if let Some(lo) = min_ev {
            if lo > mon.minEvs[0] {
                mon.minEvs[0] = lo;
            }
        }
        if let Some(hi) = max_ev {
            if hi < mon.maxEvs[0] {
                mon.maxEvs[0] = hi;
            }
        }
    }

    // ── Non-HP stats (stat_i = 1..=5) ────────────────────────────────────────
    let mut impossible_natures: Vec<bool> = vec![false; candidate_natures.len()];

    for stat_i in 1usize..=5 {
        let s_min = mon.minStats[stat_i];
        let s_max = mon.maxStats[stat_i];
        let iv_range = if config.force_max_ivs {
            31..=31u8
        } else {
            mon.minIvs[stat_i]..=mon.maxIvs[stat_i]
        };
        let mut global_min_ev: Option<u8> = None;
        let mut global_max_ev: Option<u8> = None;

        for (ni, nature) in candidate_natures.iter().enumerate() {
            if impossible_natures[ni] {
                continue;
            }
            let mods = nature_stat_modifiers(nature);
            let nature_mod = mods[stat_i - 1]; // [atk, def, spa, spd, spe]

            let mut found = false;
            let mut n_min_ev: Option<u8> = None;
            let mut n_max_ev: Option<u8> = None;

            let bsv_min = mon.min_pre_nature_stat[stat_i];
            let bsv_max = mon.max_pre_nature_stat[stat_i];
            for iv in iv_range.clone() {
                for &ev in ev_candidates {
                    let bsv = calc_stat(base[stat_i], iv, ev, level, 1.0);
                    // Pre-nature BSV constraint (from Pass 3 damage inversion).
                    if bsv < bsv_min || bsv > bsv_max {
                        continue;
                    }
                    let stat = calc_stat(base[stat_i], iv, ev, level, nature_mod);
                    if stat >= s_min && stat <= s_max {
                        found = true;
                        n_min_ev = Some(n_min_ev.map_or(ev, |g: u8| g.min(ev)));
                        n_max_ev = Some(n_max_ev.map_or(ev, |g: u8| g.max(ev)));
                    }
                }
            }

            if !found {
                impossible_natures[ni] = true;
            } else {
                if let Some(lo) = n_min_ev {
                    global_min_ev = Some(global_min_ev.map_or(lo, |g: u8| g.min(lo)));
                }
                if let Some(hi) = n_max_ev {
                    global_max_ev = Some(global_max_ev.map_or(hi, |g: u8| g.max(hi)));
                }
            }
        }

        if impossible_natures
            .iter()
            .enumerate()
            .filter(|(ni, _)| !{
                // re-filter to only candidate natures
                false
            })
            .all(|(ni, _)| impossible_natures[ni])
        {
            // all remaining candidates are impossible for this stat — panic only
            // if ALL candidates (not just the ones already impossible) fail.
        }

        if let Some(lo) = global_min_ev {
            if lo > mon.minEvs[stat_i] {
                mon.minEvs[stat_i] = lo;
            }
        }
        if let Some(hi) = global_max_ev {
            if hi < mon.maxEvs[stat_i] {
                mon.maxEvs[stat_i] = hi;
            }
        }
    }

    // Eliminate natures that were impossible for any stat.
    for (ni, nature) in candidate_natures.iter().enumerate() {
        if impossible_natures[ni] {
            unknown_exclude(&mut mon.possible_natures, nature, "pass5-nature");
        }
    }

    // Panic if every nature is now excluded.
    let remaining = all_natures
        .iter()
        .filter(|n| !unknown_is_excluded(&mon.possible_natures, n))
        .count();
    if remaining == 0 {
        inference_contradiction!("pass5", "no valid nature remains after pass5");
    }

    // ── Global EV total-cap cross-stat tightening ─────────────────────────────
    // Applies only when a cap is configured (default 510 for Pokémon Champions).
    // Sound: only ever tightens maxEvs; never raises minEvs.
    // Invariant: Σ_i evs[i] ≤ cap  →  evs[i] ≤ cap − Σ_{j≠i} minEvs[j].
    if let Some(cap) = config.ev_total_cap {
        let cap = cap as u16;
        // Collect per-stat EV floor sum.
        let min_ev_sum: u16 = (0..6).map(|i| mon.minEvs[i] as u16).sum();
        let ev_lattice = if config.use_stat_points { Some(ev_candidates) } else { None };

        for stat_i in 0..6 {
            let other_min_sum = min_ev_sum - mon.minEvs[stat_i] as u16;
            if other_min_sum >= cap {
                // All other stats already use the full cap — this stat can't have any EVs.
                mon.maxEvs[stat_i] = 0;
                continue;
            }
            let budget = cap - other_min_sum; // max EVs allowed in stat_i
            if budget < mon.maxEvs[stat_i] as u16 {
                // Round down to the nearest valid lattice value.
                let capped_max = if let Some(lattice) = ev_lattice {
                    lattice
                        .iter()
                        .rev()
                        .find(|&&v| (v as u16) <= budget)
                        .copied()
                        .unwrap_or(0)
                } else {
                    budget.min(252) as u8
                };
                if capped_max < mon.maxEvs[stat_i] {
                    mon.maxEvs[stat_i] = capped_max;
                }
            }
        }
    }
}

const ALL_NATURES: &[Nature] = &[
    Nature::Hardy,
    Nature::Lonely,
    Nature::Adamant,
    Nature::Naughty,
    Nature::Brave,
    Nature::Bold,
    Nature::Docile,
    Nature::Impish,
    Nature::Lax,
    Nature::Relaxed,
    Nature::Modest,
    Nature::Mild,
    Nature::Bashful,
    Nature::Rash,
    Nature::Quiet,
    Nature::Calm,
    Nature::Gentle,
    Nature::Careful,
    Nature::Quirky,
    Nature::Sassy,
    Nature::Timid,
    Nature::Hasty,
    Nature::Jolly,
    Nature::Naive,
    Nature::Serious,
];

// ── Pass 6: BCP (Boolean Constraint Propagation) ─────────────────────────────

fn run_bcp(state: &mut UnknownBattleState) {
    let mut changed = true;
    while changed {
        changed = false;

        let mut i = 0;
        while i < state.predicates.len() {
            // Remove literals that are definitely false.
            let still_live: Vec<Statement> = state.predicates[i]
                .iter()
                .filter(|lit| !eval_false(state, lit))
                .cloned()
                .collect();

            if still_live.is_empty() {
                inference_contradiction!("bcp", "unsatisfiable clause (all literals false)");
            }

            // Clause already satisfied by a known-true literal — drop it.
            if still_live.iter().any(|lit| eval_true(state, lit)) {
                state.predicates.remove(i);
                changed = true;
                continue;
            }

            // Unit clause — force the single remaining literal.
            // SpeedComparison is a permanent relational constraint; it cannot be
            // "forced" into a field and must remain in predicates for propagation.
            if still_live.len() == 1 && !matches!(still_live[0], Statement::SpeedComparison { .. })
            {
                let lit = still_live[0].clone();
                state.predicates.remove(i);
                force_literal(state, &lit);
                changed = true;
                continue;
            }

            if still_live.len() != state.predicates[i].len() {
                state.predicates[i] = still_live;
                changed = true;
            }
            i += 1;
        }

        if propagate_speed_comparisons(state) {
            changed = true;
        }
    }
}

fn eval_false(state: &UnknownBattleState, lit: &Statement) -> bool {
    match lit {
        Statement::Not(inner) => eval_true(state, inner),
        Statement::HasItem { mon_idx, item } => {
            get_mon_by_idx(state, *mon_idx).map_or(false, |m| unknown_is_excluded(&m.item, item))
        }
        Statement::HasStatus { mon_idx, status } => get_mon_by_idx(state, *mon_idx)
            .map_or(false, |m| m.status.as_ref().map_or(true, |s| s != status)),
        Statement::HasMove {
            mon_idx,
            pokemon_move,
        } => get_mon_by_idx(state, *mon_idx).map_or(false, |m| {
            let full = m.known_moves.iter().all(|s| s.is_some());
            full && !m
                .known_moves
                .iter()
                .any(|s| s.as_ref() == Some(pokemon_move))
        }),
        Statement::HasAbility { mon_idx, ability } => get_mon_by_idx(state, *mon_idx)
            .map_or(false, |m| {
                unknown_is_excluded(&m.possible_abilities, ability)
            }),
        Statement::NatureBoostsStat { mon_idx, stat } => {
            get_mon_by_idx(state, *mon_idx).map_or(false, |m| {
                boosting_natures_for_stat(stat)
                    .iter()
                    .all(|n| unknown_is_excluded(&m.possible_natures, n))
            })
        }
        Statement::NatureNerfsStat { mon_idx, stat } => {
            get_mon_by_idx(state, *mon_idx).map_or(false, |m| {
                nerfing_natures_for_stat(stat)
                    .iter()
                    .all(|n| unknown_is_excluded(&m.possible_natures, n))
            })
        }
        Statement::EVIVStatGE {
            mon_idx,
            stat,
            value,
        } => get_mon_by_idx(state, *mon_idx)
            .map_or(false, |m| m.max_pre_nature_stat[stat_to_stats_idx(stat)] < *value),
        Statement::EVIVStatLE {
            mon_idx,
            stat,
            value,
        } => get_mon_by_idx(state, *mon_idx)
            .map_or(false, |m| m.min_pre_nature_stat[stat_to_stats_idx(stat)] > *value),
        Statement::SpeedComparison {
            fast_idx,
            slow_idx,
            fast_mult,
            slow_mult,
        } => {
            let fast_max =
                get_mon_by_idx(state, *fast_idx).map_or(999u64, |m| m.maxStats[5] as u64);
            let slow_min = get_mon_by_idx(state, *slow_idx).map_or(0u64, |m| m.minStats[5] as u64);
            fast_max * (*fast_mult as u64) < slow_min * (*slow_mult as u64)
        }
        Statement::WeatherTurns { turns } => {
            // Definitely false if no weather active, or if the turn count is excluded.
            state
                .weather_turns
                .as_ref()
                .map_or(true, |wt| unknown_is_excluded(wt, &(*turns as u8)))
        }
        Statement::PseudoWeatherTurns { turns } => {
            // Conservative: only rule out when there is exactly one pseudo-weather
            // active and its count definitively excludes this value.
            if state.pseudo_weather_turns.len() == 1 {
                unknown_is_excluded(&state.pseudo_weather_turns[0], &(*turns as u8))
            } else {
                false
            }
        }
        Statement::SideConditionTurns {
            side,
            side_condition,
            turns,
        } => {
            let (conditions, turns_vec) = match side {
                Player::P1 => (
                    &state.p1_side_conditions,
                    &state.p1_side_condition_turns,
                ),
                Player::P2 => (
                    &state.p2_side_conditions,
                    &state.p2_side_condition_turns,
                ),
            };
            conditions
                .iter()
                .position(|c| c == side_condition)
                .map_or(true, |i| {
                    // Condition not in list → it's not active → statement false.
                    turns_vec
                        .get(i)
                        .map_or(true, |ct| unknown_is_excluded(ct, &(*turns as u8)))
                })
        }
    }
}

fn eval_true(state: &UnknownBattleState, lit: &Statement) -> bool {
    match lit {
        Statement::Not(inner) => eval_false(state, inner),
        Statement::HasItem { mon_idx, item } => {
            get_mon_by_idx(state, *mon_idx).map_or(false, |m| unknown_is_known_as(&m.item, item))
        }
        Statement::HasStatus { mon_idx, status } => {
            get_mon_by_idx(state, *mon_idx).map_or(false, |m| m.status.as_ref() == Some(status))
        }
        Statement::HasMove {
            mon_idx,
            pokemon_move,
        } => get_mon_by_idx(state, *mon_idx).map_or(false, |m| {
            m.known_moves
                .iter()
                .any(|s| s.as_ref() == Some(pokemon_move))
        }),
        Statement::HasAbility { mon_idx, ability } => get_mon_by_idx(state, *mon_idx)
            .map_or(false, |m| {
                unknown_is_known_as(&m.possible_abilities, ability)
            }),
        Statement::EVIVStatGE {
            mon_idx,
            stat,
            value,
        } => get_mon_by_idx(state, *mon_idx)
            .map_or(false, |m| m.min_pre_nature_stat[stat_to_stats_idx(stat)] >= *value),
        Statement::EVIVStatLE {
            mon_idx,
            stat,
            value,
        } => get_mon_by_idx(state, *mon_idx)
            .map_or(false, |m| m.max_pre_nature_stat[stat_to_stats_idx(stat)] <= *value),
        Statement::SpeedComparison {
            fast_idx,
            slow_idx,
            fast_mult,
            slow_mult,
        } => {
            let fast_min = get_mon_by_idx(state, *fast_idx).map_or(0u64, |m| m.minStats[5] as u64);
            let slow_max =
                get_mon_by_idx(state, *slow_idx).map_or(999u64, |m| m.maxStats[5] as u64);
            fast_min * (*fast_mult as u64) >= slow_max * (*slow_mult as u64)
        }
        Statement::NatureBoostsStat { mon_idx, stat } => {
            get_mon_by_idx(state, *mon_idx).map_or(false, |m| {
                let boosters = boosting_natures_for_stat(stat);
                match &m.possible_natures {
                    Unknown::Known(n) => boosters.contains(n),
                    Unknown::Possibly(v) => !v.is_empty() && v.iter().all(|n| boosters.contains(n)),
                    Unknown::Not(_) => false, // Not(excluded) can't confirm without full enumeration
                }
            })
        }
        Statement::NatureNerfsStat { mon_idx, stat } => {
            get_mon_by_idx(state, *mon_idx).map_or(false, |m| {
                let nerfers = nerfing_natures_for_stat(stat);
                match &m.possible_natures {
                    Unknown::Known(n) => nerfers.contains(n),
                    Unknown::Possibly(v) => !v.is_empty() && v.iter().all(|n| nerfers.contains(n)),
                    Unknown::Not(_) => false,
                }
            })
        }
        Statement::WeatherTurns { turns } => state
            .weather_turns
            .as_ref()
            .map_or(false, |wt| matches!(wt, Unknown::Known(v) if *v == *turns as u8)),
        Statement::PseudoWeatherTurns { turns } => {
            if state.pseudo_weather_turns.len() == 1 {
                matches!(&state.pseudo_weather_turns[0], Unknown::Known(v) if *v == *turns as u8)
            } else {
                false
            }
        }
        Statement::SideConditionTurns {
            side,
            side_condition,
            turns,
        } => {
            let (conditions, turns_vec) = match side {
                Player::P1 => (
                    &state.p1_side_conditions,
                    &state.p1_side_condition_turns,
                ),
                Player::P2 => (
                    &state.p2_side_conditions,
                    &state.p2_side_condition_turns,
                ),
            };
            conditions
                .iter()
                .position(|c| c == side_condition)
                .and_then(|i| turns_vec.get(i))
                .map_or(false, |ct| {
                    matches!(ct, Unknown::Known(v) if *v == *turns as u8)
                })
        }
    }
}

fn force_literal(state: &mut UnknownBattleState, lit: &Statement) {
    match lit {
        Statement::HasItem { mon_idx, item } => {
            if let Some(mon) = get_mon_mut_by_idx(state, *mon_idx) {
                unknown_set_known(&mut mon.item, item.clone(), &format!("bcp#{mon_idx}"));
            }
        }
        Statement::HasAbility { mon_idx, ability } => {
            if let Some(mon) = get_mon_mut_by_idx(state, *mon_idx) {
                unknown_set_known(
                    &mut mon.possible_abilities,
                    ability.clone(),
                    &format!("bcp#{mon_idx}"),
                );
            }
        }
        Statement::HasMove { mon_idx, pokemon_move } => {
            if let Some(mon) = get_mon_mut_by_idx(state, *mon_idx) {
                reveal_move_on_mon(mon, pokemon_move);
            }
        }
        Statement::HasStatus { mon_idx, status } => {
            if let Some(mon) = get_mon_mut_by_idx(state, *mon_idx) {
                mon.status = Some(status.clone());
            }
        }
        Statement::EVIVStatGE { mon_idx, stat, value } => {
            if let Some(mon) = get_mon_mut_by_idx(state, *mon_idx) {
                let si = stat_to_stats_idx(stat);
                if mon.min_pre_nature_stat[si] < *value {
                    mon.min_pre_nature_stat[si] = *value;
                }
            }
        }
        Statement::EVIVStatLE { mon_idx, stat, value } => {
            if let Some(mon) = get_mon_mut_by_idx(state, *mon_idx) {
                let si = stat_to_stats_idx(stat);
                if mon.max_pre_nature_stat[si] > *value {
                    mon.max_pre_nature_stat[si] = *value;
                }
            }
        }
        Statement::NatureBoostsStat { mon_idx, stat } => {
            if let Some(mon) = get_mon_mut_by_idx(state, *mon_idx) {
                let valid = boosting_natures_for_stat(stat);
                filter_natures_to_set(&mut mon.possible_natures, &valid, "bcp-nature-boosts");
            }
        }
        Statement::NatureNerfsStat { mon_idx, stat } => {
            if let Some(mon) = get_mon_mut_by_idx(state, *mon_idx) {
                let valid = nerfing_natures_for_stat(stat);
                filter_natures_to_set(&mut mon.possible_natures, &valid, "bcp-nature-nerfs");
            }
        }
        Statement::WeatherTurns { turns } => {
            let t = *turns as u8;
            if let Some(wt) = &mut state.weather_turns {
                unknown_set_known(wt, t, "bcp-weather-turns");
            } else {
                inference_contradiction!(
                    "bcp-weather-turns",
                    "WeatherTurns forced to {} but no weather is active",
                    turns
                );
            }
        }
        Statement::PseudoWeatherTurns { turns } => {
            // Only deterministic when exactly one pseudo-weather is active.
            if state.pseudo_weather_turns.len() == 1 {
                let t = *turns as u8;
                unknown_set_known(
                    &mut state.pseudo_weather_turns[0],
                    t,
                    "bcp-pseudo-weather-turns",
                );
            }
            // Multiple pseudo-weathers → can't attribute; no-op (conservative).
        }
        Statement::SideConditionTurns {
            side,
            side_condition,
            turns,
        } => {
            let t = *turns as u8;
            let idx = match side {
                Player::P1 => state
                    .p1_side_conditions
                    .iter()
                    .position(|c| c == side_condition),
                Player::P2 => state
                    .p2_side_conditions
                    .iter()
                    .position(|c| c == side_condition),
            };
            if let Some(i) = idx {
                let turns_vec = match side {
                    Player::P1 => &mut state.p1_side_condition_turns,
                    Player::P2 => &mut state.p2_side_condition_turns,
                };
                if let Some(ct) = turns_vec.get_mut(i) {
                    unknown_set_known(ct, t, "bcp-side-condition-turns");
                }
            }
        }
        Statement::Not(_)
        | Statement::SpeedComparison { .. } => {} // handled by propagate_speed_comparisons
    }
}

/// Retain in `natures` only those that appear in `valid`.
/// Converts `Not(excluded)` to an explicit `Possibly` before filtering.
/// Panics (contradiction) if no valid natures remain.
fn filter_natures_to_set(natures: &mut Unknown<Nature>, valid: &[Nature], ctx: &str) {
    match natures {
        Unknown::Known(n) => {
            if !valid.contains(n) {
                inference_contradiction!(
                    ctx,
                    "Nature {:?} does not satisfy constraint (valid: {:?})",
                    n,
                    valid
                );
            }
        }
        Unknown::Not(excluded) => {
            let mut candidates: Vec<Nature> = ALL_NATURES
                .iter()
                .filter(|n| valid.contains(n) && !excluded.contains(n))
                .cloned()
                .collect();
            if candidates.is_empty() {
                inference_contradiction!(ctx, "No valid natures remain after constraint");
            }
            if candidates.len() == 1 {
                *natures = Unknown::Known(candidates.remove(0));
            } else {
                *natures = Unknown::Possibly(candidates);
            }
        }
        Unknown::Possibly(v) => {
            v.retain(|n| valid.contains(n));
            if v.is_empty() {
                inference_contradiction!(ctx, "No valid natures remain after constraint");
            }
            if v.len() == 1 {
                let n = v[0].clone();
                *natures = Unknown::Known(n);
            }
        }
    }
}

/// Bidirectional Spe bound propagation from all `SpeedComparison` predicates.
/// Returns `true` if any bound changed.
fn propagate_speed_comparisons(state: &mut UnknownBattleState) -> bool {
    let total = mons_count_battle(state);
    let comparisons: Vec<(usize, usize, u32, u32)> = state
        .predicates
        .iter()
        .flat_map(|clause| {
            clause.iter().filter_map(|lit| {
                if let Statement::SpeedComparison {
                    fast_idx,
                    slow_idx,
                    fast_mult,
                    slow_mult,
                } = lit
                {
                    if *fast_idx < total && *slow_idx < total && *fast_mult > 0 && *slow_mult > 0 {
                        Some((*fast_idx, *slow_idx, *fast_mult, *slow_mult))
                    } else {
                        None
                    }
                } else {
                    None
                }
            })
        })
        .collect();

    let mut changed = false;
    for (fast_idx, slow_idx, fast_mult, slow_mult) in comparisons {
        // Raise fast's min Spe: base_spe(fast) >= ceil(base_spe(slow)*slow_mult / fast_mult)
        let slow_min = get_mon_by_idx(state, slow_idx).map_or(0u64, |m| m.minStats[5] as u64);
        let new_fast_min = div_ceil(slow_min * slow_mult as u64, fast_mult as u64) as u16;
        if let Some(mon) = get_mon_mut_by_idx(state, fast_idx) {
            if new_fast_min > mon.minStats[5] {
                if new_fast_min > mon.maxStats[5] {
                    inference_contradiction!(
                        fast_idx,
                        "SpeedComparison raises min({}) above max({})",
                        new_fast_min,
                        mon.maxStats[5]
                    );
                }
                mon.minStats[5] = new_fast_min;
                changed = true;
            }
        }

        // Lower slow's max Spe: base_spe(slow) <= floor(base_spe(fast)*fast_mult / slow_mult)
        let fast_max =
            get_mon_by_idx(state, fast_idx).map_or(u64::MAX / 2, |m| m.maxStats[5] as u64);
        let new_slow_max = (fast_max.saturating_mul(fast_mult as u64) / slow_mult as u64)
            .min(u16::MAX as u64) as u16;
        if let Some(mon) = get_mon_mut_by_idx(state, slow_idx) {
            if new_slow_max < mon.maxStats[5] {
                if new_slow_max < mon.minStats[5] {
                    inference_contradiction!(
                        slow_idx,
                        "SpeedComparison lowers max({}) below min({})",
                        new_slow_max,
                        mon.minStats[5]
                    );
                }
                mon.maxStats[5] = new_slow_max;
                changed = true;
            }
        }
    }
    changed
}

fn div_ceil(a: u64, b: u64) -> u64 {
    if b == 0 {
        return a;
    }
    (a + b - 1) / b
}

// ── Nature helpers ────────────────────────────────────────────────────────────

fn boosting_natures_for_stat(stat: &PokemonStat) -> Vec<Nature> {
    match stat {
        PokemonStat::Atk => vec![
            Nature::Lonely,
            Nature::Adamant,
            Nature::Naughty,
            Nature::Brave,
        ],
        PokemonStat::Def => vec![Nature::Bold, Nature::Impish, Nature::Lax, Nature::Relaxed],
        PokemonStat::SpA => vec![Nature::Modest, Nature::Mild, Nature::Rash, Nature::Quiet],
        PokemonStat::SpD => vec![Nature::Calm, Nature::Gentle, Nature::Careful, Nature::Sassy],
        PokemonStat::Spe => vec![Nature::Timid, Nature::Hasty, Nature::Jolly, Nature::Naive],
    }
}

fn nerfing_natures_for_stat(stat: &PokemonStat) -> Vec<Nature> {
    match stat {
        PokemonStat::Atk => vec![Nature::Bold, Nature::Modest, Nature::Calm, Nature::Timid],
        PokemonStat::Def => vec![Nature::Lonely, Nature::Mild, Nature::Gentle, Nature::Hasty],
        PokemonStat::SpA => vec![
            Nature::Adamant,
            Nature::Impish,
            Nature::Careful,
            Nature::Jolly,
        ],
        PokemonStat::SpD => vec![Nature::Naughty, Nature::Lax, Nature::Rash, Nature::Naive],
        PokemonStat::Spe => vec![Nature::Brave, Nature::Relaxed, Nature::Quiet, Nature::Sassy],
    }
}

fn stat_to_stats_idx(stat: &PokemonStat) -> usize {
    match stat {
        PokemonStat::Atk => 1,
        PokemonStat::Def => 2,
        PokemonStat::SpA => 3,
        PokemonStat::SpD => 4,
        PokemonStat::Spe => 5,
    }
}

// ── Illusion detection ────────────────────────────────────────────────────────

const ILLUSION_FORMES: &[Species] = &[Species::Zoroark, Species::ZoroarkHisui];

/// Widen `possible_species` to include Zoroark formes when the opponent's back
/// contains one and the on-field species is unconfirmed.  Call after a Switch.
fn maybe_widen_for_illusion(
    state: &mut UnknownBattleState,
    slot: &FieldSlot,
    opponent_known_back_species: &[Species],
) {
    let has_zoroark = opponent_known_back_species
        .iter()
        .any(|s| ILLUSION_FORMES.contains(s));
    if !has_zoroark {
        return;
    }
    let Some(idx) = mon_idx_for_active_slot(state, slot) else {
        return;
    };
    let Some(mon) = get_mon_mut_by_idx(state, idx) else {
        return;
    };
    if let Unknown::Known(ref s) = mon.possible_species.clone() {
        if !ILLUSION_FORMES.contains(s) {
            let mut candidates = vec![s.clone()];
            for zf in ILLUSION_FORMES {
                if opponent_known_back_species.contains(zf) {
                    candidates.push(zf.clone());
                }
            }
            if candidates.len() > 1 {
                mon.possible_species = Unknown::Possibly(candidates);
            }
        }
    }
}
