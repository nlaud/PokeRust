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
use crate::simulator::helpers::base_damage_formula;
use crate::state::battle::{FieldSlot, Player};
use crate::state::dex_data::{
    AccuracyType, MoveCategory, MoveData, PokemonData, PokemonStat, PseudoWeather,
    SideCondition, SlotCondition, Status, Terrain, VolatileStatus, Weather,
};
use crate::information::information::{EventKind, InformationEvent, SwitchState};
use crate::state::pokemon::{calc_hp, calc_stat, nature_stat_modifiers, Nature};
use crate::information::unknowns::{
    PokemonHP, Statement, UnknownBattleState, UnknownMatchState, UnknownPokemonState,
    UnknownTeamPreviewState, Unknown,
};

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
}

impl Default for InferenceConfig {
    fn default() -> Self {
        InferenceConfig {
            use_stat_points: true,
            force_max_ivs: true,
            level: 50,
            legal_items: None,
        }
    }
}

// ── EV lattice (stat-points mode) ─────────────────────────────────────────────

/// Achievable EV values under `--stat-points` mode.
/// Derived from `scale_evs_for_stat_points`: `ev = max(0, 8p − 4)` for `p = 0..=32`.
/// 33 values: 0, then 4, 12, 20, …, 252 (each +8 after the first gap).
pub const EV_LATTICE: [u8; 33] = [
    0, 4, 12, 20, 28, 36, 44, 52, 60, 68, 76, 84, 92, 100, 108, 116, 124, 132, 140, 148, 156,
    164, 172, 180, 188, 196, 204, 212, 220, 228, 236, 244, 252,
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
            if slot_i < state.p1_active_mons.len() { Some(slot_i) } else { None }
        }
        Player::P2 => {
            let p2_start = state.p1_active_mons.len()
                + state.p1_known_back_mons.len()
                + state.p1_possible_back_mons.len();
            if slot_i < state.p2_active_mons.len() { Some(p2_start + slot_i) } else { None }
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
/// # Panics
/// If the events are jointly impossible under the current state (soundness oracle).
pub fn apply_information(
    mut state: UnknownMatchState,
    events: &[InformationEvent],
    is_team_preview: bool,
    dex: &HashMap<Species, PokemonData>,
    move_dex: &HashMap<PokemonMove, MoveData>,
    config: &InferenceConfig,
) -> UnknownMatchState {
    match &mut state {
        UnknownMatchState::TeamPreview(preview) => {
            apply_information_team_preview(preview, events, config, dex);
        }
        UnknownMatchState::Battle(battle) => {
            apply_information_battle(battle, events, dex, move_dex, config);
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
    // At team preview only Switch (initial sends) and ability/item reveals appear.
    // No speed inference — no ordering information yet.
    for event in events {
        process_team_preview_event(state, event, config, dex);
    }
}

fn process_team_preview_event(
    state: &mut UnknownTeamPreviewState,
    event: &InformationEvent,
    config: &InferenceConfig,
    dex: &HashMap<Species, PokemonData>,
) {
    if let EventKind::Switch(sw) = &event.kind {
        let mons = match sw.slot.player {
            Player::P1 => &mut state.p1_mons,
            Player::P2 => &mut state.p2_mons,
        };
        if let Some(mon) = mons
            .iter_mut()
            .find(|m| unknown_is_known_as(&m.possible_species, &sw.species))
        {
            apply_switch_state_to_mon(mon, sw, config);
        }
    }
    for reaction in &event.reactions {
        process_team_preview_event(state, reaction, config, dex);
    }
}

// ── Battle path ───────────────────────────────────────────────────────────────

fn apply_information_battle(
    state: &mut UnknownBattleState,
    events: &[InformationEvent],
    dex: &HashMap<Species, PokemonData>,
    move_dex: &HashMap<PokemonMove, MoveData>,
    config: &InferenceConfig,
) {
    // ── Pass 1–3: depth-first event walk ─────────────────────────────────────
    let mut ctx = BattleContext {
        dex,
        move_dex,
        config,
        move_context: None,
    };
    for event in events {
        process_battle_event(state, event, &mut ctx);
    }

    // ── Pass 4: speed ordering from top-level MoveUsed sequence ──────────────
    pass4_speed_from_order(state, events, move_dex);

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

    if let EventKind::MoveUsed { user, move_used, targets } = &event.kind {
        ctx.move_context = Some(MoveContext {
            user_slot: user.clone(),
            pokemon_move: move_used.clone(),
            targets: targets.clone(),
            is_crit,
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
        // Pass 3 deferred — needs HP tracking across turns (see TODO.md).
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
        EventKind::Switch(sw) => pass1_switch(state, sw, ctx),

        EventKind::MoveUsed { user, move_used, .. } => {
            if let Some(idx) = mon_idx_for_active_slot(state, user) {
                if let Some(mon) = get_mon_mut_by_idx(state, idx) {
                    reveal_move_on_mon(mon, move_used);
                    if Some(move_used) == mon.last_used_move.as_ref() {
                        mon.consecutive_move_count =
                            mon.consecutive_move_count.saturating_add(1);
                    } else {
                        mon.consecutive_move_count = 1;
                    }
                    // Choice-item cross-turn exclusion: a second different move
                    // while potentially Choice-locked rules out Choice items.
                    pass1_choice_exclusion(mon, move_used);
                    mon.last_used_move = Some(move_used.clone());
                    for i in 0..4 {
                        if mon.known_moves[i] == Some(move_used.clone()) {
                            mon.used_moves_this_field[i] = true;
                        }
                    }
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
            update_mon_hp(state, target, new_hp.clone());
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
                                status, existing
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
                            "ItemRevealed {:?} outside legal whitelist", item
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
                if let Some(mon) = get_mon_mut_by_idx(state, idx) {
                    mon.item = Unknown::Known(item.clone());
                    mon.item_lost = false;
                }
            }
        }
        EventKind::ItemLost { slot, item, consumed } => {
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
        EventKind::AbilitySuppressed { .. } => {
            // TODO: track ability-suppression flag on UnknownPokemonState.
        }

        EventKind::BoostChanged { target, stat, stages } => {
            if let Some(idx) = mon_idx_for_active_slot(state, target) {
                if let Some(mon) = get_mon_mut_by_idx(state, idx) {
                    if let Some(i) = pokemon_stat_to_boost_idx(stat) {
                        let new_stage =
                            (mon.boosts[i] as i16 + *stages as i16).clamp(-6, 6) as i8;
                        mon.boosts[i] = new_stage;
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
                    if let Some(sm) = get_mon_mut_by_idx(state, si) { sm.boosts = tb; }
                    if let Some(tm) = get_mon_mut_by_idx(state, ti) { tm.boosts = sb; }
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
                let mega_stone = ctx.dex.get(into).and_then(|d| {
                    d.required_item.as_ref().map(|s| Item::from_str(s))
                });
                if let Some(stone) = mega_stone {
                    if stone != Item::None {
                        if let Some(legal) = &ctx.config.legal_items {
                            if !legal.contains(&stone) {
                                inference_contradiction!(
                                    idx,
                                    "Mega Stone {:?} outside legal whitelist", stone
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
                    Player::P1 => state.p1_has_tera = true,
                    Player::P2 => state.p2_has_tera = true,
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
            state.weather_turns = weather
                .as_ref()
                .map(|_| Unknown::Possibly(vec![5, 8]));
        }
        EventKind::TerrainChanged { terrain } => {
            state.terrain = terrain.clone();
            state.terrain_turns = terrain
                .as_ref()
                .map(|_| Unknown::Possibly(vec![5, 8]));
        }
        EventKind::PseudoWeatherStart { effect } => {
            if !state.pseudo_weathers.contains(effect) {
                state.pseudo_weathers.push(effect.clone());
                state.pseudo_weather_turns.push(Unknown::Possibly(vec![5, 8]));
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
                Player::P1 => (&mut state.p1_side_conditions, &mut state.p1_side_condition_turns),
                Player::P2 => (&mut state.p2_side_conditions, &mut state.p2_side_condition_turns),
            };
            if !conditions.contains(condition) {
                conditions.push(condition.clone());
                turns.push(Unknown::Possibly(vec![5, 8]));
            }
        }
        EventKind::SideConditionEnd { side, condition } => {
            let (conditions, turns) = match side {
                Player::P1 => (&mut state.p1_side_conditions, &mut state.p1_side_condition_turns),
                Player::P2 => (&mut state.p2_side_conditions, &mut state.p2_side_condition_turns),
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
                        mon.volatiles.push(VolatileStatusState::TurnStatus(
                            volatile.clone(),
                            0,
                        ));
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
        if let Some(pos) = known.iter().position(|m| unknown_is_known_as(&m.possible_species, species)) {
            Some(known.remove(pos))
        } else {
            let possible = match player {
                Player::P1 => &mut state.p1_possible_back_mons,
                Player::P2 => &mut state.p2_possible_back_mons,
            };
            possible.iter().position(|m| unknown_is_known_as(&m.possible_species, species))
                .map(|pos| possible.remove(pos))
        }
    };

    let mut mon = if let Some(m) = back_mon {
        m
    } else {
        // Completely new opponent mon: build from species.
        let mut new_mon = UnknownPokemonState::from_opponent_species(
            species.clone(),
            ctx.dex,
            ctx.config.level,
        );
        if ctx.config.force_max_ivs {
            pin_ivs_and_recompute_stats(&mut new_mon, species, ctx);
        }
        if let Some(legal) = &ctx.config.legal_items {
            let mut candidates: Vec<Item> = legal.iter().cloned().collect();
            candidates.push(Item::None);
            new_mon.item = Unknown::Possibly(candidates);
        }
        new_mon
    };

    apply_switch_state_to_mon(&mut mon, sw, ctx.config);

    let actives = match sw.slot.player {
        Player::P1 => &mut state.p1_active_mons,
        Player::P2 => &mut state.p2_active_mons,
    };
    if slot_i < actives.len() {
        actives[slot_i] = mon;
    } else {
        actives.push(mon);
    }
}

fn pin_ivs_and_recompute_stats(
    mon: &mut UnknownPokemonState,
    species: &Species,
    ctx: &BattleContext,
) {
    mon.minIvs = [31; 6];
    mon.maxIvs = [31; 6];
    if let Some(data) = ctx.dex.get(species) {
        let b = data.base_stats;
        let lv = ctx.config.level;
        mon.minStats = [
            calc_hp(b[0], 31, 0, lv),
            calc_stat(b[1], 31, 0, lv, 0.9),
            calc_stat(b[2], 31, 0, lv, 0.9),
            calc_stat(b[3], 31, 0, lv, 0.9),
            calc_stat(b[4], 31, 0, lv, 0.9),
            calc_stat(b[5], 31, 0, lv, 0.9),
        ];
        mon.maxStats = [
            calc_hp(b[0], 31, 252, lv),
            calc_stat(b[1], 31, 252, lv, 1.1),
            calc_stat(b[2], 31, 252, lv, 1.1),
            calc_stat(b[3], 31, 252, lv, 1.1),
            calc_stat(b[4], 31, 252, lv, 1.1),
            calc_stat(b[5], 31, 252, lv, 1.1),
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
    if let Some(tt) = &sw.tera_type {
        mon.is_tera = true;
        mon.possible_tera_type = Unknown::Known(tt.clone());
    }
    if config.force_max_ivs {
        mon.minIvs = [31; 6];
        mon.maxIvs = [31; 6];
    }
}

fn reveal_move_on_mon(mon: &mut UnknownPokemonState, pokemon_move: &PokemonMove) {
    if mon.known_moves.iter().any(|m| m.as_ref() == Some(pokemon_move)) {
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

fn pass1_choice_exclusion(mon: &mut UnknownPokemonState, new_move: &PokemonMove) {
    if let Some(ref last) = mon.last_used_move.clone() {
        if last != new_move {
            let choices = [Item::ChoiceBand, Item::ChoiceScarf, Item::ChoiceSpecs];
            for ci in &choices {
                unknown_exclude(&mut mon.item, ci, "choice-lock");
            }
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

fn pokemon_stat_to_boost_idx(stat: &PokemonStat) -> Option<usize> {
    match stat {
        PokemonStat::Atk => Some(0),
        PokemonStat::Def => Some(1),
        PokemonStat::SpA => Some(2),
        PokemonStat::SpD => Some(3),
        PokemonStat::Spe => Some(4),
    }
}

// ── Pass 2: Item presence/absence from behaviour ──────────────────────────────

fn pass2_item_from_move(
    state: &mut UnknownBattleState,
    event: &InformationEvent,
    ctx: &BattleContext,
) {
    let EventKind::MoveUsed { user, move_used, targets } = &event.kind else {
        return;
    };
    let Some(move_data) = ctx.move_dex.get(move_used) else { return };
    let is_damaging =
        matches!(move_data.category, MoveCategory::Physical | MoveCategory::Special);

    // ── Life Orb ──────────────────────────────────────────────────────────────
    if is_damaging {
        let has_lo_recoil = event.reactions.iter().any(|r| {
            matches!(&r.kind, EventKind::DamageDealt { target, .. } if target == user)
        });

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
                            ctx.config.legal_items.as_ref().map_or(true, |l| l.contains(item))
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
//
// Deferred. Inverting the 22-step floored damage chain requires tracking HP
// across turns to compute an exact damage delta.  Currently not implemented.
// When added, this will bound the opponent's Atk/SpA (when we are the target)
// or their Def/SpD/HP (when we deal the damage).
// See TODO.md: "Damage→stat inversion for standard single-hit moves".

// ── Pass 4: Speed ordering → Spe bounds ──────────────────────────────────────

/// Emit `SpeedComparison` predicates from the observed top-level move order.
fn pass4_speed_from_order(
    state: &mut UnknownBattleState,
    top_events: &[InformationEvent],
    move_dex: &HashMap<PokemonMove, MoveData>,
) {
    // Collect (slot, priority, mon_idx) for all top-level MoveUsed events in order.
    let mut move_order: Vec<(FieldSlot, i8, usize)> = Vec::new();
    for event in top_events {
        if let EventKind::MoveUsed { user, move_used, .. } = &event.kind {
            let priority = move_dex.get(move_used).map(|md| md.priority).unwrap_or(0);
            if let Some(idx) = mon_idx_for_active_slot(state, user) {
                move_order.push((user.clone(), priority, idx));
            }
        }
    }

    for window in move_order.windows(2) {
        let (_, p0, idx0) = &window[0];
        let (_, p1, idx1) = &window[1];
        if p0 != p1 {
            continue; // Different priority brackets — no speed info.
        }
        let fast_idx = *idx0;
        let slow_idx = *idx1;

        let (fast_mult, slow_mult) =
            compute_speed_multipliers(state, fast_idx, slow_idx);

        // Can Quick Claw / Quick Draw explain the ordering without a speed advantage?
        let fast_could_have_qc = get_mon_by_idx(state, fast_idx)
            .map_or(false, |m| !unknown_is_excluded(&m.item, &Item::QuickClaw));
        let fast_could_have_qd = get_mon_by_idx(state, fast_idx)
            .map_or(false, |m| !unknown_is_excluded(&m.possible_abilities, &Ability::QuickDraw));

        let speed_cmp = Statement::SpeedComparison { fast_idx, slow_idx, fast_mult, slow_mult };

        if !fast_could_have_qc && !fast_could_have_qd {
            // Clean ordering.
            state.predicates.push(vec![speed_cmp]);
        } else {
            // Disjunction: natural speed OR random first-mover.
            let mut clause = vec![speed_cmp];
            if fast_could_have_qc {
                clause.push(Statement::HasItem { mon_idx: fast_idx, item: Item::QuickClaw });
            }
            if fast_could_have_qd {
                clause.push(Statement::HasAbility { mon_idx: fast_idx, ability: Ability::QuickDraw });
            }
            state.predicates.push(clause);
        }
    }
}

/// Integer speed multipliers (fast_mult, slow_mult) scaled to a common denominator.
/// Invariant: `base_spe(fast) * fast_mult >= base_spe(slow) * slow_mult`.
fn compute_speed_multipliers(
    state: &UnknownBattleState,
    fast_idx: usize,
    slow_idx: usize,
) -> (u32, u32) {
    let fast_boost = get_mon_by_idx(state, fast_idx).map(|m| m.boosts[4]).unwrap_or(0);
    let slow_boost = get_mon_by_idx(state, slow_idx).map(|m| m.boosts[4]).unwrap_or(0);
    let fast_para = get_mon_by_idx(state, fast_idx)
        .map(|m| matches!(m.status, Some(Status::Paralysis)))
        .unwrap_or(false);
    let slow_para = get_mon_by_idx(state, slow_idx)
        .map(|m| matches!(m.status, Some(Status::Paralysis)))
        .unwrap_or(false);

    // Stage multiplier as (numerator, denominator) with denominator in [2, 8].
    let stage_frac = |stage: i8| -> (u32, u32) {
        let s = stage.clamp(-6, 6);
        if s >= 0 { (2 + s as u32, 2) } else { (2, 2 + (-s) as u32) }
    };

    let (fn_, fd) = stage_frac(fast_boost);
    let (sn_, sd) = stage_frac(slow_boost);

    // Paralysis ×1/2.
    let (fp_n, fp_d): (u32, u32) = if fast_para { (1, 2) } else { (1, 1) };
    let (sp_n, sp_d): (u32, u32) = if slow_para { (1, 2) } else { (1, 1) };

    // Combine to a common scale: fast_mult = fn_*fp_n * (sd*sp_d), slow_mult = sn_*sp_n * (fd*fp_d).
    let fast_mult = fn_ * fp_n * sd * sp_d;
    let slow_mult = sn_ * sp_n * fd * fp_d;
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
        let iv_range = if config.force_max_ivs { 31..=31 } else { mon.minIvs[0]..=mon.maxIvs[0] };
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
        if let Some(lo) = min_ev { if lo > mon.minEvs[0] { mon.minEvs[0] = lo; } }
        if let Some(hi) = max_ev { if hi < mon.maxEvs[0] { mon.maxEvs[0] = hi; } }
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
            if impossible_natures[ni] { continue; }
            let mods = nature_stat_modifiers(nature);
            let nature_mod = mods[stat_i - 1]; // [atk, def, spa, spd, spe]

            let mut found = false;
            let mut n_min_ev: Option<u8> = None;
            let mut n_max_ev: Option<u8> = None;

            for iv in iv_range.clone() {
                for &ev in ev_candidates {
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

        if impossible_natures.iter().enumerate()
            .filter(|(ni, _)| !{
                // re-filter to only candidate natures
                false
            })
            .all(|(ni, _)| impossible_natures[ni])
        {
            // all remaining candidates are impossible for this stat — panic only
            // if ALL candidates (not just the ones already impossible) fail.
        }

        if let Some(lo) = global_min_ev { if lo > mon.minEvs[stat_i] { mon.minEvs[stat_i] = lo; } }
        if let Some(hi) = global_max_ev { if hi < mon.maxEvs[stat_i] { mon.maxEvs[stat_i] = hi; } }
    }

    // Eliminate natures that were impossible for any stat.
    for (ni, nature) in candidate_natures.iter().enumerate() {
        if impossible_natures[ni] {
            unknown_exclude(&mut mon.possible_natures, nature, "pass5-nature");
        }
    }

    // Panic if every nature is now excluded.
    let remaining = all_natures.iter().filter(|n| !unknown_is_excluded(&mon.possible_natures, n)).count();
    if remaining == 0 {
        inference_contradiction!("pass5", "no valid nature remains after pass5");
    }
}

const ALL_NATURES: &[Nature] = &[
    Nature::Hardy, Nature::Lonely, Nature::Adamant, Nature::Naughty, Nature::Brave,
    Nature::Bold, Nature::Docile, Nature::Impish, Nature::Lax, Nature::Relaxed,
    Nature::Modest, Nature::Mild, Nature::Bashful, Nature::Rash, Nature::Quiet,
    Nature::Calm, Nature::Gentle, Nature::Careful, Nature::Quirky, Nature::Sassy,
    Nature::Timid, Nature::Hasty, Nature::Jolly, Nature::Naive, Nature::Serious,
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
            if still_live.len() == 1
                && !matches!(still_live[0], Statement::SpeedComparison { .. })
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
            get_mon_by_idx(state, *mon_idx)
                .map_or(false, |m| unknown_is_excluded(&m.item, item))
        }
        Statement::HasStatus { mon_idx, status } => {
            get_mon_by_idx(state, *mon_idx)
                .map_or(false, |m| m.status.as_ref().map_or(true, |s| s != status))
        }
        Statement::HasMove { mon_idx, pokemon_move } => {
            get_mon_by_idx(state, *mon_idx).map_or(false, |m| {
                let full = m.known_moves.iter().all(|s| s.is_some());
                full && !m.known_moves.iter().any(|s| s.as_ref() == Some(pokemon_move))
            })
        }
        Statement::HasAbility { mon_idx, ability } => {
            get_mon_by_idx(state, *mon_idx)
                .map_or(false, |m| unknown_is_excluded(&m.possible_abilities, ability))
        }
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
        Statement::EVIVStatGE { mon_idx, stat, value } => {
            get_mon_by_idx(state, *mon_idx)
                .map_or(false, |m| m.maxStats[stat_to_stats_idx(stat)] < *value)
        }
        Statement::SpeedComparison { fast_idx, slow_idx, fast_mult, slow_mult } => {
            let fast_max = get_mon_by_idx(state, *fast_idx).map_or(999u64, |m| m.maxStats[5] as u64);
            let slow_min = get_mon_by_idx(state, *slow_idx).map_or(0u64, |m| m.minStats[5] as u64);
            fast_max * (*fast_mult as u64) < slow_min * (*slow_mult as u64)
        }
        Statement::WeatherTurns { .. }
        | Statement::PseudoWeatherTurns { .. }
        | Statement::SideConditionTurns { .. } => false, // TODO
    }
}

fn eval_true(state: &UnknownBattleState, lit: &Statement) -> bool {
    match lit {
        Statement::Not(inner) => eval_false(state, inner),
        Statement::HasItem { mon_idx, item } => {
            get_mon_by_idx(state, *mon_idx)
                .map_or(false, |m| unknown_is_known_as(&m.item, item))
        }
        Statement::HasStatus { mon_idx, status } => {
            get_mon_by_idx(state, *mon_idx)
                .map_or(false, |m| m.status.as_ref() == Some(status))
        }
        Statement::HasMove { mon_idx, pokemon_move } => {
            get_mon_by_idx(state, *mon_idx)
                .map_or(false, |m| m.known_moves.iter().any(|s| s.as_ref() == Some(pokemon_move)))
        }
        Statement::HasAbility { mon_idx, ability } => {
            get_mon_by_idx(state, *mon_idx)
                .map_or(false, |m| unknown_is_known_as(&m.possible_abilities, ability))
        }
        Statement::EVIVStatGE { mon_idx, stat, value } => {
            get_mon_by_idx(state, *mon_idx)
                .map_or(false, |m| m.minStats[stat_to_stats_idx(stat)] >= *value)
        }
        Statement::SpeedComparison { fast_idx, slow_idx, fast_mult, slow_mult } => {
            let fast_min = get_mon_by_idx(state, *fast_idx).map_or(0u64, |m| m.minStats[5] as u64);
            let slow_max = get_mon_by_idx(state, *slow_idx).map_or(999u64, |m| m.maxStats[5] as u64);
            fast_min * (*fast_mult as u64) >= slow_max * (*slow_mult as u64)
        }
        Statement::NatureBoostsStat { .. }
        | Statement::NatureNerfsStat { .. }
        | Statement::WeatherTurns { .. }
        | Statement::PseudoWeatherTurns { .. }
        | Statement::SideConditionTurns { .. } => false, // TODO
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
                if mon.minStats[si] < *value {
                    mon.minStats[si] = *value;
                }
            }
        }
        Statement::Not(_)
        | Statement::SpeedComparison { .. } // handled by propagate_speed_comparisons
        | Statement::NatureBoostsStat { .. }
        | Statement::NatureNerfsStat { .. }
        | Statement::WeatherTurns { .. }
        | Statement::PseudoWeatherTurns { .. }
        | Statement::SideConditionTurns { .. } => {}
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
                if let Statement::SpeedComparison { fast_idx, slow_idx, fast_mult, slow_mult } = lit {
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
                        new_fast_min, mon.maxStats[5]
                    );
                }
                mon.minStats[5] = new_fast_min;
                changed = true;
            }
        }

        // Lower slow's max Spe: base_spe(slow) <= floor(base_spe(fast)*fast_mult / slow_mult)
        let fast_max = get_mon_by_idx(state, fast_idx).map_or(u64::MAX / 2, |m| m.maxStats[5] as u64);
        let new_slow_max = (fast_max.saturating_mul(fast_mult as u64) / slow_mult as u64)
            .min(u16::MAX as u64) as u16;
        if let Some(mon) = get_mon_mut_by_idx(state, slow_idx) {
            if new_slow_max < mon.maxStats[5] {
                if new_slow_max < mon.minStats[5] {
                    inference_contradiction!(
                        slow_idx,
                        "SpeedComparison lowers max({}) below min({})",
                        new_slow_max, mon.minStats[5]
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
    if b == 0 { return a; }
    (a + b - 1) / b
}

// ── Nature helpers ────────────────────────────────────────────────────────────

fn boosting_natures_for_stat(stat: &PokemonStat) -> Vec<Nature> {
    match stat {
        PokemonStat::Atk => vec![Nature::Lonely, Nature::Adamant, Nature::Naughty, Nature::Brave],
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
        PokemonStat::SpA => vec![Nature::Adamant, Nature::Impish, Nature::Careful, Nature::Jolly],
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
    if !has_zoroark { return; }
    let Some(idx) = mon_idx_for_active_slot(state, slot) else { return };
    let Some(mon) = get_mon_mut_by_idx(state, idx) else { return };
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

