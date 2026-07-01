use crate::state::battle::{Action, BattleState, FieldSlot, Player};
use crate::information::information::{CantReason, EventKind, InformationEvent};
use crate::information::unknowns::PokemonHP;
use crate::data::ability::Ability;
use crate::data::item::Item;
use crate::data::pokemon_move::PokemonMove;
use crate::data::species::Species;
use crate::state::dex_data::VolatileStatus;
use crate::state::dex_data::{
    AccuracyType, DamageOverride, HitEffect, MoveCategory, MoveData, MoveFlag, MoveTarget,
    PokemonStat, PokemonType, PseudoWeather, SelfSwitchType, SideCondition, Status, Terrain,
    Weather,
};
use crate::state::pokemon::{Nature, PokemonState, VolatileStatusState};
use rand::{Rng, thread_rng};
use std::collections::HashMap;
use std::hash::Hash;
use std::sync::atomic::Ordering;

pub fn get_verbosity() -> u8 {
    crate::VERBOSITY.get().copied().unwrap_or(1)
}

pub fn shared_multihit_damage_rolls_enabled() -> bool {
    crate::SHARED_MULTIHIT_DAMAGE_ROLLS.load(Ordering::Relaxed)
}

// ── Event collection helpers ──────────────────────────────────────────────────

/// Push an `InformationEvent` onto `state.pending_events` when an observer is set.
/// Near-zero cost when `event_observer` is `None` (one `is_some()` check, no alloc).
#[inline]
pub fn emit(bs: &mut BattleState, kind: EventKind) {
    if bs.event_observer.is_some() {
        bs.pending_events.push(InformationEvent { kind, reactions: vec![] });
    }
}

/// Run `f`; any events it emits into `bs.pending_events` become the `reactions` of a
/// parent node whose `kind` is `parent`.  The parent is pushed **after** its children
/// (split off from the pending tail), preserving causal ordering.
///
/// Near-zero cost when `event_observer` is `None` — `f` is still called but no
/// allocation or split occurs.
pub fn with_reactions<R>(
    bs: &mut BattleState,
    parent: EventKind,
    f: impl FnOnce(&mut BattleState) -> R,
) -> R {
    if bs.event_observer.is_none() {
        return f(bs);
    }
    let start = bs.pending_events.len();
    let result = f(bs);
    let reactions = bs.pending_events.split_off(start);
    bs.pending_events.push(InformationEvent { kind: parent, reactions });
    result
}

/// Emit `Healed` events for a batch collected during a loop where the mutable `iter_mut`
/// borrow prevented inline emission.  Each tuple is `(slot, post-heal hp, max hp)`.
/// No-ops when `event_observer` is `None`.
pub fn emit_healed_batch(bs: &mut BattleState, batch: &[(FieldSlot, u16, u16)]) {
    if let Some(observer) = bs.event_observer {
        for &(slot, hp, max_hp) in batch {
            let new_hp = if slot.player == observer {
                PokemonHP::Number(hp)
            } else {
                PokemonHP::Percent(hp_to_percent(hp, max_hp))
            };
            emit(bs, EventKind::Healed { target: slot, new_hp });
        }
    }
}

/// Convert a raw HP value to the display percentage a real player would see.
/// Matches Showdown's convention: 0 only at faint, 100 only at full HP,
/// otherwise round(hp × 100 / max_hp) clamped to 1–99.
/// Uses integer round-half-up to avoid float nondeterminism.
#[inline]
pub fn hp_to_percent(hp: u16, max_hp: u16) -> u8 {
    let max = max_hp.max(1);
    if hp == 0 { return 0; }
    if hp >= max { return 100; }
    let p = (hp as u32 * 100 + max as u32 / 2) / max as u32;
    p.clamp(1, 99) as u8
}

/// Return `PokemonHP` from the observer's perspective for the given slot:
/// - `Number(exact)` for the observer's own Pokémon
/// - `Percent(display)` for the opponent's Pokémon
pub fn observed_hp(bs: &BattleState, slot: FieldSlot, observer: Player) -> PokemonHP {
    let mon = get_pokemon_at_slot(bs, slot)
        .expect("observed_hp: no mon at slot");
    if slot.player == observer {
        PokemonHP::Number(mon.hp)
    } else {
        PokemonHP::Percent(hp_to_percent(mon.hp, mon.stats[0]))
    }
}

/// Perspective-correct `PokemonHP` from already-captured `(hp, max_hp)` values.
///
/// Use this variant (instead of [`observed_hp`]) when the post-mutation HP has already
/// been read from the Pokémon *before* the mutable borrow was released — e.g. inside
/// the 28+ inline `if slot.player == observer { Number(hp) } else { Percent(...) }`
/// sites that exist throughout `simulator/mod.rs` and `simulator/helpers.rs`.
///
/// Consolidating those sites here removes a whole class of perspective-leak risk: any
/// future mistake would only need to be fixed in one place.
#[inline]
pub fn observed_hp_value(observer: Player, slot_player: Player, hp: u16, max_hp: u16) -> PokemonHP {
    if slot_player == observer {
        PokemonHP::Number(hp)
    } else {
        PokemonHP::Percent(hp_to_percent(hp, max_hp))
    }
}

// ── Move-outcome resolver ─────────────────────────────────────────────────────

/// Player-visible result of a move attempt, used by `note_move_outcome` to emit the
/// right `EventKind` and suppress the `MoveUsed` wrapper when appropriate.
pub enum MoveOutcome {
    /// Pokémon could not act at all (flinch, sleep, paralysis, confusion, etc.).
    /// Emits `Cant { slot, reason }` and sets `move_was_prevented = true` so the
    /// `MoveUsed` wrapper is suppressed (Showdown emits `|cant|` instead of `|move|`).
    Cant(CantReason),
    /// The move executed but had no effect (wrong condition, streak failure, etc.).
    /// Emits `MoveFailed { slot }` — still wrapped under `MoveUsed`.
    Failed,
}

/// Record a move's player-visible outcome on `slot` (always the acting Pokémon).
///
/// - Sets `last_move_failed = true` on the slot's Pokémon (preserving Stomping Tantrum /
///   Micle Berry / stall-counter semantics from before this refactor).
/// - Emits the matching `EventKind` and, for `Cant`, marks `move_was_prevented = true`
///   so `execute_action` suppresses the `MoveUsed` wrapper for that branch.
pub fn note_move_outcome(bs: &mut BattleState, slot: FieldSlot, outcome: MoveOutcome) {
    if let Some(mon) = get_pokemon_at_slot_mut(bs, slot) {
        mon.last_move_failed = true;
    }
    match outcome {
        MoveOutcome::Cant(reason) => {
            bs.move_was_prevented = true;
            emit(bs, EventKind::Cant { slot, reason });
        }
        MoveOutcome::Failed => {
            emit(bs, EventKind::MoveFailed { slot });
        }
    }
}

/// Observable effects from a berry-cure helper (`try_consume_status_cure_berry` or
/// `apply_eaten_berry_effects`) that callers must emit as `InformationEvent`s after
/// releasing the `&mut PokemonState` borrow.
pub(crate) struct BerryCure {
    /// Status that was cured (captured before `mon.status` was cleared).
    pub status_cured: Option<Status>,
    /// Whether `VolatileStatus::Confusion` was cured.
    pub confusion_cured: bool,
    /// Berry item consumed by `try_consume_status_cure_berry` (which clears `mon.item`
    /// itself).  `None` when the caller already manages the held-item slot (e.g.
    /// `force_eat_held_berry` emits `ItemLost` before calling the helper).
    pub item_consumed: Option<Item>,
}

impl BerryCure {
    pub(crate) fn none() -> Self {
        Self { status_cured: None, confusion_cured: false, item_consumed: None }
    }
}

/// Emit the `InformationEvent`s captured in a [`BerryCure`] for the given slot.
/// No-ops when `event_observer` is `None` (inherits the `emit` gate).
pub(crate) fn emit_berry_cure(bs: &mut BattleState, slot: FieldSlot, cure: &BerryCure) {
    if let Some(ref status) = cure.status_cured {
        emit(bs, EventKind::StatusCured { target: slot, status: status.clone() });
    }
    if cure.confusion_cured {
        emit(bs, EventKind::VolatileEnd { target: slot, volatile: VolatileStatus::Confusion });
    }
    if let Some(ref item) = cure.item_consumed {
        emit(bs, EventKind::ItemLost { slot, item: item.clone(), consumed: true });
    }
}

// ─────────────────────────────────────────────────────────────────────────────

pub(crate) fn coalesce_branches<T>(branches: Vec<(T, f64)>) -> Vec<(T, f64)>
where
    T: Eq + Hash + Clone,
{
    let mut combined: HashMap<T, f64> = HashMap::new();

    for (state, probability) in branches {
        if probability <= 0.0 {
            continue;
        }

        *combined.entry(state).or_insert(0.0) += probability;
    }

    let mut merged: Vec<(T, f64)> = combined.into_iter().collect();
    merged.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    merged
}

pub fn permutations<T: Clone>(items: &[T]) -> Vec<Vec<T>> {
    let mut results: Vec<Vec<T>> = Vec::new();

    fn helper<T: Clone>(
        items: &[T],
        current: &mut Vec<T>,
        used: &mut Vec<bool>,
        results: &mut Vec<Vec<T>>,
    ) {
        if current.len() == items.len() {
            results.push(current.clone());
            return;
        }

        for i in 0..items.len() {
            if used[i] {
                continue;
            }

            used[i] = true;
            current.push(items[i].clone());
            helper(items, current, used, results);
            current.pop();
            used[i] = false;
        }
    }

    let mut current: Vec<T> = Vec::new();
    let mut used = vec![false; items.len()];
    helper(items, &mut current, &mut used, &mut results);
    results
}

/// Check if a move has a specific MoveFlag
pub fn move_has_flag(move_data: &MoveData, flag: &MoveFlag) -> bool {
    move_data
        .flags
        .iter()
        .any(|f| std::mem::discriminant(f) == std::mem::discriminant(flag))
}

/// Returns true if Sheer Force should trigger for this move — i.e., the move has at least one
/// target secondary that Sheer Force can remove (status, stat drop on target, volatile, or flinch).
/// User-stat-decrease secondaries (e.g. Draco Meteor's −2 SpA, stored in self_secondaries) are
/// NOT eligible per Gen VI+ rules; the `secondaries` vec only holds target-facing effects,
/// so `!move_data.secondaries.is_empty()` is the correct predicate here.
pub(crate) fn move_has_sheer_force_secondary(move_data: &MoveData) -> bool {
    !move_data.secondaries.is_empty()
}

/// Which protection is blocking an incoming move. Carries the *kind* so callers can apply both
/// the right coverage (King's Shield = damaging-only) and the right contact punishment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtectKind {
    Protect,
    KingsShield,
    SpikyShield,
    BanefulBunker,
    QuickGuard,
    WideGuard,
}

/// Map a stalling protect-family move to the volatile it sets, or `None` if it isn't one.
pub(crate) fn protect_volatile_for_move(m: &PokemonMove) -> Option<VolatileStatus> {
    match m {
        PokemonMove::Protect | PokemonMove::Detect => Some(VolatileStatus::Protect),
        PokemonMove::KingsShield => Some(VolatileStatus::KingsShield),
        PokemonMove::SpikyShield => Some(VolatileStatus::SpikyShield),
        PokemonMove::BanefulBunker => Some(VolatileStatus::BanefulBunker),
        PokemonMove::Endure => Some(VolatileStatus::Endure),
        _ => None,
    }
}

/// True for spread moves (gates Wide Guard) — matches Showdown's `allAdjacent`/`allAdjacentFoes`.
pub(crate) fn move_is_spread_target(target: &MoveTarget) -> bool {
    matches!(
        target,
        MoveTarget::AllAdjacent | MoveTarget::AllAdjacentFoes
    )
}

/// Decide whether an incoming `move_data` from `attacker_slot` targeting `target` is blocked by an
/// active protection on the target's side. Returns the blocking `ProtectKind` (so the caller can
/// apply the correct contact punishment) or `None`. The stall-success roll already happened when the
/// protection was set, so this check is deterministic.
pub(crate) fn protect_blocks_move(
    state: &BattleState,
    attacker_slot: FieldSlot,
    target_slot: FieldSlot,
    target: &PokemonState,
    move_data: &MoveData,
    is_spread: bool,
) -> Option<ProtectKind> {
    // Never block the user's own move or an ally's move.
    if attacker_slot.player == target_slot.player {
        return None;
    }
    // Feint and similar moves ignore all protection.
    if move_data.breaks_protect {
        return None;
    }

    // Side-wide guards: independent of the protect flag and of the stall counter.
    let side_conditions = match target_slot.player {
        Player::P1 => &state.p1_side_conditions,
        Player::P2 => &state.p2_side_conditions,
    };
    if side_conditions
        .iter()
        .any(|c| matches!(c, SideCondition::QuickGuard))
    {
        if let Some(attacker) = get_pokemon_at_slot(state, attacker_slot) {
            if effective_move_priority(state, attacker, move_data) > 0 {
                return Some(ProtectKind::QuickGuard);
            }
        }
    }
    if is_spread
        && side_conditions
            .iter()
            .any(|c| matches!(c, SideCondition::WideGuard))
    {
        return Some(ProtectKind::WideGuard);
    }

    // Single-target self-protects only block moves carrying the protect flag.
    if !move_has_flag(move_data, &MoveFlag::Protect) {
        return None;
    }
    let kind = target.volatiles.iter().find_map(|v| match v {
        VolatileStatusState::TurnStatus(VolatileStatus::Protect, _) => Some(ProtectKind::Protect),
        VolatileStatusState::TurnStatus(VolatileStatus::KingsShield, _) => {
            Some(ProtectKind::KingsShield)
        }
        VolatileStatusState::TurnStatus(VolatileStatus::SpikyShield, _) => {
            Some(ProtectKind::SpikyShield)
        }
        VolatileStatusState::TurnStatus(VolatileStatus::BanefulBunker, _) => {
            Some(ProtectKind::BanefulBunker)
        }
        _ => None,
    })?;
    // King's Shield blocks damaging moves only — status moves land through it.
    if kind == ProtectKind::KingsShield && matches!(move_data.category, MoveCategory::Status) {
        return None;
    }
    Some(kind)
}

/// Apply the on-contact punishment a self-protect inflicts when it blocks a CONTACT move:
/// Spiky Shield → 1/8 of the attacker's max HP, Baneful Bunker → poison, King's Shield → −1 Atk.
/// No-op for Protect/Detect and the side guards. The caller has already confirmed the move makes
/// contact. Deterministic (the protect roll happened when the shield was raised). The status / stat
/// drop route through the normal helpers, so immunities (Steel/Poison, Clear Body) and reactions
/// (Defiant) are respected.
pub(crate) fn apply_protect_contact_punishment(
    state: &mut BattleState,
    attacker_slot: FieldSlot,
    blocker_slot: FieldSlot,
    kind: ProtectKind,
) {
    match kind {
        ProtectKind::SpikyShield => {
            apply_hp_damage_to_attacker(state, attacker_slot, 1, 8);
        }
        ProtectKind::BanefulBunker => {
            let eff = HitEffect {
                status: Some(Status::Poison),
                ..Default::default()
            };
            apply_effect_to_target(
                state,
                blocker_slot,
                attacker_slot,
                &eff,
                attacker_slot.player,
            );
        }
        ProtectKind::KingsShield => {
            let eff = HitEffect {
                boosts: [-1, 0, 0, 0, 0, 0, 0],
                ..Default::default()
            };
            apply_effect_to_target(
                state,
                blocker_slot,
                attacker_slot,
                &eff,
                attacker_slot.player,
            );
        }
        ProtectKind::Protect | ProtectKind::QuickGuard | ProtectKind::WideGuard => {}
    }
}

// --- Damage Calculation Helpers ---

pub fn stage_multiplier(stage: i8) -> f64 {
    let stage = stage.clamp(-6, 6);
    if stage >= 0 {
        (2.0 + stage as f64) / 2.0
    } else {
        2.0 / (2.0 - stage as f64)
    }
}

/// Apply a conditional ×multiplier to `val` when `stat_check` matches and `condition` is true.
fn apply_ability_stat_boost(
    state: &BattleState,
    mon: &PokemonState,
    stat: PokemonStat,
    required_stat: PokemonStat,
    required_ability: Ability,
    condition: bool,
    multiplier: f64,
    val: f64,
) -> f64 {
    if stat == required_stat
        && !pokemon_ability_is_suppressed(state, mon)
        && mon.ability == required_ability
        && condition
    {
        val * multiplier
    } else {
        val
    }
}

pub fn effective_stat(
    state: &BattleState,
    mon: &PokemonState,
    stat: PokemonStat,
    ignore_negative: bool,
    ignore_positive: bool,
) -> f64 {
    let wonder_room_active = state
        .pseudo_weathers
        .iter()
        .any(|pseudo_weather| matches!(pseudo_weather, PseudoWeather::WonderRoom));

    let (stat_index, boost_index) = match stat {
        PokemonStat::Atk => (1, 0),
        PokemonStat::Def if wonder_room_active => (4, 3),
        PokemonStat::SpD if wonder_room_active => (2, 1),
        PokemonStat::Def => (2, 1),
        PokemonStat::SpD => (4, 3),
        PokemonStat::SpA => (3, 2),
        PokemonStat::Spe => (5, 4),
    };

    let base_stat = mon.stats[stat_index] as f64;
    let boost = mon.boosts[boost_index];
    let applied_stage = if boost > 0 && ignore_positive {
        0
    } else if boost < 0 && ignore_negative {
        0
    } else {
        boost
    };

    let val = base_stat * stage_multiplier(applied_stage);
    let val = apply_ability_stat_boost(
        state,
        mon,
        stat,
        PokemonStat::Atk,
        Ability::Guts,
        mon.status.is_some(),
        1.5,
        val,
    );
    let val = apply_ability_stat_boost(
        state,
        mon,
        stat,
        PokemonStat::Def,
        Ability::MarvelScale,
        mon.status.is_some(),
        1.5,
        val,
    );
    let val = apply_ability_stat_boost(
        state,
        mon,
        stat,
        PokemonStat::Def,
        Ability::GrassPelt,
        matches!(current_terrain(state), Some(Terrain::GrassyTerrain)),
        1.5,
        val,
    );
    let val = apply_ability_stat_boost(
        state,
        mon,
        stat,
        PokemonStat::SpA,
        Ability::HadronEngine,
        matches!(current_terrain(state), Some(Terrain::ElectricTerrain)),
        5461.0 / 4096.0,
        val,
    );

    // Huge Power / Pure Power: double Attack stat (unconditional; only physical moves read Atk).
    let val = apply_ability_stat_boost(
        state,
        mon,
        stat,
        PokemonStat::Atk,
        Ability::HugePower,
        true,
        2.0,
        val,
    );
    let val = apply_ability_stat_boost(
        state,
        mon,
        stat,
        PokemonStat::Atk,
        Ability::PurePower,
        true,
        2.0,
        val,
    );
    // Hustle: +50% Attack (accuracy penalty is handled separately in compute_accuracy_modifier_fp).
    let val = apply_ability_stat_boost(
        state,
        mon,
        stat,
        PokemonStat::Atk,
        Ability::Hustle,
        true,
        1.5,
        val,
    );

    // Light Ball: doubles Pikachu's Attack and Special Attack.
    let val = if item_is_active(state, mon)
        && mon.item == Item::LightBall
        && matches!(
            mon.species,
            Species::Pikachu
                | Species::PikachuAlola
                | Species::PikachuBelle
                | Species::PikachuCosplay
                | Species::PikachuGmax
                | Species::PikachuHoenn
                | Species::PikachuKalos
                | Species::PikachuLibre
                | Species::PikachuOriginal
                | Species::PikachuPartner
                | Species::PikachuPhD
                | Species::PikachuPopStar
                | Species::PikachuRockStar
                | Species::PikachuSinnoh
                | Species::PikachuStarter
                | Species::PikachuUnova
                | Species::PikachuWorld
        )
        && (stat == PokemonStat::Atk || stat == PokemonStat::SpA)
    {
        val * 2.0
    } else {
        val
    };

    // Choice Band: 1.5× Attack.
    let val =
        if item_is_active(state, mon) && mon.item == Item::ChoiceBand && stat == PokemonStat::Atk {
            val * 1.5
        } else {
            val
        };

    // Choice Specs: 1.5× Special Attack.
    let val = if item_is_active(state, mon)
        && mon.item == Item::ChoiceSpecs
        && stat == PokemonStat::SpA
    {
        val * 1.5
    } else {
        val
    };

    val
}

/// Foul Play attack stat (non-crit): target's base Atk × target's stage multiplier,
/// then run through the *attacker*'s ability/item/burn multipliers.
/// Foul Play attack stat: reads target's Atk but applies attacker's ability/item/burn multipliers.
/// `is_crit` ignores negative Atk stages on the target; `zero_target_atk` zeroes all stages
/// (used when the defending Pokémon has Unaware).
fn foul_play_attack_stat_inner(
    state: &BattleState,
    attacker: &PokemonState,
    target: &PokemonState,
    is_crit: bool,
    zero_target_atk: bool,
) -> f64 {
    let target_stage = target.boosts[0];
    // Crits ignore negative target Atk stages; Unaware (defender) zeroes all target Atk stages.
    let applied_stage = if zero_target_atk || (is_crit && target_stage < 0) {
        0
    } else {
        target_stage
    };
    let base = target.stats[1] as f64 * stage_multiplier(applied_stage);
    // Apply attacker-side multipliers only (ability, item, burn).
    let val = apply_ability_stat_boost(
        state,
        attacker,
        PokemonStat::Atk,
        PokemonStat::Atk,
        Ability::Guts,
        attacker.status.is_some(),
        1.5,
        base,
    );
    let val = apply_ability_stat_boost(
        state,
        attacker,
        PokemonStat::Atk,
        PokemonStat::Atk,
        Ability::HugePower,
        true,
        2.0,
        val,
    );
    let val = apply_ability_stat_boost(
        state,
        attacker,
        PokemonStat::Atk,
        PokemonStat::Atk,
        Ability::PurePower,
        true,
        2.0,
        val,
    );
    let val = apply_ability_stat_boost(
        state,
        attacker,
        PokemonStat::Atk,
        PokemonStat::Atk,
        Ability::Hustle,
        true,
        1.5,
        val,
    );
    let val = if item_is_active(state, attacker) && attacker.item == Item::ChoiceBand {
        val * 1.5
    } else {
        val
    };
    val
}

pub fn pokemon_has_type(mon: &PokemonState, pokemon_type: &PokemonType) -> bool {
    mon.types.iter().any(|current_type| {
        std::mem::discriminant(current_type) == std::mem::discriminant(pokemon_type)
    })
}

pub fn single_type_effectiveness(move_type: &PokemonType, target_type: &PokemonType) -> f64 {
    use PokemonType::*;

    match (move_type, target_type) {
        (Normal, Steel) => 0.5,
        (Normal, Ghost) => 0.0,
        (Normal, Rock) => 0.5,

        (Fire, Fire) | (Fire, Water) | (Fire, Rock) | (Fire, Dragon) => 0.5,
        (Fire, Grass) | (Fire, Ice) | (Fire, Bug) | (Fire, Steel) => 2.0,

        (Water, Fire) | (Water, Ground) | (Water, Rock) => 2.0,
        (Water, Water) | (Water, Grass) | (Water, Dragon) => 0.5,

        (Electric, Water) | (Electric, Flying) => 2.0,
        (Electric, Electric) | (Electric, Grass) | (Electric, Dragon) => 0.5,
        (Electric, Ground) => 0.0,

        (Grass, Water) | (Grass, Ground) | (Grass, Rock) => 2.0,
        (Grass, Fire)
        | (Grass, Grass)
        | (Grass, Poison)
        | (Grass, Flying)
        | (Grass, Bug)
        | (Grass, Dragon)
        | (Grass, Steel) => 0.5,

        (Ice, Grass) | (Ice, Ground) | (Ice, Flying) | (Ice, Dragon) => 2.0,
        (Ice, Fire) | (Ice, Water) | (Ice, Ice) | (Ice, Steel) => 0.5,

        (Fighting, Normal)
        | (Fighting, Ice)
        | (Fighting, Rock)
        | (Fighting, Dark)
        | (Fighting, Steel) => 2.0,
        (Fighting, Poison)
        | (Fighting, Flying)
        | (Fighting, Psychic)
        | (Fighting, Bug)
        | (Fighting, Fairy) => 0.5,
        (Fighting, Ghost) => 0.0,

        (Poison, Grass) | (Poison, Fairy) => 2.0,
        (Poison, Poison) | (Poison, Ground) | (Poison, Rock) | (Poison, Ghost) => 0.5,
        (Poison, Steel) => 0.0,

        (Ground, Fire)
        | (Ground, Electric)
        | (Ground, Poison)
        | (Ground, Rock)
        | (Ground, Steel) => 2.0,
        (Ground, Grass) | (Ground, Bug) => 0.5,
        (Ground, Flying) => 0.0,

        (Flying, Grass) | (Flying, Fighting) | (Flying, Bug) => 2.0,
        (Flying, Electric) | (Flying, Rock) | (Flying, Steel) => 0.5,

        (Psychic, Fighting) | (Psychic, Poison) => 2.0,
        (Psychic, Psychic) | (Psychic, Steel) => 0.5,
        (Psychic, Dark) => 0.0,

        (Bug, Grass) | (Bug, Psychic) | (Bug, Dark) => 2.0,
        (Bug, Fire)
        | (Bug, Fighting)
        | (Bug, Poison)
        | (Bug, Flying)
        | (Bug, Ghost)
        | (Bug, Steel)
        | (Bug, Fairy) => 0.5,

        (Rock, Fire) | (Rock, Ice) | (Rock, Flying) | (Rock, Bug) => 2.0,
        (Rock, Fighting) | (Rock, Ground) | (Rock, Steel) => 0.5,

        (Ghost, Psychic) | (Ghost, Ghost) => 2.0,
        (Ghost, Dark) => 0.5,
        (Ghost, Normal) => 0.0,

        (Dragon, Dragon) => 2.0,
        (Dragon, Steel) => 0.5,
        (Dragon, Fairy) => 0.0,

        (Dark, Psychic) | (Dark, Ghost) => 2.0,
        (Dark, Fighting) | (Dark, Dark) | (Dark, Fairy) => 0.5,

        (Steel, Ice) | (Steel, Rock) | (Steel, Fairy) => 2.0,
        (Steel, Fire) | (Steel, Water) | (Steel, Electric) | (Steel, Steel) => 0.5,

        (Fairy, Fighting) | (Fairy, Dragon) | (Fairy, Dark) => 2.0,
        (Fairy, Fire) | (Fairy, Poison) | (Fairy, Steel) => 0.5,

        _ => 1.0,
    }
}

/// The types a Pokémon defends with right now. Identical to `mon.types` except while Roost
/// is active on a Flying-type: Roost suppresses the Flying type for the rest of the turn
/// (a dual-type keeps its other type; a pure Flying-type is treated as Normal).
pub fn defensive_types(state: &BattleState, mon: &PokemonState) -> Vec<PokemonType> {
    // Flying is stripped from defensive types when Roost is active on the user, or when
    // Gravity is active on the field (both effects ground Flying-type Pokémon, removing
    // their immunity to Ground-type moves for the duration).
    let strip_flying = (has_status_volatile(mon, &VolatileStatus::Roost)
        || is_gravity_active(state))
        && pokemon_has_type(mon, &PokemonType::Flying);
    if strip_flying {
        let remaining: Vec<PokemonType> = mon
            .types
            .iter()
            .filter(|t| !matches!(t, PokemonType::Flying))
            .cloned()
            .collect();
        if remaining.is_empty() {
            vec![PokemonType::Normal]
        } else {
            remaining
        }
    } else {
        mon.types.clone()
    }
}

pub fn move_type_effectiveness(
    state: &BattleState,
    move_type: &PokemonType,
    target: &PokemonState,
) -> f64 {
    move_type_effectiveness_with_attacker(state, move_type, None, target)
}

/// Type-effectiveness calculation with optional attacker context. When `attacker` is provided,
/// Scrappy (and the equivalent Foresight/Odor Sleuth volatile and Mind's Eye ability) can
/// cause Normal- and Fighting-type moves to treat Ghost-types as non-immune.
pub fn move_type_effectiveness_with_attacker(
    state: &BattleState,
    move_type: &PokemonType,
    attacker: Option<&PokemonState>,
    target: &PokemonState,
) -> f64 {
    let scrappy_applies = attacker.is_some_and(|a| {
        !pokemon_ability_is_suppressed(state, a)
            && matches!(a.ability, Ability::Scrappy | Ability::MindsEye)
    }) || attacker
        .is_some_and(|a| has_status_volatile(a, &VolatileStatus::Foresight));

    let target_types = defensive_types(state, target);
    if target_types.is_empty() {
        return 1.0;
    }

    target_types.iter().fold(1.0, |effectiveness, target_type| {
        // Scrappy / Mind's Eye / Foresight: Normal and Fighting hit Ghost-types normally.
        if scrappy_applies
            && matches!(target_type, PokemonType::Ghost)
            && matches!(move_type, PokemonType::Normal | PokemonType::Fighting)
        {
            return effectiveness * 1.0;
        }
        let mut type_effectiveness = single_type_effectiveness(move_type, target_type);
        if weather_is_strong_winds(state)
            && matches!(target_type, PokemonType::Flying)
            && matches!(
                move_type,
                PokemonType::Electric | PokemonType::Ice | PokemonType::Rock
            )
            && (type_effectiveness - 2.0).abs() < f64::EPSILON
        {
            type_effectiveness = 1.0;
        }
        // Grounded Flying-types (Iron Ball / Gravity / Smack Down) lose their Ground immunity.
        // The early-exit in simulator.rs already skips Ground moves on non-grounded targets;
        // this overrides the type chart 0× so grounded Flying-types take actual damage.
        if matches!(move_type, PokemonType::Ground)
            && matches!(target_type, PokemonType::Flying)
            && type_effectiveness == 0.0
            && pokemon_is_grounded(state, target)
        {
            type_effectiveness = 1.0;
        }
        effectiveness * type_effectiveness
    })
}

/// Flying Press effectiveness = Fighting chart × Flying chart against the target.
/// Strong-winds tempering applies to the Flying component (already inside `move_type_effectiveness`).
/// STAB remains Fighting-only; call this only when the move name is FlyingPress.
/// `attacker` is optional — provide it so Scrappy can bypass Ghost immunity on the Fighting component.
pub fn flying_press_type_effectiveness(
    state: &BattleState,
    attacker: Option<&PokemonState>,
    target: &PokemonState,
) -> f64 {
    move_type_effectiveness_with_attacker(state, &PokemonType::Fighting, attacker, target)
        * move_type_effectiveness(state, &PokemonType::Flying, target)
}

pub fn stab_multiplier(attacker: &PokemonState, move_type: &PokemonType) -> f64 {
    if !pokemon_has_type(attacker, move_type)
        && (!attacker.is_tera || attacker.tera_type != *move_type)
    {
        return 1.0;
    }

    let has_adaptability = attacker.ability == Ability::Adaptability;
    let matches_original_type = pokemon_has_type(attacker, move_type);
    let matches_tera_type = attacker.is_tera && attacker.tera_type == *move_type;
    let tera_type_matches_original =
        attacker.is_tera && pokemon_has_type(attacker, &attacker.tera_type);

    if matches_tera_type {
        if tera_type_matches_original {
            if has_adaptability { 2.25 } else { 2.0 }
        } else if has_adaptability {
            2.0
        } else {
            1.5
        }
    } else if matches_original_type {
        if has_adaptability { 2.0 } else { 1.5 }
    } else {
        1.0
    }
}

pub fn crit_is_prevented(target: &PokemonState) -> bool {
    if target.ability == Ability::BattleArmor || target.ability == Ability::ShellArmor {
        return true;
    }
    false
}

pub fn crit_is_guaranteed(
    attacker: &PokemonState,
    target: &PokemonState,
    move_name: &PokemonMove,
) -> bool {
    let target_is_poisoned = matches!(
        target.status,
        Some(Status::Poison) | Some(Status::ToxicPoison(_))
    );
    let merciless_crit = attacker.ability == Ability::Merciless && target_is_poisoned;
    let laser_focus = attacker.volatiles.iter().any(|volatile| {
        matches!(
            volatile,
            VolatileStatusState::TurnStatus(VolatileStatus::LaserFocus, _)
        ) || matches!(
            volatile,
            VolatileStatusState::MoveStatus(VolatileStatus::LaserFocus, _)
        )
    });
    let always_crit_move = matches!(
        move_name,
        PokemonMove::StormThrow
            | PokemonMove::FrostBreath
            | PokemonMove::ZippyZap
            | PokemonMove::SurgingStrikes
            | PokemonMove::WickedBlow
            | PokemonMove::FlowerTrick
    );

    merciless_crit || laser_focus || always_crit_move
}

/// Returns the effective crit ratio after applying held-item boosts (Scope Lens) and
/// status-based crit-stage boosts (Focus Energy +2, Dragon Cheer +1/+2).
pub(crate) fn effective_crit_ratio(state: &BattleState, attacker: &PokemonState, base: u8) -> u8 {
    let mut ratio = base;
    if item_is_active(state, attacker) && attacker.item == Item::ScopeLens {
        ratio = ratio.saturating_add(1);
    }
    if has_status_volatile(attacker, &VolatileStatus::FocusEnergy) {
        ratio = ratio.saturating_add(2);
    }
    ratio = ratio.saturating_add(dragon_cheer_crit_bonus(attacker));
    if !pokemon_ability_is_suppressed(state, attacker) && attacker.ability == Ability::SuperLuck {
        ratio = ratio.saturating_add(1);
    }
    ratio
}

/// The crit-stage bonus a Pokémon is currently receiving from Dragon Cheer (0 if none).
/// The amount (1 or 2) was locked in when the move was used and is stored on the volatile.
fn dragon_cheer_crit_bonus(mon: &PokemonState) -> u8 {
    mon.volatiles
        .iter()
        .find_map(|v| match v {
            VolatileStatusState::TurnStatus(VolatileStatus::DragonCheer(n), _)
            | VolatileStatusState::MoveStatus(VolatileStatus::DragonCheer(n), _) => Some(*n),
            _ => None,
        })
        .unwrap_or(0)
}

/// The Pokémon's current Stockpile level (0–3). 0 means no Stockpile charge.
pub(crate) fn stockpile_level(mon: &PokemonState) -> u8 {
    mon.volatiles
        .iter()
        .find_map(|v| match v {
            VolatileStatusState::TurnStatus(VolatileStatus::Stockpile(n), _)
            | VolatileStatusState::MoveStatus(VolatileStatus::Stockpile(n), _) => Some(*n),
            _ => None,
        })
        .unwrap_or(0)
}

/// Moves that deal double damage to — and never miss — a Minimized target.
pub(crate) fn move_hits_minimized_harder(move_name: &PokemonMove) -> bool {
    matches!(
        move_name,
        PokemonMove::BodySlam
            | PokemonMove::Stomp
            | PokemonMove::DragonRush
            | PokemonMove::HeatCrash
            | PokemonMove::HeavySlam
            | PokemonMove::FlyingPress
            | PokemonMove::Steamroller
            | PokemonMove::SupercellSlam
            | PokemonMove::MaliciousMoonsault
    )
}

pub fn critical_hit_probability(
    attacker: &PokemonState,
    target: &PokemonState,
    move_name: &PokemonMove,
    consider_crit: bool,
    crit_ratio: u8,
    // Lucky Chant on the target's side blocks ALL crits, including guaranteed-crit moves
    // (Storm Throw, Frost Breath, etc.) and Laser Focus. The check must come before
    // crit_is_guaranteed so that always-crit moves cannot bypass it.
    lucky_chant_active: bool,
) -> Vec<(bool, f64)> {
    if !consider_crit {
        return vec![(false, 1.0)];
    }

    // Lucky Chant: suppress all critical hits — including guaranteed ones.
    if lucky_chant_active {
        return vec![(false, 1.0)];
    }

    if target.ability == Ability::BattleArmor || target.ability == Ability::ShellArmor {
        return vec![(false, 1.0)];
    }

    if crit_is_prevented(target) {
        return vec![(false, 1.0)];
    }
    if crit_is_guaranteed(attacker, target, move_name) {
        return vec![(true, 1.0)];
    }

    // Champions crit-stage odds. `crit_ratio` is 1-indexed (1 = stage +0):
    //   +0 → 1/24, +1 → 1/8, +2 → 1/2, +3 and beyond → guaranteed.
    let crit_chance = match crit_ratio {
        0 | 1 => 1.0 / 24.0,
        2 => 0.125,
        3 => 0.5,
        _ => 1.0,
    };

    vec![(false, 1.0 - crit_chance), (true, crit_chance)]
}

fn screen_damage_multiplier(
    state: &BattleState,
    target_slot: FieldSlot,
    move_data: &MoveData,
    is_crit: bool,
    attacker_has_infiltrator: bool,
) -> f64 {
    if is_crit {
        return 1.0;
    }

    // Brick Break, Psychic Fangs, and Raging Bull bypass screens on the hit that clears them.
    if matches!(
        move_data.name,
        PokemonMove::BrickBreak | PokemonMove::PsychicFangs | PokemonMove::RagingBull
    ) {
        return 1.0;
    }

    // Infiltrator bypasses Reflect, Light Screen, and Aurora Veil entirely.
    if attacker_has_infiltrator {
        return 1.0;
    }

    let target_side_conditions = match target_slot.player {
        Player::P1 => &state.p1_side_conditions,
        Player::P2 => &state.p2_side_conditions,
    };

    let is_physical = matches!(move_data.category, MoveCategory::Physical);
    let is_special = matches!(move_data.category, MoveCategory::Special);

    let has_reflect = target_side_conditions
        .iter()
        .any(|condition| matches!(condition, SideCondition::Reflect));
    let has_light_screen = target_side_conditions
        .iter()
        .any(|condition| matches!(condition, SideCondition::LightScreen));
    let has_aurora_veil = target_side_conditions
        .iter()
        .any(|condition| matches!(condition, SideCondition::AuroraVeil));

    if is_physical && (has_reflect || has_aurora_veil) {
        0.5
    } else if is_special && (has_light_screen || has_aurora_veil) {
        0.5
    } else {
        1.0
    }
}

pub fn selected_damage_rolls(count: u8) -> Vec<u8> {
    let count = count.clamp(1, 16);
    if count == 1 {
        return vec![92];
    }

    (0..count)
        .map(|index| {
            let fraction = index as f64 / (count - 1) as f64;
            let offset = (fraction * 15.0).round() as u8;
            85 + offset
        })
        .collect()
}

pub fn move_offensive_stat(move_data: &MoveData) -> Option<PokemonStat> {
    if let Some(stat) = move_data.override_offensive_stat {
        return Some(stat);
    }

    match move_data.category {
        MoveCategory::Physical => Some(PokemonStat::Atk),
        MoveCategory::Special => Some(PokemonStat::SpA),
        MoveCategory::Status => None,
    }
}

pub fn move_defensive_stat(move_data: &MoveData) -> Option<PokemonStat> {
    if let Some(stat) = move_data.override_defensive_stat {
        return Some(stat);
    }

    match move_data.category {
        MoveCategory::Physical => Some(PokemonStat::Def),
        MoveCategory::Special => Some(PokemonStat::SpD),
        MoveCategory::Status => None,
    }
}

/// Collect non-fainted active slots for `player`, optionally excluding `exclude`.
fn collect_active_slots(
    state: &BattleState,
    player: Player,
    exclude: Option<u8>,
) -> Vec<FieldSlot> {
    let mons = match player {
        Player::P1 => &state.p1_active_mons,
        Player::P2 => &state.p2_active_mons,
    };
    mons.iter()
        .enumerate()
        .filter(|(idx, mon)| !mon.fainted && exclude.map_or(true, |ex| *idx as u8 != ex))
        .map(|(idx, _)| FieldSlot {
            player,
            slot_index: idx as u8,
        })
        .collect()
}

pub fn resolve_move_targets(
    state: &BattleState,
    user_slot: FieldSlot,
    target: &MoveTarget,
) -> Vec<FieldSlot> {
    let foe = match user_slot.player {
        Player::P1 => Player::P2,
        Player::P2 => Player::P1,
    };

    match target {
        // Single-target foe — fallback: first healthy opponent
        MoveTarget::AdjacentFoe | MoveTarget::Normal | MoveTarget::Any => {
            collect_active_slots(state, foe, None)
                .into_iter()
                .take(1)
                .collect()
        }
        // All adjacent foes / foe side
        MoveTarget::AllAdjacentFoes | MoveTarget::FoeSide => collect_active_slots(state, foe, None),
        // Whole-side effects (side conditions like Reflect / Tailwind / Quick Guard / Wide Guard):
        // the user is on the protected side, so include self. Without this, these moves resolve to
        // an empty target list in singles and silently do nothing.
        MoveTarget::AllySide | MoveTarget::AllyTeam => {
            collect_active_slots(state, user_slot.player, None)
        }
        // Partner-targeting moves: exclude the user itself.
        MoveTarget::Allies | MoveTarget::AdjacentAlly => {
            collect_active_slots(state, user_slot.player, Some(user_slot.slot_index))
        }
        // All adjacent (exclude self) or all (include self)
        MoveTarget::All | MoveTarget::AllAdjacent => {
            let exclude_self = matches!(target, MoveTarget::AllAdjacent);
            let mut slots = collect_active_slots(
                state,
                user_slot.player,
                if exclude_self {
                    Some(user_slot.slot_index)
                } else {
                    None
                },
            );
            slots.extend(collect_active_slots(state, foe, None));
            slots
        }
        // Self-target
        MoveTarget::SelfTarget | MoveTarget::AdjacentAllyOrSelf => {
            vec![user_slot]
        }
        // Fallback: first healthy opponent
        _ => collect_active_slots(state, foe, None)
            .into_iter()
            .take(1)
            .collect(),
    }
}

pub fn damage_targets_multiplier(target_count: usize) -> f64 {
    if target_count > 1 { 0.75 } else { 1.0 }
}

/// The move's type after applying only its *own* conditional mechanics (Weather Ball, Terrain
/// Pulse, etc.), but *before* any ability-based type conversion (Aerilate, Liquid Voice, …).
fn natural_move_type(
    state: &BattleState,
    attacker: &PokemonState,
    move_data: &MoveData,
) -> PokemonType {
    match move_data.name {
        PokemonMove::WeatherBall => match weather_for(state, attacker) {
            Some(Weather::Sun | Weather::ExtremeSunlight) => PokemonType::Fire,
            Some(Weather::Rain | Weather::HeavyRain) => PokemonType::Water,
            Some(Weather::Sandstorm) => PokemonType::Rock,
            Some(Weather::Snow) => PokemonType::Ice,
            _ => PokemonType::Normal,
        },
        PokemonMove::TerrainPulse if pokemon_is_grounded(state, attacker) => {
            match current_terrain(state) {
                Some(Terrain::ElectricTerrain) => PokemonType::Electric,
                Some(Terrain::GrassyTerrain) => PokemonType::Grass,
                Some(Terrain::MistyTerrain) => PokemonType::Fairy,
                Some(Terrain::PsychicTerrain) => PokemonType::Psychic,
                _ => move_data.pokemon_type.clone(),
            }
        }
        // Aura Wheel is Electric in Full Belly (Morpeko) and Dark in Hangry (MorpekoHangry).
        // The type is determined by the user's current form at the time of use.
        PokemonMove::AuraWheel => {
            if attacker.species == Species::MorpekoHangry {
                PokemonType::Dark
            } else {
                PokemonType::Electric
            }
        }
        _ => move_data.pokemon_type.clone(),
    }
}

/// Moves whose own type-setting depends on a held item/plate/memory/drive/berry or the user's
/// own type — mechanics the simulator does not yet model. We conservatively skip -ate conversion
/// for these so we don't wrongly boost a move that almost always has a non-Normal type in play.
/// Once their type logic is implemented in `natural_move_type`, remove them from this list.
fn ate_typeset_unmodeled(name: &PokemonMove) -> bool {
    matches!(
        name,
        PokemonMove::Judgment
            | PokemonMove::MultiAttack
            | PokemonMove::TechnoBlast
            | PokemonMove::NaturalGift
            | PokemonMove::RevelationDance
    )
}

/// The type that an -ate ability converts Normal moves into, or `None` for any other ability.
fn ate_ability_target_type(ability: &Ability) -> Option<PokemonType> {
    match ability {
        Ability::Aerilate => Some(PokemonType::Flying),
        Ability::Pixilate => Some(PokemonType::Fairy),
        Ability::Refrigerate => Some(PokemonType::Ice),
        Ability::Dragonize => Some(PokemonType::Dragon),
        Ability::Galvanize => Some(PokemonType::Electric),
        _ => None,
    }
}

/// Returns `true` iff the holder's -ate ability will actually convert `move_data` this use.
/// This single predicate drives *both* the type change and the 1.2× power boost — they must
/// be tied together so the boost only fires when the type actually changes.
fn ate_ability_converts(
    state: &BattleState,
    attacker: &PokemonState,
    move_data: &MoveData,
) -> bool {
    if pokemon_ability_is_suppressed(state, attacker) {
        return false;
    }
    if ate_ability_target_type(&attacker.ability).is_none() {
        return false;
    }
    // Tera Blast sets its own type while Terastallized; skip conversion then.
    if move_data.name == PokemonMove::TeraBlast && attacker.is_tera {
        return false;
    }
    // Moves whose own type-setting is unmodeled — skip to avoid wrong conversions.
    if ate_typeset_unmodeled(&move_data.name) {
        return false;
    }
    // Weather Ball is always exempt from -ate conversion regardless of weather.
    // In active weather it returns a non-Normal type anyway; in clear weather it is Normal
    // but Bulbapedia explicitly states -ate abilities do not affect it in any condition.
    if move_data.name == PokemonMove::WeatherBall {
        return false;
    }
    // Convert only when the move's own-effect-resolved type is still Normal.
    // Terrain Pulse (non-Normal on grounded + active terrain) is handled here too.
    matches!(
        natural_move_type(state, attacker, move_data),
        PokemonType::Normal
    )
}

pub(crate) fn effective_move_type(
    state: &BattleState,
    attacker: &PokemonState,
    move_data: &MoveData,
) -> PokemonType {
    let base = natural_move_type(state, attacker, move_data);
    if pokemon_ability_is_suppressed(state, attacker) {
        return base;
    }
    // Liquid Voice: any sound-based move → Water (no power boost).
    if attacker.ability == Ability::LiquidVoice && move_has_flag(move_data, &MoveFlag::Sound) {
        return PokemonType::Water;
    }
    // -ate abilities: Normal-typed moves → the ability's target type.
    if ate_ability_converts(state, attacker, move_data) {
        return ate_ability_target_type(&attacker.ability).unwrap();
    }
    // Electrify makes the user's move Electric-type for the turn. Among type-changing
    // effects it applies last, so it overrides Normalize/-ate. It does not affect Struggle.
    if move_data.name != PokemonMove::Struggle
        && has_status_volatile(attacker, &VolatileStatus::Electrify)
    {
        return PokemonType::Electric;
    }
    base
}

/// Compute the incremental priority boost contributed by terrain and abilities,
/// *not* including the move's base priority. This separates "what is the base?"
/// from "how much do we add?", so callers can supply their own base:
///
/// - [`effective_move_priority`] uses `move_data.priority` as the base (correct for
///   gameplay — the QM block calls this during move execution).
/// - `compare_action_order` uses the baked `MoveAction.priority` field as the base
///   so that manually-constructed MoveActions in tests (which override `priority`)
///   are respected; in production those fields are always equal to the dex value.
fn effective_priority_boost(state: &BattleState, user: &PokemonState, move_data: &MoveData) -> i8 {
    let mut boost = 0i8;

    // Grassy Glide: +1 priority on Grassy Terrain.
    if move_data.name == PokemonMove::GrassyGlide
        && pokemon_is_on_terrain(state, user, &Terrain::GrassyTerrain)
    {
        boost += 1;
    }

    if !pokemon_ability_is_suppressed(state, user) {
        // Prankster: status moves get +1 priority.
        if user.ability == Ability::Prankster && matches!(move_data.category, MoveCategory::Status)
        {
            boost += 1;
        }

        // Gale Wings: Flying-type moves get +1 priority while the user is at full HP.
        if user.ability == Ability::GaleWings
            && user.hp == user.stats[0].max(1)
            && effective_move_type(state, user, move_data) == PokemonType::Flying
        {
            boost += 1;
        }
    }

    boost
}

/// Compute the effective priority of a move for turn-order purposes, starting
/// from the move's dex base priority (`move_data.priority`).
///
/// This is the canonical function for gameplay code that needs to check effective
/// priority (e.g. the Queenly Majesty per-target block in `possible_damage_outcomes_for_move`).
/// Turn ordering in `compare_action_order` uses the baked `MoveAction.priority` field
/// as the base (via `effective_priority_boost`) rather than re-reading the dex, so
/// that mid-turn HP changes (e.g. Fake Out removing a Gale Wings boost) are
/// reflected correctly at compare time.
pub(crate) fn effective_move_priority(
    state: &BattleState,
    user: &PokemonState,
    move_data: &MoveData,
) -> i8 {
    move_data.priority + effective_priority_boost(state, user, move_data)
}

// ── Damage-calculation sub-helpers ────────────────────────────────────────────

/// Apply SolarPower / OrichalcumPulse attack boosts.
fn apply_weather_attack_boost(
    state: &BattleState,
    attacker: &PokemonState,
    attacking_stat: PokemonStat,
    stat: f64,
) -> f64 {
    let mut stat = stat;
    if matches!(attacking_stat, PokemonStat::SpA)
        && attacker.ability == Ability::SolarPower
        && weather_is_sunlight_for(state, attacker)
    {
        stat = (stat * 1.5).floor();
    }
    if matches!(attacking_stat, PokemonStat::Atk)
        && attacker.ability == Ability::OrichalcumPulse
        && weather_is_sunlight_for(state, attacker)
    {
        stat = (stat * 5461.0 / 4096.0).floor();
    }
    stat
}

/// Apply sandstorm (+Rock SpD) and snow (+Ice Def) weather defense bonuses.
fn apply_weather_defense_bonus(
    state: &BattleState,
    target: &PokemonState,
    defending_stat: PokemonStat,
    defense: f64,
) -> f64 {
    let mut defense = defense;
    if matches!(defending_stat, PokemonStat::SpD)
        && weather_is_sandstorm(state)
        && pokemon_has_type(target, &PokemonType::Rock)
    {
        defense *= 1.5;
    }
    if matches!(defending_stat, PokemonStat::Def)
        && weather_is_snow(state)
        && pokemon_has_type(target, &PokemonType::Ice)
    {
        defense *= 1.5;
    }
    defense
}

/// Terrain-type ×1.3 base-power boost for the attacker's move type.
fn terrain_type_bp_boost(
    state: &BattleState,
    attacker: &PokemonState,
    move_type: &PokemonType,
) -> f64 {
    if pokemon_is_on_terrain(state, attacker, &Terrain::ElectricTerrain)
        && matches!(move_type, PokemonType::Electric)
    {
        return 1.3;
    }
    if pokemon_is_on_terrain(state, attacker, &Terrain::GrassyTerrain)
        && matches!(move_type, PokemonType::Grass)
    {
        return 1.3;
    }
    if pokemon_is_on_terrain(state, attacker, &Terrain::PsychicTerrain)
        && matches!(move_type, PokemonType::Psychic)
    {
        return 1.3;
    }
    1.0
}

/// Per-move terrain multiplier (ExpandingForce, MistyExplosion, Psyblade, TerrainPulse, RisingVoltage, ground moves).
fn move_terrain_bp_modifier(
    state: &BattleState,
    attacker: &PokemonState,
    target: &PokemonState,
    move_data: &MoveData,
) -> f64 {
    match move_data.name {
        PokemonMove::ExpandingForce
            if pokemon_is_on_terrain(state, attacker, &Terrain::PsychicTerrain) =>
        {
            1.5
        }
        PokemonMove::MistyExplosion
            if pokemon_is_on_terrain(state, attacker, &Terrain::MistyTerrain) =>
        {
            1.5
        }
        PokemonMove::Psyblade
            if pokemon_is_on_terrain(state, attacker, &Terrain::ElectricTerrain) =>
        {
            1.5
        }
        PokemonMove::TerrainPulse
            if pokemon_is_grounded(state, attacker) && current_terrain(state).is_some() =>
        {
            2.0
        }
        PokemonMove::RisingVoltage
            if pokemon_is_on_terrain(state, target, &Terrain::ElectricTerrain) =>
        {
            2.0
        }
        PokemonMove::GravApple if is_gravity_active(state) => 1.5,
        PokemonMove::Bulldoze | PokemonMove::Earthquake | PokemonMove::Magnitude
            if matches!(current_terrain(state), Some(Terrain::GrassyTerrain)) =>
        {
            0.5
        }
        _ => 1.0,
    }
}

/// Variable base power for formula-based and conditionally scaled moves. Returns
/// `Some(bp)` to override `move_data.base_power` for this calculation, `None` for every
/// other move. Merged into `base_power_override` in
/// `calculate_damage_outcomes_for_target_with_options`; multi-hit overrides keep
/// precedence (the move sets are disjoint).
fn variable_move_base_power(
    state: &BattleState,
    attacker: &PokemonState,
    target: &PokemonState,
    user_slot: FieldSlot,
    target_slot: FieldSlot,
    move_data: &MoveData,
) -> Option<u16> {
    use crate::data::pokemon_move::PokemonMove as M;
    let bp = move_data.base_power;
    match move_data.name {
        // ── Formula-based ──────────────────────────────────────────────────────────
        // Speed-ratio moves use fully modified Speed (stages, paralysis, items,
        // weather abilities, Tailwind); Trick Room does not apply.
        M::ElectroBall => {
            let user_spe = effective_speed_for_slot(state, user_slot, attacker);
            let target_spe = effective_speed_for_slot(state, target_slot, target);
            // Documented quirk: a 0-Speed target yields the minimum 40 BP.
            Some(if target_spe <= 0.0 {
                40
            } else {
                let r = user_spe / target_spe;
                if r >= 4.0 {
                    150
                } else if r >= 3.0 {
                    120
                } else if r >= 2.0 {
                    80
                } else if r >= 1.0 {
                    60
                } else {
                    40
                }
            })
        }
        M::GyroBall => {
            let user_spe = effective_speed_for_slot(state, user_slot, attacker);
            let target_spe = effective_speed_for_slot(state, target_slot, target);
            // Gen VI+: a user Speed of 0 sets the power to 1 outright.
            Some(if user_spe <= 0.0 {
                1
            } else {
                (((25.0 * target_spe / user_spe).floor() as u16) + 1).min(150)
            })
        }
        M::Eruption | M::WaterSpout => {
            let max_hp = attacker.stats[0].max(1) as u32;
            Some(((150 * attacker.hp as u32 / max_hp) as u16).max(1))
        }
        M::Flail | M::Reversal => {
            let max_hp = attacker.stats[0].max(1) as u32;
            let p = 48 * attacker.hp as u32 / max_hp;
            Some(match p {
                0..=1 => 200,
                2..=4 => 150,
                5..=9 => 100,
                10..=16 => 80,
                17..=32 => 40,
                _ => 20,
            })
        }
        // Target-weight table (weight_hg = kg × 10). Heavy Metal / Light Metal are respected.
        M::GrassKnot | M::LowKick => {
            let attacker_breaks = attacker_breaks_mold(state, attacker);
            let hg = effective_weight_hg(state, target, attacker_breaks);
            Some(if hg >= 2000 {
                120
            } else if hg >= 1000 {
                100
            } else if hg >= 500 {
                80
            } else if hg >= 250 {
                60
            } else if hg >= 100 {
                40
            } else {
                20
            })
        }
        M::HardPress => {
            let max_hp = target.stats[0].max(1) as u32;
            Some(((100 * target.hp as u32 / max_hp) as u16).max(1))
        }
        // Weight-ratio table. (×2 damage + accuracy bypass vs Minimized targets is applied
        // generically in calculate_damage_outcomes_for_target_with_options / accuracy_hit_probability.)
        M::HeatCrash | M::HeavySlam => {
            let attacker_breaks = attacker_breaks_mold(state, attacker);
            // Heavy Metal / Light Metal on the attacker affect the attacker's own weight.
            // Attacker's own ability is never suppressed by its own Mold Breaker.
            let user_w = effective_weight_hg(state, attacker, false);
            // For the target, Mold Breaker on the attacker suppresses target's weight ability.
            let target_w = effective_weight_hg(state, target, attacker_breaks).max(1);
            Some(if user_w >= 5 * target_w {
                120
            } else if user_w >= 4 * target_w {
                100
            } else if user_w >= 3 * target_w {
                80
            } else if user_w >= 2 * target_w {
                60
            } else {
                40
            })
        }
        // ── Conditionally scaled (×2 of the dex base power) ────────────────────────
        // A consumed Flying Gem etc. counts naturally: the item is already None here.
        M::Acrobatics => Some(if attacker.item == Item::None {
            bp * 2
        } else {
            bp
        }),
        // Non-volatile status on the target.
        M::Hex | M::InfernalParade => Some(if target.status.is_some() { bp * 2 } else { bp }),
        // Target is poisoned or badly poisoned (Champions: both conditions double power).
        M::BarbBarrage => Some(
            if matches!(
                target.status,
                Some(Status::Poison) | Some(Status::ToxicPoison(_))
            ) {
                bp * 2
            } else {
                bp
            },
        ),
        // Target took any damage earlier this turn (direct or indirect).
        M::Assurance => Some(if target.damaged_this_turn { bp * 2 } else { bp }),
        // This specific target damaged the user earlier this turn.
        M::Avalanche => Some(if attacker.damaged_by_this_turn.contains(&target_slot) {
            bp * 2
        } else {
            bp
        }),
        // Any of the user's stats actually fell this turn.
        M::LashOut => Some(if attacker.stats_lowered_this_turn {
            bp * 2
        } else {
            bp
        }),
        // Doubled when the target has already taken its action this turn and didn't
        // just switch in (mirrors Showdown's `newlyActive || willMove` check). A switch
        // consumes the target's queued action, so the switched_in flag is what separates
        // "moved already" from "replaced its action with a switch".
        M::Payback => {
            let doubled =
                target_has_acted_this_turn(state, target_slot) && !target.switched_in_this_turn;
            Some(if doubled { bp * 2 } else { bp })
        }
        // 50 + 50 per fainted ally in the user's party. Revival doesn't exist in this
        // simulator, so "times fainted" equals "currently fainted" (cartridge cap of
        // 5050 is unreachable with 6-mon parties).
        M::LastRespects => {
            let (active, back) = match user_slot.player {
                Player::P1 => (&state.p1_active_mons, &state.p1_back_mons),
                Player::P2 => (&state.p2_active_mons, &state.p2_back_mons),
            };
            let fainted = active
                .iter()
                .chain(back.iter())
                .filter(|m| m.fainted)
                .count() as u16;
            Some(50 + 50 * fainted)
        }
        // 20 + 20 per positive boost stage across all seven stats (max 860).
        M::StoredPower | M::PowerTrip => {
            let stages: u16 = attacker.boosts.iter().map(|&b| b.max(0) as u16).sum();
            Some(20 + 20 * stages)
        }
        // Doubled when the user's previous move missed, had no effect, or was
        // prevented (paralysis / sleep / flinch). Recharging does not count.
        M::StompingTantrum | M::TemperFlare => Some(if attacker.last_move_failed {
            bp * 2
        } else {
            bp
        }),
        // Spit Up: 100 / 200 / 300 for Stockpile level 1 / 2 / 3. The fail-on-no-Stockpile
        // check runs before the damage path; the Stockpile charge (and its Def/SpD boosts)
        // are consumed afterwards in apply_post_damage_move_effects, so the level is still
        // readable here.
        M::SpitUp => Some(100 * stockpile_level(attacker) as u16),
        // Round: BP 60 on first cast this turn; 120 for every subsequent Round same turn.
        M::Round => Some(if state.round_used_this_turn {
            bp * 2
        } else {
            bp
        }),
        // Rage Fist: 50 base + 50 per hit taken, capped at 350 (= 7 hits).
        // `attacker.times_hit` is incremented each time this Pokémon is hit by a damaging move
        // and resets on switch-out or faint (Champions rules).
        M::RageFist => Some((50u16 + 50 * attacker.times_hit).min(350)),
        // Rollout / Ice Ball: power doubles each consecutive turn (30→60→120→240→480 over 5 turns).
        // At damage-calc time the post-damage fork has not yet bumped the counter, so n_before is
        // 0,1,2,3,4 on successive turns → bp << n_before = 30,60,120,240,480.
        // If the user holds the DefenseCurl volatile, every hit in the run is doubled again.
        M::Rollout | M::IceBall => {
            let n_before = attacker
                .volatiles
                .iter()
                .find_map(|v| {
                    if let crate::state::pokemon::VolatileStatusState::MoveStatus(
                        VolatileStatus::LockedMove(_),
                        t,
                    ) = v
                    {
                        Some(*t)
                    } else {
                        None
                    }
                })
                .unwrap_or(0);
            let mut power = bp << n_before;
            let has_curl = attacker.volatiles.iter().any(|v| {
                matches!(
                    v,
                    crate::state::pokemon::VolatileStatusState::TurnStatus(VolatileStatus::DefenseCurl, _)
                )
            });
            if has_curl {
                power *= 2;
            }
            Some(power)
        }
        _ => None,
    }
}

/// Effective weight (in hectograms, kg × 10) of a Pokémon after applying Heavy Metal
/// and Light Metal. `mold_break` is true when an opposing Mold Breaker attacker is using
/// a weight-based move against this Pokémon (so its weight ability is ignored).
/// Autotomize (not yet implemented) would also modify the result here.
/// Float Stone halves weight but is not yet implemented.
fn effective_weight_hg(state: &BattleState, mon: &PokemonState, mold_break: bool) -> u32 {
    let base = mon.weight_hg as u32;
    if mold_break || pokemon_ability_is_suppressed(state, mon) {
        return base;
    }
    match mon.ability {
        Ability::HeavyMetal => base.saturating_mul(2),
        Ability::LightMetal => (base / 2).max(1),
        _ => base,
    }
}

/// Compute the effective base power considering all modifiers (weather, terrain, abilities, etc.).
/// Does NOT include the weather damage multiplier (Fire/Water in sun/rain) — that is separate.
fn effective_base_power(
    state: &BattleState,
    attacker: &PokemonState,
    target: &PokemonState,
    move_data: &MoveData,
    base_power_override: Option<u16>,
) -> f64 {
    let mut bp = if let Some(ov) = base_power_override {
        ov as f64
    } else if move_data.name == PokemonMove::WeatherBall {
        if current_weather(state).is_some() {
            100.0
        } else {
            50.0
        }
    } else if move_data.name == PokemonMove::Fling {
        // Fling's power is determined entirely by the held item being thrown.
        attacker.item.fling_power().unwrap_or(0) as f64
    } else {
        move_data.base_power as f64
    };

    if move_data.name == PokemonMove::Facade && attacker.status.is_some() {
        bp = (move_data.base_power as f64 * 2.0).floor();
    }

    // Knock Off gains a 1.5× boost when the target holds an item that can be removed.
    // The boost ignores Sticky Hold (which only prevents the actual removal) but not
    // locked/untransferable items.
    if move_data.name == PokemonMove::KnockOff
        && target.item != Item::None
        && !item_cannot_be_transferred(&target.item, target)
    {
        bp = (bp * 1.5).floor();
    }

    if matches!(
        move_data.name,
        PokemonMove::SolarBeam | PokemonMove::SolarBlade
    ) && !weather_is_sunlight_for(state, attacker)
        && !weather_is_strong_winds(state)
        && weather_for(state, attacker).is_some()
    {
        bp = (bp * 0.5).floor();
    }

    // Technician checks the move's variable/callback base power at THIS point — after
    // the intrinsic-power block above (Facade, WeatherBall, SolarBeam half) but BEFORE
    // terrain and Helping Hand modifiers.  The ×1.5 is applied at the end of the
    // function so it compounds with those later modifiers, matching game behaviour.
    let technician_bp_snapshot = bp;

    bp = (bp * terrain_type_bp_boost(state, attacker, &move_data.pokemon_type)).floor();
    bp = (bp * move_terrain_bp_modifier(state, attacker, target, move_data)).floor();

    // Reckless: ×1.2 to moves with recoil OR crash damage (Jump Kick / High Jump Kick).
    // Struggle is explicitly excluded (its struggleRecoil flag must NOT count here).
    if attacker.ability == Ability::Reckless
        && move_data.name != PokemonMove::Struggle
        && ((move_data.recoil_fraction[0] > 0 && move_data.recoil_fraction[1] > 0)
            || move_data.has_crash_damage)
    {
        bp = (bp * 1.2).floor();
    }

    // -ate abilities grant a 1.2× boost to the moves they convert (Gen 7+ rate).
    // Liquid Voice is intentionally excluded: `ate_ability_converts` returns false for it.
    if ate_ability_converts(state, attacker, move_data) {
        bp = (bp * 1.2).floor();
    }

    // Move-flag-based ability boosts. Suppression guard is shared.
    if !pokemon_ability_is_suppressed(state, attacker) {
        // Iron Fist: 1.2× punching moves.
        if attacker.ability == Ability::IronFist && move_has_flag(move_data, &MoveFlag::Punch) {
            bp = (bp * 1.2).floor();
        }
        // Tough Claws: 1.3× contact moves. Long Reach removes contact entirely (no boost).
        // Protective Pads does NOT suppress this: it only blocks contact-triggered punishment.
        let long_reach = !pokemon_ability_is_suppressed(state, attacker)
            && attacker.ability == Ability::LongReach;
        if attacker.ability == Ability::ToughClaws
            && !long_reach
            && move_has_flag(move_data, &MoveFlag::Contact)
        {
            bp = (bp * 1.3).floor();
        }
        // Strong Jaw: 1.5× biting moves.
        if attacker.ability == Ability::StrongJaw && move_has_flag(move_data, &MoveFlag::Bite) {
            bp = (bp * 1.5).floor();
        }
        // Sharpness: 1.5× slicing moves.
        if attacker.ability == Ability::Sharpness && move_has_flag(move_data, &MoveFlag::Slicing) {
            bp = (bp * 1.5).floor();
        }
        // Mega Launcher: 1.5× pulse/aura moves.
        if attacker.ability == Ability::MegaLauncher && move_has_flag(move_data, &MoveFlag::Pulse) {
            bp = (bp * 1.5).floor();
        }
        // Water Bubble: ×2 power for Water-type moves used by the holder.
        if attacker.ability == Ability::WaterBubble
            && matches!(
                effective_move_type(state, attacker, move_data),
                PokemonType::Water
            )
        {
            bp = (bp * 2.0).floor();
        }
        // Sand Force: ×1.3 for Rock/Ground/Steel moves in sandstorm.
        if attacker.ability == Ability::SandForce
            && weather_is_sandstorm(state)
            && matches!(
                effective_move_type(state, attacker, move_data),
                PokemonType::Rock | PokemonType::Ground | PokemonType::Steel
            )
        {
            bp = (bp * 1.3).floor();
        }
        // Sheer Force: ×(5325/4096) ≈ 1.3× for moves with eligible target secondaries;
        // the secondary effects are suppressed separately in apply_secondary_effects.
        if attacker.ability == Ability::SheerForce && move_has_sheer_force_secondary(move_data) {
            bp = ((bp * 5325.0) / 4096.0).floor();
        }
    }

    // HydroSteam in sun: BP boost (no accompanying damage-type penalty)
    if move_data.name == PokemonMove::HydroSteam && weather_is_sunlight_for(state, attacker) {
        bp = (bp * 1.5).floor();
    }

    // Helping Hand boosts the user's next move by 50%
    if attacker.volatiles.iter().any(|v| {
        matches!(
            v,
            VolatileStatusState::TurnStatus(VolatileStatus::HelpingHand, _)
                | VolatileStatusState::MoveStatus(VolatileStatus::HelpingHand, _)
        )
    }) {
        bp = (bp * 1.5).floor();
    }

    // Condition / stat power boosts (grouped together; suppression guard is shared).
    if !pokemon_ability_is_suppressed(state, attacker) {
        // Rivalry: ×1.25 same gender, ×0.75 opposite gender, ×1.0 if either is Genderless.
        use crate::state::pokemon::PokemonGender;
        let rivalry_mult = match (attacker.gender, target.gender) {
            (PokemonGender::Male, PokemonGender::Male)
            | (PokemonGender::Female, PokemonGender::Female)
                if attacker.ability == Ability::Rivalry =>
            {
                1.25
            }
            (PokemonGender::Male, PokemonGender::Female)
            | (PokemonGender::Female, PokemonGender::Male)
                if attacker.ability == Ability::Rivalry =>
            {
                0.75
            }
            _ => 1.0,
        };
        bp = (bp * rivalry_mult).floor();

        // Low-HP emergency type boosts (Blaze/Overgrow/Swarm/Torrent): ×1.5 when the
        // attacker's HP ≤ 1/3 max AND the move's effective type matches.
        // Note: these are technically Attack-stat multipliers in-game; applying them here as
        // a BP multiplier yields the same final damage and keeps all condition-based BP
        // boosts in one coherent block.
        let at_low_hp = attacker.hp.saturating_mul(3) <= attacker.stats[0].max(1) as u16;
        if at_low_hp {
            let eff_type = effective_move_type(state, attacker, move_data);
            let pinch_mult = match (&attacker.ability, &eff_type) {
                (Ability::Blaze, PokemonType::Fire) => 1.5,
                (Ability::Overgrow, PokemonType::Grass) => 1.5,
                (Ability::Swarm, PokemonType::Bug) => 1.5,
                (Ability::Torrent, PokemonType::Water) => 1.5,
                _ => 1.0,
            };
            bp = (bp * pinch_mult).floor();
        }

        // Flash Fire: ×1.5 power on Fire-type moves when the Flash Fire volatile is active.
        // This stacks with weather/STAB but not with itself (second Fire hit re-grants immunity
        // but does not add a second volatile).
        if has_status_volatile(attacker, &VolatileStatus::FlashFire) {
            let eff_type = effective_move_type(state, attacker, move_data);
            if matches!(eff_type, PokemonType::Fire) {
                bp = (bp * 1.5).floor();
            }
        }

        // Fire Mane: ×1.5 to all Fire-type moves, always (no HP condition, unlike Blaze).
        // Exclusive to Mega Pyroar. Treated as an attack-stat multiplier here (same final
        // damage, keeps all multiplicative boosts in one coherent block).
        if !pokemon_ability_is_suppressed(state, attacker) && attacker.ability == Ability::FireMane
        {
            let eff_type = effective_move_type(state, attacker, move_data);
            if matches!(eff_type, PokemonType::Fire) {
                bp = (bp * 1.5).floor();
            }
        }

        // Charge / Electromorphosis / Wind Power: ×2 for the next Electric-type move.
        // The volatile is consumed (removed from the attacker's list) after the hit in
        // apply_contact_hit_reactions so that the doubling only applies to the first hit.
        // Does not stack: multiple Charge instances have no additional effect.
        if has_status_volatile(attacker, &VolatileStatus::Charge) {
            let eff_type = effective_move_type(state, attacker, move_data);
            if matches!(eff_type, PokemonType::Electric) {
                bp = (bp * 2.0).floor();
            }
        }

        // Technician: ×1.5 for moves with variable base power ≤ 60 (inclusive).
        // The gate uses the snapshot taken before terrain/Helping-Hand modifiers.
        if attacker.ability == Ability::Technician && technician_bp_snapshot <= 60.0 {
            bp = (bp * 1.5).floor();
        }

        // Supreme Overlord: +10% move power per fainted ally, up to +50% (5 allies).
        // The count is snapshotted at switch-in into a permanent TurnStatus(SupremeOverlord(n), 0)
        // volatile, so it correctly reflects the count at the time the Pokémon entered.
        if attacker.ability == Ability::SupremeOverlord {
            let fainted = attacker
                .volatiles
                .iter()
                .find_map(|v| {
                    if let VolatileStatusState::TurnStatus(VolatileStatus::SupremeOverlord(n), _) =
                        v
                    {
                        Some(*n)
                    } else {
                        None
                    }
                })
                .unwrap_or(0);
            if fainted > 0 {
                bp = (bp * (1.0 + 0.1 * fainted as f64)).floor();
            }
        }
    }

    bp
}

/// Weather damage multiplier for Fire/Water in sun/rain (HydroSteam is excluded — its bonus is in base power).
/// Takes `attack_type` (the *effective* type after ability conversion) so that e.g. Liquid Voice
/// sound moves get the rain boost after becoming Water-type, and Weather Ball gets the correct
/// multiplier for its own active-weather type.
/// Uses `weather_for(state, attacker)` so Mega Sol is respected for the attacker's moves.
fn weather_damage_multiplier(
    state: &BattleState,
    attacker: &PokemonState,
    move_data: &MoveData,
    attack_type: &PokemonType,
) -> f64 {
    let Some(weather) = weather_for(state, attacker) else {
        return 1.0;
    };
    match weather {
        Weather::Sun | Weather::ExtremeSunlight => {
            if move_data.name == PokemonMove::HydroSteam {
                1.0
            } else if matches!(attack_type, PokemonType::Fire) {
                1.5
            } else if matches!(attack_type, PokemonType::Water) {
                0.5
            } else {
                1.0
            }
        }
        Weather::Rain | Weather::HeavyRain => {
            if matches!(attack_type, PokemonType::Fire) {
                0.5
            } else if matches!(attack_type, PokemonType::Water) {
                1.5
            } else {
                1.0
            }
        }
        _ => 1.0,
    }
}

/// Burn halves physical damage (not for Guts or Facade).
fn burn_damage_multiplier(attacker: &PokemonState, move_data: &MoveData) -> f64 {
    if matches!(move_data.category, MoveCategory::Physical)
        && matches!(attacker.status, Some(Status::Burn))
        && attacker.ability != Ability::Guts
        && move_data.name != PokemonMove::Facade
    {
        0.5
    } else {
        1.0
    }
}

/// Dry Skin ×1.25 when hit by Fire.
fn dry_skin_fire_multiplier(target: &PokemonState, attack_type: &PokemonType) -> f64 {
    if target.ability == Ability::DrySkin && matches!(attack_type, PokemonType::Fire) {
        1.25
    } else {
        1.0
    }
}

// ──── Defender-side damage reduction abilities ─────────────────────────────

/// Filter / Solid Rock: ×0.75 damage from super-effective hits.
fn filter_solidrock_mult(
    state: &BattleState,
    target: &PokemonState,
    effectiveness: f64,
    mold_breaker: bool,
) -> f64 {
    if effectiveness > 1.0
        && !pokemon_ability_is_suppressed(state, target)
        && !mold_breaker
        && matches!(
            target.ability,
            Ability::Filter | Ability::SolidRock | Ability::PrismArmor
        )
    {
        0.75
    } else {
        1.0
    }
}

/// Multiscale / Shadow Shield: ×0.5 damage when the holder is at full HP.
fn multiscale_mult(state: &BattleState, target: &PokemonState, mold_breaker: bool) -> f64 {
    if !pokemon_ability_is_suppressed(state, target)
        && !mold_breaker
        && matches!(target.ability, Ability::Multiscale | Ability::ShadowShield)
        && target.hp == target.stats[0].max(1)
    {
        0.5
    } else {
        1.0
    }
}

/// Fur Coat: ×0.5 damage from Physical moves.
fn fur_coat_mult(
    state: &BattleState,
    target: &PokemonState,
    move_data: &MoveData,
    mold_breaker: bool,
) -> f64 {
    if !pokemon_ability_is_suppressed(state, target)
        && !mold_breaker
        && target.ability == Ability::FurCoat
        && matches!(move_data.category, MoveCategory::Physical)
    {
        0.5
    } else {
        1.0
    }
}

/// Fluffy: ×0.5 damage from contact moves (negated when the attacker has Long Reach) and
/// ×2 damage from Fire-type moves. The two stack, so a Fire-type contact move is neutral.
/// Kept separate from `defender_type_reduction_mult` because it needs the move's contact flag
/// and the attacker (for Long Reach), which that helper does not take.
fn fluffy_mult(
    state: &BattleState,
    target: &PokemonState,
    attacker: &PokemonState,
    move_data: &MoveData,
    attack_type: &PokemonType,
    mold_breaker: bool,
) -> f64 {
    if pokemon_ability_is_suppressed(state, target)
        || mold_breaker
        || target.ability != Ability::Fluffy
    {
        return 1.0;
    }
    let mut mult = 1.0f64;
    let attacker_long_reach =
        !pokemon_ability_is_suppressed(state, attacker) && attacker.ability == Ability::LongReach;
    if move_has_flag(move_data, &MoveFlag::Contact) && !attacker_long_reach {
        mult *= 0.5;
    }
    if matches!(attack_type, PokemonType::Fire) {
        mult *= 2.0;
    }
    mult
}

/// Defender type-based damage reduction abilities.  Each ability halves damage from one or
/// two attacking types; they compose multiplicatively if somehow stacked.
///
/// - Heatproof       → ×0.5 vs Fire
/// - Thick Fat       → ×0.5 vs Fire and ×0.5 vs Ice
/// - Water Bubble    → ×0.5 vs Fire (defensive half; offensive ×2 Water is in base-power)
/// - Purifying Salt  → ×0.5 vs Ghost
fn defender_type_reduction_mult(
    state: &BattleState,
    target: &PokemonState,
    attack_type: &PokemonType,
    mold_breaker: bool,
) -> f64 {
    if pokemon_ability_is_suppressed(state, target) || mold_breaker {
        return 1.0;
    }
    let mut mult = 1.0f64;
    match target.ability {
        Ability::Heatproof => {
            if matches!(attack_type, PokemonType::Fire) {
                mult *= 0.5;
            }
        }
        Ability::ThickFat => {
            if matches!(attack_type, PokemonType::Fire | PokemonType::Ice) {
                mult *= 0.5;
            }
        }
        Ability::WaterBubble => {
            if matches!(attack_type, PokemonType::Fire) {
                mult *= 0.5;
            }
        }
        Ability::PurifyingSalt => {
            if matches!(attack_type, PokemonType::Ghost) {
                mult *= 0.5;
            }
        }
        _ => {}
    }
    mult
}

/// Friend Guard: an unsuppressed, non-fainted ally with this ability reduces damage to the
/// target by ×0.75 per ally (stacks multiplicatively, matching the in-game rule).
/// The holder itself does NOT benefit from its own Friend Guard.
fn friend_guard_mult(state: &BattleState, target_slot: FieldSlot) -> f64 {
    let target_side = match target_slot.player {
        Player::P1 => &state.p1_active_mons,
        Player::P2 => &state.p2_active_mons,
    };
    let ally_count = target_side
        .iter()
        .enumerate()
        .filter(|(i, ally)| {
            // Not the target itself, not fainted, ability not suppressed.
            *i != target_slot.slot_index as usize
                && !ally.fainted
                && !pokemon_ability_is_suppressed(state, ally)
                && ally.ability == Ability::FriendGuard
        })
        .count();
    0.75f64.powi(ally_count as i32)
}

// ──── Type-boosting held items (1.2×, never consumed) ────────────────────────

/// Maps a type-boosting held item to the move type it boosts.
fn type_boost_item_type(item: &Item) -> Option<PokemonType> {
    Some(match item {
        Item::BlackBelt => PokemonType::Fighting,
        Item::BlackGlasses => PokemonType::Dark,
        Item::Charcoal => PokemonType::Fire,
        Item::DragonFang => PokemonType::Dragon,
        Item::FairyFeather => PokemonType::Fairy,
        Item::HardStone => PokemonType::Rock,
        Item::Magnet => PokemonType::Electric,
        Item::MetalCoat => PokemonType::Steel,
        Item::MiracleSeed => PokemonType::Grass,
        Item::MysticWater => PokemonType::Water,
        Item::NeverMeltIce => PokemonType::Ice,
        Item::PoisonBarb => PokemonType::Poison,
        Item::SharpBeak => PokemonType::Flying,
        Item::SilkScarf => PokemonType::Normal,
        Item::SilverPowder => PokemonType::Bug,
        Item::SoftSand => PokemonType::Ground,
        Item::SpellTag => PokemonType::Ghost,
        Item::TwistedSpoon => PokemonType::Psychic,
        _ => return None,
    })
}

/// 1.2× damage when the attacker holds the type-boosting item matching `attack_type`.
/// Caller must gate on `!items_are_suppressed`.
fn type_boost_item_multiplier(attacker: &PokemonState, attack_type: &PokemonType) -> f64 {
    match type_boost_item_type(&attacker.item) {
        Some(t) if t == *attack_type => 1.2,
        _ => 1.0,
    }
}

/// Multiplicative damage bonus from "universal power items" held by the attacker:
/// Life Orb (×1.3), Expert Belt (×1.2 on super-effective), Muscle Band (×1.1 physical),
/// Wise Glasses (×1.1 special), Metronome item (×(1.0 + 0.2·min(streak,5)), cap ×2.0).
/// Items must already be confirmed active (item_is_active gate applied by caller).
/// Does NOT include type-boosting items (see `type_boost_item_multiplier`).
fn user_power_item_multiplier(
    attacker: &PokemonState,
    category: &MoveCategory,
    effectiveness: f64,
) -> f64 {
    let mut mult = 1.0f64;
    match &attacker.item {
        Item::LifeOrb => {
            // Only boosts moves that go through the damage formula (not fixed-damage moves).
            // Fixed-damage moves have their BP set to 0 before reaching here; they early-return
            // before this function is called, so we always boost here.
            mult *= 5324.0 / 4096.0;
        }
        Item::ExpertBelt => {
            if effectiveness > 1.0 {
                mult *= 4915.0 / 4096.0;
            }
        }
        Item::MuscleBand => {
            if matches!(category, MoveCategory::Physical) {
                mult *= 4505.0 / 4096.0;
            }
        }
        Item::WiseGlasses => {
            if matches!(category, MoveCategory::Special) {
                mult *= 4505.0 / 4096.0;
            }
        }
        Item::Metronome => {
            // streak 0 = first use (×1.0); streak n ≥ 1 = n-th consecutive (×1.2 × n, cap ×2.0).
            let streak = attacker.consecutive_move_count.min(5) as u32;
            if streak > 0 {
                let numerator = 4096u32 + 819 * streak;
                mult *= numerator as f64 / 4096.0;
            }
        }
        _ => {}
    }
    mult
}

// ──── Type-resist berries (0.5× on super-effective hit, then consumed) ────────

/// Maps a type-resist berry to the attacking type it weakens.
/// Chilan Berry is handled separately: triggers on any Normal-type hit (not just SE).
fn resist_berry_type(item: &Item) -> Option<PokemonType> {
    Some(match item {
        Item::BabiriBerry => PokemonType::Steel,
        Item::ChartiBerry => PokemonType::Rock,
        Item::ChopleBerry => PokemonType::Fighting,
        Item::CobaBerry => PokemonType::Flying,
        Item::ColburBerry => PokemonType::Dark,
        Item::HabanBerry => PokemonType::Dragon,
        Item::KasibBerry => PokemonType::Ghost,
        Item::KebiaBerry => PokemonType::Poison,
        Item::OccaBerry => PokemonType::Fire,
        Item::PasshoBerry => PokemonType::Water,
        Item::PayapaBerry => PokemonType::Psychic,
        Item::RindoBerry => PokemonType::Grass,
        Item::RoseliBerry => PokemonType::Fairy,
        Item::ShucaBerry => PokemonType::Ground,
        Item::TangaBerry => PokemonType::Bug,
        Item::WacanBerry => PokemonType::Electric,
        Item::YacheBerry => PokemonType::Ice,
        _ => return None,
    })
}

/// Whether the target's held berry should halve the incoming hit.
/// - Chilan Berry: any Normal-type hit (Struggle is unimplemented; when added, exclude it here).
/// - All other resist berries: only when the move is super-effective (`effectiveness > 1.0`).
/// Caller must gate on `!items_are_suppressed`.
pub(crate) fn resist_berry_triggers(
    target: &PokemonState,
    attack_type: &PokemonType,
    effectiveness: f64,
) -> bool {
    if matches!(target.item, Item::ChilanBerry) && matches!(attack_type, PokemonType::Normal) {
        return true;
    }
    matches!(resist_berry_type(&target.item), Some(t) if t == *attack_type && effectiveness > 1.0)
}

fn resist_berry_multiplier(
    target: &PokemonState,
    attack_type: &PokemonType,
    effectiveness: f64,
) -> f64 {
    if resist_berry_triggers(target, attack_type, effectiveness) {
        0.5
    } else {
        1.0
    }
}

// ──── Status-cure berries ─────────────────────────────────────────────────────

/// If `mon` holds a status-cure berry matching its current status or confusion,
/// cure the condition and consume the berry (set item to None).
/// Must be called with the current item-suppression state.
/// Called after any successful berry consumption. Centralises post-eat side-effects.
pub(crate) fn on_berry_eaten(mon: &mut PokemonState, _eaten: &Item, env: &BerryEnv) {
    mon.ate_berry_this_battle = true;
    // Cheek Pouch: heal ⅓ max HP on top of the berry effect, suppressed by Heal Block.
    if env.ability_active
        && mon.ability == Ability::CheekPouch
        && !has_status_volatile(mon, &VolatileStatus::HealBlock)
    {
        let max_hp = mon.stats[0].max(1);
        heal_mon(mon, max_hp / 3);
    }
    // Cud Chew: arm the delayed re-eat for the following EOT.
    if env.ability_active && mon.ability == Ability::CudChew {
        mon.cud_chew_pending = Some((_eaten.clone(), false));
    }
}

pub(crate) fn try_consume_status_cure_berry(mon: &mut PokemonState, env: &BerryEnv) -> BerryCure {
    let mut cure = BerryCure::none();
    if env.suppressed {
        return cure;
    }
    let cures_status = matches!(
        (&mon.item, &mon.status),
        (Item::AspearBerry, Some(Status::Frozen(_)))
            | (Item::CheriBerry, Some(Status::Paralysis))
            | (Item::ChestoBerry, Some(Status::Sleep(_)))
            | (
                Item::PechaBerry,
                Some(Status::Poison | Status::ToxicPoison(_))
            )
            | (Item::RawstBerry, Some(Status::Burn))
            | (Item::LumBerry, Some(_))
    );
    let cures_confusion =
        is_confused(mon) && matches!(mon.item, Item::PersimBerry | Item::LumBerry);
    if cures_status {
        cure.status_cured = mon.status.clone();
        mon.status = None;
    }
    if cures_confusion {
        cure.confusion_cured = true;
        remove_status_volatile(mon, &VolatileStatus::Confusion);
    }
    if cures_status || cures_confusion {
        cure.item_consumed = Some(mon.item.clone());
        mon.consumed_item = Some(mon.item.clone());
        mon.item = Item::None;
    }
    cure
}

/// Call this whenever a Pokémon gains or re-enables a held item (e.g. Trick/Switcheroo,
/// Magic Room lifting). Triggers any immediate item effects such as status-cure berries.
/// Returns a [`BerryCure`] that the caller must emit after releasing the
/// `&mut PokemonState` borrow.
pub(crate) fn on_item_obtained_or_enabled(mon: &mut PokemonState, env: &BerryEnv) -> BerryCure {
    try_consume_status_cure_berry(mon, env)
}

/// The canonical `(2L/5+2)*BP*Atk/Def/50+2` formula with floor after each step.
pub(crate) fn base_damage_formula(level: u8, bp: f64, attack: f64, defense: f64) -> f64 {
    let mut d = (2.0 * level as f64 / 5.0).floor();
    d = (d + 2.0).floor();
    d = (d * bp).floor();
    d = (d * attack).floor();
    d = (d / defense).floor();
    d = (d / 50.0).floor();
    (d + 2.0).floor()
}

// ──────────────────────────────────────────────────────────────────────────────

/// Calculate damage outcomes for a single target. Returns Vec of (damage, is_crit, probability).
pub fn calculate_damage_outcomes_for_target(
    _state: &BattleState,
    attacker: &PokemonState,
    target: &PokemonState,
    _user_slot: FieldSlot,
    _target_slot: FieldSlot,
    move_data: &MoveData,
    config: crate::simulator::DamageConfig,
    targets_multiplier: f64,
    invulnerability_multiplier: f64,
) -> Vec<(u16, bool, f64)> {
    calculate_damage_outcomes_for_target_with_options(
        _state,
        attacker,
        target,
        _user_slot,
        _target_slot,
        move_data,
        config,
        targets_multiplier,
        invulnerability_multiplier,
        None,
        None,
    )
}

pub(crate) fn calculate_damage_outcomes_for_target_with_options(
    _state: &BattleState,
    attacker: &PokemonState,
    target: &PokemonState,
    _user_slot: FieldSlot,
    _target_slot: FieldSlot,
    move_data: &MoveData,
    config: crate::simulator::DamageConfig,
    targets_multiplier: f64,
    invulnerability_multiplier: f64,
    base_power_override: Option<u16>,
    forced_damage_roll: Option<u8>,
) -> Vec<(u16, bool, f64)> {
    // Shell Side Arm: forecasts physical vs special and uses whichever would deal more.
    // Compares staged Atk/Def vs SpA/SpD via cross-multiplication (no item/ability multipliers
    // per Bulbapedia). Ties resolve deterministically Physical (50/50 tie-branching is a
    // Note: ties are rare in practice and the damage value is identical either way.
    let (attacking_stat, defending_stat) = if move_data.name == PokemonMove::ShellSideArm {
        let phys_a = effective_stat(_state, attacker, PokemonStat::Atk, false, false);
        let phys_d = effective_stat(_state, target, PokemonStat::Def, false, false).max(1.0);
        let spec_a = effective_stat(_state, attacker, PokemonStat::SpA, false, false);
        let spec_d = effective_stat(_state, target, PokemonStat::SpD, false, false).max(1.0);
        if spec_a * phys_d > phys_a * spec_d {
            (PokemonStat::SpA, PokemonStat::SpD) // special wins
        } else {
            (PokemonStat::Atk, PokemonStat::Def) // physical wins (or tie → physical)
        }
    } else {
        let Some(atk) = move_offensive_stat(move_data) else {
            return vec![(0, false, 1.0)];
        };
        let Some(def) = move_defensive_stat(move_data) else {
            return vec![(0, false, 1.0)];
        };
        (atk, def)
    };

    // Pre-compute values that don't change per crit branch.
    // Mold Breaker / Turboblaze / Teravolt: computed early so Unaware gating can use it.
    let mold_breaker = attacker_breaks_mold(_state, attacker);
    // Unaware: when defending, a Pokémon with Unaware ignores the attacker's Atk/SpA stages;
    // when attacking, it ignores the target's Def/SpD stages.
    // Mold Breaker bypasses defender Unaware (but not attacker Unaware — self-ability).
    let defender_unaware = !mold_breaker
        && !pokemon_ability_is_suppressed(_state, target)
        && target.ability == Ability::Unaware;
    let attacker_unaware =
        !pokemon_ability_is_suppressed(_state, attacker) && attacker.ability == Ability::Unaware;
    // Foul Play: use the target's Attack stages but the user's ability/item multipliers.
    let base_attack = if move_data.foul_play {
        apply_weather_attack_boost(
            _state,
            attacker,
            attacking_stat,
            foul_play_attack_stat_inner(_state, attacker, target, false, defender_unaware),
        )
    } else {
        apply_weather_attack_boost(
            _state,
            attacker,
            attacking_stat,
            effective_stat(
                _state,
                attacker,
                attacking_stat,
                defender_unaware,
                defender_unaware,
            ),
        )
    };
    // Plus / Minus: ×1.5 SpA when an ally carries Plus or Minus (either; Gen 9+: same counts).
    let base_attack = if matches!(attacking_stat, PokemonStat::SpA)
        && !pokemon_ability_is_suppressed(_state, attacker)
        && matches!(attacker.ability, Ability::Plus | Ability::Minus)
        && has_plus_minus_partner(_state, _user_slot)
    {
        (base_attack * 1.5).floor()
    } else {
        base_attack
    };
    // ignore_defense_boosts (Sacred Sword, Darkest Lariat): ignore positive defensive stages.
    // Unaware (attacker): also zero all defensive stages (positive and negative).
    let base_defense = apply_weather_defense_bonus(
        _state,
        target,
        defending_stat,
        effective_stat(
            _state,
            target,
            defending_stat,
            attacker_unaware,
            move_data.ignore_defense_boosts || attacker_unaware,
        ),
    );
    let attack_type = effective_move_type(_state, attacker, move_data);
    let effectiveness = {
        let base = if move_data.name == PokemonMove::FlyingPress {
            // Flying Press: Fighting chart × Flying chart (both with Strong Winds tempering).
            // Scrappy applies to the Fighting component; pass attacker for that check.
            flying_press_type_effectiveness(_state, Some(attacker), target)
        } else {
            move_type_effectiveness_with_attacker(_state, &attack_type, Some(attacker), target)
        };
        // Freeze-Dry: Water type is treated as super-effective (2×) regardless of the chart.
        // The chart normally gives 0.5× (Water resists Ice), so the correction factor per Water
        // type in the target's defensive type list is 2.0 / 0.5 = 4.0. This is additive per
        // type, so Water/Ground correctly becomes Ice×Ground(2×) × Ice×Water(override 2×) = 4×.
        if move_data.name == PokemonMove::FreezeDry && attack_type == PokemonType::Ice {
            let water_count = defensive_types(_state, target)
                .iter()
                .filter(|t| **t == PokemonType::Water)
                .count();
            base * 4.0_f64.powi(water_count as i32)
        } else {
            base
        }
    };

    // Counter / Mirror Coat / Metal Burst / Comeuppance: return a multiple of damage taken
    // this turn. All four deal typeless damage — they bypass type immunity entirely (a Ghost
    // can be hit by Counter). Invulnerability still zeroes them out.
    //
    //   Counter       — 2× last physical damage taken
    //   Mirror Coat   — 2× last special damage taken
    //   Metal Burst / Comeuppance — 1.5× most-recent damage of any category
    //
    // Fail logic (no qualifying damage) is handled as an early return upstream in
    // possible_damage_outcomes_for_move; if we reach here, the damage is > 0.
    {
        use crate::data::pokemon_move::PokemonMove as M;
        let retaliation_dmg: Option<u16> = match move_data.name {
            M::Counter => {
                let raw = attacker.last_physical_damage_taken;
                if raw > 0 {
                    Some((raw as u32 * 2).min(u16::MAX as u32) as u16)
                } else {
                    None
                }
            }
            M::MirrorCoat => {
                let raw = attacker.last_special_damage_taken;
                if raw > 0 {
                    Some((raw as u32 * 2).min(u16::MAX as u32) as u16)
                } else {
                    None
                }
            }
            M::MetalBurst | M::Comeuppance => {
                let raw = attacker.last_damage_taken;
                if raw > 0 {
                    Some(((raw as u32 * 3) / 2).max(1).min(u16::MAX as u32) as u16)
                } else {
                    None
                }
            }
            _ => None,
        };
        if let Some(dmg) = retaliation_dmg {
            // Typeless: bypass effectiveness entirely (no immunity, no resistance).
            // Invulnerability (e.g. mid-Fly) still blocks the hit.
            let effective_dmg = if invulnerability_multiplier > 0.0 {
                dmg
            } else {
                0
            };
            return vec![(effective_dmg, false, 1.0)];
        }
    }

    // Super Fang: deals damage equal to ½ of the target's current HP (rounded down, min 1).
    // Ghost-type immunity applies (Normal-type move). No crit / spread / roll scaling.
    if move_data.name == crate::data::pokemon_move::PokemonMove::SuperFang {
        let dmg = if effectiveness > 0.0 && invulnerability_multiplier > 0.0 {
            (target.hp / 2).max(1)
        } else {
            0
        };
        return vec![(dmg, false, 1.0)];
    }

    // Endeavor: reduces the target's HP to match the user's. Deals (target.hp − user.hp)
    // damage; fails (no damage) if user HP ≥ target HP. Ghost immunity applies (Normal-type).
    if move_data.name == crate::data::pokemon_move::PokemonMove::Endeavor {
        let dmg =
            if effectiveness > 0.0 && invulnerability_multiplier > 0.0 && target.hp > attacker.hp {
                target.hp - attacker.hp
            } else {
                0
            };
        return vec![(dmg, false, 1.0)];
    }

    // Final Gambit: deals fixed damage equal to the user's current HP. Type immunity (e.g.
    // Ghost vs Fighting) and invulnerability still zero out the damage. No crit / spread /
    // roll scaling. The user faint is handled in apply_post_damage_move_effects via the
    // SelfDestructType::IfHit path (triggered only when total_dmg > 0).
    if move_data.name == crate::data::pokemon_move::PokemonMove::FinalGambit {
        let dmg = if effectiveness > 0.0 && invulnerability_multiplier > 0.0 {
            attacker.hp
        } else {
            0
        };
        return vec![(dmg, false, 1.0)];
    }

    // One-hit KO moves (Fissure, Guillotine, Horn Drill, Sheer Cold): faint the target
    // outright. Type immunity (Ground vs Flying, Ice vs Ice for Sheer Cold) and
    // invulnerability still zero the hit. Sturdy grants full immunity. Dealing the
    // target's current HP routes Focus Sash / Band / Endure / Disguise through the
    // normal survive-at-1 pipeline. No crit / spread / roll scaling.
    if move_data.ohko {
        let sturdy_immune =
            !pokemon_ability_is_suppressed(_state, target) && target.ability == Ability::Sturdy;
        // Sheer Cold cannot affect Ice-type targets (Gen 7+). This is a move-specific
        // immunity, not a type-chart one (Ice resists Ice at 0.5×, it is not immune).
        let sheer_cold_ice_immune = move_data.name
            == crate::data::pokemon_move::PokemonMove::SheerCold
            && pokemon_has_type(target, &PokemonType::Ice);
        let dmg = if effectiveness > 0.0
            && invulnerability_multiplier > 0.0
            && !sturdy_immune
            && !sheer_cold_ice_immune
        {
            target.hp
        } else {
            0
        };
        return vec![(dmg, false, 1.0)];
    }

    // Fixed-damage moves: bypass the base-power formula entirely.
    // Type immunity still applies (e.g. Night Shade/Ghost vs Normal → 0),
    // as does the invulnerability multiplier. No crit / spread / roll scaling.
    let fixed_damage = match move_data.damage_override {
        DamageOverride::Number(n) => Some(n),
        DamageOverride::Level => Some(attacker.level as u16),
        DamageOverride::None => None,
    };
    if let Some(amount) = fixed_damage {
        let dmg = if effectiveness > 0.0 && invulnerability_multiplier > 0.0 {
            amount
        } else {
            0
        };
        return vec![(dmg, false, 1.0)];
    }

    // Struggle is typeless (???): neutral vs every type, no STAB, hits Ghost.
    // The parser stores its type as Normal — override both effectiveness and STAB here.
    let is_struggle = move_data.name == crate::data::pokemon_move::PokemonMove::Struggle;
    let effectiveness = if is_struggle { 1.0 } else { effectiveness };
    let stab = if is_struggle {
        1.0
    } else {
        stab_multiplier(attacker, &attack_type)
    };
    // Variable-BP moves (Electro Ball, Flail, Assurance, …) compute their power here;
    // explicit overrides (multi-hit per-hit powers) take precedence.
    let base_power_override = base_power_override.or_else(|| {
        variable_move_base_power(
            _state,
            attacker,
            target,
            _user_slot,
            _target_slot,
            move_data,
        )
    });
    let bp = effective_base_power(_state, attacker, target, move_data, base_power_override);

    // Fickle Beam: 30% chance to double power (80 → 160). Fork into two weighted branches.
    // Only when base_power_override is None to avoid infinite recursion on re-entry.
    if move_data.name == crate::data::pokemon_move::PokemonMove::FickleBeam
        && base_power_override.is_none()
    {
        let normal_outcomes = calculate_damage_outcomes_for_target_with_options(
            _state,
            attacker,
            target,
            _user_slot,
            _target_slot,
            move_data,
            config,
            targets_multiplier,
            invulnerability_multiplier,
            Some(move_data.base_power),
            forced_damage_roll,
        );
        let double_outcomes = calculate_damage_outcomes_for_target_with_options(
            _state,
            attacker,
            target,
            _user_slot,
            _target_slot,
            move_data,
            config,
            targets_multiplier,
            invulnerability_multiplier,
            Some(move_data.base_power * 2),
            forced_damage_roll,
        );
        let mut combined = Vec::with_capacity(normal_outcomes.len() + double_outcomes.len());
        for (d, c, p) in normal_outcomes {
            combined.push((d, c, p * 0.7));
        }
        for (d, c, p) in double_outcomes {
            combined.push((d, c, p * 0.3));
        }
        return combined;
    }

    // A genuinely 0-BP hit deals 0 damage — no phantom +2 from the formula,
    // and no min-1 clamp. This covers moves with basePower: 0 and no override.
    if bp == 0.0 {
        return vec![(0, false, 1.0)];
    }
    let weather_mult = weather_damage_multiplier(_state, attacker, move_data, &attack_type);
    let burn_mult = burn_damage_multiplier(attacker, move_data);
    let dry_skin_mult = dry_skin_fire_multiplier(target, &attack_type);
    let type_boost_mult = if !item_is_active(_state, attacker) {
        1.0
    } else {
        type_boost_item_multiplier(attacker, &attack_type)
    };
    let user_power_mult = if !item_is_active(_state, attacker) {
        1.0
    } else {
        user_power_item_multiplier(attacker, &move_data.category, effectiveness)
    };
    let resist_berry_mult = if !item_is_active(_state, target) {
        1.0
    } else {
        let base = resist_berry_multiplier(target, &attack_type, effectiveness);
        // Ripen halves the resist-berry multiplier again (½ → ¼).
        if base < 1.0
            && !pokemon_ability_is_suppressed(_state, target)
            && target.ability == Ability::Ripen
        {
            base / 2.0
        } else {
            base
        }
    };
    let attacker_infiltrator = !pokemon_ability_is_suppressed(_state, attacker)
        && attacker.ability == Ability::Infiltrator;
    let screen_mult =
        screen_damage_multiplier(_state, _target_slot, move_data, false, attacker_infiltrator); // overridden per-crit below

    // Minimize: a handful of moves (Body Slam, Stomp, …) deal double damage to a Minimized
    // target. The accompanying accuracy bypass lives in accuracy_hit_probability.
    let minimize_mult = if has_status_volatile(target, &VolatileStatus::Minimize)
        && move_hits_minimized_harder(&move_data.name)
    {
        2.0
    } else {
        1.0
    };

    // Analytic: ×1.3 when the attacker is the very last mover this turn.
    let analytic_mult = if !pokemon_ability_is_suppressed(_state, attacker)
        && attacker.ability == Ability::Analytic
        && attacker_is_last_mover(_state, _user_slot)
    {
        1.3
    } else {
        1.0
    };

    // Fairy Aura: ×5448/4096 (~1.33) to all Fairy-type moves for any Pokémon on the field
    // when any active mon carries the ability.  Non-stacking.  Aura Break inverts to ×4096/5448.
    let aura_mult = if matches!(attack_type, PokemonType::Fairy) {
        let has_fairy_aura = _state
            .p1_active_mons
            .iter()
            .chain(_state.p2_active_mons.iter())
            .any(|mon| {
                !mon.fainted
                    && !pokemon_ability_is_suppressed(_state, mon)
                    && mon.ability == Ability::FairyAura
            });
        let has_aura_break = _state
            .p1_active_mons
            .iter()
            .chain(_state.p2_active_mons.iter())
            .any(|mon| {
                !mon.fainted
                    && !pokemon_ability_is_suppressed(_state, mon)
                    && mon.ability == Ability::AuraBreak
            });
        if has_fairy_aura && has_aura_break {
            4096.0 / 5448.0
        } else if has_fairy_aura {
            5448.0 / 4096.0
        } else {
            1.0
        }
    } else {
        1.0
    };

    // ── Defender-side damage reduction abilities ──────────────────────────────
    // (mold_breaker was already computed above, before the Unaware pre-compute block.)
    // Filter / Solid Rock / Prism Armor: ×0.75 from super-effective hits.
    let filter_solidrock_mult = filter_solidrock_mult(_state, target, effectiveness, mold_breaker);
    // Multiscale / Shadow Shield: ×0.5 when the target is at full HP.
    let multiscale_mult = multiscale_mult(_state, target, mold_breaker);
    // Fur Coat: ×0.5 from Physical moves.
    let fur_coat_mult = fur_coat_mult(_state, target, move_data, mold_breaker);
    // Heatproof / Thick Fat / Water Bubble / Purifying Salt: type-keyed ×0.5.
    let defender_type_mult =
        defender_type_reduction_mult(_state, target, &attack_type, mold_breaker);
    // Fluffy: ×0.5 from contact moves (unless attacker has Long Reach), ×2 from Fire moves.
    let fluffy_mult = fluffy_mult(
        _state,
        target,
        attacker,
        move_data,
        &attack_type,
        mold_breaker,
    );
    // Friend Guard: ×0.75 per unsuppressed, non-fainted ally carrying the ability.
    let friend_guard_mult = friend_guard_mult(_state, _target_slot);

    let rolls = forced_damage_roll
        .map(|r| vec![r])
        .unwrap_or_else(|| selected_damage_rolls(config.damage_rolls));
    // Lucky Chant is on the DEFENDER'S side — it blocks crits against that side's Pokémon.
    let lucky_chant_on_target_side = {
        let (conditions, _) = match _target_slot.player {
            crate::state::battle::Player::P1 => {
                (&_state.p1_side_conditions, &_state.p1_side_condition_turns)
            }
            crate::state::battle::Player::P2 => {
                (&_state.p2_side_conditions, &_state.p2_side_condition_turns)
            }
        };
        conditions.contains(&crate::state::dex_data::SideCondition::LuckyChant)
    };
    let crits = critical_hit_probability(
        attacker,
        target,
        &move_data.name,
        config.consider_crit,
        effective_crit_ratio(_state, attacker, move_data.crit_ratio),
        lucky_chant_on_target_side,
    );

    let mut outcomes = Vec::new();

    for (is_crit, crit_prob) in crits {
        let crit_mult = if is_crit {
            if attacker.ability == Ability::Sniper {
                2.25
            } else {
                1.5
            }
        } else {
            1.0
        };

        // On a crit, re-compute attack/defense ignoring unfavourable boosts.
        let attack_stat = if is_crit {
            if move_data.foul_play {
                // Crits ignore negative Atk stages on target; Unaware (defender) zeroes all stages.
                apply_weather_attack_boost(
                    _state,
                    attacker,
                    attacking_stat,
                    foul_play_attack_stat_inner(_state, attacker, target, true, defender_unaware),
                )
            } else {
                // Crit: ignore negative attacker stages. Unaware (defender): also ignore positive.
                apply_weather_attack_boost(
                    _state,
                    attacker,
                    attacking_stat,
                    effective_stat(_state, attacker, attacking_stat, true, defender_unaware),
                )
            }
        } else {
            base_attack
        };
        let defense_stat = if is_crit {
            // Crit always ignores positive defensive stages. Unaware (attacker): also ignore negative.
            apply_weather_defense_bonus(
                _state,
                target,
                defending_stat,
                effective_stat(
                    _state,
                    target,
                    defending_stat,
                    move_data.ignore_defense_boosts || attacker_unaware,
                    true,
                ),
            )
        } else {
            base_defense
        };

        let this_screen_mult = if is_crit { 1.0 } else { screen_mult };
        let base_dmg = base_damage_formula(attacker.level, bp, attack_stat, defense_stat);

        for &roll in &rolls {
            let mut dmg = base_dmg;
            dmg = (dmg * targets_multiplier).floor();
            dmg = (dmg * crit_mult).floor();
            dmg = (dmg * (roll as f64 / 100.0)).floor();
            dmg = (dmg * stab).floor();
            dmg = (dmg * effectiveness).floor();
            dmg = (dmg * resist_berry_mult).floor(); // type-resist berry halves after type effectiveness
            dmg = (dmg * this_screen_mult).floor();
            dmg = (dmg * burn_mult).floor();
            dmg = (dmg * invulnerability_multiplier).floor();
            dmg = (dmg * minimize_mult).floor();
            dmg = (dmg * weather_mult).floor();
            dmg = (dmg * dry_skin_mult).floor();
            dmg = (dmg * type_boost_mult).floor(); // type-boosting item in the "other" multiplier bucket
            dmg = (dmg * user_power_mult).floor(); // Life Orb / Expert Belt / Muscle Band / Wise Glasses / Metronome item
            dmg = (dmg * analytic_mult).floor(); // Analytic: ×1.3 when moving last
            dmg = (dmg * aura_mult).floor(); // Fairy Aura / Aura Break field effect
            // Defender-side damage reduction abilities:
            dmg = (dmg * filter_solidrock_mult).floor(); // Filter / Solid Rock / Prism Armor
            dmg = (dmg * multiscale_mult).floor(); // Multiscale / Shadow Shield
            dmg = (dmg * fur_coat_mult).floor(); // Fur Coat
            dmg = (dmg * defender_type_mult).floor(); // Heatproof / Thick Fat / Water Bubble / Purifying Salt
            dmg = (dmg * fluffy_mult).floor(); // Fluffy: ×0.5 contact, ×2 Fire
            dmg = (dmg * friend_guard_mult).floor(); // Friend Guard

            let mut damage = dmg.max(0.0) as u16;
            if damage == 0
                && matches!(
                    move_data.category,
                    MoveCategory::Physical | MoveCategory::Special
                )
                && effectiveness > 0.0
                && invulnerability_multiplier > 0.0
            {
                damage = 1;
            }

            let probability = if forced_damage_roll.is_some() {
                crit_prob
            } else {
                crit_prob / rolls.len() as f64
            };
            outcomes.push((damage, is_crit, probability));
        }
    }

    outcomes
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvulnerabilityResolution {
    Blocked,
    ZeroDamage,
    Normal,
    DoubleDamage,
}

fn target_has_sky_drop_airborne_immunity(target: &PokemonState) -> bool {
    // Only Flying-type grants damage immunity to Sky Drop.
    // Levitate, Magnet Rise, and Telekinesis grant general ground immunity but NOT Sky Drop immunity.
    pokemon_has_type(target, &PokemonType::Flying)
}

fn move_can_hit_sky_drop_target(
    attacker: &PokemonState,
    _target: &PokemonState,
    attack_move: &PokemonMove,
) -> bool {
    // Only No Guard and the specific moves that hit airborne targets bypass Sky Drop invulnerability.
    // Foresight and Miracle Eye do NOT override semi-invulnerable turns per Bulbapedia.
    attacker.ability == Ability::NoGuard
        || matches!(
            attack_move,
            PokemonMove::Gust
                | PokemonMove::Hurricane
                | PokemonMove::SkyUppercut
                | PokemonMove::SmackDown
                | PokemonMove::Thunder
                | PokemonMove::Twister
        )
}

pub fn sky_drop_first_turn_fails(state: &BattleState, target: &PokemonState) -> bool {
    is_gravity_active(state)
        || (item_is_active(state, target) && matches!(target.item, Item::IronBall))
        || has_status_volatile(target, &VolatileStatus::Substitute(0))
}

pub fn move_causes_invulnerability(move_name: &PokemonMove) -> bool {
    matches!(
        move_name,
        PokemonMove::Bounce
            | PokemonMove::Dig
            | PokemonMove::Dive
            | PokemonMove::Fly
            | PokemonMove::PhantomForce
            | PokemonMove::ShadowForce
            | PokemonMove::SkyDrop
    )
}

fn invulnerability_resolution_for_source_move(
    source_move: &PokemonMove,
    attack_move: &PokemonMove,
) -> InvulnerabilityResolution {
    match source_move {
        PokemonMove::Dig => match attack_move {
            PokemonMove::Earthquake | PokemonMove::Magnitude => {
                InvulnerabilityResolution::DoubleDamage
            }
            PokemonMove::Fissure => InvulnerabilityResolution::Normal,
            _ => InvulnerabilityResolution::Blocked,
        },
        PokemonMove::Dive => match attack_move {
            PokemonMove::Surf | PokemonMove::Whirlpool => InvulnerabilityResolution::DoubleDamage,
            _ => InvulnerabilityResolution::Blocked,
        },
        PokemonMove::Fly | PokemonMove::Bounce => match attack_move {
            PokemonMove::Gust | PokemonMove::Twister => InvulnerabilityResolution::DoubleDamage,
            PokemonMove::Thunder
            | PokemonMove::SkyUppercut
            | PokemonMove::SmackDown
            | PokemonMove::Hurricane => InvulnerabilityResolution::Normal,
            _ => InvulnerabilityResolution::Blocked,
        },
        PokemonMove::PhantomForce | PokemonMove::ShadowForce => InvulnerabilityResolution::Blocked,
        _ => InvulnerabilityResolution::Normal,
    }
}

pub fn invulnerability_resolution(
    attacker: &PokemonState,
    target: &PokemonState,
    attack_move: &PokemonMove,
) -> InvulnerabilityResolution {
    let source_move_opt = target.volatiles.iter().find_map(|volatile| match volatile {
        VolatileStatusState::MoveStatus(VolatileStatus::SemiInvulnerable(mov), _) => Some(mov),
        VolatileStatusState::TurnStatus(VolatileStatus::SkyDrop, _) => Some(&PokemonMove::SkyDrop),
        _ => None,
    });

    let Some(source_move) = source_move_opt else {
        return InvulnerabilityResolution::Normal;
    };

    if *source_move == PokemonMove::SkyDrop {
        if *attack_move == PokemonMove::SkyDrop {
            return if target_has_sky_drop_airborne_immunity(target) {
                InvulnerabilityResolution::ZeroDamage
            } else {
                InvulnerabilityResolution::Normal
            };
        }

        if move_can_hit_sky_drop_target(attacker, target, attack_move) {
            return InvulnerabilityResolution::Normal;
        }

        return InvulnerabilityResolution::Blocked;
    }

    let resolution = invulnerability_resolution_for_source_move(source_move, attack_move);

    if matches!(resolution, InvulnerabilityResolution::Blocked)
        && move_can_hit_sky_drop_target(attacker, target, attack_move)
    {
        InvulnerabilityResolution::Normal
    } else {
        resolution
    }
}

pub fn add_invulnerable_volatile(
    mon: &mut PokemonState,
    move_name: PokemonMove,
    _targets: Vec<FieldSlot>,
) {
    let already_has = mon.volatiles.iter().any(|volatile| {
        matches!(
            volatile,
            VolatileStatusState::MoveStatus(VolatileStatus::SemiInvulnerable(_), _)
        )
    });

    if !already_has {
        mon.volatiles.push(VolatileStatusState::MoveStatus(
            VolatileStatus::SemiInvulnerable(move_name),
            0,
        ));
    }
}

pub fn remove_invulnerable_volatile(mon: &mut PokemonState, move_name: &PokemonMove) {
    if let Some(pos) = mon.volatiles.iter().position(|volatile| {
        matches!(
            volatile,
            VolatileStatusState::MoveStatus(VolatileStatus::SemiInvulnerable(mov), _) if mov == move_name
        )
    }) {
        mon.volatiles.remove(pos);
    }
}

pub fn has_status_volatile(mon: &PokemonState, volatile: &VolatileStatus) -> bool {
    mon.volatiles.iter().any(|v| match (v, volatile) {
        (
            VolatileStatusState::TurnStatus(vst, _) | VolatileStatusState::MoveStatus(vst, _),
            vol,
        ) => std::mem::discriminant(vst) == std::mem::discriminant(vol),
        _ => false,
    })
}

/// Remove the given volatile from `mon`'s list (by discriminant, ignoring payload).
/// Returns `true` if a matching volatile was found and removed; `false` if nothing changed.
pub fn remove_status_volatile(mon: &mut PokemonState, volatile: &VolatileStatus) -> bool {
    if let Some(pos) = mon.volatiles.iter().position(|v| match (v, volatile) {
        (
            VolatileStatusState::TurnStatus(vst, _) | VolatileStatusState::MoveStatus(vst, _),
            vol,
        ) => std::mem::discriminant(vst) == std::mem::discriminant(vol),
        _ => false,
    }) {
        mon.volatiles.remove(pos);
        true
    } else {
        false
    }
}

/// Returns the Substitute's current HP (payload of `TurnStatus(Substitute, hp)`), or 0 if
/// the holder has no Substitute. Zero means the sub is absent or has already broken.
pub fn get_substitute_hp(mon: &PokemonState) -> u16 {
    mon.volatiles
        .iter()
        .find_map(|v| {
            if let VolatileStatusState::TurnStatus(VolatileStatus::Substitute(hp), _) = v {
                Some(*hp)
            } else {
                None
            }
        })
        .unwrap_or(0)
}

/// Update the Substitute HP stored in the VolatileStatus variant. No-op if the holder has no sub.
pub fn set_substitute_hp(mon: &mut PokemonState, hp: u16) {
    for volatile in mon.volatiles.iter_mut() {
        if let VolatileStatusState::TurnStatus(VolatileStatus::Substitute(old_hp), _) = volatile {
            *old_hp = hp;
            return;
        }
    }
}

/// True when the incoming attack (from `attacker_slot`) bypasses the target's Substitute.
/// Sound moves, moves with the `bypasssub` flag, and moves used by Infiltrator users all
/// bypass the sub. Self-targeting damage is handled at the call site (never routed to sub).
pub fn attack_bypasses_substitute(
    state: &BattleState,
    attacker_slot: crate::state::battle::FieldSlot,
    move_data: &crate::state::dex_data::MoveData,
) -> bool {
    if move_has_flag(move_data, &crate::state::dex_data::MoveFlag::Sound) {
        return true;
    }
    if move_has_flag(move_data, &crate::state::dex_data::MoveFlag::BypassSub) {
        return true;
    }
    get_pokemon_at_slot(state, attacker_slot)
        .filter(|m| !pokemon_ability_is_suppressed(state, m))
        .map(|m| m.ability == crate::data::ability::Ability::Infiltrator)
        .unwrap_or(false)
}

/// Returns true when `mon` is prevented from switching out voluntarily.
///
/// Binding (`PartiallyTrapped`): Ghost-types ignore the switch-lock but still take chip
/// damage — the volatile stays on them for that purpose; they just cannot be locked in.
/// Pure trapping (`Trapped`): full switch-lock; Ghosts are immune to the move's application
/// in the first place (see `apply_trapping_move`), so they never carry this volatile.
/// Shed Shell bypasses all trapping.
pub fn is_trapped(state: &BattleState, mon: &PokemonState) -> bool {
    if item_is_active(state, mon) && mon.item == Item::ShedShell {
        return false;
    }
    let has_binding = has_status_volatile(mon, &VolatileStatus::PartiallyTrapped(0));
    if has_binding && !pokemon_has_type(mon, &PokemonType::Ghost) {
        return true;
    }
    if has_status_volatile(mon, &VolatileStatus::Trapped(0)) {
        return true;
    }
    // Fairy Lock: all non-Ghost Pokémon cannot voluntarily switch out for the next turn.
    // Shed Shell does not bypass Fairy Lock. Self-switch moves (U-turn, Volt Switch, etc.)
    // are not blocked because they route through self_switch_pending, not this gate.
    if state
        .pseudo_weathers
        .iter()
        .any(|pw| matches!(pw, PseudoWeather::FairyLock))
        && !pokemon_has_type(mon, &PokemonType::Ghost)
    {
        return true;
    }
    // Shadow Tag: adjacent opponents cannot voluntarily switch.
    // Immune: Ghost-types, Shed Shell holders (already returned false above), other Shadow Tag users.
    let mon_has_shadow_tag =
        !pokemon_ability_is_suppressed(state, mon) && mon.ability == Ability::ShadowTag;
    if !pokemon_has_type(mon, &PokemonType::Ghost) && !mon_has_shadow_tag {
        let opp_player = match find_player_of_mon(state, mon) {
            Some(p) => match p {
                Player::P1 => Player::P2,
                Player::P2 => Player::P1,
            },
            None => return false,
        };
        let opponents = collect_active_slots(state, opp_player, None);
        if opponents.iter().any(|slot| {
            get_pokemon_at_slot(state, *slot).is_some_and(|opp| {
                !pokemon_ability_is_suppressed(state, opp) && opp.ability == Ability::ShadowTag
            })
        }) {
            return true;
        }
    }
    false
}

/// Find which player owns `mon` by searching active slots.
fn find_player_of_mon(state: &BattleState, mon: &PokemonState) -> Option<Player> {
    if state.p1_active_mons.iter().any(|m| m.mon_id == mon.mon_id) {
        Some(Player::P1)
    } else if state.p2_active_mons.iter().any(|m| m.mon_id == mon.mon_id) {
        Some(Player::P2)
    } else {
        None
    }
}

/// When a Pokémon leaves the field (switch-out or faint), release any trapping volatiles
/// (`PartiallyTrapped` / `Trapped`) whose source `mon_id` matches the departing Pokémon.
/// Scans all currently-active slots on both sides.
pub fn release_traps_set_by(state: &mut BattleState, source_mon_id: u8) {
    for mon in state
        .p1_active_mons
        .iter_mut()
        .chain(state.p2_active_mons.iter_mut())
    {
        mon.volatiles.retain(|v| {
            !matches!(v,
                VolatileStatusState::TurnStatus(VolatileStatus::PartiallyTrapped(src), _)
                | VolatileStatusState::TurnStatus(VolatileStatus::Trapped(src), _)
                if *src == source_mon_id
            )
        });
    }
}

/// `field_abilities_suppressed` should be the result of `abilities_are_suppressed(state)`.
/// Pass `false` when no battle state is available (e.g. recoil helpers that lack state access).
pub fn apply_damage(mon: &mut PokemonState, damage: u16, field_abilities_suppressed: bool) {
    let hp_before = mon.hp;
    mon.hp = hp_before.saturating_sub(damage);
    mon.fainted = mon.hp == 0;

    // Berserk (project divergence): +1 Sp. Atk when HP crosses from above 50% to ≤ 50%.
    // Triggers on ANY HP loss — move damage, burn, sandstorm, Leech Seed, recoil, etc.
    // (Canon Gen 9 restricts this to direct move damage; this simulator intentionally
    // broadens the trigger per user request.)
    // Skip if the holder fainted; if ability is suppressed via Gastro Acid; or if
    // field-level Neutralizing Gas is active (field_abilities_suppressed).
    if !mon.fainted
        && !field_abilities_suppressed
        && mon.ability == Ability::Berserk
        && !has_status_volatile(mon, &VolatileStatus::GastroAcid)
        && (hp_before as u32) * 2 > (mon.stats[0] as u32)
        && (mon.hp as u32) * 2 <= (mon.stats[0] as u32)
    {
        if mon.boosts[2] < 6 {
            mon.stats_raised_this_turn = true;
        }
        mon.boosts[2] = (mon.boosts[2] + 1).min(6);
    }
}

/// Consume an HP-threshold berry (Oran, Sitrus) if the holder is at ≤ 50% HP.
/// Uses bare `heal_mon` (not `gain_hp`) to avoid re-entrancy into `on_hp_change`.
/// Phase 1 will expand this to handle ≤ 25% pinch/flavor berries.
/// Returns true if the holder's nature reduces the stat at `stat_idx`
/// (0=Atk, 1=Def, 2=SpA, 3=SpD, 4=Spe). Used for flavor-berry confusion.
fn nature_lowers_stat(nature: &Nature, stat_idx: usize) -> bool {
    matches!(
        (nature, stat_idx),
        (
            Nature::Bold | Nature::Modest | Nature::Calm | Nature::Timid,
            0
        ) | (
            Nature::Lonely | Nature::Mild | Nature::Gentle | Nature::Hasty,
            1
        ) | (
            Nature::Adamant | Nature::Impish | Nature::Jolly | Nature::Careful,
            2
        ) | (
            Nature::Naughty | Nature::Lax | Nature::Rash | Nature::Naive,
            3
        ) | (
            Nature::Brave | Nature::Relaxed | Nature::Quiet | Nature::Sassy,
            4
        )
    )
}

fn maybe_apply_berry_confusion(mon: &mut PokemonState, env: &BerryEnv) {
    if env.misty_terrain {
        return;
    }
    if env.ability_active && mon.ability == Ability::OwnTempo {
        return;
    }
    if !is_confused(mon) {
        let duration = thread_rng().gen_range(2..=5);
        mon.volatiles.push(VolatileStatusState::MoveStatus(
            VolatileStatus::Confusion,
            duration,
        ));
    }
}

/// Apply the effect of a consumed berry to its holder.
/// This is decoupled from item-clearing so Cud Chew can re-invoke it without
/// re-consuming the item.
pub(crate) fn apply_berry_effect(mon: &mut PokemonState, berry: &Item, env: &BerryEnv) {
    let ripen = env.ability_active && mon.ability == Ability::Ripen;
    let max_hp = mon.stats[0].max(1);
    match berry {
        Item::OranBerry => {
            heal_mon(mon, if ripen { 20 } else { 10 });
        }
        Item::SitrusBerry => {
            heal_mon(mon, if ripen { max_hp / 2 } else { max_hp / 4 });
        }
        Item::FigyBerry => {
            heal_mon(mon, if ripen { max_hp * 2 / 3 } else { max_hp / 3 });
            if nature_lowers_stat(&mon.nature, 0) {
                maybe_apply_berry_confusion(mon, env);
            }
        }
        Item::WikiBerry => {
            heal_mon(mon, if ripen { max_hp * 2 / 3 } else { max_hp / 3 });
            if nature_lowers_stat(&mon.nature, 2) {
                maybe_apply_berry_confusion(mon, env);
            }
        }
        Item::MagoBerry => {
            heal_mon(mon, if ripen { max_hp * 2 / 3 } else { max_hp / 3 });
            if nature_lowers_stat(&mon.nature, 4) {
                maybe_apply_berry_confusion(mon, env);
            }
        }
        Item::AguavBerry => {
            heal_mon(mon, if ripen { max_hp * 2 / 3 } else { max_hp / 3 });
            if nature_lowers_stat(&mon.nature, 3) {
                maybe_apply_berry_confusion(mon, env);
            }
        }
        Item::IapapaBerry => {
            heal_mon(mon, if ripen { max_hp * 2 / 3 } else { max_hp / 3 });
            if nature_lowers_stat(&mon.nature, 1) {
                maybe_apply_berry_confusion(mon, env);
            }
        }
        Item::LiechiBerry => {
            let s = if ripen { 2 } else { 1 };
            apply_stat_boosts_to_pokemon(mon, &[s, 0, 0, 0, 0, 0, 0], env.suppressed, false);
        }
        Item::GanlonBerry => {
            let s = if ripen { 2 } else { 1 };
            apply_stat_boosts_to_pokemon(mon, &[0, s, 0, 0, 0, 0, 0], env.suppressed, false);
        }
        Item::PetayaBerry => {
            let s = if ripen { 2 } else { 1 };
            apply_stat_boosts_to_pokemon(mon, &[0, 0, s, 0, 0, 0, 0], env.suppressed, false);
        }
        Item::ApicotBerry => {
            let s = if ripen { 2 } else { 1 };
            apply_stat_boosts_to_pokemon(mon, &[0, 0, 0, s, 0, 0, 0], env.suppressed, false);
        }
        Item::SalacBerry => {
            let s = if ripen { 2 } else { 1 };
            apply_stat_boosts_to_pokemon(mon, &[0, 0, 0, 0, s, 0, 0], env.suppressed, false);
        }
        Item::StarfBerry => {
            // Picks one of 5 stats at random (non-branching, same pattern as Confusion duration).
            let s = if ripen { 4 } else { 2 };
            let idx = thread_rng().gen_range(0..5usize);
            let mut boosts = [0i8; 7];
            boosts[idx] = s;
            apply_stat_boosts_to_pokemon(mon, &boosts, env.suppressed, false);
        }
        Item::LansatBerry => {
            if !has_status_volatile(mon, &VolatileStatus::FocusEnergy) {
                mon.volatiles.push(VolatileStatusState::TurnStatus(
                    VolatileStatus::FocusEnergy,
                    0,
                ));
            }
        }
        _ => {}
    }
}

pub(crate) fn try_consume_hp_berry(mon: &mut PokemonState, env: &BerryEnv) {
    if env.suppressed || mon.fainted {
        return;
    }
    let max_hp = mon.stats[0].max(1);
    // Oran/Sitrus fire at ≤50%; pinch/flavor berries fire at ≤25%, or ≤50% with Gluttony.
    let pinch_threshold = if env.ability_active && mon.ability == Ability::Gluttony {
        max_hp / 2
    } else {
        max_hp / 4
    };
    let threshold = match mon.item {
        Item::OranBerry | Item::SitrusBerry => max_hp / 2,
        Item::FigyBerry
        | Item::WikiBerry
        | Item::MagoBerry
        | Item::AguavBerry
        | Item::IapapaBerry
        | Item::LiechiBerry
        | Item::GanlonBerry
        | Item::PetayaBerry
        | Item::ApicotBerry
        | Item::SalacBerry
        | Item::StarfBerry
        | Item::LansatBerry => pinch_threshold,
        _ => return,
    };
    if mon.hp == 0 || mon.hp > threshold {
        return;
    }
    let eaten = mon.item.clone();
    mon.consumed_item = Some(eaten.clone());
    mon.item = Item::None;
    apply_berry_effect(mon, &eaten, env);
    on_berry_eaten(mon, &eaten, env);
}

/// Hook called after any HP change (damage or healing).
/// Future HP-threshold triggers (pinch berries, Berserk ability, etc.) slot in here.
pub(crate) fn on_hp_change(mon: &mut PokemonState, env: &BerryEnv) {
    try_consume_hp_berry(mon, env);
}

/// Consume a Leppa Berry if the holder has any move at 0 PP.
/// Restores 10 PP (capped at the move's max) to the first 0-PP move in slot order.
/// Only considers slots with an actual move assigned (max_pp > 0 — empty slots have max_pp 0).
pub(crate) fn try_consume_leppa_berry(mon: &mut PokemonState, env: &BerryEnv) {
    if env.suppressed || mon.item != Item::LeppaBerry {
        return;
    }
    if let Some(i) = mon
        .move_pp
        .iter()
        .zip(mon.max_pp.iter())
        .position(|(&pp, &max)| pp == 0 && max > 0)
    {
        mon.move_pp[i] = mon.max_pp[i].min(10);
        let eaten = mon.item.clone();
        mon.consumed_item = Some(eaten.clone());
        mon.item = Item::None;
        on_berry_eaten(mon, &eaten, env);
    }
}

/// Consume a White Herb if the holder has any lowered stat stage, restoring all negative
/// stages to 0. Called from `apply_stat_boosts_to_pokemon` whenever the incoming delta
/// contains a negative entry, so it fires from all sources (moves, Intimidate, etc.).
pub(crate) fn try_consume_white_herb(mon: &mut PokemonState, items_suppressed: bool) {
    if items_suppressed || klutz_disables_item(mon) || mon.item != Item::WhiteHerb {
        return;
    }
    if !mon.boosts.iter().any(|&b| b < 0) {
        return;
    }
    for b in mon.boosts.iter_mut() {
        if *b < 0 {
            *b = 0;
        }
    }
    mon.item = Item::None;
}

/// Consume a Mental Herb if the holder is currently afflicted by any of the six "mental"
/// volatile statuses (Attract, Taunt, Encore, Torment, Heal Block, Disable), curing all
/// that are present. Called from `apply_volatile_to_pokemon` after each push, so it fires
/// from all sources (move effects on target or attacker, Cursed Body, etc.).
/// Returns the volatile statuses that were removed by Mental Herb consumption
/// (empty if the herb did not fire). Callers must emit `VolatileEnd` events
/// and `ItemLost{MentalHerb}` for each non-empty return after releasing the
/// `&mut PokemonState` borrow.
pub(crate) fn try_consume_mental_herb(mon: &mut PokemonState, items_suppressed: bool) -> Vec<VolatileStatus> {
    if items_suppressed || klutz_disables_item(mon) || mon.item != Item::MentalHerb {
        return Vec::new();
    }
    let mental_volatiles = [
        VolatileStatus::Attract,
        VolatileStatus::Taunt,
        VolatileStatus::Encore(PokemonMove::Struggle),
        VolatileStatus::Torment,
        VolatileStatus::HealBlock,
        VolatileStatus::Disable(PokemonMove::Struggle),
    ];
    let mut removed: Vec<VolatileStatus> = Vec::new();
    for v in &mental_volatiles {
        if has_status_volatile(mon, v) {
            remove_status_volatile(mon, v);
            removed.push(v.clone());
        }
    }
    if !removed.is_empty() {
        mon.item = Item::None;
    }
    removed
}

/// Returns the set of `(damage_to_apply, consume_item, probability)` outcomes for a direct
/// move hit, accounting for Focus Sash and Focus Band survivability.
///
/// - Normal / non-lethal:         `[(damage, false, 1.0)]`.
/// - Focus Sash at full HP, KO:   `[(hp − 1, true,  1.0)]` — survive at 1 HP, item consumed.
/// - Focus Band, would KO:        `[(damage, false, 0.9), (hp − 1, false, 0.1)]` — 10% survive,
///   not consumed; chance is checked independently on each hit of multi-hit moves.
///
/// Must only be called for direct move hits; residual / recoil / confusion self-damage bypass
/// this so that Sash / Band do not protect against those sources.
pub(crate) fn compute_endure_outcomes(
    target: &PokemonState,
    damage: u16,
    items_suppressed: bool,
    ability_suppressed: bool,
) -> Vec<(u16, bool, f64)> {
    // Endure (volatile): survive any lethal *move* damage at 1 HP, deterministically. It is not an
    // item, so it ignores items_suppressed / Klutz and takes precedence over Focus Sash / Band.
    let has_endure = target.volatiles.iter().any(|v| {
        matches!(
            v,
            VolatileStatusState::TurnStatus(VolatileStatus::Endure, _)
        )
    });
    if has_endure && damage > 0 && !target.fainted && damage >= target.hp {
        return vec![(target.hp.saturating_sub(1), false, 1.0)];
    }
    if items_suppressed
        || klutz_disables_item(target)
        || damage == 0
        || target.fainted
        || damage < target.hp
    {
        return vec![(damage, false, 1.0)];
    }
    // damage >= target.hp: this hit would KO the target
    let survive_damage = target.hp.saturating_sub(1); // leaves 1 HP after taking this amount
    // Sturdy: survive any lethal hit when at full HP (no item consumed; cannot be bypassed
    // by Mold Breaker — that is handled at the call site via ability_suppressed).
    if !ability_suppressed
        && target.ability == Ability::Sturdy
        && target.hp == target.stats[0].max(1)
    {
        return vec![(survive_damage, false, 1.0)];
    }
    match target.item {
        Item::FocusSash if target.hp == target.stats[0].max(1) => {
            // Full-HP requirement: Sash only activates when the holder is at max HP
            vec![(survive_damage, true, 1.0)]
        }
        Item::FocusBand => {
            // 10% chance to survive; not consumed; chance rolled independently per hit
            vec![(damage, false, 0.9), (survive_damage, false, 0.1)]
        }
        _ => vec![(damage, false, 1.0)],
    }
}

/// Apply damage and trigger the HP-change hook. Use this instead of bare `apply_damage`
/// at any call site where item-triggered effects should fire (direct hits, recoil, residual).
///
/// Pass the result of `abilities_are_suppressed(state)` for `field_abilities_suppressed` so
/// that Berserk is correctly disabled while Neutralizing Gas is active.
pub(crate) fn take_damage(
    mon: &mut PokemonState,
    damage: u16,
    env: BerryEnv,
    field_abilities_suppressed: bool,
) {
    if damage == 0 {
        return;
    }
    apply_damage(mon, damage, field_abilities_suppressed);
    if !mon.fainted {
        on_hp_change(mon, &env);
    }
}

/// Heal a Pokémon and trigger the HP-change hook. Use this instead of bare `heal_mon`
/// at any call site where item-triggered effects should fire (drain, weather, moves).
pub(crate) fn gain_hp(mon: &mut PokemonState, amount: u16, env: BerryEnv) {
    if amount == 0 {
        return;
    }
    heal_mon(mon, amount);
    on_hp_change(mon, &env);
}

/// True if `mon` is currently prevented from being healed by Heal Block.
pub(crate) fn heal_is_blocked(mon: &PokemonState) -> bool {
    has_status_volatile(mon, &VolatileStatus::HealBlock)
}

/// True if `mon` holds an active Big Root (item not suppressed / disabled by Klutz).
pub(crate) fn holds_active_big_root(mon: &PokemonState, items_suppressed: bool) -> bool {
    !items_suppressed && !klutz_disables_item(mon) && mon.item == Item::BigRoot
}

/// Scale a heal/drain amount by Big Root when the recipient holds one. Big Root boosts
/// the HP recovered from draining moves, Strength Sap, Leech Seed, Ingrain and Aqua Ring
/// by a factor of 5324/4096 (≈1.3) since Generation V. It does not change the damage dealt
/// to a Leech Seed target, only the amount the seeder recovers (or loses to Liquid Ooze).
pub(crate) fn apply_big_root(mon: &PokemonState, base: u16, items_suppressed: bool) -> u16 {
    if base == 0 || !holds_active_big_root(mon, items_suppressed) {
        return base;
    }
    ((base as u32 * 5324) / 4096) as u16
}

pub fn team_has_remaining_pokemon(state: &BattleState, player: Player) -> bool {
    match player {
        Player::P1 => state
            .p1_active_mons
            .iter()
            .chain(state.p1_back_mons.iter())
            .any(|mon| !mon.fainted),
        Player::P2 => state
            .p2_active_mons
            .iter()
            .chain(state.p2_back_mons.iter())
            .any(|mon| !mon.fainted),
    }
}

pub fn apply_damage_and_check_game_over(
    state: &mut BattleState,
    target_slot: FieldSlot,
    damage: u16,
) -> Option<crate::state::battle::MatchState> {
    let item_active = get_pokemon_at_slot(state, target_slot)
        .map(|m| item_is_active(state, m))
        .unwrap_or(false);
    let target_env = berry_env(state, target_slot);
    // Compute before mutable borrow of state below.
    let field_suppressed = abilities_are_suppressed(state);
    let target_mon = match target_slot.player {
        Player::P1 => state
            .p1_active_mons
            .get_mut(target_slot.slot_index as usize),
        Player::P2 => state
            .p2_active_mons
            .get_mut(target_slot.slot_index as usize),
    }?;

    take_damage(target_mon, damage, target_env, field_suppressed);

    // Capture HP data for DamageDealt event before the borrow of target_mon ends.
    // (NLL ends the borrow at the last *use* of target_mon, not end-of-scope.)
    let post_hp = target_mon.hp;
    let max_hp = target_mon.stats[0];
    let fainted = target_mon.fainted;

    if damage > 0 && item_active && matches!(target_mon.item, Item::AirBalloon) {
        target_mon.item = Item::None;
    }

    if fainted {
        clear_pokemon_on_faint(target_mon);
        // target_mon last used above; NLL allows state borrow below.
        handle_pokemon_faint(state, target_slot.player, target_slot.slot_index);
        // Emit DamageDealt (hp=0 at faint) then Faint as a sibling.
        if let Some(observer) = state.event_observer {
            let new_hp = if target_slot.player == observer {
                PokemonHP::Number(0)
            } else {
                PokemonHP::Percent(0)
            };
            emit(state, EventKind::DamageDealt { target: target_slot, new_hp });
            emit(state, EventKind::Faint { slot: target_slot });
        }
        if !team_has_remaining_pokemon(state, target_slot.player) {
            let winner = match target_slot.player {
                Player::P1 => Player::P2,
                Player::P2 => Player::P1,
            };
            return Some(crate::state::battle::MatchState::GameOverState { winner });
        }
    } else {
        // Emit DamageDealt for non-faint damage.
        if let Some(observer) = state.event_observer {
            let new_hp = if target_slot.player == observer {
                PokemonHP::Number(post_hp)
            } else {
                PokemonHP::Percent(hp_to_percent(post_hp, max_hp))
            };
            emit(state, EventKind::DamageDealt { target: target_slot, new_hp });
        }
    }

    None
}

fn humanize_identifier(value: &str) -> String {
    let mut result = String::new();
    let mut previous: Option<char> = None;
    for current in value.chars() {
        let insert_space = match previous {
            Some(prev) => {
                (prev.is_ascii_lowercase() && current.is_ascii_uppercase())
                    || (prev.is_ascii_digit() && current.is_ascii_alphabetic())
                    || (prev.is_ascii_alphabetic() && current.is_ascii_digit())
            }
            None => false,
        };
        if insert_space && !result.ends_with(' ') {
            result.push(' ');
        }
        result.push(current);
        previous = Some(current);
    }
    result
}

pub fn species_name_sim(species: &crate::data::species::Species) -> String {
    humanize_identifier(&format!("{:?}", species))
}

pub fn move_name_sim(mov: &crate::data::pokemon_move::PokemonMove) -> String {
    humanize_identifier(&format!("{:?}", mov))
}

pub fn pokemon_type_name(pokemon_type: &PokemonType) -> &'static str {
    match pokemon_type {
        PokemonType::Normal => "Normal",
        PokemonType::Fire => "Fire",
        PokemonType::Water => "Water",
        PokemonType::Electric => "Electric",
        PokemonType::Grass => "Grass",
        PokemonType::Ice => "Ice",
        PokemonType::Fighting => "Fighting",
        PokemonType::Poison => "Poison",
        PokemonType::Ground => "Ground",
        PokemonType::Flying => "Flying",
        PokemonType::Psychic => "Psychic",
        PokemonType::Bug => "Bug",
        PokemonType::Rock => "Rock",
        PokemonType::Ghost => "Ghost",
        PokemonType::Dragon => "Dragon",
        PokemonType::Dark => "Dark",
        PokemonType::Steel => "Steel",
        PokemonType::Fairy => "Fairy",
    }
}

pub fn move_target_is_multitarget(target: &MoveTarget) -> bool {
    matches!(
        target,
        MoveTarget::All
            | MoveTarget::AllAdjacent
            | MoveTarget::AllAdjacentFoes
            | MoveTarget::Allies
            | MoveTarget::AllySide
            | MoveTarget::AllyTeam
            | MoveTarget::FoeSide
    )
}

pub fn is_gravity_active(state: &BattleState) -> bool {
    state
        .pseudo_weathers
        .iter()
        .any(|effect| matches!(effect, PseudoWeather::Gravity))
}

fn weather_is_suspended(state: &BattleState) -> bool {
    if abilities_are_suppressed(state) {
        return false;
    }

    state
        .p1_active_mons
        .iter()
        .chain(state.p2_active_mons.iter())
        .any(|mon| {
            !mon.fainted && (mon.ability == Ability::AirLock || mon.ability == Ability::CloudNine)
        })
}

pub fn current_weather(state: &BattleState) -> Option<Weather> {
    if weather_is_suspended(state) {
        return None;
    }
    state.weather.clone()
}

pub fn current_terrain(state: &BattleState) -> Option<Terrain> {
    state.terrain.clone()
}

pub fn items_are_suppressed(state: &BattleState) -> bool {
    state
        .pseudo_weathers
        .iter()
        .any(|pseudo_weather| matches!(pseudo_weather, PseudoWeather::MagicDeluge))
}

/// Whether `mon`'s held item currently has any effect. Combines the global Magic Room
/// gate with the per-Pokémon Klutz gate (Klutz itself can be suppressed by Gastro Acid /
/// Neutralizing Gas, which re-enables the item).
pub fn item_is_active(state: &BattleState, mon: &PokemonState) -> bool {
    !items_are_suppressed(state)
        && !(mon.ability == Ability::Klutz && !pokemon_ability_is_suppressed(state, mon))
}

/// Mon-level Klutz check for call sites that only have the `PokemonState` (no
/// `BattleState`). Gastro Acid suppression of Klutz re-enables the item. (Neutralizing
/// Gas would also re-enable it, but field state is not visible here — corner case.)
pub(crate) fn klutz_disables_item(mon: &PokemonState) -> bool {
    mon.ability == Ability::Klutz && !has_status_volatile(mon, &VolatileStatus::GastroAcid)
}

// ── Berry consumption context ─────────────────────────────────────────────────

/// Context for berry consumption at a specific field slot. Pre-computed (via
/// [`berry_env`]) before any mutable borrow to avoid split-borrow issues.
///
/// - `suppressed`: items are globally suppressed (Magic Room), OR the opposing
///   side has an active, non-suppressed Unnerve user. Either prevents the holder
///   from eating a berry.
/// - `ability_active`: the holder's *own* ability is not suppressed (gates
///   Gluttony / Ripen / Cheek Pouch / Cud Chew).
/// - `misty_terrain`: Misty Terrain is active (blocks flavor-berry confusion).
#[derive(Clone, Copy)]
pub(crate) struct BerryEnv {
    pub suppressed: bool,
    pub ability_active: bool,
    pub misty_terrain: bool,
}

impl BerryEnv {
    /// Construct with only items_suppressed context; ability effects won't fire.
    /// Use for switch-out healing and other contexts without per-slot ability info.
    pub fn simple(items_suppressed: bool) -> Self {
        BerryEnv {
            suppressed: items_suppressed,
            ability_active: false,
            misty_terrain: false,
        }
    }
}

/// Whether any active, non-suppressed Pokémon on the *opposing* side has Unnerve.
fn opposing_unnerve_active(state: &BattleState, slot: FieldSlot) -> bool {
    let opposing_mons = match slot.player {
        Player::P1 => &state.p2_active_mons,
        Player::P2 => &state.p1_active_mons,
    };
    opposing_mons.iter().any(|mon| {
        !mon.fainted
            && mon.ability == Ability::Unnerve
            && !pokemon_ability_is_suppressed(state, mon)
    })
}

/// Build the [`BerryEnv`] for a field slot from the current battle state.
/// Call this before any mutable borrow of `state`.
pub(crate) fn berry_env(state: &BattleState, slot: FieldSlot) -> BerryEnv {
    // Per-mon gate: Magic Room, or the holder's own (unsuppressed) Klutz.
    let item_inactive = get_pokemon_at_slot(state, slot)
        .map(|mon| !item_is_active(state, mon))
        .unwrap_or_else(|| items_are_suppressed(state));
    let unnerve = if item_inactive {
        false
    } else {
        opposing_unnerve_active(state, slot)
    };
    let ability_active = get_pokemon_at_slot(state, slot)
        .map(|mon| !pokemon_ability_is_suppressed(state, mon))
        .unwrap_or(false);
    let misty_terrain = matches!(state.terrain, Some(Terrain::MistyTerrain));
    BerryEnv {
        suppressed: item_inactive || unnerve,
        ability_active,
        misty_terrain,
    }
}

// ── Item-loss ledger (Unburden / Pickup / Symbiosis) ──────────────────────────
//
// Berry consumption happens inside mon-level functions (`take_damage`, `gain_hp`,
// `try_consume_*`) that have no `BattleState` access, so item-loss reactions are
// driven by a snapshot-diff sweep instead of call-site hooks: snapshot held items
// before an action resolves, then diff afterwards. Theft (`try_steal_item`) handles
// its own bookkeeping and is skipped by the sweep via the `item_lost` flag.

/// Snapshot the held item and HP of every active Pokémon, for `process_item_loss_events`.
/// Species is recorded so slots whose occupant changed mid-action (switches) are skipped.
pub(crate) fn snapshot_active_items(state: &BattleState) -> Vec<(FieldSlot, Species, Item, u16)> {
    let mut v = Vec::new();
    for (i, mon) in state.p1_active_mons.iter().enumerate() {
        v.push((
            FieldSlot {
                player: Player::P1,
                slot_index: i as u8,
            },
            mon.species.clone(),
            mon.item.clone(),
            mon.hp,
        ));
    }
    for (i, mon) in state.p2_active_mons.iter().enumerate() {
        v.push((
            FieldSlot {
                player: Player::P2,
                slot_index: i as u8,
            },
            mon.species.clone(),
            mon.item.clone(),
            mon.hp,
        ));
    }
    v
}

/// Diff current held items against a `snapshot_active_items` snapshot and fire item-loss
/// reactions: set `item_lost` (Unburden), record the item for Pickup (popped Air Balloons
/// excluded), and run Symbiosis. Mons whose `item_lost` flag is already set (theft) and
/// slots whose occupant changed (switches) are skipped.
///
/// Also diffs HP: any decrease marks `damaged_this_turn` (Assurance), catching indirect
/// damage (recoil, crash, confusion self-hits) that bypasses `apply_single_hit_branch`.
pub(crate) fn process_item_loss_events(
    state: &mut BattleState,
    before: &[(FieldSlot, Species, Item, u16)],
) {
    for (slot, prev_species, prev_item, prev_hp) in before {
        // HP diff → per-turn damage flag (occupant must be unchanged).
        let hp_dropped = get_pokemon_at_slot(state, *slot)
            .is_some_and(|m| m.species == *prev_species && m.hp < *prev_hp);
        if hp_dropped {
            if let Some(m) = get_pokemon_at_slot_mut(state, *slot) {
                m.damaged_this_turn = true;
            }
        }

        if *prev_item == Item::None {
            continue;
        }
        let lost_now = match get_pokemon_at_slot(state, *slot) {
            Some(m) => m.species == *prev_species && m.item == Item::None && !m.item_lost,
            None => false,
        };
        if !lost_now {
            continue;
        }
        if let Some(m) = get_pokemon_at_slot_mut(state, *slot) {
            m.item_lost = true;
        }
        // Popped Air Balloons are destroyed, not used — Pickup cannot retrieve them.
        let is_consumed = *prev_item != Item::AirBalloon;
        if is_consumed {
            state
                .items_consumed_this_turn
                .push((*slot, prev_item.clone()));
        }
        // Emit ItemLost; Symbiosis pass (if any) emits nested reactions inside.
        with_reactions(
            state,
            EventKind::ItemLost { slot: *slot, item: prev_item.clone(), consumed: is_consumed },
            |bs| try_symbiosis_pass(bs, *slot),
        );
    }
}

/// Symbiosis: the Pokémon at `receiver_slot` just used up its held item — an ally with
/// (unsuppressed) Symbiosis immediately passes its own held item over. With several
/// eligible allies, slot order decides (cartridge uses speed order — simplification).
fn try_symbiosis_pass(state: &mut BattleState, receiver_slot: FieldSlot) {
    let receiver_ok = get_pokemon_at_slot(state, receiver_slot)
        .map(|m| !m.fainted && m.item == Item::None)
        .unwrap_or(false);
    if !receiver_ok {
        return;
    }

    let donor_slot =
        collect_active_slots(state, receiver_slot.player, Some(receiver_slot.slot_index))
            .into_iter()
            .find(|s| {
                get_pokemon_at_slot(state, *s).is_some_and(|m| {
                    !m.fainted
                        && m.ability == Ability::Symbiosis
                        && !pokemon_ability_is_suppressed(state, m)
                        && m.item != Item::None
                        && !item_cannot_be_transferred(&m.item, m)
                })
            });
    let Some(donor_slot) = donor_slot else { return };

    let item = match get_pokemon_at_slot_mut(state, donor_slot) {
        Some(d) => {
            let item = d.item.clone();
            d.item = Item::None;
            // Giving the item away counts as losing it (triggers the donor's Unburden).
            d.item_lost = true;
            item
        }
        None => return,
    };
    // Donor lost their item; emit before giving it to the receiver.
    emit(state, EventKind::ItemLost { slot: donor_slot, item: item.clone(), consumed: false });
    let env = berry_env(state, receiver_slot);
    let item_copy = item.clone();
    let cure = if let Some(r) = get_pokemon_at_slot_mut(state, receiver_slot) {
        r.item = item;
        r.item_lost = false;
        // A freshly received berry may activate immediately (e.g. status-cure berries).
        on_item_obtained_or_enabled(r, &env)
    } else {
        BerryCure::none()
    };
    // Receiver gained the item.
    emit(state, EventKind::ItemGained { slot: receiver_slot, item: item_copy });
    emit_berry_cure(state, receiver_slot, &cure);
}

/// Items that can never change holder or be removed mid-battle (Knock Off, Trick,
/// Thief, Covet, Corrosive Gas, Magician, Pickpocket). Covers the holder's own Mega
/// Stone, Booster Energy, Z-Crystals (always), and the species-locked type items /
/// signature items when held by the species they belong to.
pub(crate) fn item_cannot_be_transferred(item: &Item, holder: &PokemonState) -> bool {
    // The holder's own Mega Stone, and Paradox Booster Energy.
    if holder.has_mega_form || holder.is_mega || matches!(item, Item::BoosterEnergy) {
        return true;
    }
    // Z-Crystals can never be transferred or removed, regardless of holder.
    if item.is_z_crystal() {
        return true;
    }
    // Species-locked type items / signature items: only locked on their own species.
    let species = holder.species.to_string();
    if item.is_plate() && species.starts_with("Arceus") {
        return true;
    }
    if item.is_drive() && species.starts_with("Genesect") {
        return true;
    }
    if item.is_memory() && species.starts_with("Silvally") {
        return true;
    }
    match item {
        Item::GriseousOrb | Item::GriseousCore => species.starts_with("Giratina"),
        Item::RedOrb => species.starts_with("Groudon"),
        Item::BlueOrb => species.starts_with("Kyogre"),
        Item::RustedSword => species.starts_with("Zacian"),
        Item::RustedShield => species.starts_with("Zamazenta"),
        Item::WellspringMask | Item::HearthflameMask | Item::CornerstoneMask => {
            species.starts_with("Ogerpon")
        }
        _ => false,
    }
}

/// Move the victim's held item to the (empty-handed, alive) thief, respecting Sticky Hold
/// and untransferable items. Shared by Magician / Pickpocket and, in future, Knock Off /
/// Thief / Covet. Returns `true` if an item changed hands.
pub(crate) fn try_steal_item(
    state: &mut BattleState,
    thief_slot: FieldSlot,
    victim_slot: FieldSlot,
) -> bool {
    let thief_ok = get_pokemon_at_slot(state, thief_slot)
        .map(|m| !m.fainted && m.item == Item::None)
        .unwrap_or(false);
    if !thief_ok {
        return false;
    }

    let blocked = match get_pokemon_at_slot(state, victim_slot) {
        Some(v) => {
            v.item == Item::None
                || item_cannot_be_transferred(&v.item, v)
                // Sticky Hold protects the item — but not once the holder has fainted.
                || (!v.fainted
                    && v.ability == Ability::StickyHold
                    && !pokemon_ability_is_suppressed(state, v))
        }
        None => true,
    };
    if blocked {
        return false;
    }

    let item = match get_pokemon_at_slot_mut(state, victim_slot) {
        Some(v) => {
            let item = v.item.clone();
            v.item = Item::None;
            v.item_lost = true; // theft triggers the victim's Unburden
            item
        }
        None => return false,
    };
    let env = berry_env(state, thief_slot);
    let item_copy = item.clone();
    let cure = if let Some(t) = get_pokemon_at_slot_mut(state, thief_slot) {
        t.item = item_copy.clone();
        t.item_lost = false; // gaining an item ends a previous Unburden boost
        on_item_obtained_or_enabled(t, &env)
    } else {
        BerryCure::none()
    };
    // Loss and gain are both direct results of the move — emit as siblings.
    emit(state, EventKind::ItemLost { slot: victim_slot, item, consumed: false });
    emit(state, EventKind::ItemGained { slot: thief_slot, item: item_copy });
    emit_berry_cure(state, thief_slot, &cure);
    true
}

/// Destroy/remove the held item of the Pokémon at `slot` (Knock Off, Corrosive Gas).
/// Unlike consumption, the item is not recorded in `consumed_item` (it cannot be
/// recovered by Recycle). Respects Sticky Hold and untransferable/locked items.
/// Returns `true` if an item was removed.
pub(crate) fn try_remove_item(state: &mut BattleState, slot: FieldSlot) -> bool {
    // Capture the item while checking whether removal is legal.
    let item_to_remove = match get_pokemon_at_slot(state, slot) {
        Some(v) => {
            let blocked = v.item == Item::None
                || item_cannot_be_transferred(&v.item, v)
                // Sticky Hold keeps the item (the holder must still be on the field).
                || (!v.fainted
                    && v.ability == Ability::StickyHold
                    && !pokemon_ability_is_suppressed(state, v));
            if blocked { None } else { Some(v.item.clone()) }
        }
        None => None,
    };
    let Some(item) = item_to_remove else { return false; };
    if let Some(v) = get_pokemon_at_slot_mut(state, slot) {
        v.item = Item::None;
        v.item_lost = true; // item removal triggers Unburden
    }
    emit(state, EventKind::ItemLost { slot, item, consumed: false });
    true
}

/// Swap the held items of two Pokémon (Trick / Switcheroo). Fails if neither holds an
/// item, if either item is untransferable/locked, or if the target has Sticky Hold.
/// (Substitute is checked by the caller.) Returns `true` on a successful swap.
pub(crate) fn try_swap_items(
    state: &mut BattleState,
    user_slot: FieldSlot,
    target_slot: FieldSlot,
) -> bool {
    let (user_item, target_item) = match (
        get_pokemon_at_slot(state, user_slot),
        get_pokemon_at_slot(state, target_slot),
    ) {
        (Some(u), Some(t)) => {
            // Sticky Hold on the target blocks the swap entirely.
            if !t.fainted
                && t.ability == Ability::StickyHold
                && !pokemon_ability_is_suppressed(state, t)
            {
                return false;
            }
            // Neither side may hold a locked/untransferable item.
            if (u.item != Item::None && item_cannot_be_transferred(&u.item, u))
                || (t.item != Item::None && item_cannot_be_transferred(&t.item, t))
            {
                return false;
            }
            (u.item.clone(), t.item.clone())
        }
        _ => return false,
    };
    // Both empty-handed: nothing to swap.
    if user_item == Item::None && target_item == Item::None {
        return false;
    }
    let user_env = berry_env(state, user_slot);
    let target_env = berry_env(state, target_slot);
    let user_cure = if let Some(u) = get_pokemon_at_slot_mut(state, user_slot) {
        u.item = target_item.clone();
        u.item_lost = target_item == Item::None; // gaining an item clears Unburden; losing one sets it
        if target_item != Item::None {
            on_item_obtained_or_enabled(u, &user_env)
        } else {
            BerryCure::none()
        }
    } else {
        BerryCure::none()
    };
    let target_cure = if let Some(t) = get_pokemon_at_slot_mut(state, target_slot) {
        t.item = user_item.clone();
        t.item_lost = user_item == Item::None;
        if user_item != Item::None {
            on_item_obtained_or_enabled(t, &target_env)
        } else {
            BerryCure::none()
        }
    } else {
        BerryCure::none()
    };
    // Emit the swap as four sibling events: each side loses then gains.
    if user_item != Item::None {
        emit(state, EventKind::ItemLost { slot: user_slot, item: user_item.clone(), consumed: false });
        emit(state, EventKind::ItemGained { slot: target_slot, item: user_item });
    }
    if target_item != Item::None {
        emit(state, EventKind::ItemLost { slot: target_slot, item: target_item.clone(), consumed: false });
        emit(state, EventKind::ItemGained { slot: user_slot, item: target_item });
    }
    emit_berry_cure(state, user_slot, &user_cure);
    emit_berry_cure(state, target_slot, &target_cure);
    true
}

/// Recover the user's most recently consumed item (Recycle). Fails if the user already
/// holds an item or has no consumed item on record. Returns `true` on success.
pub(crate) fn recover_consumed_item(state: &mut BattleState, slot: FieldSlot) -> bool {
    let item = match get_pokemon_at_slot(state, slot) {
        Some(m) if m.item == Item::None => match &m.consumed_item {
            Some(i) => i.clone(),
            None => return false,
        },
        _ => return false,
    };
    let env = berry_env(state, slot);
    let cure = if let Some(m) = get_pokemon_at_slot_mut(state, slot) {
        m.item = item.clone();
        m.consumed_item = None;
        m.item_lost = false;
        // A restored item may activate immediately (e.g. a status-cure berry).
        on_item_obtained_or_enabled(m, &env)
    } else {
        BerryCure::none()
    };
    emit(state, EventKind::ItemGained { slot, item });
    emit_berry_cure(state, slot, &cure);
    true
}

/// Apply the full effects of eating `berry` to `mon` (HP/stat berries, status-cure
/// berries, Cheek Pouch / Cud Chew) and record it as the consumed item. Does NOT touch
/// `mon.item` — callers manage the held-item slot. Used by Teatime, Bug Bite / Pluck and
/// Fling (berry thrown at the target).
pub(crate) fn apply_eaten_berry_effects(mon: &mut PokemonState, berry: &Item, env: &BerryEnv) -> BerryCure {
    let mut cure = BerryCure::none();
    if env.suppressed {
        return cure;
    }
    // Status-cure / confusion-cure berries (Lum, Cheri, Persim, …) act only if the
    // matching condition is present; mirrors try_consume_status_cure_berry minus the
    // item-slot clearing handled by the caller.
    let cures_status = matches!(
        (berry, &mon.status),
        (Item::AspearBerry, Some(Status::Frozen(_)))
            | (Item::CheriBerry, Some(Status::Paralysis))
            | (Item::ChestoBerry, Some(Status::Sleep(_)))
            | (
                Item::PechaBerry,
                Some(Status::Poison | Status::ToxicPoison(_))
            )
            | (Item::RawstBerry, Some(Status::Burn))
            | (Item::LumBerry, Some(_))
    );
    let cures_confusion = is_confused(mon) && matches!(berry, Item::PersimBerry | Item::LumBerry);
    if cures_status {
        cure.status_cured = mon.status.clone();
        mon.status = None;
    }
    if cures_confusion {
        cure.confusion_cured = true;
        remove_status_volatile(mon, &VolatileStatus::Confusion);
    }
    // HP / stat / Focus Energy berries.
    apply_berry_effect(mon, berry, env);
    // Cheek Pouch healing and Cud Chew arming.
    on_berry_eaten(mon, berry, env);
    mon.consumed_item = Some(berry.clone());
    cure
}

/// Force the Pokémon at `slot` to eat its own held Berry (Teatime). `ignore_unnerve`
/// bypasses opposing Unnerve (Teatime forces consumption). Returns `true` if a berry
/// was eaten.
pub(crate) fn force_eat_held_berry(
    state: &mut BattleState,
    slot: FieldSlot,
    ignore_unnerve: bool,
) -> bool {
    let berry = match get_pokemon_at_slot(state, slot) {
        Some(m) if !m.fainted && m.item.is_berry() => m.item.clone(),
        _ => return false,
    };
    let mut env = berry_env(state, slot);
    if ignore_unnerve {
        // Teatime ignores Unnerve, but Magic Room / Klutz still suppress the item.
        let item_inactive = get_pokemon_at_slot(state, slot)
            .map(|m| !item_is_active(state, m))
            .unwrap_or(true);
        env.suppressed = item_inactive;
    }
    if env.suppressed {
        return false;
    }
    if let Some(m) = get_pokemon_at_slot_mut(state, slot) {
        m.item = Item::None;
        m.item_lost = true;
    }
    // item_lost = true bypasses the snapshot mechanism; emit directly.
    emit(state, EventKind::ItemLost { slot, item: berry.clone(), consumed: true });
    let cure = if let Some(m) = get_pokemon_at_slot_mut(state, slot) {
        apply_eaten_berry_effects(m, &berry, &env)
    } else {
        BerryCure::none()
    };
    // Emit status/confusion cure events now that the mutable borrow has ended.
    emit_berry_cure(state, slot, &cure);
    true
}

/// Bug Bite / Pluck: the attacker eats the target's held Berry and gains its effect
/// (regardless of the berry's normal trigger conditions). The target loses the berry
/// (not recorded as its consumed item). Sticky Hold does NOT prevent this. Returns
/// `true` if a berry was eaten.
pub(crate) fn try_eat_targets_berry(
    state: &mut BattleState,
    eater_slot: FieldSlot,
    holder_slot: FieldSlot,
) -> bool {
    let berry = match get_pokemon_at_slot(state, holder_slot) {
        Some(h) if h.item.is_berry() => h.item.clone(),
        _ => return false,
    };
    // The eater must still be on the field (it eats the berry itself).
    let eater_ok = get_pokemon_at_slot(state, eater_slot)
        .map(|m| !m.fainted)
        .unwrap_or(false);
    if !eater_ok {
        return false;
    }
    let env = berry_env(state, eater_slot);
    if env.suppressed {
        return false;
    }
    // Remove the berry from the target (Unburden triggers; not recoverable via Recycle).
    if let Some(h) = get_pokemon_at_slot_mut(state, holder_slot) {
        h.item = Item::None;
        h.item_lost = true;
    }
    // item_lost = true bypasses the snapshot; emit directly. consumed=false because
    // the holder did not consume it — it was taken by the attacker.
    emit(state, EventKind::ItemLost { slot: holder_slot, item: berry.clone(), consumed: false });
    let cure = if let Some(eater) = get_pokemon_at_slot_mut(state, eater_slot) {
        apply_eaten_berry_effects(eater, &berry, &env)
    } else {
        BerryCure::none()
    };
    // Emit status/confusion cure events for the eater now that the mutable borrow has ended.
    emit_berry_cure(state, eater_slot, &cure);
    true
}

/// Apply Fling's item-dependent added effect to the target it hit. Berries are eaten by
/// the target; status/flinch riders (from the item's Fling data) go through the normal
/// secondary-effect path (respecting immunities and Shield Dust); Mental Herb / White
/// Herb cure / restore the target. Called after the Fling damage connects.
pub(crate) fn apply_fling_effect(
    state: &mut BattleState,
    attacker_slot: FieldSlot,
    target_slot: FieldSlot,
    item: &Item,
) {
    // A flung Berry is eaten by the target — applied via `onHit`, so Shield Dust does
    // not block it.
    if item.is_berry() {
        let env = berry_env(state, target_slot);
        if env.suppressed {
            return;
        }
        let cure = if let Some(t) = get_pokemon_at_slot_mut(state, target_slot) {
            apply_eaten_berry_effects(t, item, &env)
        } else {
            BerryCure::none()
        };
        // Emit status/confusion cure events for the target now that the borrow has ended.
        emit_berry_cure(state, target_slot, &cure);
        return; // a flung berry produces no other rider
    }

    // Shield Dust blocks the remaining (secondary-style) riders.
    let shield_dust = get_pokemon_at_slot(state, target_slot).map_or(false, |m| {
        !pokemon_ability_is_suppressed(state, m) && m.ability == Ability::ShieldDust
    });
    if shield_dust {
        return;
    }

    // Mental Herb / White Herb fire their item callback on the target.
    match item {
        Item::WhiteHerb => {
            if let Some(t) = get_pokemon_at_slot_mut(state, target_slot) {
                for b in t.boosts.iter_mut() {
                    if *b < 0 {
                        *b = 0;
                    }
                }
            }
            return;
        }
        Item::MentalHerb => {
            let mental = [
                VolatileStatus::Attract,
                VolatileStatus::Taunt,
                VolatileStatus::Encore(PokemonMove::Struggle),
                VolatileStatus::Torment,
                VolatileStatus::HealBlock,
                VolatileStatus::Disable(PokemonMove::Struggle),
            ];
            // Capture which volatiles were actually present before removing them so we
            // can emit VolatileEnd events after the mutable borrow ends.
            let mut removed: Vec<VolatileStatus> = Vec::new();
            if let Some(t) = get_pokemon_at_slot_mut(state, target_slot) {
                for v in &mental {
                    if has_status_volatile(t, v) {
                        removed.push(v.clone());
                        remove_status_volatile(t, v);
                    }
                }
            }
            // Borrow ended — emit VolatileEnd for each removed volatile.
            // (No ItemLost here: the flung Herb was already removed from the attacker
            // earlier in Fling move processing; the target does not consume an item.)
            for v in removed {
                emit(state, EventKind::VolatileEnd { target: target_slot, volatile: v });
            }
            return;
        }
        _ => {}
    }

    // Status / flinch riders declared in the item's Fling data.
    let effect = match item.fling_effect_id() {
        Some("brn") => HitEffect {
            status: Some(Status::Burn),
            ..Default::default()
        },
        Some("par") => HitEffect {
            status: Some(Status::Paralysis),
            ..Default::default()
        },
        Some("psn") => HitEffect {
            status: Some(Status::Poison),
            ..Default::default()
        },
        Some("tox") => HitEffect {
            status: Some(Status::ToxicPoison(0)),
            ..Default::default()
        },
        Some("flinch") => HitEffect {
            volatile_status: Some(VolatileStatus::Flinch),
            ..Default::default()
        },
        _ => return,
    };
    apply_effect_to_target(
        state,
        attacker_slot,
        target_slot,
        &effect,
        target_slot.player,
    );
}

pub fn any_pokemon_has_neutralizing_gas(state: &BattleState) -> bool {
    state
        .p1_active_mons
        .iter()
        .chain(state.p2_active_mons.iter())
        .any(|mon| !mon.fainted && mon.ability == Ability::NeutralizingGas)
}

pub fn abilities_are_suppressed(state: &BattleState) -> bool {
    any_pokemon_has_neutralizing_gas(state)
}

pub(crate) fn pokemon_ability_is_suppressed(state: &BattleState, mon: &PokemonState) -> bool {
    // Field-wide suppression via Neutralizing Gas (does not suppress NeutralizingGas itself).
    if abilities_are_suppressed(state) && mon.ability != Ability::NeutralizingGas {
        return true;
    }
    // Per-Pokémon suppression via the Gastro Acid volatile.
    has_status_volatile(mon, &VolatileStatus::GastroAcid)
}

/// Returns true if the attacker's active ability is one of the Mold Breaker family,
/// meaning it suppresses the *target's* ignorable abilities during its move.
pub(crate) fn attacker_breaks_mold(state: &BattleState, attacker: &PokemonState) -> bool {
    if pokemon_ability_is_suppressed(state, attacker) {
        return false;
    }
    matches!(
        attacker.ability,
        Ability::MoldBreaker | Ability::Turboblaze | Ability::Teravolt
    )
}

/// True when the ability is on the canonical Bulbapedia "ignorable" list — i.e. an ability
/// that Mold Breaker / Turboblaze / Teravolt can suppress on the target. Dark Aura /
/// Fairy Aura and partner abilities are intentionally excluded (not ignored since Gen 8).
pub(crate) fn ability_is_ignorable(ability: &Ability) -> bool {
    matches!(
        ability,
        // Type immunities / absorbtion
        Ability::Levitate | Ability::Eelevate | Ability::WaterAbsorb | Ability::VoltAbsorb | Ability::FlashFire
        | Ability::EarthEater | Ability::SapSipper | Ability::MotorDrive | Ability::LightningRod
        | Ability::StormDrain | Ability::DrySkin | Ability::WindRider
        // Hazard landing
        | Ability::MagicGuard
        // Endure / full-HP survival
        | Ability::Sturdy
        // Defensive damage reductions
        | Ability::ThickFat | Ability::Heatproof | Ability::WaterBubble | Ability::Fluffy
        | Ability::PunkRock | Ability::IceScales | Ability::Filter | Ability::SolidRock
        | Ability::PrismArmor | Ability::Multiscale | Ability::ShadowShield
        // Wonder Guard / immunities
        | Ability::WonderGuard | Ability::Bulletproof | Ability::Soundproof
        | Ability::Damp | Ability::Overcoat
        // Status / volatile immunity
        | Ability::Limber | Ability::Insomnia | Ability::VitalSpirit | Ability::SweetVeil
        | Ability::Immunity | Ability::PastelVeil | Ability::WaterVeil | Ability::MagmaArmor
        | Ability::Oblivious | Ability::OwnTempo | Ability::InnerFocus | Ability::AromaVeil
        | Ability::Comatose | Ability::FlowerVeil
        // Stat-change inversion / doubling
        | Ability::Contrary | Ability::Simple
        // Crit prevention
        | Ability::BattleArmor | Ability::ShellArmor
        // Move-use prevention
        | Ability::ShadowTag | Ability::MagicBounce
        // Evasion / accuracy
        | Ability::SandVeil | Ability::SnowCloak | Ability::TangledFeet
        // Contact punish (target side) — Mold Breaker suppresses these on the target
        | Ability::RoughSkin | Ability::IronBarbs | Ability::FlameBody | Ability::Static
        | Ability::PoisonPoint | Ability::CuteCharm | Ability::Gooey
        | Ability::CursedBody | Ability::Mummy | Ability::WanderingSpirit | Ability::Pickpocket
        | Ability::Aftermath
    )
}

/// True when a move can be reflected by Magic Bounce / Magic Coat.
/// Reads the `Reflectable` flag from move data — the authoritative source from Showdown.
/// This correctly bounces trapping moves (Block, Mean Look, Spider Web) and heal-redirect
/// moves (Heal Pulse, Floral Healing) that the old hardcoded list incorrectly exempted.
pub fn move_is_reflectable(move_data: &MoveData) -> bool {
    move_has_flag(move_data, &MoveFlag::Reflectable)
}

/// True when an ally of `user_slot` on the same side has Plus or Minus (and is alive, unsuppressed).
/// Gen 9+: same-ability allies count (Plus+Plus triggers, Minus+Minus triggers).
pub(crate) fn has_plus_minus_partner(state: &BattleState, user_slot: FieldSlot) -> bool {
    let allies = collect_active_slots(state, user_slot.player, Some(user_slot.slot_index));
    allies.iter().any(|ally_slot| {
        get_pokemon_at_slot(state, *ally_slot).is_some_and(|ally| {
            !pokemon_ability_is_suppressed(state, ally)
                && matches!(ally.ability, Ability::Plus | Ability::Minus)
        })
    })
}

/// Return the effective ability of `target` as seen by `attacker`. If the attacker
/// has a Mold-Breaker ability and the target's ability is ignorable, returns
/// `Ability::None` (as if the target had no ability).
pub(crate) fn target_ability_as_seen_by(
    state: &BattleState,
    attacker: &PokemonState,
    target: &PokemonState,
) -> Ability {
    if pokemon_ability_is_suppressed(state, target) {
        return Ability::None;
    }
    if attacker_breaks_mold(state, attacker) && ability_is_ignorable(&target.ability) {
        return Ability::None;
    }
    target.ability.clone()
}

fn terrain_matches(state: &BattleState, terrain: &Terrain) -> bool {
    matches!(current_terrain(state), Some(current) if std::mem::discriminant(&current) == std::mem::discriminant(terrain))
}

pub fn pokemon_is_grounded(state: &BattleState, mon: &PokemonState) -> bool {
    if mon.fainted {
        return false;
    }

    if is_gravity_active(state) {
        return true;
    }

    // Ingrain and Smack Down / Thousand Arrows (SmackDown volatile) force the user to be
    // grounded regardless of Flying type, Levitate, Air Balloon, Magnet Rise or Telekinesis.
    if has_status_volatile(mon, &VolatileStatus::Ingrain)
        || has_status_volatile(mon, &VolatileStatus::SmackDown)
    {
        return true;
    }

    // A Flying-type that has just used Roost loses its Flying type for the turn, so it is
    // grounded (susceptible to Ground moves, Spikes and terrain) like any other type.
    let counts_as_flying = pokemon_has_type(mon, &PokemonType::Flying)
        && !has_status_volatile(mon, &VolatileStatus::Roost);

    // Iron Ball overrides all other ungrounding effects (Flying type, Levitate, Eelevate,
    // Air Balloon, Magnet Rise, Telekinesis) and forces the holder to be grounded.
    // Klutz / Magic Room / Embargo already suppress item_is_active, so those negate Iron Ball.
    if item_is_active(state, mon) && matches!(mon.item, Item::IronBall) {
        return true;
    }

    !counts_as_flying
        && mon.ability != Ability::Levitate
        && mon.ability != Ability::Eelevate
        && (!matches!(mon.item, Item::AirBalloon) || !item_is_active(state, mon))
        && !has_status_volatile(mon, &VolatileStatus::MagnetRise)
        && !has_status_volatile(mon, &VolatileStatus::Telekinesis)
        && !mon.volatiles.iter().any(|volatile| {
            matches!(
                volatile,
                VolatileStatusState::MoveStatus(VolatileStatus::SemiInvulnerable(_), _)
                    | VolatileStatusState::TurnStatus(VolatileStatus::SkyDrop, _)
            )
        })
}

pub fn pokemon_is_on_terrain(state: &BattleState, mon: &PokemonState, terrain: &Terrain) -> bool {
    terrain_matches(state, terrain) && pokemon_is_grounded(state, mon)
}

pub fn clear_terrain(state: &mut BattleState) {
    state.terrain = None;
    state.terrain_turns = None;
    update_mimicry_forms(state);
}

pub fn terrain_replacement_move(state: &BattleState) -> Option<PokemonMove> {
    match current_terrain(state) {
        Some(Terrain::ElectricTerrain) => Some(PokemonMove::Thunderbolt),
        Some(Terrain::GrassyTerrain) => Some(PokemonMove::EnergyBall),
        Some(Terrain::MistyTerrain) => Some(PokemonMove::Moonblast),
        Some(Terrain::PsychicTerrain) => Some(PokemonMove::Psychic),
        None => None,
    }
}

fn terrain_seed_for_current_terrain(state: &BattleState) -> Option<(Item, PokemonStat)> {
    match current_terrain(state) {
        Some(Terrain::ElectricTerrain) => Some((Item::ElectricSeed, PokemonStat::Def)),
        Some(Terrain::GrassyTerrain) => Some((Item::GrassySeed, PokemonStat::Def)),
        Some(Terrain::MistyTerrain) => Some((Item::MistySeed, PokemonStat::SpD)),
        Some(Terrain::PsychicTerrain) => Some((Item::PsychicSeed, PokemonStat::SpD)),
        None => None,
    }
}

fn trigger_terrain_seed_items(state: &mut BattleState) {
    let Some((seed_item, boost_stat)) = terrain_seed_for_current_terrain(state) else {
        return;
    };
    if items_are_suppressed(state) {
        return;
    }

    let mut slots = collect_active_slots(state, Player::P1, None);
    slots.extend(collect_active_slots(state, Player::P2, None));
    for slot in slots {
        let eligible = get_pokemon_at_slot(state, slot)
            .map(|m| m.item == seed_item && !klutz_disables_item(m))
            .unwrap_or(false);
        if !eligible {
            continue;
        }

        if let Some(mon) = get_pokemon_at_slot_mut(state, slot) {
            mon.item = Item::None;
            mon.item_lost = true;
            match boost_stat {
                PokemonStat::Def => {
                    if mon.boosts[1] < 6 {
                        mon.stats_raised_this_turn = true;
                    }
                    mon.boosts[1] = (mon.boosts[1] + 1).clamp(-6, 6);
                }
                PokemonStat::SpD => {
                    if mon.boosts[3] < 6 {
                        mon.stats_raised_this_turn = true;
                    }
                    mon.boosts[3] = (mon.boosts[3] + 1).clamp(-6, 6);
                }
                _ => {}
            }
        }
        // Seeds are used up — eligible for Pickup and trigger Symbiosis.
        state
            .items_consumed_this_turn
            .push((slot, seed_item.clone()));
        // Emit ItemLost; Symbiosis pass (if any) emits nested reactions inside.
        with_reactions(
            state,
            EventKind::ItemLost { slot, item: seed_item.clone(), consumed: true },
            |bs| try_symbiosis_pass(bs, slot),
        );
    }
}

pub fn weather_is_sunlight(state: &BattleState) -> bool {
    matches!(
        current_weather(state),
        Some(Weather::Sun) | Some(Weather::ExtremeSunlight)
    )
}

pub fn weather_is_rain(state: &BattleState) -> bool {
    matches!(
        current_weather(state),
        Some(Weather::Rain) | Some(Weather::HeavyRain)
    )
}

pub fn weather_is_harsh_sunlight(state: &BattleState) -> bool {
    matches!(current_weather(state), Some(Weather::ExtremeSunlight))
}

pub fn weather_is_heavy_rain(state: &BattleState) -> bool {
    matches!(current_weather(state), Some(Weather::HeavyRain))
}

fn weather_is_sandstorm(state: &BattleState) -> bool {
    matches!(current_weather(state), Some(Weather::Sandstorm))
}

fn weather_is_snow(state: &BattleState) -> bool {
    matches!(current_weather(state), Some(Weather::Snow))
}

fn weather_is_strong_winds(state: &BattleState) -> bool {
    matches!(current_weather(state), Some(Weather::StrongWinds))
}

// ── Per-mon weather perception (Mega Sol) ────────────────────────────────────
//
// Mega Sol makes the holder perceive harsh sunlight regardless of the actual field
// weather. Cloud Nine / Air Lock do NOT suppress it (it's not actual weather).
// For all other mons, these delegates to the global helpers above.

/// The weather as perceived by `mon` for the purpose of damage/accuracy/speed/status checks.
fn weather_for(state: &BattleState, mon: &PokemonState) -> Option<Weather> {
    if !pokemon_ability_is_suppressed(state, mon) && mon.ability == Ability::MegaSol {
        return Some(Weather::Sun);
    }
    current_weather(state)
}

fn weather_is_sunlight_for(state: &BattleState, mon: &PokemonState) -> bool {
    matches!(
        weather_for(state, mon),
        Some(Weather::Sun) | Some(Weather::ExtremeSunlight)
    )
}

/// Public variant for call sites that have a `FieldSlot` but no direct `&PokemonState`.
pub fn weather_is_sunlight_for_slot(state: &BattleState, slot: FieldSlot) -> bool {
    if let Some(mon) = get_pokemon_at_slot(state, slot) {
        weather_is_sunlight_for(state, mon)
    } else {
        weather_is_sunlight(state)
    }
}

fn weather_is_rain_for(state: &BattleState, mon: &PokemonState) -> bool {
    matches!(
        weather_for(state, mon),
        Some(Weather::Rain) | Some(Weather::HeavyRain)
    )
}

fn weather_is_sandstorm_for(state: &BattleState, mon: &PokemonState) -> bool {
    matches!(weather_for(state, mon), Some(Weather::Sandstorm))
}

fn weather_is_snow_for(state: &BattleState, mon: &PokemonState) -> bool {
    matches!(weather_for(state, mon), Some(Weather::Snow))
}

pub(crate) fn is_confused(mon: &PokemonState) -> bool {
    mon.volatiles
        .iter()
        .any(|volatile_status| match volatile_status {
            VolatileStatusState::TurnStatus(VolatileStatus::Confusion, _) => true,
            VolatileStatusState::MoveStatus(VolatileStatus::Confusion, _) => true,
            _ => false,
        })
}

pub fn confusion_turns_remaining(mon: &PokemonState) -> Option<u16> {
    mon.volatiles
        .iter()
        .find_map(|volatile_status| match volatile_status {
            VolatileStatusState::MoveStatus(VolatileStatus::Confusion, turns) => Some(*turns),
            VolatileStatusState::TurnStatus(VolatileStatus::Confusion, turns) => Some(*turns),
            _ => None,
        })
}

pub fn confusion_self_hit_damage_outcomes(
    state: &BattleState,
    attacker: &PokemonState,
    damage_rolls: u8,
) -> Vec<(u16, f64)> {
    let attacking_stat = PokemonStat::Atk;
    let defending_stat = PokemonStat::Def;

    let attacker_stat = effective_stat(state, attacker, attacking_stat, false, false);
    let target_defense = effective_stat(state, attacker, defending_stat, false, false);

    let mut base_damage = (2.0 * attacker.level as f64 / 5.0).floor();
    base_damage = (base_damage + 2.0).floor();
    base_damage = (base_damage * 40.0).floor();
    base_damage = (base_damage * attacker_stat).floor();
    base_damage = (base_damage / target_defense).floor();
    base_damage = (base_damage / 50.0).floor();
    base_damage = (base_damage + 2.0).floor();

    let burn_multiplier =
        if matches!(attacker.status, Some(Status::Burn)) && attacker.ability != Ability::Guts {
            0.5
        } else {
            1.0
        };

    let damage_roll_values = selected_damage_rolls(damage_rolls);
    let roll_probability = 1.0 / damage_roll_values.len() as f64;
    let mut outcomes = Vec::new();

    for roll in damage_roll_values {
        let random_multiplier = roll as f64 / 100.0;
        let mut damage = base_damage;
        damage = (damage * random_multiplier).floor();
        damage = (damage * burn_multiplier).floor();

        outcomes.push((damage.max(0.0) as u16, roll_probability));
    }

    outcomes
}

fn round_div_half_up(numerator: i32, denominator: i32) -> i32 {
    if denominator <= 0 {
        return 0;
    }
    if numerator <= 0 {
        return 0;
    }
    let q = numerator / denominator;
    let r = numerator % denominator;
    if r * 2 >= denominator { q + 1 } else { q }
}

fn round_div_half_down(numerator: i32, denominator: i32) -> i32 {
    if denominator <= 0 {
        return 0;
    }
    if numerator <= 0 {
        return 0;
    }
    let q = numerator / denominator;
    let r = numerator % denominator;
    if r * 2 > denominator { q + 1 } else { q }
}

fn user_active_mons<'a>(state: &'a BattleState, player: Player) -> &'a Vec<PokemonState> {
    match player {
        Player::P1 => &state.p1_active_mons,
        Player::P2 => &state.p2_active_mons,
    }
}

fn target_has_acted_this_turn(state: &BattleState, target_slot: FieldSlot) -> bool {
    !state.action_queue.iter().any(|action| {
        let slot = match action {
            Action::MoveAction(m) => Some(m.user_slot),
            Action::SwitchAction(s) => Some(s.user_slot),
            Action::MegaAction(m) => Some(m.user_slot),
            Action::TeraAction(t) => Some(t.user_slot),
        };

        slot.map(|s| s.player == target_slot.player && s.slot_index == target_slot.slot_index)
            .unwrap_or(false)
    })
}

/// Returns true when the attacker's slot is the last (or only) remaining mover this turn.
/// The attacker's own action has already been removed from the queue before execution,
/// so Analytic fires when no MoveAction from any OTHER slot is still pending.
fn attacker_is_last_mover(state: &BattleState, user_slot: FieldSlot) -> bool {
    !state.action_queue.iter().any(|action| {
        if let Action::MoveAction(m) = action {
            !(m.user_slot.player == user_slot.player
                && m.user_slot.slot_index == user_slot.slot_index)
        } else {
            false
        }
    })
}

fn apply_modifier_fp(current: i32, numerator: i32) -> i32 {
    round_div_half_up(current.saturating_mul(numerator), 4096)
}

fn compute_accuracy_modifier_fp(
    state: &BattleState,
    attacker: &PokemonState,
    target: &PokemonState,
    user_slot: FieldSlot,
    target_slot: FieldSlot,
    move_data: &MoveData,
) -> i32 {
    let mut modifier = 4096i32;

    if is_gravity_active(state) {
        modifier = apply_modifier_fp(modifier, 6840);
    }

    if !pokemon_ability_is_suppressed(state, target)
        && target.ability == Ability::TangledFeet
        && is_confused(target)
    {
        modifier = apply_modifier_fp(modifier, 2048);
    }

    if !pokemon_ability_is_suppressed(state, attacker)
        && attacker.ability == Ability::Hustle
        && matches!(move_data.category, MoveCategory::Physical)
    {
        modifier = apply_modifier_fp(modifier, 3277);
    }

    if !pokemon_ability_is_suppressed(state, target)
        && target.ability == Ability::SandVeil
        && weather_is_sandstorm(state)
    {
        modifier = apply_modifier_fp(modifier, 3277);
    }

    if !pokemon_ability_is_suppressed(state, target)
        && target.ability == Ability::SnowCloak
        && weather_is_snow(state)
    {
        modifier = apply_modifier_fp(modifier, 3277);
    }

    let allies = user_active_mons(state, user_slot.player);
    let victory_star_count = allies
        .iter()
        .enumerate()
        .filter(|(idx, mon)| {
            !mon.fainted
                && !pokemon_ability_is_suppressed(state, mon)
                && mon.ability == Ability::VictoryStar
                && (*idx as u8 != user_slot.slot_index
                    || (!pokemon_ability_is_suppressed(state, attacker)
                        && attacker.ability == Ability::VictoryStar))
        })
        .count();

    for _ in 0..victory_star_count {
        modifier = apply_modifier_fp(modifier, 4506);
    }

    if !pokemon_ability_is_suppressed(state, attacker) && attacker.ability == Ability::CompoundEyes
    {
        modifier = apply_modifier_fp(modifier, 5325);
    }

    if item_is_active(state, target) && matches!(target.item, Item::BrightPowder) {
        modifier = apply_modifier_fp(modifier, 3686);
    }

    if item_is_active(state, target) && matches!(target.item, Item::LaxIncense) {
        modifier = apply_modifier_fp(modifier, 3686);
    }

    if item_is_active(state, attacker) && matches!(attacker.item, Item::WideLens) {
        modifier = apply_modifier_fp(modifier, 4505);
    }

    if item_is_active(state, attacker)
        && matches!(attacker.item, Item::ZoomLens)
        && target_has_acted_this_turn(state, target_slot)
    {
        modifier = apply_modifier_fp(modifier, 4915);
    }

    modifier.max(0)
}

fn adjusted_accuracy_stage(
    state: &BattleState,
    attacker: &PokemonState,
    target: &PokemonState,
    ignore_evasion: bool,
) -> i8 {
    // Unaware (attacker): ignore target's evasion stage.
    // Unaware (target/defender): ignore attacker's accuracy stage.
    let attacker_unaware =
        !pokemon_ability_is_suppressed(state, attacker) && attacker.ability == Ability::Unaware;
    let defender_unaware =
        !pokemon_ability_is_suppressed(state, target) && target.ability == Ability::Unaware;

    let attacker_accuracy = if defender_unaware {
        0
    } else {
        attacker.boosts[5]
    };
    // Keen Eye / Illuminate / Unaware: when the attacker has one of these (unsuppressed), the
    // target's evasiveness stages are ignored entirely.  Non-stage accuracy modifiers
    // (Sand Veil, Wonder Skin, etc.) are NOT ignored — this only zeroes the stage term.
    // Mold Breaker does not apply here (this protects the attacker's own accuracy calc).
    // ignore_evasion flag (Sacred Sword, Darkest Lariat): also zeroes target evasion stage.
    let target_evasion = if ignore_evasion
        || attacker_unaware
        || (!pokemon_ability_is_suppressed(state, attacker)
            && matches!(attacker.ability, Ability::KeenEye | Ability::Illuminate))
    {
        0
    } else {
        target.boosts[6]
    };
    (attacker_accuracy - target_evasion).clamp(-6, 6)
}

fn accuracy_stage_multiplier(stage: i8) -> f64 {
    let stage = stage.clamp(-6, 6);
    let base = 3.0;
    if stage >= 0 {
        (base + stage as f64) / base
    } else {
        base / (base - stage as f64)
    }
}

fn micle_berry_multiplier_fp(attacker: &PokemonState) -> i32 {
    if matches!(attacker.item, Item::MicleBerry)
        && !klutz_disables_item(attacker)
        && attacker.last_move_failed
    {
        4915
    } else {
        4096
    }
}

fn affection_adjustment(_target: &PokemonState) -> i32 {
    0
}

fn weather_forced_accuracy(
    state: &BattleState,
    attacker: &PokemonState,
    move_name: &PokemonMove,
) -> Option<f64> {
    if weather_is_rain(state)
        && matches!(
            move_name,
            PokemonMove::Thunder
                | PokemonMove::Hurricane
                | PokemonMove::BleakwindStorm
                | PokemonMove::WildboltStorm
                | PokemonMove::SandsearStorm
        )
    {
        return Some(1.0);
    }

    if weather_is_snow(state) && matches!(move_name, PokemonMove::Blizzard) {
        return Some(1.0);
    }

    // Thunder / Hurricane accuracy is halved in sun; Mega Sol counts as sun for the attacker.
    if weather_is_sunlight_for(state, attacker)
        && matches!(move_name, PokemonMove::Thunder | PokemonMove::Hurricane)
    {
        return Some(0.5);
    }

    None
}

pub fn accuracy_hit_probability(
    state: &BattleState,
    attacker: &PokemonState,
    target: &PokemonState,
    user_slot: FieldSlot,
    target_slot: FieldSlot,
    move_data: &MoveData,
) -> f64 {
    if let Some(forced_accuracy) = weather_forced_accuracy(state, attacker, &move_data.name) {
        return forced_accuracy;
    }

    // Minimize: moves that hit Minimized targets harder also never miss them.
    if has_status_volatile(target, &VolatileStatus::Minimize)
        && move_hits_minimized_harder(&move_data.name)
    {
        return 1.0;
    }

    // Lock-On / Mind Reader: if the attacker is locked onto this exact target, the move
    // cannot miss (bypasses evasion and semi-invulnerability, but not Protect or immunity).
    {
        let locked_target_id = attacker.volatiles.iter().find_map(|v| match v {
            VolatileStatusState::MoveStatus(VolatileStatus::LockedOn(id), _)
            | VolatileStatusState::TurnStatus(VolatileStatus::LockedOn(id), _) => Some(*id),
            _ => None,
        });
        if let Some(locked_id) = locked_target_id {
            if locked_id == target.mon_id {
                return 1.0;
            }
        }
    }

    // One-hit KO moves use a level-based accuracy that ignores accuracy/evasion stages
    // and all accuracy modifiers. They fail outright against a higher-level target (even
    // under No Guard). Sheer Cold's base is 20 for non-Ice users (30 otherwise).
    if move_data.ohko {
        if attacker.level < target.level {
            return 0.0;
        }
        // No Guard (on either side) guarantees the hit.
        let no_guard = (!pokemon_ability_is_suppressed(state, attacker)
            && attacker.ability == Ability::NoGuard)
            || (!pokemon_ability_is_suppressed(state, target)
                && target.ability == Ability::NoGuard);
        if no_guard {
            return 1.0;
        }
        let base: i32 = if move_data.name == PokemonMove::SheerCold
            && !pokemon_has_type(attacker, &PokemonType::Ice)
        {
            20
        } else {
            30
        };
        let acc = (attacker.level as i32 - target.level as i32 + base).clamp(0, 100);
        return acc as f64 / 100.0;
    }

    match move_data.accuracy {
        AccuracyType::True => 1.0,
        AccuracyType::Percent(base_accuracy) => {
            let base = base_accuracy as i32;
            let modifier_fp = compute_accuracy_modifier_fp(
                state,
                attacker,
                target,
                user_slot,
                target_slot,
                move_data,
            );

            let accuracy_after_modifiers =
                round_div_half_down(base.saturating_mul(modifier_fp), 4096);

            let stage = adjusted_accuracy_stage(state, attacker, target, move_data.ignore_evasion);
            let stage_adjusted =
                (accuracy_after_modifiers as f64 * accuracy_stage_multiplier(stage)).floor() as i32;

            let micle_adjusted = round_div_half_down(
                stage_adjusted.saturating_mul(micle_berry_multiplier_fp(attacker)),
                4096,
            );

            let final_accuracy = (micle_adjusted - affection_adjustment(target)).clamp(0, 100);
            final_accuracy as f64 / 100.0
        }
    }
}

fn get_effective_speed(state: &BattleState, mon: &PokemonState) -> f32 {
    let base_speed = mon.stats[5] as f32;
    let speed_boost = mon.boosts[4];

    let multiplier = if speed_boost > 0 {
        1.0 + (0.5 * speed_boost as f32)
    } else if speed_boost < 0 {
        1.0 / (1.0 + (0.5 * (-speed_boost) as f32))
    } else {
        1.0
    };

    let mut speed = base_speed * multiplier;

    if !pokemon_ability_is_suppressed(state, mon)
        && mon.ability == Ability::SurgeSurfer
        && matches!(current_terrain(state), Some(Terrain::ElectricTerrain))
    {
        speed *= 2.0;
    }

    // Quick Feet: +50% speed if afflicted by any non-volatile status
    if mon.ability == Ability::QuickFeet && mon.status.is_some() {
        speed *= 1.5;
    }

    // Paralysis halves speed unless Quick Feet prevents speed loss
    if matches!(mon.status, Some(Status::Paralysis)) && mon.ability != Ability::QuickFeet {
        speed *= 0.5;
    }

    // Choice Scarf: 1.5× Speed.
    if item_is_active(state, mon) && mon.item == Item::ChoiceScarf {
        speed *= 1.5;
    }

    // Iron Ball: ×0.5 Speed. Klutz / Magic Room (via item_is_active) negate this.
    if item_is_active(state, mon) && matches!(mon.item, Item::IronBall) {
        speed *= 0.5;
    }

    speed
}

fn side_has_tailwind(state: &BattleState, player: Player) -> bool {
    let side_conditions = match player {
        Player::P1 => &state.p1_side_conditions,
        Player::P2 => &state.p2_side_conditions,
    };

    side_conditions
        .iter()
        .any(|condition| matches!(condition, SideCondition::TailWind))
}

pub fn effective_speed_for_slot(state: &BattleState, slot: FieldSlot, mon: &PokemonState) -> f32 {
    let mut speed = get_effective_speed(state, mon);

    if !pokemon_ability_is_suppressed(state, mon)
        && mon.ability == Ability::Chlorophyll
        && weather_is_sunlight_for(state, mon)
    {
        speed *= 2.0;
    }
    if !pokemon_ability_is_suppressed(state, mon)
        && mon.ability == Ability::SwiftSwim
        && weather_is_rain_for(state, mon)
    {
        speed *= 2.0;
    }
    if !pokemon_ability_is_suppressed(state, mon)
        && mon.ability == Ability::SandRush
        && weather_is_sandstorm_for(state, mon)
    {
        speed *= 2.0;
    }
    if !pokemon_ability_is_suppressed(state, mon)
        && mon.ability == Ability::SlushRush
        && weather_is_snow_for(state, mon)
    {
        speed *= 2.0;
    }
    // Unburden: ×2 Speed once the held item has been consumed or lost (and no new item
    // gained). The `item_lost` flag is cleared on switch-out / item gain; while the
    // ability is suppressed the boost is dormant and returns when suppression ends.
    if !pokemon_ability_is_suppressed(state, mon)
        && mon.ability == Ability::Unburden
        && mon.item == Item::None
        && mon.item_lost
    {
        speed *= 2.0;
    }

    if side_has_tailwind(state, slot.player) {
        speed *= 2.0;
    }
    speed
}

pub fn trick_room_is_active(state: &BattleState) -> bool {
    state
        .pseudo_weathers
        .iter()
        .any(|pseudo_weather| matches!(pseudo_weather, PseudoWeather::TrickRoom))
}

fn get_action_type_priority(action: &Action) -> u8 {
    match action {
        Action::SwitchAction(_) => 0,
        Action::MegaAction(_) => 1,
        Action::TeraAction(_) => 2,
        Action::MoveAction(_) => 3,
    }
}

pub fn compare_action_order(
    action1: &Action,
    action2: &Action,
    state: &BattleState,
    move_dex: &std::collections::HashMap<PokemonMove, MoveData>,
) -> std::cmp::Ordering {
    use std::cmp::Ordering;

    let type_priority1 = get_action_type_priority(action1);
    let type_priority2 = get_action_type_priority(action2);

    if type_priority1 != type_priority2 {
        return type_priority1.cmp(&type_priority2);
    }

    match (action1, action2) {
        (Action::MoveAction(m1), Action::MoveAction(m2)) => {
            // Fetch users early — reused for effective priority, Stall, and speed checks.
            let user1 = get_pokemon_at_slot(state, m1.user_slot);
            let user2 = get_pokemon_at_slot(state, m2.user_slot);

            // Effective priority: m.priority (baked from the dex at queue-build, or
            // manually set in tests) plus dynamic boosts (Grassy Glide terrain,
            // Prankster, Gale Wings) computed from live state so mid-turn HP changes
            // (e.g. Fake Out dropping a Gale Wings user below full HP) are reflected.
            // Using m.priority as the base (not move_data.priority) ensures test code
            // that manually overrides priority on a MoveAction is still respected.
            let ep1 = match (user1, move_dex.get(&m1.move_name)) {
                (Some(u), Some(md)) => m1.priority + effective_priority_boost(state, u, md),
                _ => m1.priority,
            };
            let ep2 = match (user2, move_dex.get(&m2.move_name)) {
                (Some(u), Some(md)) => m2.priority + effective_priority_boost(state, u, md),
                _ => m2.priority,
            };
            if ep1 != ep2 {
                return ep2.cmp(&ep1);
            }

            // moves_first flag: set probabilistically at turn start for Quick Claw (20%)
            // and Quick Draw (30%), combined into a single activation. An active flag
            // always wins within the same effective-priority bracket.
            match (m1.moves_first, m2.moves_first) {
                (true, false) => return Ordering::Less,
                (false, true) => return Ordering::Greater,
                _ => {}
            }

            // moves_last flag: set by Quash. Loses to moves_first (checked above).
            // When both are Quashed, falls through to speed for Gen IX ordering.
            match (m1.moves_last, m2.moves_last) {
                (true, false) => return Ordering::Greater,
                (false, true) => return Ordering::Less,
                _ => {}
            }

            // Stall: holder always moves last within its bracket, regardless of speed
            // or Trick Room. Overridden only when moves_first is active (handled above).
            if let (Some(p1), Some(p2)) = (user1, user2) {
                let m1_stall =
                    p1.ability == Ability::Stall && !pokemon_ability_is_suppressed(state, p1);
                let m2_stall =
                    p2.ability == Ability::Stall && !pokemon_ability_is_suppressed(state, p2);
                match (m1_stall, m2_stall) {
                    (true, false) => return Ordering::Greater,
                    (false, true) => return Ordering::Less,
                    _ => {} // both or neither: fall through to speed
                }
            }

            // Speed comparison and Trick Room.
            match (user1, user2) {
                (Some(p1), Some(p2)) => {
                    let speed1 = effective_speed_for_slot(state, m1.user_slot, p1);
                    let speed2 = effective_speed_for_slot(state, m2.user_slot, p2);
                    let trick_room = trick_room_is_active(state);

                    if (speed2 - speed1).abs() < 0.01 {
                        Ordering::Equal
                    } else if trick_room {
                        if speed1 < speed2 {
                            Ordering::Less
                        } else {
                            Ordering::Greater
                        }
                    } else if speed2 > speed1 {
                        Ordering::Greater
                    } else {
                        Ordering::Less
                    }
                }
                _ => Ordering::Equal,
            }
        }
        _ => Ordering::Equal,
    }
}

pub fn get_pokemon_at_slot<'a>(
    state: &'a BattleState,
    slot: FieldSlot,
) -> Option<&'a PokemonState> {
    let mons = match slot.player {
        Player::P1 => &state.p1_active_mons,
        Player::P2 => &state.p2_active_mons,
    };
    mons.get(slot.slot_index as usize)
}

/// Returns the weather duration for `mon` setting `weather`:
/// 8 turns if `mon` holds the matching weather rock (item active), 5 turns otherwise.
/// Strong/permanent weather (duration 0) is unaffected — callers pass 0 directly.
pub fn weather_rock_duration(mon: &PokemonState, weather: &Weather) -> u8 {
    // Rock items are not suppressed by Klutz / Magic Room — use raw item check here.
    // (This mirrors how Light Clay is read for screens: item is not gated by item_is_active.)
    let has_rock = match weather {
        Weather::Sun => matches!(mon.item, Item::HeatRock),
        Weather::Rain => matches!(mon.item, Item::DampRock),
        Weather::Sandstorm => matches!(mon.item, Item::SmoothRock),
        Weather::Snow => matches!(mon.item, Item::IcyRock),
        _ => false,
    };
    if has_rock { 8 } else { 5 }
}

/// Set weather, respecting strong weather precedence.
/// Strong weather can only be overridden by other strong weather.
pub fn set_weather(state: &mut BattleState, weather: Weather, duration: u8) {
    let current_is_strong = matches!(
        state.weather.as_ref(),
        Some(Weather::ExtremeSunlight) | Some(Weather::HeavyRain) | Some(Weather::StrongWinds)
    );
    let new_is_strong = matches!(
        weather,
        Weather::ExtremeSunlight | Weather::HeavyRain | Weather::StrongWinds
    );

    if current_is_strong && !new_is_strong {
        return;
    }

    state.weather = Some(weather.clone());
    state.weather_turns = Some(duration);
    emit(state, EventKind::WeatherChanged { weather: Some(weather) });
}

/// Apply entry-hazard effects to the Pokémon that just entered `slot`.
///
/// Deterministic — entry hazards never branch the outcome tree. Effects resolve in the fixed
/// order Sticky Web → Stealth Rock → Spikes → Toxic Spikes, short-circuiting as soon as the
/// entrant faints (so a mon KO'd by Stealth Rock is not also poisoned by Toxic Spikes; the
/// "skip the switch-in ability" half of that interaction lives in `process_pokemon_send_out`).
fn apply_entry_hazards(state: &mut BattleState, slot: FieldSlot) {
    let Some(mon) = get_pokemon_at_slot(state, slot) else {
        return;
    };
    if mon.fainted {
        return;
    }

    // Heavy-Duty Boots makes the holder immune to every entry hazard.
    if item_is_active(state, mon) && mon.item == Item::HeavyDutyBoots {
        return;
    }

    // Snapshot entrant properties before taking any mutable borrow of `state`.
    let grounded = pokemon_is_grounded(state, mon);
    let ability_suppressed = pokemon_ability_is_suppressed(state, mon);
    let entrant_ability = mon.ability.clone();
    // Magic Guard blocks Stealth Rock / Spikes *damage* only (not the Sticky Web Speed drop or
    // Toxic Spikes poison).
    let magic_guard = !ability_suppressed && entrant_ability == Ability::MagicGuard;
    let is_poison_type = pokemon_has_type(mon, &PokemonType::Poison);
    let max_hp = mon.stats[0].max(1);
    let rock_eff = move_type_effectiveness(state, &PokemonType::Rock, mon);

    let conditions = match slot.player {
        Player::P1 => state.p1_side_conditions.clone(),
        Player::P2 => state.p2_side_conditions.clone(),
    };

    // ── 1. Sticky Web — −1 Speed, grounded only ─────────────────────────────────────────────
    if grounded {
        if let Some(SideCondition::StickyWeb(setter_id)) = conditions
            .iter()
            .find(|sc| matches!(sc, SideCondition::StickyWeb(_)))
        {
            let items_suppressed = items_are_suppressed(state);
            let speed_drop = [0, 0, 0, 0, -1, 0, 0];
            if !ability_suppressed && entrant_ability == Ability::MirrorArmor {
                // Mirror Armor reflects the drop back to the *specific* Pokémon that set the web,
                // matched by its `mon_id` among the opposing side's current actives. If that setter
                // is no longer on the field, nobody's Speed is lowered.
                let opp = match slot.player {
                    Player::P1 => Player::P2,
                    Player::P2 => Player::P1,
                };
                let opp_actives = match opp {
                    Player::P1 => &state.p1_active_mons,
                    Player::P2 => &state.p2_active_mons,
                };
                let setter_slot = setter_id.and_then(|id| {
                    opp_actives
                        .iter()
                        .position(|m| !m.fainted && m.mon_id == id)
                        .map(|i| FieldSlot {
                            player: opp,
                            slot_index: i as u8,
                        })
                });
                if let Some(src) = setter_slot {
                    apply_opponent_stat_drop(state, slot, src, speed_drop, items_suppressed, false);
                }
            } else {
                // No Mirror Armor: lower the entrant's Speed directly. `source_slot` is unused on the
                // already-reflected path, so the entrant slot is a harmless placeholder.
                apply_opponent_stat_drop(state, slot, slot, speed_drop, items_suppressed, true);
            }
        }
    }
    if get_pokemon_at_slot(state, slot).map_or(true, |m| m.fainted) {
        return;
    }

    // ── 2. Stealth Rock — Rock-typed damage, affects every entrant (airborne included) ──────
    if !magic_guard
        && conditions
            .iter()
            .any(|sc| matches!(sc, SideCondition::StealthRock))
    {
        let dmg = (((max_hp as f64) * rock_eff / 8.0).floor() as u16).max(1);
        let env = berry_env(state, slot);
        let as_ = abilities_are_suppressed(state);
        if let Some(m) = get_pokemon_at_slot_mut(state, slot) {
            take_damage(m, dmg, env, as_);
        }
    }
    if get_pokemon_at_slot(state, slot).map_or(true, |m| m.fainted) {
        return;
    }

    // ── 3. Spikes — flat fraction by layer count, grounded only ─────────────────────────────
    if grounded && !magic_guard {
        if let Some(SideCondition::Spikes(layers)) = conditions
            .iter()
            .find(|sc| matches!(sc, SideCondition::Spikes(_)))
        {
            let denom: u16 = match layers {
                1 => 8,
                2 => 6,
                _ => 4,
            };
            let dmg = (max_hp / denom).max(1);
            let env = berry_env(state, slot);
            let as_ = abilities_are_suppressed(state);
            if let Some(m) = get_pokemon_at_slot_mut(state, slot) {
                take_damage(m, dmg, env, as_);
            }
        }
    }
    if get_pokemon_at_slot(state, slot).map_or(true, |m| m.fainted) {
        return;
    }

    // ── 4. Toxic Spikes — poison, or absorbed by a grounded Poison-type ─────────────────────
    if grounded {
        if let Some(SideCondition::ToxicSpikes(layers)) = conditions
            .iter()
            .find(|sc| matches!(sc, SideCondition::ToxicSpikes(_)))
        {
            if is_poison_type {
                // A grounded Poison-type soaks up Toxic Spikes entirely (the layer payload is
                // ignored by the discriminant-based removal).
                remove_side_condition(state, slot.player, &SideCondition::ToxicSpikes(0));
            } else {
                let status = if *layers >= 2 {
                    Status::ToxicPoison(0)
                } else {
                    Status::Poison
                };
                // `apply_status_to_pokemon` reads weather from an immutable `&BattleState` while we
                // mutate the entrant, so hand it a snapshot (the established pattern in this module).
                let snapshot = state.clone();
                let sun_blocks_freeze = weather_is_sunlight(&snapshot);
                let tspikes_applied = if let Some(m) = get_pokemon_at_slot_mut(state, slot) {
                    apply_status_to_pokemon(&snapshot, sun_blocks_freeze, false, m, &status)
                } else { false };
                if tspikes_applied {
                    emit(state, EventKind::StatusInflicted { target: slot, status: status.clone() });
                }
            }
        }
    }
}

/// Returns the Showdown `onSwitchInPriority` for an ability.
/// Abilities not listed here default to 0.
/// Higher values activate first; Trick Room reverses only the speed tiebreak within a bracket.
pub fn ability_switch_in_priority(ability: &crate::data::ability::Ability) -> i8 {
    use crate::data::ability::Ability;
    match ability {
        Ability::NeutralizingGas | Ability::TeraShift => 2,
        Ability::AsOneGlastrier | Ability::AsOneSpectrier | Ability::Klutz | Ability::Unnerve => 1,
        Ability::Mimicry | Ability::Schooling | Ability::ShieldsDown => -1,
        Ability::Costar
        | Ability::FlowerGift
        | Ability::Forecast
        | Ability::Hospitality
        | Ability::IceFace
        | Ability::Protosynthesis
        | Ability::QuarkDrive => -2,
        _ => 0,
    }
}

/// Returns true for abilities that produce visible events on switch-in, warranting an
/// `AbilityRevealed` wrapper around their send-out effects.
fn ability_has_visible_send_out_effect(ability: &Ability) -> bool {
    matches!(ability,
        // Stat-drop target effects
        Ability::Intimidate
        // Weather setters
        | Ability::Drought | Ability::OrichalcumPulse | Ability::DesolateLand
        | Ability::Drizzle | Ability::PrimordialSea
        | Ability::SandStream | Ability::SnowWarning | Ability::DeltaStream
        // Terrain setters
        | Ability::ElectricSurge | Ability::HadronEngine
        | Ability::GrassySurge | Ability::MistySurge | Ability::PsychicSurge
        // Send-out-only visible effects
        | Ability::CuriousMedicine | Ability::Hospitality | Ability::ScreenCleaner
        | Ability::SupersweetSyrup | Ability::SupremeOverlord
        | Ability::Trace | Ability::Imposter
        | Ability::NeutralizingGas | Ability::TeraShift
        | Ability::Mimicry | Ability::Unnerve | Ability::AsOneGlastrier | Ability::AsOneSpectrier
        | Ability::Klutz
    )
}

/// Compute the Illusion disguise species for a Pokémon entering battle.
///
/// Returns the species the Pokémon should appear as, or `None` if:
/// - The mon's ability is not Illusion (or is suppressed).
/// - The mon has already Transformed (Imposter/Transform).
/// - The mon is itself the last non-fainted party member (no one to disguise as).
///
/// The disguise is the **last non-fainted** Pokémon in the holder's `*_back_mons`
/// (party order), matching the game's "last conscious party member" rule.
pub fn compute_illusion_disguise(state: &BattleState, slot: FieldSlot) -> Option<Species> {
    let mon = get_pokemon_at_slot(state, slot)?;
    if mon.ability != Ability::Illusion || mon.pre_transform.is_some() {
        return None;
    }
    // Find the last non-fainted mon in the back. Party order: active first, bench after.
    // Illusion picks the last non-fainted party member across the whole party.
    let back = match slot.player {
        Player::P1 => &state.p1_back_mons,
        Player::P2 => &state.p2_back_mons,
    };
    // Scan from the end of the bench to find the last non-fainted mon.
    back.iter().rev().find(|m| !m.fainted).map(|m| m.species.clone())
}

/// Return the species to report in a `SwitchState` event for the given slot,
/// applying Illusion disguise from the observer's perspective.
///
/// - When `slot.player == observer`, the observer sees their own mon — report the true species.
/// - Otherwise (opponent's mon), report the disguise species if Illusion is active.
pub fn observed_species(mon: &PokemonState, slot: FieldSlot, observer: Player) -> Species {
    if slot.player != observer {
        if let Some(ref disguise) = mon.illusion_disguise {
            return disguise.clone();
        }
    }
    mon.species.clone()
}

/// Break the Illusion disguise of the Pokémon at `slot` if it currently has one.
///
/// Call this whenever the mon's ability is suppressed, changed, or replaced (GastroAcid,
/// Mummy contact, Entrainment, Skill Swap, Worry Seed, Neutralizing Gas activation).
/// Emits `IllusionEnded` so the inference engine learns the true species.
pub fn maybe_break_illusion_on_ability_change(state: &mut BattleState, slot: FieldSlot) {
    let actual_species = get_pokemon_at_slot(state, slot)
        .and_then(|m| m.illusion_disguise.as_ref().map(|_| m.species.clone()));
    if let Some(species) = actual_species {
        if let Some(mon) = get_pokemon_at_slot_mut(state, slot) {
            mon.illusion_disguise = None;
        }
        emit(state, crate::information::information::EventKind::IllusionEnded {
            slot,
            actual_species: species,
        });
    }
}

pub fn process_pokemon_send_out(
    state: &mut BattleState,
    slot: FieldSlot,
    move_dex: &HashMap<PokemonMove, MoveData>,
) {
    // Borrow scope: extract the info we need before taking a mutable borrow.
    let (is_fainted, is_replacement_turn) = match get_pokemon_at_slot(state, slot) {
        None => return,
        Some(mon) => (mon.fainted, state.turn_started),
    };
    if is_fainted {
        return;
    }

    // Mark the Pokémon as having entered this turn, so end-of-turn abilities like Speed Boost
    // skip their effect on the entry turn.  Faint replacements arrive when turn_started == true
    // (the replacement "mini-turn" flag), so we leave the flag false in that case — they should
    // receive Speed Boost normally on their first end_turn.
    if !is_replacement_turn {
        if let Some(mon_mut) = get_pokemon_at_slot_mut(state, slot) {
            mon_mut.entered_this_turn = true;
        }
    }
    // Always mark the Pokémon as on their first turn on field (for Fake Out / First Impression).
    // Unlike `entered_this_turn`, this applies to faint-replacements too.
    // For U-turn/Volt Switch mid-turn entries (turn_started=true, turn_ended=false), EOT will
    // still run this turn BEFORE the Pokémon gets to act. Set the pending flag so EOT skips
    // clearing first_move_on_field once, preserving it to the Pokémon's actual first turn.
    let is_mid_turn_pre_eot = is_replacement_turn && !state.turn_ended;
    if let Some(mon_mut) = get_pokemon_at_slot_mut(state, slot) {
        mon_mut.first_move_on_field = true;
        mon_mut.first_turn_on_field_pending = is_mid_turn_pre_eot;
        mon_mut.used_moves_this_field = [false; 4];
    }

    // Entry hazards resolve before the switch-in ability. A Pokémon that faints to hazards (e.g.
    // Stealth Rock) never gets to trigger its entry ability (Intimidate, weather setters, …).
    apply_entry_hazards(state, slot);
    if get_pokemon_at_slot(state, slot).map_or(true, |m| m.fainted) {
        return;
    }

    // Healing Wish: if a HealingWish slot condition is present on this slot, fully restore the
    // entrant's HP and cure its status. The condition persists if the entrant is already at full
    // HP with no status condition (Gen 8+ behaviour — it waits for a damaged/statused Pokémon).
    // Hazard chip is healed because this check runs after apply_entry_hazards.
    {
        let slot_idx = slot.slot_index as usize;
        let has_healing_wish = match slot.player {
            crate::state::battle::Player::P1 => state
                .p1_slot_conditions
                .get(slot_idx)
                .map(|conds| {
                    conds
                        .iter()
                        .any(|sc| matches!(sc, crate::state::dex_data::SlotCondition::HealingWish))
                })
                .unwrap_or(false),
            crate::state::battle::Player::P2 => state
                .p2_slot_conditions
                .get(slot_idx)
                .map(|conds| {
                    conds
                        .iter()
                        .any(|sc| matches!(sc, crate::state::dex_data::SlotCondition::HealingWish))
                })
                .unwrap_or(false),
        };
        if has_healing_wish {
            let (max_hp, current_hp, has_status, old_status) = match get_pokemon_at_slot(state, slot) {
                Some(mon) => (mon.stats[0].max(1), mon.hp, mon.status.is_some(), mon.status.clone()),
                None => (0, 0, false, None),
            };
            let needs_heal = current_hp < max_hp || has_status;
            if needs_heal {
                if let Some(mon) = get_pokemon_at_slot_mut(state, slot) {
                    mon.hp = max_hp;
                    mon.status = None;
                }
                // Remove the HealingWish condition from this slot.
                {
                    let conds = match slot.player {
                        crate::state::battle::Player::P1 => &mut state.p1_slot_conditions,
                        crate::state::battle::Player::P2 => &mut state.p2_slot_conditions,
                    };
                    if let Some(slot_conds) = conds.get_mut(slot_idx) {
                        slot_conds
                            .retain(|sc| !matches!(sc, crate::state::dex_data::SlotCondition::HealingWish));
                    }
                } // conds borrow ends here — state is free for emit
                // Emit SlotConditionEnd with Healed / StatusCured nested as reactions.
                with_reactions(
                    state,
                    EventKind::SlotConditionEnd {
                        slot,
                        condition: crate::state::dex_data::SlotCondition::HealingWish,
                    },
                    |bs| {
                        if let Some(observer) = bs.event_observer {
                            if current_hp < max_hp {
                                let new_hp = if slot.player == observer {
                                    PokemonHP::Number(max_hp)
                                } else {
                                    PokemonHP::Percent(hp_to_percent(max_hp, max_hp))
                                };
                                emit(bs, EventKind::Healed { target: slot, new_hp });
                            }
                            if let Some(ref status) = old_status {
                                emit(bs, EventKind::StatusCured { target: slot, status: status.clone() });
                            }
                        }
                    },
                );
            }
            // If the entrant is already full HP with no status, leave the condition in place.
        }
    }

    let ability = match get_pokemon_at_slot(state, slot) {
        None => return,
        Some(mon) => mon.ability.clone(),
    };

    let ability_suppressed = get_pokemon_at_slot(state, slot)
        .map(|mon| pokemon_ability_is_suppressed(state, mon))
        .unwrap_or(true);

    // Set up Illusion disguise: if the entrant has Illusion and a valid target in the back,
    // record the disguise species on the mon. The disguise is conveyed to the observer via
    // the Switch/SimultaneousSwitch event (the species field is perspective-gated there).
    // We compute it even if the ability appears suppressed — suppression itself clears it later.
    if !ability_suppressed {
        let disguise = compute_illusion_disguise(state, slot);
        if let Some(mon) = get_pokemon_at_slot_mut(state, slot) {
            mon.illusion_disguise = disguise;
        }
    }

    if !ability_suppressed {
        if ability_has_visible_send_out_effect(&ability) {
            let ability_event = ability.clone();
            with_reactions(state, crate::information::information::EventKind::AbilityRevealed { slot, ability: ability_event }, |bs| {
                apply_entry_ability_field_effects(bs, slot, &ability);
                apply_entry_ability_target_effects(bs, slot, &ability);
                apply_send_out_only_ability_effects(bs, slot, &ability, move_dex);
            });
        } else {
            apply_entry_ability_field_effects(state, slot, &ability);
            apply_entry_ability_target_effects(state, slot, &ability);
            apply_send_out_only_ability_effects(state, slot, &ability, move_dex);
        }
    }

    trigger_terrain_seed_items(state);

    // A Pokémon entering may bring Neutralizing Gas, which suppresses primal-weather
    // abilities and so ends the extreme weather they were maintaining.
    handle_gas_primal_weather_suppression(state);

    // The entrant may be a Castform, set new weather, or bring Cloud Nine / Air Lock.
    update_forecast_forms(state);
}

/// Transform `transformer` into `target`, following the rules for both the Transform move
/// and the Imposter ability.
///
/// Returns `true` if the transform succeeded; the caller is responsible for firing any
/// on-gain ability effects afterwards when needed.
///
/// **What is copied:** species, types, non-HP stats, stat stages, moves (PP capped at 5),
/// ability, gender, weight.
/// **What is NOT copied:** HP (both current and max), level, item, status, nature, EVs/IVs,
/// tera_type / is_tera.
///
/// **Failure conditions (no change, returns false):**
/// - Target is behind a Substitute.
/// - Target is already transformed.
/// - Target has Illusion or Imposter ability.
pub fn transform_into(transformer: &mut PokemonState, target: &PokemonState) -> bool {
    // Fail if target is behind a Substitute.
    if has_status_volatile(target, &VolatileStatus::Substitute(0)) {
        return false;
    }
    // Fail if target is already transformed.
    if target.pre_transform.is_some() {
        return false;
    }
    // Fail if target's ability is Illusion or Imposter.
    if matches!(target.ability, Ability::Illusion | Ability::Imposter) {
        return false;
    }

    // Save original form exactly once (re-entering after a failed transform is fine).
    if transformer.pre_transform.is_none() {
        transformer.pre_transform = Some(Box::new(transformer.clone()));
    }

    // Copy species and appearance.
    transformer.species = target.species.clone();
    transformer.types = target.types.clone();
    transformer.gender = target.gender;
    transformer.weight_hg = target.weight_hg;

    // Copy non-HP stats (index 0 = max HP, which stays own).
    transformer.stats[1] = target.stats[1];
    transformer.stats[2] = target.stats[2];
    transformer.stats[3] = target.stats[3];
    transformer.stats[4] = target.stats[4];
    transformer.stats[5] = target.stats[5];

    // Copy stat stages.
    transformer.boosts = target.boosts;

    // Copy moves, capping PP at 5 per move (Transform/Imposter rule).
    transformer.moves = target.moves.clone();
    for i in 0..4 {
        let capped = target.max_pp[i].min(5);
        transformer.move_pp[i] = capped;
        transformer.max_pp[i] = capped;
    }

    // Copy ability.
    transformer.ability = target.ability.clone();

    // Note: hp, stats[0], level, item, status, nature, evs, ivs, tera_type, is_tera
    // are intentionally not copied.
    true
}

/// Apply entry abilities that affect opposing Pokémon (e.g. Intimidate lowering the
/// Attack of every opposing active Pokémon). Shared by `process_pokemon_send_out` and
/// `process_pokemon_gain_ability`.
fn apply_entry_ability_target_effects(state: &mut BattleState, slot: FieldSlot, ability: &Ability) {
    if *ability == Ability::Intimidate {
        let opposing_player = match slot.player {
            Player::P1 => Player::P2,
            Player::P2 => Player::P1,
        };
        let items_suppressed = items_are_suppressed(state);
        for target in collect_active_slots(state, opposing_player, None) {
            // InnerFocus / OwnTempo / Oblivious / Scrappy grant immunity to Intimidate.
            // Intimidate is ability-sourced (not a move), so Mold Breaker does not apply.
            let immune = get_pokemon_at_slot(state, target).map_or(false, |m| {
                !pokemon_ability_is_suppressed(state, m)
                    && matches!(
                        m.ability,
                        Ability::InnerFocus
                            | Ability::OwnTempo
                            | Ability::Oblivious
                            | Ability::Scrappy
                    )
            });
            if immune {
                continue;
            }
            // apply_opponent_stat_drop handles Clear Body / Hyper Cutter / Mirror Armor /
            // Defiant / Competitive reactions and the new Contrary inversion.
            apply_opponent_stat_drop(
                state,
                target,
                slot,
                [-1, 0, 0, 0, 0, 0, 0],
                items_suppressed,
                false,
            );
        }
    }
}

/// Apply entry abilities whose effects only make sense on switch-in (healing, stat resets,
/// screen removal, Imposter/Trace, Frisk, Anticipation). These are deliberately NOT shared
/// with `apply_entry_ability_field_effects` or `apply_entry_ability_target_effects`, so they
/// will not re-fire when Neutralizing Gas lifts.
fn apply_send_out_only_ability_effects(
    state: &mut BattleState,
    slot: FieldSlot,
    ability: &Ability,
    move_dex: &HashMap<PokemonMove, MoveData>,
) {
    let own_player = slot.player;
    let opp_player = match slot.player {
        Player::P1 => Player::P2,
        Player::P2 => Player::P1,
    };
    let items_suppressed = items_are_suppressed(state);

    match ability {
        // ── Curious Medicine ──────────────────────────────────────────────────────────
        // Reset all stat stages of every ally (not self) to zero.
        Ability::CuriousMedicine => {
            for ally_slot in collect_active_slots(state, own_player, Some(slot.slot_index)) {
                if let Some(mon) = get_pokemon_at_slot_mut(state, ally_slot) {
                    mon.boosts = [0; 7];
                }
            }
        }

        // ── Hospitality ───────────────────────────────────────────────────────────────
        // Heal each ally by ¼ of that ally's max HP.
        Ability::Hospitality => {
            for ally_slot in collect_active_slots(state, own_player, Some(slot.slot_index)) {
                let heal = get_pokemon_at_slot(state, ally_slot)
                    .map(|m| (m.stats[0] / 4).max(1))
                    .unwrap_or(0);
                let ally_env = berry_env(state, ally_slot);
                if heal > 0 {
                    if let Some(mon) = get_pokemon_at_slot_mut(state, ally_slot) {
                        gain_hp(mon, heal, ally_env);
                    }
                }
            }
        }

        // ── Screen Cleaner ────────────────────────────────────────────────────────────
        // Remove Light Screen, Reflect, and Aurora Veil from BOTH sides.
        Ability::ScreenCleaner => {
            for player in [Player::P1, Player::P2] {
                remove_side_condition(state, player, &SideCondition::LightScreen);
                remove_side_condition(state, player, &SideCondition::Reflect);
                remove_side_condition(state, player, &SideCondition::AuroraVeil);
            }
        }

        // ── Supersweet Syrup ──────────────────────────────────────────────────────────
        // Once per battle: lower all opponents' evasiveness by 1.
        Ability::SupersweetSyrup => {
            let already_used = get_pokemon_at_slot(state, slot)
                .map(|m| m.one_time_ability_used)
                .unwrap_or(true);
            if !already_used {
                // Mark used first (borrow ends before the loop below).
                if let Some(mon) = get_pokemon_at_slot_mut(state, slot) {
                    mon.one_time_ability_used = true;
                }
                for target in collect_active_slots(state, opp_player, None) {
                    // apply_opponent_stat_drop handles immunity, Mirror Armor, and reactions.
                    // Index 6 = Evasiveness.
                    apply_opponent_stat_drop(
                        state,
                        target,
                        slot,
                        [0, 0, 0, 0, 0, 0, -1],
                        items_suppressed,
                        false,
                    );
                }
            }
        }

        // ── Supreme Overlord ──────────────────────────────────────────────────────────
        // Snapshot fainted ally count (1–5) into a permanent volatile at switch-in.
        Ability::SupremeOverlord => {
            let (active, back) = match own_player {
                Player::P1 => (&state.p1_active_mons, &state.p1_back_mons),
                Player::P2 => (&state.p2_active_mons, &state.p2_back_mons),
            };
            // Count all fainted party members (holder itself is not fainted — guarded at top).
            let fainted = active
                .iter()
                .chain(back.iter())
                .filter(|m| m.fainted)
                .count()
                .min(5) as u8;

            // Remove any stale SupremeOverlord volatile (re-entry after bench time).
            if let Some(mon) = get_pokemon_at_slot_mut(state, slot) {
                mon.volatiles.retain(|v| {
                    !matches!(
                        v,
                        VolatileStatusState::TurnStatus(VolatileStatus::SupremeOverlord(_), _)
                    )
                });
                if fainted > 0 {
                    mon.volatiles.push(VolatileStatusState::TurnStatus(
                        VolatileStatus::SupremeOverlord(fainted),
                        0,
                    ));
                }
            }
        }

        // ── Trace ────────────────────────────────────────────────────────────────────
        // Copy the first traceable opponent's ability; revert on switch-out via
        // the existing `original_ability` mechanism.
        Ability::Trace => {
            // Find the first eligible opponent (non-fainted, non-suppressed, traceable ability).
            let mut traced: Option<Ability> = None;
            for opp_slot in collect_active_slots(state, opp_player, None) {
                let eligible = get_pokemon_at_slot(state, opp_slot).and_then(|m| {
                    let suppressed = pokemon_ability_is_suppressed(state, m);
                    if !suppressed && !ability_cannot_be_traced(&m.ability) {
                        Some(m.ability.clone())
                    } else {
                        None
                    }
                });
                if let Some(ab) = eligible {
                    traced = Some(ab);
                    break;
                }
            }
            if let Some(new_ability) = traced {
                if let Some(mon) = get_pokemon_at_slot_mut(state, slot) {
                    // Stash original so switch-out reverts it (same as Mummy/Wandering Spirit).
                    if mon.original_ability.is_none() {
                        mon.original_ability = Some(mon.ability.clone());
                    }
                    mon.ability = new_ability.clone();
                }
                // Fire the newly-traced ability's gain effects (Intimidate, weather, etc.),
                // wrapped under AbilityRevealed for the traced ability so the player can see
                // what was copied and observe its downstream effects.
                if ability_has_visible_send_out_effect(&new_ability) {
                    let ab_ev = new_ability.clone();
                    with_reactions(state, crate::information::information::EventKind::AbilityRevealed { slot, ability: ab_ev }, |bs| {
                        apply_entry_ability_field_effects(bs, slot, &new_ability);
                        apply_entry_ability_target_effects(bs, slot, &new_ability);
                    });
                } else {
                    emit(state, crate::information::information::EventKind::AbilityRevealed { slot, ability: new_ability.clone() });
                    apply_entry_ability_field_effects(state, slot, &new_ability);
                    apply_entry_ability_target_effects(state, slot, &new_ability);
                }
            }
        }

        // ── Imposter ─────────────────────────────────────────────────────────────────
        // Transform into the directly-opposite opponent on entry.
        Ability::Imposter => {
            // In singles (and as the default for doubles), the directly-opposite slot
            // is the slot with the same index on the opposing side.
            let opposite = FieldSlot {
                player: opp_player,
                slot_index: slot.slot_index,
            };
            let target_snapshot = get_pokemon_at_slot(state, opposite).cloned();
            if let Some(target) = target_snapshot {
                if let Some(transformer) = get_pokemon_at_slot_mut(state, slot) {
                    let success = transform_into(transformer, &target);
                    if success {
                        let new_ability = transformer.ability.clone();
                        // Fire the copied ability's gain effects (Intimidate, weather, etc.).
                        apply_entry_ability_field_effects(state, slot, &new_ability);
                        apply_entry_ability_target_effects(state, slot, &new_ability);
                    }
                }
            }
        }

        // ── Mimicry ───────────────────────────────────────────────────────────────
        // On switch-in, immediately adopt the type matching the active terrain (if any).
        Ability::Mimicry => {
            update_mimicry_forms(state);
        }

        // ── Frisk ─────────────────────────────────────────────────────────────────
        // Reveal every opposing active Pokémon's held item. No message if a foe holds
        // no item (only emit ItemRevealed for item-holders). Gen VI+ behaviour: reveals
        // ALL opposing active mons' items, left-to-right.
        Ability::Frisk => {
            // Collect (slot, item) pairs for foes that hold an item.
            let opp_items: Vec<(FieldSlot, Item)> = collect_active_slots(state, opp_player, None)
                .into_iter()
                .filter_map(|opp_slot| {
                    get_pokemon_at_slot(state, opp_slot).and_then(|m| {
                        if m.item != Item::None { Some((opp_slot, m.item.clone())) } else { None }
                    })
                })
                .collect();
            if !opp_items.is_empty() {
                with_reactions(
                    state,
                    crate::information::information::EventKind::AbilityRevealed {
                        slot,
                        ability: Ability::Frisk,
                    },
                    |bs| {
                        for (opp_slot, item) in opp_items {
                            emit(bs, crate::information::information::EventKind::ItemRevealed {
                                slot: opp_slot,
                                item,
                            });
                        }
                    },
                );
            }
        }

        // ── Anticipation ─────────────────────────────────────────────────────────
        // Shudder on switch-in if any opposing active Pokémon knows a move that is
        // super-effective against the holder's types, or an OHKO move.
        // Message-only: no battle-state change (full-information sim).
        //
        // Type rules per Bulbapedia: use the move's declared pokemon_type field.
        // Overrides: Revelation Dance / Multi-Attack → Normal; Flying Press → Fighting;
        // Aura Wheel → Electric; Freeze-Dry → ordinary Ice.
        // Self-Destruct / Explosion are Normal and only trigger if SE.
        // Does NOT account for attacker's type-changing abilities (Aerilate, etc.).
        Ability::Anticipation => {
            let holder_types: Vec<PokemonType> = get_pokemon_at_slot(state, slot)
                .map(|m| m.types.clone())
                .unwrap_or_default();
            if holder_types.is_empty() {
                return; // can't check without types
            }

            // Helper: Anticipation-effective type for a move (overrides per Bulbapedia).
            let anticipation_type = |move_name: &PokemonMove, md: &MoveData| -> PokemonType {
                match move_name {
                    PokemonMove::RevelationDance | PokemonMove::MultiAttack => PokemonType::Normal,
                    PokemonMove::FlyingPress => PokemonType::Fighting,
                    PokemonMove::AuraWheel => PokemonType::Electric,
                    // Freeze-Dry is ordinary Ice for Anticipation (ignore Water quirk)
                    PokemonMove::FreezeDry => PokemonType::Ice,
                    _ => md.pokemon_type.clone(),
                }
            };

            let threat_found = collect_active_slots(state, opp_player, None)
                .into_iter()
                .any(|opp_slot| {
                    get_pokemon_at_slot(state, opp_slot).map_or(false, |foe| {
                        foe.moves.iter().any(|mv_opt| {
                            let Some(mv_name) = mv_opt else { return false; };
                            let Some(md) = move_dex.get(mv_name) else { return false; };
                            // Skip status-category moves.
                            if md.category == MoveCategory::Status {
                                return false;
                            }
                            // OHKO moves always trigger.
                            if md.ohko {
                                return true;
                            }
                            // Check super-effective: product of per-type multipliers > 1.
                            let eff_type = anticipation_type(mv_name, md);
                            let effectiveness: f64 = holder_types
                                .iter()
                                .map(|dt| single_type_effectiveness(&eff_type, dt))
                                .product();
                            effectiveness > 1.0
                        })
                    })
                });

            if threat_found {
                with_reactions(
                    state,
                    crate::information::information::EventKind::AnticipationShudder { slot },
                    |bs| {
                        emit(bs, crate::information::information::EventKind::AbilityRevealed {
                            slot,
                            ability: Ability::Anticipation,
                        });
                    },
                );
            }
        }

        _ => {}
    }
}

/// Apply the field-setting effects of an entry ability (weather/terrain setters).
/// Shared by `process_pokemon_send_out` (a Pokémon switching in) and
/// `process_pokemon_gain_ability` (a Pokémon gaining an ability mid-battle).
/// `setter_slot` is used to check for weather-extending rock items on the setter.
fn apply_entry_ability_field_effects(
    state: &mut BattleState,
    setter_slot: FieldSlot,
    ability: &Ability,
) {
    // Pre-snapshot whether each rock item is held (before mutably borrowing state).
    let dur_for = |w: &Weather| -> u8 {
        get_pokemon_at_slot(state, setter_slot)
            .map(|m| weather_rock_duration(m, w))
            .unwrap_or(5)
    };
    match ability {
        Ability::ElectricSurge | Ability::HadronEngine => {
            set_terrain(state, Terrain::ElectricTerrain, 5)
        }
        Ability::GrassySurge => set_terrain(state, Terrain::GrassyTerrain, 5),
        Ability::MistySurge => set_terrain(state, Terrain::MistyTerrain, 5),
        Ability::PsychicSurge => set_terrain(state, Terrain::PsychicTerrain, 5),
        Ability::Drought | Ability::OrichalcumPulse => {
            let d = dur_for(&Weather::Sun);
            set_weather(state, Weather::Sun, d);
        }
        Ability::DesolateLand => set_weather(state, Weather::ExtremeSunlight, 0),
        Ability::Drizzle => {
            let d = dur_for(&Weather::Rain);
            set_weather(state, Weather::Rain, d);
        }
        Ability::PrimordialSea => set_weather(state, Weather::HeavyRain, 0),
        Ability::SandStream => {
            let d = dur_for(&Weather::Sandstorm);
            set_weather(state, Weather::Sandstorm, d);
        }
        Ability::SnowWarning => {
            let d = dur_for(&Weather::Snow);
            set_weather(state, Weather::Snow, d);
        }
        Ability::DeltaStream => set_weather(state, Weather::StrongWinds, 0),
        _ => {}
    }
}

/// Apply the on-gain effects of the ability of the Pokémon at `slot`.
///
/// Used when a Pokémon *gains* an ability mid-battle rather than switching in — most
/// notably when Neutralizing Gas stops applying and previously-suppressed entry
/// abilities (weather/terrain setters) reactivate. Mirrors `process_pokemon_send_out`
/// but deliberately does not run switch-in-only effects such as entry hazards.
pub fn process_pokemon_gain_ability(state: &mut BattleState, slot: FieldSlot) {
    let Some(mon) = get_pokemon_at_slot(state, slot) else {
        return;
    };

    if mon.fainted {
        return;
    }

    let ability = mon.ability.clone();

    if pokemon_ability_is_suppressed(state, mon) {
        return;
    }

    apply_entry_ability_field_effects(state, slot, &ability);
    apply_entry_ability_target_effects(state, slot, &ability);
    trigger_terrain_seed_items(state);
    update_forecast_forms(state);
}

/// Handle every effect triggered when a Pokémon switches out. Call this *after* the
/// active/bench swap, passing the bench index where the departing Pokémon now rests.
///
/// Covers:
/// - Switch-out abilities on the departing Pokémon (Natural Cure, Regenerator),
///   skipped while abilities are suppressed.
/// - Neutralizing Gas suppression lifting once its holder is gone.
/// - Primal weather (Desolate Land / Primordial Sea / Delta Stream) ending, unless
///   another active holder of the same ability remains.
pub fn handle_pokemon_switch_out(state: &mut BattleState, player: Player, bench_index: usize) {
    let abilities_suppressed = abilities_are_suppressed(state);
    let items_suppressed = items_are_suppressed(state);

    // Apply the departing Pokémon's own switch-out ability, and note its ability and id.
    let (departed_ability, departing_mon_id) = {
        let back = match player {
            Player::P1 => &mut state.p1_back_mons,
            Player::P2 => &mut state.p2_back_mons,
        };
        let Some(departed) = back.get_mut(bench_index) else {
            return;
        };
        let ability = departed.ability.clone();
        let mon_id = departed.mon_id;
        if !abilities_suppressed {
            apply_switch_out_ability_effects(departed, BerryEnv::simple(items_suppressed));
        }
        (ability, mon_id)
    };

    // Neutralizing Gas suppression lifts when its holder leaves the field.
    if departed_ability == Ability::NeutralizingGas && !any_pokemon_has_neutralizing_gas(state) {
        handle_neutralizing_gas_lift(state);
    }

    // Primal weather ends when its source leaves, unless another holder remains.
    handle_primal_weather_departure(state, &departed_ability);

    // Departing weather sources / Cloud Nine users may change Castform's form.
    update_forecast_forms(state);

    // Release any binding/trapping volatiles that this Pokémon had set on opponents.
    release_traps_set_by(state, departing_mon_id);
}

/// Handle field effects when the Pokémon at `slot_index` (for `player`) faints:
/// Neutralizing Gas suppression lifting and primal weather ending. Unlike switching out,
/// fainting does not trigger Natural Cure / Regenerator. The fainted Pokémon is expected
/// to still occupy its active slot (with `fainted == true`); the helpers below ignore
/// fainted Pokémon, so it is correctly treated as gone from the field.
pub fn handle_pokemon_faint(state: &mut BattleState, player: Player, slot_index: u8) {
    let (fainted_ability, fainted_mon_id) = {
        let mons = match player {
            Player::P1 => &state.p1_active_mons,
            Player::P2 => &state.p2_active_mons,
        };
        let Some(mon) = mons.get(slot_index as usize) else {
            return;
        };
        (mon.ability.clone(), mon.mon_id)
    };

    // Neutralizing Gas suppression lifts when its holder faints.
    if fainted_ability == Ability::NeutralizingGas && !any_pokemon_has_neutralizing_gas(state) {
        handle_neutralizing_gas_lift(state);
    }

    // Primal weather ends when its source faints, unless another holder remains.
    handle_primal_weather_departure(state, &fainted_ability);

    // Receiver: an ally with (unsuppressed) Receiver inherits the fainted Pokémon's
    // ability. The original is stashed so the usual switch-out revert applies.
    if !ability_cannot_be_received(&fainted_ability) {
        let receiver_slot = collect_active_slots(state, player, Some(slot_index))
            .into_iter()
            .find(|s| {
                get_pokemon_at_slot(state, *s).is_some_and(|m| {
                    !m.fainted
                        && m.ability == Ability::Receiver
                        && !pokemon_ability_is_suppressed(state, m)
                })
            });
        if let Some(receiver_slot) = receiver_slot {
            if let Some(receiver) = get_pokemon_at_slot_mut(state, receiver_slot) {
                if receiver.original_ability.is_none() {
                    receiver.original_ability = Some(receiver.ability.clone());
                }
                receiver.ability = fainted_ability.clone();
            }
            // Fire on-gain effects (weather setters, Intimidate, …) for the new ability.
            process_pokemon_gain_ability(state, receiver_slot);
        }
    }

    // A fainting weather source / Cloud Nine user may change Castform's form.
    update_forecast_forms(state);

    // Release any binding/trapping volatiles that this Pokémon had set on opponents.
    release_traps_set_by(state, fainted_mon_id);
}

/// While Neutralizing Gas is active it suppresses primal-weather abilities, so any
/// extreme weather they were maintaining ends. Called when a Pokémon enters the field
/// (which may bring Neutralizing Gas with it).
fn handle_gas_primal_weather_suppression(state: &mut BattleState) {
    if !abilities_are_suppressed(state) {
        return;
    }
    // End primal weather if the source ability is now suppressed.
    if matches!(
        state.weather,
        Some(Weather::ExtremeSunlight | Weather::HeavyRain | Weather::StrongWinds)
    ) {
        state.weather = None;
        state.weather_turns = None;
    }
    // Break any active Illusion disguises — Neutralizing Gas now suppresses Illusion.
    let all_slots: Vec<FieldSlot> = collect_active_slots(state, Player::P1, None)
        .into_iter()
        .chain(collect_active_slots(state, Player::P2, None))
        .collect();
    for slot in all_slots {
        maybe_break_illusion_on_ability_change(state, slot);
    }
}

/// Apply on-switch-out ability effects for `mon` (Natural Cure curing status,
/// Regenerator restoring up to 1/3 of max HP). Callers must skip this while abilities
/// are suppressed.
fn apply_switch_out_ability_effects(mon: &mut PokemonState, env: BerryEnv) {
    if mon.fainted {
        return;
    }
    // Revert a Transform/Imposter transformation.  Preserve live HP, status, and the
    // fainted flag (damage taken while transformed carries over); everything else reverts
    // to the saved pre-transform snapshot.  Boosts are zeroed separately by
    // `clear_pokemon_for_switch_out`, which runs before this function, so we don't
    // need to touch them here.
    if let Some(saved) = mon.pre_transform.take() {
        let live_hp = mon.hp;
        let live_status = mon.status.clone();
        let live_fainted = mon.fainted;
        *mon = *saved;
        mon.hp = live_hp;
        mon.status = live_status;
        mon.fainted = live_fainted;
        // `boosts` will be zeroed by the caller; no need to overwrite here.
    }
    // Revert ability stolen/replaced by Mummy or Wandering Spirit.
    if let Some(original) = mon.original_ability.take() {
        mon.ability = original;
    }
    // Mimicry: restore original types on switch-out if they were overwritten by terrain.
    if let Some(orig) = mon.pre_mimicry_types.take() {
        mon.types = orig;
    }
    match mon.ability {
        Ability::NaturalCure => {
            mon.status = None;
        }
        Ability::Regenerator => {
            let heal = (mon.stats[0] / 3).max(1);
            gain_hp(mon, heal, env);
        }
        _ => {}
    }
}

/// Re-trigger on-gain abilities for every active Pokémon once Neutralizing Gas is no
/// longer applying. Each non-fainted active Pokémon effectively re-gains its ability,
/// so suppressed entry abilities (weather/terrain setters) activate again.
fn handle_neutralizing_gas_lift(state: &mut BattleState) {
    let mut slots = collect_active_slots(state, Player::P1, None);
    slots.extend(collect_active_slots(state, Player::P2, None));
    for slot in slots {
        process_pokemon_gain_ability(state, slot);
    }
}

/// Forecast: keep every active Castform's form and type in sync with the current weather.
/// Sun → Sunny (Fire), rain → Rainy (Water), snow → Snowy (Ice); anything else, no
/// weather, Cloud Nine / Air Lock on the field, or a suppressed/absent Forecast ability
/// reverts to base Castform (Normal). All forms share stats, so no dex lookup is needed.
/// Call after anything that can alter weather, ability, or the active roster.
pub fn update_forecast_forms(state: &mut BattleState) {
    let weather_negated = active_mons_have_ability(state, &Ability::CloudNine)
        || active_mons_have_ability(state, &Ability::AirLock);
    let mut slots = collect_active_slots(state, Player::P1, None);
    slots.extend(collect_active_slots(state, Player::P2, None));
    for slot in slots {
        let (is_castform, forecast_inactive, current) = match get_pokemon_at_slot(state, slot) {
            Some(m) => (
                matches!(
                    m.species,
                    Species::Castform
                        | Species::CastformRainy
                        | Species::CastformSnowy
                        | Species::CastformSunny
                ),
                m.ability != Ability::Forecast || pokemon_ability_is_suppressed(state, m),
                m.species.clone(),
            ),
            None => continue,
        };
        if !is_castform {
            continue;
        }
        let target = if forecast_inactive || weather_negated {
            Species::Castform
        } else if weather_is_sunlight(state) {
            Species::CastformSunny
        } else if weather_is_rain(state) {
            Species::CastformRainy
        } else if weather_is_snow(state) {
            Species::CastformSnowy
        } else {
            Species::Castform
        };
        if target == current {
            continue;
        }
        let types = match target {
            Species::CastformSunny => vec![PokemonType::Fire],
            Species::CastformRainy => vec![PokemonType::Water],
            Species::CastformSnowy => vec![PokemonType::Ice],
            _ => vec![PokemonType::Normal],
        };
        if let Some(m) = get_pokemon_at_slot_mut(state, slot) {
            m.species = target.clone();
            m.types = types;
        }
        emit(state, crate::information::information::EventKind::FormeChange {
            slot,
            into: target,
            permanent: false,
        });
    }
}

/// Mimicry: change each active Mimicry holder's type to match the current terrain
/// (Electric→Electric, Grassy→Grass, Misty→Fairy, Psychic→Psychic). When terrain ends,
/// revert to the saved `pre_mimicry_types`. Mirrors `update_forecast_forms`.
/// Call after any terrain change (set_terrain, clear_terrain) and on send-in.
pub fn update_mimicry_forms(state: &mut BattleState) {
    let terrain = state.terrain.clone();
    let new_type = match &terrain {
        Some(Terrain::ElectricTerrain) => Some(PokemonType::Electric),
        Some(Terrain::GrassyTerrain) => Some(PokemonType::Grass),
        Some(Terrain::MistyTerrain) => Some(PokemonType::Fairy),
        Some(Terrain::PsychicTerrain) => Some(PokemonType::Psychic),
        _ => None,
    };

    let mut slots = collect_active_slots(state, Player::P1, None);
    slots.extend(collect_active_slots(state, Player::P2, None));

    for slot in slots {
        let has_mimicry = get_pokemon_at_slot(state, slot).map_or(false, |m| {
            !m.fainted && !pokemon_ability_is_suppressed(state, m) && m.ability == Ability::Mimicry
        });
        if !has_mimicry {
            continue;
        }

        if let Some(t) = &new_type {
            // Terrain is active: store original types (if not already stored) and overwrite.
            let changed = get_pokemon_at_slot(state, slot).map_or(false, |m| {
                m.types.len() != 1 || m.types[0] != *t
            });
            if let Some(mon) = get_pokemon_at_slot_mut(state, slot) {
                if mon.pre_mimicry_types.is_none() {
                    mon.pre_mimicry_types = Some(mon.types.clone());
                }
                mon.types = vec![t.clone()];
            }
            if changed {
                emit(state, crate::information::information::EventKind::TypeChanged {
                    slot,
                    new_types: vec![t.clone()],
                });
            }
        } else {
            // No terrain: restore original types if we saved them.
            let saved = get_pokemon_at_slot(state, slot).and_then(|m| m.pre_mimicry_types.clone());
            if let Some(orig) = saved {
                if let Some(mon) = get_pokemon_at_slot_mut(state, slot) {
                    mon.types = orig.clone();
                    mon.pre_mimicry_types = None;
                }
                emit(state, crate::information::information::EventKind::TypeChanged {
                    slot,
                    new_types: orig,
                });
            }
        }
    }
}

/// Return true if any non-fainted active Pokémon has `ability`.
fn active_mons_have_ability(state: &BattleState, ability: &Ability) -> bool {
    state
        .p1_active_mons
        .iter()
        .chain(state.p2_active_mons.iter())
        .any(|mon| !mon.fainted && mon.ability == *ability)
}

/// When a Pokémon with a primal-weather ability leaves the field, the weather it
/// maintained ends — unless another active Pokémon still has the same ability.
///
/// - Desolate Land  -> Extreme Sunlight
/// - Primordial Sea -> Heavy Rain
/// - Delta Stream   -> Strong Winds
fn handle_primal_weather_departure(state: &mut BattleState, departed_ability: &Ability) {
    let weather = match departed_ability {
        Ability::DesolateLand => Weather::ExtremeSunlight,
        Ability::PrimordialSea => Weather::HeavyRain,
        Ability::DeltaStream => Weather::StrongWinds,
        _ => return,
    };

    // Another holder of the same ability keeps the weather active.
    if active_mons_have_ability(state, departed_ability) {
        return;
    }

    // Only clear if that primal weather is the one currently in effect.
    if state.weather.as_ref() == Some(&weather) {
        state.weather = None;
        state.weather_turns = None;
    }
}

/// Set terrain. Only one terrain can be active at a time. Provide a duration in turns (0 = permanent).
pub fn set_terrain(state: &mut BattleState, terrain: Terrain, duration: u8) {
    state.terrain = Some(terrain.clone());
    state.terrain_turns = Some(duration);
    emit(state, EventKind::TerrainChanged { terrain: Some(terrain) });

    trigger_terrain_seed_items(state);
    update_mimicry_forms(state);
}

/// Add pseudo-weather, avoiding duplicates and handling duration.
pub fn add_pseudo_weather(state: &mut BattleState, pseudo_weather: PseudoWeather, duration: u8) {
    if state
        .pseudo_weathers
        .iter()
        .any(|pw| std::mem::discriminant(pw) == std::mem::discriminant(&pseudo_weather))
    {
        return;
    }
    let is_gravity = matches!(pseudo_weather, PseudoWeather::Gravity);
    state.pseudo_weathers.push(pseudo_weather.clone());
    state.pseudo_weather_turns.push(duration);
    emit(state, EventKind::PseudoWeatherStart { effect: pseudo_weather });
    if is_gravity {
        on_gravity_activated(state);
    }
}

/// When Gravity activates, cancel any Fly/Bounce/Sky Drop semi-invulnerability or charging
/// volatiles, and strip Magnet Rise and Telekinesis from all active Pokémon.
fn on_gravity_activated(state: &mut BattleState) {
    let gravity_interrupted: &[PokemonMove] =
        &[PokemonMove::Fly, PokemonMove::Bounce, PokemonMove::SkyDrop];
    for mon in state
        .p1_active_mons
        .iter_mut()
        .chain(state.p2_active_mons.iter_mut())
    {
        mon.volatiles.retain(|v| {
            if let VolatileStatusState::MoveStatus(VolatileStatus::SemiInvulnerable(m), _) = v {
                if gravity_interrupted.contains(m) {
                    return false;
                }
            }
            if let VolatileStatusState::Charging(m, _) = v {
                if gravity_interrupted.contains(m) {
                    return false;
                }
            }
            if matches!(
                v,
                VolatileStatusState::TurnStatus(VolatileStatus::MagnetRise, _)
                    | VolatileStatusState::MoveStatus(VolatileStatus::MagnetRise, _)
                    | VolatileStatusState::TurnStatus(VolatileStatus::Telekinesis, _)
                    | VolatileStatusState::MoveStatus(VolatileStatus::Telekinesis, _)
            ) {
                return false;
            }
            true
        });
    }
}

/// Remove pseudo-weather by discriminant.
pub fn remove_pseudo_weather(state: &mut BattleState, pseudo_weather: &PseudoWeather) {
    if let Some(pos) = state
        .pseudo_weathers
        .iter()
        .position(|pw| std::mem::discriminant(pw) == std::mem::discriminant(pseudo_weather))
    {
        state.pseudo_weathers.remove(pos);
        state.pseudo_weather_turns.remove(pos);
        emit(state, EventKind::PseudoWeatherEnd { effect: pseudo_weather.clone() });
    }
}

/// Add side condition for player, avoiding duplicates.
pub fn add_side_condition(
    state: &mut BattleState,
    player: Player,
    condition: SideCondition,
    duration: u8,
) {
    // Use a block scope so that the mutable borrows of the per-side vecs are
    // released before calling `emit`, which also needs `&mut state`.
    let added = {
        let (conditions, turns) = match player {
            Player::P1 => (
                &mut state.p1_side_conditions,
                &mut state.p1_side_condition_turns,
            ),
            Player::P2 => (
                &mut state.p2_side_conditions,
                &mut state.p2_side_condition_turns,
            ),
        };

        // Layered entry hazards (Spikes, Toxic Spikes): a repeat use adds a layer up to the cap
        // rather than failing as a duplicate.
        let layer_cap = match condition {
            SideCondition::Spikes(_) => Some(3u8),
            SideCondition::ToxicSpikes(_) => Some(2u8),
            _ => None,
        };
        if let Some(cap) = layer_cap {
            if let Some(existing) = conditions
                .iter_mut()
                .find(|sc| std::mem::discriminant(*sc) == std::mem::discriminant(&condition))
            {
                if let SideCondition::Spikes(n) | SideCondition::ToxicSpikes(n) = existing {
                    *n = (*n + 1).min(cap);
                }
                false // incremented existing layer — not a new entry
            } else {
                conditions.push(condition.clone());
                turns.push(duration);
                true // new entry
            }
        } else {
            // Single-layer conditions: reject duplicates by discriminant.
            if conditions
                .iter()
                .any(|sc| std::mem::discriminant(sc) == std::mem::discriminant(&condition))
            {
                false // duplicate — not added
            } else {
                conditions.push(condition.clone());
                turns.push(duration);
                true // new entry
            }
        }
    }; // per-side borrows dropped here

    if added {
        emit(state, EventKind::SideConditionStart { side: player, condition });
    }
}

/// Remove side condition by discriminant.
pub fn remove_side_condition(state: &mut BattleState, player: Player, condition: &SideCondition) {
    let removed = match player {
        Player::P1 => {
            if let Some(pos) = state
                .p1_side_conditions
                .iter()
                .position(|sc| std::mem::discriminant(sc) == std::mem::discriminant(condition))
            {
                state.p1_side_conditions.remove(pos);
                state.p1_side_condition_turns.remove(pos);
                true
            } else {
                false
            }
        }
        Player::P2 => {
            if let Some(pos) = state
                .p2_side_conditions
                .iter()
                .position(|sc| std::mem::discriminant(sc) == std::mem::discriminant(condition))
            {
                state.p2_side_conditions.remove(pos);
                state.p2_side_condition_turns.remove(pos);
                true
            } else {
                false
            }
        }
    };
    if removed {
        emit(state, EventKind::SideConditionEnd { side: player, condition: condition.clone() });
    }
}

fn prune_timed_effects<T: Clone>(effects: &mut Vec<T>, turns: &mut Vec<u8>) {
    let mut kept_effects = Vec::with_capacity(effects.len());
    let mut kept_turns = Vec::with_capacity(turns.len());

    for (effect, turn_count) in effects.drain(..).zip(turns.drain(..)) {
        if turn_count == 0 {
            kept_effects.push(effect);
            kept_turns.push(0);
        } else if turn_count > 1 {
            kept_effects.push(effect);
            kept_turns.push(turn_count - 1);
        }
    }

    *effects = kept_effects;
    *turns = kept_turns;
}

/// Fire end-of-turn effects for volatiles that are about to expire this turn (turns == 1)
/// or that deal recurring effects (SyrupBomb speed drop). Must be called BEFORE
/// `decrement_volatile_statuses` so the volatiles are still present when we check them.
fn apply_volatile_eot_effects(state: &mut BattleState) {
    let abilities_suppressed = abilities_are_suppressed(state);
    let items_suppressed = items_are_suppressed(state);
    let sun_blocks_freeze = weather_is_sunlight(state);

    // ── SyrupBomb: −1 Speed stage each turn the volatile is active ───────────────────────
    // Collect slots first to avoid borrow conflicts.
    let syrup_slots: Vec<(Player, usize)> = state.p1_active_mons.iter().enumerate()
        .filter_map(|(i, m)| {
            if !m.fainted && m.volatiles.iter().any(|v|
                matches!(v, VolatileStatusState::TurnStatus(VolatileStatus::SyrupBomb, n) if *n > 0))
            { Some((Player::P1, i)) } else { None }
        })
        .chain(state.p2_active_mons.iter().enumerate().filter_map(|(i, m)| {
            if !m.fainted && m.volatiles.iter().any(|v|
                matches!(v, VolatileStatusState::TurnStatus(VolatileStatus::SyrupBomb, n) if *n > 0))
            { Some((Player::P2, i)) } else { None }
        }))
        .collect();

    let mut syrup_bomb_deltas: Vec<(Player, usize, [i8; 7])> = Vec::new();
    for (player, idx) in syrup_slots {
        let mons = match player {
            Player::P1 => &mut state.p1_active_mons,
            Player::P2 => &mut state.p2_active_mons,
        };
        let delta = if let Some(mon) = mons.get_mut(idx) {
            apply_stat_boosts_to_pokemon(mon, &[0, 0, 0, 0, -1, 0, 0], items_suppressed, false)
        } else { [0; 7] };
        syrup_bomb_deltas.push((player, idx, delta));
    }
    // Emit BoostChanged for SyrupBomb speed drops (mons borrows have ended).
    for (player, idx, delta) in syrup_bomb_deltas {
        let slot = FieldSlot { player, slot_index: idx as u8 };
        for (boost_idx, &stages) in delta.iter().enumerate() {
            if stages != 0 {
                emit(state, EventKind::BoostChanged { target: slot, boost_idx, stages });
            }
        }
    }

    // ── Yawn: apply sleep when the volatile expires (turns == 1 → about to be removed) ───
    let yawn_slots: Vec<(Player, usize)> = state
        .p1_active_mons
        .iter()
        .enumerate()
        .filter_map(|(i, m)| {
            if !m.fainted
                && m.volatiles
                    .iter()
                    .any(|v| matches!(v, VolatileStatusState::TurnStatus(VolatileStatus::Yawn, 1)))
            {
                Some((Player::P1, i))
            } else {
                None
            }
        })
        .chain(
            state
                .p2_active_mons
                .iter()
                .enumerate()
                .filter_map(|(i, m)| {
                    if !m.fainted
                        && m.volatiles.iter().any(|v| {
                            matches!(v, VolatileStatusState::TurnStatus(VolatileStatus::Yawn, 1))
                        })
                    {
                        Some((Player::P2, i))
                    } else {
                        None
                    }
                }),
        )
        .collect();

    if !yawn_slots.is_empty() {
        let state_snapshot = state.clone();
        // Collect (slot) where Yawn actually applied sleep, to emit StatusInflicted after loop.
        let mut yawn_inflicted: Vec<FieldSlot> = Vec::new();
        for (player, idx) in yawn_slots {
            let mons = match player {
                Player::P1 => &mut state.p1_active_mons,
                Player::P2 => &mut state.p2_active_mons,
            };
            if let Some(mon) = mons.get_mut(idx) {
                let applied = apply_status_to_pokemon(
                    &state_snapshot,
                    sun_blocks_freeze,
                    false,
                    mon,
                    &crate::state::dex_data::Status::Sleep(0),
                );
                if applied {
                    yawn_inflicted.push(FieldSlot { player, slot_index: idx as u8 });
                }
            }
        }
        // Emit StatusInflicted after the mutable loop borrows are released.
        for slot in yawn_inflicted {
            emit(state, EventKind::StatusInflicted { target: slot, status: Status::Sleep(0) });
        }
    }

    // ── PerishSong: faint when counter reaches 1 (about to be removed) ───────────────────
    let perish_slots: Vec<(Player, usize)> = state
        .p1_active_mons
        .iter()
        .enumerate()
        .filter_map(|(i, m)| {
            if !m.fainted
                && m.volatiles.iter().any(|v| {
                    matches!(
                        v,
                        VolatileStatusState::TurnStatus(VolatileStatus::PerishSong, 1)
                    )
                })
            {
                Some((Player::P1, i))
            } else {
                None
            }
        })
        .chain(
            state
                .p2_active_mons
                .iter()
                .enumerate()
                .filter_map(|(i, m)| {
                    if !m.fainted
                        && m.volatiles.iter().any(|v| {
                            matches!(
                                v,
                                VolatileStatusState::TurnStatus(VolatileStatus::PerishSong, 1)
                            )
                        })
                    {
                        Some((Player::P2, i))
                    } else {
                        None
                    }
                }),
        )
        .collect();

    for (player, idx) in perish_slots {
        let mons = match player {
            Player::P1 => &mut state.p1_active_mons,
            Player::P2 => &mut state.p2_active_mons,
        };
        if let Some(mon) = mons.get_mut(idx) {
            if !abilities_suppressed && mon.ability == Ability::MagicGuard {
                continue;
            }
            mon.hp = 0;
            mon.fainted = true;
            clear_pokemon_on_faint(mon);
        }
    }
}

/// Decrement all TurnStatus volatile counters for `mons`. Volatiles whose counter
/// reaches 0 are dropped (expired). Returns `(mon_index, VolatileStatus)` pairs for
/// each expired volatile so the caller can emit `VolatileEnd` events.
fn decrement_volatile_statuses(mons: &mut [PokemonState]) -> Vec<(usize, VolatileStatus)> {
    let mut expired: Vec<(usize, VolatileStatus)> = Vec::new();
    for (mon_idx, mon) in mons.iter_mut().enumerate() {
        let mut kept = Vec::with_capacity(mon.volatiles.len());

        for volatile in mon.volatiles.drain(..) {
            match volatile {
                VolatileStatusState::TurnStatus(effect, turns) => {
                    if turns == 0 {
                        kept.push(VolatileStatusState::TurnStatus(effect, 0)); // permanent
                    } else if turns > 1 {
                        kept.push(VolatileStatusState::TurnStatus(effect, turns - 1));
                    } else {
                        // turns == 1 → expires this tick
                        expired.push((mon_idx, effect));
                    }
                }
                other_volatile => kept.push(other_volatile),
            }
        }

        mon.volatiles = kept;
    }
    expired
}

pub fn decrement_move_statuses(mon: &mut PokemonState) {
    let mut kept = Vec::with_capacity(mon.volatiles.len());

    for volatile in mon.volatiles.drain(..) {
        match volatile {
            VolatileStatusState::MoveStatus(effect, turns) => {
                // LockedMove counter is count-up (attacks completed so far) and is
                // managed exclusively by the rampage end-timing fork in
                // apply_post_damage_move_effects. Do not decrement it here.
                if matches!(effect, crate::state::dex_data::VolatileStatus::LockedMove(_)) {
                    kept.push(VolatileStatusState::MoveStatus(effect, turns));
                } else if turns == 0 {
                    kept.push(VolatileStatusState::MoveStatus(effect, 0));
                } else if turns > 1 {
                    kept.push(VolatileStatusState::MoveStatus(effect, turns - 1));
                }
                // turns == 1: drop (volatile expires)
            }
            other_volatile => kept.push(other_volatile),
        }
    }

    mon.volatiles = kept;
}

/// Decrement effect timers at end of turn.
/// Call this before setting turn_ended = true.
pub fn decrement_effect_timers(state: &mut BattleState) {
    if let Some(turns) = state.weather_turns.as_mut() {
        if *turns > 1 {
            *turns -= 1;
        } else if *turns == 1 {
            state.weather = None;
            state.weather_turns = None;
            emit(state, EventKind::WeatherChanged { weather: None });
        }
    }

    if let Some(turns) = state.terrain_turns.as_mut() {
        if *turns > 1 {
            *turns -= 1;
        } else if *turns == 1 {
            state.terrain = None;
            state.terrain_turns = None;
            emit(state, EventKind::TerrainChanged { terrain: None });
        }
    }

    // Detect Magic Room (MagicDeluge) expiry: items go from suppressed to re-enabled.
    let was_items_suppressed = items_are_suppressed(state);
    prune_timed_effects(&mut state.pseudo_weathers, &mut state.pseudo_weather_turns);
    if was_items_suppressed && !items_are_suppressed(state) {
        // Items are now re-enabled — trigger immediate on-enable effects (e.g. status-cure berries).
        // ability_active is unknown here (no per-slot state); use simple env so
        // Cheek Pouch / Cud Chew don't fire on Magic Room expiry (corner case).
        let env = BerryEnv::simple(false);
        // Collect cures from each side before borrowing state for emit (can't hold
        // iter_mut and &mut state simultaneously).
        let mut item_enable_cures: Vec<(FieldSlot, BerryCure)> = Vec::new();
        for (idx, mon) in state.p1_active_mons.iter_mut().enumerate() {
            if !klutz_disables_item(mon) {
                item_enable_cures.push((
                    FieldSlot { player: Player::P1, slot_index: idx as u8 },
                    on_item_obtained_or_enabled(mon, &env),
                ));
            }
        }
        for (idx, mon) in state.p2_active_mons.iter_mut().enumerate() {
            if !klutz_disables_item(mon) {
                item_enable_cures.push((
                    FieldSlot { player: Player::P2, slot_index: idx as u8 },
                    on_item_obtained_or_enabled(mon, &env),
                ));
            }
        }
        for (slot, cure) in &item_enable_cures {
            emit_berry_cure(state, *slot, cure);
        }
    }

    prune_timed_effects(
        &mut state.p1_side_conditions,
        &mut state.p1_side_condition_turns,
    );
    prune_timed_effects(
        &mut state.p2_side_conditions,
        &mut state.p2_side_condition_turns,
    );

    // Before decrementing, fire effects for volatiles that expire this turn.
    apply_volatile_eot_effects(state);

    // Emit PerishCount for every active mon with PerishSong before the counter ticks down.
    // turns_left = counter - 1 (what the player will see after this tick); 0 if already fainting.
    {
        let perish_counts: Vec<(FieldSlot, u8)> = state.p1_active_mons.iter().enumerate()
            .filter_map(|(i, m)| {
                m.volatiles.iter().find_map(|v| {
                    if let VolatileStatusState::TurnStatus(VolatileStatus::PerishSong, n) = v {
                        Some((FieldSlot { player: Player::P1, slot_index: i as u8 }, n.saturating_sub(1) as u8))
                    } else { None }
                })
            })
            .chain(state.p2_active_mons.iter().enumerate().filter_map(|(i, m)| {
                m.volatiles.iter().find_map(|v| {
                    if let VolatileStatusState::TurnStatus(VolatileStatus::PerishSong, n) = v {
                        Some((FieldSlot { player: Player::P2, slot_index: i as u8 }, n.saturating_sub(1) as u8))
                    } else { None }
                })
            }))
            .collect();
        for (slot, turns_left) in perish_counts {
            emit(state, crate::information::information::EventKind::PerishCount { target: slot, turns_left });
        }
    }

    // Volatile status duration 0 means permanent, so preserve it. (Back mons cannot have volatiles)
    let p1_expired = decrement_volatile_statuses(&mut state.p1_active_mons);
    //decrement_volatile_statuses(&mut state.p1_back_mons);
    let p2_expired = decrement_volatile_statuses(&mut state.p2_active_mons);
    //decrement_volatile_statuses(&mut state.p2_back_mons);

    // Emit VolatileEnd for each expired TurnStatus volatile (borrows of active_mons have ended).
    for (slot_idx, vs) in p1_expired {
        emit(state, EventKind::VolatileEnd { target: FieldSlot { player: Player::P1, slot_index: slot_idx as u8 }, volatile: vs });
    }
    for (slot_idx, vs) in p2_expired {
        emit(state, EventKind::VolatileEnd { target: FieldSlot { player: Player::P2, slot_index: slot_idx as u8 }, volatile: vs });
    }

    // timers decremented; other end-of-turn effects handled by `end_turn`
}

/// Perform full end-of-turn processing. Returns all possible outcomes with their probabilities,
/// branching wherever a probabilistic ability (Shed Skin, Healer, Moody, Harvest) fires.
///
/// Pipeline:
///   1. `apply_pre_status_residuals` — weather/terrain/item healing + Future Sight/Doom Desire
///   2. `apply_status_cure_abilities` — Hydration/Shed Skin/Healer (may branch)
///   3. `apply_status_damage` — burn/poison/toxic (reads cured status, deterministic per branch)
///   4. `apply_late_eot_abilities` — Speed Boost/Moody/Harvest/Hunger Switch (may branch)
///   5. Clear `entered_this_turn` so Speed Boost fires normally next turn.
pub fn end_turn(
    state: &mut BattleState,
    move_dex: &HashMap<PokemonMove, MoveData>,
    config: crate::simulator::DamageConfig,
) -> Vec<(BattleState, f64)> {
    // Item-loss ledger snapshot: residual damage in Phases 1–3 can consume berries
    // (e.g. Sitrus after burn chip); the diff after Phase 3 fires Unburden / Pickup /
    // Symbiosis reactions before Pickup resolves in Phase 4.
    let item_snapshot = snapshot_active_items(state);

    // Decrement effect timers (weather, pseudo-weather, side conditions).
    decrement_effect_timers(state);

    // Weather may have just expired — re-evaluate Castform's Forecast form.
    update_forecast_forms(state);

    // Advance the battle turn counter.
    state.turn_number = state.turn_number.saturating_add(1);

    // Phase 1: weather/terrain/item healing (deterministic, &mut in-place).
    // Also ticks Future Sight / Doom Desire; fires that have resolved are returned
    // so their branched damage can be applied before Phase 2.
    apply_pre_status_residuals(state);
    let fired_future_moves = extract_fired_future_moves(state);

    // Phase 1.5: apply branched Future Sight / Doom Desire damage on all branches.
    let mut branches = apply_future_move_damage(
        vec![(state.clone(), 1.0)],
        &fired_future_moves,
        move_dex,
        config,
    );

    // Phase 2: probabilistic status-cure abilities. Branches if Shed Skin or Healer fires.
    branches = apply_status_cure_abilities(branches);

    // Phase 3: burn/poison/toxic damage (deterministic per branch).
    for (bs, _) in branches.iter_mut() {
        apply_status_damage(bs);
        process_item_loss_events(bs, &item_snapshot);
    }

    // Phase 4: late ability effects (Speed Boost, Moody, Harvest, Hunger Switch).
    branches = apply_late_eot_abilities(branches);

    // Phase 5: clear the entry-turn flag so Speed Boost fires normally next turn,
    // the per-turn event flags (Assurance / Avalanche / Lash Out / Burning Jealousy),
    // and empty the per-turn consumed-item pool (Pickup has already run in Phase 4).
    for (bs, _) in branches.iter_mut() {
        // Collect (slot, which volatile ended) for emission after the loop borrows end.
        let mut roost_ended: Vec<FieldSlot> = Vec::new();
        let mut electrify_ended: Vec<FieldSlot> = Vec::new();
        let mut encore_ended: Vec<FieldSlot> = Vec::new();
        let n_p1 = bs.p1_active_mons.len();
        let n_p2 = bs.p2_active_mons.len();
        for (player, count) in [(Player::P1, n_p1), (Player::P2, n_p2)] {
            for i in 0..count {
                let slot = FieldSlot { player, slot_index: i as u8 };
                let mon = match player {
                    Player::P1 => &mut bs.p1_active_mons[i],
                    Player::P2 => &mut bs.p2_active_mons[i],
                };
                mon.entered_this_turn = false;
                // U-turn/Volt Switch mid-turn entries set first_turn_on_field_pending so EOT skips
                // clearing first_move_on_field once, preserving it to their actual first turn.
                if mon.first_turn_on_field_pending {
                    mon.first_turn_on_field_pending = false;
                } else {
                    mon.first_move_on_field = false;
                }
                mon.damaged_this_turn = false;
                mon.damaged_by_this_turn.clear();
                mon.last_physical_damage_taken = 0;
                mon.last_physical_attacker = None;
                mon.last_special_damage_taken = 0;
                mon.last_special_attacker = None;
                mon.last_damage_taken = 0;
                mon.last_damage_attacker = None;
                mon.stats_raised_this_turn = false;
                mon.stats_lowered_this_turn = false;
                mon.switched_in_this_turn = false;
                // Roost's Flying-type suppression only lasts the turn it is used.
                if remove_status_volatile(mon, &VolatileStatus::Roost) {
                    roost_ended.push(slot);
                }
                // Electrify only redirects the affected Pokémon's move for the current turn.
                if remove_status_volatile(mon, &VolatileStatus::Electrify) {
                    electrify_ended.push(slot);
                }
                // Encore ends immediately once its move runs out of PP.
                let encore_move_out_of_pp = mon
                    .volatiles
                    .iter()
                    .find_map(|v| match v {
                        VolatileStatusState::MoveStatus(VolatileStatus::Encore(m), _) => {
                            Some(m.clone())
                        }
                        _ => None,
                    })
                    .is_some_and(|m| {
                        !mon.moves
                            .iter()
                            .zip(mon.move_pp.iter())
                            .any(|(slot, pp)| slot.as_ref() == Some(&m) && *pp > 0)
                    });
                if encore_move_out_of_pp {
                    remove_status_volatile(mon, &VolatileStatus::Encore(PokemonMove::Struggle));
                    encore_ended.push(slot);
                }
            }
        }
        // Emit VolatileEnd for turn-scoped volatiles that expired this EOT.
        for slot in roost_ended {
            emit(bs, EventKind::VolatileEnd { target: slot, volatile: VolatileStatus::Roost });
        }
        for slot in electrify_ended {
            emit(bs, EventKind::VolatileEnd { target: slot, volatile: VolatileStatus::Electrify });
        }
        for slot in encore_ended {
            emit(bs, EventKind::VolatileEnd { target: slot, volatile: VolatileStatus::Encore(PokemonMove::Struggle) });
        }
        bs.items_consumed_this_turn.clear();
        bs.round_used_this_turn = false;
    }

    coalesce_branches(branches)
}

/// Determine the duration for a volatile status condition.
fn get_volatile_duration(volatile: &VolatileStatus) -> u16 {
    match volatile {
        // End-of-turn only (lasts 1 turn)
        VolatileStatus::Flinch
        | VolatileStatus::Protect
        | VolatileStatus::KingsShield
        | VolatileStatus::SpikyShield
        | VolatileStatus::BanefulBunker
        | VolatileStatus::Endure
        | VolatileStatus::MaxGuard
        | VolatileStatus::HelpingHand
        | VolatileStatus::FollowMe
        | VolatileStatus::RagePowder => 1,
        // MustRecharge should last for 2 turns (expires after 1 end-of-turn decrement)
        VolatileStatus::MustRecharge => 2,
        // Yawn: apply sleep at end of next turn (decrement 2→1→removed+sleep)
        VolatileStatus::Yawn => 2,
        // HealBlock: Psychic Noise applies 2-turn block (Heal Block move is Past/nonstandard)
        VolatileStatus::HealBlock => 2,
        // SyrupBomb: 3 speed drops over 3 turns (decrement 3→2→1→removed)
        VolatileStatus::SyrupBomb => 3,
        // Uproar: lock user into 3 turns (decrement 3→2→1→removed)
        VolatileStatus::Uproar => 3,
        // PerishSong: faint after 3 more turns (apply with 4, decrement 4→3→2→1→removed+faint)
        VolatileStatus::PerishSong => 4,
        // Default: permanent until explicitly removed
        _ => 0,
    }
}

/// Determine the duration for a side condition.
fn get_side_condition_duration(condition: &SideCondition) -> u8 {
    match condition {
        // Last only until end of turn (turn-of-use protect variants)
        SideCondition::CraftyShield
        | SideCondition::MatBlock
        | SideCondition::QuickGuard
        | SideCondition::WideGuard => 1,
        // Entry hazards last indefinitely (until removed by spin/Defog/Tidy Up/Court Change).
        SideCondition::Spikes(_)
        | SideCondition::StealthRock
        | SideCondition::StickyWeb(_)
        | SideCondition::ToxicSpikes(_) => 0,
        // Default duration (5 turns): Reflect, Light Screen, Aurora Veil, Safeguard, Mist, etc.
        _ => 5,
    }
}

/// Determine the initial duration for a pseudo-weather effect.
fn get_pseudo_weather_duration(pseudo_weather: &PseudoWeather) -> u8 {
    match pseudo_weather {
        // Fairy Lock lasts 2 turns: the use-turn and the immediately following turn.
        PseudoWeather::FairyLock => 2,
        // All others (Trick Room, Magic Room, Wonder Room, Gravity, …) default to 5 turns.
        _ => 5,
    }
}

/// Apply a status condition to a pokemon (only if it doesn't already have one).
/// `mold_break` is true when the source is a Mold Breaker / Turboblaze / Teravolt attacker,
/// which suppresses ignorable abilities (SweetVeil, etc.) on the target.
/// Returns `true` if the status was successfully applied (so callers can emit `StatusInflicted`).
fn apply_status_to_pokemon(
    state: &BattleState,
    sun_blocks_freeze: bool,
    mold_break: bool,
    mon: &mut PokemonState,
    status: &crate::state::dex_data::Status,
) -> bool {
    // Prevent statuses if ability blocks all non-volatile statuses
    if mon.ability == Ability::Comatose || mon.ability == Ability::PurifyingSalt {
        return false;
    }

    if mon.ability == Ability::LeafGuard && sun_blocks_freeze {
        return false;
    }

    // Sweet Veil: the holder cannot fall asleep (including self-induced sleep from Rest).
    // Ally protection (Sweet Veil protecting teammates) is handled at the apply_effect_to_target
    // call site where side context is available.
    if matches!(status, Status::Sleep(_))
        && !mold_break
        && !pokemon_ability_is_suppressed(state, mon)
        && mon.ability == Ability::SweetVeil
    {
        return false;
    }

    if matches!(status, Status::Frozen(_)) && sun_blocks_freeze {
        return false;
    }

    if matches!(status, Status::Sleep(_))
        && pokemon_is_on_terrain(state, mon, &Terrain::ElectricTerrain)
    {
        return false;
    }

    // Uproar: while any Pokémon on the field is making an uproar, no Pokémon can fall asleep.
    if matches!(status, Status::Sleep(_)) {
        let uproar_active = state
            .p1_active_mons
            .iter()
            .chain(state.p2_active_mons.iter())
            .any(|m| !m.fainted && has_status_volatile(m, &VolatileStatus::Uproar));
        if uproar_active {
            return false;
        }
    }

    if matches!(
        status,
        Status::Burn
            | Status::Poison
            | Status::ToxicPoison(_)
            | Status::Paralysis
            | Status::Sleep(_)
            | Status::Frozen(_)
    ) && pokemon_is_on_terrain(state, mon, &Terrain::MistyTerrain)
    {
        return false;
    }

    if mon.status.is_some() {
        return false;
    }

    match status {
        Status::Burn => {
            // Fire types and certain abilities prevent burn
            if pokemon_has_type(mon, &PokemonType::Fire) {
                return false;
            }
            if mon.ability == Ability::WaterBubble
                || mon.ability == Ability::WaterVeil
                || mon.ability == Ability::ThermalExchange
            {
                return false;
            }
            mon.status = Some(Status::Burn);
        }
        Status::Poison => {
            // Poison/Steel types are immune unless attacker has Corrosion
            if pokemon_has_type(mon, &PokemonType::Poison)
                || pokemon_has_type(mon, &PokemonType::Steel)
            {
                return false;
            }
            if mon.ability == Ability::Immunity {
                return false;
            }
            mon.status = Some(Status::Poison);
        }
        Status::ToxicPoison(_) => {
            if pokemon_has_type(mon, &PokemonType::Poison)
                || pokemon_has_type(mon, &PokemonType::Steel)
            {
                return false;
            }
            if mon.ability == Ability::Immunity {
                return false;
            }
            mon.status = Some(Status::ToxicPoison(0));
        }
        Status::Paralysis => {
            if mon.ability == Ability::Limber || pokemon_has_type(mon, &PokemonType::Electric) {
                return false;
            }
            mon.status = Some(Status::Paralysis);
        }
        Status::Sleep(_) => {
            if mon.ability == Ability::Insomnia || mon.ability == Ability::VitalSpirit {
                return false;
            }
            mon.status = Some(Status::Sleep(0));
        }
        Status::Frozen(_) => {
            if pokemon_has_type(mon, &PokemonType::Ice) {
                return false;
            }
            if mon.ability == Ability::MagmaArmor || mon.ability == Ability::IceFace {
                return false;
            }
            mon.status = Some(Status::Frozen(0));
        }
    }
    true
}

/// Heal `mon` by `amount` HP, clamped to max. Clears fainted flag.
fn heal_mon(mon: &mut PokemonState, amount: u16) {
    let max_hp = mon.stats[0].max(1);
    mon.hp = mon.hp.saturating_add(amount).min(max_hp);
    mon.fainted = false;
}

/// Deal `amount` residual damage, clearing on faint. Returns true if the mon fainted.
fn deal_residual_damage(
    mon: &mut PokemonState,
    amount: u16,
    env: BerryEnv,
    abilities_suppressed: bool,
) -> bool {
    if amount == 0 {
        return false;
    }
    take_damage(mon, amount, env, abilities_suppressed);
    if mon.fainted {
        clear_pokemon_on_faint(mon);
        true
    } else {
        false
    }
}

struct WeatherResidualCtx {
    rain: bool,
    snow: bool,
    sun: bool,
    sandstorm: bool,
    abilities_suppressed: bool,
    items_suppressed: bool,
}

fn apply_weather_residual(mon: &mut PokemonState, ctx: &WeatherResidualCtx, env: BerryEnv) {
    if mon.fainted {
        return;
    }
    let max_hp = mon.stats[0].max(1);

    if ctx.rain && !ctx.abilities_suppressed {
        if mon.ability == Ability::RainDish {
            gain_hp(mon, (max_hp as u32 / 16) as u16, env);
        }
        if mon.ability == Ability::DrySkin {
            gain_hp(mon, (max_hp as u32 / 8) as u16, env);
        }
    }

    if ctx.snow && !ctx.abilities_suppressed && mon.ability == Ability::IceBody {
        gain_hp(mon, (max_hp as u32 / 16) as u16, env);
    }

    if ctx.sun && !ctx.abilities_suppressed {
        if mon.ability == Ability::DrySkin
            && deal_residual_damage(
                mon,
                (max_hp as u32 / 8) as u16,
                env,
                ctx.abilities_suppressed,
            )
        {
            return;
        }
        if mon.ability == Ability::SolarPower
            && deal_residual_damage(
                mon,
                (max_hp as u32 / 8) as u16,
                env,
                ctx.abilities_suppressed,
            )
        {
            return;
        }
    }

    if !ctx.sandstorm {
        return;
    }

    let sandstorm_immune = pokemon_has_type(mon, &PokemonType::Steel)
        || pokemon_has_type(mon, &PokemonType::Rock)
        || pokemon_has_type(mon, &PokemonType::Ground)
        || (!ctx.abilities_suppressed
            && matches!(
                mon.ability,
                Ability::SandForce
                    | Ability::SandRush
                    | Ability::SandVeil
                    | Ability::MagicGuard
                    | Ability::Overcoat
            ))
        || (!ctx.items_suppressed
            && !klutz_disables_item(mon)
            && matches!(mon.item, Item::SafetyGoggles));

    if !sandstorm_immune {
        deal_residual_damage(
            mon,
            (mon.stats[0] as u32 / 16) as u16,
            env,
            ctx.abilities_suppressed,
        );
    }
}

fn apply_status_residual(mon: &mut PokemonState, abilities_suppressed: bool, env: BerryEnv) {
    if mon.fainted {
        return;
    }

    // Hydration is now handled in apply_status_cure_abilities (before damage), not here.

    let magic_guard = !abilities_suppressed && mon.ability == Ability::MagicGuard;

    match mon.status {
        Some(Status::Burn) => {
            if !magic_guard {
                // Heatproof halves burn residual damage (from 1/16 to 1/32 max HP).
                let divisor = if !abilities_suppressed && mon.ability == Ability::Heatproof {
                    32
                } else {
                    16
                };
                deal_residual_damage(
                    mon,
                    (mon.stats[0] as u32 / divisor) as u16,
                    env,
                    abilities_suppressed,
                );
            }
        }
        Some(Status::Poison) => {
            if !magic_guard {
                deal_residual_damage(
                    mon,
                    (mon.stats[0] as u32 / 8) as u16,
                    env,
                    abilities_suppressed,
                );
            }
        }
        Some(Status::ToxicPoison(n)) => {
            let new_n = n.saturating_add(1);
            mon.status = Some(Status::ToxicPoison(new_n));
            if !magic_guard {
                deal_residual_damage(
                    mon,
                    (mon.stats[0] as u32 * new_n as u32 / 16) as u16,
                    env,
                    abilities_suppressed,
                );
            }
        }
        _ => {}
    }
}

/// Phase 1 of end-of-turn processing: deterministic weather/terrain/item healing.
/// This runs before any status-cure abilities and before burn/poison damage.
fn apply_pre_status_residuals(state: &mut BattleState) {
    // Wish resolves at the very start of the end-of-turn residual phase (before weather).
    resolve_wish_slot_conditions(state);

    let ctx = WeatherResidualCtx {
        rain: weather_is_rain(state),
        snow: weather_is_snow(state),
        sun: weather_is_sunlight(state),
        sandstorm: weather_is_sandstorm(state),
        abilities_suppressed: abilities_are_suppressed(state),
        items_suppressed: items_are_suppressed(state),
    };

    // Pre-compute BerryEnv per slot (shared borrows) before any mutable iteration.
    let p1_envs: Vec<BerryEnv> = (0..state.p1_active_mons.len())
        .map(|i| {
            berry_env(
                state,
                FieldSlot {
                    player: Player::P1,
                    slot_index: i as u8,
                },
            )
        })
        .collect();
    let p2_envs: Vec<BerryEnv> = (0..state.p2_active_mons.len())
        .map(|i| {
            berry_env(
                state,
                FieldSlot {
                    player: Player::P2,
                    slot_index: i as u8,
                },
            )
        })
        .collect();

    for (i, mon) in state.p1_active_mons.iter_mut().enumerate() {
        apply_weather_residual(mon, &ctx, p1_envs[i]);
    }
    for (i, mon) in state.p2_active_mons.iter_mut().enumerate() {
        apply_weather_residual(mon, &ctx, p2_envs[i]);
    }

    // Batch-collect (slot, post-heal hp, max-hp) for Healed event emission after each
    // iter_mut loop (the mutable slice borrow prevents inline emit; emit_healed_batch fires
    // after the borrow is released).
    let mut healed: Vec<(FieldSlot, u16, u16)> = Vec::new();

    // Grassy Terrain healing
    let terrain_snapshot = state.clone();
    if matches!(
        current_terrain(&terrain_snapshot),
        Some(Terrain::GrassyTerrain)
    ) {
        for (i, mon) in state.p1_active_mons.iter_mut().enumerate() {
            if !mon.fainted && pokemon_is_grounded(&terrain_snapshot, mon) {
                let max_hp = mon.stats[0].max(1);
                let before = mon.hp;
                gain_hp(mon, (max_hp as u32 / 16) as u16, p1_envs[i]);
                if mon.hp != before {
                    healed.push((FieldSlot { player: Player::P1, slot_index: i as u8 }, mon.hp, max_hp));
                }
            }
        }
        for (i, mon) in state.p2_active_mons.iter_mut().enumerate() {
            if !mon.fainted && pokemon_is_grounded(&terrain_snapshot, mon) {
                let max_hp = mon.stats[0].max(1);
                let before = mon.hp;
                gain_hp(mon, (max_hp as u32 / 16) as u16, p2_envs[i]);
                if mon.hp != before {
                    healed.push((FieldSlot { player: Player::P2, slot_index: i as u8 }, mon.hp, max_hp));
                }
            }
        }
    }
    emit_healed_batch(state, &healed);
    healed.clear();

    // Leftovers: restore 1/16 max HP (rounded down, min 1) at end of turn.
    // Does not consume the item. Capped at max HP by gain_hp.
    // Blocked by Heal Block.
    for (i, mon) in state.p1_active_mons.iter_mut().enumerate() {
        if !mon.fainted
            && !ctx.items_suppressed
            && !klutz_disables_item(mon)
            && mon.item == Item::Leftovers
            && !heal_is_blocked(mon)
        {
            let max_hp = mon.stats[0].max(1);
            let before = mon.hp;
            gain_hp(mon, (max_hp as u32 / 16).max(1) as u16, p1_envs[i]);
            if mon.hp != before {
                healed.push((FieldSlot { player: Player::P1, slot_index: i as u8 }, mon.hp, max_hp));
            }
        }
    }
    for (i, mon) in state.p2_active_mons.iter_mut().enumerate() {
        if !mon.fainted
            && !ctx.items_suppressed
            && !klutz_disables_item(mon)
            && mon.item == Item::Leftovers
            && !heal_is_blocked(mon)
        {
            let max_hp = mon.stats[0].max(1);
            let before = mon.hp;
            gain_hp(mon, (max_hp as u32 / 16).max(1) as u16, p2_envs[i]);
            if mon.hp != before {
                healed.push((FieldSlot { player: Player::P2, slot_index: i as u8 }, mon.hp, max_hp));
            }
        }
    }
    emit_healed_batch(state, &healed);
    healed.clear();

    // Aqua Ring: restore 1/16 max HP (rounded down) at end of turn. Heal Block suppresses
    // the heal but not the volatile. Big Root boosts the amount by ≈1.3×.
    for (i, mon) in state.p1_active_mons.iter_mut().enumerate() {
        let before = mon.hp;
        apply_aqua_ring_residual(mon, ctx.items_suppressed, p1_envs[i]);
        if mon.hp != before {
            healed.push((FieldSlot { player: Player::P1, slot_index: i as u8 }, mon.hp, mon.stats[0]));
        }
    }
    for (i, mon) in state.p2_active_mons.iter_mut().enumerate() {
        let before = mon.hp;
        apply_aqua_ring_residual(mon, ctx.items_suppressed, p2_envs[i]);
        if mon.hp != before {
            healed.push((FieldSlot { player: Player::P2, slot_index: i as u8 }, mon.hp, mon.stats[0]));
        }
    }
    emit_healed_batch(state, &healed);
    healed.clear();

    // Ingrain: restore 1/16 max HP (rounded down) at end of turn, same rules as Aqua Ring.
    for (i, mon) in state.p1_active_mons.iter_mut().enumerate() {
        let before = mon.hp;
        apply_ingrain_residual(mon, ctx.items_suppressed, p1_envs[i]);
        if mon.hp != before {
            healed.push((FieldSlot { player: Player::P1, slot_index: i as u8 }, mon.hp, mon.stats[0]));
        }
    }
    for (i, mon) in state.p2_active_mons.iter_mut().enumerate() {
        let before = mon.hp;
        apply_ingrain_residual(mon, ctx.items_suppressed, p2_envs[i]);
        if mon.hp != before {
            healed.push((FieldSlot { player: Player::P2, slot_index: i as u8 }, mon.hp, mon.stats[0]));
        }
    }
    emit_healed_batch(state, &healed);
    healed.clear();

    // Leech Seed: drain 1/8 of the seeded mon's max HP to the Pokémon in the seeder's slot
    // (the opposing active mon in singles). Liquid Ooze reverses the heal into damage.
    apply_leech_seed_residual(state, &p1_envs, &p2_envs, ctx.items_suppressed);

    // Curse volatile: cursed Pokémon lose ¼ of their max HP each end of turn.
    // Magic Guard prevents the damage. The volatile persists until the holder switches out or
    // faints (it is Baton-Passable, so it can transfer, but the damage still applies to the
    // new holder). Ordered after Leech Seed, before binding chip.
    {
        let abilities_suppressed = ctx.abilities_suppressed;
        let cursed_p1: Vec<usize> = state
            .p1_active_mons
            .iter()
            .enumerate()
            .filter(|(_, m)| !m.fainted && has_status_volatile(m, &VolatileStatus::Curse))
            .map(|(i, _)| i)
            .collect();
        let cursed_p2: Vec<usize> = state
            .p2_active_mons
            .iter()
            .enumerate()
            .filter(|(_, m)| !m.fainted && has_status_volatile(m, &VolatileStatus::Curse))
            .map(|(i, _)| i)
            .collect();
        for idx in cursed_p1 {
            let env = p1_envs[idx];
            let mon = &mut state.p1_active_mons[idx];
            let magic_guard = !abilities_suppressed && mon.ability == Ability::MagicGuard;
            if !magic_guard {
                let dmg = (mon.stats[0].max(1) / 4).max(1);
                take_damage(mon, dmg, env, abilities_suppressed);
            }
        }
        for idx in cursed_p2 {
            let env = p2_envs[idx];
            let mon = &mut state.p2_active_mons[idx];
            let magic_guard = !abilities_suppressed && mon.ability == Ability::MagicGuard;
            if !magic_guard {
                let dmg = (mon.stats[0].max(1) / 4).max(1);
                take_damage(mon, dmg, env, abilities_suppressed);
            }
        }
    }

    // Binding chip damage (PartiallyTrapped residual).
    // Canonical order: same phase as Leech Seed, after Leftovers but before burn/poison.
    // Ghost-types are NOT exempt — they still take chip; they are only exempt from the
    // switch-prevention (enforced separately in `is_trapped`).
    apply_binding_chip_damage(
        state,
        &p1_envs,
        &p2_envs,
        ctx.abilities_suppressed,
        ctx.items_suppressed,
    );

    // Salt Cure chip damage: 1/8 HP (1/4 for Water/Steel). Magic Guard prevents.
    apply_salt_cure_damage(state, &p1_envs, &p2_envs, ctx.abilities_suppressed);
}

/// Tick pending Wishes on every slot. A Wish set this turn carries `turns_remaining == 2`;
/// it decrements each end of turn and, on reaching 0 (the end of the turn after it was used),
/// heals the slot's current occupant by the stored amount (½ the wisher's max HP). The heal is
/// skipped if the occupant is fainted, already at full HP, or under Heal Block. Big Root does
/// not affect Wish.
fn resolve_wish_slot_conditions(state: &mut BattleState) {
    // Collect resolved Wishes: (player, slot_idx, pre_hp, post_hp, max_hp).
    // SlotConditionEnd always emits; Healed only emits when HP actually changed.
    let mut resolved: Vec<(Player, usize, u16, u16, u16)> = Vec::new();

    for player in [Player::P1, Player::P2] {
        let (conds, mons) = match player {
            Player::P1 => (&mut state.p1_slot_conditions, &mut state.p1_active_mons),
            Player::P2 => (&mut state.p2_slot_conditions, &mut state.p2_active_mons),
        };
        for (slot_idx, slot_conds) in conds.iter_mut().enumerate() {
            let mut resolved_heal: Option<u16> = None;
            slot_conds.retain_mut(|sc| {
                if let crate::state::dex_data::SlotCondition::Wish {
                    heal,
                    turns_remaining,
                } = sc
                {
                    *turns_remaining = turns_remaining.saturating_sub(1);
                    if *turns_remaining == 0 {
                        resolved_heal = Some(*heal);
                        return false; // remove the resolved Wish
                    }
                }
                true
            });
            if let Some(heal) = resolved_heal {
                // Record that this Wish condition ended at (player, slot_idx).
                // Also apply the heal and capture pre/post HP for the Healed event.
                let (pre_hp, post_hp, max_hp) = if let Some(mon) = mons.get_mut(slot_idx) {
                    let pre = mon.hp;
                    let max = mon.stats[0];
                    if !mon.fainted && !heal_is_blocked(mon) {
                        gain_hp(mon, heal, BerryEnv::simple(false));
                    }
                    (pre, mon.hp, max)
                } else {
                    (0, 0, 1)
                };
                resolved.push((player, slot_idx, pre_hp, post_hp, max_hp));
            }
        }
    }

    // Emit SlotConditionEnd with Healed nested as its reaction (iter_mut borrows ended).
    for (player, slot_idx, pre_hp, post_hp, max_hp) in resolved {
        let slot = FieldSlot { player, slot_index: slot_idx as u8 };
        with_reactions(
            state,
            EventKind::SlotConditionEnd {
                slot,
                condition: crate::state::dex_data::SlotCondition::Wish { heal: 0, turns_remaining: 0 },
            },
            |bs| {
                if post_hp != pre_hp {
                    if let Some(observer) = bs.event_observer {
                        let new_hp = if player == observer {
                            PokemonHP::Number(post_hp)
                        } else {
                            PokemonHP::Percent(hp_to_percent(post_hp, max_hp))
                        };
                        emit(bs, EventKind::Healed { target: slot, new_hp });
                    }
                }
            },
        );
    }
}

/// Snapshot struct holding the resolved data for a fired Future Sight / Doom Desire.
struct FiredFutureMove {
    move_name: PokemonMove,
    /// Player whose slot the move is targeting (the DEFENDER's side).
    target_player: Player,
    target_slot_index: usize,
    snapshot_raw_spa: u16,
    snapshot_spa_boost: i8,
    snapshot_level: u8,
    snapshot_type1: Option<crate::state::dex_data::PokemonType>,
    snapshot_type2: Option<crate::state::dex_data::PokemonType>,
    snapshot_ability: Ability,
    snapshot_item: Item,
}

/// Tick all FutureMove slot conditions. Returns those that have just fired (turns_remaining → 0),
/// removing them from the state's slot conditions. Non-firing conditions are decremented in place.
fn extract_fired_future_moves(state: &mut BattleState) -> Vec<FiredFutureMove> {
    let mut fired = Vec::new();
    for player in [Player::P1, Player::P2] {
        let conds = match player {
            Player::P1 => &mut state.p1_slot_conditions,
            Player::P2 => &mut state.p2_slot_conditions,
        };
        for (slot_idx, slot_conds) in conds.iter_mut().enumerate() {
            slot_conds.retain_mut(|sc| {
                if let crate::state::dex_data::SlotCondition::FutureMove {
                    move_name,
                    turns_remaining,
                    snapshot_raw_spa,
                    snapshot_spa_boost,
                    snapshot_level,
                    snapshot_type1,
                    snapshot_type2,
                    snapshot_ability,
                    snapshot_item,
                    ..
                } = sc
                {
                    *turns_remaining = turns_remaining.saturating_sub(1);
                    if *turns_remaining == 0 {
                        fired.push(FiredFutureMove {
                            move_name: move_name.clone(),
                            target_player: player,
                            target_slot_index: slot_idx,
                            snapshot_raw_spa: *snapshot_raw_spa,
                            snapshot_spa_boost: *snapshot_spa_boost,
                            snapshot_level: *snapshot_level,
                            snapshot_type1: snapshot_type1.clone(),
                            snapshot_type2: snapshot_type2.clone(),
                            snapshot_ability: snapshot_ability.clone(),
                            snapshot_item: snapshot_item.clone(),
                        });
                        return false; // remove fired condition
                    }
                }
                true
            });
        }
    }
    // Emit SlotConditionEnd for each fired FutureMove (retain_mut borrows have ended).
    // Inference matches by move_name only (discriminant + name); snapshot fields are sentinels.
    for f in &fired {
        let slot = FieldSlot { player: f.target_player, slot_index: f.target_slot_index as u8 };
        emit(state, EventKind::SlotConditionEnd {
            slot,
            condition: crate::state::dex_data::SlotCondition::FutureMove {
                move_name: f.move_name.clone(),
                attacker_is_p1: false,
                attacker_slot_index: 0,
                attacker_mon_id: 0,
                snapshot_raw_spa: 0,
                snapshot_spa_boost: 0,
                snapshot_level: 1,
                snapshot_type1: None,
                snapshot_type2: None,
                snapshot_ability: crate::data::ability::Ability::None,
                snapshot_item: crate::data::item::Item::None,
                turns_remaining: 0,
            },
        });
    }
    fired
}

/// Apply the damage from fired Future Sight / Doom Desire hits, branching on damage rolls
/// and crits. Ignores Protect, Substitute, and Wonder Guard. The target's live stats and
/// types are used for defensive values; attacker values come from the snapshot.
fn apply_future_move_damage(
    branches: Vec<(BattleState, f64)>,
    fired: &[FiredFutureMove],
    move_dex: &HashMap<PokemonMove, MoveData>,
    config: crate::simulator::DamageConfig,
) -> Vec<(BattleState, f64)> {
    if fired.is_empty() {
        return branches;
    }
    let mut current = branches;
    for hit in fired {
        let Some(move_data) = move_dex.get(&hit.move_name) else {
            continue;
        };
        let mut next_branches: Vec<(BattleState, f64)> = Vec::new();
        for (state, prob) in current {
            let target_slot = FieldSlot {
                player: hit.target_player,
                slot_index: hit.target_slot_index as u8,
            };
            let target = match hit.target_player {
                Player::P1 => state.p1_active_mons.get(hit.target_slot_index),
                Player::P2 => state.p2_active_mons.get(hit.target_slot_index),
            };
            let Some(target) = target else {
                next_branches.push((state, prob));
                continue;
            };
            if target.fainted {
                next_branches.push((state, prob));
                continue;
            }
            // Check Dark-type immunity (Future Sight only; Doom Desire is Steel)
            let attack_type = effective_move_type(&state, target, move_data); // uses move_data.move_type
            let effectiveness = {
                // Build the move type from the snapshot (not from attacker mon — this is what
                // effective_move_type does using the move's own type, not the attacker's type).
                // Future Sight's type is Psychic, fixed in move_data. Electrify can change it
                // but that volatile is on the target here; normally it applies to the attacker.
                // For simplicity, use the move_data type directly (Psychic for FS, Steel for DD).
                move_type_effectiveness(&state, &move_data.pokemon_type, target)
            };
            if effectiveness == 0.0 {
                // Immune — no hit, no message in this branch
                next_branches.push((state, prob));
                continue;
            }
            // Build a synthetic attacker using the target as a clone template, then patch
            // the fields that affect the offensive damage calculation.
            let mut synthetic_attacker = target.clone();
            synthetic_attacker.stats[3] = hit.snapshot_raw_spa;
            synthetic_attacker.boosts = [0, 0, hit.snapshot_spa_boost, 0, 0, 0, 0];
            synthetic_attacker.types = {
                let mut ts = Vec::new();
                if let Some(t) = hit.snapshot_type1.clone() {
                    ts.push(t);
                }
                if let Some(t) = hit.snapshot_type2.clone() {
                    ts.push(t);
                }
                ts
            };
            synthetic_attacker.ability = hit.snapshot_ability.clone();
            synthetic_attacker.item = hit.snapshot_item.clone();
            synthetic_attacker.level = hit.snapshot_level;
            synthetic_attacker.status = None;
            synthetic_attacker.volatiles = vec![];
            synthetic_attacker.is_tera = false;
            synthetic_attacker.is_mega = false;
            // Compute damage outcomes (branches on rolls and crits).
            // targets_multiplier = 1.0, invulnerability_multiplier = 1.0 (ignores Protect).
            let damage_outcomes = calculate_damage_outcomes_for_target(
                &state,
                &synthetic_attacker,
                target,
                target_slot, // attacker slot unknown; pass target slot as placeholder
                target_slot,
                move_data,
                config,
                1.0,
                1.0,
            );
            for (dmg, _is_crit, dmg_prob) in damage_outcomes {
                let mut new_state = state.clone();
                let env = berry_env(&new_state, target_slot);
                let as_ = abilities_are_suppressed(&new_state);
                if let Some(t) = get_pokemon_at_slot_mut(&mut new_state, target_slot) {
                    take_damage(t, dmg, env, as_);
                }
                next_branches.push((new_state, prob * dmg_prob));
            }
        }
        current = next_branches;
    }
    current
}

/// Aqua Ring end-of-turn heal: 1/16 max HP, Big Root–boosted, blocked by Heal Block.
fn apply_aqua_ring_residual(mon: &mut PokemonState, items_suppressed: bool, env: BerryEnv) {
    if mon.fainted || !has_status_volatile(mon, &VolatileStatus::AquaRing) || heal_is_blocked(mon) {
        return;
    }
    let max_hp = mon.stats[0].max(1);
    let heal = apply_big_root(mon, (max_hp as u32 / 16) as u16, items_suppressed);
    gain_hp(mon, heal, env);
}

/// Ingrain end-of-turn heal: 1/16 max HP, Big Root–boosted, blocked by Heal Block.
fn apply_ingrain_residual(mon: &mut PokemonState, items_suppressed: bool, env: BerryEnv) {
    if mon.fainted || !has_status_volatile(mon, &VolatileStatus::Ingrain) || heal_is_blocked(mon) {
        return;
    }
    let max_hp = mon.stats[0].max(1);
    let heal = apply_big_root(mon, (max_hp as u32 / 16) as u16, items_suppressed);
    gain_hp(mon, heal, env);
}

/// Leech Seed end-of-turn drain. Each seeded active Pokémon loses 1/8 of its max HP (min 1).
/// In singles the HP goes to the opposing active mon (the seeder's slot). If that mon has
/// Liquid Ooze the seeder takes the drained amount as damage instead of healing. Big Root on
/// the seeder boosts the heal (or the Liquid Ooze backlash) but never the damage to the target.
fn apply_leech_seed_residual(
    state: &mut BattleState,
    p1_envs: &[BerryEnv],
    p2_envs: &[BerryEnv],
    items_suppressed: bool,
) {
    let abilities_suppressed = abilities_are_suppressed(state);
    // (seeded player, slot index) for each active mon currently carrying Leech Seed.
    let mut seeded: Vec<(Player, usize)> = Vec::new();
    for (i, mon) in state.p1_active_mons.iter().enumerate() {
        if !mon.fainted && has_status_volatile(mon, &VolatileStatus::LeechSeed) {
            seeded.push((Player::P1, i));
        }
    }
    for (i, mon) in state.p2_active_mons.iter().enumerate() {
        if !mon.fainted && has_status_volatile(mon, &VolatileStatus::LeechSeed) {
            seeded.push((Player::P2, i));
        }
    }

    for (player, idx) in seeded {
        // In singles the seeder occupies the mirroring slot on the opposing side.
        let seeder_player = match player {
            Player::P1 => Player::P2,
            Player::P2 => Player::P1,
        };
        let (target_mon, target_env) = match player {
            Player::P1 => (&state.p1_active_mons[idx], p1_envs[idx]),
            Player::P2 => (&state.p2_active_mons[idx], p2_envs[idx]),
        };
        let drain = (target_mon.stats[0].max(1) as u32 / 8).max(1) as u16;
        // Does the seeded mon have (unsuppressed) Liquid Ooze?
        let liquid_ooze = !abilities_suppressed && target_mon.ability == Ability::LiquidOoze;
        // Magic Guard prevents Leech Seed damage to the holder.
        let target_magic_guard = !abilities_suppressed && target_mon.ability == Ability::MagicGuard;

        // Apply the drain to the seeded mon (skipped by Magic Guard).
        if !target_magic_guard {
            match player {
                Player::P1 => take_damage(
                    &mut state.p1_active_mons[idx],
                    drain,
                    target_env,
                    abilities_suppressed,
                ),
                Player::P2 => take_damage(
                    &mut state.p2_active_mons[idx],
                    drain,
                    target_env,
                    abilities_suppressed,
                ),
            }
        }

        // Resolve the seeder (opposing active slot in singles); skip if empty/fainted.
        let (seeder_opt, seeder_env) = match seeder_player {
            Player::P1 => (state.p1_active_mons.get_mut(idx), p1_envs.get(idx).copied()),
            Player::P2 => (state.p2_active_mons.get_mut(idx), p2_envs.get(idx).copied()),
        };
        let (Some(seeder), Some(seeder_env)) = (seeder_opt, seeder_env) else {
            continue;
        };
        if seeder.fainted {
            continue;
        }
        // If Magic Guard blocked the drain, no HP was transferred; seeder gets nothing.
        if target_magic_guard {
            continue;
        }
        let amount = apply_big_root(seeder, drain, items_suppressed);
        if liquid_ooze {
            take_damage(seeder, amount, seeder_env, abilities_suppressed);
        } else if !heal_is_blocked(seeder) {
            let before = seeder.hp;
            gain_hp(seeder, amount, seeder_env);
            let post_hp = seeder.hp;
            let max_hp = seeder.stats[0];
            // NLL: seeder last used above; borrow of state.{p1,p2}_active_mons ends here.
            if post_hp != before {
                let seeder_slot = FieldSlot { player: seeder_player, slot_index: idx as u8 };
                if let Some(observer) = state.event_observer {
                    let new_hp = if seeder_slot.player == observer {
                        PokemonHP::Number(post_hp)
                    } else {
                        PokemonHP::Percent(hp_to_percent(post_hp, max_hp))
                    };
                    emit(state, EventKind::Healed { target: seeder_slot, new_hp });
                }
            }
        }
    }
}

/// Phase 1.5 of end-of-turn processing: deal chip damage to partially-trapped Pokémon.
/// Each trapped mon takes 1/8 (or 1/6 with trapper's Binding Band) of its own max HP.
/// Magic Guard prevents this. BerryEnvs must have been pre-computed for the active slots.
fn apply_binding_chip_damage(
    state: &mut BattleState,
    p1_envs: &[BerryEnv],
    p2_envs: &[BerryEnv],
    abilities_suppressed: bool,
    items_suppressed: bool,
) {
    // Collect (side, slot_index, trapper_mon_id) for each trapped active mon.
    let trapped_slots: Vec<(Player, usize, u8)> = {
        let p1_trapped: Vec<_> = state
            .p1_active_mons
            .iter()
            .enumerate()
            .filter_map(|(i, m)| {
                m.volatiles.iter().find_map(|v| {
                    if let VolatileStatusState::TurnStatus(
                        VolatileStatus::PartiallyTrapped(src),
                        _,
                    ) = v
                    {
                        Some((Player::P1, i, *src))
                    } else {
                        None
                    }
                })
            })
            .collect();
        let p2_trapped: Vec<_> = state
            .p2_active_mons
            .iter()
            .enumerate()
            .filter_map(|(i, m)| {
                m.volatiles.iter().find_map(|v| {
                    if let VolatileStatusState::TurnStatus(
                        VolatileStatus::PartiallyTrapped(src),
                        _,
                    ) = v
                    {
                        Some((Player::P2, i, *src))
                    } else {
                        None
                    }
                })
            })
            .collect();
        p1_trapped.into_iter().chain(p2_trapped).collect()
    };

    for (side, slot_idx, src_id) in trapped_slots {
        // Find whether the trapper (by mon_id) holds an active Binding Band.
        let binding_band = state
            .p1_active_mons
            .iter()
            .chain(state.p2_active_mons.iter())
            .find(|m| m.mon_id == src_id)
            .map_or(false, |trapper| {
                !items_suppressed
                    && !klutz_disables_item(trapper)
                    && trapper.item == Item::BindingBand
            });

        let env = match side {
            Player::P1 => p1_envs[slot_idx],
            Player::P2 => p2_envs[slot_idx],
        };

        let mons = match side {
            Player::P1 => &mut state.p1_active_mons,
            Player::P2 => &mut state.p2_active_mons,
        };
        let Some(mon) = mons.get_mut(slot_idx) else {
            continue;
        };
        if mon.fainted {
            continue;
        }

        let magic_guard = !abilities_suppressed && mon.ability == Ability::MagicGuard;
        if magic_guard {
            continue;
        }

        let max_hp = mon.stats[0].max(1);
        let chip = if binding_band {
            ((max_hp as u32) / 6).max(1) as u16
        } else {
            ((max_hp as u32) / 8).max(1) as u16
        };
        deal_residual_damage(mon, chip, env, abilities_suppressed);
    }
}

/// End-of-turn Salt Cure chip damage: 1/8 max HP (1/4 for Water- or Steel-types).
/// Magic Guard prevents the damage. Called from `apply_pre_status_residuals`.
fn apply_salt_cure_damage(
    state: &mut BattleState,
    p1_envs: &[BerryEnv],
    p2_envs: &[BerryEnv],
    abilities_suppressed: bool,
) {
    let salt_cured_slots: Vec<(Player, usize)> = {
        let p1: Vec<_> = state
            .p1_active_mons
            .iter()
            .enumerate()
            .filter_map(|(i, m)| {
                if !m.fainted && has_status_volatile(m, &VolatileStatus::SaltCure) {
                    Some((Player::P1, i))
                } else {
                    None
                }
            })
            .collect();
        let p2: Vec<_> = state
            .p2_active_mons
            .iter()
            .enumerate()
            .filter_map(|(i, m)| {
                if !m.fainted && has_status_volatile(m, &VolatileStatus::SaltCure) {
                    Some((Player::P2, i))
                } else {
                    None
                }
            })
            .collect();
        p1.into_iter().chain(p2).collect()
    };

    for (side, slot_idx) in salt_cured_slots {
        let env = match side {
            Player::P1 => p1_envs[slot_idx],
            Player::P2 => p2_envs[slot_idx],
        };
        let mons = match side {
            Player::P1 => &mut state.p1_active_mons,
            Player::P2 => &mut state.p2_active_mons,
        };
        let Some(mon) = mons.get_mut(slot_idx) else {
            continue;
        };
        if mon.fainted {
            continue;
        }
        if !abilities_suppressed && mon.ability == Ability::MagicGuard {
            continue;
        }

        let max_hp = mon.stats[0].max(1);
        let is_water_or_steel = pokemon_has_type(mon, &PokemonType::Water)
            || pokemon_has_type(mon, &PokemonType::Steel);
        let divisor: u32 = if is_water_or_steel { 4 } else { 8 };
        let chip = ((max_hp as u32) / divisor).max(1) as u16;
        deal_residual_damage(mon, chip, env, abilities_suppressed);
    }
}

/// Phase 3 of end-of-turn processing: apply burn/poison/toxic damage.
/// Called after the status-cure phase so that cured statuses take no damage.
fn apply_status_damage(state: &mut BattleState) {
    let abilities_suppressed = abilities_are_suppressed(state);
    let p1_envs: Vec<BerryEnv> = (0..state.p1_active_mons.len())
        .map(|i| {
            berry_env(
                state,
                FieldSlot {
                    player: Player::P1,
                    slot_index: i as u8,
                },
            )
        })
        .collect();
    let p2_envs: Vec<BerryEnv> = (0..state.p2_active_mons.len())
        .map(|i| {
            berry_env(
                state,
                FieldSlot {
                    player: Player::P2,
                    slot_index: i as u8,
                },
            )
        })
        .collect();
    for (i, mon) in state.p1_active_mons.iter_mut().enumerate() {
        apply_status_residual(mon, abilities_suppressed, p1_envs[i]);
    }
    for (i, mon) in state.p2_active_mons.iter_mut().enumerate() {
        apply_status_residual(mon, abilities_suppressed, p2_envs[i]);
    }
}

/// Public wrapper that combines all three deterministic EoT phases (pre-residuals + cure + damage).
/// Used by tests and any caller that wants the full deterministic end-of-turn without the
/// probabilistic ability phases (Shed Skin, Healer, Moody, Harvest).
/// For the full probabilistic pipeline, use `end_turn` which returns branched outcomes.
pub fn apply_end_of_turn_status_effects(state: &mut BattleState) {
    apply_pre_status_residuals(state);
    // Apply Hydration (the one deterministic status cure) before damage.
    let rain = weather_is_rain(state);
    let abilities_suppressed = abilities_are_suppressed(state);
    for mon in state
        .p1_active_mons
        .iter_mut()
        .chain(state.p2_active_mons.iter_mut())
    {
        if !mon.fainted && !abilities_suppressed && mon.ability == Ability::Hydration && rain {
            mon.status = None;
        }
    }
    apply_status_damage(state);
}

/// Phase 2 of end-of-turn processing: probabilistic status-cure abilities.
/// Handles: Hydration (deterministic in rain), Shed Skin (1/3 chance), Healer (1/2 per ally).
/// Returns branched outcomes because Shed Skin and Healer can each flip a coin.
fn apply_status_cure_abilities(branches: Vec<(BattleState, f64)>) -> Vec<(BattleState, f64)> {
    let mut result = branches;

    // Collect all active slots across all branches — the ability set is the same, so we can
    // determine which (player, slot_index) pairs to process from the first branch.
    let slots_to_check: Vec<FieldSlot> = if let Some((first, _)) = result.first() {
        let mut slots = Vec::new();
        for (i, _) in first.p1_active_mons.iter().enumerate() {
            slots.push(FieldSlot {
                player: Player::P1,
                slot_index: i as u8,
            });
        }
        for (i, _) in first.p2_active_mons.iter().enumerate() {
            slots.push(FieldSlot {
                player: Player::P2,
                slot_index: i as u8,
            });
        }
        slots
    } else {
        return result;
    };

    for slot in &slots_to_check {
        // For each branch, inspect the ability of the mon at this slot.
        let ability = result
            .first()
            .and_then(|(bs, _)| get_pokemon_at_slot(bs, *slot))
            .map(|m| m.ability.clone())
            .unwrap_or(Ability::None);

        let abilities_suppressed_for_slot = result
            .first()
            .map(|(bs, _)| {
                get_pokemon_at_slot(bs, *slot)
                    .map(|m| pokemon_ability_is_suppressed(bs, m))
                    .unwrap_or(true)
            })
            .unwrap_or(true);

        if abilities_suppressed_for_slot {
            continue;
        }

        match ability {
            // Hydration: deterministic cure in rain.
            Ability::Hydration => {
                for (bs, _) in result.iter_mut() {
                    let rain = weather_is_rain(bs);
                    if let Some(mon) = get_pokemon_at_slot_mut(bs, *slot) {
                        // Setting None to None is a no-op; the is_some() guard is omitted.
                        if !mon.fainted && rain {
                            mon.status = None;
                        }
                    }
                }
            }
            // Shed Skin: 1/3 chance to cure the holder's own non-volatile status.
            Ability::ShedSkin => {
                result = eot_fork_per_slot(result, *slot, 1.0 / 3.0, |mon| {
                    if mon.status.is_some() {
                        mon.status = None;
                    }
                });
            }
            // Healer: 50% chance per adjacent ally to cure that ally's status.
            // In singles there are no allies, so this is a no-op.
            Ability::Healer => {
                // Collect ally slots (same side, different index).
                let ally_slots: Vec<FieldSlot> = slots_to_check
                    .iter()
                    .filter(|s| s.player == slot.player && s.slot_index != slot.slot_index)
                    .copied()
                    .collect();
                for ally_slot in ally_slots {
                    result = eot_fork_per_slot(result, ally_slot, 0.5, |mon| {
                        if mon.status.is_some() {
                            mon.status = None;
                        }
                    });
                }
            }
            _ => {}
        }
    }

    result
}

/// Phase 4 of end-of-turn processing: late-trigger ability effects.
/// Handles: Speed Boost (+1 Spe), Moody (+2/-1 random stats), Harvest (berry restore),
/// Hunger Switch (Morpeko form toggle).
fn apply_late_eot_abilities(branches: Vec<(BattleState, f64)>) -> Vec<(BattleState, f64)> {
    let mut result = branches;

    let slots_to_check: Vec<FieldSlot> = if let Some((first, _)) = result.first() {
        let mut slots = Vec::new();
        for (i, _) in first.p1_active_mons.iter().enumerate() {
            slots.push(FieldSlot {
                player: Player::P1,
                slot_index: i as u8,
            });
        }
        for (i, _) in first.p2_active_mons.iter().enumerate() {
            slots.push(FieldSlot {
                player: Player::P2,
                slot_index: i as u8,
            });
        }
        slots
    } else {
        return result;
    };

    for slot in &slots_to_check {
        let ability = result
            .first()
            .and_then(|(bs, _)| get_pokemon_at_slot(bs, *slot))
            .map(|m| m.ability.clone())
            .unwrap_or(Ability::None);

        let abilities_suppressed_for_slot = result
            .first()
            .map(|(bs, _)| {
                get_pokemon_at_slot(bs, *slot)
                    .map(|m| pokemon_ability_is_suppressed(bs, m))
                    .unwrap_or(true)
            })
            .unwrap_or(true);

        if abilities_suppressed_for_slot {
            continue;
        }

        match ability {
            // Speed Boost: +1 Speed every turn, but not on the turn the Pokémon switched in.
            Ability::SpeedBoost => {
                for (bs, _) in result.iter_mut() {
                    let items_suppressed = items_are_suppressed(bs);
                    let delta = if let Some(mon) = get_pokemon_at_slot_mut(bs, *slot) {
                        if !mon.fainted && !mon.entered_this_turn {
                            apply_stat_boosts_to_pokemon(
                                mon,
                                &[0, 0, 0, 0, 1, 0, 0],
                                items_suppressed,
                                false,
                            )
                        } else { [0; 7] }
                    } else { [0; 7] };
                    // Emit after the get_pokemon_at_slot_mut borrow ends.
                    for (boost_idx, &stages) in delta.iter().enumerate() {
                        if stages != 0 {
                            emit(bs, EventKind::BoostChanged { target: *slot, boost_idx, stages });
                        }
                    }
                }
            }
            // Moody: +2 to one random stat, -1 to a different random stat (Gen VIII+: 5 main
            // stats only, no accuracy/evasion).
            Ability::Moody => {
                // Enumerate (raise, lower) pairs from the first branch's state (same boosts on all).
                let (can_raise, can_lower, boosts_snapshot) = if let Some((bs, _)) = result.first()
                {
                    if let Some(mon) = get_pokemon_at_slot(bs, *slot) {
                        if mon.fainted {
                            continue;
                        }
                        let b = mon.boosts;
                        let raise: Vec<usize> = (0..5).filter(|&i| b[i] < 6).collect();
                        let lower: Vec<usize> = (0..5).filter(|&i| b[i] > -6).collect();
                        (raise, lower, b)
                    } else {
                        continue;
                    }
                } else {
                    continue;
                };

                // Build outcome table: each row is (raise_idx_opt, lower_idx_opt, probability).
                // Degenerate cases handled per Bulbapedia: all-capped → only lower; all-floored → only raise.
                let outcomes: Vec<(Option<usize>, Option<usize>, f64)> =
                    match (can_raise.len(), can_lower.len()) {
                        (0, 0) => vec![(None, None, 1.0)],
                        (0, m) => can_lower
                            .iter()
                            .map(|&j| (None, Some(j), 1.0 / m as f64))
                            .collect(),
                        (n, 0) => can_raise
                            .iter()
                            .map(|&i| (Some(i), None, 1.0 / n as f64))
                            .collect(),
                        (n, _) => {
                            let mut out = Vec::new();
                            for &i in &can_raise {
                                let lower_cands: Vec<usize> = can_lower
                                    .iter()
                                    .copied()
                                    .filter(|&j| j != i || boosts_snapshot[i] > -6)
                                    .collect();
                                // Re-filter: lower candidates are stats that can still go lower
                                // and differ from the raised stat.
                                let lower_eligible: Vec<usize> = (0..5)
                                    .filter(|&j| boosts_snapshot[j] > -6 && j != i)
                                    .collect();
                                let _ = lower_cands; // replaced by lower_eligible
                                let m_after = lower_eligible.len();
                                if m_after == 0 {
                                    // No valid lower stat given this raise — only raise occurs.
                                    out.push((Some(i), None, 1.0 / n as f64));
                                } else {
                                    for &j in &lower_eligible {
                                        out.push((
                                            Some(i),
                                            Some(j),
                                            1.0 / n as f64 / m_after as f64,
                                        ));
                                    }
                                }
                            }
                            out
                        }
                    };

                // Expand each current branch by the Moody outcome table.
                let mut new_result: Vec<(BattleState, f64)> =
                    Vec::with_capacity(result.len() * outcomes.len());
                for (bs, prob) in result {
                    if prob <= 0.0 {
                        continue;
                    }
                    let items_suppressed = items_are_suppressed(&bs);
                    for &(raise_idx, lower_idx, outcome_prob) in &outcomes {
                        if outcome_prob <= 0.0 {
                            continue;
                        }
                        let mut branch = bs.clone();
                        let (raise_delta, lower_delta) = if let Some(mon) = get_pokemon_at_slot_mut(&mut branch, *slot) {
                            let r = if let Some(i) = raise_idx {
                                let mut d = [0i8; 7]; d[i] = 2;
                                apply_stat_boosts_to_pokemon(mon, &d, items_suppressed, false)
                            } else { [0i8; 7] };
                            let l = if let Some(j) = lower_idx {
                                let mut d = [0i8; 7]; d[j] = -1;
                                apply_stat_boosts_to_pokemon(mon, &d, items_suppressed, false)
                            } else { [0i8; 7] };
                            (r, l)
                        } else { ([0i8; 7], [0i8; 7]) };
                        // Emit BoostChanged for each stat that actually changed (NLL: mon borrow ended).
                        for (boost_idx, &stages) in raise_delta.iter().enumerate() {
                            if stages != 0 { emit(&mut branch, EventKind::BoostChanged { target: *slot, boost_idx, stages }); }
                        }
                        for (boost_idx, &stages) in lower_delta.iter().enumerate() {
                            if stages != 0 { emit(&mut branch, EventKind::BoostChanged { target: *slot, boost_idx, stages }); }
                        }
                        new_result.push((branch, prob * outcome_prob));
                    }
                }
                result = coalesce_branches(new_result);
            }
            // Harvest: 50% chance to restore a consumed Berry (100% in harsh sunlight).
            // Requires: item slot empty, last consumed item was a Berry.
            Ability::Harvest => {
                // Check conditions from the first branch (same for all branches at this point).
                let (restorable_berry, in_sun) = if let Some((bs, _)) = result.first() {
                    if let Some(mon) = get_pokemon_at_slot(bs, *slot) {
                        let berry = mon
                            .consumed_item
                            .as_ref()
                            .filter(|it| format!("{:?}", it).ends_with("Berry"))
                            .cloned();
                        let empty = mon.item == Item::None;
                        // Mega Sol: holder perceives sun, so Harvest always restores.
                        (
                            if empty && !mon.fainted { berry } else { None },
                            weather_is_sunlight_for(bs, mon),
                        )
                    } else {
                        (None, false)
                    }
                } else {
                    (None, false)
                };

                let Some(berry_item) = restorable_berry else {
                    continue;
                };
                let chance = if in_sun { 1.0 } else { 0.5 };

                result = eot_fork_per_slot(result, *slot, chance, |mon| {
                    if let Some(berry) = mon.consumed_item.take() {
                        mon.item = berry;
                        mon.item_lost = false;
                        // consumed_item is now None; on_item_obtained_or_enabled would fire
                        // pinch-berry re-triggers, but those require items_suppressed context.
                        // The item is simply restored here; pinch-berry logic runs on next HP change.
                    }
                });

                // A Harvest-restored berry no longer counts as "used this turn" — remove it
                // from the Pickup pool in the branches where the restore fired.
                for (bs, _) in result.iter_mut() {
                    let restored = get_pokemon_at_slot(bs, *slot)
                        .map(|m| m.item == berry_item && m.consumed_item.is_none())
                        .unwrap_or(false);
                    if restored {
                        if let Some(pos) = bs
                            .items_consumed_this_turn
                            .iter()
                            .position(|(_, it)| *it == berry_item)
                        {
                            bs.items_consumed_this_turn.remove(pos);
                        }
                    }
                }
            }
            // Pickup: at end of turn, an empty-handed holder retrieves the most recently
            // consumed one-time item used by *another* Pokémon this turn. Multiple Pickup
            // users resolve in slot order (cartridge: speed order — simplification).
            Ability::Pickup => {
                for (bs, _) in result.iter_mut() {
                    let can_pick = get_pokemon_at_slot(bs, *slot)
                        .map(|m| !m.fainted && m.item == Item::None)
                        .unwrap_or(false);
                    if !can_pick {
                        continue;
                    }
                    let Some(pos) = bs
                        .items_consumed_this_turn
                        .iter()
                        .rposition(|(consumer, _)| consumer != slot)
                    else {
                        continue;
                    };
                    let (_, item) = bs.items_consumed_this_turn.remove(pos);
                    let env = berry_env(bs, *slot);
                    let cure = if let Some(mon) = get_pokemon_at_slot_mut(bs, *slot) {
                        mon.item = item.clone();
                        mon.item_lost = false;
                        on_item_obtained_or_enabled(mon, &env)
                    } else {
                        BerryCure::none()
                    };
                    emit(bs, EventKind::ItemGained { slot: *slot, item });
                    emit_berry_cure(bs, *slot, &cure);
                }
            }
            // Cud Chew: re-apply a consumed berry's effect at the end of the turn
            // *after* it was eaten. `armed=false` means this is the first EOT; flip to
            // `armed=true`. On the second EOT (`armed=true`) fire the re-eat and clear.
            Ability::CudChew => {
                let pending = result
                    .first()
                    .and_then(|(bs, _)| get_pokemon_at_slot(bs, *slot))
                    .and_then(|m| m.cud_chew_pending.clone());

                match pending {
                    Some((_, false)) => {
                        for (bs, _) in result.iter_mut() {
                            if let Some(mon) = get_pokemon_at_slot_mut(bs, *slot) {
                                if let Some((_, ref mut armed)) = mon.cud_chew_pending {
                                    *armed = true;
                                }
                            }
                        }
                    }
                    Some((berry, true)) => {
                        for (bs, _) in result.iter_mut() {
                            let env = berry_env(bs, *slot);
                            if let Some(mon) = get_pokemon_at_slot_mut(bs, *slot) {
                                if !mon.fainted {
                                    mon.cud_chew_pending = None;
                                    apply_berry_effect(mon, &berry, &env);
                                    on_berry_eaten(mon, &berry, &env);
                                }
                            }
                        }
                    }
                    None => {}
                }
            }
            // Hunger Switch: toggle Morpeko between Full Belly and Hangry form each turn.
            // Does not toggle while Terastallized.
            Ability::HungerSwitch => {
                for (bs, _) in result.iter_mut() {
                    if let Some(mon) = get_pokemon_at_slot_mut(bs, *slot) {
                        if mon.fainted || mon.is_tera {
                            continue;
                        }
                        mon.species = match mon.species {
                            Species::Morpeko => Species::MorpekoHangry,
                            Species::MorpekoHangry => Species::Morpeko,
                            _ => mon.species.clone(),
                        };
                    }
                }
            }
            _ => {}
        }
    }

    result
}

/// Helper: for each branch, fork on a probability `chance` applied to the mon at `slot`.
/// `apply_fn` mutates the mon in the "triggered" branch.
fn eot_fork_per_slot<F>(
    branches: Vec<(BattleState, f64)>,
    slot: FieldSlot,
    chance: f64,
    apply_fn: F,
) -> Vec<(BattleState, f64)>
where
    F: Fn(&mut PokemonState),
{
    if chance <= 0.0 {
        return branches;
    }
    if chance >= 1.0 {
        let mut result = branches;
        for (bs, _) in result.iter_mut() {
            if let Some(mon) = get_pokemon_at_slot_mut(bs, slot) {
                apply_fn(mon);
            }
        }
        return result;
    }
    let mut result = Vec::with_capacity(branches.len() * 2);
    for (bs, prob) in branches {
        if prob <= 0.0 {
            continue;
        }
        // Check whether the ability should fire at all (skip fainted mons).
        let should_check = get_pokemon_at_slot(&bs, slot)
            .map(|m| !m.fainted)
            .unwrap_or(false);
        if !should_check {
            result.push((bs, prob));
            continue;
        }
        // "Not triggered" branch.
        result.push((bs.clone(), prob * (1.0 - chance)));
        // "Triggered" branch.
        let mut triggered = bs;
        if let Some(mon) = get_pokemon_at_slot_mut(&mut triggered, slot) {
            apply_fn(mon);
        }
        result.push((triggered, prob * chance));
    }
    result
}

/// If a mon is frozen and takes damage from a fire move or certain moves, unfreeze it.
/// `thaws_target` comes from `MoveData::thaws_target` (the parsed `thawsTarget` field).
pub fn handle_unfreeze_on_damage(
    mon: &mut PokemonState,
    thaws_target: bool,
    move_type: &PokemonType,
    damage: u16,
) {
    if damage == 0 {
        return;
    }
    if let Some(Status::Frozen(_)) = mon.status {
        // Fire-type moves thaw
        if std::mem::discriminant(move_type) == std::mem::discriminant(&PokemonType::Fire) {
            mon.status = None;
            return;
        }

        // Moves with thawsTarget: true (Scald, Steam Eruption, Scorching Sands, Matcha Gotcha)
        if thaws_target {
            mon.status = None;
        }
    }
}

/// Returns true if this move thaws the user when used (has the Defrost flag).
pub fn move_thaws_user_on_use(move_data: &MoveData) -> bool {
    move_has_flag(move_data, &MoveFlag::Defrost)
}

/// Apply a volatile status to a pokemon (prevents duplicate volatiles of the same type).
/// `attacker_mold_break` should be `true` when the source has a Mold Breaker / Turboblaze /
/// Teravolt ability — it bypasses the Sweet Veil self-protection block on the holder.
///
/// Returns the list of volatile statuses removed by a Mental Herb activation (empty if none).
/// Callers must emit `VolatileEnd` for each entry and `ItemLost{MentalHerb, consumed:true}`
/// after releasing the `&mut PokemonState` borrow.
fn apply_volatile_to_pokemon(
    state: &BattleState,
    mon: &mut PokemonState,
    volatile: &VolatileStatus,
    attacker_mold_break: bool,
) -> Vec<VolatileStatus> {
    // Check if pokemon already has this volatile status
    let already_has = has_status_volatile(mon, volatile);

    if !already_has {
        // Leech Seed cannot be planted on Grass-type Pokémon or through a Substitute.
        if matches!(volatile, VolatileStatus::LeechSeed)
            && (pokemon_has_type(mon, &PokemonType::Grass)
                || has_status_volatile(mon, &VolatileStatus::Substitute(0)))
        {
            return Vec::new();
        }

        if matches!(volatile, VolatileStatus::Confusion)
            && !pokemon_ability_is_suppressed(state, mon)
            && mon.ability == Ability::OwnTempo
        {
            return Vec::new();
        }

        // Oblivious: blocks Taunt and Attract (Mold Breaker bypass handled in apply_effect_to_target).
        if matches!(volatile, VolatileStatus::Taunt | VolatileStatus::Attract)
            && !pokemon_ability_is_suppressed(state, mon)
            && mon.ability == Ability::Oblivious
        {
            return Vec::new();
        }

        if matches!(volatile, VolatileStatus::Confusion)
            && pokemon_is_on_terrain(state, mon, &Terrain::MistyTerrain)
        {
            return Vec::new();
        }

        if matches!(volatile, VolatileStatus::Yawn)
            && pokemon_is_on_terrain(state, mon, &Terrain::ElectricTerrain)
        {
            return Vec::new();
        }

        // Sweet Veil / Insomnia / Vital Spirit: the holder cannot receive Yawn (it can never
        // fall asleep, so the drowsy volatile must not attach in the first place).
        // Ally protection (Sweet Veil protecting teammates' Yawn) is handled at the
        // apply_effect_to_target call site where side context is available.
        // Mold Breaker bypasses this self-protection (attacker_mold_break passed from caller).
        if matches!(volatile, VolatileStatus::Yawn)
            && !attacker_mold_break
            && !pokemon_ability_is_suppressed(state, mon)
            && matches!(
                mon.ability,
                Ability::SweetVeil | Ability::Insomnia | Ability::VitalSpirit
            )
        {
            return Vec::new();
        }

        let is_move_status = matches!(
            volatile,
            VolatileStatus::Disable(_)
                | VolatileStatus::CantUseRepeatedly(_)
                | VolatileStatus::Encore(_)
                | VolatileStatus::GlaiveRush
                | VolatileStatus::Taunt
                | VolatileStatus::ThroatChop
                | VolatileStatus::SemiInvulnerable(_)
                | VolatileStatus::Confusion
        );

        let duration = match volatile {
            VolatileStatus::Disable(_) => 4,
            VolatileStatus::CantUseRepeatedly(_) => 1,
            VolatileStatus::Encore(_) => 3,
            VolatileStatus::Taunt => 3,
            VolatileStatus::ThroatChop => 2,
            VolatileStatus::GlaiveRush => 1,
            VolatileStatus::SemiInvulnerable(_) => 0,
            VolatileStatus::Confusion => thread_rng().gen_range(2..=5),
            _ => get_volatile_duration(volatile),
        };

        if is_move_status {
            mon.volatiles
                .push(VolatileStatusState::MoveStatus(volatile.clone(), duration));
        } else {
            mon.volatiles
                .push(VolatileStatusState::TurnStatus(volatile.clone(), duration));
        }

        // Mental Herb: immediately cure if the newly-added volatile is one it targets.
        // Return the list of cleared volatiles so callers can emit VolatileEnd events.
        let items_suppressed = items_are_suppressed(state);
        return try_consume_mental_herb(mon, items_suppressed);
    }
    Vec::new()
}

/// Apply stat boosts to a pokemon. If any entry of `boosts` is negative (a stat drop was
/// applied), also try to trigger a White Herb.
/// Public thin wrapper around `apply_stat_boosts_to_pokemon` for callers outside this module
/// (primarily `simulator.rs`).  Passes `defer_white_herb: false` so White Herb fires
/// immediately — appropriate for all self-boost paths (Moxie, post-KO boosts, etc.).
/// Returns the boost-array index (0=Atk,1=Def,2=SpA,3=SpD,4=Spe) of the highest base stat
/// (non-HP), with tie-breaking order Atk > Def > SpA > SpD > Spe.
/// Used by Eelevate's KO bonus (raise highest stat +1) and can serve future Beast Boost.
pub(crate) fn highest_boostable_stat_index(mon: &PokemonState) -> usize {
    // stats layout: [hp, atk, def, spa, spd, spe] → boost indices [0=atk,1=def,2=spa,3=spd,4=spe]
    let stat_to_boost = [(1usize, 0usize), (2, 1), (3, 2), (4, 3), (5, 4)];
    let (_, best_boost) = stat_to_boost
        .iter()
        .max_by_key(|&&(stat_idx, _)| mon.stats[stat_idx])
        .copied()
        .unwrap_or((1, 0));
    best_boost
}

/// Returns the clamped delta so callers can emit `BoostChanged` events.
pub(crate) fn apply_stat_boost_external(
    mon: &mut PokemonState,
    boosts: &[i8; 7],
    items_suppressed: bool,
) -> [i8; 7] {
    apply_stat_boosts_to_pokemon(mon, boosts, items_suppressed, false)
}

/// Public thin wrapper around `apply_volatile_to_pokemon` for callers outside this module.
/// Always passes `attacker_mold_break = false`; callers that need Mold Breaker bypass should
/// route through `apply_effect_to_target` instead.
///
/// The Mental Herb return value is intentionally discarded here: all current call sites in
/// `mod.rs` apply non-mental volatiles (PerishSong, DestinyBond, FocusEnergy, GastroAcid),
/// so the return is always empty.  Route through `apply_effect_to_target` for volatiles
/// that could trigger Mental Herb.
pub(crate) fn apply_volatile_to_pokemon_pub(
    state: &BattleState,
    mon: &mut PokemonState,
    volatile: &VolatileStatus,
) {
    let _ = apply_volatile_to_pokemon(state, mon, volatile, false);
}

/// Apply a stat boost delta to `mon`, clamping each stage to `[-6, 6]`.
/// If `defer_white_herb` is false (the default for all self-boost paths), White Herb
/// is checked immediately after the apply.  Pass `true` only from
/// `apply_opponent_stat_drop`, which calls `try_consume_white_herb` manually *after*
/// Defiant / Competitive have had a chance to run — ensuring a Defiant +2 that cancels
/// the only negative stage suppresses the herb.
/// Apply stat boosts to `mon`, respecting Contrary / Simple, and return the *actual* per-stat
/// change after clamping.  A stat already at ±6 reports delta 0 even if the incoming value
/// was nonzero.  Return value can be used by callers that need to emit `BoostChanged` events.
fn apply_stat_boosts_to_pokemon(
    mon: &mut PokemonState,
    boosts: &[i8; 7],
    items_suppressed: bool,
    defer_white_herb: bool,
) -> [i8; 7] {
    // Contrary inverts all stat changes; Simple doubles them. Suppression via
    // Neutralizing Gas cannot be checked here (no state); per-mon GastroAcid is used instead.
    let modified: [i8; 7];
    let boosts = if !has_status_volatile(mon, &VolatileStatus::GastroAcid) {
        match mon.ability {
            Ability::Contrary => {
                modified = std::array::from_fn(|i| -boosts[i]);
                &modified
            }
            Ability::Simple => {
                modified = std::array::from_fn(|i| boosts[i].saturating_mul(2));
                &modified
            }
            _ => boosts,
        }
    } else {
        boosts
    };
    let mut delta = [0i8; 7];
    for i in 0..7 {
        let before = mon.boosts[i];
        let after = (before + boosts[i]).clamp(-6, 6);
        delta[i] = after - before;
        mon.boosts[i] = after;
        // Per-turn stat-change flags use the post-clamp delta: a stat pinned at ±6
        // doesn't count as raised/lowered (Burning Jealousy / Lash Out conditions).
        if after > before {
            mon.stats_raised_this_turn = true;
        }
        if after < before {
            mon.stats_lowered_this_turn = true;
        }
    }
    if !defer_white_herb && boosts.iter().any(|&b| b < 0) {
        try_consume_white_herb(mon, items_suppressed);
    }
    delta
}

/// Apply a stat boost delta and return the *actual* per-stat change after clamping.
/// A stat already at −6 will report delta 0 even if the incoming value was negative.
/// Used by `apply_opponent_stat_drop` to count how many stats were truly lowered
/// (for Defiant / Competitive triggers) and what was truly raised (for Opportunist).
fn apply_boosts_returning_delta(mon: &mut PokemonState, boosts: &[i8; 7]) -> [i8; 7] {
    let mut delta = [0i8; 7];
    for i in 0..7 {
        let before = mon.boosts[i];
        let after = (before + boosts[i]).clamp(-6, 6);
        delta[i] = after - before;
        mon.boosts[i] = after;
        // Per-turn stat-change flags (post-clamp), as in apply_stat_boosts_to_pokemon.
        if after > before {
            mon.stats_raised_this_turn = true;
        }
        if after < before {
            mon.stats_lowered_this_turn = true;
        }
    }
    delta
}

/// Returns `true` if any non-fainted, unsuppressed Pokémon on `player`'s side carries
/// `veil_ability`.  Used by Sweet Veil, Flower Veil, and Aroma Veil to protect both the
/// holder and all active allies.
fn side_has_veil(state: &BattleState, player: Player, veil_ability: Ability) -> bool {
    let mons = match player {
        Player::P1 => &state.p1_active_mons,
        Player::P2 => &state.p2_active_mons,
    };
    mons.iter()
        .filter(|mon| !mon.fainted)
        .any(|mon| !pokemon_ability_is_suppressed(state, mon) && mon.ability == veil_ability)
}

/// Zero out stat-drop entries in `boosts` that the target's ability blocks when the
/// stat change originates from another Pokémon (i.e. not self-inflicted).
///
/// - `ClearBody | WhiteSmoke | FullMetalBody` — block all external stat drops.
/// - `HyperCutter`         — block only Attack (index 0) being lowered.
/// - `BigPecks`            — block only Defense (index 1) being lowered.
/// - `KeenEye | Illuminate`— block only accuracy (index 5) being lowered.
///
/// `mold_break` skips all filtering (Mold Breaker / Turboblaze / Teravolt ignores
/// these protective abilities on the target).
///
/// Positive entries and self-inflicted drops (those going through the attacker/self_boost
/// paths) are never touched.  Callers must pass the pre-computed suppression flag so that
/// Mold Breaker and Neutralizing Gas are respected when they land.
fn filter_opponent_stat_drops(
    mon: &PokemonState,
    boosts: &[i8; 7],
    ability_suppressed: bool,
    mold_break: bool,
) -> [i8; 7] {
    if ability_suppressed || mold_break {
        return *boosts;
    }
    let mut filtered = *boosts;
    match mon.ability {
        Ability::ClearBody | Ability::WhiteSmoke | Ability::FullMetalBody => {
            for b in &mut filtered {
                if *b < 0 {
                    *b = 0;
                }
            }
        }
        Ability::HyperCutter => {
            if filtered[0] < 0 {
                filtered[0] = 0;
            }
        }
        Ability::BigPecks => {
            if filtered[1] < 0 {
                filtered[1] = 0;
            }
        }
        // Keen Eye / Illuminate: the holder's accuracy stage cannot be lowered by opponents.
        Ability::KeenEye | Ability::Illuminate => {
            if filtered[5] < 0 {
                filtered[5] = 0;
            }
        }
        _ => {}
    }
    filtered
}

/// Unified entry point for every opponent-sourced stat drop.
///
/// Order of operations:
/// 1. **Mirror Armor** (if holder is unsuppressed and `!already_reflected`): split off the
///    negative portion, bounce it back to `source_slot` (recursive call with
///    `already_reflected = true` to prevent an infinite Armor↔Armor loop), then apply any
///    non-negative remainder directly to the holder and return.
/// 2. **Immunity filter** via `filter_opponent_stat_drops` (Clear Body / Hyper Cutter / …).
/// 3. Apply the filtered delta via `apply_boosts_returning_delta`, deferring White Herb.
/// 4. **Defiant / Competitive**: +2 per stat actually lowered (after clamping).
/// 5. White Herb (`try_consume_white_herb`), now that Defiant/Competitive have run.
///
/// `already_reflected = true` is passed only for the Mirror Armor recursive bounce so that
/// the source's own Mirror Armor cannot reflect it a second time.
/// Flower Veil zeroing (if applicable) must be applied to `raw_boosts` by the caller
/// *before* calling this function.
/// Sticky Web and Octolock are not yet implemented; add Mirror Armor interactions when they are.
pub(crate) fn apply_opponent_stat_drop(
    state: &mut BattleState,
    target_slot: FieldSlot,
    source_slot: FieldSlot,
    raw_boosts: [i8; 7],
    items_suppressed: bool,
    already_reflected: bool,
) {
    if raw_boosts == [0; 7] {
        return;
    }

    // Snapshot the target's ability, suppression, and source's Mold Breaker status before
    // taking any mutable borrow.
    let (target_ability, target_suppressed) = match get_pokemon_at_slot(state, target_slot) {
        Some(m) => (m.ability.clone(), pokemon_ability_is_suppressed(state, m)),
        None => return,
    };
    let source_breaks_mold =
        get_pokemon_at_slot(state, source_slot).map_or(false, |a| attacker_breaks_mold(state, a));

    // ── 0. Contrary / Simple pre-processing ────────────────────────────────────────────────
    // Contrary inverts all opponent-sourced stat changes; Simple doubles them.
    // Bypassed when the source is a Mold Breaker attacker (Contrary/Simple are ignorable).
    let raw_boosts: [i8; 7] =
        if !target_suppressed && !source_breaks_mold && ability_is_ignorable(&target_ability) {
            match target_ability {
                Ability::Contrary => std::array::from_fn(|i| -raw_boosts[i]),
                Ability::Simple => std::array::from_fn(|i| raw_boosts[i].saturating_mul(2)),
                _ => raw_boosts,
            }
        } else {
            raw_boosts
        };
    if raw_boosts == [0; 7] {
        return;
    }

    // ── 1. Mirror Armor ────────────────────────────────────────────────────────────────────
    // Bounce the negative portion back to the source.  Only the holder's portion bounces;
    // positive entries (if any) are applied directly.  A previously-reflected drop cannot be
    // re-reflected (loop / infinite-recursion guard).
    if !already_reflected && !target_suppressed && target_ability == Ability::MirrorArmor {
        let mut bounced = [0i8; 7];
        let mut kept = [0i8; 7];
        for i in 0..7 {
            if raw_boosts[i] < 0 {
                bounced[i] = raw_boosts[i];
            } else {
                kept[i] = raw_boosts[i];
            }
        }
        // Bounce: the drop now originates from the holder (target_slot) targeting the source.
        // already_reflected = true prevents the source's Mirror Armor from sending it back.
        if bounced != [0; 7] {
            apply_opponent_stat_drop(
                state,
                source_slot,
                target_slot,
                bounced,
                items_suppressed,
                true,
            );
        }
        // Apply any positive boosts (e.g. from Decorate) directly to the holder.
        if kept != [0; 7] {
            let kept_delta = if let Some(mon) = get_pokemon_at_slot_mut(state, target_slot) {
                apply_stat_boosts_to_pokemon(mon, &kept, items_suppressed, false)
            } else { [0i8; 7] };
            for (boost_idx, &stages) in kept_delta.iter().enumerate() {
                if stages != 0 { emit(state, EventKind::BoostChanged { target: target_slot, boost_idx, stages }); }
            }
        }
        return;
    }

    // ── 2. Immunity filter (Clear Body / Hyper Cutter / Big Pecks / Keen Eye / …) ─────────
    let filtered = {
        let Some(mon) = get_pokemon_at_slot(state, target_slot) else {
            return;
        };
        filter_opponent_stat_drops(mon, &raw_boosts, target_suppressed, source_breaks_mold)
    };
    if filtered == [0; 7] {
        return;
    }

    // ── 3. Apply with clamping, defer White Herb ────────────────────────────────────────────
    let delta = {
        let Some(mon) = get_pokemon_at_slot_mut(state, target_slot) else {
            return;
        };
        apply_boosts_returning_delta(mon, &filtered)
    };
    // Emit one BoostChanged per stat that actually changed (NLL: mon borrow ended above).
    for i in 0..7 {
        if delta[i] != 0 {
            emit(state, EventKind::BoostChanged { target: target_slot, boost_idx: i, stages: delta[i] });
        }
    }

    // ── 4a. Opportunist: mirror any positive raise to opponents of target_slot ──────────────
    // Covers moves that raise the TARGET's stats (e.g. Decorate, target-boosting secondaries).
    // Uses the actual clamped delta so opponents mirror what truly landed.
    {
        let positive: [i8; 7] = {
            let mut p = [0i8; 7];
            for i in 0..7 {
                if delta[i] > 0 {
                    p[i] = delta[i];
                }
            }
            p
        };
        if positive != [0; 7] {
            mirror_opportunist_raises(state, target_slot, &positive, items_suppressed);
        }
    }

    // ── 4b. Defiant / Competitive: +2 per stat actually lowered ────────────────────────────
    // Each *distinct* stat that moved lower (after clamping) triggers once regardless of the
    // stage amount (e.g. Charm −2 Atk → one trigger; Memento −1 Atk −1 SpA → two triggers).
    let stats_lowered = delta.iter().filter(|&&d| d < 0).count() as i8;
    if stats_lowered > 0 && !target_suppressed {
        let reaction_idx: Option<usize> = match target_ability {
            Ability::Defiant => Some(0),     // +2 Attack per stat lowered
            Ability::Competitive => Some(2), // +2 Sp. Atk per stat lowered
            _ => None,
        };
        if let Some(idx) = reaction_idx {
            let def_comp_delta = {
                let Some(mon) = get_pokemon_at_slot_mut(state, target_slot) else {
                    return;
                };
                let mut boost = [0i8; 7];
                boost[idx] = 2 * stats_lowered;
                // Self-boost from Defiant/Competitive — use the non-deferring path (no White Herb
                // on a pure positive delta).
                apply_stat_boosts_to_pokemon(mon, &boost, items_suppressed, false)
            };
            // Emit BoostChanged for the Defiant/Competitive self-raise (mon borrow ended).
            for i in 0..7 {
                if def_comp_delta[i] != 0 {
                    emit(state, EventKind::BoostChanged { target: target_slot, boost_idx: i, stages: def_comp_delta[i] });
                }
            }
        }
    }

    // ── 5. White Herb (after Defiant/Competitive have potentially cancelled the negative) ───
    if let Some(mon) = get_pokemon_at_slot_mut(state, target_slot) {
        try_consume_white_herb(mon, items_suppressed);
    }
}

/// Apply weather or pseudo-weather effects.
/// `attacker_slot` is the setter's field slot; used to check for weather-extending rock items.
fn apply_weather_effects(state: &mut BattleState, effect: &HitEffect, attacker_slot: FieldSlot) {
    if let Some(weather) = &effect.weather {
        // Compute duration before mutably borrowing state.
        let dur = get_pokemon_at_slot(state, attacker_slot)
            .map(|m| weather_rock_duration(m, weather))
            .unwrap_or(5);
        set_weather(state, weather.clone(), dur);
    }

    if let Some(pseudo_weather) = &effect.pseudo_weather {
        let already_active = state
            .pseudo_weathers
            .iter()
            .any(|pw| std::mem::discriminant(pw) == std::mem::discriminant(pseudo_weather));

        // Trick Room, Wonder Room, and Magic Room toggle: re-using them while active cancels them.
        // Gravity and Fairy Lock do NOT toggle: re-use simply fails if already active.
        let is_toggleable = matches!(
            pseudo_weather,
            PseudoWeather::TrickRoom | PseudoWeather::WonderRoom
        );

        if already_active && is_toggleable {
            remove_pseudo_weather(state, pseudo_weather);
        } else if !already_active {
            let duration = get_pseudo_weather_duration(pseudo_weather);
            add_pseudo_weather(state, pseudo_weather.clone(), duration);
        }
        // If already_active && !is_toggleable: fail silently (no change).
    }
}

/// Apply terrain effects.
fn apply_terrain_effects(state: &mut BattleState, effect: &HitEffect) {
    if let Some(terrain) = &effect.terrain {
        set_terrain(state, terrain.clone(), 5);
    }
}

/// Opportunist: when an opposing Pokémon raises its stats, the holder copies those same
/// positive stage changes.
///
/// `raiser_slot` is the Pokémon whose stats just went up.  `raised_boosts` should be the
/// nominal intended boost (the delta applied to the raiser) so that Opportunist copies
/// the same stage count regardless of whether the raiser's stat was already capped.
/// Only positive entries are mirrored; negative entries (drops in the same array) are ignored.
/// Does NOT re-trigger Opportunist (no recursive call) — the copy is applied via a direct
/// `apply_stat_boosts_to_pokemon` call.
/// Opportunist does not activate on the holder's own boosts or on ally boosts (not called for
/// those paths).
fn mirror_opportunist_raises(
    state: &mut BattleState,
    raiser_slot: FieldSlot,
    raised_boosts: &[i8; 7],
    items_suppressed: bool,
) {
    // Extract positive portion only.
    let mut positive = [0i8; 7];
    if raised_boosts.iter().all(|&b| b <= 0) {
        return;
    }
    for i in 0..7 {
        if raised_boosts[i] > 0 {
            positive[i] = raised_boosts[i];
        }
    }

    let opp_player = match raiser_slot.player {
        Player::P1 => Player::P2,
        Player::P2 => Player::P1,
    };
    let opp_slots: Vec<FieldSlot> = collect_active_slots(state, opp_player, None);
    for slot in opp_slots {
        let has_opportunist = get_pokemon_at_slot(state, slot)
            .map(|m| {
                !m.fainted
                    && !pokemon_ability_is_suppressed(state, m)
                    && m.ability == Ability::Opportunist
            })
            .unwrap_or(false);
        if has_opportunist {
            let opp_delta = if let Some(mon) = get_pokemon_at_slot_mut(state, slot) {
                apply_stat_boosts_to_pokemon(mon, &positive, items_suppressed, false)
            } else { [0i8; 7] };
            for (boost_idx, &stages) in opp_delta.iter().enumerate() {
                if stages != 0 { emit(state, EventKind::BoostChanged { target: slot, boost_idx, stages }); }
            }
        }
    }
}

/// Apply all effects from a HitEffect to the target pokemon.
fn apply_effect_to_target(
    state: &mut BattleState,
    attacker_slot: FieldSlot,
    target_slot: FieldSlot,
    effect: &HitEffect,
    side_condition_player: Player,
) {
    // Extract attacker ability and Mold Breaker status before taking a mutable borrow.
    let attacker_ability = get_pokemon_at_slot(state, attacker_slot).map(|a| a.ability.clone());
    let attacker_mold_break =
        get_pokemon_at_slot(state, attacker_slot).map_or(false, |a| attacker_breaks_mold(state, a));
    // Snapshot target ability and suppression before the mutable borrow.
    let (target_ability, target_ability_suppressed) = get_pokemon_at_slot(state, target_slot)
        .map(|m| (m.ability.clone(), pokemon_ability_is_suppressed(state, m)))
        .unwrap_or((Ability::None, false));
    let sun_blocks_freeze = weather_is_sunlight(state);
    let items_suppressed = items_are_suppressed(state);
    let target_berry_env = berry_env(state, target_slot);
    let terrain_snapshot = state.clone();

    // Pre-compute veil protections before taking the mutable borrow.
    // Mold Breaker suppresses veil abilities on the holder (not side-wide), but the
    // side-wide protection (Sweet/Flower/Aroma Veil) still applies unless the veil
    // HOLDER is suppressed. In practice, the conservative approach is to skip the
    // veil gate when the attacker breaks mold, matching the Bulbapedia "ignorable" list.
    let sweet_veil_on_side =
        !attacker_mold_break && side_has_veil(state, target_slot.player, Ability::SweetVeil);
    let flower_veil_on_side =
        !attacker_mold_break && side_has_veil(state, target_slot.player, Ability::FlowerVeil);
    let aroma_veil_on_side =
        !attacker_mold_break && side_has_veil(state, target_slot.player, Ability::AromaVeil);
    // Snapshot target type for Flower Veil (Grass-only protection) before the mutable borrow.
    let target_is_grass = get_pokemon_at_slot(state, target_slot)
        .map_or(false, |mon| pokemon_has_type(mon, &PokemonType::Grass));
    // Safeguard: blocks status and confusion from opponents (unless attacker has Infiltrator).
    let safeguard_on_target_side = match target_slot.player {
        Player::P1 => &state.p1_side_conditions,
        Player::P2 => &state.p2_side_conditions,
    }
    .iter()
    .any(|c| matches!(c, SideCondition::SafeGuard));
    let attacker_has_infiltrator = get_pokemon_at_slot(state, attacker_slot).map_or(false, |a| {
        !pokemon_ability_is_suppressed(state, a) && a.ability == Ability::Infiltrator
    });
    // Light Clay: extend screen/veil duration from 5 to 8 turns.
    let attacker_has_light_clay = get_pokemon_at_slot(state, attacker_slot).map_or(false, |a| {
        item_is_active(state, a) && a.item == Item::LightClay
    });

    // Synchronize: snapshot pre-apply conditions so we can trigger after the mutable borrow.
    let synchronize_status_to_bounce: Option<Status> = if attacker_slot != target_slot {
        if let (Some(effect_status), Some(target)) =
            (&effect.status, get_pokemon_at_slot(state, target_slot))
        {
            let target_has_synchronize = !pokemon_ability_is_suppressed(state, target)
                && target.ability == Ability::Synchronize
                && target.status.is_none(); // will only land if target has no status
            if target_has_synchronize
                && matches!(
                    effect_status,
                    Status::Burn | Status::Paralysis | Status::Poison | Status::ToxicPoison(_)
                )
            {
                Some(effect_status.clone())
            } else {
                None
            }
        } else {
            None
        }
    } else {
        None
    };

    // Capture events to emit after the target_mon mutable borrow is released.
    let mut status_inflicted: Option<Status> = None;
    let mut volatile_started: Option<VolatileStatus> = None;
    let mut steadfast_boost_delta = [0i8; 7];
    let mut uproar_ended = false;
    let mut target_berry_cure = BerryCure::none();
    let mut target_mental_herb_cures: Vec<VolatileStatus> = Vec::new();

    if let Some(target_mon) = get_pokemon_at_slot_mut(state, target_slot) {
        if let Some(status) = &effect.status {
            // Sweet Veil: block sleep on the entire side (including Rest).
            let sleep_blocked_by_sweet_veil =
                matches!(status, Status::Sleep(_)) && sweet_veil_on_side;
            // Flower Veil: block all non-volatile status on Grass-type targets.
            // Rest / Flame Orb / Toxic Orb are not blocked (those go through separate paths).
            let status_blocked_by_flower_veil = flower_veil_on_side && target_is_grass;
            // Safeguard: block status from opponents (not self) unless Infiltrator.
            let status_blocked_by_safeguard = safeguard_on_target_side
                && !attacker_has_infiltrator
                && attacker_slot.player != target_slot.player;

            if !sleep_blocked_by_sweet_veil
                && !status_blocked_by_flower_veil
                && !status_blocked_by_safeguard
            {
                // If attacker has Corrosion, allow poisoning of Poison/Steel types,
                // but do not overwrite an existing non-volatile status on the target.
                if attacker_ability == Some(Ability::Corrosion) {
                    if target_mon.status.is_none() {
                        match status {
                            Status::Poison => {
                                target_mon.status = Some(Status::Poison);
                                status_inflicted = Some(Status::Poison);
                            }
                            Status::ToxicPoison(_) => {
                                target_mon.status = Some(Status::ToxicPoison(0));
                                status_inflicted = Some(Status::ToxicPoison(0));
                            }
                            other => {
                                let applied = apply_status_to_pokemon(
                                    &terrain_snapshot,
                                    sun_blocks_freeze,
                                    attacker_mold_break,
                                    target_mon,
                                    other,
                                );
                                if applied { status_inflicted = target_mon.status.clone(); }
                            }
                        }
                    }
                } else {
                    let applied = apply_status_to_pokemon(
                        &terrain_snapshot,
                        sun_blocks_freeze,
                        attacker_mold_break,
                        target_mon,
                        status,
                    );
                    if applied { status_inflicted = target_mon.status.clone(); }
                }
            }
        }

        if let Some(volatile) = &effect.volatile_status {
            // Sweet Veil: block Yawn on the target's side.
            // (Mold Breaker gate already applied to sweet_veil_on_side above.)
            let yawn_blocked_by_sweet_veil =
                matches!(volatile, VolatileStatus::Yawn) && sweet_veil_on_side;
            // Aroma Veil: block mental volatile statuses for the target's entire side.
            // Protects against Taunt, Torment, Encore, Disable, Attract, Heal Block.
            // Does NOT block Imprison (Bulbapedia explicitly excludes it).
            // (Mold Breaker gate already applied to aroma_veil_on_side above.)
            let aroma_veil_blocked = aroma_veil_on_side
                && matches!(
                    volatile,
                    VolatileStatus::Taunt
                        | VolatileStatus::Torment
                        | VolatileStatus::Encore(_)
                        | VolatileStatus::Disable(_)
                        | VolatileStatus::Attract
                        | VolatileStatus::HealBlock
                );
            // Safeguard: block Confusion and Yawn from opponents (not self) unless Infiltrator.
            let volatile_blocked_by_safeguard = safeguard_on_target_side
                && !attacker_has_infiltrator
                && attacker_slot.player != target_slot.player
                && matches!(volatile, VolatileStatus::Confusion | VolatileStatus::Yawn);
            // Oblivious: blocks Taunt and Attract individually (Mold Breaker bypasses this).
            let oblivious_blocks =
                matches!(volatile, VolatileStatus::Taunt | VolatileStatus::Attract)
                    && !attacker_mold_break
                    && !target_ability_suppressed
                    && target_ability == Ability::Oblivious;
            // Inner Focus: blocks flinching (Mold Breaker bypasses this).
            let inner_focus_blocks_flinch = matches!(volatile, VolatileStatus::Flinch)
                && !attacker_mold_break
                && !target_ability_suppressed
                && target_ability == Ability::InnerFocus;
            if !yawn_blocked_by_sweet_veil
                && !aroma_veil_blocked
                && !volatile_blocked_by_safeguard
                && !oblivious_blocks
                && !inner_focus_blocks_flinch
            {
                // Track before/after to detect newly added volatile (apply is a no-op on duplicates).
                let had_volatile = has_status_volatile(target_mon, volatile);
                target_mental_herb_cures = apply_volatile_to_pokemon(
                    &terrain_snapshot,
                    target_mon,
                    volatile,
                    attacker_mold_break,
                );
                if !had_volatile && has_status_volatile(target_mon, volatile) {
                    volatile_started = Some(volatile.clone());
                }

                // Steadfast: +1 Speed when the holder flinches.
                // The flinch volatile has just been pushed; fire immediately so the boost
                // lands in the same "apply flinch" moment (consistent with how the ability
                // resolves in-game before the flinched Pokémon would move).
                if matches!(volatile, VolatileStatus::Flinch)
                    && target_mon.ability == Ability::Steadfast
                    && !has_status_volatile(target_mon, &VolatileStatus::GastroAcid)
                {
                    steadfast_boost_delta = apply_stat_boosts_to_pokemon(
                        target_mon,
                        &[0, 0, 0, 0, 1, 0, 0],
                        items_suppressed,
                        false,
                    );
                }

                // Throat Chop: if the target was in the middle of an Uproar, it ends immediately.
                // (The ThroatChop volatile has just been applied; the Uproar self-lock must cease.)
                if matches!(volatile, VolatileStatus::ThroatChop) {
                    let had_uproar = has_status_volatile(target_mon, &VolatileStatus::Uproar);
                    remove_status_volatile(target_mon, &VolatileStatus::Uproar);
                    if had_uproar { uproar_ended = true; }
                }
            }
        }

        // After both status and volatile are applied, check status-cure berries.
        // A single call handles Aspear/Cheri/etc. (status just set) and Persim/Lum (confusion just pushed).
        target_berry_cure = try_consume_status_cure_berry(target_mon, &target_berry_env);
    }

    // Emit captured events (target_mon borrow released above; NLL safe).
    if uproar_ended {
        emit(state, EventKind::VolatileEnd { target: target_slot, volatile: VolatileStatus::Uproar });
    }
    if let Some(status) = status_inflicted {
        emit(state, EventKind::StatusInflicted { target: target_slot, status });
    }
    if let Some(v) = volatile_started {
        if matches!(v, VolatileStatus::Flinch) {
            // Flinch is NOT announced at hit time — the observer only learns of it when the
            // flinched Pokémon fails to act (EventKind::Cant { reason: CantReason::Flinch }).
            // Emitting VolatileStart here would reveal which attacker's move caused the
            // flinch, which is wrong in doubles (two unknown opponents; can't tell who holds
            // King's Rock).  The inference engine attributes the flinch cause from Cant instead.
            //
            // Steadfast's +1 Spe boost is still game-visible; emit it flat (not nested under
            // any attacker-attributed event, since the boost is on the defender).
            // Mental Herb never cures Flinch, but route those events the same way defensively.
            for i in 0..7 {
                if steadfast_boost_delta[i] != 0 {
                    emit(state, EventKind::BoostChanged { target: target_slot, boost_idx: i, stages: steadfast_boost_delta[i] });
                }
            }
            for mv in &target_mental_herb_cures {
                emit(state, EventKind::VolatileEnd { target: target_slot, volatile: mv.clone() });
            }
            if !target_mental_herb_cures.is_empty() {
                emit(state, EventKind::ItemLost { slot: target_slot, item: Item::MentalHerb, consumed: true });
            }
        } else {
            // For all other volatiles, Steadfast's Speed boost and Mental Herb cures nest
            // under VolatileStart (the volatile application caused them both).
            with_reactions(state, EventKind::VolatileStart { target: target_slot, volatile: v }, |bs| {
                for i in 0..7 {
                    if steadfast_boost_delta[i] != 0 {
                        emit(bs, EventKind::BoostChanged { target: target_slot, boost_idx: i, stages: steadfast_boost_delta[i] });
                    }
                }
                for mv in &target_mental_herb_cures {
                    emit(bs, EventKind::VolatileEnd { target: target_slot, volatile: mv.clone() });
                }
                if !target_mental_herb_cures.is_empty() {
                    emit(bs, EventKind::ItemLost { slot: target_slot, item: Item::MentalHerb, consumed: true });
                }
            });
        }
    } else if steadfast_boost_delta != [0i8; 7] {
        // Steadfast boost without newly-started flinch (shouldn't happen, but guard anyway).
        for i in 0..7 {
            if steadfast_boost_delta[i] != 0 {
                emit(state, EventKind::BoostChanged { target: target_slot, boost_idx: i, stages: steadfast_boost_delta[i] });
            }
        }
    }
    // Berry-cure events (status/confusion cleared by a held berry).
    emit_berry_cure(state, target_slot, &target_berry_cure);

    // Synchronize: after status is applied to the target, bounce Burn/Paralysis/Poison/Toxic
    // back at the source. The bounce goes through the normal apply_effect_to_target path so
    // type/ability/Safeguard immunities are respected on the source. The `from_synchronize`
    // flag prevents recursive ping-pong (the second call will not see a Synchronize target
    // because the source has no Synchronize context — we simply call apply_status_to_pokemon
    // directly here to skip another full apply_effect_to_target call).
    if let Some(synch_status) = synchronize_status_to_bounce {
        // Verify the status actually landed (was not blocked by type immunity etc.)
        let status_landed = get_pokemon_at_slot(state, target_slot)
            .map_or(false, |t| matches!(&t.status, Some(s) if std::mem::discriminant(s) == std::mem::discriminant(&synch_status)));
        if status_landed {
            // Apply the same status to the source, going through type/ability checks.
            let synch_effect = HitEffect {
                status: Some(synch_status),
                ..Default::default()
            };
            // Use target_slot as the "attacker" so the source is the "target" — this means
            // Safeguard on the source's side is checked, type immunity is checked, etc.
            apply_effect_to_target(
                state,
                target_slot,
                attacker_slot,
                &synch_effect,
                attacker_slot.player,
            );
        }
    }

    // Handle opponent-sourced stat changes OUTSIDE the target_mon mutable borrow so that
    // apply_opponent_stat_drop can take &mut BattleState freely (needed for Mirror Armor
    // recursion onto source_slot and for Defiant/Competitive self-boost application).
    // attacker_slot is already a parameter and all needed locals were snapshotted above.
    if effect.boosts != [0; 7] {
        let mut incoming = effect.boosts;
        // Flower Veil: zero all opponent-sourced stat drops on Grass-type targets.
        // Self-inflicted drops (Leaf Storm, Weak Armor, etc.) go through apply_effect_to_attacker,
        // not this path, so they are correctly unaffected.
        // (Mold Breaker gate already applied to flower_veil_on_side above.)
        if flower_veil_on_side && target_is_grass {
            for b in &mut incoming {
                if *b < 0 {
                    *b = 0;
                }
            }
        }
        if incoming != [0; 7] {
            apply_opponent_stat_drop(
                state,
                target_slot,
                attacker_slot,
                incoming,
                items_suppressed,
                false,
            );
        }
    }

    if let Some(side_condition) = &effect.side_condition {
        if !(matches!(side_condition, SideCondition::AuroraVeil) && !weather_is_snow(state)) {
            // Sticky Web records the `mon_id` of its setter so Mirror Armor can later reflect the
            // Speed drop back to that specific Pokémon (and to nobody if it has left the field).
            let to_add = match side_condition {
                SideCondition::StickyWeb(_) => {
                    let setter_id = get_pokemon_at_slot(state, attacker_slot).map(|m| m.mon_id);
                    SideCondition::StickyWeb(setter_id)
                }
                other => other.clone(),
            };
            let duration = {
                let base = get_side_condition_duration(&to_add);
                // Light Clay extends Reflect / Light Screen / Aurora Veil from 5 to 8 turns.
                if matches!(
                    &to_add,
                    SideCondition::Reflect | SideCondition::LightScreen | SideCondition::AuroraVeil
                ) && base == 5
                    && attacker_has_light_clay
                {
                    8
                } else {
                    base
                }
            };
            add_side_condition(state, side_condition_player, to_add, duration);
        }
    }

    apply_weather_effects(state, effect, attacker_slot);
    apply_terrain_effects(state, effect);
}

/// Apply all effects from a HitEffect to the attacker pokemon.
fn apply_effect_to_attacker(state: &mut BattleState, attacker_slot: FieldSlot, effect: &HitEffect) {
    let sun_blocks_freeze = weather_is_sunlight(state);
    let items_suppressed = items_are_suppressed(state);
    let attacker_berry_env = berry_env(state, attacker_slot);
    let terrain_snapshot = state.clone();
    // Capture events to emit after the attacker_mon borrow is released.
    let mut self_status_inflicted: Option<Status> = None;
    let mut self_volatile_started: Option<VolatileStatus> = None;
    let mut self_boost_delta = [0i8; 7];
    let mut self_berry_cure = BerryCure::none();
    let mut self_mental_herb_cures: Vec<VolatileStatus> = Vec::new();

    if let Some(attacker_mon) = get_pokemon_at_slot_mut(state, attacker_slot) {
        if let Some(status) = &effect.status {
            let applied = apply_status_to_pokemon(
                &terrain_snapshot,
                sun_blocks_freeze,
                false,
                attacker_mon,
                status,
            );
            if applied { self_status_inflicted = attacker_mon.status.clone(); }
        }

        if let Some(volatile) = &effect.volatile_status {
            let had = has_status_volatile(attacker_mon, volatile);
            self_mental_herb_cures = apply_volatile_to_pokemon(&terrain_snapshot, attacker_mon, volatile, false);
            if !had && has_status_volatile(attacker_mon, volatile) {
                self_volatile_started = Some(volatile.clone());
            }
        }

        // After both status and volatile are applied, check status-cure berries.
        // Covers self-inflicted confusion (e.g. Outrage rampaging) and self-status moves.
        self_berry_cure = try_consume_status_cure_berry(attacker_mon, &attacker_berry_env);

        if effect.boosts != [0; 7] {
            self_boost_delta = apply_stat_boosts_to_pokemon(attacker_mon, &effect.boosts, items_suppressed, false);
        }
    }

    // Emit captured events (attacker_mon borrow released; NLL safe).
    if let Some(status) = self_status_inflicted {
        emit(state, EventKind::StatusInflicted { target: attacker_slot, status });
    }
    if let Some(v) = self_volatile_started {
        // Flinch is never self-applied, but guard defensively: don't emit VolatileStart{Flinch}
        // even on the self-target path (see apply_effect_to_target for the full rationale).
        if !matches!(v, VolatileStatus::Flinch) {
            // Mental Herb cures nest under the VolatileStart (the volatile application caused them).
            with_reactions(state, EventKind::VolatileStart { target: attacker_slot, volatile: v }, |bs| {
                for mv in &self_mental_herb_cures {
                    emit(bs, EventKind::VolatileEnd { target: attacker_slot, volatile: mv.clone() });
                }
                if !self_mental_herb_cures.is_empty() {
                    emit(bs, EventKind::ItemLost { slot: attacker_slot, item: Item::MentalHerb, consumed: true });
                }
            });
        }
    }
    for i in 0..7 {
        if self_boost_delta[i] != 0 {
            emit(state, EventKind::BoostChanged { target: attacker_slot, boost_idx: i, stages: self_boost_delta[i] });
        }
    }
    // Berry-cure events (status/confusion cleared by a held berry).
    emit_berry_cure(state, attacker_slot, &self_berry_cure);

    // Opportunist: mirror any positive self-boost the attacker just applied to opponents
    // who hold Opportunist.  Negative drops (Leaf Storm −2 SpA, etc.) are ignored.
    if effect.boosts.iter().any(|&b| b > 0) {
        mirror_opportunist_raises(state, attacker_slot, &effect.boosts, items_suppressed);
    }

    if let Some(side_condition) = &effect.side_condition {
        if !(matches!(side_condition, SideCondition::AuroraVeil) && !weather_is_snow(state)) {
            let attacker_has_light_clay = get_pokemon_at_slot(state, attacker_slot)
                .map_or(false, |a| {
                    item_is_active(state, a) && a.item == Item::LightClay
                });
            let duration = {
                let base = get_side_condition_duration(side_condition);
                // Light Clay extends Reflect / Light Screen / Aurora Veil from 5 to 8 turns.
                if matches!(
                    side_condition,
                    SideCondition::Reflect | SideCondition::LightScreen | SideCondition::AuroraVeil
                ) && base == 5
                    && attacker_has_light_clay
                {
                    8
                } else {
                    base
                }
            };
            add_side_condition(
                state,
                attacker_slot.player,
                side_condition.clone(),
                duration,
            );
        }
    }

    apply_weather_effects(state, effect, attacker_slot);
    apply_terrain_effects(state, effect);
}

/// Branch every existing `branches` state into a "miss" branch plus one branch per
/// effect in `choices`, which are chosen uniformly at random when the secondary
/// fires. `apply_fn` applies a single chosen effect to a state.
///
/// With a single choice this is an ordinary chance roll. With several choices it
/// keeps the chance roll and the random-selection roll as *separate* branches
/// (e.g. Tri Attack: 80% nothing, ~6.67% each of burn/freeze/paralyze).
fn branch_on_secondary_effects<F>(
    branches: Vec<(BattleState, f64)>,
    chance: f64,
    choices: &[HitEffect],
    mut apply_fn: F,
) -> Vec<(BattleState, f64)>
where
    F: FnMut(&mut BattleState, &HitEffect),
{
    if choices.is_empty() {
        return branches;
    }
    let per_choice = chance / choices.len() as f64;
    let mut new_branches = Vec::new();
    for (bs, prob) in branches {
        if 1.0 - chance > 0.0 {
            new_branches.push((bs.clone(), prob * (1.0 - chance)));
        }
        if per_choice > 0.0 {
            for choice in choices {
                let mut applied = bs.clone();
                apply_fn(&mut applied, choice);
                new_branches.push((applied, prob * per_choice));
            }
        }
    }
    new_branches
}

/// Apply a healing/recovery move effect to the attacker in-place.
fn apply_healing_move(
    bs: &mut BattleState,
    attacker_slot: FieldSlot,
    move_name: &PokemonMove,
    terrain_snapshot: &BattleState,
) -> bool {
    // Use weather_for so Mega Sol counts as sun for the move user's recovery.
    let attacker_snapshot = get_pokemon_at_slot(bs, attacker_slot).cloned();
    let branch_weather = attacker_snapshot
        .as_ref()
        .map(|m| weather_for(bs, m))
        .unwrap_or_else(|| current_weather(bs));
    let branch_sun = matches!(
        branch_weather,
        Some(Weather::Sun | Weather::ExtremeSunlight)
    );
    let branch_sandstorm = matches!(branch_weather, Some(Weather::Sandstorm));
    let env = berry_env(bs, attacker_slot); // compute before the mutable borrow below

    // Capture HP before the mutable borrow so we can compute the delta for Healed emission.
    let pre_hp = get_pokemon_at_slot(bs, attacker_slot).map(|m| m.hp).unwrap_or(0);
    let mut rest_slept = false;

    let Some(attacker_mon) = get_pokemon_at_slot_mut(bs, attacker_slot) else {
        return false;
    };

    match move_name {
        PokemonMove::Rest => {
            if pokemon_is_on_terrain(terrain_snapshot, attacker_mon, &Terrain::ElectricTerrain) {
                return false;
            }
            // Insomnia / Vital Spirit cannot fall asleep, so Rest fails outright (no heal).
            // NOTE: this Rest path sets Sleep directly rather than going through
            // apply_status_to_pokemon, so Sweet Veil and the full-HP/already-asleep fail
            // conditions are also bypassed here — worth a follow-up.
            if !pokemon_ability_is_suppressed(terrain_snapshot, attacker_mon)
                && matches!(
                    attacker_mon.ability,
                    Ability::Insomnia | Ability::VitalSpirit
                )
            {
                return false;
            }
            attacker_mon.volatiles.clear();
            attacker_mon.status = Some(Status::Sleep(0));
            rest_slept = true;
            let max_hp = attacker_mon.stats[0].max(1);
            let heal = max_hp.saturating_sub(attacker_mon.hp);
            gain_hp(attacker_mon, heal, env);
        }
        PokemonMove::Synthesis | PokemonMove::MorningSun | PokemonMove::Moonlight => {
            let (num, den) = if branch_sun {
                (2u32, 3u32)
            } else if matches!(branch_weather, None | Some(Weather::StrongWinds)) {
                (1u32, 2u32)
            } else {
                (1u32, 4u32)
            };
            let max_hp = attacker_mon.stats[0].max(1);
            let heal = ((max_hp as u32 * num) / den) as u16;
            gain_hp(attacker_mon, heal, env);
        }
        PokemonMove::ShoreUp => {
            let (num, den) = if branch_sandstorm {
                (2u32, 3u32)
            } else {
                (1u32, 4u32)
            };
            let max_hp = attacker_mon.stats[0].max(1);
            let heal = ((max_hp as u32 * num) / den) as u16;
            gain_hp(attacker_mon, heal, env);
        }
        _ => return false,
    }
    // attacker_mon borrow ends here — emit events now that &mut BattleState is free.
    // Rest: StatusInflicted precedes Healed (sleep comes before the recovery completes).
    if rest_slept {
        emit(bs, EventKind::StatusInflicted { target: attacker_slot, status: Status::Sleep(0) });
    }
    let post_hp = get_pokemon_at_slot(bs, attacker_slot).map(|m| m.hp).unwrap_or(0);
    if post_hp > pre_hp {
        if let Some(observer) = bs.event_observer {
            let new_hp = observed_hp(bs, attacker_slot, observer);
            emit(bs, EventKind::Healed { target: attacker_slot, new_hp });
        }
    }
    true
}

/// Remove all four entry hazards from one side of the field. Removal is by discriminant, so the
/// payloads passed here (layer count / setter id) are placeholders and irrelevant.
fn clear_entry_hazards(bs: &mut BattleState, player: Player) {
    remove_side_condition(bs, player, &SideCondition::Spikes(0));
    remove_side_condition(bs, player, &SideCondition::StealthRock);
    remove_side_condition(bs, player, &SideCondition::StickyWeb(None));
    remove_side_condition(bs, player, &SideCondition::ToxicSpikes(0));
}

/// Apply the hazard/substitute *clearing* side effects of the spin and Defog/Tidy Up moves.
/// The accompanying stat changes (Rapid Spin +1 Spe, Tidy Up +1 Atk/Spe), Defog's −1 evasion,
/// and Mortal Spin's poison are ordinary parsed effects handled elsewhere — this only clears.
fn apply_hazard_removal_move(
    bs: &mut BattleState,
    attacker_slot: FieldSlot,
    move_name: &PokemonMove,
) {
    let user = attacker_slot.player;
    let foe = match user {
        Player::P1 => Player::P2,
        Player::P2 => Player::P1,
    };
    match move_name {
        PokemonMove::RapidSpin | PokemonMove::MortalSpin => {
            // Clear the user's own side and free the user from binding + Leech Seed.
            clear_entry_hazards(bs, user);
            let had_trap = get_pokemon_at_slot(bs, attacker_slot)
                .map(|m| has_status_volatile(m, &VolatileStatus::PartiallyTrapped(0)))
                .unwrap_or(false);
            let had_leech = get_pokemon_at_slot(bs, attacker_slot)
                .map(|m| has_status_volatile(m, &VolatileStatus::LeechSeed))
                .unwrap_or(false);
            if let Some(mon) = get_pokemon_at_slot_mut(bs, attacker_slot) {
                remove_status_volatile(mon, &VolatileStatus::PartiallyTrapped(0));
                remove_status_volatile(mon, &VolatileStatus::LeechSeed);
            }
            if had_trap {
                emit(bs, EventKind::VolatileEnd { target: attacker_slot, volatile: VolatileStatus::PartiallyTrapped(0) });
            }
            if had_leech {
                emit(bs, EventKind::VolatileEnd { target: attacker_slot, volatile: VolatileStatus::LeechSeed });
            }
        }
        PokemonMove::Defog => {
            // Defog's −1 evasion (target) lives in Showdown's onHit JS, so it isn't parsed —
            // apply it here to the directly-opposing foe, unless that foe is behind a Substitute.
            let foe_target = FieldSlot {
                player: foe,
                slot_index: attacker_slot.slot_index,
            };
            let blocked_by_sub = get_pokemon_at_slot(bs, foe_target)
                .map(|m| has_status_volatile(m, &VolatileStatus::Substitute(0)))
                .unwrap_or(true);
            if !blocked_by_sub {
                let items_suppressed = items_are_suppressed(bs);
                apply_opponent_stat_drop(
                    bs,
                    foe_target,
                    attacker_slot,
                    [0, 0, 0, 0, 0, 0, -1],
                    items_suppressed,
                    false,
                );
            }
            // Hazards clear from both sides; screens clear from the target's (foe) side; terrain ends.
            clear_entry_hazards(bs, user);
            clear_entry_hazards(bs, foe);
            for sc in [
                SideCondition::Reflect,
                SideCondition::LightScreen,
                SideCondition::AuroraVeil,
                SideCondition::SafeGuard,
                SideCondition::Mist,
            ] {
                remove_side_condition(bs, foe, &sc);
            }
            bs.terrain = None;
            bs.terrain_turns = None;
        }
        PokemonMove::TidyUp => {
            // Hazards and Substitutes clear from both sides.
            clear_entry_hazards(bs, user);
            clear_entry_hazards(bs, foe);
            for mon in bs
                .p1_active_mons
                .iter_mut()
                .chain(bs.p1_back_mons.iter_mut())
                .chain(bs.p2_active_mons.iter_mut())
                .chain(bs.p2_back_mons.iter_mut())
            {
                remove_status_volatile(mon, &VolatileStatus::Substitute(0));
            }
            // Tidy Up's +1 Atk / +1 Spe live in Showdown's onHit JS, so they aren't parsed into
            // `self_boost` — apply them here.
            let items_suppressed = items_are_suppressed(bs);
            if let Some(mon) = get_pokemon_at_slot_mut(bs, attacker_slot) {
                apply_stat_boosts_to_pokemon(mon, &[1, 0, 0, 0, 1, 0, 0], items_suppressed, false);
            }
        }
        PokemonMove::BrickBreak | PokemonMove::PsychicFangs | PokemonMove::RagingBull => {
            // Remove Reflect, Light Screen, and Aurora Veil from the target's (foe) side.
            // This is the post-damage clear; the bypass of screen reduction on this same hit
            // is handled in screen_damage_multiplier. Removal happens even if the target was
            // behind a Substitute (the move still "hits the side" per current-gen Showdown).
            for sc in [
                SideCondition::Reflect,
                SideCondition::LightScreen,
                SideCondition::AuroraVeil,
            ] {
                remove_side_condition(bs, foe, &sc);
            }
        }
        _ => {}
    }
}

/// Apply a King's Rock (or Razor Fang) flinch once per move, using the combined
/// probability across all connecting strikes: P(flinch) = 1 - 0.9^hits_landed.
///
/// Called *after* all per-hit branches for a target are resolved, so we never
/// fork the tree once-per-strike.  Returns `branches` unchanged if the move is
/// ineligible (status move, move already flinches, 0 hits, items suppressed,
/// holder doesn't carry King's Rock).
///
/// Serene Grace would double the per-hit rate to 20% (combined: 1 - 0.8^n), but
/// Serene Grace is not yet implemented; add it here when that ability is handled.
pub fn apply_kings_rock_flinch(
    branches: Vec<(BattleState, f64)>,
    attacker_slot: FieldSlot,
    target_slot: FieldSlot,
    move_data: &MoveData,
    hits_landed: u32,
) -> Vec<(BattleState, f64)> {
    if hits_landed == 0 {
        return branches;
    }
    if matches!(move_data.category, MoveCategory::Status) {
        return branches;
    }

    // Skip if the move already has a flinch secondary (don't double-dip).
    let move_already_flinches = move_data.secondaries.iter().any(|sec| {
        sec.effect.volatile_status == Some(VolatileStatus::Flinch)
            || sec
                .random_choices
                .iter()
                .any(|c| c.volatile_status == Some(VolatileStatus::Flinch))
    });
    if move_already_flinches {
        return branches;
    }

    // Check that the attacker holds King's Rock and its item is active (Magic Room / Klutz).
    let eligible = branches.first().map_or(false, |(bs, _)| {
        get_pokemon_at_slot(bs, attacker_slot).map_or(false, |m| {
            item_is_active(bs, m) && (m.item == Item::KingsRock || m.item == Item::RazorFang)
        })
    });
    if !eligible {
        return branches;
    }

    // Shield Dust / Covert Cloak on the target blocks King's Rock flinch.
    let blocked_by_shield_dust = branches.first().map_or(false, |(bs, _)| {
        let attacker_breaks =
            get_pokemon_at_slot(bs, attacker_slot).map_or(false, |a| attacker_breaks_mold(bs, a));
        !attacker_breaks
            && get_pokemon_at_slot(bs, target_slot).map_or(false, |m| {
                !pokemon_ability_is_suppressed(bs, m) && m.ability == Ability::ShieldDust
            })
    });
    if blocked_by_shield_dust {
        return branches;
    }

    let chance = 1.0 - 0.9_f64.powi(hits_landed as i32);
    let flinch_effect = HitEffect {
        volatile_status: Some(VolatileStatus::Flinch),
        ..Default::default()
    };
    // side_condition_player is unused by the flinch path in apply_effect_to_target.
    let side_condition_player = target_slot.player;
    branch_on_secondary_effects(
        branches,
        chance,
        std::slice::from_ref(&flinch_effect),
        |bs, eff| {
            apply_effect_to_target(bs, attacker_slot, target_slot, eff, side_condition_player);
        },
    )
}

/// Stench: add a 10% flinch chance to any damaging move that doesn't already flinch.
/// Each hit of a multi-hit move rolls independently (same structure as `apply_kings_rock_flinch`).
/// Does not stack with King's Rock / Razor Fang.
pub fn apply_stench_flinch(
    branches: Vec<(BattleState, f64)>,
    attacker_slot: FieldSlot,
    target_slot: FieldSlot,
    move_data: &MoveData,
    hits_landed: u32,
) -> Vec<(BattleState, f64)> {
    if hits_landed == 0 {
        return branches;
    }
    if matches!(move_data.category, MoveCategory::Status) {
        return branches;
    }

    // Skip if the move already has a flinch secondary (don't double-dip).
    let move_already_flinches = move_data.secondaries.iter().any(|sec| {
        sec.effect.volatile_status == Some(VolatileStatus::Flinch)
            || sec
                .random_choices
                .iter()
                .any(|c| c.volatile_status == Some(VolatileStatus::Flinch))
    });
    if move_already_flinches {
        return branches;
    }

    // Check that the attacker has Stench and ability is active (not suppressed).
    let eligible = branches.first().map_or(false, |(bs, _)| {
        get_pokemon_at_slot(bs, attacker_slot).map_or(false, |m| {
            !pokemon_ability_is_suppressed(bs, m) && m.ability == Ability::Stench
        })
    });
    if !eligible {
        return branches;
    }

    // Shield Dust on the target blocks Stench flinch (same as King's Rock).
    let blocked_by_shield_dust = branches.first().map_or(false, |(bs, _)| {
        let attacker_breaks =
            get_pokemon_at_slot(bs, attacker_slot).map_or(false, |a| attacker_breaks_mold(bs, a));
        !attacker_breaks
            && get_pokemon_at_slot(bs, target_slot).map_or(false, |m| {
                !pokemon_ability_is_suppressed(bs, m) && m.ability == Ability::ShieldDust
            })
    });
    if blocked_by_shield_dust {
        return branches;
    }

    // 10% per-hit chance, independent rolls (1 - 0.9^n combined probability).
    let chance = 1.0 - 0.9_f64.powi(hits_landed as i32);
    let flinch_effect = HitEffect {
        volatile_status: Some(VolatileStatus::Flinch),
        ..Default::default()
    };
    let side_condition_player = target_slot.player;
    branch_on_secondary_effects(
        branches,
        chance,
        std::slice::from_ref(&flinch_effect),
        |bs, eff| {
            apply_effect_to_target(bs, attacker_slot, target_slot, eff, side_condition_player);
        },
    )
}

// ── On-contact / on-hit reactive abilities ───────────────────────────────────

fn ability_excluded_from_mummy(ability: &Ability) -> bool {
    matches!(
        ability,
        Ability::AsOneGlastrier
            | Ability::AsOneSpectrier
            | Ability::BattleBond
            | Ability::Comatose
            | Ability::Commander
            | Ability::Disguise
            | Ability::GulpMissile
            | Ability::IceFace
            | Ability::LingeringAroma
            | Ability::Multitype
            | Ability::PowerConstruct
            | Ability::RKSSystem
            | Ability::Schooling
            | Ability::ShieldsDown
            | Ability::StanceChange
            | Ability::ZenMode
            | Ability::ZerotoHero
            | Ability::Mummy
    )
}

/// Blocklist for Receiver — abilities that cannot be inherited from a fainted ally.
fn ability_cannot_be_received(ability: &Ability) -> bool {
    matches!(
        ability,
        Ability::AsOneGlastrier
            | Ability::AsOneSpectrier
            | Ability::BattleBond
            | Ability::Comatose
            | Ability::Commander
            | Ability::Disguise
            | Ability::FlowerGift
            | Ability::Forecast
            | Ability::GulpMissile
            | Ability::HungerSwitch
            | Ability::IceFace
            | Ability::Illusion
            | Ability::Imposter
            | Ability::Multitype
            | Ability::PowerConstruct
            | Ability::PowerofAlchemy
            | Ability::Protosynthesis
            | Ability::QuarkDrive
            | Ability::Receiver
            | Ability::RKSSystem
            | Ability::Schooling
            | Ability::ShieldsDown
            | Ability::StanceChange
            | Ability::Trace
            | Ability::WanderingSpirit
            | Ability::WonderGuard
            | Ability::ZenMode
            | Ability::ZerotoHero
    )
}

/// Gen IX blocklist for Trace — abilities that cannot be copied.
fn ability_cannot_be_traced(ability: &Ability) -> bool {
    matches!(
        ability,
        Ability::AsOneGlastrier
            | Ability::AsOneSpectrier
            | Ability::BattleBond
            | Ability::Comatose
            | Ability::Commander
            | Ability::Disguise
            | Ability::EmbodyAspectCornerstone
            | Ability::EmbodyAspectHearthflame
            | Ability::EmbodyAspectTeal
            | Ability::EmbodyAspectWellspring
            | Ability::FlowerGift
            | Ability::Forecast
            | Ability::GulpMissile
            | Ability::HungerSwitch
            | Ability::IceFace
            | Ability::Illusion
            | Ability::Imposter
            | Ability::Multitype
            | Ability::NeutralizingGas
            | Ability::PoisonPuppeteer
            | Ability::PowerConstruct
            | Ability::PowerofAlchemy
            | Ability::Protosynthesis
            | Ability::QuarkDrive
            | Ability::Receiver
            | Ability::RKSSystem
            | Ability::Schooling
            | Ability::ShieldsDown
            | Ability::StanceChange
            | Ability::TeraShell
            | Ability::TeraShift
            | Ability::TeraformZero
            | Ability::Trace
            | Ability::ZenMode
            | Ability::ZerotoHero
    )
}

fn ability_excluded_from_wandering_spirit(ability: &Ability) -> bool {
    matches!(
        ability,
        Ability::AsOneGlastrier
            | Ability::AsOneSpectrier
            | Ability::BattleBond
            | Ability::Comatose
            | Ability::Commander
            | Ability::Disguise
            | Ability::FlowerGift
            | Ability::Forecast
            | Ability::GulpMissile
            | Ability::HungerSwitch
            | Ability::IceFace
            | Ability::Illusion
            | Ability::Imposter
            | Ability::Multitype
            | Ability::NeutralizingGas
            | Ability::PowerofAlchemy
            | Ability::Receiver
            | Ability::RKSSystem
            | Ability::Schooling
            | Ability::ShieldsDown
            | Ability::StanceChange
            | Ability::WonderGuard
            | Ability::ZenMode
            | Ability::ZerotoHero
    )
}

/// Abilities with the `cantsuppress` flag — cannot be replaced, suppressed, or given away
/// by Gastro Acid, Worry Seed, Simple Beam, or as the target of Entrainment.
pub(crate) fn ability_cannot_be_suppressed(ability: &Ability) -> bool {
    matches!(
        ability,
        Ability::AsOneGlastrier
            | Ability::AsOneSpectrier
            | Ability::BattleBond
            | Ability::Comatose
            | Ability::Commander
            | Ability::Disguise
            | Ability::GulpMissile
            | Ability::IceFace
            | Ability::Multitype
            | Ability::PowerConstruct
            | Ability::RKSSystem
            | Ability::Schooling
            | Ability::ShieldsDown
            | Ability::StanceChange
            | Ability::ZerotoHero
    )
}

/// Abilities with the `failskillswap` flag — neither side may have these for Skill Swap.
pub(crate) fn ability_excluded_from_skill_swap(ability: &Ability) -> bool {
    matches!(
        ability,
        Ability::AsOneGlastrier
            | Ability::AsOneSpectrier
            | Ability::BattleBond
            | Ability::Comatose
            | Ability::Commander
            | Ability::Disguise
            | Ability::GulpMissile
            | Ability::HadronEngine
            | Ability::HungerSwitch
            | Ability::IceFace
            | Ability::Illusion
            | Ability::Multitype
            | Ability::NeutralizingGas
            | Ability::OrichalcumPulse
            | Ability::PoisonPuppeteer
            | Ability::PowerConstruct
            | Ability::Protosynthesis
            | Ability::QuarkDrive
            | Ability::RKSSystem
            | Ability::Schooling
            | Ability::ShieldsDown
            | Ability::StanceChange
            | Ability::TeraShell
            | Ability::TeraShift
            | Ability::TeraformZero
            | Ability::WonderGuard
            | Ability::ZenMode
            | Ability::ZerotoHero
    )
}

/// Abilities with the `failroleplay` flag — the TARGET cannot have these for Role Play.
pub(crate) fn ability_cannot_be_role_played(ability: &Ability) -> bool {
    matches!(
        ability,
        Ability::AsOneGlastrier
            | Ability::AsOneSpectrier
            | Ability::BattleBond
            | Ability::Comatose
            | Ability::Commander
            | Ability::Disguise
            | Ability::FlowerGift
            | Ability::Forecast
            | Ability::GulpMissile
            | Ability::IceFace
            | Ability::Illusion
            | Ability::Imposter
            | Ability::Multitype
            | Ability::NeutralizingGas
            | Ability::PowerConstruct
            | Ability::PowerofAlchemy
            | Ability::Protosynthesis
            | Ability::QuarkDrive
            | Ability::Receiver
            | Ability::RKSSystem
            | Ability::Schooling
            | Ability::ShieldsDown
            | Ability::StanceChange
            | Ability::Trace
            | Ability::WonderGuard
            | Ability::ZenMode
            | Ability::ZerotoHero
    )
}

/// Abilities with the `noentrain` flag — the USER cannot have these for Entrainment.
pub(crate) fn ability_excluded_from_entrainment_user(ability: &Ability) -> bool {
    matches!(
        ability,
        Ability::AsOneGlastrier
            | Ability::AsOneSpectrier
            | Ability::Commander
            | Ability::Disguise
            | Ability::FlowerGift
            | Ability::Forecast
            | Ability::GulpMissile
            | Ability::HungerSwitch
            | Ability::IceFace
            | Ability::Illusion
            | Ability::Imposter
            | Ability::NeutralizingGas
            | Ability::PowerConstruct
            | Ability::PowerofAlchemy
            | Ability::Receiver
            | Ability::Trace
            | Ability::ZerotoHero
    )
}

/// Return true if infatuation can be applied from `source_slot` to `target_slot`.
/// Requires opposite, non-genderless genders; target must not already be Attracted or Oblivious.
pub fn can_be_infatuated(
    state: &BattleState,
    source_slot: FieldSlot,
    target_slot: FieldSlot,
) -> bool {
    use crate::state::pokemon::PokemonGender;
    let (sg, tg, tab, already) = {
        let Some(src) = get_pokemon_at_slot(state, source_slot) else {
            return false;
        };
        let Some(tgt) = get_pokemon_at_slot(state, target_slot) else {
            return false;
        };
        (
            src.gender,
            tgt.gender,
            tgt.ability.clone(),
            has_status_volatile(tgt, &VolatileStatus::Attract),
        )
    };
    if matches!(sg, PokemonGender::Genderless) || matches!(tg, PokemonGender::Genderless) {
        return false;
    }
    if sg == tg {
        return false;
    }
    if already {
        return false;
    }
    if tab == Ability::Oblivious {
        return false;
    }
    true
}

/// Apply the Attract volatile from `source_slot` to `target_slot`. Returns true if applied.
pub fn try_apply_attract(
    state: &mut BattleState,
    source_slot: FieldSlot,
    target_slot: FieldSlot,
) -> bool {
    if !can_be_infatuated(state, source_slot, target_slot) {
        return false;
    }
    let effect = HitEffect {
        volatile_status: Some(VolatileStatus::Attract),
        ..Default::default()
    };
    apply_effect_to_target(state, source_slot, target_slot, &effect, target_slot.player);
    true
}

/// Apply Disable to `target_slot` using its `last_used_move`. Returns true if applied.
pub fn try_apply_disable(
    state: &mut BattleState,
    source_slot: FieldSlot,
    target_slot: FieldSlot,
) -> bool {
    let (last_move, already_disabled) = {
        let Some(tgt) = get_pokemon_at_slot(state, target_slot) else {
            return false;
        };
        let m = match &tgt.last_used_move {
            Some(mv) if *mv != PokemonMove::Struggle => mv.clone(),
            _ => return false,
        };
        let disabled = has_status_volatile(tgt, &VolatileStatus::Disable(PokemonMove::Struggle));
        (m, disabled)
    };
    if already_disabled {
        return false;
    }
    let effect = HitEffect {
        volatile_status: Some(VolatileStatus::Disable(last_move)),
        ..Default::default()
    };
    apply_effect_to_target(state, source_slot, target_slot, &effect, target_slot.player);
    true
}

/// Set the target's type to a single `new_type` (Soak → Water, Magic Powder → Psychic).
/// Fails if the target is Terastallized, already that pure type, or — unless its ability is
/// suppressed — has Multitype/RKS System (form-locked abilities). Fully replacing the type
/// also clears any added-type markers. Returns true on success.
pub fn try_set_single_type(
    state: &mut BattleState,
    target_slot: FieldSlot,
    new_type: PokemonType,
) -> bool {
    let blocked = {
        let Some(tgt) = get_pokemon_at_slot(state, target_slot) else {
            return false;
        };
        let form_locked = !pokemon_ability_is_suppressed(state, tgt)
            && matches!(tgt.ability, Ability::Multitype | Ability::RKSSystem);
        tgt.is_tera || form_locked || tgt.types == vec![new_type.clone()]
    };
    if blocked {
        return false;
    }
    let Some(tgt) = get_pokemon_at_slot_mut(state, target_slot) else {
        return false;
    };
    tgt.types = vec![new_type];
    // Clear pre_mimicry_types so Mimicry doesn't restore over the new type (Soak overrides Mimicry).
    tgt.pre_mimicry_types = None;
    remove_status_volatile(tgt, &VolatileStatus::ForestsCurse);
    remove_status_volatile(tgt, &VolatileStatus::TrickorTreat);
    true
}

/// Add an extra type to the target (Forest's Curse → Grass, Trick-or-Treat → Ghost).
/// Fails if the target is Terastallized or already has `added_type`. If the *other*
/// add-type move had previously added `other_type`, that type is replaced rather than
/// stacking a fourth type. Returns true on success.
pub fn try_add_type(
    state: &mut BattleState,
    target_slot: FieldSlot,
    added_type: PokemonType,
    marker: VolatileStatus,
    other_type: PokemonType,
    other_marker: VolatileStatus,
) -> bool {
    let has_other = {
        let Some(tgt) = get_pokemon_at_slot(state, target_slot) else {
            return false;
        };
        if tgt.is_tera || pokemon_has_type(tgt, &added_type) {
            return false;
        }
        has_status_volatile(tgt, &other_marker)
    };
    let Some(tgt) = get_pokemon_at_slot_mut(state, target_slot) else {
        return false;
    };
    if has_other {
        // The other add-type move's contribution is replaced, not stacked.
        tgt.types.retain(|t| t != &other_type);
        remove_status_volatile(tgt, &other_marker);
    }
    tgt.types.push(added_type);
    tgt.volatiles
        .push(VolatileStatusState::TurnStatus(marker, 0));
    true
}

/// Reflect Type: change the user's type(s) to match the target's current type(s),
/// including any type added by Forest's Curse / Trick-or-Treat. If the target is
/// Terastallized the user copies its Tera type. Fails if the user is Terastallized or the
/// target is typeless. Returns true on success.
pub fn try_apply_reflect_type(
    state: &mut BattleState,
    user_slot: FieldSlot,
    target_slot: FieldSlot,
) -> bool {
    let copied = {
        let Some(user) = get_pokemon_at_slot(state, user_slot) else {
            return false;
        };
        if user.is_tera {
            return false;
        }
        let Some(tgt) = get_pokemon_at_slot(state, target_slot) else {
            return false;
        };
        let types = if tgt.is_tera {
            vec![tgt.tera_type.clone()]
        } else {
            tgt.types.clone()
        };
        if types.is_empty() {
            return false;
        }
        types
    };
    let Some(user) = get_pokemon_at_slot_mut(state, user_slot) else {
        return false;
    };
    user.types = copied;
    remove_status_volatile(user, &VolatileStatus::ForestsCurse);
    remove_status_volatile(user, &VolatileStatus::TrickorTreat);
    true
}

/// Electrify: give the target the Electrify volatile so that the move it uses later this
/// turn becomes Electric-type. The volatile is cleared at end of turn.
pub fn apply_electrify(state: &mut BattleState, source_slot: FieldSlot, target_slot: FieldSlot) {
    let effect = HitEffect {
        volatile_status: Some(VolatileStatus::Electrify),
        ..Default::default()
    };
    apply_effect_to_target(state, source_slot, target_slot, &effect, target_slot.player);
}

/// Moves Encore cannot lock the target into. Mirrors Showdown's `failencore` flag: copying /
/// move-calling moves, Encore/Mimic/Sketch/Mirror Move, Struggle, and Transform.
/// Returns true if Encore should fail / cannot lock the target into this move.
/// Uses the FailEncore flag as the authoritative source. Mimic is a manual addition:
/// it carries FailEncore semantics per Bulbapedia but the Showdown data omits the flag.
/// Struggle has no move data entry, so it is also excluded manually.
pub(crate) fn encore_immune_move(
    mv: &PokemonMove,
    move_dex: &std::collections::HashMap<
        crate::data::pokemon_move::PokemonMove,
        crate::state::dex_data::MoveData,
    >,
) -> bool {
    if matches!(mv, PokemonMove::Struggle | PokemonMove::Mimic) {
        return true;
    }
    move_dex.get(mv).map_or(false, |d| {
        move_has_flag(d, &crate::state::dex_data::MoveFlag::FailEncore)
    })
}

/// Apply Encore to `target_slot`, locking it into its `last_used_move` for 3 turns. Returns the
/// encored move on success (so the caller can also rewrite a pending queued action this turn), or
/// `None` if Encore fails (no last move, an Encore-immune move, or already Encored).
pub fn try_apply_encore(
    state: &mut BattleState,
    source_slot: FieldSlot,
    target_slot: FieldSlot,
    move_dex: &std::collections::HashMap<
        crate::data::pokemon_move::PokemonMove,
        crate::state::dex_data::MoveData,
    >,
) -> Option<PokemonMove> {
    let last_move = {
        let tgt = get_pokemon_at_slot(state, target_slot)?;
        let m = match &tgt.last_used_move {
            Some(mv) if !encore_immune_move(mv, move_dex) => mv.clone(),
            _ => return None,
        };
        // Fail if the target no longer carries that move with PP (e.g. it was forgotten/0 PP).
        let has_pp = tgt
            .moves
            .iter()
            .zip(tgt.move_pp.iter())
            .any(|(slot, pp)| slot.as_ref() == Some(&m) && *pp > 0);
        if !has_pp {
            return None;
        }
        if has_status_volatile(tgt, &VolatileStatus::Encore(PokemonMove::Struggle)) {
            return None;
        }
        m
    };
    let effect = HitEffect {
        volatile_status: Some(VolatileStatus::Encore(last_move.clone())),
        ..Default::default()
    };
    apply_effect_to_target(state, source_slot, target_slot, &effect, target_slot.player);
    // Confirm the volatile actually landed (Aroma Veil / Mental Herb may have blocked or cured it).
    let landed = get_pokemon_at_slot(state, target_slot)
        .is_some_and(|t| has_status_volatile(t, &VolatileStatus::Encore(PokemonMove::Struggle)));
    if landed { Some(last_move) } else { None }
}

/// Damage the attacker by `numer/denom` of its max HP (Rough Skin / Aftermath pattern).
/// Indirect HP damage paid by the attacker after using a move: Rough Skin / Iron Barbs recoil,
/// Rocky Helmet, crash damage, entry hazards, etc.  Blocked by Magic Guard.
/// Handles faint bookkeeping.
///
/// Note: Life Orb recoil (1/10 max HP) is not yet implemented (feature gap).
pub(crate) fn apply_hp_damage_to_attacker(
    bs: &mut BattleState,
    attacker_slot: FieldSlot,
    numer: u32,
    denom: u32,
) {
    let abilities_suppressed = abilities_are_suppressed(bs);
    let (max_hp, magic_guard) = {
        let Some(m) = get_pokemon_at_slot(bs, attacker_slot) else {
            return;
        };
        if m.fainted {
            return;
        }
        let mg = !abilities_suppressed && m.ability == Ability::MagicGuard;
        (m.stats[0].max(1), mg)
    };
    if magic_guard {
        return;
    }
    let env = berry_env(bs, attacker_slot);
    let damage = ((max_hp as u32 * numer) / denom).max(1) as u16;
    let mut fainted = false;
    if let Some(atk) = get_pokemon_at_slot_mut(bs, attacker_slot) {
        take_damage(atk, damage, env, abilities_suppressed);
        if atk.fainted {
            clear_pokemon_on_faint(atk);
            fainted = true;
        }
    }
    if fainted {
        handle_pokemon_faint(bs, attacker_slot.player, attacker_slot.slot_index);
    }
}

/// Deal an exact amount of HP damage to the attacker. Used by Innards Out (and similar
/// abilities that deal back a specific, pre-computed amount rather than a fraction of max HP).
/// Blocked by Magic Guard.
fn apply_flat_hp_damage_to_attacker(bs: &mut BattleState, attacker_slot: FieldSlot, amount: u16) {
    if amount == 0 {
        return;
    }
    let abilities_suppressed = abilities_are_suppressed(bs);
    let magic_guard = {
        let Some(m) = get_pokemon_at_slot(bs, attacker_slot) else {
            return;
        };
        if m.fainted {
            return;
        }
        !abilities_suppressed && m.ability == Ability::MagicGuard
    };
    if magic_guard {
        return;
    }
    let env = berry_env(bs, attacker_slot);
    let mut fainted = false;
    if let Some(atk) = get_pokemon_at_slot_mut(bs, attacker_slot) {
        take_damage(atk, amount, env, abilities_suppressed);
        if atk.fainted {
            clear_pokemon_on_faint(atk);
            fainted = true;
        }
    }
    if fainted {
        handle_pokemon_faint(bs, attacker_slot.player, attacker_slot.slot_index);
    }
}

/// Returns `true` if the move can trigger contact-based punishment effects on the attacker.
/// Long Reach removes contact entirely (so Tough Claws is also lost); Protective Pads only
/// blocks the *punishment* side — the holder still benefits from user-side contact bonuses
/// (Tough Claws etc.).
pub(crate) fn contact_effects_apply(
    state: &BattleState,
    attacker: &PokemonState,
    move_data: &MoveData,
) -> bool {
    if !move_has_flag(move_data, &MoveFlag::Contact) {
        return false;
    }
    let abilities_suppressed = abilities_are_suppressed(state);
    // Long Reach removes contact entirely.
    if !abilities_suppressed && attacker.ability == Ability::LongReach {
        return false;
    }
    // Protective Pads: blocks contact-triggered punishment while still "making contact" for
    // user-side bonuses. Item must be active (not suppressed by Klutz/Magic Room).
    if item_is_active(state, attacker) && attacker.item == Item::ProtectivePads {
        return false;
    }
    true
}

/// Fire all on-hit reactive ability effects for the ability holder (`holder_slot`) after it
/// takes `damage_dealt` HP damage from `attacker_slot`'s move. Returns the updated branch set.
///
/// Called from `apply_single_hit_branch` immediately before the per-hit outcomes are returned.
/// Because this runs per-hit, multi-hit moves get independent rolls — matching game behaviour.
pub fn apply_contact_hit_reactions(
    branches: Vec<(BattleState, f64)>,
    holder_slot: FieldSlot,
    attacker_slot: FieldSlot,
    move_name: &PokemonMove,
    move_data: &MoveData,
    damage_dealt: u16,
    is_crit: bool,
) -> Vec<(BattleState, f64)> {
    if damage_dealt == 0 || branches.is_empty() {
        return branches;
    }

    let holder_ability = {
        let ability_opt = {
            let first_bs = &branches[0].0;
            let attacker_opt = get_pokemon_at_slot(first_bs, attacker_slot);
            get_pokemon_at_slot(first_bs, holder_slot)
                .filter(|m| !pokemon_ability_is_suppressed(first_bs, m))
                .filter(|m| {
                    !attacker_opt.is_some_and(|a| {
                        attacker_breaks_mold(first_bs, a) && ability_is_ignorable(&m.ability)
                    })
                })
                .map(|m| m.ability.clone())
        };
        match ability_opt {
            Some(a) => a,
            None => return branches,
        }
    };

    // `contact_effects_apply` accounts for Long Reach (removes contact) and Protective Pads
    // (blocks punishment while keeping user-side contact bonuses like Tough Claws).
    let contact_punish = {
        let first_bs = &branches[0].0;
        get_pokemon_at_slot(first_bs, attacker_slot)
            .map_or(false, |atk| contact_effects_apply(first_bs, atk, move_data))
    };
    // Sheer Force: when the attacker's move is boosted by Sheer Force, a specific set of
    // after-hit effects is skipped. Of the reactions handled here, only Pickpocket is in that
    // negated set (Rough Skin / Static / Poison Point / Flame Body / Effect Spore / etc. are
    // explicitly NOT negated). Life Orb recoil and Shell Bell are handled at their own sites.
    let attacker_sheer_force_boosted = {
        let first_bs = &branches[0].0;
        get_pokemon_at_slot(first_bs, attacker_slot).map_or(false, |atk| {
            !pokemon_ability_is_suppressed(first_bs, atk)
                && atk.ability == Ability::SheerForce
                && move_has_sheer_force_secondary(move_data)
        })
    };
    let is_physical = matches!(move_data.category, MoveCategory::Physical);

    // Beak Blast: if the holder has the BeakBlastCharging volatile and the attacker's move
    // makes contact (and contact punishment applies), burn the attacker.
    let branches = if contact_punish {
        let has_beak_blast_charging = {
            let first_bs = &branches[0].0;
            get_pokemon_at_slot(first_bs, holder_slot)
                .is_some_and(|m| has_status_volatile(m, &VolatileStatus::BeakBlastCharging))
        };
        if has_beak_blast_charging {
            let eff = HitEffect {
                status: Some(Status::Burn),
                ..Default::default()
            };
            branches
                .into_iter()
                .map(|(mut bs, prob)| {
                    apply_effect_to_target(
                        &mut bs,
                        holder_slot,
                        attacker_slot,
                        &eff,
                        attacker_slot.player,
                    );
                    (bs, prob)
                })
                .collect()
        } else {
            branches
        }
    } else {
        branches
    };

    let mut branches = match holder_ability {
        Ability::RoughSkin => {
            if !contact_punish {
                return branches;
            }
            branches
                .into_iter()
                .map(|(mut bs, prob)| {
                    apply_hp_damage_to_attacker(&mut bs, attacker_slot, 1, 8);
                    (bs, prob)
                })
                .collect()
        }
        Ability::Aftermath => {
            if !contact_punish {
                return branches;
            }
            branches
                .into_iter()
                .map(|(mut bs, prob)| {
                    let holder_fainted = get_pokemon_at_slot(&bs, holder_slot)
                        .map(|m| m.fainted)
                        .unwrap_or(false);
                    if !holder_fainted {
                        return (bs, prob);
                    }
                    if active_mons_have_ability(&bs, &Ability::Damp) {
                        return (bs, prob);
                    }
                    apply_hp_damage_to_attacker(&mut bs, attacker_slot, 1, 4);
                    (bs, prob)
                })
                .collect()
        }
        Ability::FlameBody => {
            if !contact_punish {
                return branches;
            }
            let eff = HitEffect {
                status: Some(Status::Burn),
                ..Default::default()
            };
            branch_on_secondary_effects(branches, 0.30, std::slice::from_ref(&eff), |bs, e| {
                apply_effect_to_target(bs, holder_slot, attacker_slot, e, attacker_slot.player);
            })
        }
        Ability::PoisonPoint => {
            if !contact_punish {
                return branches;
            }
            let eff = HitEffect {
                status: Some(Status::Poison),
                ..Default::default()
            };
            branch_on_secondary_effects(branches, 0.30, std::slice::from_ref(&eff), |bs, e| {
                apply_effect_to_target(bs, holder_slot, attacker_slot, e, attacker_slot.player);
            })
        }
        Ability::Static => {
            if !contact_punish {
                return branches;
            }
            let eff = HitEffect {
                status: Some(Status::Paralysis),
                ..Default::default()
            };
            branch_on_secondary_effects(branches, 0.30, std::slice::from_ref(&eff), |bs, e| {
                apply_effect_to_target(bs, holder_slot, attacker_slot, e, attacker_slot.player);
            })
        }
        // Effect Spore: on contact, the attacker has a 30% total chance of a status —
        // split 11% sleep / 10% paralysis / 9% poison (NOT equal thirds). Independent roll
        // per hit. Powder immunity (Grass-type / Overcoat / Safety Goggles) on the *attacker*
        // skips it entirely; per-status immunities (Poison/Steel, Electric, Insomnia/Vital
        // Spirit, etc.) are enforced downstream in apply_status_to_pokemon.
        Ability::EffectSpore => {
            if !contact_punish {
                return branches;
            }
            let attacker_immune = {
                let first_bs = &branches[0].0;
                get_pokemon_at_slot(first_bs, attacker_slot)
                    .map_or(true, |atk| is_immune_to_powder(first_bs, atk, None))
            };
            if attacker_immune {
                return branches;
            }
            // (status, probability) — totals 0.30; remaining 0.70 is the no-status branch.
            let statuses = [
                (Status::Sleep(0), 0.11f64),
                (Status::Paralysis, 0.10f64),
                (Status::Poison, 0.09f64),
            ];
            branches
                .into_iter()
                .flat_map(|(bs, prob)| {
                    let mut out = Vec::with_capacity(statuses.len() + 1);
                    let mut applied_chance = 0.0;
                    for (status, chance) in statuses.iter() {
                        let eff = HitEffect {
                            status: Some(status.clone()),
                            ..Default::default()
                        };
                        let mut applied = bs.clone();
                        apply_effect_to_target(
                            &mut applied,
                            holder_slot,
                            attacker_slot,
                            &eff,
                            attacker_slot.player,
                        );
                        out.push((applied, prob * chance));
                        applied_chance += chance;
                    }
                    out.push((bs, prob * (1.0 - applied_chance)));
                    out
                })
                .collect()
        }
        Ability::SpicySpray => {
            // Any damaging move; fires even when the holder has already fainted.
            let eff = HitEffect {
                status: Some(Status::Burn),
                ..Default::default()
            };
            branches
                .into_iter()
                .map(|(mut bs, prob)| {
                    apply_effect_to_target(
                        &mut bs,
                        holder_slot,
                        attacker_slot,
                        &eff,
                        attacker_slot.player,
                    );
                    (bs, prob)
                })
                .collect()
        }
        Ability::CuteCharm => {
            if !contact_punish {
                return branches;
            }
            let eligible = {
                let first_bs = &branches[0].0;
                can_be_infatuated(first_bs, holder_slot, attacker_slot)
            };
            if !eligible {
                return branches;
            }
            let eff = HitEffect {
                volatile_status: Some(VolatileStatus::Attract),
                ..Default::default()
            };
            branch_on_secondary_effects(branches, 0.30, std::slice::from_ref(&eff), |bs, e| {
                apply_effect_to_target(bs, holder_slot, attacker_slot, e, attacker_slot.player);
            })
        }
        Ability::CursedBody => {
            // Any damaging move; Struggle cannot be disabled.
            if *move_name == PokemonMove::Struggle {
                return branches;
            }
            let eff = HitEffect {
                volatile_status: Some(VolatileStatus::Disable(move_name.clone())),
                ..Default::default()
            };
            branch_on_secondary_effects(branches, 0.30, std::slice::from_ref(&eff), |bs, e| {
                apply_effect_to_target(bs, holder_slot, attacker_slot, e, attacker_slot.player);
            })
        }
        Ability::Gooey => {
            if !contact_punish {
                return branches;
            }
            let eff = HitEffect {
                boosts: [0, 0, 0, 0, -1, 0, 0],
                ..Default::default()
            };
            branches
                .into_iter()
                .map(|(mut bs, prob)| {
                    apply_effect_to_target(
                        &mut bs,
                        holder_slot,
                        attacker_slot,
                        &eff,
                        attacker_slot.player,
                    );
                    (bs, prob)
                })
                .collect()
        }
        Ability::WeakArmor => {
            if !is_physical {
                return branches;
            }
            let boosts: [i8; 7] = [0, -1, 0, 0, 2, 0, 0];
            branches
                .into_iter()
                .map(|(mut bs, prob)| {
                    let alive = get_pokemon_at_slot(&bs, holder_slot)
                        .map(|m| !m.fainted)
                        .unwrap_or(false);
                    if !alive {
                        return (bs, prob);
                    }
                    let items_suppressed = items_are_suppressed(&bs);
                    if let Some(mon) = get_pokemon_at_slot_mut(&mut bs, holder_slot) {
                        apply_stat_boosts_to_pokemon(mon, &boosts, items_suppressed, false);
                    }
                    (bs, prob)
                })
                .collect()
        }
        // ── Stat-change reaction abilities (Subgroup B) ───────────────────────────────

        // Stamina: +1 Defense on any damaging hit; fires per hit of multi-hit moves.
        Ability::Stamina => branches
            .into_iter()
            .map(|(mut bs, prob)| {
                let alive = get_pokemon_at_slot(&bs, holder_slot)
                    .map(|m| !m.fainted)
                    .unwrap_or(false);
                if !alive {
                    return (bs, prob);
                }
                let items_suppressed = items_are_suppressed(&bs);
                if let Some(mon) = get_pokemon_at_slot_mut(&mut bs, holder_slot) {
                    apply_stat_boosts_to_pokemon(
                        mon,
                        &[0, 1, 0, 0, 0, 0, 0],
                        items_suppressed,
                        false,
                    );
                }
                (bs, prob)
            })
            .collect(),
        // Justified: +1 Attack when hit by a Dark-type damaging move; fires per hit.
        Ability::Justified => {
            if move_data.pokemon_type != PokemonType::Dark {
                return branches;
            }
            branches
                .into_iter()
                .map(|(mut bs, prob)| {
                    let alive = get_pokemon_at_slot(&bs, holder_slot)
                        .map(|m| !m.fainted)
                        .unwrap_or(false);
                    if !alive {
                        return (bs, prob);
                    }
                    let items_suppressed = items_are_suppressed(&bs);
                    if let Some(mon) = get_pokemon_at_slot_mut(&mut bs, holder_slot) {
                        apply_stat_boosts_to_pokemon(
                            mon,
                            &[1, 0, 0, 0, 0, 0, 0],
                            items_suppressed,
                            false,
                        );
                    }
                    (bs, prob)
                })
                .collect()
        }
        // Anger Point: on a critical hit, maximise Attack to +6 (set, not add).
        // Does not activate if the hit goes into a Substitute.
        Ability::AngerPoint => {
            if !is_crit {
                return branches;
            }
            branches
                .into_iter()
                .map(|(mut bs, prob)| {
                    let alive = get_pokemon_at_slot(&bs, holder_slot)
                        .map(|m| !m.fainted)
                        .unwrap_or(false);
                    if !alive {
                        return (bs, prob);
                    }
                    if let Some(mon) = get_pokemon_at_slot_mut(&mut bs, holder_slot) {
                        if mon.boosts[0] < 6 {
                            mon.stats_raised_this_turn = true;
                        }
                        mon.boosts[0] = 6; // maximise Attack regardless of current stage
                    }
                    (bs, prob)
                })
                .collect()
        }
        // Electromorphosis: gain the Charge status (×2 next Electric move) when hit.
        // Charge does not stack; re-hitting a charged Pokémon just refreshes it.
        Ability::Electromorphosis => {
            branches
                .into_iter()
                .map(|(mut bs, prob)| {
                    let alive = get_pokemon_at_slot(&bs, holder_slot)
                        .map(|m| !m.fainted)
                        .unwrap_or(false);
                    if !alive {
                        return (bs, prob);
                    }
                    if let Some(mon) = get_pokemon_at_slot_mut(&mut bs, holder_slot) {
                        // Remove any existing Charge volatile first (no stacking),
                        // then push a fresh one.
                        remove_status_volatile(mon, &VolatileStatus::Charge);
                        mon.volatiles
                            .push(VolatileStatusState::TurnStatus(VolatileStatus::Charge, 0));
                    }
                    (bs, prob)
                })
                .collect()
        }
        // Pickpocket: steal the attacker's item when hit by a contact move while
        // empty-handed. Doesn't trigger if the holder is KO'd by the hit. Fires on the
        // first contact hit rather than the cartridge's last strike — equivalent outcome,
        // since the attacker's item is gone either way.
        Ability::Pickpocket => {
            if !contact_punish || attacker_sheer_force_boosted {
                return branches;
            }
            branches
                .into_iter()
                .map(|(mut bs, prob)| {
                    let holder_alive = get_pokemon_at_slot(&bs, holder_slot)
                        .map(|m| !m.fainted)
                        .unwrap_or(false);
                    if holder_alive {
                        try_steal_item(&mut bs, holder_slot, attacker_slot);
                    }
                    (bs, prob)
                })
                .collect()
        }
        Ability::Mummy => {
            if !contact_punish {
                return branches;
            }
            branches
                .into_iter()
                .map(|(mut bs, prob)| {
                    let (atk_ability, excluded) = {
                        let Some(atk) = get_pokemon_at_slot(&bs, attacker_slot) else {
                            return (bs, prob);
                        };
                        if atk.fainted {
                            return (bs, prob);
                        }
                        (
                            atk.ability.clone(),
                            ability_excluded_from_mummy(&atk.ability)
                                || atk.ability == Ability::Mummy,
                        )
                    };
                    if excluded {
                        return (bs, prob);
                    }
                    if let Some(atk) = get_pokemon_at_slot_mut(&mut bs, attacker_slot) {
                        if atk.original_ability.is_none() {
                            atk.original_ability = Some(atk_ability);
                        }
                        atk.ability = Ability::Mummy;
                    }
                    // Attacker lost Illusion (gained Mummy); break disguise if active.
                    maybe_break_illusion_on_ability_change(&mut bs, attacker_slot);
                    emit(&mut bs, crate::information::information::EventKind::AbilityRevealed {
                        slot: attacker_slot,
                        ability: Ability::Mummy,
                    });
                    (bs, prob)
                })
                .collect()
        }
        Ability::WanderingSpirit => {
            if !contact_punish {
                return branches;
            }
            branches
                .into_iter()
                .map(|(mut bs, prob)| {
                    // Check attacker's ability can be swapped
                    let (atk_ability, excluded) = {
                        let Some(atk) = get_pokemon_at_slot(&bs, attacker_slot) else {
                            return (bs, prob);
                        };
                        let excluded =
                            atk.fainted || ability_excluded_from_wandering_spirit(&atk.ability);
                        (atk.ability.clone(), excluded)
                    };
                    if excluded {
                        return (bs, prob);
                    }
                    let hld_ability = {
                        let Some(hld) = get_pokemon_at_slot(&bs, holder_slot) else {
                            return (bs, prob);
                        };
                        hld.ability.clone()
                    };
                    // Swap: attacker gets hld_ability (WanderingSpirit), holder gets atk_ability
                    if let Some(atk) = get_pokemon_at_slot_mut(&mut bs, attacker_slot) {
                        if atk.original_ability.is_none() {
                            atk.original_ability = Some(atk_ability.clone());
                        }
                        atk.ability = hld_ability.clone();
                    }
                    if let Some(hld) = get_pokemon_at_slot_mut(&mut bs, holder_slot) {
                        if hld.original_ability.is_none() {
                            hld.original_ability = Some(hld_ability.clone());
                        }
                        hld.ability = atk_ability.clone();
                    }
                    // Break Illusion if either mon's ability changed away from Illusion.
                    maybe_break_illusion_on_ability_change(&mut bs, attacker_slot);
                    maybe_break_illusion_on_ability_change(&mut bs, holder_slot);
                    // Fire on-gain effects for both
                    process_pokemon_gain_ability(&mut bs, attacker_slot);
                    process_pokemon_gain_ability(&mut bs, holder_slot);
                    emit(&mut bs, crate::information::information::EventKind::AbilityRevealed {
                        slot: attacker_slot,
                        ability: hld_ability,
                    });
                    emit(&mut bs, crate::information::information::EventKind::AbilityRevealed {
                        slot: holder_slot,
                        ability: atk_ability,
                    });
                    (bs, prob)
                })
                .collect()
        }
        // Innards Out: when KO'd by a damaging move, deal the holder's pre-hit HP back to
        // the attacker. `damage_dealt` is clamped to the holder's remaining HP at the time
        // of the hit, so it equals the "HP before last hit" when the holder faints.
        // No contact requirement; not blocked by Damp.
        Ability::InnardsOut => branches
            .into_iter()
            .map(|(mut bs, prob)| {
                let holder_fainted = get_pokemon_at_slot(&bs, holder_slot)
                    .map(|m| m.fainted)
                    .unwrap_or(false);
                if !holder_fainted {
                    return (bs, prob);
                }
                apply_flat_hp_damage_to_attacker(&mut bs, attacker_slot, damage_dealt);
                (bs, prob)
            })
            .collect(),

        // Toxic Debris: when hit by a physical move, scatter Toxic Spikes on the attacker's side.
        // Triggers per hit on multi-hit moves; capped at 2 layers by add_side_condition.
        // No contact requirement (any physical category move).
        Ability::ToxicDebris => {
            if !is_physical {
                return branches;
            }
            branches
                .into_iter()
                .map(|(mut bs, prob)| {
                    add_side_condition(
                        &mut bs,
                        attacker_slot.player,
                        SideCondition::ToxicSpikes(1),
                        0,
                    );
                    (bs, prob)
                })
                .collect()
        }

        _ => branches,
    };

    // ── Charge consumer (attacker-side, outside the holder-ability match) ─────────────────
    // If the attacker used a damaging Electric-type move and holds the Charge volatile,
    // consume it now.  The doubling already happened inside effective_base_power.
    // Runs per-hit; on the first hit Charge is consumed, subsequent hits see it gone.
    if move_data.pokemon_type == PokemonType::Electric {
        branches = branches
            .into_iter()
            .map(|(mut bs, prob)| {
                if let Some(atk) = get_pokemon_at_slot_mut(&mut bs, attacker_slot) {
                    remove_status_volatile(atk, &VolatileStatus::Charge);
                }
                (bs, prob)
            })
            .collect();
    }

    branches
}

// ── Type-immunity / absorption abilities ──────────────────────────────────────

/// React-on-hit absorption: absorb a move that has *already hit* the target and apply the
/// appropriate bonus (heal, stat boost, or Flash Fire flag) to the target instead of damage.
///
/// Returns `true` if an absorption ability fires, in which case the caller must
/// - skip all damage, endure, and secondary effects (treat the move as fully consumed),
/// - push the mutated state as the sole outcome.
///
/// Returns `false` (and leaves `state` unchanged) when no react-on-hit ability matches.
///
/// Covers: Volt Absorb, Water Absorb, Earth Eater, Sap Sipper, Motor Drive, Flash Fire,
/// and Dry Skin's Water absorption.
///
/// Lightning Rod and Storm Drain are **not** handled here — they are draw-in abilities that
/// fire before the accuracy roll.  See `try_drawin_negate`.
///
/// Mold Breaker bypass is handled via `target_ability_as_seen_by`, which returns `Ability::None`
/// for ignorable abilities when the attacker has a Mold Breaker family ability.
pub(crate) fn try_absorb_move(
    state: &mut BattleState,
    target_slot: FieldSlot,
    attacker: &PokemonState,
    move_data: &MoveData,
    items_suppressed: bool,
) -> bool {
    // Fetch the target's ability as seen by the attacker; suppressed or Mold-Broken → hits normally.
    let target_ability = match get_pokemon_at_slot(state, target_slot) {
        Some(t) => target_ability_as_seen_by(state, attacker, t),
        _ => return false,
    };
    if target_ability == Ability::None {
        return false;
    }

    // Use the canonical move type (respects -ate abilities, Liquid Voice, etc.).
    let move_type = effective_move_type(state, attacker, move_data);
    let target_env = berry_env(state, target_slot);

    let absorbs = match (&move_type, &target_ability) {
        (PokemonType::Electric, Ability::VoltAbsorb)
        | (PokemonType::Water, Ability::WaterAbsorb)
        | (PokemonType::Water, Ability::DrySkin)
        | (PokemonType::Ground, Ability::EarthEater) => {
            if let Some(mon) = get_pokemon_at_slot_mut(state, target_slot) {
                let heal = (mon.stats[0].max(1) as u32 / 4) as u16;
                gain_hp(mon, heal, target_env);
            }
            true
        }
        (PokemonType::Grass, Ability::SapSipper) => {
            if let Some(mon) = get_pokemon_at_slot_mut(state, target_slot) {
                apply_stat_boosts_to_pokemon(mon, &[1, 0, 0, 0, 0, 0, 0], items_suppressed, false);
            }
            true
        }
        (PokemonType::Electric, Ability::MotorDrive) => {
            if let Some(mon) = get_pokemon_at_slot_mut(state, target_slot) {
                apply_stat_boosts_to_pokemon(mon, &[0, 0, 0, 0, 1, 0, 0], items_suppressed, false);
            }
            true
        }
        (PokemonType::Fire, Ability::FlashFire) => {
            if let Some(mon) = get_pokemon_at_slot_mut(state, target_slot) {
                if !has_status_volatile(mon, &VolatileStatus::FlashFire) {
                    mon.volatiles
                        .push(crate::state::pokemon::VolatileStatusState::MoveStatus(
                            VolatileStatus::FlashFire,
                            0,
                        ));
                }
            }
            true
        }
        _ => false,
    };

    absorbs
}

/// Draw-in negation: Lightning Rod (Electric) and Storm Drain (Water) pull a single-target
/// move of the matching type toward the holder and absorb it, granting +1 Sp. Atk.
///
/// Crucially, this fires **before** accuracy is rolled — the move is negated and the bonus
/// applied whether the move would have hit or missed (or been blocked by Protect once that
/// mechanic is implemented).
///
/// Returns `true` if a draw-in ability fires (caller should push no-effect outcome and skip
/// the rest of target processing for this slot).  Returns `false` otherwise.
pub(crate) fn try_drawin_negate(
    state: &mut BattleState,
    target_slot: FieldSlot,
    attacker: &PokemonState,
    move_data: &MoveData,
    items_suppressed: bool,
) -> bool {
    let target_ability = match get_pokemon_at_slot(state, target_slot) {
        Some(t) => target_ability_as_seen_by(state, attacker, t),
        _ => return false,
    };
    if target_ability == Ability::None {
        return false;
    }

    let move_type = effective_move_type(state, attacker, move_data);

    let negated = match (&move_type, &target_ability) {
        (PokemonType::Electric, Ability::LightningRod)
        | (PokemonType::Water, Ability::StormDrain) => {
            if let Some(mon) = get_pokemon_at_slot_mut(state, target_slot) {
                apply_stat_boosts_to_pokemon(mon, &[0, 0, 1, 0, 0, 0, 0], items_suppressed, false);
            }
            true
        }
        _ => false,
    };

    negated
}

/// Apply the binding (partial-trapping) volatile to `target_slot` with duration branching.
///
/// Called from `apply_secondary_effects` to handle the parsed `PartiallyTrapped(u8::MAX)`
/// sentinel.  Branches the outcome tree based on duration (4 or 5 turns) unless the
/// attacker holds a Grip Claw (fixed 7 turns).  Ghosts receive the volatile (they still
/// take chip damage) but the switch-prevention is waived in `is_trapped`.
fn apply_binding_trap(
    branches: Vec<(BattleState, f64)>,
    attacker_mon_id: u8,
    target_slot: FieldSlot,
) -> Vec<(BattleState, f64)> {
    let mut new_branches = Vec::with_capacity(branches.len() * 2);
    for (bs, prob) in branches {
        // Skip if already bound (cannot stack) or protected by a Substitute.
        let already_bound = get_pokemon_at_slot(&bs, target_slot).map_or(false, |m| {
            has_status_volatile(m, &VolatileStatus::PartiallyTrapped(0))
        });
        let has_sub = get_pokemon_at_slot(&bs, target_slot).map_or(false, |m| {
            has_status_volatile(m, &VolatileStatus::Substitute(0))
        });
        if already_bound || has_sub {
            new_branches.push((bs, prob));
            continue;
        }

        // Grip Claw (held by the trapper): locate the trapper by mon_id for doubles safety.
        let grip_claw = bs
            .p1_active_mons
            .iter()
            .chain(bs.p2_active_mons.iter())
            .find(|m| m.mon_id == attacker_mon_id)
            .map_or(false, |trapper| {
                item_is_active(&bs, trapper) && trapper.item == Item::GripClaw
            });

        if grip_claw {
            let mut applied = bs.clone();
            if let Some(mon) = get_pokemon_at_slot_mut(&mut applied, target_slot) {
                mon.volatiles.push(VolatileStatusState::TurnStatus(
                    VolatileStatus::PartiallyTrapped(attacker_mon_id),
                    7,
                ));
            }
            new_branches.push((applied, prob));
        } else {
            // 50/50 branch: 4 turns or 5 turns.
            for duration in [4u16, 5u16] {
                let mut applied = bs.clone();
                if let Some(mon) = get_pokemon_at_slot_mut(&mut applied, target_slot) {
                    mon.volatiles.push(VolatileStatusState::TurnStatus(
                        VolatileStatus::PartiallyTrapped(attacker_mon_id),
                        duration,
                    ));
                }
                new_branches.push((applied, prob * 0.5));
            }
        }
    }
    new_branches
}

/// Apply the pure-trapping effect for Block / Mean Look / Spirit Shackle.
///
/// These moves store their trap in Showdown's unparsed `onHit` JS, so they are
/// hand-coded here.  Unlike partial trapping there is no chip damage; the trap lasts
/// until the trapper leaves the field (duration = 0 → permanent, released by
/// `release_traps_set_by`).
///
/// Ghost-type targets are fully immune (the volatile is simply not applied).  Spirit
/// Shackle's damage is handled by the normal damage pipeline; this function only adds
/// the `Trapped` volatile post-damage.
fn apply_trapping_move(
    bs: &mut BattleState,
    attacker_slot: FieldSlot,
    target_slot: FieldSlot,
    move_name: &PokemonMove,
) {
    match move_name {
        PokemonMove::Block | PokemonMove::MeanLook | PokemonMove::SpiritShackle => {
            let attacker_mon_id = get_pokemon_at_slot(bs, attacker_slot)
                .map(|m| m.mon_id)
                .unwrap_or(u8::MAX);
            // Ghost targets are fully immune to pure-trapping moves.
            let target_is_ghost = get_pokemon_at_slot(bs, target_slot)
                .map_or(false, |m| pokemon_has_type(m, &PokemonType::Ghost));
            // Substitute blocks the trapping effect.
            let target_has_sub = get_pokemon_at_slot(bs, target_slot).map_or(false, |m| {
                has_status_volatile(m, &VolatileStatus::Substitute(0))
            });
            if target_is_ghost || target_has_sub {
                return;
            }
            if let Some(mon) = get_pokemon_at_slot_mut(bs, target_slot) {
                if !has_status_volatile(mon, &VolatileStatus::Trapped(0)) {
                    mon.volatiles.push(VolatileStatusState::TurnStatus(
                        VolatileStatus::Trapped(attacker_mon_id),
                        0,
                    ));
                }
            }
        }
        _ => {}
    }
}

/// Apply move secondary effects with appropriate probability.
/// This is called after a move hits to apply status, volatile status, side conditions, etc.
pub fn apply_secondary_effects(
    state: &BattleState,
    attacker_slot: FieldSlot,
    target_slot: FieldSlot,
    move_data: &MoveData,
) -> Vec<(BattleState, f64)> {
    let side_condition_target = match move_data.target {
        MoveTarget::FoeSide | MoveTarget::AllAdjacentFoes | MoveTarget::AllAdjacent => {
            match attacker_slot.player {
                Player::P1 => Player::P2,
                Player::P2 => Player::P1,
            }
        }
        MoveTarget::AllySide | MoveTarget::Allies | MoveTarget::AllyTeam => attacker_slot.player,
        _ => target_slot.player,
    };
    let attacker_env = berry_env(state, attacker_slot);

    let mut branches: Vec<(BattleState, f64)> = vec![(state.clone(), 1.0)];

    // Shed Tail: Showdown data includes `volatileStatus:'substitute'` as a top-level field,
    // which the parser turns into a 100% entry in `secondaries`. Without special handling that
    // secondary would unconditionally apply Substitute before any fail-check runs. Instead we:
    //   • determine success/failure up front (all three fail conditions checked here),
    //   • on success: apply the HP cost (ceil(max_hp/2)) so it precedes the Substitute creation,
    //   • on failure: set `shed_tail_failed` so the secondary is skipped below.
    // The Substitute itself is then created (or not) by the normal secondaries path.
    // All three fail conditions must be checked here (not just in apply_post_damage_move_effects)
    // so that the HP cost and Substitute are never applied when the move fails.
    let shed_tail_failed = if move_data.self_switch == SelfSwitchType::ShedTail {
        let failed = branches.first().map_or(true, |(bs, _)| {
            // No healthy bench → move fails entirely (no HP cost, no sub, no switch).
            let no_bench = match attacker_slot.player {
                Player::P1 => bs.p1_back_mons.iter().all(|m| m.fainted),
                Player::P2 => bs.p2_back_mons.iter().all(|m| m.fainted),
            };
            no_bench
                || get_pokemon_at_slot(bs, attacker_slot).map_or(true, |m| {
                    let max_hp = m.stats[0].max(1);
                    m.volatiles.iter().any(|v| {
                        matches!(
                            v,
                            VolatileStatusState::TurnStatus(VolatileStatus::Substitute(_), _)
                        )
                    }) || m.hp <= max_hp / 2
                })
        });
        if !failed {
            // HP cost: ceil(max_hp / 2)
            for (bs, _) in branches.iter_mut() {
                let as_ = abilities_are_suppressed(bs);
                if let Some(m) = get_pokemon_at_slot_mut(bs, attacker_slot) {
                    let max_hp = m.stats[0].max(1);
                    let cost = (max_hp + 1) / 2;
                    take_damage(m, cost, attacker_env, as_);
                }
            }
        }
        failed
    } else {
        false
    };

    // Shield Dust: block all additional (secondary) effects that would be applied to the
    // target — status riders, stat-drop chances, flinch, King's Rock / Razor Fang flinch,
    // Poison Touch / Toxic Chain procs, etc.  Self-effects on the attacker (self_boost,
    // self_secondaries) are intentionally NOT blocked; those are applied further below.
    // Mold Breaker bypasses Shield Dust.
    let attacker_breaks =
        get_pokemon_at_slot(state, attacker_slot).map_or(false, |a| attacker_breaks_mold(state, a));
    let target_has_shield_dust = !attacker_breaks
        && get_pokemon_at_slot(state, target_slot).map_or(false, |mon| {
            !pokemon_ability_is_suppressed(state, mon) && mon.ability == Ability::ShieldDust
        });

    // Sheer Force: when the attacker has Sheer Force (unsuppressed) and the move has
    // eligible target secondaries, those secondaries are suppressed in exchange for the BP
    // boost already applied in base_power_for_move. Self-effects (self_boost, self_secondaries)
    // are NOT suppressed.
    let attacker_has_sheer_force = get_pokemon_at_slot(state, attacker_slot).map_or(false, |mon| {
        !pokemon_ability_is_suppressed(state, mon) && mon.ability == Ability::SheerForce
    }) && move_has_sheer_force_secondary(move_data);

    // Burning Jealousy: the burn applies only to targets whose stats were actually raised
    // earlier this turn. The Showdown source stores this as an `onHit` callback, so the
    // parsed `secondaries` carry no status — apply it explicitly. The 70 BP is unconditional.
    let burning_jealousy_burn = move_data.name == PokemonMove::BurningJealousy
        && get_pokemon_at_slot(state, target_slot).is_some_and(|m| m.stats_raised_this_turn);

    // Branch target secondaries
    if !target_has_shield_dust && !attacker_has_sheer_force {
        for secondary in &move_data.secondaries {
            // Shed Tail's auto-parsed Substitute secondary carries duration 0 (the generic
            // "permanent" value), which would create a sub with HP = 0 under the new model
            // where the payload stores sub HP.  Always skip the generic path and, on success,
            // manually create the sub with the correct HP (max_hp / 4 of the user).
            if secondary.random_choices.is_empty()
                && matches!(
                    secondary.effect.volatile_status,
                    Some(VolatileStatus::Substitute(_))
                )
                && move_data.self_switch == SelfSwitchType::ShedTail
            {
                if !shed_tail_failed {
                    for (bs, _) in branches.iter_mut() {
                        if let Some(m) = get_pokemon_at_slot_mut(bs, attacker_slot) {
                            let sub_hp = (m.stats[0].max(1) / 4).max(1);
                            m.volatiles.push(VolatileStatusState::TurnStatus(
                                VolatileStatus::Substitute(sub_hp),
                                0,
                            ));
                        }
                    }
                }
                continue;
            }
            // PartiallyTrapped sentinel: handled separately below with duration branching.
            if secondary.random_choices.is_empty()
                && matches!(
                    &secondary.effect.volatile_status,
                    Some(VolatileStatus::PartiallyTrapped(_))
                )
            {
                continue;
            }
            let chance = secondary.chance as f64 / 100.0;
            let choices = if secondary.random_choices.is_empty() {
                std::slice::from_ref(&secondary.effect)
            } else {
                &secondary.random_choices
            };
            branches = branch_on_secondary_effects(branches, chance, choices, |bs, eff| {
                apply_effect_to_target(bs, attacker_slot, target_slot, eff, side_condition_target);
            });
        }

        if burning_jealousy_burn {
            let burn = HitEffect {
                status: Some(Status::Burn),
                ..Default::default()
            };
            for (bs, _) in branches.iter_mut() {
                apply_effect_to_target(
                    bs,
                    attacker_slot,
                    target_slot,
                    &burn,
                    side_condition_target,
                );
            }
        }

        // Alluring Voice: confuse the target only if its stats were raised earlier this turn.
        // Showdown stores this as a 100% `onHit` secondary the parser cannot represent, so apply
        // it explicitly. The move is `bypasssub`, so the confusion applies through a Substitute
        // (no sub guard); Own Tempo / Misty Terrain immunity is handled in apply_volatile_to_pokemon.
        let alluring_voice_confuse = move_data.name == PokemonMove::AlluringVoice
            && get_pokemon_at_slot(state, target_slot).is_some_and(|m| m.stats_raised_this_turn);
        if alluring_voice_confuse {
            let eff = HitEffect {
                volatile_status: Some(VolatileStatus::Confusion),
                ..Default::default()
            };
            for (bs, _) in branches.iter_mut() {
                apply_effect_to_target(bs, attacker_slot, target_slot, &eff, side_condition_target);
            }
        }

        // Throat Chop: on hit, prevent the target from using sound moves for 2 turns. Showdown
        // stores this as a 100% `onHit` secondary that the parser cannot represent. Blocked by a
        // Substitute; consecutive hits do not refresh the duration (apply_volatile_to_pokemon's
        // already-has guard handles that).
        if move_data.name == PokemonMove::ThroatChop {
            let target_has_sub = get_pokemon_at_slot(state, target_slot)
                .is_some_and(|m| has_status_volatile(m, &VolatileStatus::Substitute(0)));
            if !target_has_sub {
                let eff = HitEffect {
                    volatile_status: Some(VolatileStatus::ThroatChop),
                    ..Default::default()
                };
                for (bs, _) in branches.iter_mut() {
                    apply_effect_to_target(
                        bs,
                        attacker_slot,
                        target_slot,
                        &eff,
                        side_condition_target,
                    );
                }
            }
        }

        // Binding trap: apply PartiallyTrapped with source-id and duration branching.
        // Handled after all other target secondaries so flinch / stat drops fire first.
        let has_bind_secondary = move_data.secondaries.iter().any(|s| {
            s.random_choices.is_empty()
                && matches!(
                    &s.effect.volatile_status,
                    Some(VolatileStatus::PartiallyTrapped(_))
                )
        });
        if has_bind_secondary {
            let attacker_mon_id = get_pokemon_at_slot(state, attacker_slot)
                .map(|m| m.mon_id)
                .unwrap_or(u8::MAX);
            branches = apply_binding_trap(branches, attacker_mon_id, target_slot);
        }
    }

    // Poison Touch: the attacker's contact moves have a 30% chance per hit to poison the
    // target. Independent of the move's own secondary effect; blocked by the target's Shield
    // Dust; requires actual contact (Long Reach / Protective Pads handled by
    // contact_effects_apply). Poison/Steel target immunity is enforced downstream in
    // apply_status_to_pokemon.
    if !target_has_shield_dust {
        let poison_touch_contact = get_pokemon_at_slot(state, attacker_slot).map_or(false, |atk| {
            !pokemon_ability_is_suppressed(state, atk)
                && atk.ability == Ability::PoisonTouch
                && contact_effects_apply(state, atk, move_data)
        });
        if poison_touch_contact {
            let eff = HitEffect {
                status: Some(Status::Poison),
                ..Default::default()
            };
            branches =
                branch_on_secondary_effects(branches, 0.30, std::slice::from_ref(&eff), |bs, e| {
                    apply_effect_to_target(bs, attacker_slot, target_slot, e, target_slot.player);
                });
        }
    }

    // Sparkling Aria: cure burn on the target if they were successfully hit (signalled by the
    // SparklingAria volatile that the 100%-secondary applied). The volatile is consumed here and
    // then removed. Sheer Force suppresses the secondary → no volatile → no cure. Sound move flag
    // (bypasssub) means the cure can happen through a Substitute as well.
    if move_data.name == PokemonMove::SparklingAria {
        for (bs, _) in branches.iter_mut() {
            if let Some(tgt) = get_pokemon_at_slot_mut(bs, target_slot) {
                if has_status_volatile(tgt, &VolatileStatus::SparklingAria) {
                    remove_status_volatile(tgt, &VolatileStatus::SparklingAria);
                    if matches!(tgt.status, Some(Status::Burn)) {
                        tgt.status = None;
                    }
                }
            }
        }
    }

    // Unconditional self-boosts
    if move_data.self_boost != [0; 7] {
        for (bs, _) in branches.iter_mut() {
            let growth_in_sun = move_data.name == PokemonMove::Growth && weather_is_sunlight(bs);
            let items_suppressed = items_are_suppressed(bs);
            let mut boosts = move_data.self_boost;
            if growth_in_sun {
                boosts[0] = boosts[0].saturating_add(1);
                boosts[2] = boosts[2].saturating_add(1);
            }
            if let Some(attacker_mon) = get_pokemon_at_slot_mut(bs, attacker_slot) {
                apply_stat_boosts_to_pokemon(attacker_mon, &boosts, items_suppressed, false);
            }
            // Opportunist: mirror any positive raise to opponents holding the ability.
            if boosts.iter().any(|&b| b > 0) {
                mirror_opportunist_raises(bs, attacker_slot, &boosts, items_suppressed);
            }
        }
    }

    // Branch self-secondaries
    for secondary in &move_data.self_secondaries {
        let chance = secondary.chance as f64 / 100.0;
        let choices = if secondary.random_choices.is_empty() {
            std::slice::from_ref(&secondary.effect)
        } else {
            &secondary.random_choices
        };
        branches = branch_on_secondary_effects(branches, chance, choices, |bs, eff| {
            apply_effect_to_attacker(bs, attacker_slot, eff);
        });
    }

    // Burn Up: after dealing damage the user loses their Fire type. If it was their only type,
    // they become typeless (empty type list). Exempt when Terastallized as a Fire type (the
    // Tera type replaces the original types, so the original Fire is not "exposed" to loss).
    if move_data.name == PokemonMove::BurnUp {
        for (bs, _) in branches.iter_mut() {
            if let Some(user) = get_pokemon_at_slot_mut(bs, attacker_slot) {
                let tera_fire = user.is_tera && user.tera_type == PokemonType::Fire;
                if !tera_fire {
                    user.types.retain(|t| *t != PokemonType::Fire);
                }
            }
        }
    }

    // Healing moves (Rest, Synthesis, etc.)
    let terrain_snapshot = state.clone();
    for (bs, _) in branches.iter_mut() {
        apply_healing_move(bs, attacker_slot, &move_data.name, &terrain_snapshot);
    }

    // Hazard removal / clearing moves (Rapid Spin, Mortal Spin, Defog, Tidy Up).
    for (bs, _) in branches.iter_mut() {
        apply_hazard_removal_move(bs, attacker_slot, &move_data.name);
    }

    // Pure-trapping moves (Block, Mean Look, Spirit Shackle): apply Trapped volatile.
    // These moves store their trap in unparsed Showdown onHit JS, so they are hand-coded.
    for (bs, _) in branches.iter_mut() {
        apply_trapping_move(bs, attacker_slot, target_slot, &move_data.name);
    }

    // Salt Cure: apply the SaltCure volatile on hit. The condition-based volatile is not
    // parsed from Showdown's JS condition block, so it is hand-coded here.
    if move_data.name == PokemonMove::SaltCure {
        for (bs, _) in branches.iter_mut() {
            let state_snapshot = bs.clone();
            if let Some(target_mon) = get_pokemon_at_slot_mut(bs, target_slot) {
                if !target_mon.fainted {
                    // SaltCure is not a mental volatile, so the Mental Herb return is always empty.
                    let _ = apply_volatile_to_pokemon(
                        &state_snapshot,
                        target_mon,
                        &VolatileStatus::SaltCure,
                        false,
                    );
                }
            }
        }
    }

    // Clear Smog: reset all of the target's stat stages to 0 on hit.
    // The onHit effect is blocked if the target was behind a Substitute (no bypasssub flag).
    if move_data.name == PokemonMove::ClearSmog {
        let target_had_sub = get_pokemon_at_slot(state, target_slot)
            .map(|m| has_status_volatile(m, &VolatileStatus::Substitute(0)))
            .unwrap_or(false);
        if !target_had_sub {
            for (bs, _) in branches.iter_mut() {
                let had_nonzero = get_pokemon_at_slot(bs, target_slot)
                    .map(|m| m.boosts.iter().any(|&b| b != 0))
                    .unwrap_or(false);
                if let Some(target_mon) = get_pokemon_at_slot_mut(bs, target_slot) {
                    if !target_mon.fainted {
                        target_mon.boosts = [0; 7];
                    }
                }
                if had_nonzero {
                    emit(bs, crate::information::information::EventKind::BoostsCleared { target: target_slot });
                }
            }
        }
    }

    // Uproar: wake all sleeping Pokémon on the field when first used.
    // Sleep prevention for subsequent turns is handled in apply_status_to_pokemon.
    if move_data.name == PokemonMove::Uproar {
        for (bs, _) in branches.iter_mut() {
            for mon in bs
                .p1_active_mons
                .iter_mut()
                .chain(bs.p2_active_mons.iter_mut())
            {
                if matches!(mon.status, Some(crate::state::dex_data::Status::Sleep(_))) {
                    mon.status = None;
                }
            }
        }
    }

    // Transform move: deterministic, no branching.
    if move_data.name == PokemonMove::Transform {
        // The default opposite slot for Transform is the directly-opposite slot index on
        // the other side (same slot_index, opposing player).
        let opp_player = match attacker_slot.player {
            Player::P1 => Player::P2,
            Player::P2 => Player::P1,
        };
        let opposite = FieldSlot {
            player: opp_player,
            slot_index: attacker_slot.slot_index,
        };
        for (bs, _) in branches.iter_mut() {
            let target_snapshot = get_pokemon_at_slot(bs, opposite).cloned();
            if let Some(target) = target_snapshot {
                let success = get_pokemon_at_slot_mut(bs, attacker_slot)
                    .map(|t| transform_into(t, &target))
                    .unwrap_or(false);
                if success {
                    let new_ability =
                        get_pokemon_at_slot(bs, attacker_slot).map(|m| m.ability.clone());
                    if let Some(ab) = new_ability {
                        // Fire the copied ability's gain effects.
                        apply_entry_ability_field_effects(bs, attacker_slot, &ab);
                        apply_entry_ability_target_effects(bs, attacker_slot, &ab);
                    }
                }
            }
        }
    }

    branches.into_iter().filter(|(_, p)| *p > 0.0).collect()
}

/// Clear all volatile statuses and non-volatile statuses from a PokÃ©mon when it faints.
pub fn clear_pokemon_on_faint(mon: &mut PokemonState) {
    mon.volatiles.clear();
    mon.status = None;
    // Rage Fist hit counter resets on faint (Champions rules).
    mon.times_hit = 0;
}

/// Check if a PokÃ©mon is immune to Rage Powder based on type, ability, or item.
/// Grass-types, PokÃ©mon with Overcoat ability, and those holding Safety Googles are immune.
/// Returns true when `mon` is immune to powder/spore moves.
///
/// `attacker` — pass the attacking Pokémon to enable Mold Breaker bypass: a Mold Breaker /
/// Turboblaze / Teravolt attacker suppresses Overcoat (it is an ignorable ability) but does
/// NOT bypass Grass-type immunity or Safety Goggles (those are type/item, not ability).
/// Pass `None` when the caller has no attacker context (e.g. Rage Powder redirect check).
pub fn is_immune_to_powder(
    state: &BattleState,
    mon: &PokemonState,
    attacker: Option<&PokemonState>,
) -> bool {
    if pokemon_has_type(mon, &PokemonType::Grass) {
        return true;
    }
    let abilities_suppressed = abilities_are_suppressed(state);
    let mold_breaks_overcoat = attacker.is_some_and(|a| attacker_breaks_mold(state, a));
    if !abilities_suppressed && !mold_breaks_overcoat && mon.ability == Ability::Overcoat {
        return true;
    }
    if item_is_active(state, mon) && matches!(mon.item, Item::SafetyGoggles) {
        return true;
    }
    false
}

/// Check if a redirect target has both Sky Drop and a Follow Me/Rage Powder effect.
/// If so, it should not redirect the move to itself.
fn has_skyrop_and_redirect(mon: &PokemonState) -> bool {
    let has_skyrop = has_status_volatile(mon, &VolatileStatus::SkyDrop);
    let has_redirect = has_status_volatile(mon, &VolatileStatus::FollowMe)
        || has_status_volatile(mon, &VolatileStatus::RagePowder);
    has_skyrop && has_redirect
}

/// Check for and apply move redirection based on Follow Me and Rage Powder volatile statuses.
/// Returns the potentially modified target_slots.
/// `move_data`: The move being used, if known. Required for Lightning Rod / Storm Drain
/// type-based redirection.  Pass `None` to skip ability-based redirection (e.g. in unit tests
/// that only exercise FollowMe / Rage Powder behaviour).
pub fn check_and_apply_redirection(
    state: &BattleState,
    user_slot: FieldSlot,
    target_slots: Vec<FieldSlot>,
    move_data: Option<&MoveData>,
) -> Vec<FieldSlot> {
    // Only apply redirection if there's exactly one target
    if target_slots.len() != 1 {
        return target_slots;
    }

    // Stalwart and Propeller Tail: ignore all target-redirecting effects (moves and abilities).
    let attacker_ignores_redirection = get_pokemon_at_slot(state, user_slot).map_or(false, |a| {
        !pokemon_ability_is_suppressed(state, a)
            && matches!(a.ability, Ability::Stalwart | Ability::PropellerTail)
    });
    if attacker_ignores_redirection {
        return target_slots;
    }

    let target_slot = target_slots[0];

    // Get the target's effective speed for tiebreaking
    let Some(_target_mon) = get_pokemon_at_slot(state, target_slot) else {
        return target_slots;
    };

    // Get the opposing team
    let opposing_mons = match user_slot.player {
        Player::P1 => &state.p2_active_mons,
        Player::P2 => &state.p1_active_mons,
    };
    let opposing_player = match user_slot.player {
        Player::P1 => Player::P2,
        Player::P2 => Player::P1,
    };

    // Rage Powder only redirects moves from attackers that are not immune to powder
    // (Grass types, Overcoat, Safety Goggles). The immunity belongs to the attacker
    // whose move is being redirected, not to the redirector.
    // No attacker context needed here — the check is on the attacker itself, and
    // Mold Breaker does not suppress an attacker's own Overcoat for redirect purposes.
    let attacker_immune_to_powder = get_pokemon_at_slot(state, user_slot)
        .map(|attacker| is_immune_to_powder(state, attacker, None))
        .unwrap_or(false);

    // --- Priority 1: FollowMe / RagePowder (volatile-based) ---
    let mut redirectors: Vec<(FieldSlot, &PokemonState)> = Vec::new();

    for (idx, mon) in opposing_mons.iter().enumerate() {
        if mon.fainted || has_skyrop_and_redirect(mon) {
            continue;
        }

        // Check for FollowMe (not a powder move, so not affected by powder immunity)
        if has_status_volatile(mon, &VolatileStatus::FollowMe) {
            redirectors.push((
                FieldSlot {
                    player: opposing_player,
                    slot_index: idx as u8,
                },
                mon,
            ));
            continue;
        }

        // Check for RagePowder (skipped if the attacker is immune to powder)
        if has_status_volatile(mon, &VolatileStatus::RagePowder) {
            if !attacker_immune_to_powder {
                redirectors.push((
                    FieldSlot {
                        player: opposing_player,
                        slot_index: idx as u8,
                    },
                    mon,
                ));
            }
        }
    }

    // FollowMe/RagePowder take priority over ability-based redirection.
    if !redirectors.is_empty() {
        let best_redirector = redirectors.into_iter().max_by(|a, b| {
            let speed_a = get_effective_speed(state, a.1);
            let speed_b = get_effective_speed(state, b.1);
            speed_a
                .partial_cmp(&speed_b)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        if let Some((slot, _)) = best_redirector {
            return vec![slot];
        }
    }

    // --- Priority 2: Lightning Rod (Electric) / Storm Drain (Water) ---
    // These draw single-target moves of the matching type toward the ability holder.
    // Mold Breaker / Turboblaze / Teravolt bypass the redirection entirely.
    if let Some(md) = move_data {
        if let Some(attacker) = get_pokemon_at_slot(state, user_slot) {
            // Mold Breaker suppresses Lightning Rod / Storm Drain on the redirector.
            if attacker_breaks_mold(state, attacker) {
                return target_slots;
            }
            let move_type = effective_move_type(state, attacker, md);
            let mut ability_redirectors: Vec<(FieldSlot, &PokemonState)> = Vec::new();

            for (idx, mon) in opposing_mons.iter().enumerate() {
                if mon.fainted {
                    continue;
                }
                if pokemon_ability_is_suppressed(state, mon) {
                    continue;
                }

                let draws = matches!(
                    (&move_type, &mon.ability),
                    (PokemonType::Electric, Ability::LightningRod)
                        | (PokemonType::Water, Ability::StormDrain)
                );
                if draws {
                    ability_redirectors.push((
                        FieldSlot {
                            player: opposing_player,
                            slot_index: idx as u8,
                        },
                        mon,
                    ));
                }
            }

            if !ability_redirectors.is_empty() {
                let best = ability_redirectors.into_iter().max_by(|a, b| {
                    let speed_a = get_effective_speed(state, a.1);
                    let speed_b = get_effective_speed(state, b.1);
                    speed_a
                        .partial_cmp(&speed_b)
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
                if let Some((slot, _)) = best {
                    return vec![slot];
                }
            }
        }
    }

    target_slots
}

pub(crate) fn get_pokemon_at_slot_mut<'a>(
    state: &'a mut BattleState,
    slot: FieldSlot,
) -> Option<&'a mut PokemonState> {
    let mons = match slot.player {
        Player::P1 => &mut state.p1_active_mons,
        Player::P2 => &mut state.p2_active_mons,
    };
    mons.get_mut(slot.slot_index as usize)
}
