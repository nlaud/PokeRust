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
use crate::simulator::helpers::{base_damage_formula, move_has_flag};
use crate::state::battle::{FieldSlot, Player};
use crate::state::dex_data::{
    AbilityData, AccuracyType, MoveCategory, MoveData, MoveFlag, PokemonData, PokemonStat,
    PokemonType, PseudoWeather, SideCondition, SlotCondition, Status, Terrain, VolatileStatus,
    Weather,
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
    /// Whether two Pokémon on the same team may hold the same item (item clause).
    /// When `false` (the Pokémon Champions default), the engine assumes each
    /// non-`Item::None` item appears at most once per team: once a teammate's item
    /// is confirmed as `X`, `X` is excluded from every other distinct teammate's
    /// item lattice. `Item::None` (no item) is exempt and may appear on any number
    /// of teammates. When `true`, no cross-teammate exclusion is performed.
    pub allow_repeat_items: bool,
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
            allow_repeat_items: false,
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
pub fn unknown_is_excluded<T: PartialEq>(u: &Unknown<T>, val: &T) -> bool {
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

// ── Item-clause helpers ────────────────────────────────────────────────────────

/// Return the `mon_idx` values for every Pokémon on the **same side** as
/// `source_idx`, excluding `source_idx` itself. Used by item-clause propagation.
///
/// Soundness assumption: each entry in the six roster lists
/// (`p1_active`, `p1_known_back`, `p1_possible_back`, and their p2 mirrors) is a
/// *pairwise-distinct* physical roster member. This holds today — `possible_back`
/// is only populated by the not-yet-built frontend and is empty in all current
/// code paths. If future Illusion / overlapping-hypothesis modeling lets two
/// `possible_back` entries represent the *same* physical slot, gate exclusion
/// here so it never fires across such alternatives.
// TODO: revisit if possible_back ever holds non-distinct Illusion hypotheses.
fn teammate_indices(state: &UnknownBattleState, source_idx: usize) -> Vec<usize> {
    let p1a = state.p1_active_mons.len();
    let p1k = state.p1_known_back_mons.len();
    let p1p = state.p1_possible_back_mons.len();
    let p1_end = p1a + p1k + p1p;

    let p2a = state.p2_active_mons.len();
    let p2k = state.p2_known_back_mons.len();
    let p2p = state.p2_possible_back_mons.len();
    let p2_end = p1_end + p2a + p2k + p2p;

    let (start, end) = if source_idx < p1_end {
        (0, p1_end)
    } else if source_idx < p2_end {
        (p1_end, p2_end)
    } else {
        return vec![];
    };

    (start..end).filter(|&i| i != source_idx).collect()
}

/// Under item clause, exclude `item` from every distinct teammate of the mon at
/// `source_idx`. No-op when `allow_repeat_items` is `true` or `item` is
/// `Item::None` (no-item may appear on multiple mons freely).
fn enforce_unique_item(
    state: &mut UnknownBattleState,
    source_idx: usize,
    item: &Item,
    allow_repeat_items: bool,
) {
    if allow_repeat_items || *item == Item::None {
        return;
    }
    for idx in teammate_indices(state, source_idx) {
        if let Some(mon) = get_mon_mut_by_idx(state, idx) {
            unknown_exclude(&mut mon.item, item, &format!("item-clause#{idx}"));
        }
    }
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

/// Run `pass5_back_solve` on every mon in `state` whose species is fully known.
/// This is a pure information-gain step: it converts tightened `min/max_pre_nature_stat`
/// bounds into narrower `minEvs/maxEvs` and excluded natures.  Safe to call multiple
/// times — bounds are monotone so it always converges.
fn run_pass5_all_mons(
    state: &mut UnknownBattleState,
    config: &InferenceConfig,
    dex: &HashMap<Species, PokemonData>,
) {
    let total = mons_count_battle(state);
    for idx in 0..total {
        let has_known_species = get_mon_by_idx(state, idx)
            .map(|m| matches!(m.possible_species, Unknown::Known(_)))
            .unwrap_or(false);
        if has_known_species {
            if let Some(mon) = get_mon_mut_by_idx(state, idx) {
                pass5_back_solve(mon, config, dex);
            }
        }
    }
}

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
        switch_slot: None,
    };
    for event in events {
        process_battle_event(state, event, &mut ctx);
    }

    // ── Pass 5 (first): back-solve EV/IV/nature from tightened stat bounds ───
    run_pass5_all_mons(state, config, dex);

    // ── Pass 6: BCP to fixpoint ────────────────────────────────────────────────
    run_bcp(state, config.allow_repeat_items);

    // ── Pass 4 re-derivation: if BCP forced a priority ability to Known, re-run
    // speed ordering with the tighter bracket so speed bounds are updated.
    // One re-run is sufficient; duplicate clauses are now guarded against.
    pass4_speed_from_order(state, events, move_dex, ability_dex);
    while propagate_speed_comparisons(state) {}
    run_bcp(state, config.allow_repeat_items);

    // ── Pass 5 (second): re-run after BCP so that stat bounds tightened by
    // force_literal (e.g. from a SpeedComparison clause resolving to Known) are
    // reflected in EV/IV/nature narrowing.  BCP is re-run once more to propagate
    // any newly excluded natures.  Bounds are monotone → guaranteed to terminate.
    run_pass5_all_mons(state, config, dex);
    run_bcp(state, config.allow_repeat_items);
}

/// Context threaded through the recursive event walk.
struct BattleContext<'a> {
    dex: &'a HashMap<Species, PokemonData>,
    move_dex: &'a HashMap<PokemonMove, MoveData>,
    ability_dex: &'a HashMap<Ability, AbilityData>,
    config: &'a InferenceConfig,
    /// The nearest enclosing `MoveUsed`, for nested-reaction analysis.
    move_context: Option<MoveContext>,
    /// The nearest enclosing single-mon `Switch`, set while processing that event's
    /// reactions.  Used by `WeatherChanged` / `TerrainChanged` handlers to attribute
    /// ability-triggered field effects (Drizzle, Drought, etc.) to the switching mon.
    switch_slot: Option<FieldSlot>,
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
    let prev_switch_slot = ctx.switch_slot.clone();

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
        // Clear switch_slot: we're now in a move context, not a switch context.
        ctx.switch_slot = None;
    }

    // For a single-mon switch, record the slot so that field-effect reactions
    // (WeatherChanged / TerrainChanged from Drizzle / Electric Surge, etc.)
    // can attribute the effect to the switching mon.
    if let EventKind::Switch(sw) = &event.kind {
        ctx.switch_slot = Some(sw.slot.clone());
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
        pass2_contact_absence(state, event, ctx);
        pass2_prankster_immunity(state, event, ctx);
        pass2_powder_immunity(state, event, ctx);
        pass2_guaranteed_status_absence(state, event, ctx);
        pass2_ground_immune_clause(state, event, ctx);
        pass3_damage_to_stats(state, event, ctx);
    }

    ctx.move_context = prev_move_ctx;
    ctx.switch_slot = prev_switch_slot;
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
            pass_eot_heal(state, event, ctx);
            pass_eot_sand_immunity(state, event, ctx);
            pass_eot_self_status(state, event, ctx);
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

            // Compute the HP delta (amount of damage dealt, not the pre-hit HP value).
            // The simulator stores eff_damage (the delta) in last_damage_taken; we must
            // match that so Counter / Mirror Coat / Metal Burst work correctly.
            let damage_delta: PokemonHP = match (&old_hp, &new_hp) {
                (Some(PokemonHP::Number(o)), PokemonHP::Number(n)) => {
                    PokemonHP::Number(o.saturating_sub(*n))
                }
                (Some(PokemonHP::Percent(o)), PokemonHP::Percent(n)) => {
                    PokemonHP::Percent(o.saturating_sub(*n))
                }
                _ => PokemonHP::Percent(0),
            };

            // Per-turn damage tracking (mirrors end_turn Phase 5 fields).
            if let Some(idx) = mon_idx_for_active_slot(state, target) {
                if let Some(mon) = get_mon_mut_by_idx(state, idx) {
                    mon.damaged_this_turn = true;
                    mon.last_damage_taken = damage_delta.clone();
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
                                    mon.last_physical_damage_taken = damage_delta.clone();
                                    mon.last_physical_attacker = Some(attacker.clone());
                                }
                                MoveCategory::Special => {
                                    mon.last_special_damage_taken = damage_delta.clone();
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
                // Item clause: a confirmed team-built item cannot be held by any
                // other roster member on the same side.
                enforce_unique_item(state, idx, item, ctx.config.allow_repeat_items);
            }
        }
        EventKind::ItemGained { slot, item } => {
            // NOTE: ItemGained covers mid-battle item transfers (Trick, Switcheroo,
            // Recycle, Pickup). These are not team-built items, so item-clause
            // exclusion must NOT propagate to teammates here.
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
                        // Preserve the named item revealed by Knock Off / Thief / Fling.
                        mon.removed_item = Some(item.clone());
                    }
                    mon.item = Unknown::Known(Item::None);
                }
            }
        }

        EventKind::AbilityRevealed { slot, ability } => {
            if let Some(idx) = mon_idx_for_active_slot(state, slot) {
                if let Some(mon) = get_mon_mut_by_idx(state, idx) {
                    // Narrow-vs-overwrite: if the revealed ability is already
                    // in the candidate set (or the set is wide `Not`), narrow.
                    // If it is outside a `Possibly` set, a live ability-change
                    // (Trace, Skill Swap, Mummy, etc.) occurred — overwrite the
                    // live ability to `Known` without treating it as a
                    // contradiction. `possible_original_abilities` is untouched.
                    let outside_possibly = matches!(
                        &mon.possible_abilities,
                        Unknown::Possibly(v) if !v.contains(ability)
                    );
                    if outside_possibly {
                        mon.possible_abilities = Unknown::Known(ability.clone());
                    } else {
                        unknown_set_known(
                            &mut mon.possible_abilities,
                            ability.clone(),
                            &format!("mon#{idx} ability"),
                        );
                    }
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
                    // Update types and ability set from the mega species dex entry.
                    if let Some(data) = ctx.dex.get(into) {
                        mon.possible_types = Unknown::Known(data.types.clone());
                        // Mega abilities are fixed per mega species — recompute both
                        // original and live ability to the mega's slot set.
                        let mega_abilities = if data.abilities.is_empty() {
                            Unknown::Not(Vec::new())
                        } else {
                            Unknown::Possibly(data.abilities.clone())
                        };
                        mon.possible_original_abilities = mega_abilities.clone();
                        mon.possible_abilities = mega_abilities;
                    }
                }
                match slot.player {
                    // p1_has_mega / p2_has_mega means "resource still available" —
                    // initialized true, flipped to false when the Mega is used.
                    Player::P1 => state.p1_has_mega = false,
                    Player::P2 => state.p2_has_mega = false,
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
                        // Forme-change abilities are fixed per forme — recompute ability
                        // sets to the new forme's slot set.
                        let forme_abilities = if data.abilities.is_empty() {
                            Unknown::Not(Vec::new())
                        } else {
                            Unknown::Possibly(data.abilities.clone())
                        };
                        mon.possible_original_abilities = forme_abilities.clone();
                        mon.possible_abilities = forme_abilities;
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
            state.weather_turns = weather.as_ref().map(|w| weather_timer(w));
            // I-A: record the setter so the rock item can be revealed when the timer
            // collapses from Possibly([5,8]) to Known(3) after 5 end-of-turns.
            state.weather_setter_mon_idx = if let Some(mctx) = &ctx.move_context {
                // Move-triggered weather (Rain Dance, Sunny Day, …) — setter is move user.
                mon_idx_for_active_slot(state, &mctx.user_slot)
            } else if let Some(sw_slot) = &ctx.switch_slot {
                // Ability-triggered weather on single-mon switch-in (Drizzle, Drought, …).
                mon_idx_for_active_slot(state, sw_slot)
            } else {
                None // SimultaneousSwitch or other; setter unknown.
            };
        }
        EventKind::TerrainChanged { terrain } => {
            state.terrain = terrain.clone();
            state.terrain_turns = terrain.as_ref().map(|t| terrain_timer(t));
            // I-A: record the setter for TerrainExtender reveal on timer collapse.
            state.terrain_setter_mon_idx = if let Some(mctx) = &ctx.move_context {
                mon_idx_for_active_slot(state, &mctx.user_slot)
            } else if let Some(sw_slot) = &ctx.switch_slot {
                mon_idx_for_active_slot(state, sw_slot)
            } else {
                None
            };
        }
        EventKind::PseudoWeatherStart { effect } => {
            if !state.pseudo_weathers.contains(effect) {
                state.pseudo_weathers.push(effect.clone());
                state
                    .pseudo_weather_turns
                    .push(pseudo_weather_timer(effect));
            }
        }
        EventKind::PseudoWeatherEnd { effect } => {
            if let Some(pos) = state.pseudo_weathers.iter().position(|e| e == effect) {
                state.pseudo_weathers.remove(pos);
                state.pseudo_weather_turns.remove(pos);
            }
        }
        EventKind::SideConditionStart { side, condition } => {
            // Determine the setter mon_idx for I-A screen reveals.
            let setter_idx = if let Some(mctx) = &ctx.move_context {
                // Screens are only set by moves; move_context is always available here.
                mon_idx_for_active_slot(state, &mctx.user_slot)
            } else {
                None
            };
            let (conditions, turns, setters) = match side {
                Player::P1 => (
                    &mut state.p1_side_conditions,
                    &mut state.p1_side_condition_turns,
                    &mut state.p1_side_condition_setters,
                ),
                Player::P2 => (
                    &mut state.p2_side_conditions,
                    &mut state.p2_side_condition_turns,
                    &mut state.p2_side_condition_setters,
                ),
            };
            if !conditions.contains(condition) {
                conditions.push(condition.clone());
                turns.push(side_condition_timer(condition));
                setters.push(setter_idx);
            }
        }
        EventKind::SideConditionEnd { side, condition } => {
            let (conditions, turns, setters) = match side {
                Player::P1 => (
                    &mut state.p1_side_conditions,
                    &mut state.p1_side_condition_turns,
                    &mut state.p1_side_condition_setters,
                ),
                Player::P2 => (
                    &mut state.p2_side_conditions,
                    &mut state.p2_side_condition_turns,
                    &mut state.p2_side_condition_setters,
                ),
            };
            if let Some(pos) = conditions.iter().position(|c| c == condition) {
                conditions.remove(pos);
                turns.remove(pos);
                if pos < setters.len() {
                    setters.remove(pos);
                }
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
        // Clear all stat boosts (mirrors simulator clear_pokemon_for_switch_out:6206).
        mon.boosts.iter_mut().for_each(|b| *b = 0);
        // Clear all volatile statuses (mirrors simulator:6205).
        mon.volatiles.clear();
        // Reset ToxicPoison tier to 0 on switch-out (mirrors simulator:6213-6214).
        if matches!(mon.status, Some(Status::ToxicPoison(_))) {
            mon.status = Some(Status::ToxicPoison(0));
        }
        // Entry / field flags that don't persist on the bench.
        mon.entered_this_turn = false;
        mon.first_move_on_field = false;
        mon.first_turn_on_field_pending = false;
        mon.cud_chew_pending = None;
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
        // Live ability resets to the innate ability set on switch-out.
        // Trace / Skill Swap / Mummy / etc. do not persist across a switch.
        mon.possible_abilities = mon.possible_original_abilities.clone();
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
/// Return the item that extends a weather effect beyond its base 5-turn duration.
/// Used by I-A to reveal the rock item when the weather timer confirms the 8-turn branch.
fn weather_extension_item(weather: &Weather) -> Option<Item> {
    match weather {
        Weather::Rain => Some(Item::DampRock),
        Weather::Sun => Some(Item::HeatRock),
        Weather::Sandstorm => Some(Item::SmoothRock),
        Weather::Snow => Some(Item::IcyRock),
        // Primordial weathers: timer is Known(0) sentinel; never emitted.
        Weather::HeavyRain | Weather::ExtremeSunlight | Weather::StrongWinds => None,
    }
}

/// Return the item that extends a terrain effect beyond its base 5-turn duration.
fn terrain_extension_item(_terrain: &Terrain) -> Item {
    Item::TerrainExtender
}

/// Return the item that extends a screen beyond its base 5-turn duration.
fn screen_extension_item(sc: &SideCondition) -> Option<Item> {
    match sc {
        SideCondition::Reflect | SideCondition::LightScreen | SideCondition::AuroraVeil => {
            Some(Item::LightClay)
        }
        _ => None,
    }
}

/// Emit `HasItem{mon_idx, item}` as a `Known` fact when a timer has just
/// collapsed from `Possibly` to `Known(n > 0)`, confirming the extended duration.
/// This is a pure narrowing (soundness: only fires when the longer branch is
/// the ONLY remaining candidate) and is always guaranteed information.
fn emit_extension_item_if_collapsed(
    state: &mut UnknownBattleState,
    was_possibly: bool,
    setter_idx: Option<usize>,
    item: Item,
) {
    if !was_possibly {
        return; // Not a collapse; timer was already Known or was Never decremented.
    }
    // After decrement, if a Possibly collapsed to Known it will now be Known(n).
    // We don't re-check the value here — the caller verifies n > 0.
    if let Some(idx) = setter_idx {
        if let Some(mon) = get_mon_mut_by_idx(state, idx) {
            unknown_set_known(&mut mon.item, item, "ia-extension-item");
        }
    }
}

fn apply_end_of_turn(state: &mut UnknownBattleState) {
    // ── Decrement field timers and detect Possibly→Known collapses (I-A) ─────

    // Weather
    let weather_was_possibly = matches!(&state.weather_turns, Some(Unknown::Possibly(_)));
    let weather_setter = state.weather_setter_mon_idx;
    let weather_type_snap = state.weather.clone();
    decrement_unknown_turns(&mut state.weather_turns, &mut state.weather);
    if weather_was_possibly {
        if let Some(Unknown::Known(n)) = &state.weather_turns {
            if *n > 0 {
                // Extended duration confirmed (8-turn branch survived the 5-turn filter).
                if let Some(weather) = &weather_type_snap {
                    if let Some(rock) = weather_extension_item(weather) {
                        emit_extension_item_if_collapsed(state, true, weather_setter, rock);
                    }
                }
            }
        }
    }

    // Terrain
    let terrain_was_possibly = matches!(&state.terrain_turns, Some(Unknown::Possibly(_)));
    let terrain_setter = state.terrain_setter_mon_idx;
    decrement_unknown_turns(&mut state.terrain_turns, &mut state.terrain);
    if terrain_was_possibly {
        if let Some(Unknown::Known(n)) = &state.terrain_turns {
            if *n > 0 {
                emit_extension_item_if_collapsed(
                    state,
                    true,
                    terrain_setter,
                    terrain_extension_item(&Terrain::ElectricTerrain), // any Terrain gives TerrainExtender
                );
            }
        }
    }

    for t in state.pseudo_weather_turns.iter_mut() {
        decrement_unknown_turns_raw(t);
    }
    // Remove expired pseudo-weathers (those whose turn set collapsed to empty).
    // (We don't know which pseudo-weather expired — leave for event-driven clearing.)

    // P1 side conditions
    for i in 0..state.p1_side_conditions.len() {
        let was_possibly = matches!(&state.p1_side_condition_turns[i], Unknown::Possibly(_));
        decrement_unknown_turns_raw(&mut state.p1_side_condition_turns[i]);
        if was_possibly {
            if let Unknown::Known(n) = &state.p1_side_condition_turns[i] {
                if *n > 0 {
                    let sc = state.p1_side_conditions[i].clone();
                    if let Some(clay) = screen_extension_item(&sc) {
                        let setter = state.p1_side_condition_setters.get(i).copied().flatten();
                        emit_extension_item_if_collapsed(state, true, setter, clay);
                    }
                }
            }
        }
    }
    // P2 side conditions
    for i in 0..state.p2_side_conditions.len() {
        let was_possibly = matches!(&state.p2_side_condition_turns[i], Unknown::Possibly(_));
        decrement_unknown_turns_raw(&mut state.p2_side_condition_turns[i]);
        if was_possibly {
            if let Unknown::Known(n) = &state.p2_side_condition_turns[i] {
                if *n > 0 {
                    let sc = state.p2_side_conditions[i].clone();
                    if let Some(clay) = screen_extension_item(&sc) {
                        let setter = state.p2_side_condition_setters.get(i).copied().flatten();
                        emit_extension_item_if_collapsed(state, true, setter, clay);
                    }
                }
            }
        }
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

// ── Per-effect timer models ───────────────────────────────────────────────────
//
// Soundness requirement: the candidate set must be a *superset* of every true
// duration the game can produce.  Where an item can extend the duration (weather
// rocks, Light Clay, Terrain Extender) we use `Possibly([5,8])`; where the
// duration is fixed by mechanic (not by item), we use `Known(n)`.
//
// `Known(0)` is the "permanent / no countdown" sentinel used for primordial
// weathers and entry hazards.  `decrement_unknown_turns_raw` is a no-op on 0,
// and the predicate machinery never emits turn-count clauses for these effects.
//
// Durations confirmed against Bulbapedia (newest-generation behaviour):
//   Tailwind=4 (Gen V+), Screens=5/8 (Light Clay), Tricks/Gravity/WR=5,
//   Safeguard/Mist/Lucky Chant=5, one-turn guards=1, FairyLock/IonDeluge=1,
//   Mud/Water Sport=5, MagicDeluge(Magic Room)=5.

/// Timer model for a newly-set weather effect.
fn weather_timer(w: &Weather) -> Unknown<u8> {
    match w {
        // Standard weathers: base 5; Heat/Damp/Smooth/Icy Rock extends to 8.
        Weather::Rain | Weather::Sun | Weather::Sandstorm | Weather::Snow => {
            Unknown::Possibly(vec![5, 8])
        }
        // Primordial weathers (from Abilities): never tick down.
        Weather::HeavyRain | Weather::ExtremeSunlight | Weather::StrongWinds => {
            Unknown::Known(0)
        }
    }
}

/// Timer model for a newly-set terrain effect.
fn terrain_timer(_t: &Terrain) -> Unknown<u8> {
    // All terrains: base 5; Terrain Extender extends to 8.
    Unknown::Possibly(vec![5, 8])
}

/// Timer model for a newly-active pseudo-weather effect.
fn pseudo_weather_timer(pw: &PseudoWeather) -> Unknown<u8> {
    match pw {
        // All 5-turn pseudo-weathers (no item extension exists).
        PseudoWeather::TrickRoom
        | PseudoWeather::Gravity
        | PseudoWeather::WonderRoom
        | PseudoWeather::MudSport
        | PseudoWeather::WaterSport
        | PseudoWeather::MagicDeluge => Unknown::Known(5),
        // One-turn effects.
        PseudoWeather::FairyLock | PseudoWeather::IonDeluge => Unknown::Known(1),
    }
}

/// Timer model for a newly-active side condition.
fn side_condition_timer(sc: &SideCondition) -> Unknown<u8> {
    match sc {
        // Screens: base 5; Light Clay extends to 8.
        SideCondition::Reflect
        | SideCondition::LightScreen
        | SideCondition::AuroraVeil => Unknown::Possibly(vec![5, 8]),
        // Tailwind: exactly 4 turns (Gen V+).  Formerly Possibly([5,8]) — UNSOUND.
        SideCondition::TailWind => Unknown::Known(4),
        // Fixed 5-turn side conditions.
        SideCondition::SafeGuard
        | SideCondition::Mist
        | SideCondition::LuckyChant => Unknown::Known(5),
        // One-turn protections (expire at end of the turn they are used).
        SideCondition::QuickGuard
        | SideCondition::WideGuard
        | SideCondition::CraftyShield
        | SideCondition::MatBlock => Unknown::Known(1),
        // Entry hazards: permanent until cleared (no countdown).
        SideCondition::Spikes(_)
        | SideCondition::StealthRock
        | SideCondition::StickyWeb(_)
        | SideCondition::ToxicSpikes(_) => Unknown::Known(0),
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
        // LO recoil only fires when the move actually deals HP damage to at least one
        // target (not when all targets miss, are immune, or are behind a Substitute).
        // If no opponent took HP damage, the absence of LO recoil is uninformative —
        // excluding LO based on it would be unsound.
        let hit_any_opponent = targets.iter().any(|t| {
            event.reactions.iter().any(|r| {
                matches!(&r.kind, EventKind::DamageDealt { target, .. } if target == t)
            })
        });

        let has_lo_recoil = event
            .reactions
            .iter()
            .any(|r| matches!(&r.kind, EventKind::DamageDealt { target, .. } if target == user));

        if hit_any_opponent {
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
        } // end hit_any_opponent
    } // end is_damaging

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

// ── Pass 2b: Contact-reaction absence inference ───────────────────────────────

/// Infer the *absence* of always-on contact-reactive items/abilities on the
/// defender when a contact move hit but produced no such reaction in its nested
/// event tree.
///
/// **Rocky Helmet** (1/6 chip to attacker) and **Rough Skin / Iron Barbs** (1/8
/// chip to attacker) are unconditional on contact — they always produce an
/// `ItemRevealed` / `AbilityRevealed` nested under the `DamageDealt` reaction.
/// If no such reveal appeared and no attacker-side escape is possible, we can
/// definitively exclude those from the defender.
///
/// Presence of these items/abilities is handled by the nested-reveal convention
/// (Pass 1 `ItemRevealed`/`AbilityRevealed`) and needs no inference here.
fn pass2_contact_absence(
    state: &mut UnknownBattleState,
    event: &InformationEvent,
    ctx: &BattleContext,
) {
    let EventKind::MoveUsed { user, targets, move_used } = &event.kind else {
        return;
    };
    let Some(move_data) = ctx.move_dex.get(move_used) else {
        return;
    };

    // Only contact moves trigger contact reactions.
    if !move_has_flag(move_data, &MoveFlag::Contact) {
        return;
    }

    // --- Attacker-side escape checks (if any apply, skip — sound: wider) ---
    let attacker_idx = mon_idx_for_active_slot(state, user);
    let attacker_escapes = {
        let am = attacker_idx.and_then(|i| get_mon_by_idx(state, i));
        // Long Reach (attacker ability) makes the move non-contact.
        let might_be_long_reach = am.map_or(false, |m| {
            !unknown_is_excluded(&m.possible_abilities, &Ability::LongReach)
        });
        // Protective Pads (attacker item) prevents contact-triggered effects.
        let might_have_pads = am.map_or(false, |m| {
            !unknown_is_excluded(&m.item, &Item::ProtectivePads)
        });
        // Magic Guard prevents Rocky Helmet chip to the attacker, but NOT Rough
        // Skin/Iron Barbs chip.  We track Magic Guard only for the Helmet check.
        let might_have_magic_guard = am.map_or(false, |m| {
            !unknown_is_excluded(&m.possible_abilities, &Ability::MagicGuard)
        });
        (might_be_long_reach, might_have_pads, might_have_magic_guard)
    };
    let (long_reach_possible, pads_possible, magic_guard_possible) = attacker_escapes;

    // If either Long Reach or Protective Pads is possible, no contact reaction is
    // guaranteed — skip all exclusions (sound).
    if long_reach_possible || pads_possible {
        return;
    }

    for target in targets {
        let Some(target_idx) = mon_idx_for_active_slot(state, target) else {
            continue;
        };

        // Was the hit actually landed? (Check that a DamageDealt reaction exists for
        // the defender — if the move missed / was blocked, no contact reaction fires.)
        let hit_landed = event.reactions.iter().any(|r| {
            matches!(&r.kind, EventKind::DamageDealt { target: t, .. } if t == target)
        });
        if !hit_landed {
            continue;
        }

        // Did a Rocky Helmet reveal appear? (nested under DamageDealt or directly.)
        let helmet_revealed = reaction_contains_item_reveal(event, target, &Item::RockyHelmet);

        // Did a Rough Skin / Iron Barbs reveal appear?
        let rough_skin_revealed =
            reaction_contains_ability_reveal(event, target, &Ability::RoughSkin);
        let iron_barbs_revealed =
            reaction_contains_ability_reveal(event, target, &Ability::IronBarbs);

        let Some(mon) = get_mon_mut_by_idx(state, target_idx) else {
            continue;
        };

        // Rocky Helmet: Magic Guard on the attacker prevents the chip, so Helmet
        // absence is only certain when Magic Guard is also excluded.
        if !helmet_revealed && !magic_guard_possible {
            unknown_exclude(&mut mon.item, &Item::RockyHelmet, "no-helmet-chip");
        }

        // Rough Skin / Iron Barbs: Magic Guard does NOT prevent these (they deal
        // damage directly), so absence is unconditional once contact/pads are checked.
        if !rough_skin_revealed {
            unknown_exclude(
                &mut mon.possible_abilities,
                &Ability::RoughSkin,
                "no-rough-skin-chip",
            );
        }
        if !iron_barbs_revealed {
            unknown_exclude(
                &mut mon.possible_abilities,
                &Ability::IronBarbs,
                "no-iron-barbs-chip",
            );
        }
    }
}

/// Recursively scan a `MoveUsed` event (and its nested reactions) for an
/// `ItemRevealed` event naming `item` on the given `slot`.
fn reaction_contains_item_reveal(
    event: &InformationEvent,
    slot: &FieldSlot,
    item: &Item,
) -> bool {
    for r in &event.reactions {
        if matches!(&r.kind, EventKind::ItemRevealed { slot: s, item: i } if s == slot && i == item)
        {
            return true;
        }
        if reaction_contains_item_reveal(r, slot, item) {
            return true;
        }
    }
    false
}

/// Recursively scan for an `AbilityRevealed` event naming `ability` on `slot`.
fn reaction_contains_ability_reveal(
    event: &InformationEvent,
    slot: &FieldSlot,
    ability: &Ability,
) -> bool {
    for r in &event.reactions {
        if matches!(&r.kind, EventKind::AbilityRevealed { slot: s, ability: a } if s == slot && a == ability)
        {
            return true;
        }
        if reaction_contains_ability_reveal(r, slot, ability) {
            return true;
        }
    }
    false
}

// ── Pass 2c: Prankster-immunity reveal ────────────────────────────────────────

/// If the opponent used a **Status-category** move that targeted one of our
/// Pokémon and the reaction includes `Immune`/`MoveFailed`/`Blocked` — while the
/// target is Dark-type and the move is Status-category — the Dark-type immunity
/// to Prankster-boosted moves is the only sound explanation.  Emit
/// `[HasAbility(Prankster)]` on the user; BCP will force it to `Known`.
///
/// Sound: we only infer when the normal move would have applied (no other immunity
/// reason), so the Dark immunity implies the priority boost which implies Prankster.
fn pass2_prankster_immunity(
    state: &mut UnknownBattleState,
    event: &InformationEvent,
    ctx: &BattleContext,
) {
    let EventKind::MoveUsed { user, targets, move_used } = &event.kind else {
        return;
    };
    let Some(move_data) = ctx.move_dex.get(move_used) else {
        return;
    };
    // Only Status-category moves get the Prankster +1.
    if move_data.category != MoveCategory::Status {
        return;
    }

    let Some(user_idx) = mon_idx_for_active_slot(state, user) else {
        return;
    };
    let user_mon = get_mon_by_idx(state, user_idx);

    // Already know the ability — no need to infer.
    if user_mon.map_or(false, |m| matches!(&m.possible_abilities, Unknown::Known(_))) {
        return;
    }
    // Prankster already excluded — no clause needed.
    if user_mon.map_or(false, |m| {
        unknown_is_excluded(&m.possible_abilities, &Ability::Prankster)
    }) {
        return;
    }

    for target in targets {
        // Check if this target is Dark-type (known types on our side).
        let target_idx = mon_idx_for_active_slot(state, target);
        let is_dark = target_idx
            .and_then(|i| get_mon_by_idx(state, i))
            .map(|m| matches!(&m.possible_types, Unknown::Known(ts) if ts.contains(&PokemonType::Dark)))
            .unwrap_or(false);
        if !is_dark {
            continue;
        }

        // Check that the reaction includes Immune/MoveFailed/Blocked for this target.
        let failed_on_target = event.reactions.iter().any(|r| {
            matches!(
                &r.kind,
                EventKind::Immune { target: t } | EventKind::MoveFailed { slot: t } | EventKind::Blocked { target: t }
                if t == target
            )
        });
        if !failed_on_target {
            continue;
        }

        // Emit a unit clause (or near-unit after BCP) — Prankster is the only explanation.
        state.predicates.push(vec![Statement::HasAbility {
            mon_idx: user_idx,
            ability: Ability::Prankster,
        }]);
        // Only need to emit once per user (the clause is user-specific).
        return;
    }
}

// ── Pass 2d: Powder-move immunity reveal ──────────────────────────────────────

/// When a move with `MoveFlag::Powder` targets a **non-Grass** Pokémon and
/// results in `Immune`/`MoveFailed`/`Blocked`, the only non-type-immunity
/// explanation is Safety Goggles or Overcoat on the target.
fn pass2_powder_immunity(
    state: &mut UnknownBattleState,
    event: &InformationEvent,
    ctx: &BattleContext,
) {
    let EventKind::MoveUsed { targets, move_used, .. } = &event.kind else {
        return;
    };
    let Some(move_data) = ctx.move_dex.get(move_used) else {
        return;
    };
    if !move_has_flag(move_data, &MoveFlag::Powder) {
        return;
    }

    for target in targets {
        let Some(target_idx) = mon_idx_for_active_slot(state, target) else {
            continue;
        };
        let target_mon = get_mon_by_idx(state, target_idx);

        // Grass types are inherently immune — no item/ability inference.
        let is_grass = target_mon
            .map(|m| matches!(&m.possible_types, Unknown::Known(ts) if ts.contains(&PokemonType::Grass)))
            .unwrap_or(false);
        if is_grass {
            continue;
        }

        // Did the move fail/be immune on this target?
        let failed = event.reactions.iter().any(|r| {
            matches!(
                &r.kind,
                EventKind::Immune { target: t } | EventKind::MoveFailed { slot: t } | EventKind::Blocked { target: t }
                if t == target
            )
        });
        if !failed {
            continue;
        }

        let tm = get_mon_by_idx(state, target_idx);
        let mut clause: Vec<Statement> = Vec::new();
        let legal_ok = |item: &Item| {
            ctx.config.legal_items.as_ref().map_or(true, |l| l.contains(item))
        };
        if legal_ok(&Item::SafetyGoggles)
            && tm.map_or(true, |m| !unknown_is_excluded(&m.item, &Item::SafetyGoggles))
        {
            clause.push(Statement::HasItem { mon_idx: target_idx, item: Item::SafetyGoggles });
        }
        if tm.map_or(true, |m| !unknown_is_excluded(&m.possible_abilities, &Ability::Overcoat)) {
            clause.push(Statement::HasAbility { mon_idx: target_idx, ability: Ability::Overcoat });
        }
        if !clause.is_empty() {
            state.predicates.push(clause);
        }
    }
}

// ── Pass 2e: Guaranteed-status absence reveals ────────────────────────────────

/// When a move **hit** the target (a `DamageDealt` or a Status-category move with
/// no `Missed` reaction) and carries a **guaranteed status** (`chance == 100`,
/// `effect.status == Some(s)`, empty `random_choices`) yet produces **no**
/// `StatusInflicted{target}`, emit a disjunction of the unknown status-prevention
/// abilities/items on the target.
///
/// Only fires when all *decidable* preventers have been ruled out (type immunity,
/// already-statused, Substitute, Safeguard, terrain).
fn pass2_guaranteed_status_absence(
    state: &mut UnknownBattleState,
    event: &InformationEvent,
    ctx: &BattleContext,
) {
    let EventKind::MoveUsed { user: _, targets, move_used } = &event.kind else {
        return;
    };
    let Some(move_data) = ctx.move_dex.get(move_used) else {
        return;
    };

    // Find all guaranteed statuses in this move's secondaries.
    // A "guaranteed status secondary" has chance==100, one status, and no random choices.
    let is_damaging = matches!(move_data.category, MoveCategory::Physical | MoveCategory::Special);
    let guaranteed_statuses: Vec<(Status, bool)> = move_data
        .secondaries
        .iter()
        .filter(|s| {
            s.chance == 100
                && s.effect.status.is_some()
                && s.random_choices.is_empty()
        })
        .map(|s| (s.effect.status.clone().unwrap(), is_damaging))
        .collect();

    if guaranteed_statuses.is_empty() {
        return;
    }

    for target in targets {
        let Some(target_idx) = mon_idx_for_active_slot(state, target) else {
            continue;
        };

        // Did the move actually hit? (Missed / Blocked / Immune = no status applies.)
        let hit = if is_damaging {
            event.reactions.iter().any(|r| {
                matches!(&r.kind, EventKind::DamageDealt { target: t, .. } if t == target)
            })
        } else {
            // Status-category moves: no `Missed` and no `MoveFailed`/`Immune`.
            !event.reactions.iter().any(|r| {
                matches!(
                    &r.kind,
                    EventKind::Missed { target: t }
                    | EventKind::Immune { target: t }
                    | EventKind::MoveFailed { slot: t }
                    | EventKind::Blocked { target: t }
                    if t == target
                )
            })
        };
        if !hit {
            continue;
        }

        // Was a status inflicted (on this target)?
        let status_inflicted = event.reactions.iter().any(|r| {
            matches!(&r.kind, EventKind::StatusInflicted { target: t, .. } if t == target)
        });
        if status_inflicted {
            continue; // Status did land — nothing to infer.
        }

        // Extract all data from target_mon into owned copies so we can later
        // push to state.predicates without a live immutable borrow of state.
        let (already_statused, has_sub, known_types, tm_item, tm_abilities) = {
            let tm = get_mon_by_idx(state, target_idx);
            let already_statused = tm.map_or(false, |m| m.status.is_some());
            let has_sub = tm.map_or(false, |m| {
                m.volatiles.iter().any(|v| matches!(
                    v,
                    VolatileStatusState::TurnStatus(VolatileStatus::Substitute(_), _)
                    | VolatileStatusState::MoveStatus(VolatileStatus::Substitute(_), _)
                ))
            });
            let known_types = tm.and_then(|m| {
                if let Unknown::Known(ts) = &m.possible_types { Some(ts.clone()) } else { None }
            });
            let tm_item = tm.map(|m| m.item.clone());
            let tm_abilities = tm.map(|m| m.possible_abilities.clone());
            (already_statused, has_sub, known_types, tm_item, tm_abilities)
        };

        // Already statused prevents the secondary from applying.
        if already_statused {
            continue;
        }

        // Has Substitute?
        if has_sub {
            continue;
        }

        // SafeGuard on the target's side?
        let has_safeguard = {
            let is_p2 = mon_is_p2(state, target_idx);
            if is_p2 {
                state.p2_side_conditions.contains(&SideCondition::SafeGuard)
            } else {
                state.p1_side_conditions.contains(&SideCondition::SafeGuard)
            }
        };
        if has_safeguard {
            continue;
        }

        // LeafGuard only prevents status under harsh sun — snapshot weather now.
        let is_sun = matches!(state.weather, Some(Weather::Sun) | Some(Weather::ExtremeSunlight));

        let mut pending_clauses: Vec<Vec<Statement>> = Vec::new();

        for (status, from_secondary) in &guaranteed_statuses {
            // Type-immune check (known types only — absent knowledge is not immunity).
            let type_immune = match status {
                Status::Burn => known_types.as_ref().map_or(false, |ts| ts.contains(&PokemonType::Fire)),
                Status::Paralysis => known_types.as_ref().map_or(false, |ts| {
                    // Electric-type: unconditional paralysis immunity (Gen VI+).
                    // Ground-type: only immune to Electric-type paralysis moves (Thunder Wave,
                    // Nuzzle, Zap Cannon); Body Slam etc. CAN paralyze Ground-types.
                    ts.contains(&PokemonType::Electric)
                        || (ts.contains(&PokemonType::Ground)
                            && move_data.pokemon_type == PokemonType::Electric)
                }),
                Status::Poison | Status::ToxicPoison(_) => known_types.as_ref().map_or(false, |ts| {
                    ts.contains(&PokemonType::Poison) || ts.contains(&PokemonType::Steel)
                }),
                Status::Frozen(_) => known_types.as_ref().map_or(false, |ts| ts.contains(&PokemonType::Ice)),
                Status::Sleep(_) => false, // No blanket type immunity to sleep
            };
            if type_immune {
                continue;
            }

            // Terrain immunity (treat all mons as grounded for sound approximation).
            // Misty Terrain: mons immune to all status.
            // Electric Terrain: mons immune to sleep.
            let terrain_immune = match status {
                Status::Sleep(_) => state.terrain == Some(Terrain::MistyTerrain)
                    || state.terrain == Some(Terrain::ElectricTerrain),
                _ => state.terrain == Some(Terrain::MistyTerrain),
            };
            if terrain_immune {
                continue;
            }

            // Freeze in harsh sunlight: blanket immunity regardless of ability or type.
            // "Pokémon cannot be frozen when harsh sunlight is active." — Bulbapedia.
            // The absence is fully explained by weather, so emitting an ability clause
            // would be unsound (it could force-exclude a valid item/ability config).
            if matches!(status, Status::Frozen(_)) && is_sun {
                continue;
            }

            let mut clause: Vec<Statement> = Vec::new();

            // Covert Cloak: blocks secondary effects of damaging moves.
            let legal_ok = |item: &Item| {
                ctx.config.legal_items.as_ref().map_or(true, |l| l.contains(item))
            };
            let item_excluded_cc = tm_item.as_ref()
                .map_or(false, |it| unknown_is_excluded(it, &Item::CovertCloak));
            if *from_secondary && legal_ok(&Item::CovertCloak) && !item_excluded_cc {
                clause.push(Statement::HasItem { mon_idx: target_idx, item: Item::CovertCloak });
            }

            // Shield Dust: blocks the additional effects of damaging moves (same scope as
            // Covert Cloak — secondary effects of damaging moves only).  Shield Dust is
            // an Ignorable ability (Mold Breaker bypasses), but including it as a disjunct
            // is sound regardless.
            if *from_secondary {
                let sd_excluded = tm_abilities.as_ref()
                    .map_or(false, |pa| unknown_is_excluded(pa, &Ability::ShieldDust));
                if !sd_excluded {
                    clause.push(Statement::HasAbility {
                        mon_idx: target_idx,
                        ability: Ability::ShieldDust,
                    });
                }
            }

            // Per-status prevention abilities.
            let preventer_abilities: Vec<Ability> = match status {
                Status::Burn => vec![
                    Ability::WaterVeil,
                    Ability::WaterBubble,
                    Ability::ThermalExchange,
                    Ability::Comatose,
                    Ability::PurifyingSalt,
                    Ability::ShieldsDown,
                    Ability::LeafGuard,
                    Ability::FlowerVeil,
                ],
                Status::Paralysis => vec![
                    Ability::Limber,
                    Ability::Comatose,
                    Ability::PurifyingSalt,
                    Ability::ShieldsDown,
                    Ability::LeafGuard,
                    Ability::FlowerVeil,
                ],
                Status::Poison | Status::ToxicPoison(_) => vec![
                    Ability::Immunity,
                    Ability::PastelVeil,
                    Ability::Comatose,
                    Ability::PurifyingSalt,
                    Ability::ShieldsDown,
                    Ability::LeafGuard, // all non-volatile statuses under harsh sun
                    Ability::FlowerVeil,
                ],
                Status::Sleep(_) => vec![
                    Ability::Insomnia,
                    Ability::VitalSpirit,
                    Ability::SweetVeil,
                    Ability::Comatose,
                    Ability::PurifyingSalt,
                    Ability::ShieldsDown,
                    Ability::LeafGuard,
                    Ability::FlowerVeil,
                ],
                Status::Frozen(_) => vec![
                    Ability::MagmaArmor,
                    Ability::Comatose,
                    Ability::PurifyingSalt,
                    Ability::ShieldsDown,
                    Ability::LeafGuard, // all non-volatile statuses under harsh sun
                    Ability::FlowerVeil, // all non-volatile statuses for Grass-type holders
                ],
            };

            for ab in &preventer_abilities {
                if *ab == Ability::LeafGuard && !is_sun {
                    continue;
                }
                let ab_excluded = tm_abilities.as_ref()
                    .map_or(false, |pa| unknown_is_excluded(pa, ab));
                if !ab_excluded {
                    clause.push(Statement::HasAbility { mon_idx: target_idx, ability: ab.clone() });
                }
            }

            if !clause.is_empty() {
                pending_clauses.push(clause);
            }
        }

        for clause in pending_clauses {
            state.predicates.push(clause);
        }
    }
}

// ── Pass 2f: EOT healing reveals (Leftovers / Black Sludge) ──────────────────

/// When an opponent's Pokémon heals at end-of-turn and the cause is not
/// attributable to a known source (Aqua Ring, Ingrain, Grassy Terrain, Wish,
/// or Leech Seed draining our mon), infer Leftovers (or Leftovers ∨ Black
/// Sludge for Poison types).
fn pass_eot_heal(
    state: &mut UnknownBattleState,
    event: &InformationEvent, // must be EndOfTurn
    ctx: &BattleContext,
) {
    // Collect (target_idx, target FieldSlot) for all opponent heals in top-level reactions.
    // Gather data into owned values first to avoid holding state borrows during push.
    let legal_ok = |item: &Item| {
        ctx.config.legal_items.as_ref().map_or(true, |l| l.contains(item))
    };

    // If our own active mon has LeechSeed, the opponent may be getting a Leech Seed heal.
    // This is a conservative skip — if uncertain, don't infer Leftovers.
    let our_mon_is_seeded = state.p1_active_mons.iter().any(|m| {
        m.volatiles.iter().any(|v| {
            matches!(v, VolatileStatusState::TurnStatus(VolatileStatus::LeechSeed, _))
        })
    });

    let is_grassy = state.terrain == Some(Terrain::GrassyTerrain);

    let mut pending_clauses: Vec<Vec<Statement>> = Vec::new();

    for reaction in &event.reactions {
        let EventKind::Healed { target, .. } = &reaction.kind else {
            continue;
        };
        // Only infer from opponent heals (p2 from our perspective).
        if target.player != crate::state::battle::Player::P2 {
            continue;
        }

        let Some(target_idx) = mon_idx_for_active_slot(state, target) else {
            continue;
        };

        // Extract needed state into owned copies before any mutable borrow.
        let (tm_item, tm_abilities, known_types, has_aqua_ring, has_ingrain, has_wish) = {
            let tm = get_mon_by_idx(state, target_idx);
            let tm_item = tm.map(|m| m.item.clone());
            let tm_abilities = tm.map(|m| m.possible_abilities.clone());
            let known_types = tm.and_then(|m| {
                if let Unknown::Known(ts) = &m.possible_types { Some(ts.clone()) } else { None }
            });
            let has_aqua_ring = tm.map_or(false, |m| {
                m.volatiles.iter().any(|v| {
                    matches!(v, VolatileStatusState::TurnStatus(VolatileStatus::AquaRing, _))
                })
            });
            let has_ingrain = tm.map_or(false, |m| {
                m.volatiles.iter().any(|v| {
                    matches!(v, VolatileStatusState::TurnStatus(VolatileStatus::Ingrain, _))
                })
            });
            let has_wish = state
                .p2_slot_conditions
                .get(target.slot_index as usize)
                .map_or(false, |conds| {
                    conds.iter().any(|c| matches!(c, SlotCondition::Wish { .. }))
                });
            (tm_item, tm_abilities, known_types, has_aqua_ring, has_ingrain, has_wish)
        };

        // Skip if a decidable EOT-heal source explains the heal.
        if has_aqua_ring || has_ingrain || is_grassy || has_wish || our_mon_is_seeded {
            continue;
        }

        // Skip if the item is already known.
        if tm_item.as_ref().map_or(false, |it| matches!(it, Unknown::Known(_))) {
            continue;
        }

        let is_poison = known_types
            .as_ref()
            .map_or(false, |ts| ts.contains(&PokemonType::Poison));

        let mut clause: Vec<Statement> = Vec::new();

        if legal_ok(&Item::Leftovers)
            && tm_item
                .as_ref()
                .map_or(true, |it| !unknown_is_excluded(it, &Item::Leftovers))
        {
            clause.push(Statement::HasItem { mon_idx: target_idx, item: Item::Leftovers });
        }
        // Black Sludge heals Poison-types at the same rate; add to the disjunction.
        if is_poison
            && legal_ok(&Item::BlackSludge)
            && tm_item
                .as_ref()
                .map_or(true, |it| !unknown_is_excluded(it, &Item::BlackSludge))
            && tm_abilities
                .as_ref()
                .map_or(true, |_| true) // BlackSludge is unconditional on Poison types
        {
            clause.push(Statement::HasItem { mon_idx: target_idx, item: Item::BlackSludge });
        }

        if !clause.is_empty() {
            pending_clauses.push(clause);
        }
    }

    for clause in pending_clauses {
        state.predicates.push(clause);
    }
}

// ── Pass 2g: Sandstorm EOT chip absence → immunity reveal ─────────────────────

/// When Sandstorm is active and an opponent's non-Rock/Ground/Steel Pokémon takes
/// **no** EOT sand chip, emit a disjunction of the abilities/items that grant
/// sand immunity.
fn pass_eot_sand_immunity(
    state: &mut UnknownBattleState,
    event: &InformationEvent, // must be EndOfTurn
    ctx: &BattleContext,
) {
    if !matches!(state.weather, Some(Weather::Sandstorm)) {
        return;
    }

    let legal_ok = |item: &Item| {
        ctx.config.legal_items.as_ref().map_or(true, |l| l.contains(item))
    };

    // p2 active mons start after all p1 segments.
    let p2_active_start = state.p1_active_mons.len()
        + state.p1_known_back_mons.len()
        + state.p1_possible_back_mons.len();

    let p2_active_count = state.p2_active_mons.len();

    let mut pending_clauses: Vec<Vec<Statement>> = Vec::new();

    for slot_i in 0..p2_active_count {
        let mon_idx = p2_active_start + slot_i;
        let field_slot = FieldSlot {
            player: Player::P2,
            slot_index: slot_i as u8,
        };

        // Extract data into owned values to avoid borrow conflicts.
        let (known_types, tm_item, tm_abilities) = {
            let tm = get_mon_by_idx(state, mon_idx);
            let known_types = tm.and_then(|m| {
                if let Unknown::Known(ts) = &m.possible_types { Some(ts.clone()) } else { None }
            });
            let tm_item = tm.map(|m| m.item.clone());
            let tm_abilities = tm.map(|m| m.possible_abilities.clone());
            (known_types, tm_item, tm_abilities)
        };

        // Rock, Ground, Steel types are innately immune — no inference.
        let innately_immune = known_types.as_ref().map_or(false, |ts| {
            ts.contains(&PokemonType::Rock)
                || ts.contains(&PokemonType::Ground)
                || ts.contains(&PokemonType::Steel)
        });
        if innately_immune {
            continue;
        }

        // Did the mon take an EOT sand chip?
        let took_sand_chip = event.reactions.iter().any(|r| {
            matches!(&r.kind, EventKind::DamageDealt { target: t, .. } if t == &field_slot)
        });
        if took_sand_chip {
            continue;
        }

        let mut clause: Vec<Statement> = Vec::new();

        if legal_ok(&Item::SafetyGoggles)
            && tm_item
                .as_ref()
                .map_or(true, |it| !unknown_is_excluded(it, &Item::SafetyGoggles))
        {
            clause.push(Statement::HasItem { mon_idx, item: Item::SafetyGoggles });
        }

        for ab in &[
            Ability::SandVeil,
            Ability::SandRush,
            Ability::SandForce,
            Ability::Overcoat,
            Ability::MagicGuard,
        ] {
            let excluded = tm_abilities
                .as_ref()
                .map_or(false, |pa| unknown_is_excluded(pa, ab));
            if !excluded {
                clause.push(Statement::HasAbility { mon_idx, ability: ab.clone() });
            }
        }

        if !clause.is_empty() {
            pending_clauses.push(clause);
        }
    }

    for clause in pending_clauses {
        state.predicates.push(clause);
    }
}

// ── I2: EOT self-status orb reveal ────────────────────────────────────────────

/// When a Pokémon gets a **new** non-volatile status from an `EndOfTurn` event
/// (i.e. no prior status) and the only EOT source for that status is a held item,
/// reveal the item as `Known`:
///
/// * `StatusInflicted{Burn}` at EOT → `Known(FlameOrb)` — the only held item that
///   self-inflicts Burn at end of turn.
/// * `StatusInflicted{ToxicPoison}` at EOT → `Known(ToxicOrb)` — the only held
///   item that self-inflicts bad poison at end of turn.
///
/// Soundness guards:
///   1. The mon must have had **no status** before this EOT (status set by pass1
///      during recursive descent runs after this pass, so `mon.status` is pre-EOT).
///   2. Only infer for P2 mons (opponent — P1 item is already known).
///   3. Skip if the item is already `Known` (nothing to infer).
///   4. Skip if the inferred item is already excluded from the item's possibility set.
fn pass_eot_self_status(
    state: &mut UnknownBattleState,
    event: &InformationEvent, // must be EndOfTurn
    ctx: &BattleContext,
) {
    let legal_ok = |item: &Item| {
        ctx.config.legal_items.as_ref().map_or(true, |l| l.contains(item))
    };

    for reaction in &event.reactions {
        let (target_slot, status) = match &reaction.kind {
            EventKind::StatusInflicted { target, status } => (target, status),
            _ => continue,
        };

        // Only infer for opponents (P2).
        if target_slot.player != crate::state::battle::Player::P2 {
            continue;
        }

        let Some(target_idx) = mon_idx_for_active_slot(state, target_slot) else {
            continue;
        };

        // Extract needed state before any mutable borrow.
        let (mon_item, had_prior_status) = {
            let mon = get_mon_by_idx(state, target_idx);
            let item = mon.map(|m| m.item.clone()).unwrap_or(Unknown::Not(Vec::new()));
            // pass1 hasn't processed this StatusInflicted yet (recursive descent runs
            // after this pass for EndOfTurn reactions), so mon.status is the pre-EOT value.
            let had_status = mon.map_or(false, |m| m.status.is_some());
            (item, had_status)
        };

        // Guard: no pre-existing status (orbs only apply when the holder is healthy).
        if had_prior_status {
            continue;
        }

        // Guard: item already known — nothing to infer.
        if matches!(mon_item, Unknown::Known(_)) {
            continue;
        }

        let infer_item = match status {
            Status::Burn => Item::FlameOrb,
            Status::ToxicPoison(_) => Item::ToxicOrb,
            _ => continue, // Paralysis, Sleep, Freeze, plain Poison not caused by orbs.
        };

        if !legal_ok(&infer_item) {
            continue;
        }
        // Guard: the inferred item is not already excluded.
        if unknown_is_excluded(&mon_item, &infer_item) {
            continue;
        }

        if let Some(mon) = get_mon_mut_by_idx(state, target_idx) {
            unknown_set_known(
                &mut mon.item,
                infer_item,
                &format!("mon#{target_idx} eot-orb"),
            );
        }
    }
}

// ── I2: Ground-type immunity clause ───────────────────────────────────────────

/// When a Ground-type damaging move results in `Immune` on a P2 mon whose types
/// are **fully known** and do not include Flying, the immunity must come from a
/// held item or ability.  Emit a disjunctive CNF clause so BCP can force the
/// exact explanation once other facts narrow the candidate set:
///
///   `HasItem(AirBalloon) ∨ HasAbility(Levitate) ∨ HasAbility(Eelevate) ∨ HasAbility(EarthEater)`
///
/// Soundness guards:
///   * Only fire when types are `Known` and exclude Flying (unknown types → could
///     be Flying → not safe to emit this clause).
///   * Skip if the mon has Magnet Rise or Telekinesis volatile (Ground immunity
///     explained by field effect — no item/ability clause needed).
///   * Exclude disjuncts already impossible (item excluded / ability excluded).
fn pass2_ground_immune_clause(
    state: &mut UnknownBattleState,
    event: &InformationEvent,
    ctx: &BattleContext,
) {
    let EventKind::MoveUsed { targets, move_used, .. } = &event.kind else {
        return;
    };
    let Some(move_data) = ctx.move_dex.get(move_used) else {
        return;
    };
    // Only Ground-type damaging moves.
    if move_data.pokemon_type != PokemonType::Ground {
        return;
    }
    if move_data.category == MoveCategory::Status {
        return;
    }

    let mut pending_clauses: Vec<Vec<Statement>> = Vec::new();

    for target in targets {
        // Only infer from opponent (P2) immunity.
        if target.player != crate::state::battle::Player::P2 {
            continue;
        }

        // Did the move actually result in Immune for this target?
        let immune_on_target = event.reactions.iter().any(|r| {
            matches!(&r.kind, EventKind::Immune { target: t } if t == target)
        });
        if !immune_on_target {
            continue;
        }

        let Some(target_idx) = mon_idx_for_active_slot(state, target) else {
            continue;
        };

        let (known_types, tm_item, tm_abilities, has_magnet_rise, has_telekinesis) = {
            let tm = get_mon_by_idx(state, target_idx);
            let known_types = tm.and_then(|m| {
                if let Unknown::Known(ts) = &m.possible_types { Some(ts.clone()) } else { None }
            });
            let tm_item = tm.map(|m| m.item.clone());
            let tm_abilities = tm.map(|m| m.possible_abilities.clone());
            let has_magnet_rise = tm.map_or(false, |m| {
                m.volatiles.iter().any(|v| {
                    matches!(v, VolatileStatusState::TurnStatus(VolatileStatus::MagnetRise, _))
                })
            });
            let has_telekinesis = tm.map_or(false, |m| {
                m.volatiles.iter().any(|v| {
                    matches!(v, VolatileStatusState::TurnStatus(VolatileStatus::Telekinesis, _))
                })
            });
            (known_types, tm_item, tm_abilities, has_magnet_rise, has_telekinesis)
        };

        // Guard: types must be fully known.
        let Some(types) = known_types else { continue };

        // Guard: if Flying type is already in the type set, the immunity is explained.
        if types.contains(&PokemonType::Flying) {
            continue;
        }

        // Guard: Magnet Rise / Telekinesis volatiles explain the immunity.
        if has_magnet_rise || has_telekinesis {
            continue;
        }

        // Build the disjunctive clause: each candidate that is not already excluded.
        let mut clause: Vec<Statement> = Vec::new();

        // Air Balloon item.
        let ab_excluded = tm_item
            .as_ref()
            .map_or(false, |it| unknown_is_excluded(it, &Item::AirBalloon));
        if !ab_excluded {
            clause.push(Statement::HasItem { mon_idx: target_idx, item: Item::AirBalloon });
        }

        // Levitate ability.
        let lev_excluded = tm_abilities
            .as_ref()
            .map_or(false, |ab| unknown_is_excluded(ab, &Ability::Levitate));
        if !lev_excluded {
            clause.push(Statement::HasAbility { mon_idx: target_idx, ability: Ability::Levitate });
        }

        // Eelevate ability (custom ability with Levitate effect).
        let eel_excluded = tm_abilities
            .as_ref()
            .map_or(false, |ab| unknown_is_excluded(ab, &Ability::Eelevate));
        if !eel_excluded {
            clause.push(Statement::HasAbility { mon_idx: target_idx, ability: Ability::Eelevate });
        }

        // Earth Eater ability (absorbs Ground-type moves).
        let ee_excluded = tm_abilities
            .as_ref()
            .map_or(false, |ab| unknown_is_excluded(ab, &Ability::EarthEater));
        if !ee_excluded {
            clause.push(Statement::HasAbility {
                mon_idx: target_idx,
                ability: Ability::EarthEater,
            });
        }

        if !clause.is_empty() {
            pending_clauses.push(clause);
        }
    }

    for clause in pending_clauses {
        state.predicates.push(clause);
    }
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

/// Items that can reduce incoming damage for the **defender**.
///
/// Used in Direction A to union over possible defensive items when back-solving
/// the defender's defensive BSV. Without this union, the feasibility scan
/// materializes the defender with no item (neutral), which over-estimates the
/// minimum defensive BSV when the true defender has a bulk item (S1 soundness fix).
///
/// **Completeness is a soundness invariant, not an optimisation.**  If a reducer
/// item is omitted, the feasibility scan never materializes the defender with it,
/// so the "lower BSV + reducer" scenario is excluded and `min_pre_nature_stat`
/// may be raised above the true value — an unsound exclusion.
///
/// Always includes `neutral_item(mon)` (the Known item, or `Item::None`) so the
/// "no-boosting item" scenario is always in the candidate set.  Type-resist
/// berries that do not apply (wrong type / not super-effective) produce no change
/// in the oracle output, so including them is safe — the oracle gates on
/// `resist_berry_triggers`.
fn defensive_damage_items(mon: &UnknownPokemonState) -> Vec<Item> {
    let mut items: Vec<Item> = [
        Item::Eviolite,    // ×1.5 Def/SpDef for non-fully-evolved species
        Item::AssaultVest, // ×1.5 SpDef (special moves only)
        // Type-resist berries: each halves damage from one super-effective type.
        // Chilan Berry is Normal-type resist (any Normal hit, not just SE).
        // The damage oracle gates activation on type effectiveness, so including
        // all berries is safe for non-matching move types.
        Item::OccaBerry,   // Fire SE
        Item::PasshoBerry, // Water SE
        Item::WacanBerry,  // Electric SE
        Item::RindoBerry,  // Grass SE
        Item::YacheBerry,  // Ice SE
        Item::ChopleBerry, // Fighting SE
        Item::KebiaBerry,  // Poison SE
        Item::ShucaBerry,  // Ground SE
        Item::CobaBerry,   // Flying SE
        Item::PayapaBerry, // Psychic SE
        Item::TangaBerry,  // Bug SE
        Item::ChartiBerry, // Rock SE
        Item::KasibBerry,  // Ghost SE
        Item::HabanBerry,  // Dragon SE
        Item::ColburBerry, // Dark SE
        Item::BabiriBerry, // Steel SE
        Item::RoseliBerry, // Fairy SE
        Item::ChilanBerry, // Normal (any Normal hit)
    ]
    .iter()
    .filter(|i| !unknown_is_excluded(&mon.item, i))
    .cloned()
    .collect();
    // Always include the neutral item so the no-boost scenario is covered.
    let neutral = neutral_item(mon);
    if !items.contains(&neutral) {
        items.push(neutral);
    }
    items
}

/// Abilities that can reduce incoming damage for the **defender**.
///
/// Parallel to `offensive_damage_abilities` but for the defensive side.
/// Always includes `neutral_ability(mon)` so the no-boost scenario is covered.
///
/// **Completeness is a soundness invariant.**  Any reducer the damage oracle
/// implements but this list omits will cause `min_pre_nature_stat` to be raised
/// above the true value for defenders that could have that ability.
fn defensive_damage_abilities(mon: &UnknownPokemonState) -> Vec<Ability> {
    let mut abilities: Vec<Ability> = [
        Ability::Filter,       // ×0.75 on super-effective hits
        Ability::SolidRock,    // ×0.75 on super-effective hits
        Ability::PrismArmor,   // ×0.75 on super-effective hits (pierces Mold Breaker)
        Ability::Multiscale,   // ×0.5 at full HP
        Ability::ShadowShield, // ×0.5 at full HP (Lunala only)
        Ability::TeraShell,    // all moves → not-very-effective (≈×0.5) at full HP
        Ability::PurifyingSalt, // ×0.5 to Ghost-type moves
        Ability::ThickFat,     // ×0.5 to Fire and Ice moves
        Ability::FurCoat,      // ×0.5 to Physical moves
        Ability::IceScales,    // ×0.5 to Special moves
        Ability::Heatproof,    // ×0.5 to Fire moves
        Ability::Fluffy,       // ×0.5 to contact moves (but ×2 to Fire — oracle handles both)
        Ability::PunkRock,     // ×0.5 to sound-based moves received
        Ability::WaterBubble,  // ×0.5 to Fire moves received
    ]
    .iter()
    .filter(|a| !unknown_is_excluded(&mon.possible_abilities, a))
    .cloned()
    .collect();
    let neutral = neutral_ability(mon);
    if !abilities.contains(&neutral) {
        abilities.push(neutral);
    }
    abilities
}

/// Enumerate the distinct max-HP values the defender could have, given the
/// known species base stat, the defender's current IV/EV constraints, and the
/// current `[minStats[0], maxStats[0]]` window.
///
/// **Soundness rationale (S-B fix):** Direction A samples the defender's possible
/// max-HP values to back-solve the defensive BSV from a percent-HP observation.
/// The true max-HP is exactly one value in `[hp_lo, hp_hi]`.  Iterating only a
/// stride-4 subset can skip achievable values whose feasible BSV interval extends
/// past the sampled union, causing `min_pre_nature_stat` to be raised above the
/// true value (unsound exclusion).  Iterating the exact EV-lattice HP values
/// eliminates that gap while remaining fast (at most 33 × max_ivs iterations).
///
/// When `config.force_max_ivs` is true the IV is fixed at 31; otherwise all 32
/// IVs are tried.  The returned list is sorted and deduplicated.
fn achievable_defender_hp_values(
    base_hp: u16,
    level: u8,
    config: &InferenceConfig,
    mon: &UnknownPokemonState,
) -> Vec<u16> {
    let hp_lo = mon.minStats[0];
    let hp_hi = mon.maxStats[0];
    let iv_lo: u8 = if config.force_max_ivs { 31 } else { mon.minIvs[0] };
    let iv_hi: u8 = if config.force_max_ivs { 31 } else { mon.maxIvs[0] };

    let mut vals: Vec<u16> = Vec::with_capacity(33);
    for iv in iv_lo..=iv_hi {
        for &ev in &EV_LATTICE {
            // Also respect the EV bounds tracked on the mon.
            if ev < mon.minEvs[0] || ev > mon.maxEvs[0] {
                continue;
            }
            let hp = calc_hp(base_hp, iv, ev, level);
            if hp >= hp_lo && hp <= hp_hi && !vals.contains(&hp) {
                vals.push(hp);
            }
        }
    }
    if vals.is_empty() {
        // Fallback: should not happen if minStats/maxStats are consistent, but
        // be sound by including both endpoints.
        vals.push(hp_lo);
        if hp_hi != hp_lo {
            vals.push(hp_hi);
        }
    }
    vals.sort_unstable();
    vals
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

    // Build oracle outcomes for a single (bsv, spe_override) pair.
    let run_oracle = |bsv: u16, spe: u16| -> Vec<(u16, bool, f64)> {
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
        calculate_damage_outcomes_for_target_with_options(
            &battle,
            &atk_ps,
            &target_ps,
            user_slot.clone(),
            target_slot.clone(),
            move_data,
            *oracle_config,
            targets_mult,
            1.0,
            bp_override,
            None,
        )
    };

    // A BSV is feasible if the oracle produces `exact_damage` with correct crit for
    // *any* speed endpoint (sound union over the speed range).
    let can_produce = |bsv: u16| -> bool {
        speed_endpoints.iter().any(|&spe| {
            run_oracle(bsv, spe)
                .iter()
                .any(|(dmg, crit, _)| *dmg == exact_damage && *crit == is_crit)
        })
    };

    // (min, max) damage for outcomes matching the crit flag, unioned over speed endpoints.
    // Monotone: attacker offense ↑ as bsv ↑, so min_dmg and max_dmg are non-decreasing.
    let roll_band = |bsv: u16| -> (Option<u16>, Option<u16>) {
        let mut lo: Option<u16> = None;
        let mut hi: Option<u16> = None;
        for &spe in &speed_endpoints {
            for (dmg, crit, _) in run_oracle(bsv, spe) {
                if crit == is_crit {
                    lo = Some(lo.map_or(dmg, |m: u16| m.min(dmg)));
                    hi = Some(hi.map_or(dmg, |m: u16| m.max(dmg)));
                }
            }
        }
        (lo, hi)
    };

    // Binary-search for the outer bracket of the feasible BSV interval, exploiting
    // the monotone damage property (higher offensive BSV → more damage).
    //
    // Feasibility: min_roll(bsv) ≤ exact_damage ≤ max_roll(bsv).
    // • bsv_bracket_lo = smallest bsv where max_roll(bsv) ≥ exact_damage   (non-decreasing)
    // • bsv_bracket_hi = largest  bsv where min_roll(bsv) ≤ exact_damage   (non-decreasing)
    // These are sound outer brackets: true found_lo ≥ bsv_bracket_lo,
    //                                  true found_hi ≤ bsv_bracket_hi.
    //
    // A short linear walk from each bracket endpoint finds the exact feasible endpoints,
    // preserving full precision at O(log N + ε) oracle calls vs the former O(N) linear scan.
    let bsv_bracket_lo: Option<u16> = {
        let (mut lo, mut hi) = (bsv_lo as i32, bsv_hi as i32);
        let mut found = None;
        while lo <= hi {
            let mid = (lo + hi) / 2;
            if roll_band(mid as u16).1.map_or(false, |m| m >= exact_damage) {
                found = Some(mid as u16);
                hi = mid - 1;
            } else {
                lo = mid + 1;
            }
        }
        found
    };
    let bsv_bracket_hi: Option<u16> = {
        let (mut lo, mut hi) = (bsv_lo as i32, bsv_hi as i32);
        let mut found = None;
        while lo <= hi {
            let mid = (lo + hi) / 2;
            if roll_band(mid as u16).0.map_or(false, |m| m <= exact_damage) {
                found = Some(mid as u16);
                lo = mid + 1;
            } else {
                hi = mid - 1;
            }
        }
        found
    };

    match (bsv_bracket_lo, bsv_bracket_hi) {
        (Some(bl), Some(bh)) if bl <= bh => {
            // Refine: linear walk from each bracket to the first actually-feasible BSV.
            // In practice this is ≤ 1-2 steps since the band bracket is tight.
            let found_lo = (bl..=bh).find(|&b| can_produce(b));
            let found_hi = (bl..=bh).rev().find(|&b| can_produce(b));
            (found_lo, found_hi)
        }
        _ => (None, None),
    }
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
    // Spread multiplier: ×0.75 in doubles when the move targets all adjacent foes.
    // Mirroring Direction B (pass3_direction_b:3198-3207); omitting this caused the
    // back-solved defensive BSV to be off by 1/0.75 for spread moves in doubles (S2).
    let targets_mult = if state.active_per_side > 1
        && matches!(
            move_data.target,
            crate::state::dex_data::MoveTarget::AllAdjacent
                | crate::state::dex_data::MoveTarget::AllAdjacentFoes
        )
    {
        0.75_f64
    } else {
        1.0_f64
    };

    // ── Unconditional tightening: union over (nat, hp_candidate, def_bsv, def_item, def_ability) ────
    // Also accumulates per-nature-class neutral-gear bounds used by I1 predicate emission.
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

    // S1 soundness fix: union over defender's possible (item, ability) pairs so we
    // never raise min_pre_nature_stat above the truth for a bulk-item/resistance-ability
    // defender.  Mirrors how Direction B already unions over offensive items/abilities.
    let def_items = defensive_damage_items(&defender_unk);
    let def_abilities = defensive_damage_abilities(&defender_unk);

    let neutral_def_item = neutral_item(&defender_unk);
    let neutral_def_ability = neutral_ability(&defender_unk);

    // Per-nature-class neutral-gear BSV bounds, accumulated across hp_cand hypotheses.
    // Used by the I1 CNF predicate emission after the main loops.
    struct NatureClassResultA {
        mod_f32: f32,
        is_boost: bool,
        is_nerf: bool,
        bsv_lo_neutral: Option<u16>, // min over hp_cands (widest/most-conservative)
        bsv_hi_neutral: Option<u16>, // max over hp_cands
    }
    let mut per_class_a: Vec<NatureClassResultA> = nature_classes
        .iter()
        .map(|&(m, b, n)| NatureClassResultA {
            mod_f32: m, is_boost: b, is_nerf: n,
            bsv_lo_neutral: None, bsv_hi_neutral: None,
        })
        .collect();

    // Enumerate exactly the achievable HP values for this defender (S-B soundness fix).
    // A stride-4 sample can skip achievable HP values whose feasible-BSV interval
    // lies outside the sampled union, causing min_pre_nature_stat to be raised
    // above the true value (unsound exclusion).  Using the EV-lattice enumeration
    // ensures every realistically achievable HP is covered.
    let hp_candidates =
        achievable_defender_hp_values(base_stats[0], level, ctx.config, &defender_unk);
    for hp_cand in hp_candidates {
        // Convert percent delta to raw damage interval for this candidate max HP.
        // Convention: Percent(p) = round(current_hp * 100 / max_hp), so:
        //   p = round(hp * 100 / max_hp)  →  hp = round(p * max_hp / 100)
        // Damage interval for delta_pct p: [floor((p-0.5)*max_hp/100), ceil((p+0.5)*max_hp/100)]
        // clamped to [1, max_hp].  Sound: this is wider than the actual rounding bucket.
        let hp_c = hp_cand as f64;
        let d_lo = ((delta_pct as f64 - 0.5) * hp_c / 100.0).floor().max(1.0) as u16;
        let d_hi = ((delta_pct as f64 + 0.5) * hp_c / 100.0).ceil().min(hp_c) as u16;

        for (class_idx, (nat_mod, _is_boost, _is_nerf)) in nature_classes.iter().enumerate() {
            // Build and run the damage oracle for a fixed (bsv, def_item, def_ability, speed).
            // Pre-bakes defensive item stat multiplier (AV ×1.5 SpD, Eviolite ×1.5 Def+SpD)
            // since the oracle's effective_stat only handles offensive items.
            let run_def_oracle = |bsv: u16, def_item: &Item, def_ability: &Ability, def_spe: u16| {
                let item_stat_mult: f64 = match def_item {
                    Item::AssaultVest
                        if matches!(move_data.category, MoveCategory::Special) => 1.5,
                    Item::Eviolite => 1.5,
                    _ => 1.0,
                };
                let mut def_stats = defender_unk.minStats;
                def_stats[0] = hp_cand;
                if si == 0 {
                    def_stats[0] = bsv;
                } else {
                    let raw = (bsv as f64 * *nat_mod as f64).floor() as u16;
                    def_stats[si] = if item_stat_mult != 1.0 {
                        (raw as f64 * item_stat_mult).floor() as u16
                    } else {
                        raw
                    };
                }
                def_stats[5] = def_spe;
                let def_ps = materialize_pokemon(
                    &defender_unk, def_stats, def_item.clone(), def_ability.clone(),
                );
                let (p1_active, p2_active) =
                    if user_slot.player == crate::state::battle::Player::P1 {
                        (vec![atk_ps.clone()], vec![def_ps.clone()])
                    } else {
                        (vec![def_ps.clone()], vec![atk_ps.clone()])
                    };
                let battle = materialize_battle(state, p1_active, p2_active);
                calculate_damage_outcomes_for_target_with_options(
                    &battle, &atk_ps, &def_ps,
                    user_slot.clone(), target_slot.clone(),
                    move_data, oracle_config, targets_mult, 1.0, bp_override, None,
                )
            };

            // Binary-search for the feasible BSV interval for one fixed (def_item, def_ability).
            //
            // Monotone property: higher defensive BSV → more defense → less damage.
            // So min_roll and max_roll (damage) are both non-increasing in bsv.
            //
            // Feasibility: max_roll(bsv) ≥ d_lo  AND  min_roll(bsv) ≤ d_hi
            //   • bsv_bracket_lo = smallest bsv where min_roll ≤ d_hi   [F→T as bsv↑]
            //   • bsv_bracket_hi = largest  bsv where max_roll ≥ d_lo   [T→F as bsv↑]
            // These are sound outer brackets; a short linear walk from each bracket finds
            // the exact endpoints.
            let binary_search_def_combo =
                |def_item: &Item, def_ability: &Ability| -> (Option<u16>, Option<u16>)
            {
                // (min_dmg, max_dmg) for crit-matched outcomes, unioned over speed endpoints.
                let roll_band = |bsv: u16| -> (Option<u16>, Option<u16>)  {
                    let mut lo: Option<u16> = None;
                    let mut hi: Option<u16> = None;
                    for &def_spe in &defender_speed_endpoints {
                        for (dmg, crit, _) in run_def_oracle(bsv, def_item, def_ability, def_spe) {
                            if crit == is_crit {
                                lo = Some(lo.map_or(dmg, |m: u16| m.min(dmg)));
                                hi = Some(hi.map_or(dmg, |m: u16| m.max(dmg)));
                            }
                        }
                    }
                    (lo, hi)
                };

                let can_produce = |bsv: u16| -> bool {
                    defender_speed_endpoints.iter().any(|&def_spe| {
                        run_def_oracle(bsv, def_item, def_ability, def_spe)
                            .iter()
                            .any(|(dmg, crit, _)| *dmg >= d_lo && *dmg <= d_hi && *crit == is_crit)
                    })
                };

                let bsv_bracket_lo: Option<u16> = {
                    let (mut lo, mut hi) = (bsv_lo as i32, bsv_hi as i32);
                    let mut found = None;
                    while lo <= hi {
                        let mid = (lo + hi) / 2;
                        if roll_band(mid as u16).0.map_or(false, |m| m <= d_hi) {
                            found = Some(mid as u16);
                            hi = mid - 1;
                        } else {
                            lo = mid + 1;
                        }
                    }
                    found
                };
                let bsv_bracket_hi: Option<u16> = {
                    let (mut lo, mut hi) = (bsv_lo as i32, bsv_hi as i32);
                    let mut found = None;
                    while lo <= hi {
                        let mid = (lo + hi) / 2;
                        if roll_band(mid as u16).1.map_or(false, |m| m >= d_lo) {
                            found = Some(mid as u16);
                            lo = mid + 1;
                        } else {
                            hi = mid - 1;
                        }
                    }
                    found
                };

                match (bsv_bracket_lo, bsv_bracket_hi) {
                    (Some(bl), Some(bh)) if bl <= bh => {
                        let found_lo = (bl..=bh).find(|&b| can_produce(b));
                        let found_hi = (bl..=bh).rev().find(|&b| can_produce(b));
                        (found_lo, found_hi)
                    }
                    _ => (None, None),
                }
            };

            // Neutral-gear bounds (for I1 predicate emission): union across hp_cands.
            // min(bsv_lo) gives the widest/most-conservative lower bound across all HP hypotheses.
            let (neutral_lo, neutral_hi) =
                binary_search_def_combo(&neutral_def_item, &neutral_def_ability);
            {
                let cr = &mut per_class_a[class_idx];
                if let Some(lo) = neutral_lo {
                    cr.bsv_lo_neutral = Some(cr.bsv_lo_neutral.map_or(lo, |g: u16| g.min(lo)));
                }
                if let Some(hi) = neutral_hi {
                    cr.bsv_hi_neutral = Some(cr.bsv_hi_neutral.map_or(hi, |g: u16| g.max(hi)));
                }
            }

            // Full union over all (def_item, def_ability) combos for unconditional tightening.
            let mut found_lo_local: Option<u16> = None;
            let mut found_hi_local: Option<u16> = None;
            for def_item in &def_items {
                for def_ability in &def_abilities {
                    let (lo, hi) = binary_search_def_combo(def_item, def_ability);
                    if let (Some(lo_v), Some(hi_v)) = (lo, hi) {
                        found_lo_local = Some(found_lo_local.map_or(lo_v, |g: u16| g.min(lo_v)));
                        found_hi_local = Some(found_hi_local.map_or(hi_v, |g: u16| g.max(hi_v)));
                    }
                }
            }
            if let (Some(lo_v), Some(hi_v)) = (found_lo_local, found_hi_local) {
                let nat_mod = nature_classes[class_idx].0;
                let final_lo = (lo_v as f64 * nat_mod as f64).floor() as u16;
                let final_hi = (hi_v as f64 * nat_mod as f64).floor() as u16;
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

    // ── I1: Conditional CNF predicates (Direction A) ─────────────────────────
    // For each nature class κ, emit:
    //   LOWER: [not-κ guards] ∨ EVIVStatGE{bsv_lo_neutral} ∨ ⋁ reducer_items ∨ ⋁ reducer_abilities
    //   UPPER: [not-κ guards] ∨ EVIVStatLE{bsv_hi_neutral} ∨ ⋁ reducer_items ∨ ⋁ reducer_abilities
    //
    // Reducers (Eviolite, AV, Multiscale, …) allow a *lower* raw BSV to produce the
    // same observed damage, so they must appear as disjuncts in the lower-bound clause.
    // The upper bound is also unconditionally valid (reducers lower the feasible stat,
    // never raising it), but including reducer disjuncts mirrors Direction B's structure.
    // Only emitted when the bound is strictly tighter than the current tracked min/max.

    // Reducer literals: items/abilities that can lower the effective stat (exclude neutral).
    let reducer_items: Vec<Item> = def_items
        .iter()
        .filter(|i| **i != neutral_def_item)
        .cloned()
        .collect();
    let reducer_abilities: Vec<Ability> = def_abilities
        .iter()
        .filter(|a| **a != neutral_def_ability)
        .cloned()
        .collect();

    let current_pre_min = defender_unk.min_pre_nature_stat[si];
    let current_pre_max = defender_unk.max_pre_nature_stat[si];

    for cr in &per_class_a {
        let not_kappa_guards: Vec<Statement> = match (cr.is_boost, cr.is_nerf) {
            (true, _) => vec![Statement::Not(Box::new(Statement::NatureBoostsStat {
                mon_idx: target_idx,
                stat: def_stat.clone(),
            }))],
            (_, true) => vec![Statement::Not(Box::new(Statement::NatureNerfsStat {
                mon_idx: target_idx,
                stat: def_stat.clone(),
            }))],
            (false, false) => vec![
                Statement::NatureBoostsStat { mon_idx: target_idx, stat: def_stat.clone() },
                Statement::NatureNerfsStat { mon_idx: target_idx, stat: def_stat.clone() },
            ],
        };

        let reducer_literals: Vec<Statement> = reducer_items
            .iter()
            .map(|i| Statement::HasItem { mon_idx: target_idx, item: i.clone() })
            .chain(reducer_abilities.iter().map(|a| Statement::HasAbility {
                mon_idx: target_idx,
                ability: a.clone(),
            }))
            .collect();

        // Lower bound: EVIVStatGE{bsv_lo_neutral} — BSV must be at least this unless a reducer
        // is present (which could allow a lower raw BSV to explain the observed damage).
        if let Some(lo) = cr.bsv_lo_neutral {
            if lo > current_pre_min {
                let mut clause = not_kappa_guards.clone();
                clause.push(Statement::EVIVStatGE {
                    mon_idx: target_idx,
                    stat: def_stat.clone(),
                    value: lo,
                });
                clause.extend(reducer_literals.clone());
                if clause.len() > not_kappa_guards.len() + 1 {
                    state.predicates.push(clause);
                } else {
                    // No reducers possible — force directly.
                    if let Some(mon) = get_mon_mut_by_idx(state, target_idx) {
                        if lo > mon.min_pre_nature_stat[si] {
                            mon.min_pre_nature_stat[si] = lo;
                        }
                    }
                }
            }
        }
        // Upper bound: EVIVStatLE{bsv_hi_neutral} — BSV must be at most this
        // (reducers only lower the effective defensive stat, never raise it above neutral).
        if let Some(hi) = cr.bsv_hi_neutral {
            if hi < current_pre_max {
                let mut clause = not_kappa_guards.clone();
                clause.push(Statement::EVIVStatLE {
                    mon_idx: target_idx,
                    stat: def_stat.clone(),
                    value: hi,
                });
                clause.extend(reducer_literals.clone());
                if clause.len() > not_kappa_guards.len() + 1 {
                    state.predicates.push(clause);
                } else {
                    if let Some(mon) = get_mon_mut_by_idx(state, target_idx) {
                        if hi < mon.max_pre_nature_stat[si] {
                            mon.max_pre_nature_stat[si] = hi;
                        }
                    }
                }
            }
        }
    }
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
/// boosts that are deterministically known from state (Grassy Glide +1 on
/// Grassy Terrain).  Does NOT fold in ability-based boosts (Prankster/Gale Wings/
/// Triage); those are folded in by callers that have access to move data and user state.
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

/// Adjust a move's effective priority by any **Known** priority-lifting ability on the user.
/// Only fires when the ability is `Known(X)` — `Possibly` leaves the escape disjunct path.
fn fold_known_ability_priority(
    move_data: &MoveData,
    base_prio: i8,
    user_mon: &crate::information::unknowns::UnknownPokemonState,
) -> i8 {
    let Unknown::Known(ab) = &user_mon.possible_abilities else {
        return base_prio;
    };
    match ab {
        Ability::Prankster if move_data.category == MoveCategory::Status => base_prio + 1,
        Ability::GaleWings
            if move_data.pokemon_type == PokemonType::Flying
                && matches!(user_mon.hp, PokemonHP::Percent(100)) =>
        {
            base_prio + 1
        }
        Ability::Triage if move_data.heal_fraction != [0, 0] => base_prio + 3,
        _ => base_prio,
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
            let mut eff_prio = effective_move_priority(move_used, base_prio, state);
            if let Some(idx) = mon_idx_for_active_slot(state, user) {
                // Fold in Known priority-lifting abilities to get the tightest bracket.
                if let (Some(mon), Some(md)) = (get_mon_by_idx(state, idx), move_dex.get(move_used)) {
                    eff_prio = fold_known_ability_priority(md, eff_prio, mon);
                }
                move_order.push((user.clone(), eff_prio, idx, move_used.clone()));
            }
        }
    }

    let trick_room_active = state.pseudo_weathers.contains(&PseudoWeather::TrickRoom);

    for window in move_order.windows(2) {
        let (_, p0, idx0, mv0) = &window[0];
        let (_, p1, idx1, _mv1) = &window[1];

        // Different effective priority brackets.
        // If the first mover has a *lower* effective priority than the second (p0 < p1),
        // the observation is only explicable by a priority-lifting ability on the first
        // mover (Prankster, Gale Wings, Triage) or by a random first-mover effect.
        // Emit a disjunction for these; if it collapses to a unit clause BCP will force
        // the ability.  If p0 > p1, normal priority ordering — no inference possible.
        if p0 != p1 {
            if *p0 < *p1 {
                // Earlier mover had lower declared priority — must have a lifter.
                let fast_idx = *idx0;
                let fast_mon = get_mon_by_idx(state, fast_idx);
                if let (Some(fast_m), Some(fast_md)) =
                    (fast_mon, move_dex.get(mv0))
                {
                    let mut clause: Vec<Statement> = Vec::new();
                    // Prankster: +1 to Status-category moves.
                    if fast_md.category == MoveCategory::Status
                        && !unknown_is_excluded(&fast_m.possible_abilities, &Ability::Prankster)
                    {
                        clause.push(Statement::HasAbility {
                            mon_idx: fast_idx,
                            ability: Ability::Prankster,
                        });
                    }
                    // Gale Wings: +1 to Flying-type moves at full HP.
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
                    // Quick Claw / Quick Draw (random first-mover escapes).
                    if !unknown_is_excluded(&fast_m.item, &Item::QuickClaw) {
                        clause.push(Statement::HasItem {
                            mon_idx: fast_idx,
                            item: Item::QuickClaw,
                        });
                    }
                    if !unknown_is_excluded(&fast_m.possible_abilities, &Ability::QuickDraw) {
                        clause.push(Statement::HasAbility {
                            mon_idx: fast_idx,
                            ability: Ability::QuickDraw,
                        });
                    }
                    if !clause.is_empty() {
                        state.predicates.push(clause);
                    }
                }
            }
            // p0 > p1: normal priority ordering, no inference.
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

        // (5b) Custap Berry on the fast mon: activates at ≤25% HP and forces the holder
        //      to move first in its priority bracket regardless of speed. Include as an
        //      escape disjunct when the fast mon might be at ≤25% HP (S3 soundness fix).
        if let Some(fast_m) = fast_mon {
            let custap_possible = match &fast_m.hp {
                PokemonHP::Percent(p) => *p <= 25,
                PokemonHP::Number(n) => {
                    let max_hp = fast_m.maxStats[0].max(1) as u32;
                    (*n as u32).saturating_mul(100) / max_hp <= 25
                }
            };
            if custap_possible && !unknown_is_excluded(&fast_m.item, &Item::CustapBerry) {
                clause.push(Statement::HasItem {
                    mon_idx: fast_idx,
                    item: Item::CustapBerry,
                });
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
        // Guard against duplicate clauses that arise when pass4 is re-run after BCP.
        // Duplicates are logically harmless but cause BCP to re-scan redundant work
        // on every fixpoint iteration.
        if !state.predicates.contains(&clause) {
            state.predicates.push(clause);
        }
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

        // Soundness assertion: if every candidate nature is infeasible for this
        // stat, we have a contradiction — the observed stat range cannot be
        // produced by any nature.  This fires only if inference itself has
        // over-narrowed (a bug), never for valid opponent data.
        if impossible_natures.iter().all(|&b| b) {
            panic!(
                "pass5: every candidate nature is infeasible for stat {stat_i} \
                 (minStat={s_min}, maxStat={s_max}) — inference over-narrowed"
            );
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

fn run_bcp(state: &mut UnknownBattleState, allow_repeat_items: bool) {
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
                force_literal(state, &lit, allow_repeat_items);
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

fn force_literal(state: &mut UnknownBattleState, lit: &Statement, allow_repeat_items: bool) {
    match lit {
        Statement::HasItem { mon_idx, item } => {
            if let Some(mon) = get_mon_mut_by_idx(state, *mon_idx) {
                unknown_set_known(&mut mon.item, item.clone(), &format!("bcp#{mon_idx}"));
            }
            // Item clause: BCP-committed team-built item cannot be held by any
            // other roster member on the same side. Because run_bcp loops to
            // fixpoint, a freshly narrowed teammate that collapses to one
            // candidate will itself trigger enforce_unique_item on the next pass.
            enforce_unique_item(state, *mon_idx, item, allow_repeat_items);
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
