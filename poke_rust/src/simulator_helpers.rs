use crate::battle::{Action, BattleState, FieldSlot, Player};
use crate::data::ability::Ability;
use crate::data::item::Item;
use crate::data::pokemon_move::PokemonMove;
use crate::data::species::Species;
use crate::dex_data::{AccuracyType, DamageOverride, MoveCategory, MoveData, MoveTarget, PokemonType, PseudoWeather, SideCondition, SelfSwitchType, Terrain, Weather, HitEffect, MoveFlag, PokemonStat, Status};
use crate::pokemon::{Nature, PokemonState, VolatileStatusState};
use crate::dex_data::VolatileStatus;
use rand::{thread_rng, Rng};
use std::sync::atomic::Ordering;
use std::collections::HashMap;
use std::hash::Hash;

pub fn get_verbosity() -> u8 {
    crate::VERBOSITY.get().copied().unwrap_or(1)
}

pub fn shared_multihit_damage_rolls_enabled() -> bool {
    crate::SHARED_MULTIHIT_DAMAGE_ROLLS.load(Ordering::Relaxed)
}

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

    fn helper<T: Clone>(items: &[T], current: &mut Vec<T>, used: &mut Vec<bool>, results: &mut Vec<Vec<T>>) {
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
    move_data.flags.iter().any(|f| std::mem::discriminant(f) == std::mem::discriminant(flag))
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
    matches!(target, MoveTarget::AllAdjacent | MoveTarget::AllAdjacentFoes)
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
    if side_conditions.iter().any(|c| matches!(c, SideCondition::QuickGuard)) {
        if let Some(attacker) = get_pokemon_at_slot(state, attacker_slot) {
            if effective_move_priority(state, attacker, move_data) > 0 {
                return Some(ProtectKind::QuickGuard);
            }
        }
    }
    if is_spread && side_conditions.iter().any(|c| matches!(c, SideCondition::WideGuard)) {
        return Some(ProtectKind::WideGuard);
    }

    // Single-target self-protects only block moves carrying the protect flag.
    if !move_has_flag(move_data, &MoveFlag::Protect) {
        return None;
    }
    let kind = target.volatiles.iter().find_map(|v| match v {
        VolatileStatusState::TurnStatus(VolatileStatus::Protect, _) => Some(ProtectKind::Protect),
        VolatileStatusState::TurnStatus(VolatileStatus::KingsShield, _) => Some(ProtectKind::KingsShield),
        VolatileStatusState::TurnStatus(VolatileStatus::SpikyShield, _) => Some(ProtectKind::SpikyShield),
        VolatileStatusState::TurnStatus(VolatileStatus::BanefulBunker, _) => Some(ProtectKind::BanefulBunker),
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
            let eff = HitEffect { status: Some(Status::Poison), ..Default::default() };
            apply_effect_to_target(state, blocker_slot, attacker_slot, &eff, attacker_slot.player);
        }
        ProtectKind::KingsShield => {
            let eff = HitEffect { boosts: [-1, 0, 0, 0, 0, 0, 0], ..Default::default() };
            apply_effect_to_target(state, blocker_slot, attacker_slot, &eff, attacker_slot.player);
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

pub fn effective_stat(state: &BattleState, mon: &PokemonState, stat: PokemonStat, ignore_negative: bool, ignore_positive: bool) -> f64 {
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
    let applied_stage = if boost > 0 && ignore_positive { 0 }
                        else if boost < 0 && ignore_negative { 0 }
                        else { boost };

    let val = base_stat * stage_multiplier(applied_stage);
    let val = apply_ability_stat_boost(state, mon, stat, PokemonStat::Atk, Ability::Guts, mon.status.is_some(), 1.5, val);
    let val = apply_ability_stat_boost(state, mon, stat, PokemonStat::Def, Ability::MarvelScale, mon.status.is_some(), 1.5, val);
    let val = apply_ability_stat_boost(state, mon, stat, PokemonStat::Def, Ability::GrassPelt, matches!(current_terrain(state), Some(Terrain::GrassyTerrain)), 1.5, val);
    let val = apply_ability_stat_boost(state, mon, stat, PokemonStat::SpA, Ability::HadronEngine, matches!(current_terrain(state), Some(Terrain::ElectricTerrain)), 5461.0 / 4096.0, val);

    // Huge Power / Pure Power: double Attack stat (unconditional; only physical moves read Atk).
    let val = apply_ability_stat_boost(state, mon, stat, PokemonStat::Atk, Ability::HugePower, true, 2.0, val);
    let val = apply_ability_stat_boost(state, mon, stat, PokemonStat::Atk, Ability::PurePower, true, 2.0, val);
    // Hustle: +50% Attack (accuracy penalty is handled separately in compute_accuracy_modifier_fp).
    let val = apply_ability_stat_boost(state, mon, stat, PokemonStat::Atk, Ability::Hustle, true, 1.5, val);

    // Light Ball: doubles Pikachu's Attack and Special Attack.
    let val = if item_is_active(state, mon)
        && mon.item == Item::LightBall
        && matches!(mon.species,
            Species::Pikachu | Species::PikachuAlola | Species::PikachuBelle
            | Species::PikachuCosplay | Species::PikachuGmax | Species::PikachuHoenn
            | Species::PikachuKalos | Species::PikachuLibre | Species::PikachuOriginal
            | Species::PikachuPartner | Species::PikachuPhD | Species::PikachuPopStar
            | Species::PikachuRockStar | Species::PikachuSinnoh | Species::PikachuStarter
            | Species::PikachuUnova | Species::PikachuWorld)
        && (stat == PokemonStat::Atk || stat == PokemonStat::SpA)
    { val * 2.0 } else { val };

    // Choice Band: 1.5× Attack.
    let val = if item_is_active(state, mon)
        && mon.item == Item::ChoiceBand
        && stat == PokemonStat::Atk
    { val * 1.5 } else { val };

    // Choice Specs: 1.5× Special Attack.
    let val = if item_is_active(state, mon)
        && mon.item == Item::ChoiceSpecs
        && stat == PokemonStat::SpA
    { val * 1.5 } else { val };

    val
}

pub fn pokemon_has_type(mon: &PokemonState, pokemon_type: &PokemonType) -> bool {
    mon.types.iter().any(|current_type| std::mem::discriminant(current_type) == std::mem::discriminant(pokemon_type))
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
        (Grass, Fire) | (Grass, Grass) | (Grass, Poison) | (Grass, Flying) | (Grass, Bug) | (Grass, Dragon) | (Grass, Steel) => 0.5,

        (Ice, Grass) | (Ice, Ground) | (Ice, Flying) | (Ice, Dragon) => 2.0,
        (Ice, Fire) | (Ice, Water) | (Ice, Ice) | (Ice, Steel) => 0.5,

        (Fighting, Normal) | (Fighting, Ice) | (Fighting, Rock) | (Fighting, Dark) | (Fighting, Steel) => 2.0,
        (Fighting, Poison) | (Fighting, Flying) | (Fighting, Psychic) | (Fighting, Bug) | (Fighting, Fairy) => 0.5,
        (Fighting, Ghost) => 0.0,

        (Poison, Grass) | (Poison, Fairy) => 2.0,
        (Poison, Poison) | (Poison, Ground) | (Poison, Rock) | (Poison, Ghost) => 0.5,
        (Poison, Steel) => 0.0,

        (Ground, Fire) | (Ground, Electric) | (Ground, Poison) | (Ground, Rock) | (Ground, Steel) => 2.0,
        (Ground, Grass) | (Ground, Bug) => 0.5,
        (Ground, Flying) => 0.0,

        (Flying, Grass) | (Flying, Fighting) | (Flying, Bug) => 2.0,
        (Flying, Electric) | (Flying, Rock) | (Flying, Steel) => 0.5,

        (Psychic, Fighting) | (Psychic, Poison) => 2.0,
        (Psychic, Psychic) | (Psychic, Steel) => 0.5,
        (Psychic, Dark) => 0.0,

        (Bug, Grass) | (Bug, Psychic) | (Bug, Dark) => 2.0,
        (Bug, Fire) | (Bug, Fighting) | (Bug, Poison) | (Bug, Flying) | (Bug, Ghost) | (Bug, Steel) | (Bug, Fairy) => 0.5,

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
pub fn defensive_types(mon: &PokemonState) -> Vec<PokemonType> {
    if has_status_volatile(mon, &VolatileStatus::Roost) && pokemon_has_type(mon, &PokemonType::Flying) {
        let remaining: Vec<PokemonType> = mon
            .types
            .iter()
            .filter(|t| !matches!(t, PokemonType::Flying))
            .cloned()
            .collect();
        if remaining.is_empty() { vec![PokemonType::Normal] } else { remaining }
    } else {
        mon.types.clone()
    }
}

pub fn move_type_effectiveness(state: &BattleState, move_type: &PokemonType, target: &PokemonState) -> f64 {
    let target_types = defensive_types(target);
    if target_types.is_empty() {
        return 1.0;
    }

    target_types.iter().fold(1.0, |effectiveness, target_type| {
        let mut type_effectiveness = single_type_effectiveness(move_type, target_type);
        if weather_is_strong_winds(state)
            && matches!(target_type, PokemonType::Flying)
            && matches!(move_type, PokemonType::Electric | PokemonType::Ice | PokemonType::Rock)
            && (type_effectiveness - 2.0).abs() < f64::EPSILON
        {
            type_effectiveness = 1.0;
        }
        effectiveness * type_effectiveness
    })
}

pub fn stab_multiplier(attacker: &PokemonState, move_type: &PokemonType) -> f64 {
    if !pokemon_has_type(attacker, move_type) && (!attacker.is_tera || attacker.tera_type != *move_type) {
        return 1.0;
    }

    let has_adaptability = attacker.ability == Ability::Adaptability;
    let matches_original_type = pokemon_has_type(attacker, move_type);
    let matches_tera_type = attacker.is_tera && attacker.tera_type == *move_type;
    let tera_type_matches_original = attacker.is_tera && pokemon_has_type(attacker, &attacker.tera_type);

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

pub fn crit_is_guaranteed(attacker: &PokemonState, target: &PokemonState, move_name: &PokemonMove) -> bool {
    let target_is_poisoned = matches!(target.status, Some(Status::Poison) | Some(Status::ToxicPoison(_)));
    let merciless_crit = attacker.ability == Ability::Merciless && target_is_poisoned;
    let laser_focus = attacker.volatiles.iter().any(|volatile| matches!(volatile, VolatileStatusState::TurnStatus(VolatileStatus::LaserFocus, _)) || matches!(volatile, VolatileStatusState::MoveStatus(VolatileStatus::LaserFocus, _)));
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

/// Returns the effective crit ratio after applying held-item boosts (e.g. Scope Lens).
fn effective_crit_ratio(state: &BattleState, attacker: &PokemonState, base: u8) -> u8 {
    if item_is_active(state, attacker) && attacker.item == Item::ScopeLens {
        base.saturating_add(1)
    } else {
        base
    }
}

pub fn critical_hit_probability(
    attacker: &PokemonState,
    target: &PokemonState,
    move_name: &PokemonMove,
    consider_crit: bool,
    crit_ratio: u8,
) -> Vec<(bool, f64)> {
    if !consider_crit {
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

    let crit_chance = match crit_ratio {
        6 => 1.0,
        5 => 0.5,
        4 => 1.0 / 3.0,
        3 => 0.25,
        2 => 0.125,
        _ => 1.0 / 24.0,
    };

    vec![(false, 1.0 - crit_chance), (true, crit_chance)]
}

fn screen_damage_multiplier(state: &BattleState, target_slot: FieldSlot, move_data: &MoveData, is_crit: bool) -> f64 {
    if is_crit {
        return 1.0;
    }

    let target_side_conditions = match target_slot.player {
        Player::P1 => &state.p1_side_conditions,
        Player::P2 => &state.p2_side_conditions,
    };

    let is_physical = matches!(move_data.category, MoveCategory::Physical);
    let is_special = matches!(move_data.category, MoveCategory::Special);

    let has_reflect = target_side_conditions.iter().any(|condition| matches!(condition, SideCondition::Reflect));
    let has_light_screen = target_side_conditions.iter().any(|condition| matches!(condition, SideCondition::LightScreen));
    let has_aurora_veil = target_side_conditions.iter().any(|condition| matches!(condition, SideCondition::AuroraVeil));

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
fn collect_active_slots(state: &BattleState, player: Player, exclude: Option<u8>) -> Vec<FieldSlot> {
    let mons = match player {
        Player::P1 => &state.p1_active_mons,
        Player::P2 => &state.p2_active_mons,
    };
    mons.iter().enumerate()
        .filter(|(idx, mon)| !mon.fainted && exclude.map_or(true, |ex| *idx as u8 != ex))
        .map(|(idx, _)| FieldSlot { player, slot_index: idx as u8 })
        .collect()
}

pub fn resolve_move_targets(
    state: &BattleState,
    user_slot: FieldSlot,
    target: &MoveTarget,
) -> Vec<FieldSlot> {
    let foe = match user_slot.player { Player::P1 => Player::P2, Player::P2 => Player::P1 };

    match target {
        // Single-target foe — fallback: first healthy opponent
        MoveTarget::AdjacentFoe | MoveTarget::Normal | MoveTarget::Any => {
            collect_active_slots(state, foe, None).into_iter().take(1).collect()
        }
        // All adjacent foes / foe side
        MoveTarget::AllAdjacentFoes | MoveTarget::FoeSide => {
            collect_active_slots(state, foe, None)
        }
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
                state, user_slot.player,
                if exclude_self { Some(user_slot.slot_index) } else { None },
            );
            slots.extend(collect_active_slots(state, foe, None));
            slots
        }
        // Self-target
        MoveTarget::SelfTarget | MoveTarget::AdjacentAllyOrSelf => {
            vec![user_slot]
        }
        // Fallback: first healthy opponent
        _ => {
            collect_active_slots(state, foe, None).into_iter().take(1).collect()
        }
    }
}

pub fn damage_targets_multiplier(target_count: usize) -> f64 {
    if target_count > 1 { 0.75 } else { 1.0 }
}

/// The move's type after applying only its *own* conditional mechanics (Weather Ball, Terrain
/// Pulse, etc.), but *before* any ability-based type conversion (Aerilate, Liquid Voice, …).
fn natural_move_type(state: &BattleState, attacker: &PokemonState, move_data: &MoveData) -> PokemonType {
    match move_data.name {
        PokemonMove::WeatherBall => match current_weather(state) {
            Some(Weather::Sun | Weather::ExtremeSunlight) => PokemonType::Fire,
            Some(Weather::Rain | Weather::HeavyRain) => PokemonType::Water,
            Some(Weather::Sandstorm) => PokemonType::Rock,
            Some(Weather::Snow) => PokemonType::Ice,
            _ => PokemonType::Normal,
        },
        PokemonMove::TerrainPulse if pokemon_is_grounded(state, attacker) => match current_terrain(state) {
            Some(Terrain::ElectricTerrain) => PokemonType::Electric,
            Some(Terrain::GrassyTerrain) => PokemonType::Grass,
            Some(Terrain::MistyTerrain) => PokemonType::Fairy,
            Some(Terrain::PsychicTerrain) => PokemonType::Psychic,
            _ => move_data.pokemon_type.clone(),
        },
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
        Ability::Aerilate    => Some(PokemonType::Flying),
        Ability::Pixilate    => Some(PokemonType::Fairy),
        Ability::Refrigerate => Some(PokemonType::Ice),
        Ability::Dragonize   => Some(PokemonType::Dragon),
        Ability::Galvanize   => Some(PokemonType::Electric),
        _ => None,
    }
}

/// Returns `true` iff the holder's -ate ability will actually convert `move_data` this use.
/// This single predicate drives *both* the type change and the 1.2× power boost — they must
/// be tied together so the boost only fires when the type actually changes.
fn ate_ability_converts(state: &BattleState, attacker: &PokemonState, move_data: &MoveData) -> bool {
    if pokemon_ability_is_suppressed(state, attacker) { return false; }
    if ate_ability_target_type(&attacker.ability).is_none() { return false; }
    // Tera Blast sets its own type while Terastallized; skip conversion then.
    if move_data.name == PokemonMove::TeraBlast && attacker.is_tera { return false; }
    // Moves whose own type-setting is unmodeled — skip to avoid wrong conversions.
    if ate_typeset_unmodeled(&move_data.name) { return false; }
    // Convert only when the move's own-effect-resolved type is still Normal.
    // This naturally handles Weather Ball (non-Normal in weather) and Terrain Pulse
    // (non-Normal on grounded + active terrain) without any special-casing.
    matches!(natural_move_type(state, attacker, move_data), PokemonType::Normal)
}

pub(crate) fn effective_move_type(state: &BattleState, attacker: &PokemonState, move_data: &MoveData) -> PokemonType {
    let base = natural_move_type(state, attacker, move_data);
    if pokemon_ability_is_suppressed(state, attacker) { return base; }
    // Liquid Voice: any sound-based move → Water (no power boost).
    if attacker.ability == Ability::LiquidVoice && move_has_flag(move_data, &MoveFlag::Sound) {
        return PokemonType::Water;
    }
    // -ate abilities: Normal-typed moves → the ability's target type.
    if ate_ability_converts(state, attacker, move_data) {
        return ate_ability_target_type(&attacker.ability).unwrap();
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
    if move_data.name == PokemonMove::GrassyGlide && pokemon_is_on_terrain(state, user, &Terrain::GrassyTerrain) {
        boost += 1;
    }

    if !pokemon_ability_is_suppressed(state, user) {
        // Prankster: status moves get +1 priority.
        if user.ability == Ability::Prankster && matches!(move_data.category, MoveCategory::Status) {
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
pub(crate) fn effective_move_priority(state: &BattleState, user: &PokemonState, move_data: &MoveData) -> i8 {
    move_data.priority + effective_priority_boost(state, user, move_data)
}

// ── Damage-calculation sub-helpers ────────────────────────────────────────────

/// Apply SolarPower / OrichalcumPulse attack boosts.
fn apply_weather_attack_boost(state: &BattleState, attacker: &PokemonState, attacking_stat: PokemonStat, stat: f64) -> f64 {
    let mut stat = stat;
    if matches!(attacking_stat, PokemonStat::SpA)
        && attacker.ability == Ability::SolarPower
        && weather_is_sunlight(state)
    {
        stat = (stat * 1.5).floor();
    }
    if matches!(attacking_stat, PokemonStat::Atk)
        && attacker.ability == Ability::OrichalcumPulse
        && weather_is_sunlight(state)
    {
        stat = (stat * 5461.0 / 4096.0).floor();
    }
    stat
}

/// Apply sandstorm (+Rock SpD) and snow (+Ice Def) weather defense bonuses.
fn apply_weather_defense_bonus(state: &BattleState, target: &PokemonState, defending_stat: PokemonStat, defense: f64) -> f64 {
    let mut defense = defense;
    if matches!(defending_stat, PokemonStat::SpD) && weather_is_sandstorm(state) && pokemon_has_type(target, &PokemonType::Rock) {
        defense *= 1.5;
    }
    if matches!(defending_stat, PokemonStat::Def) && weather_is_snow(state) && pokemon_has_type(target, &PokemonType::Ice) {
        defense *= 1.5;
    }
    defense
}

/// Terrain-type ×1.3 base-power boost for the attacker's move type.
fn terrain_type_bp_boost(state: &BattleState, attacker: &PokemonState, move_type: &PokemonType) -> f64 {
    if pokemon_is_on_terrain(state, attacker, &Terrain::ElectricTerrain) && matches!(move_type, PokemonType::Electric) {
        return 1.3;
    }
    if pokemon_is_on_terrain(state, attacker, &Terrain::GrassyTerrain) && matches!(move_type, PokemonType::Grass) {
        return 1.3;
    }
    if pokemon_is_on_terrain(state, attacker, &Terrain::PsychicTerrain) && matches!(move_type, PokemonType::Psychic) {
        return 1.3;
    }
    1.0
}

/// Per-move terrain multiplier (ExpandingForce, MistyExplosion, Psyblade, TerrainPulse, RisingVoltage, ground moves).
fn move_terrain_bp_modifier(state: &BattleState, attacker: &PokemonState, target: &PokemonState, move_data: &MoveData) -> f64 {
    match move_data.name {
        PokemonMove::ExpandingForce if pokemon_is_on_terrain(state, attacker, &Terrain::PsychicTerrain) => 1.5,
        PokemonMove::MistyExplosion if pokemon_is_on_terrain(state, attacker, &Terrain::MistyTerrain) => 1.5,
        PokemonMove::Psyblade if pokemon_is_on_terrain(state, attacker, &Terrain::ElectricTerrain) => 1.5,
        PokemonMove::TerrainPulse if pokemon_is_grounded(state, attacker) && current_terrain(state).is_some() => 2.0,
        PokemonMove::RisingVoltage if pokemon_is_on_terrain(state, target, &Terrain::ElectricTerrain) => 2.0,
        PokemonMove::Bulldoze | PokemonMove::Earthquake | PokemonMove::Magnitude
            if matches!(current_terrain(state), Some(Terrain::GrassyTerrain)) => 0.5,
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
                if r >= 4.0 { 150 } else if r >= 3.0 { 120 } else if r >= 2.0 { 80 } else if r >= 1.0 { 60 } else { 40 }
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
        // Target-weight table (weight_hg = kg × 10).
        // TODO: respect Heavy Metal / Light Metal / Float Stone once implemented.
        M::GrassKnot | M::LowKick => {
            let hg = target.weight_hg;
            Some(if hg >= 2000 { 120 }
                else if hg >= 1000 { 100 }
                else if hg >= 500 { 80 }
                else if hg >= 250 { 60 }
                else if hg >= 100 { 40 }
                else { 20 })
        }
        M::HardPress => {
            let max_hp = target.stats[0].max(1) as u32;
            Some(((100 * target.hp as u32 / max_hp) as u16).max(1))
        }
        // Weight-ratio table. TODO: ×2 damage + bypass accuracy vs Minimized targets
        // once Minimize exists (Protect-variants TODO group).
        M::HeatCrash | M::HeavySlam => {
            let user_w = attacker.weight_hg as u32;
            let target_w = target.weight_hg.max(1) as u32;
            Some(if user_w >= 5 * target_w { 120 }
                else if user_w >= 4 * target_w { 100 }
                else if user_w >= 3 * target_w { 80 }
                else if user_w >= 2 * target_w { 60 }
                else { 40 })
        }
        // ── Conditionally scaled (×2 of the dex base power) ────────────────────────
        // A consumed Flying Gem etc. counts naturally: the item is already None here.
        M::Acrobatics => Some(if attacker.item == Item::None { bp * 2 } else { bp }),
        // Non-volatile status on the target.
        M::Hex | M::InfernalParade => Some(if target.status.is_some() { bp * 2 } else { bp }),
        // Target took any damage earlier this turn (direct or indirect).
        M::Assurance => Some(if target.damaged_this_turn { bp * 2 } else { bp }),
        // This specific target damaged the user earlier this turn.
        M::Avalanche => Some(if attacker.damaged_by_this_turn.contains(&target_slot) { bp * 2 } else { bp }),
        // Any of the user's stats actually fell this turn.
        M::LashOut => Some(if attacker.stats_lowered_this_turn { bp * 2 } else { bp }),
        // Doubled when the target has already taken its action this turn and didn't
        // just switch in (mirrors Showdown's `newlyActive || willMove` check). A switch
        // consumes the target's queued action, so the switched_in flag is what separates
        // "moved already" from "replaced its action with a switch".
        M::Payback => {
            let doubled = target_has_acted_this_turn(state, target_slot) && !target.switched_in_this_turn;
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
            let fainted = active.iter().chain(back.iter()).filter(|m| m.fainted).count() as u16;
            Some(50 + 50 * fainted)
        }
        // 20 + 20 per positive boost stage across all seven stats (max 860).
        M::StoredPower | M::PowerTrip => {
            let stages: u16 = attacker.boosts.iter().map(|&b| b.max(0) as u16).sum();
            Some(20 + 20 * stages)
        }
        // Doubled when the user's previous move missed, had no effect, or was
        // prevented (paralysis / sleep / flinch). Recharging does not count.
        M::StompingTantrum | M::TemperFlare => {
            Some(if attacker.last_move_failed { bp * 2 } else { bp })
        }
        _ => None,
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
        if current_weather(state).is_some() { 100.0 } else { 50.0 }
    } else {
        move_data.base_power as f64
    };

    if move_data.name == PokemonMove::Facade && attacker.status.is_some() {
        bp = (move_data.base_power as f64 * 2.0).floor();
    }

    if matches!(move_data.name, PokemonMove::SolarBeam | PokemonMove::SolarBlade)
        && !weather_is_sunlight(state)
        && !weather_is_strong_winds(state)
        && current_weather(state).is_some()
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

    if attacker.ability == Ability::Reckless
        && (move_data.recoil_fraction[0] > 0 && move_data.recoil_fraction[1] > 0 || move_data.struggle_recoil)
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
        // Tough Claws: 1.3× contact moves.
        if attacker.ability == Ability::ToughClaws && move_has_flag(move_data, &MoveFlag::Contact) {
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
            && matches!(effective_move_type(state, attacker, move_data), PokemonType::Water)
        {
            bp = (bp * 2.0).floor();
        }
    }

    // HydroSteam in sun: BP boost (no accompanying damage-type penalty)
    if move_data.name == PokemonMove::HydroSteam && weather_is_sunlight(state) {
        bp = (bp * 1.5).floor();
    }

    // Helping Hand boosts the user's next move by 50%
    if attacker.volatiles.iter().any(|v| matches!(v,
        VolatileStatusState::TurnStatus(VolatileStatus::HelpingHand, _)
        | VolatileStatusState::MoveStatus(VolatileStatus::HelpingHand, _)
    )) {
        bp = (bp * 1.5).floor();
    }

    // Condition / stat power boosts (grouped together; suppression guard is shared).
    if !pokemon_ability_is_suppressed(state, attacker) {
        // Rivalry: ×1.25 same gender, ×0.75 opposite gender, ×1.0 if either is Genderless.
        use crate::pokemon::PokemonGender;
        let rivalry_mult = match (attacker.gender, target.gender) {
            (PokemonGender::Male, PokemonGender::Male)
            | (PokemonGender::Female, PokemonGender::Female) if attacker.ability == Ability::Rivalry => 1.25,
            (PokemonGender::Male, PokemonGender::Female)
            | (PokemonGender::Female, PokemonGender::Male) if attacker.ability == Ability::Rivalry => 0.75,
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
                (Ability::Blaze,   PokemonType::Fire)  => 1.5,
                (Ability::Overgrow, PokemonType::Grass) => 1.5,
                (Ability::Swarm,   PokemonType::Bug)   => 1.5,
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
            let fainted = attacker.volatiles.iter().find_map(|v| {
                if let VolatileStatusState::TurnStatus(VolatileStatus::SupremeOverlord(n), _) = v {
                    Some(*n)
                } else {
                    None
                }
            }).unwrap_or(0);
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
fn weather_damage_multiplier(state: &BattleState, move_data: &MoveData, attack_type: &PokemonType) -> f64 {
    let Some(weather) = current_weather(state) else { return 1.0; };
    match weather {
        Weather::Sun | Weather::ExtremeSunlight => {
            if move_data.name == PokemonMove::HydroSteam { 1.0 }
            else if matches!(attack_type, PokemonType::Fire) { 1.5 }
            else if matches!(attack_type, PokemonType::Water) { 0.5 }
            else { 1.0 }
        }
        Weather::Rain | Weather::HeavyRain => {
            if matches!(attack_type, PokemonType::Fire) { 0.5 }
            else if matches!(attack_type, PokemonType::Water) { 1.5 }
            else { 1.0 }
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
    if target.ability == Ability::DrySkin && matches!(attack_type, PokemonType::Fire) { 1.25 } else { 1.0 }
}

// ──── Defender-side damage reduction abilities ─────────────────────────────

/// Filter / Solid Rock: ×0.75 damage from super-effective hits.
fn filter_solidrock_mult(state: &BattleState, target: &PokemonState, effectiveness: f64) -> f64 {
    if effectiveness > 1.0
        && !pokemon_ability_is_suppressed(state, target)
        && matches!(target.ability, Ability::Filter | Ability::SolidRock | Ability::PrismArmor)
    { 0.75 } else { 1.0 }
}

/// Multiscale / Shadow Shield: ×0.5 damage when the holder is at full HP.
fn multiscale_mult(state: &BattleState, target: &PokemonState) -> f64 {
    if !pokemon_ability_is_suppressed(state, target)
        && matches!(target.ability, Ability::Multiscale | Ability::ShadowShield)
        && target.hp == target.stats[0].max(1)
    { 0.5 } else { 1.0 }
}

/// Fur Coat: ×0.5 damage from Physical moves.
fn fur_coat_mult(state: &BattleState, target: &PokemonState, move_data: &MoveData) -> f64 {
    if !pokemon_ability_is_suppressed(state, target)
        && target.ability == Ability::FurCoat
        && matches!(move_data.category, MoveCategory::Physical)
    { 0.5 } else { 1.0 }
}

/// Defender type-based damage reduction abilities.  Each ability halves damage from one or
/// two attacking types; they compose multiplicatively if somehow stacked.
///
/// - Heatproof       → ×0.5 vs Fire
/// - Thick Fat       → ×0.5 vs Fire and ×0.5 vs Ice
/// - Water Bubble    → ×0.5 vs Fire (defensive half; offensive ×2 Water is in base-power)
/// - Purifying Salt  → ×0.5 vs Ghost
fn defender_type_reduction_mult(state: &BattleState, target: &PokemonState, attack_type: &PokemonType) -> f64 {
    if pokemon_ability_is_suppressed(state, target) {
        return 1.0;
    }
    let mut mult = 1.0f64;
    match target.ability {
        Ability::Heatproof => {
            if matches!(attack_type, PokemonType::Fire) { mult *= 0.5; }
        }
        Ability::ThickFat => {
            if matches!(attack_type, PokemonType::Fire | PokemonType::Ice) { mult *= 0.5; }
        }
        Ability::WaterBubble => {
            if matches!(attack_type, PokemonType::Fire) { mult *= 0.5; }
        }
        Ability::PurifyingSalt => {
            if matches!(attack_type, PokemonType::Ghost) { mult *= 0.5; }
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
    let ally_count = target_side.iter().enumerate()
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
        Item::BlackBelt    => PokemonType::Fighting,
        Item::BlackGlasses => PokemonType::Dark,
        Item::Charcoal     => PokemonType::Fire,
        Item::DragonFang   => PokemonType::Dragon,
        Item::FairyFeather => PokemonType::Fairy,
        Item::HardStone    => PokemonType::Rock,
        Item::Magnet       => PokemonType::Electric,
        Item::MetalCoat    => PokemonType::Steel,
        Item::MiracleSeed  => PokemonType::Grass,
        Item::MysticWater  => PokemonType::Water,
        Item::NeverMeltIce => PokemonType::Ice,
        Item::PoisonBarb   => PokemonType::Poison,
        Item::SharpBeak    => PokemonType::Flying,
        Item::SilkScarf    => PokemonType::Normal,
        Item::SilverPowder => PokemonType::Bug,
        Item::SoftSand     => PokemonType::Ground,
        Item::SpellTag     => PokemonType::Ghost,
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

// ──── Type-resist berries (0.5× on super-effective hit, then consumed) ────────

/// Maps a type-resist berry to the attacking type it weakens.
/// Chilan Berry is handled separately: triggers on any Normal-type hit (not just SE).
fn resist_berry_type(item: &Item) -> Option<PokemonType> {
    Some(match item {
        Item::BabiriBerry => PokemonType::Steel,
        Item::ChartiBerry => PokemonType::Rock,
        Item::ChopleBerry => PokemonType::Fighting,
        Item::CobaBerry   => PokemonType::Flying,
        Item::ColburBerry => PokemonType::Dark,
        Item::HabanBerry  => PokemonType::Dragon,
        Item::KasibBerry  => PokemonType::Ghost,
        Item::KebiaBerry  => PokemonType::Poison,
        Item::OccaBerry   => PokemonType::Fire,
        Item::PasshoBerry => PokemonType::Water,
        Item::PayapaBerry => PokemonType::Psychic,
        Item::RindoBerry  => PokemonType::Grass,
        Item::RoseliBerry => PokemonType::Fairy,
        Item::ShucaBerry  => PokemonType::Ground,
        Item::TangaBerry  => PokemonType::Bug,
        Item::WacanBerry  => PokemonType::Electric,
        Item::YacheBerry  => PokemonType::Ice,
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
    if resist_berry_triggers(target, attack_type, effectiveness) { 0.5 } else { 1.0 }
}

// ──── Status-cure berries ─────────────────────────────────────────────────────

/// If `mon` holds a status-cure berry matching its current status or confusion,
/// cure the condition and consume the berry (set item to None).
/// Must be called with the current item-suppression state.
/// Called after any successful berry consumption. Centralises post-eat side-effects.
pub(crate) fn on_berry_eaten(mon: &mut PokemonState, _eaten: &Item, env: &BerryEnv) {
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

pub(crate) fn try_consume_status_cure_berry(mon: &mut PokemonState, env: &BerryEnv) {
    if env.suppressed {
        return;
    }
    let cures_status = matches!(
        (&mon.item, &mon.status),
        (Item::AspearBerry,  Some(Status::Frozen(_)))
      | (Item::CheriBerry,   Some(Status::Paralysis))
      | (Item::ChestoBerry,  Some(Status::Sleep(_)))
      | (Item::PechaBerry,   Some(Status::Poison | Status::ToxicPoison(_)))
      | (Item::RawstBerry,   Some(Status::Burn))
      | (Item::LumBerry,     Some(_))
    );
    let cures_confusion = is_confused(mon)
        && matches!(mon.item, Item::PersimBerry | Item::LumBerry);
    if cures_status {
        mon.status = None;
    }
    if cures_confusion {
        remove_status_volatile(mon, &VolatileStatus::Confusion);
    }
    if cures_status || cures_confusion {
        mon.consumed_item = Some(mon.item.clone());
        mon.item = Item::None;
    }
}

/// Call this whenever a Pokémon gains or re-enables a held item (e.g. Trick/Switcheroo,
/// Magic Room lifting). Triggers any immediate item effects such as status-cure berries.
/// Future item-gain moves (Recycle, Symbiosis, Pickup) should route through here too.
pub(crate) fn on_item_obtained_or_enabled(mon: &mut PokemonState, env: &BerryEnv) {
    try_consume_status_cure_berry(mon, env);
}

/// The canonical `(2L/5+2)*BP*Atk/Def/50+2` formula with floor after each step.
fn base_damage_formula(level: u8, bp: f64, attack: f64, defense: f64) -> f64 {
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
    let Some(attacking_stat) = move_offensive_stat(move_data) else {
        return vec![(0, false, 1.0)];
    };
    let Some(defending_stat) = move_defensive_stat(move_data) else {
        return vec![(0, false, 1.0)];
    };

    // Pre-compute values that don't change per crit branch.
    let base_attack  = apply_weather_attack_boost(_state, attacker, attacking_stat,
                           effective_stat(_state, attacker, attacking_stat, false, false));
    let base_defense = apply_weather_defense_bonus(_state, target, defending_stat,
                           effective_stat(_state, target, defending_stat, false, false));
    let attack_type  = effective_move_type(_state, attacker, move_data);
    let effectiveness = move_type_effectiveness(_state, &attack_type, target);

    // Fixed-damage moves: bypass the base-power formula entirely.
    // Type immunity still applies (e.g. Night Shade/Ghost vs Normal → 0),
    // as does the invulnerability multiplier. No crit / spread / roll scaling.
    let fixed_damage = match move_data.damage_override {
        DamageOverride::Number(n) => Some(n),
        DamageOverride::Level    => Some(attacker.level as u16),
        DamageOverride::None     => None,
    };
    if let Some(amount) = fixed_damage {
        let dmg = if effectiveness > 0.0 && invulnerability_multiplier > 0.0 { amount } else { 0 };
        return vec![(dmg, false, 1.0)];
    }

    // Struggle is typeless (???): neutral vs every type, no STAB, hits Ghost.
    // The parser stores its type as Normal — override both effectiveness and STAB here.
    let is_struggle = move_data.name == crate::data::pokemon_move::PokemonMove::Struggle;
    let effectiveness = if is_struggle { 1.0 } else { effectiveness };
    let stab = if is_struggle { 1.0 } else { stab_multiplier(attacker, &attack_type) };
    // Variable-BP moves (Electro Ball, Flail, Assurance, …) compute their power here;
    // explicit overrides (multi-hit per-hit powers) take precedence.
    let base_power_override = base_power_override.or_else(||
        variable_move_base_power(_state, attacker, target, _user_slot, _target_slot, move_data));
    let bp            = effective_base_power(_state, attacker, target, move_data, base_power_override);

    // A genuinely 0-BP hit deals 0 damage — no phantom +2 from the formula,
    // and no min-1 clamp. This covers moves with basePower: 0 and no override.
    if bp == 0.0 {
        return vec![(0, false, 1.0)];
    }
    let weather_mult  = weather_damage_multiplier(_state, move_data, &attack_type);
    let burn_mult     = burn_damage_multiplier(attacker, move_data);
    let dry_skin_mult = dry_skin_fire_multiplier(target, &attack_type);
    let type_boost_mult = if !item_is_active(_state, attacker) { 1.0 }
        else { type_boost_item_multiplier(attacker, &attack_type) };
    let resist_berry_mult = if !item_is_active(_state, target) { 1.0 }
        else {
            let base = resist_berry_multiplier(target, &attack_type, effectiveness);
            // Ripen halves the resist-berry multiplier again (½ → ¼).
            if base < 1.0
                && !pokemon_ability_is_suppressed(_state, target)
                && target.ability == Ability::Ripen
            { base / 2.0 } else { base }
        };
    let screen_mult   = screen_damage_multiplier(_state, _target_slot, move_data, false); // overridden per-crit below

    // Analytic: ×1.3 when the attacker is the very last mover this turn.
    let analytic_mult = if !pokemon_ability_is_suppressed(_state, attacker)
        && attacker.ability == Ability::Analytic
        && attacker_is_last_mover(_state, _user_slot)
    { 1.3 } else { 1.0 };

    // Fairy Aura: ×5448/4096 (~1.33) to all Fairy-type moves for any Pokémon on the field
    // when any active mon carries the ability.  Non-stacking.  Aura Break inverts to ×4096/5448.
    let aura_mult = if matches!(attack_type, PokemonType::Fairy) {
        let has_fairy_aura = _state.p1_active_mons.iter().chain(_state.p2_active_mons.iter())
            .any(|mon| !mon.fainted && !pokemon_ability_is_suppressed(_state, mon) && mon.ability == Ability::FairyAura);
        let has_aura_break = _state.p1_active_mons.iter().chain(_state.p2_active_mons.iter())
            .any(|mon| !mon.fainted && !pokemon_ability_is_suppressed(_state, mon) && mon.ability == Ability::AuraBreak);
        if has_fairy_aura && has_aura_break { 4096.0 / 5448.0 }
        else if has_fairy_aura              { 5448.0 / 4096.0 }
        else                                { 1.0 }
    } else { 1.0 };

    // ── Defender-side damage reduction abilities ──────────────────────────────
    // Filter / Solid Rock / Prism Armor: ×0.75 from super-effective hits.
    let filter_solidrock_mult = filter_solidrock_mult(_state, target, effectiveness);
    // Multiscale / Shadow Shield: ×0.5 when the target is at full HP.
    let multiscale_mult = multiscale_mult(_state, target);
    // Fur Coat: ×0.5 from Physical moves.
    let fur_coat_mult = fur_coat_mult(_state, target, move_data);
    // Heatproof / Thick Fat / Water Bubble / Purifying Salt: type-keyed ×0.5.
    let defender_type_mult = defender_type_reduction_mult(_state, target, &attack_type);
    // Friend Guard: ×0.75 per unsuppressed, non-fainted ally carrying the ability.
    let friend_guard_mult = friend_guard_mult(_state, _target_slot);

    let rolls   = forced_damage_roll.map(|r| vec![r]).unwrap_or_else(|| selected_damage_rolls(config.damage_rolls));
    let crits   = critical_hit_probability(attacker, target, &move_data.name, config.consider_crit, effective_crit_ratio(_state, attacker, move_data.crit_ratio));

    let mut outcomes = Vec::new();

    for (is_crit, crit_prob) in crits {
        let crit_mult = if is_crit {
            if attacker.ability == Ability::Sniper { 2.25 } else { 1.5 }
        } else { 1.0 };

        // On a crit, re-compute attack/defense ignoring unfavourable boosts.
        let attack_stat = if is_crit {
            apply_weather_attack_boost(_state, attacker, attacking_stat,
                effective_stat(_state, attacker, attacking_stat, true, false))
        } else { base_attack };
        let defense_stat = if is_crit {
            apply_weather_defense_bonus(_state, target, defending_stat,
                effective_stat(_state, target, defending_stat, false, true))
        } else { base_defense };

        let this_screen_mult = if is_crit { 1.0 } else { screen_mult };
        let base_dmg = base_damage_formula(attacker.level, bp, attack_stat, defense_stat);

        for &roll in &rolls {
            let mut dmg = base_dmg;
            dmg = (dmg * targets_multiplier).floor();
            dmg = (dmg * crit_mult).floor();
            dmg = (dmg * (roll as f64 / 100.0)).floor();
            dmg = (dmg * stab).floor();
            dmg = (dmg * effectiveness).floor();
            dmg = (dmg * resist_berry_mult).floor();  // type-resist berry halves after type effectiveness
            dmg = (dmg * this_screen_mult).floor();
            dmg = (dmg * burn_mult).floor();
            dmg = (dmg * invulnerability_multiplier).floor();
            dmg = (dmg * weather_mult).floor();
            dmg = (dmg * dry_skin_mult).floor();
            dmg = (dmg * type_boost_mult).floor();    // type-boosting item in the "other" multiplier bucket
            dmg = (dmg * analytic_mult).floor();      // Analytic: ×1.3 when moving last
            dmg = (dmg * aura_mult).floor();          // Fairy Aura / Aura Break field effect
            // Defender-side damage reduction abilities:
            dmg = (dmg * filter_solidrock_mult).floor(); // Filter / Solid Rock / Prism Armor
            dmg = (dmg * multiscale_mult).floor();       // Multiscale / Shadow Shield
            dmg = (dmg * fur_coat_mult).floor();         // Fur Coat
            dmg = (dmg * defender_type_mult).floor();    // Heatproof / Thick Fat / Water Bubble / Purifying Salt
            dmg = (dmg * friend_guard_mult).floor();     // Friend Guard

            let mut damage = dmg.max(0.0) as u16;
            if damage == 0
                && matches!(move_data.category, MoveCategory::Physical | MoveCategory::Special)
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
    pokemon_has_type(target, &PokemonType::Flying)
        || target.ability == Ability::Levitate
        || has_status_volatile(target, &VolatileStatus::MagnetRise)
        || has_status_volatile(target, &VolatileStatus::Telekinesis)
}

fn move_can_hit_sky_drop_target(attacker: &PokemonState, target: &PokemonState, attack_move: &PokemonMove) -> bool {
    attacker.ability == Ability::NoGuard
        || has_status_volatile(target, &VolatileStatus::Foresight)
        || has_status_volatile(target, &VolatileStatus::MiracleEye)
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
        || has_status_volatile(target, &VolatileStatus::Substitute)
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
            PokemonMove::Earthquake | PokemonMove::Magnitude => InvulnerabilityResolution::DoubleDamage,
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
    let source_move_opt = target.volatiles.iter().find_map(|volatile| {
        match volatile {
            VolatileStatusState::MoveStatus(VolatileStatus::SemiInvulnerable(mov), _) => Some(mov),
            VolatileStatusState::TurnStatus(VolatileStatus::SkyDrop, _) => Some(&PokemonMove::SkyDrop),
            _ => None,
        }
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

pub fn add_invulnerable_volatile(mon: &mut PokemonState, move_name: PokemonMove, _targets: Vec<FieldSlot>) {
    let already_has = mon.volatiles.iter().any(|volatile| {
        matches!(
            volatile,
            VolatileStatusState::MoveStatus(VolatileStatus::SemiInvulnerable(_), _)
        )
    });

    if !already_has {
        mon.volatiles.push(VolatileStatusState::MoveStatus(VolatileStatus::SemiInvulnerable(move_name), 0));
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
    mon.volatiles.iter().any(|v| {
        match (v, volatile) {
            (VolatileStatusState::TurnStatus(vst, _) | VolatileStatusState::MoveStatus(vst, _), vol) => {
                std::mem::discriminant(vst) == std::mem::discriminant(vol)
            }
            _ => false
        }
    })
}

pub fn remove_status_volatile(mon: &mut PokemonState, volatile: &VolatileStatus) {
    if let Some(pos) = mon.volatiles.iter().position(|v| {
        match (v, volatile) {
            (VolatileStatusState::TurnStatus(vst, _) | VolatileStatusState::MoveStatus(vst, _), vol) => {
                std::mem::discriminant(vst) == std::mem::discriminant(vol)
            }
            _ => false
        }
    }) {
        mon.volatiles.remove(pos);
    }
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
    false
}

/// When a Pokémon leaves the field (switch-out or faint), release any trapping volatiles
/// (`PartiallyTrapped` / `Trapped`) whose source `mon_id` matches the departing Pokémon.
/// Scans all currently-active slots on both sides.
pub fn release_traps_set_by(state: &mut BattleState, source_mon_id: u8) {
    for mon in state.p1_active_mons.iter_mut()
        .chain(state.p2_active_mons.iter_mut())
    {
        mon.volatiles.retain(|v| !matches!(v,
            VolatileStatusState::TurnStatus(VolatileStatus::PartiallyTrapped(src), _)
            | VolatileStatusState::TurnStatus(VolatileStatus::Trapped(src), _)
            if *src == source_mon_id
        ));
    }
}

pub fn apply_damage(mon: &mut PokemonState, damage: u16) {
    let hp_before = mon.hp;
    mon.hp = hp_before.saturating_sub(damage);
    mon.fainted = mon.hp == 0;

    // Berserk (project divergence): +1 Sp. Atk when HP crosses from above 50% to ≤ 50%.
    // Triggers on ANY HP loss — move damage, burn, sandstorm, Leech Seed, recoil, etc.
    // (Canon Gen 9 restricts this to direct move damage; this simulator intentionally
    // broadens the trigger per user request.)
    // Skip if the holder fainted from this hit; skip if ability is suppressed via Gastro Acid.
    // Field-level Neutralizing Gas suppression cannot be checked here (no state access) — TODO.
    if !mon.fainted
        && mon.ability == Ability::Berserk
        && !has_status_volatile(mon, &VolatileStatus::GastroAcid)
        && (hp_before as u32) * 2 > (mon.stats[0] as u32)
        && (mon.hp as u32) * 2 <= (mon.stats[0] as u32)
    {
        if mon.boosts[2] < 6 { mon.stats_raised_this_turn = true; }
        mon.boosts[2] = (mon.boosts[2] + 1).min(6);
    }
}

/// Consume an HP-threshold berry (Oran, Sitrus) if the holder is at ≤ 50% HP.
/// Uses bare `heal_mon` (not `gain_hp`) to avoid re-entrancy into `on_hp_change`.
/// Phase 1 will expand this to handle ≤ 25% pinch/flavor berries.
/// Returns true if the holder's nature reduces the stat at `stat_idx`
/// (0=Atk, 1=Def, 2=SpA, 3=SpD, 4=Spe). Used for flavor-berry confusion.
fn nature_lowers_stat(nature: &Nature, stat_idx: usize) -> bool {
    matches!((nature, stat_idx),
        (Nature::Bold   | Nature::Modest  | Nature::Calm    | Nature::Timid,   0)
      | (Nature::Lonely | Nature::Mild    | Nature::Gentle  | Nature::Hasty,   1)
      | (Nature::Adamant| Nature::Impish  | Nature::Jolly   | Nature::Careful, 2)
      | (Nature::Naughty| Nature::Lax     | Nature::Rash    | Nature::Naive,   3)
      | (Nature::Brave  | Nature::Relaxed | Nature::Quiet   | Nature::Sassy,   4)
    )
}

fn maybe_apply_berry_confusion(mon: &mut PokemonState, env: &BerryEnv) {
    if env.misty_terrain { return; }
    if env.ability_active && mon.ability == Ability::OwnTempo { return; }
    if !is_confused(mon) {
        let duration = thread_rng().gen_range(2..=5);
        mon.volatiles.push(VolatileStatusState::MoveStatus(VolatileStatus::Confusion, duration));
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
            if nature_lowers_stat(&mon.nature, 0) { maybe_apply_berry_confusion(mon, env); }
        }
        Item::WikiBerry => {
            heal_mon(mon, if ripen { max_hp * 2 / 3 } else { max_hp / 3 });
            if nature_lowers_stat(&mon.nature, 2) { maybe_apply_berry_confusion(mon, env); }
        }
        Item::MagoBerry => {
            heal_mon(mon, if ripen { max_hp * 2 / 3 } else { max_hp / 3 });
            if nature_lowers_stat(&mon.nature, 4) { maybe_apply_berry_confusion(mon, env); }
        }
        Item::AguavBerry => {
            heal_mon(mon, if ripen { max_hp * 2 / 3 } else { max_hp / 3 });
            if nature_lowers_stat(&mon.nature, 3) { maybe_apply_berry_confusion(mon, env); }
        }
        Item::IapapaBerry => {
            heal_mon(mon, if ripen { max_hp * 2 / 3 } else { max_hp / 3 });
            if nature_lowers_stat(&mon.nature, 1) { maybe_apply_berry_confusion(mon, env); }
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
                mon.volatiles.push(VolatileStatusState::TurnStatus(VolatileStatus::FocusEnergy, 0));
            }
        }
        _ => {}
    }
}

pub(crate) fn try_consume_hp_berry(mon: &mut PokemonState, env: &BerryEnv) {
    if env.suppressed || mon.fainted { return; }
    let max_hp = mon.stats[0].max(1);
    // Oran/Sitrus fire at ≤50%; pinch/flavor berries fire at ≤25%, or ≤50% with Gluttony.
    let pinch_threshold = if env.ability_active && mon.ability == Ability::Gluttony {
        max_hp / 2
    } else {
        max_hp / 4
    };
    let threshold = match mon.item {
        Item::OranBerry | Item::SitrusBerry => max_hp / 2,
        Item::FigyBerry | Item::WikiBerry | Item::MagoBerry
        | Item::AguavBerry | Item::IapapaBerry
        | Item::LiechiBerry | Item::GanlonBerry | Item::PetayaBerry
        | Item::ApicotBerry | Item::SalacBerry
        | Item::StarfBerry | Item::LansatBerry => pinch_threshold,
        _ => return,
    };
    if mon.hp == 0 || mon.hp > threshold { return; }
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
    if env.suppressed || mon.item != Item::LeppaBerry { return; }
    if let Some(i) = mon.move_pp.iter().zip(mon.max_pp.iter())
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
    if items_suppressed || klutz_disables_item(mon) || mon.item != Item::WhiteHerb { return; }
    if !mon.boosts.iter().any(|&b| b < 0) { return; }
    for b in mon.boosts.iter_mut() {
        if *b < 0 { *b = 0; }
    }
    mon.item = Item::None;
}

/// Consume a Mental Herb if the holder is currently afflicted by any of the six "mental"
/// volatile statuses (Attract, Taunt, Encore, Torment, Heal Block, Disable), curing all
/// that are present. Called from `apply_volatile_to_pokemon` after each push, so it fires
/// from all sources (move effects on target or attacker, Cursed Body, etc.).
pub(crate) fn try_consume_mental_herb(mon: &mut PokemonState, items_suppressed: bool) {
    if items_suppressed || klutz_disables_item(mon) || mon.item != Item::MentalHerb { return; }
    let mental_volatiles = [
        VolatileStatus::Attract,
        VolatileStatus::Taunt,
        VolatileStatus::Encore(PokemonMove::Struggle),
        VolatileStatus::Torment,
        VolatileStatus::HealBlock,
        VolatileStatus::Disable(PokemonMove::Struggle),
    ];
    let mut any_removed = false;
    for v in &mental_volatiles {
        if has_status_volatile(mon, v) {
            remove_status_volatile(mon, v);
            any_removed = true;
        }
    }
    if any_removed {
        mon.item = Item::None;
    }
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
) -> Vec<(u16, bool, f64)> {
    // Endure (volatile): survive any lethal *move* damage at 1 HP, deterministically. It is not an
    // item, so it ignores items_suppressed / Klutz and takes precedence over Focus Sash / Band.
    let has_endure = target.volatiles.iter().any(|v|
        matches!(v, VolatileStatusState::TurnStatus(VolatileStatus::Endure, _)));
    if has_endure && damage > 0 && !target.fainted && damage >= target.hp {
        return vec![(target.hp.saturating_sub(1), false, 1.0)];
    }
    if items_suppressed || klutz_disables_item(target) || damage == 0 || target.fainted || damage < target.hp {
        return vec![(damage, false, 1.0)];
    }
    // damage >= target.hp: this hit would KO the target
    let survive_damage = target.hp.saturating_sub(1); // leaves 1 HP after taking this amount
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
pub(crate) fn take_damage(mon: &mut PokemonState, damage: u16, env: BerryEnv) {
    if damage == 0 { return; }
    apply_damage(mon, damage);
    if !mon.fainted {
        on_hp_change(mon, &env);
    }
}

/// Heal a Pokémon and trigger the HP-change hook. Use this instead of bare `heal_mon`
/// at any call site where item-triggered effects should fire (drain, weather, moves).
pub(crate) fn gain_hp(mon: &mut PokemonState, amount: u16, env: BerryEnv) {
    if amount == 0 { return; }
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
        Player::P1 => state.p1_active_mons.iter().chain(state.p1_back_mons.iter()).any(|mon| !mon.fainted),
        Player::P2 => state.p2_active_mons.iter().chain(state.p2_back_mons.iter()).any(|mon| !mon.fainted),
    }
}

pub fn apply_damage_and_check_game_over(
    state: &mut BattleState,
    target_slot: FieldSlot,
    damage: u16,
) -> Option<crate::battle::MatchState> {
    let item_active = get_pokemon_at_slot(state, target_slot)
        .map(|m| item_is_active(state, m))
        .unwrap_or(false);
    let target_env = berry_env(state, target_slot);
    let target_mon = match target_slot.player {
        Player::P1 => state.p1_active_mons.get_mut(target_slot.slot_index as usize),
        Player::P2 => state.p2_active_mons.get_mut(target_slot.slot_index as usize),
    }?;

    take_damage(target_mon, damage, target_env);

    if damage > 0 && item_active && matches!(target_mon.item, Item::AirBalloon) {
        target_mon.item = Item::None;
    }

    if target_mon.fainted {
        clear_pokemon_on_faint(target_mon);
        handle_pokemon_faint(state, target_slot.player, target_slot.slot_index);
        if !team_has_remaining_pokemon(state, target_slot.player) {
            let winner = match target_slot.player {
                Player::P1 => Player::P2,
                Player::P2 => Player::P1,
            };
            return Some(crate::battle::MatchState::GameOverState { winner });
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
        .any(|mon| !mon.fainted && (mon.ability == Ability::AirLock || mon.ability == Ability::CloudNine))
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
        BerryEnv { suppressed: items_suppressed, ability_active: false, misty_terrain: false }
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
    let unnerve = if item_inactive { false } else { opposing_unnerve_active(state, slot) };
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
        v.push((FieldSlot { player: Player::P1, slot_index: i as u8 }, mon.species.clone(), mon.item.clone(), mon.hp));
    }
    for (i, mon) in state.p2_active_mons.iter().enumerate() {
        v.push((FieldSlot { player: Player::P2, slot_index: i as u8 }, mon.species.clone(), mon.item.clone(), mon.hp));
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
pub(crate) fn process_item_loss_events(state: &mut BattleState, before: &[(FieldSlot, Species, Item, u16)]) {
    for (slot, prev_species, prev_item, prev_hp) in before {
        // HP diff → per-turn damage flag (occupant must be unchanged).
        let hp_dropped = get_pokemon_at_slot(state, *slot)
            .is_some_and(|m| m.species == *prev_species && m.hp < *prev_hp);
        if hp_dropped {
            if let Some(m) = get_pokemon_at_slot_mut(state, *slot) {
                m.damaged_this_turn = true;
            }
        }

        if *prev_item == Item::None { continue; }
        let lost_now = match get_pokemon_at_slot(state, *slot) {
            Some(m) => m.species == *prev_species && m.item == Item::None && !m.item_lost,
            None => false,
        };
        if !lost_now { continue; }
        if let Some(m) = get_pokemon_at_slot_mut(state, *slot) {
            m.item_lost = true;
        }
        // Popped Air Balloons are destroyed, not used — Pickup cannot retrieve them.
        if *prev_item != Item::AirBalloon {
            state.items_consumed_this_turn.push((*slot, prev_item.clone()));
        }
        try_symbiosis_pass(state, *slot);
    }
}

/// Symbiosis: the Pokémon at `receiver_slot` just used up its held item — an ally with
/// (unsuppressed) Symbiosis immediately passes its own held item over. With several
/// eligible allies, slot order decides (cartridge uses speed order — simplification).
fn try_symbiosis_pass(state: &mut BattleState, receiver_slot: FieldSlot) {
    let receiver_ok = get_pokemon_at_slot(state, receiver_slot)
        .map(|m| !m.fainted && m.item == Item::None)
        .unwrap_or(false);
    if !receiver_ok { return; }

    let donor_slot = collect_active_slots(state, receiver_slot.player, Some(receiver_slot.slot_index))
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
    let env = berry_env(state, receiver_slot);
    if let Some(r) = get_pokemon_at_slot_mut(state, receiver_slot) {
        r.item = item;
        r.item_lost = false;
        // A freshly received berry may activate immediately (e.g. status-cure berries).
        on_item_obtained_or_enabled(r, &env);
    }
}

/// Items that can never change holder mid-battle: species-locked items and the
/// holder's own Mega Stone (`has_mega_form` / `is_mega` imply the held item is it).
pub(crate) fn item_cannot_be_transferred(item: &Item, holder: &PokemonState) -> bool {
    matches!(item, Item::BoosterEnergy) || holder.has_mega_form || holder.is_mega
}

/// Move the victim's held item to the (empty-handed, alive) thief, respecting Sticky Hold
/// and untransferable items. Shared by Magician / Pickpocket and, in future, Knock Off /
/// Thief / Covet. Returns `true` if an item changed hands.
pub(crate) fn try_steal_item(state: &mut BattleState, thief_slot: FieldSlot, victim_slot: FieldSlot) -> bool {
    let thief_ok = get_pokemon_at_slot(state, thief_slot)
        .map(|m| !m.fainted && m.item == Item::None)
        .unwrap_or(false);
    if !thief_ok { return false; }

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
    if blocked { return false; }

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
    if let Some(t) = get_pokemon_at_slot_mut(state, thief_slot) {
        t.item = item;
        t.item_lost = false; // gaining an item ends a previous Unburden boost
        on_item_obtained_or_enabled(t, &env);
    }
    true
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

    !counts_as_flying
        && mon.ability != Ability::Levitate
        && (!matches!(mon.item, Item::AirBalloon) || !item_is_active(state, mon))
        && !has_status_volatile(mon, &VolatileStatus::MagnetRise)
        && !has_status_volatile(mon, &VolatileStatus::Telekinesis)
        && !mon.volatiles.iter().any(|volatile| matches!(volatile, VolatileStatusState::MoveStatus(VolatileStatus::SemiInvulnerable(_), _) | VolatileStatusState::TurnStatus(VolatileStatus::SkyDrop, _)))
}

pub fn pokemon_is_on_terrain(state: &BattleState, mon: &PokemonState, terrain: &Terrain) -> bool {
    terrain_matches(state, terrain) && pokemon_is_grounded(state, mon)
}

pub fn clear_terrain(state: &mut BattleState) {
    state.terrain = None;
    state.terrain_turns = None;
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
                    if mon.boosts[1] < 6 { mon.stats_raised_this_turn = true; }
                    mon.boosts[1] = (mon.boosts[1] + 1).clamp(-6, 6);
                }
                PokemonStat::SpD => {
                    if mon.boosts[3] < 6 { mon.stats_raised_this_turn = true; }
                    mon.boosts[3] = (mon.boosts[3] + 1).clamp(-6, 6);
                }
                _ => {}
            }
        }
        // Seeds are used up — eligible for Pickup and trigger Symbiosis.
        state.items_consumed_this_turn.push((slot, seed_item.clone()));
        try_symbiosis_pass(state, slot);
    }
}

pub fn weather_is_sunlight(state: &BattleState) -> bool {
    matches!(current_weather(state), Some(Weather::Sun) | Some(Weather::ExtremeSunlight))
}

pub fn weather_is_rain(state: &BattleState) -> bool {
    matches!(current_weather(state), Some(Weather::Rain) | Some(Weather::HeavyRain))
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

pub(crate) fn is_confused(mon: &PokemonState) -> bool {
    mon.volatiles.iter().any(|volatile_status| match volatile_status {
        VolatileStatusState::TurnStatus(VolatileStatus::Confusion, _) => true,
        VolatileStatusState::MoveStatus(VolatileStatus::Confusion, _) => true,
        _ => false,
    })
}

pub fn confusion_turns_remaining(mon: &PokemonState) -> Option<u8> {
    mon.volatiles.iter().find_map(|volatile_status| match volatile_status {
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

    let burn_multiplier = if matches!(attacker.status, Some(Status::Burn)) && attacker.ability != Ability::Guts {
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
            !(m.user_slot.player == user_slot.player && m.user_slot.slot_index == user_slot.slot_index)
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

    if !pokemon_ability_is_suppressed(state, target) && target.ability == Ability::TangledFeet && is_confused(target) {
        modifier = apply_modifier_fp(modifier, 2048);
    }

    if !pokemon_ability_is_suppressed(state, attacker) && attacker.ability == Ability::Hustle && matches!(move_data.category, MoveCategory::Physical) {
        modifier = apply_modifier_fp(modifier, 3277);
    }

    if !pokemon_ability_is_suppressed(state, target) && target.ability == Ability::SandVeil && weather_is_sandstorm(state) {
        modifier = apply_modifier_fp(modifier, 3277);
    }

    if !pokemon_ability_is_suppressed(state, target) && target.ability == Ability::SnowCloak && weather_is_snow(state) {
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
                && (*idx as u8 != user_slot.slot_index || (!pokemon_ability_is_suppressed(state, attacker) && attacker.ability == Ability::VictoryStar))
        })
        .count();

    for _ in 0..victory_star_count {
        modifier = apply_modifier_fp(modifier, 4506);
    }

    if !pokemon_ability_is_suppressed(state, attacker) && attacker.ability == Ability::CompoundEyes {
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

    if item_is_active(state, attacker) && matches!(attacker.item, Item::ZoomLens) && target_has_acted_this_turn(state, target_slot) {
        modifier = apply_modifier_fp(modifier, 4915);
    }

    modifier.max(0)
}

fn adjusted_accuracy_stage(state: &BattleState, attacker: &PokemonState, target: &PokemonState) -> i8 {
    let attacker_accuracy = attacker.boosts[5];
    // Keen Eye / Illuminate: when the attacker has either ability (unsuppressed), the
    // target's evasiveness stages are ignored entirely.  Non-stage accuracy modifiers
    // (Sand Veil, Wonder Skin, etc.) are NOT ignored — this only zeroes the stage term.
    // Mold Breaker does not apply here (this protects the attacker's own accuracy calc).
    let target_evasion = if !pokemon_ability_is_suppressed(state, attacker)
        && matches!(attacker.ability, Ability::KeenEye | Ability::Illuminate)
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
    if matches!(attacker.item, Item::MicleBerry) && !klutz_disables_item(attacker) && attacker.last_move_failed {
        4915
    } else {
        4096
    }
}

fn affection_adjustment(_target: &PokemonState) -> i32 {
    0
}

fn weather_forced_accuracy(state: &BattleState, move_name: &PokemonMove) -> Option<f64> {
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

    if weather_is_sunlight(state) && matches!(move_name, PokemonMove::Thunder | PokemonMove::Hurricane) {
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
    if let Some(forced_accuracy) = weather_forced_accuracy(state, &move_data.name) {
        return forced_accuracy;
    }

    match move_data.accuracy {
        AccuracyType::True => 1.0,
        AccuracyType::Percent(base_accuracy) => {
            let base = base_accuracy as i32;
            let modifier_fp = compute_accuracy_modifier_fp(state, attacker, target, user_slot, target_slot, move_data);

            let accuracy_after_modifiers = round_div_half_down(base.saturating_mul(modifier_fp), 4096);

            let stage = adjusted_accuracy_stage(state, attacker, target);
            let stage_adjusted = (accuracy_after_modifiers as f64 * accuracy_stage_multiplier(stage)).floor() as i32;

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

    if !pokemon_ability_is_suppressed(state, mon) && mon.ability == Ability::SurgeSurfer && matches!(current_terrain(state), Some(Terrain::ElectricTerrain)) {
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

    if !pokemon_ability_is_suppressed(state, mon) && mon.ability == Ability::Chlorophyll && weather_is_sunlight(state) {
        speed *= 2.0;
    }
    if !pokemon_ability_is_suppressed(state, mon) && mon.ability == Ability::SwiftSwim && weather_is_rain(state) {
        speed *= 2.0;
    }
    if !pokemon_ability_is_suppressed(state, mon) && mon.ability == Ability::SandRush && weather_is_sandstorm(state) {
        speed *= 2.0;
    }
    if !pokemon_ability_is_suppressed(state, mon) && mon.ability == Ability::SlushRush && weather_is_snow(state) {
        speed *= 2.0;
    }
    // Unburden: ×2 Speed once the held item has been consumed or lost (and no new item
    // gained). The `item_lost` flag is cleared on switch-out / item gain; while the
    // ability is suppressed the boost is dormant and returns when suppression ends.
    if !pokemon_ability_is_suppressed(state, mon) && mon.ability == Ability::Unburden
        && mon.item == Item::None && mon.item_lost {
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

            // Stall: holder always moves last within its bracket, regardless of speed
            // or Trick Room. Overridden only when moves_first is active (handled above).
            if let (Some(p1), Some(p2)) = (user1, user2) {
                let m1_stall = p1.ability == Ability::Stall && !pokemon_ability_is_suppressed(state, p1);
                let m2_stall = p2.ability == Ability::Stall && !pokemon_ability_is_suppressed(state, p2);
                match (m1_stall, m2_stall) {
                    (true, false) => return Ordering::Greater,
                    (false, true) => return Ordering::Less,
                    _ => {}   // both or neither: fall through to speed
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

pub fn get_pokemon_at_slot<'a>(state: &'a BattleState, slot: FieldSlot) -> Option<&'a PokemonState> {
    let mons = match slot.player {
        Player::P1 => &state.p1_active_mons,
        Player::P2 => &state.p2_active_mons,
    };
    mons.get(slot.slot_index as usize)
}

/// Set weather, respecting strong weather precedence.
/// Strong weather can only be overridden by other strong weather.
pub fn set_weather(state: &mut BattleState, weather: Weather, duration: u8) {
    let current_is_strong = matches!(state.weather.as_ref(), Some(Weather::ExtremeSunlight) | Some(Weather::HeavyRain) | Some(Weather::StrongWinds));
    let new_is_strong = matches!(weather, Weather::ExtremeSunlight | Weather::HeavyRain | Weather::StrongWinds);

    if current_is_strong && !new_is_strong {
        return;
    }

    state.weather = Some(weather);
    state.weather_turns = Some(duration);
}

/// Apply entry-hazard effects to the Pokémon that just entered `slot`.
///
/// Deterministic — entry hazards never branch the outcome tree. Effects resolve in the fixed
/// order Sticky Web → Stealth Rock → Spikes → Toxic Spikes, short-circuiting as soon as the
/// entrant faints (so a mon KO'd by Stealth Rock is not also poisoned by Toxic Spikes; the
/// "skip the switch-in ability" half of that interaction lives in `process_pokemon_send_out`).
fn apply_entry_hazards(state: &mut BattleState, slot: FieldSlot) {
    let Some(mon) = get_pokemon_at_slot(state, slot) else { return; };
    if mon.fainted { return; }

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
        if let Some(SideCondition::StickyWeb(setter_id)) =
            conditions.iter().find(|sc| matches!(sc, SideCondition::StickyWeb(_)))
        {
            let items_suppressed = items_are_suppressed(state);
            let speed_drop = [0, 0, 0, 0, -1, 0, 0];
            if !ability_suppressed && entrant_ability == Ability::MirrorArmor {
                // Mirror Armor reflects the drop back to the *specific* Pokémon that set the web,
                // matched by its `mon_id` among the opposing side's current actives. If that setter
                // is no longer on the field, nobody's Speed is lowered.
                let opp = match slot.player { Player::P1 => Player::P2, Player::P2 => Player::P1 };
                let opp_actives = match opp {
                    Player::P1 => &state.p1_active_mons,
                    Player::P2 => &state.p2_active_mons,
                };
                let setter_slot = setter_id.and_then(|id| {
                    opp_actives
                        .iter()
                        .position(|m| !m.fainted && m.mon_id == id)
                        .map(|i| FieldSlot { player: opp, slot_index: i as u8 })
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
    if get_pokemon_at_slot(state, slot).map_or(true, |m| m.fainted) { return; }

    // ── 2. Stealth Rock — Rock-typed damage, affects every entrant (airborne included) ──────
    if !magic_guard && conditions.iter().any(|sc| matches!(sc, SideCondition::StealthRock)) {
        let dmg = (((max_hp as f64) * rock_eff / 8.0).floor() as u16).max(1);
        let env = berry_env(state, slot);
        if let Some(m) = get_pokemon_at_slot_mut(state, slot) { take_damage(m, dmg, env); }
    }
    if get_pokemon_at_slot(state, slot).map_or(true, |m| m.fainted) { return; }

    // ── 3. Spikes — flat fraction by layer count, grounded only ─────────────────────────────
    if grounded && !magic_guard {
        if let Some(SideCondition::Spikes(layers)) =
            conditions.iter().find(|sc| matches!(sc, SideCondition::Spikes(_)))
        {
            let denom: u16 = match layers { 1 => 8, 2 => 6, _ => 4 };
            let dmg = (max_hp / denom).max(1);
            let env = berry_env(state, slot);
            if let Some(m) = get_pokemon_at_slot_mut(state, slot) { take_damage(m, dmg, env); }
        }
    }
    if get_pokemon_at_slot(state, slot).map_or(true, |m| m.fainted) { return; }

    // ── 4. Toxic Spikes — poison, or absorbed by a grounded Poison-type ─────────────────────
    if grounded {
        if let Some(SideCondition::ToxicSpikes(layers)) =
            conditions.iter().find(|sc| matches!(sc, SideCondition::ToxicSpikes(_)))
        {
            if is_poison_type {
                // A grounded Poison-type soaks up Toxic Spikes entirely (the layer payload is
                // ignored by the discriminant-based removal).
                remove_side_condition(state, slot.player, &SideCondition::ToxicSpikes(0));
            } else {
                let status = if *layers >= 2 { Status::ToxicPoison(0) } else { Status::Poison };
                // `apply_status_to_pokemon` reads weather from an immutable `&BattleState` while we
                // mutate the entrant, so hand it a snapshot (the established pattern in this module).
                let snapshot = state.clone();
                let sun_blocks_freeze = weather_is_sunlight(&snapshot);
                if let Some(m) = get_pokemon_at_slot_mut(state, slot) {
                    apply_status_to_pokemon(&snapshot, sun_blocks_freeze, m, &status);
                }
            }
        }
    }
}

pub fn process_pokemon_send_out(state: &mut BattleState, slot: FieldSlot) {
    // Borrow scope: extract the info we need before taking a mutable borrow.
    let (is_fainted, is_replacement_turn) = match get_pokemon_at_slot(state, slot) {
        None => return,
        Some(mon) => (mon.fainted, state.turn_started),
    };
    if is_fainted { return; }

    // Mark the Pokémon as having entered this turn, so end-of-turn abilities like Speed Boost
    // skip their effect on the entry turn.  Faint replacements arrive when turn_started == true
    // (the replacement "mini-turn" flag), so we leave the flag false in that case — they should
    // receive Speed Boost normally on their first end_turn.
    if !is_replacement_turn {
        if let Some(mon_mut) = get_pokemon_at_slot_mut(state, slot) {
            mon_mut.entered_this_turn = true;
        }
    }

    // Entry hazards resolve before the switch-in ability. A Pokémon that faints to hazards (e.g.
    // Stealth Rock) never gets to trigger its entry ability (Intimidate, weather setters, …).
    apply_entry_hazards(state, slot);
    if get_pokemon_at_slot(state, slot).map_or(true, |m| m.fainted) {
        return;
    }

    let ability = match get_pokemon_at_slot(state, slot) {
        None => return,
        Some(mon) => mon.ability.clone(),
    };

    let ability_suppressed = get_pokemon_at_slot(state, slot)
        .map(|mon| pokemon_ability_is_suppressed(state, mon))
        .unwrap_or(true);

    if !ability_suppressed {
        apply_entry_ability_field_effects(state, &ability);
        apply_entry_ability_target_effects(state, slot, &ability);
        apply_send_out_only_ability_effects(state, slot, &ability);
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
/// ability, base_ability, gender, weight.
/// **What is NOT copied:** HP (both current and max), level, item, status, nature, EVs/IVs,
/// tera_type / is_tera.
///
/// **Failure conditions (no change, returns false):**
/// - Target is behind a Substitute.
/// - Target is already transformed.
/// - Target has Illusion or Imposter ability.
pub fn transform_into(transformer: &mut PokemonState, target: &PokemonState) -> bool {
    // Fail if target is behind a Substitute.
    if has_status_volatile(target, &VolatileStatus::Substitute) {
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
    transformer.species   = target.species.clone();
    transformer.types     = target.types.clone();
    transformer.gender    = target.gender;
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
        transformer.max_pp[i]  = capped;
    }

    // Copy ability.
    transformer.base_ability = target.ability.clone();
    transformer.ability      = target.ability.clone();

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
            // apply_opponent_stat_drop handles immunity (Clear Body / Hyper Cutter / …),
            // Mirror Armor reflection, and Defiant / Competitive reactions.
            apply_opponent_stat_drop(state, target, slot, [-1, 0, 0, 0, 0, 0, 0], items_suppressed, false);
        }
    }
}

/// Apply entry abilities whose effects only make sense on switch-in (healing, stat resets,
/// screen removal, Imposter/Trace). These are deliberately NOT shared with
/// `apply_entry_ability_field_effects` or `apply_entry_ability_target_effects`, so they
/// will not re-fire when Neutralizing Gas lifts.
fn apply_send_out_only_ability_effects(state: &mut BattleState, slot: FieldSlot, ability: &Ability) {
    let own_player  = slot.player;
    let opp_player  = match slot.player { Player::P1 => Player::P2, Player::P2 => Player::P1 };
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
                    apply_opponent_stat_drop(state, target, slot, [0, 0, 0, 0, 0, 0, -1], items_suppressed, false);
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
            let fainted = active.iter().chain(back.iter())
                .filter(|m| m.fainted)
                .count()
                .min(5) as u8;

            // Remove any stale SupremeOverlord volatile (re-entry after bench time).
            if let Some(mon) = get_pokemon_at_slot_mut(state, slot) {
                mon.volatiles.retain(|v| !matches!(v,
                    VolatileStatusState::TurnStatus(VolatileStatus::SupremeOverlord(_), _)
                ));
                if fainted > 0 {
                    mon.volatiles.push(VolatileStatusState::TurnStatus(
                        VolatileStatus::SupremeOverlord(fainted), 0,
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
                // Fire the newly-traced ability's gain effects (Intimidate, weather, etc.).
                apply_entry_ability_field_effects(state, &new_ability);
                apply_entry_ability_target_effects(state, slot, &new_ability);
            }
        }

        // ── Imposter ─────────────────────────────────────────────────────────────────
        // Transform into the directly-opposite opponent on entry.
        Ability::Imposter => {
            // In singles (and as the default for doubles), the directly-opposite slot
            // is the slot with the same index on the opposing side.
            let opposite = FieldSlot { player: opp_player, slot_index: slot.slot_index };
            let target_snapshot = get_pokemon_at_slot(state, opposite).cloned();
            if let Some(target) = target_snapshot {
                if let Some(transformer) = get_pokemon_at_slot_mut(state, slot) {
                    let success = transform_into(transformer, &target);
                    if success {
                        let new_ability = transformer.ability.clone();
                        // Fire the copied ability's gain effects (Intimidate, weather, etc.).
                        apply_entry_ability_field_effects(state, &new_ability);
                        apply_entry_ability_target_effects(state, slot, &new_ability);
                    }
                }
            }
        }

        _ => {}
    }
}

/// Apply the field-setting effects of an entry ability (weather/terrain setters).
/// Shared by `process_pokemon_send_out` (a Pokémon switching in) and
/// `process_pokemon_gain_ability` (a Pokémon gaining an ability mid-battle).
fn apply_entry_ability_field_effects(state: &mut BattleState, ability: &Ability) {
    match ability {
        Ability::ElectricSurge | Ability::HadronEngine => set_terrain(state, Terrain::ElectricTerrain, 5),
        Ability::GrassySurge => set_terrain(state, Terrain::GrassyTerrain, 5),
        Ability::MistySurge => set_terrain(state, Terrain::MistyTerrain, 5),
        Ability::PsychicSurge => set_terrain(state, Terrain::PsychicTerrain, 5),
        Ability::Drought | Ability::OrichalcumPulse => set_weather(state, Weather::Sun, 5),
        Ability::DesolateLand => set_weather(state, Weather::ExtremeSunlight, 0),
        Ability::Drizzle => set_weather(state, Weather::Rain, 5),
        Ability::PrimordialSea => set_weather(state, Weather::HeavyRain, 0),
        Ability::SandStream => set_weather(state, Weather::Sandstorm, 5),
        Ability::SnowWarning => set_weather(state, Weather::Snow, 5),
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

    apply_entry_ability_field_effects(state, &ability);
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
            .find(|s| get_pokemon_at_slot(state, *s).is_some_and(|m| {
                !m.fainted
                    && m.ability == Ability::Receiver
                    && !pokemon_ability_is_suppressed(state, m)
            }));
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
    if abilities_are_suppressed(state)
        && matches!(
            state.weather,
            Some(Weather::ExtremeSunlight | Weather::HeavyRain | Weather::StrongWinds)
        )
    {
        state.weather = None;
        state.weather_turns = None;
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
        let live_hp      = mon.hp;
        let live_status  = mon.status.clone();
        let live_fainted = mon.fainted;
        *mon = *saved;
        mon.hp      = live_hp;
        mon.status  = live_status;
        mon.fainted = live_fainted;
        // `boosts` will be zeroed by the caller; no need to overwrite here.
    }
    // Revert ability stolen/replaced by Mummy or Wandering Spirit.
    if let Some(original) = mon.original_ability.take() {
        mon.ability = original;
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
                matches!(m.species,
                    Species::Castform | Species::CastformRainy
                    | Species::CastformSnowy | Species::CastformSunny),
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
            m.species = target;
            m.types = types;
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
    state.terrain = Some(terrain);
    state.terrain_turns = Some(duration);

    trigger_terrain_seed_items(state);
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
    state.pseudo_weathers.push(pseudo_weather);
    state.pseudo_weather_turns.push(duration);
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
    }
}

/// Add side condition for player, avoiding duplicates.
pub fn add_side_condition(state: &mut BattleState, player: Player, condition: SideCondition, duration: u8) {
    let (conditions, turns) = match player {
        Player::P1 => (&mut state.p1_side_conditions, &mut state.p1_side_condition_turns),
        Player::P2 => (&mut state.p2_side_conditions, &mut state.p2_side_condition_turns),
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
            return;
        }
        conditions.push(condition);
        turns.push(duration);
        return;
    }

    // Single-layer conditions: reject duplicates by discriminant (existing behaviour).
    if conditions
        .iter()
        .any(|sc| std::mem::discriminant(sc) == std::mem::discriminant(&condition))
    {
        return;
    }
    conditions.push(condition);
    turns.push(duration);
}

/// Remove side condition by discriminant.
pub fn remove_side_condition(state: &mut BattleState, player: Player, condition: &SideCondition) {
    match player {
        Player::P1 => {
            if let Some(pos) = state
                .p1_side_conditions
                .iter()
                .position(|sc| std::mem::discriminant(sc) == std::mem::discriminant(condition))
            {
                state.p1_side_conditions.remove(pos);
                state.p1_side_condition_turns.remove(pos);
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
            }
        }
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

fn decrement_volatile_statuses(mons: &mut [PokemonState]) {
    for mon in mons {
        let mut kept = Vec::with_capacity(mon.volatiles.len());

        for volatile in mon.volatiles.drain(..) {
            match volatile {                VolatileStatusState::TurnStatus(effect, turns) => {
                    if turns == 0 {
                        kept.push(VolatileStatusState::TurnStatus(effect, 0));
                    } else if turns > 1 {
                        kept.push(VolatileStatusState::TurnStatus(effect, turns - 1));
                    }
                }
                other_volatile => {kept.push(other_volatile)}
            }
        }

        mon.volatiles = kept;
    }
}

pub fn decrement_move_statuses(mon: &mut PokemonState) {
    let mut kept = Vec::with_capacity(mon.volatiles.len());

    for volatile in mon.volatiles.drain(..) {
        match volatile {
            VolatileStatusState::MoveStatus(effect, turns) => {
                if turns == 0 {
                    kept.push(VolatileStatusState::MoveStatus(effect, 0));
                } else if turns > 1 {
                    kept.push(VolatileStatusState::MoveStatus(effect, turns - 1));
                }
            }
            other_volatile => {kept.push(other_volatile)}
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
        }
    }

    if let Some(turns) = state.terrain_turns.as_mut() {
        if *turns > 1 {
            *turns -= 1;
        } else if *turns == 1 {
            state.terrain = None;
            state.terrain_turns = None;
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
        for mon in state.p1_active_mons.iter_mut().chain(state.p2_active_mons.iter_mut()) {
            if klutz_disables_item(mon) { continue; }
            on_item_obtained_or_enabled(mon, &env);
        }
    }

    prune_timed_effects(&mut state.p1_side_conditions, &mut state.p1_side_condition_turns);
    prune_timed_effects(&mut state.p2_side_conditions, &mut state.p2_side_condition_turns);

    // Volatile status duration 0 means permanent, so preserve it. (Back mons cannot have volatiles)
    decrement_volatile_statuses(&mut state.p1_active_mons);
    //decrement_volatile_statuses(&mut state.p1_back_mons);
    decrement_volatile_statuses(&mut state.p2_active_mons);
    //decrement_volatile_statuses(&mut state.p2_back_mons);

    // timers decremented; other end-of-turn effects handled by `end_turn`
}

/// Perform full end-of-turn processing. Returns all possible outcomes with their probabilities,
/// branching wherever a probabilistic ability (Shed Skin, Healer, Moody, Harvest) fires.
///
/// Pipeline:
///   1. `apply_pre_status_residuals` — weather/terrain/item healing (deterministic)
///   2. `apply_status_cure_abilities` — Hydration/Shed Skin/Healer (may branch)
///   3. `apply_status_damage` — burn/poison/toxic (reads cured status, deterministic per branch)
///   4. `apply_late_eot_abilities` — Speed Boost/Moody/Harvest/Hunger Switch (may branch)
///   5. Clear `entered_this_turn` so Speed Boost fires normally next turn.
pub fn end_turn(state: &mut BattleState) -> Vec<(BattleState, f64)> {
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
    apply_pre_status_residuals(state);

    // Phase 2: probabilistic status-cure abilities. Branches if Shed Skin or Healer fires.
    let mut branches = vec![(state.clone(), 1.0)];
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
        for mon in bs.p1_active_mons.iter_mut().chain(bs.p2_active_mons.iter_mut()) {
            mon.entered_this_turn = false;
            mon.damaged_this_turn = false;
            mon.damaged_by_this_turn.clear();
            mon.stats_raised_this_turn = false;
            mon.stats_lowered_this_turn = false;
            mon.switched_in_this_turn = false;
            // Roost's Flying-type suppression only lasts the turn it is used.
            remove_status_volatile(mon, &VolatileStatus::Roost);
            // Encore ends immediately once its move runs out of PP.
            let encore_move_out_of_pp = mon.volatiles.iter().find_map(|v| match v {
                VolatileStatusState::MoveStatus(VolatileStatus::Encore(m), _) => Some(m.clone()),
                _ => None,
            }).is_some_and(|m| {
                !mon.moves.iter().zip(mon.move_pp.iter()).any(|(slot, pp)| slot.as_ref() == Some(&m) && *pp > 0)
            });
            if encore_move_out_of_pp {
                remove_status_volatile(mon, &VolatileStatus::Encore(PokemonMove::Struggle));
            }
        }
        bs.items_consumed_this_turn.clear();
    }

    coalesce_branches(branches)
}

/// Determine the duration for a volatile status condition.
fn get_volatile_duration(volatile: &VolatileStatus) -> u8 {
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
        // Default: permanent until explicitly removed
        _ => 0,
    }
}

/// Determine the duration for a side condition.
fn get_side_condition_duration(condition: &SideCondition) -> u8 {
    match condition {
        // Last only until end of turn
        SideCondition::CraftyShield
        | SideCondition::MatBlock
        | SideCondition::QuickGuard
        | SideCondition::SafeGuard
        | SideCondition::WideGuard => 1,
        // Entry hazards last indefinitely (until removed by spin/Defog/Tidy Up/Court Change).
        SideCondition::Spikes(_)
        | SideCondition::StealthRock
        | SideCondition::StickyWeb(_)
        | SideCondition::ToxicSpikes(_) => 0,
        // Default duration
        _ => 5,
    }
}

/// Apply a status condition to a pokemon (only if it doesn't already have one).
fn apply_status_to_pokemon(state: &BattleState, sun_blocks_freeze: bool, mon: &mut PokemonState, status: &crate::dex_data::Status) {
    // Prevent statuses if ability blocks all non-volatile statuses
    if mon.ability == Ability::Comatose || mon.ability == Ability::PurifyingSalt {
        return;
    }

    if mon.ability == Ability::LeafGuard && sun_blocks_freeze {
        return;
    }

    // Sweet Veil: the holder cannot fall asleep (including self-induced sleep from Rest).
    // Ally protection (Sweet Veil protecting teammates) is handled at the apply_effect_to_target
    // call site where side context is available.
    // Mold Breaker: TODO
    if matches!(status, Status::Sleep(_))
        && !pokemon_ability_is_suppressed(state, mon)
        && mon.ability == Ability::SweetVeil
    {
        return;
    }

    if matches!(status, Status::Frozen(_)) && sun_blocks_freeze {
        return;
    }

    if matches!(status, Status::Sleep(_)) && pokemon_is_on_terrain(state, mon, &Terrain::ElectricTerrain) {
        return;
    }

    if matches!(status, Status::Burn | Status::Poison | Status::ToxicPoison(_) | Status::Paralysis | Status::Sleep(_) | Status::Frozen(_))
        && pokemon_is_on_terrain(state, mon, &Terrain::MistyTerrain)
    {
        return;
    }

    if mon.status.is_some() {
        return;
    }

    match status {
        Status::Burn => {
            // Fire types and certain abilities prevent burn
            if pokemon_has_type(mon, &PokemonType::Fire) { return; }
            if mon.ability == Ability::WaterBubble || mon.ability == Ability::WaterVeil || mon.ability == Ability::ThermalExchange { return; }
            mon.status = Some(Status::Burn);
        }
        Status::Poison => {
            // Poison/Steel types are immune unless attacker has Corrosion
            if pokemon_has_type(mon, &PokemonType::Poison) || pokemon_has_type(mon, &PokemonType::Steel) {
                return;
            }
            if mon.ability == Ability::Immunity { return; }
            mon.status = Some(Status::Poison);
        }
        Status::ToxicPoison(_) => {
            if pokemon_has_type(mon, &PokemonType::Poison) || pokemon_has_type(mon, &PokemonType::Steel) {
                return;
            }
            if mon.ability == Ability::Immunity { return; }
            mon.status = Some(Status::ToxicPoison(0));
        }
        Status::Paralysis => {
            if mon.ability == Ability::Limber || pokemon_has_type(mon, &PokemonType::Electric) { return; }
            mon.status = Some(Status::Paralysis);
        }
        Status::Sleep(_) => {
            if mon.ability == Ability::Insomnia || mon.ability == Ability::VitalSpirit { return; }
            mon.status = Some(Status::Sleep(0));
        }
        Status::Frozen(_) => {
            if pokemon_has_type(mon, &PokemonType::Ice) { return; }
            if mon.ability == Ability::MagmaArmor || mon.ability == Ability::IceFace { return; }
            mon.status = Some(Status::Frozen(0));
        }
    }
}

/// Heal `mon` by `amount` HP, clamped to max. Clears fainted flag.
fn heal_mon(mon: &mut PokemonState, amount: u16) {
    let max_hp = mon.stats[0].max(1);
    mon.hp = mon.hp.saturating_add(amount).min(max_hp);
    mon.fainted = false;
}

/// Deal `amount` residual damage, clearing on faint. Returns true if the mon fainted.
fn deal_residual_damage(mon: &mut PokemonState, amount: u16, env: BerryEnv) -> bool {
    if amount == 0 { return false; }
    take_damage(mon, amount, env);
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
    if mon.fainted { return; }
    let max_hp = mon.stats[0].max(1);

    if ctx.rain && !ctx.abilities_suppressed {
        if mon.ability == Ability::RainDish { gain_hp(mon, (max_hp as u32 / 16) as u16, env); }
        if mon.ability == Ability::DrySkin  { gain_hp(mon, (max_hp as u32 / 8)  as u16, env); }
    }

    if ctx.snow && !ctx.abilities_suppressed && mon.ability == Ability::IceBody {
        gain_hp(mon, (max_hp as u32 / 16) as u16, env);
    }

    if ctx.sun && !ctx.abilities_suppressed {
        if mon.ability == Ability::DrySkin   && deal_residual_damage(mon, (max_hp as u32 / 8) as u16, env) { return; }
        if mon.ability == Ability::SolarPower && deal_residual_damage(mon, (max_hp as u32 / 8) as u16, env) { return; }
    }

    if !ctx.sandstorm { return; }

    let sandstorm_immune = pokemon_has_type(mon, &PokemonType::Steel)
        || pokemon_has_type(mon, &PokemonType::Rock)
        || pokemon_has_type(mon, &PokemonType::Ground)
        || (!ctx.abilities_suppressed && matches!(mon.ability,
            Ability::SandForce | Ability::SandRush | Ability::SandVeil | Ability::MagicGuard | Ability::Overcoat))
        || (!ctx.items_suppressed && !klutz_disables_item(mon) && matches!(mon.item, Item::SafetyGoggles));

    if !sandstorm_immune {
        deal_residual_damage(mon, (mon.stats[0] as u32 / 16) as u16, env);
    }
}

fn apply_status_residual(mon: &mut PokemonState, abilities_suppressed: bool, env: BerryEnv) {
    if mon.fainted { return; }

    // Hydration is now handled in apply_status_cure_abilities (before damage), not here.

    let magic_guard = !abilities_suppressed && mon.ability == Ability::MagicGuard;

    match mon.status {
        Some(Status::Burn) => {
            if !magic_guard {
                // Heatproof halves burn residual damage (from 1/16 to 1/32 max HP).
                let divisor = if !abilities_suppressed && mon.ability == Ability::Heatproof { 32 } else { 16 };
                deal_residual_damage(mon, (mon.stats[0] as u32 / divisor) as u16, env);
            }
        }
        Some(Status::Poison) => {
            if !magic_guard { deal_residual_damage(mon, (mon.stats[0] as u32 / 8) as u16, env); }
        }
        Some(Status::ToxicPoison(n)) => {
            let new_n = n.saturating_add(1);
            mon.status = Some(Status::ToxicPoison(new_n));
            if !magic_guard { deal_residual_damage(mon, (mon.stats[0] as u32 * new_n as u32 / 16) as u16, env); }
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
        .map(|i| berry_env(state, FieldSlot { player: Player::P1, slot_index: i as u8 }))
        .collect();
    let p2_envs: Vec<BerryEnv> = (0..state.p2_active_mons.len())
        .map(|i| berry_env(state, FieldSlot { player: Player::P2, slot_index: i as u8 }))
        .collect();

    for (i, mon) in state.p1_active_mons.iter_mut().enumerate() { apply_weather_residual(mon, &ctx, p1_envs[i]); }
    for (i, mon) in state.p2_active_mons.iter_mut().enumerate() { apply_weather_residual(mon, &ctx, p2_envs[i]); }

    // Grassy Terrain healing
    let terrain_snapshot = state.clone();
    if matches!(current_terrain(&terrain_snapshot), Some(Terrain::GrassyTerrain)) {
        for (i, mon) in state.p1_active_mons.iter_mut().enumerate() {
            if !mon.fainted && pokemon_is_grounded(&terrain_snapshot, mon) {
                let max_hp = mon.stats[0].max(1);
                gain_hp(mon, (max_hp as u32 / 16) as u16, p1_envs[i]);
            }
        }
        for (i, mon) in state.p2_active_mons.iter_mut().enumerate() {
            if !mon.fainted && pokemon_is_grounded(&terrain_snapshot, mon) {
                let max_hp = mon.stats[0].max(1);
                gain_hp(mon, (max_hp as u32 / 16) as u16, p2_envs[i]);
            }
        }
    }

    // Leftovers: restore 1/16 max HP (rounded down, min 1) at end of turn.
    // Does not consume the item. Capped at max HP by gain_hp.
    // TODO: gate on Heal Block when that mechanic is implemented.
    for (i, mon) in state.p1_active_mons.iter_mut().enumerate() {
        if !mon.fainted && !ctx.items_suppressed && !klutz_disables_item(mon) && mon.item == Item::Leftovers {
            let max_hp = mon.stats[0].max(1);
            gain_hp(mon, (max_hp as u32 / 16).max(1) as u16, p1_envs[i]);
        }
    }
    for (i, mon) in state.p2_active_mons.iter_mut().enumerate() {
        if !mon.fainted && !ctx.items_suppressed && !klutz_disables_item(mon) && mon.item == Item::Leftovers {
            let max_hp = mon.stats[0].max(1);
            gain_hp(mon, (max_hp as u32 / 16).max(1) as u16, p2_envs[i]);
        }
    }

    // Aqua Ring: restore 1/16 max HP (rounded down) at end of turn. Heal Block suppresses
    // the heal but not the volatile. Big Root boosts the amount by ≈1.3×.
    for (i, mon) in state.p1_active_mons.iter_mut().enumerate() {
        apply_aqua_ring_residual(mon, ctx.items_suppressed, p1_envs[i]);
    }
    for (i, mon) in state.p2_active_mons.iter_mut().enumerate() {
        apply_aqua_ring_residual(mon, ctx.items_suppressed, p2_envs[i]);
    }

    // Ingrain: restore 1/16 max HP (rounded down) at end of turn, same rules as Aqua Ring.
    for (i, mon) in state.p1_active_mons.iter_mut().enumerate() {
        apply_ingrain_residual(mon, ctx.items_suppressed, p1_envs[i]);
    }
    for (i, mon) in state.p2_active_mons.iter_mut().enumerate() {
        apply_ingrain_residual(mon, ctx.items_suppressed, p2_envs[i]);
    }

    // Leech Seed: drain 1/8 of the seeded mon's max HP to the Pokémon in the seeder's slot
    // (the opposing active mon in singles). Liquid Ooze reverses the heal into damage.
    apply_leech_seed_residual(state, &p1_envs, &p2_envs, ctx.items_suppressed);

    // Binding chip damage (PartiallyTrapped residual).
    // Canonical order: same phase as Leech Seed, after Leftovers but before burn/poison.
    // Ghost-types are NOT exempt — they still take chip; they are only exempt from the
    // switch-prevention (enforced separately in `is_trapped`).
    apply_binding_chip_damage(state, &p1_envs, &p2_envs, ctx.abilities_suppressed, ctx.items_suppressed);
}

/// Tick pending Wishes on every slot. A Wish set this turn carries `turns_remaining == 2`;
/// it decrements each end of turn and, on reaching 0 (the end of the turn after it was used),
/// heals the slot's current occupant by the stored amount (½ the wisher's max HP). The heal is
/// skipped if the occupant is fainted, already at full HP, or under Heal Block. Big Root does
/// not affect Wish.
fn resolve_wish_slot_conditions(state: &mut BattleState) {
    for player in [Player::P1, Player::P2] {
        let (conds, mons) = match player {
            Player::P1 => (&mut state.p1_slot_conditions, &mut state.p1_active_mons),
            Player::P2 => (&mut state.p2_slot_conditions, &mut state.p2_active_mons),
        };
        for (slot_idx, slot_conds) in conds.iter_mut().enumerate() {
            let mut resolved_heal: Option<u16> = None;
            slot_conds.retain_mut(|sc| {
                if let crate::dex_data::SlotCondition::Wish { heal, turns_remaining } = sc {
                    *turns_remaining = turns_remaining.saturating_sub(1);
                    if *turns_remaining == 0 {
                        resolved_heal = Some(*heal);
                        return false; // remove the resolved Wish
                    }
                }
                true
            });
            if let Some(heal) = resolved_heal {
                if let Some(mon) = mons.get_mut(slot_idx) {
                    if !mon.fainted && !heal_is_blocked(mon) {
                        let env = BerryEnv::simple(false);
                        gain_hp(mon, heal, env);
                    }
                }
            }
        }
    }
}

/// Aqua Ring end-of-turn heal: 1/16 max HP, Big Root–boosted, blocked by Heal Block.
fn apply_aqua_ring_residual(mon: &mut PokemonState, items_suppressed: bool, env: BerryEnv) {
    if mon.fainted
        || !has_status_volatile(mon, &VolatileStatus::AquaRing)
        || heal_is_blocked(mon)
    {
        return;
    }
    let max_hp = mon.stats[0].max(1);
    let heal = apply_big_root(mon, (max_hp as u32 / 16) as u16, items_suppressed);
    gain_hp(mon, heal, env);
}

/// Ingrain end-of-turn heal: 1/16 max HP, Big Root–boosted, blocked by Heal Block.
fn apply_ingrain_residual(mon: &mut PokemonState, items_suppressed: bool, env: BerryEnv) {
    if mon.fainted
        || !has_status_volatile(mon, &VolatileStatus::Ingrain)
        || heal_is_blocked(mon)
    {
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
        let seeder_player = match player { Player::P1 => Player::P2, Player::P2 => Player::P1 };
        let (target_mon, target_env) = match player {
            Player::P1 => (&state.p1_active_mons[idx], p1_envs[idx]),
            Player::P2 => (&state.p2_active_mons[idx], p2_envs[idx]),
        };
        let drain = (target_mon.stats[0].max(1) as u32 / 8).max(1) as u16;
        // Does the seeded mon have (unsuppressed) Liquid Ooze?
        let liquid_ooze = !abilities_suppressed && target_mon.ability == Ability::LiquidOoze;

        // Apply the drain to the seeded mon.
        match player {
            Player::P1 => take_damage(&mut state.p1_active_mons[idx], drain, target_env),
            Player::P2 => take_damage(&mut state.p2_active_mons[idx], drain, target_env),
        }

        // Resolve the seeder (opposing active slot in singles); skip if empty/fainted.
        let (seeder_opt, seeder_env) = match seeder_player {
            Player::P1 => (state.p1_active_mons.get_mut(idx), p1_envs.get(idx).copied()),
            Player::P2 => (state.p2_active_mons.get_mut(idx), p2_envs.get(idx).copied()),
        };
        let (Some(seeder), Some(seeder_env)) = (seeder_opt, seeder_env) else { continue };
        if seeder.fainted { continue; }
        let amount = apply_big_root(seeder, drain, items_suppressed);
        if liquid_ooze {
            take_damage(seeder, amount, seeder_env);
        } else if !heal_is_blocked(seeder) {
            gain_hp(seeder, amount, seeder_env);
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
        let p1_trapped: Vec<_> = state.p1_active_mons.iter().enumerate().filter_map(|(i, m)| {
            m.volatiles.iter().find_map(|v| {
                if let VolatileStatusState::TurnStatus(VolatileStatus::PartiallyTrapped(src), _) = v {
                    Some((Player::P1, i, *src))
                } else { None }
            })
        }).collect();
        let p2_trapped: Vec<_> = state.p2_active_mons.iter().enumerate().filter_map(|(i, m)| {
            m.volatiles.iter().find_map(|v| {
                if let VolatileStatusState::TurnStatus(VolatileStatus::PartiallyTrapped(src), _) = v {
                    Some((Player::P2, i, *src))
                } else { None }
            })
        }).collect();
        p1_trapped.into_iter().chain(p2_trapped).collect()
    };

    for (side, slot_idx, src_id) in trapped_slots {
        // Find whether the trapper (by mon_id) holds an active Binding Band.
        let binding_band = state.p1_active_mons.iter().chain(state.p2_active_mons.iter())
            .find(|m| m.mon_id == src_id)
            .map_or(false, |trapper| {
                !items_suppressed && !klutz_disables_item(trapper)
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
        let Some(mon) = mons.get_mut(slot_idx) else { continue; };
        if mon.fainted { continue; }

        let magic_guard = !abilities_suppressed && mon.ability == Ability::MagicGuard;
        if magic_guard { continue; }

        let max_hp = mon.stats[0].max(1);
        let chip = if binding_band {
            ((max_hp as u32) / 6).max(1) as u16
        } else {
            ((max_hp as u32) / 8).max(1) as u16
        };
        deal_residual_damage(mon, chip, env);
    }
}

/// Phase 3 of end-of-turn processing: apply burn/poison/toxic damage.
/// Called after the status-cure phase so that cured statuses take no damage.
fn apply_status_damage(state: &mut BattleState) {
    let abilities_suppressed = abilities_are_suppressed(state);
    let p1_envs: Vec<BerryEnv> = (0..state.p1_active_mons.len())
        .map(|i| berry_env(state, FieldSlot { player: Player::P1, slot_index: i as u8 }))
        .collect();
    let p2_envs: Vec<BerryEnv> = (0..state.p2_active_mons.len())
        .map(|i| berry_env(state, FieldSlot { player: Player::P2, slot_index: i as u8 }))
        .collect();
    for (i, mon) in state.p1_active_mons.iter_mut().enumerate() { apply_status_residual(mon, abilities_suppressed, p1_envs[i]); }
    for (i, mon) in state.p2_active_mons.iter_mut().enumerate() { apply_status_residual(mon, abilities_suppressed, p2_envs[i]); }
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
    for mon in state.p1_active_mons.iter_mut().chain(state.p2_active_mons.iter_mut()) {
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
            slots.push(FieldSlot { player: Player::P1, slot_index: i as u8 });
        }
        for (i, _) in first.p2_active_mons.iter().enumerate() {
            slots.push(FieldSlot { player: Player::P2, slot_index: i as u8 });
        }
        slots
    } else {
        return result;
    };

    for slot in &slots_to_check {
        // For each branch, inspect the ability of the mon at this slot.
        let ability = result.first()
            .and_then(|(bs, _)| get_pokemon_at_slot(bs, *slot))
            .map(|m| m.ability.clone())
            .unwrap_or(Ability::None);

        let abilities_suppressed_for_slot = result.first()
            .map(|(bs, _)| {
                get_pokemon_at_slot(bs, *slot)
                    .map(|m| pokemon_ability_is_suppressed(bs, m))
                    .unwrap_or(true)
            })
            .unwrap_or(true);

        if abilities_suppressed_for_slot { continue; }

        match ability {
            // Hydration: deterministic cure in rain.
            Ability::Hydration => {
                for (bs, _) in result.iter_mut() {
                    let rain = weather_is_rain(bs);
                    if let Some(mon) = get_pokemon_at_slot_mut(bs, *slot) {
                        // Setting None to None is a no-op; the is_some() guard is omitted.
                        if !mon.fainted && rain { mon.status = None; }
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
                let ally_slots: Vec<FieldSlot> = slots_to_check.iter()
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
            slots.push(FieldSlot { player: Player::P1, slot_index: i as u8 });
        }
        for (i, _) in first.p2_active_mons.iter().enumerate() {
            slots.push(FieldSlot { player: Player::P2, slot_index: i as u8 });
        }
        slots
    } else {
        return result;
    };

    for slot in &slots_to_check {
        let ability = result.first()
            .and_then(|(bs, _)| get_pokemon_at_slot(bs, *slot))
            .map(|m| m.ability.clone())
            .unwrap_or(Ability::None);

        let abilities_suppressed_for_slot = result.first()
            .map(|(bs, _)| {
                get_pokemon_at_slot(bs, *slot)
                    .map(|m| pokemon_ability_is_suppressed(bs, m))
                    .unwrap_or(true)
            })
            .unwrap_or(true);

        if abilities_suppressed_for_slot { continue; }

        match ability {
            // Speed Boost: +1 Speed every turn, but not on the turn the Pokémon switched in.
            Ability::SpeedBoost => {
                for (bs, _) in result.iter_mut() {
                    let items_suppressed = items_are_suppressed(bs);
                    if let Some(mon) = get_pokemon_at_slot_mut(bs, *slot) {
                        if !mon.fainted && !mon.entered_this_turn { apply_stat_boosts_to_pokemon(mon, &[0, 0, 0, 0, 1, 0, 0], items_suppressed, false); }
                    }
                }
            }
            // Moody: +2 to one random stat, -1 to a different random stat (Gen VIII+: 5 main
            // stats only, no accuracy/evasion).
            Ability::Moody => {
                // Enumerate (raise, lower) pairs from the first branch's state (same boosts on all).
                let (can_raise, can_lower, boosts_snapshot) =
                    if let Some((bs, _)) = result.first() {
                        if let Some(mon) = get_pokemon_at_slot(bs, *slot) {
                            if mon.fainted {
                                continue;
                            }
                            let b = mon.boosts;
                            let raise: Vec<usize> = (0..5).filter(|&i| b[i] < 6).collect();
                            let lower: Vec<usize> = (0..5).filter(|&i| b[i] > -6).collect();
                            (raise, lower, b)
                        } else { continue; }
                    } else { continue; };

                // Build outcome table: each row is (raise_idx_opt, lower_idx_opt, probability).
                // Degenerate cases handled per Bulbapedia: all-capped → only lower; all-floored → only raise.
                let outcomes: Vec<(Option<usize>, Option<usize>, f64)> = match (can_raise.len(), can_lower.len()) {
                    (0, 0) => vec![(None, None, 1.0)],
                    (0, m) => can_lower.iter().map(|&j| (None, Some(j), 1.0 / m as f64)).collect(),
                    (n, 0) => can_raise.iter().map(|&i| (Some(i), None, 1.0 / n as f64)).collect(),
                    (n, _) => {
                        let mut out = Vec::new();
                        for &i in &can_raise {
                            let lower_cands: Vec<usize> = can_lower.iter().copied()
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
                                    out.push((Some(i), Some(j), 1.0 / n as f64 / m_after as f64));
                                }
                            }
                        }
                        out
                    }
                };

                // Expand each current branch by the Moody outcome table.
                let mut new_result: Vec<(BattleState, f64)> = Vec::with_capacity(result.len() * outcomes.len());
                for (bs, prob) in result {
                    if prob <= 0.0 { continue; }
                    let items_suppressed = items_are_suppressed(&bs);
                    for &(raise_idx, lower_idx, outcome_prob) in &outcomes {
                        if outcome_prob <= 0.0 { continue; }
                        let mut branch = bs.clone();
                        if let Some(mon) = get_pokemon_at_slot_mut(&mut branch, *slot) {
                            if let Some(i) = raise_idx {
                                let mut delta = [0i8; 7];
                                delta[i] = 2;
                                apply_stat_boosts_to_pokemon(mon, &delta, items_suppressed, false);
                            }
                            if let Some(j) = lower_idx {
                                let mut delta = [0i8; 7];
                                delta[j] = -1;
                                apply_stat_boosts_to_pokemon(mon, &delta, items_suppressed, false);
                            }
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
                        let berry = mon.consumed_item.as_ref()
                            .filter(|it| format!("{:?}", it).ends_with("Berry"))
                            .cloned();
                        let empty = mon.item == Item::None;
                        (if empty && !mon.fainted { berry } else { None }, weather_is_sunlight(bs))
                    } else { (None, false) }
                } else { (None, false) };

                let Some(berry_item) = restorable_berry else { continue; };
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
                        if let Some(pos) = bs.items_consumed_this_turn.iter()
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
                    if !can_pick { continue; }
                    let Some(pos) = bs.items_consumed_this_turn.iter()
                        .rposition(|(consumer, _)| consumer != slot)
                    else { continue; };
                    let (_, item) = bs.items_consumed_this_turn.remove(pos);
                    let env = berry_env(bs, *slot);
                    if let Some(mon) = get_pokemon_at_slot_mut(bs, *slot) {
                        mon.item = item;
                        mon.item_lost = false;
                        on_item_obtained_or_enabled(mon, &env);
                    }
                }
            }
            // Cud Chew: re-apply a consumed berry's effect at the end of the turn
            // *after* it was eaten. `armed=false` means this is the first EOT; flip to
            // `armed=true`. On the second EOT (`armed=true`) fire the re-eat and clear.
            Ability::CudChew => {
                let pending = result.first()
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
                        if mon.fainted || mon.is_tera { continue; }
                        mon.species = match mon.species {
                            Species::Morpeko      => Species::MorpekoHangry,
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
    if chance <= 0.0 { return branches; }
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
        if prob <= 0.0 { continue; }
        // Check whether the ability should fire at all (skip fainted mons).
        let should_check = get_pokemon_at_slot(&bs, slot).map(|m| !m.fainted).unwrap_or(false);
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
pub fn handle_unfreeze_on_damage(mon: &mut PokemonState, move_name: &PokemonMove, move_type: &PokemonType, damage: u16) {
    if damage == 0 { return; }
    if let Some(Status::Frozen(_)) = mon.status {
        // Fire-type moves thaw
        if std::mem::discriminant(move_type) == std::mem::discriminant(&PokemonType::Fire) {
            mon.status = None;
            return;
        }

        // Specific moves thaw on hit
        if matches!(move_name, PokemonMove::Scald | PokemonMove::SteamEruption | PokemonMove::ScorchingSands | PokemonMove::MatchaGotcha) {
            mon.status = None;
            return;
        }
    }
}

/// Returns true if this move thaws the user when used.
pub fn move_thaws_user_on_use(move_name: &PokemonMove) -> bool {
    matches!(move_name,
        PokemonMove::FlameWheel
        | PokemonMove::SacredFire
        | PokemonMove::FlareBlitz
        | PokemonMove::FusionFlare
        | PokemonMove::Scald
        | PokemonMove::SteamEruption
        | PokemonMove::BurnUp
        | PokemonMove::PyroBall
        | PokemonMove::ScorchingSands
        | PokemonMove::MatchaGotcha
    )
}

/// Returns true if this move unfreezes the target on damage (specific moves only, not fire-type).
pub fn move_unfreezes_target(move_name: &PokemonMove) -> bool {
    matches!(move_name, 
        PokemonMove::Scald 
        | PokemonMove::SteamEruption 
        | PokemonMove::ScorchingSands 
        | PokemonMove::MatchaGotcha
    )
}

/// Apply a volatile status to a pokemon (prevents duplicate volatiles of the same type).
fn apply_volatile_to_pokemon(state: &BattleState, mon: &mut PokemonState, volatile: &VolatileStatus) {
    // Check if pokemon already has this volatile status
    let already_has = has_status_volatile(mon, volatile);

    if !already_has {
        // Leech Seed cannot be planted on Grass-type Pokémon or through a Substitute.
        if matches!(volatile, VolatileStatus::LeechSeed)
            && (pokemon_has_type(mon, &PokemonType::Grass)
                || has_status_volatile(mon, &VolatileStatus::Substitute))
        {
            return;
        }

        if matches!(volatile, VolatileStatus::Confusion)
            && !pokemon_ability_is_suppressed(state, mon)
            && mon.ability == Ability::OwnTempo
        {
            return;
        }

        if matches!(volatile, VolatileStatus::Confusion) && pokemon_is_on_terrain(state, mon, &Terrain::MistyTerrain) {
            return;
        }

        if matches!(volatile, VolatileStatus::Yawn) && pokemon_is_on_terrain(state, mon, &Terrain::ElectricTerrain) {
            return;
        }

        // Sweet Veil: the holder cannot receive Yawn.
        // Ally protection (Sweet Veil protecting teammates' Yawn) is handled at the
        // apply_effect_to_target call site where side context is available.
        // Mold Breaker: TODO
        if matches!(volatile, VolatileStatus::Yawn)
            && !pokemon_ability_is_suppressed(state, mon)
            && mon.ability == Ability::SweetVeil
        {
            return;
        }

            let is_move_status = matches!(
                volatile,
                VolatileStatus::Disable(_)
                    | VolatileStatus::Encore(_)
                    | VolatileStatus::GlaiveRush
                    | VolatileStatus::Taunt
                    | VolatileStatus::ThroatChop
                    | VolatileStatus::SemiInvulnerable(_)
                    | VolatileStatus::Confusion
            );

        let duration = match volatile {
            VolatileStatus::Disable(_) => 4,
            VolatileStatus::Encore(_) => 3,
            VolatileStatus::Taunt => 3,
            VolatileStatus::ThroatChop => 2,
            VolatileStatus::GlaiveRush => 1,
            VolatileStatus::SemiInvulnerable(_) => 0,
            VolatileStatus::Confusion => thread_rng().gen_range(2..=5),
            _ => get_volatile_duration(volatile),
        };

        if is_move_status {
            mon.volatiles.push(VolatileStatusState::MoveStatus(volatile.clone(), duration));
        } else {
            mon.volatiles.push(VolatileStatusState::TurnStatus(volatile.clone(), duration));
        }

        // Mental Herb: immediately cure if the newly-added volatile is one it targets.
        let items_suppressed = items_are_suppressed(state);
        try_consume_mental_herb(mon, items_suppressed);
    }
}

/// Apply stat boosts to a pokemon. If any entry of `boosts` is negative (a stat drop was
/// applied), also try to trigger a White Herb.
/// Public thin wrapper around `apply_stat_boosts_to_pokemon` for callers outside this module
/// (primarily `simulator.rs`).  Passes `defer_white_herb: false` so White Herb fires
/// immediately — appropriate for all self-boost paths (Moxie, post-KO boosts, etc.).
pub(crate) fn apply_stat_boost_external(mon: &mut PokemonState, boosts: &[i8; 7], items_suppressed: bool) {
    apply_stat_boosts_to_pokemon(mon, boosts, items_suppressed, false);
}

/// Apply a stat boost delta to `mon`, clamping each stage to `[-6, 6]`.
/// If `defer_white_herb` is false (the default for all self-boost paths), White Herb
/// is checked immediately after the apply.  Pass `true` only from
/// `apply_opponent_stat_drop`, which calls `try_consume_white_herb` manually *after*
/// Defiant / Competitive have had a chance to run — ensuring a Defiant +2 that cancels
/// the only negative stage suppresses the herb.
fn apply_stat_boosts_to_pokemon(
    mon: &mut PokemonState,
    boosts: &[i8; 7],
    items_suppressed: bool,
    defer_white_herb: bool,
) {
    for i in 0..7 {
        let before = mon.boosts[i];
        mon.boosts[i] = (before + boosts[i]).clamp(-6, 6);
        // Per-turn stat-change flags use the post-clamp delta: a stat pinned at ±6
        // doesn't count as raised/lowered (Burning Jealousy / Lash Out conditions).
        if mon.boosts[i] > before { mon.stats_raised_this_turn = true; }
        if mon.boosts[i] < before { mon.stats_lowered_this_turn = true; }
    }
    if !defer_white_herb && boosts.iter().any(|&b| b < 0) {
        try_consume_white_herb(mon, items_suppressed);
    }
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
        if after > before { mon.stats_raised_this_turn = true; }
        if after < before { mon.stats_lowered_this_turn = true; }
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
///   (Mold Breaker: TODO)
///
/// Positive entries and self-inflicted drops (those going through the attacker/self_boost
/// paths) are never touched.  Callers must pass the pre-computed suppression flag so that
/// Mold Breaker and Neutralizing Gas are respected when they land.
fn filter_opponent_stat_drops(mon: &PokemonState, boosts: &[i8; 7], ability_suppressed: bool) -> [i8; 7] {
    if ability_suppressed {
        return *boosts;
    }
    let mut filtered = *boosts;
    match mon.ability {
        Ability::ClearBody | Ability::WhiteSmoke | Ability::FullMetalBody => {
            for b in &mut filtered {
                if *b < 0 { *b = 0; }
            }
        }
        Ability::HyperCutter => {
            if filtered[0] < 0 { filtered[0] = 0; }
        }
        Ability::BigPecks => {
            if filtered[1] < 0 { filtered[1] = 0; }
        }
        // Keen Eye / Illuminate: the holder's accuracy stage cannot be lowered by opponents.
        // Mold Breaker: TODO
        Ability::KeenEye | Ability::Illuminate => {
            if filtered[5] < 0 { filtered[5] = 0; }
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
/// Mold Breaker: TODO (consistent with existing abilities — all currently ignore it).
pub(crate) fn apply_opponent_stat_drop(
    state: &mut BattleState,
    target_slot: FieldSlot,
    source_slot: FieldSlot,
    raw_boosts: [i8; 7],
    items_suppressed: bool,
    already_reflected: bool,
) {
    if raw_boosts == [0; 7] { return; }

    // Snapshot the target's ability and suppression before taking any mutable borrow.
    let (target_ability, target_suppressed) = match get_pokemon_at_slot(state, target_slot) {
        Some(m) => (m.ability.clone(), pokemon_ability_is_suppressed(state, m)),
        None => return,
    };

    // ── 1. Mirror Armor ────────────────────────────────────────────────────────────────────
    // Bounce the negative portion back to the source.  Only the holder's portion bounces;
    // positive entries (if any) are applied directly.  A previously-reflected drop cannot be
    // re-reflected (loop / infinite-recursion guard).
    if !already_reflected && !target_suppressed && target_ability == Ability::MirrorArmor {
        let mut bounced = [0i8; 7];
        let mut kept    = [0i8; 7];
        for i in 0..7 {
            if raw_boosts[i] < 0 { bounced[i] = raw_boosts[i]; }
            else                  { kept[i]    = raw_boosts[i]; }
        }
        // Bounce: the drop now originates from the holder (target_slot) targeting the source.
        // already_reflected = true prevents the source's Mirror Armor from sending it back.
        if bounced != [0; 7] {
            apply_opponent_stat_drop(state, source_slot, target_slot, bounced, items_suppressed, true);
        }
        // Apply any positive boosts (e.g. from Decorate) directly to the holder.
        if kept != [0; 7] {
            if let Some(mon) = get_pokemon_at_slot_mut(state, target_slot) {
                apply_stat_boosts_to_pokemon(mon, &kept, items_suppressed, false);
            }
        }
        return;
    }

    // ── 2. Immunity filter (Clear Body / Hyper Cutter / Big Pecks / Keen Eye / …) ─────────
    let filtered = {
        let Some(mon) = get_pokemon_at_slot(state, target_slot) else { return; };
        filter_opponent_stat_drops(mon, &raw_boosts, target_suppressed)
    };
    if filtered == [0; 7] { return; }

    // ── 3. Apply with clamping, defer White Herb ────────────────────────────────────────────
    let delta = {
        let Some(mon) = get_pokemon_at_slot_mut(state, target_slot) else { return; };
        apply_boosts_returning_delta(mon, &filtered)
    };

    // ── 4a. Opportunist: mirror any positive raise to opponents of target_slot ──────────────
    // Covers moves that raise the TARGET's stats (e.g. Decorate, target-boosting secondaries).
    // Uses the actual clamped delta so opponents mirror what truly landed.
    {
        let positive: [i8; 7] = {
            let mut p = [0i8; 7];
            for i in 0..7 { if delta[i] > 0 { p[i] = delta[i]; } }
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
            Ability::Defiant     => Some(0), // +2 Attack per stat lowered
            Ability::Competitive => Some(2), // +2 Sp. Atk per stat lowered
            _ => None,
        };
        if let Some(idx) = reaction_idx {
            let Some(mon) = get_pokemon_at_slot_mut(state, target_slot) else { return; };
            let mut boost = [0i8; 7];
            boost[idx] = 2 * stats_lowered;
            // Self-boost from Defiant/Competitive — use the non-deferring path (no White Herb
            // on a pure positive delta).
            apply_stat_boosts_to_pokemon(mon, &boost, items_suppressed, false);
        }
    }

    // ── 5. White Herb (after Defiant/Competitive have potentially cancelled the negative) ───
    if let Some(mon) = get_pokemon_at_slot_mut(state, target_slot) {
        try_consume_white_herb(mon, items_suppressed);
    }
}

/// Apply weather or pseudo-weather effects.
fn apply_weather_effects(state: &mut BattleState, effect: &HitEffect) {
    if let Some(weather) = &effect.weather {
        set_weather(state, weather.clone(), 5);
    }

    if let Some(pseudo_weather) = &effect.pseudo_weather {
        if state
            .pseudo_weathers
            .iter()
            .any(|pw| std::mem::discriminant(pw) == std::mem::discriminant(pseudo_weather))
        {
            remove_pseudo_weather(state, pseudo_weather);
        } else {
            add_pseudo_weather(state, pseudo_weather.clone(), 5);
        }
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
    if raised_boosts.iter().all(|&b| b <= 0) { return; }
    for i in 0..7 {
        if raised_boosts[i] > 0 { positive[i] = raised_boosts[i]; }
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
            if let Some(mon) = get_pokemon_at_slot_mut(state, slot) {
                apply_stat_boosts_to_pokemon(mon, &positive, items_suppressed, false);
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
    // Extract attacker ability before taking a mutable borrow of the target
    let attacker_ability = get_pokemon_at_slot(state, attacker_slot).map(|a| a.ability.clone());
    // Also snapshot target suppression before the mutable borrow (needed for stat-drop filtering).
    let target_ability_suppressed = get_pokemon_at_slot(state, target_slot)
        .map(|m| pokemon_ability_is_suppressed(state, m))
        .unwrap_or(false);
    let sun_blocks_freeze = weather_is_sunlight(state);
    let items_suppressed = items_are_suppressed(state);
    let target_berry_env = berry_env(state, target_slot);
    let terrain_snapshot = state.clone();

    // Pre-compute veil protections before taking the mutable borrow.
    // Sweet Veil: block sleep for the target's entire side (Mold Breaker: TODO).
    let sweet_veil_on_side = side_has_veil(state, target_slot.player, Ability::SweetVeil);
    // Flower Veil: block all non-volatile status and opponent stat drops on Grass-type targets
    // on the protected side (Mold Breaker: TODO).
    let flower_veil_on_side = side_has_veil(state, target_slot.player, Ability::FlowerVeil);
    // Aroma Veil: block mental volatile statuses for the target's entire side (Mold Breaker: TODO).
    let aroma_veil_on_side = side_has_veil(state, target_slot.player, Ability::AromaVeil);
    // Snapshot target type for Flower Veil (Grass-only protection) before the mutable borrow.
    let target_is_grass = get_pokemon_at_slot(state, target_slot)
        .map_or(false, |mon| pokemon_has_type(mon, &PokemonType::Grass));

    if let Some(target_mon) = get_pokemon_at_slot_mut(state, target_slot) {
        if let Some(status) = &effect.status {
            // Sweet Veil: block sleep on the entire side (including Rest).
            let sleep_blocked_by_sweet_veil =
                matches!(status, Status::Sleep(_)) && sweet_veil_on_side;
            // Flower Veil: block all non-volatile status on Grass-type targets.
            // Rest / Flame Orb / Toxic Orb are not blocked (those go through separate paths).
            let status_blocked_by_flower_veil =
                flower_veil_on_side && target_is_grass;

            if !sleep_blocked_by_sweet_veil && !status_blocked_by_flower_veil {
                // If attacker has Corrosion, allow poisoning of Poison/Steel types,
                // but do not overwrite an existing non-volatile status on the target.
                if attacker_ability == Some(Ability::Corrosion) {
                    if target_mon.status.is_none() {
                        match status {
                            Status::Poison => { target_mon.status = Some(Status::Poison); }
                            Status::ToxicPoison(_) => { target_mon.status = Some(Status::ToxicPoison(0)); }
                            other => { apply_status_to_pokemon(&terrain_snapshot, sun_blocks_freeze, target_mon, other); }
                        }
                    }
                } else {
                    apply_status_to_pokemon(&terrain_snapshot, sun_blocks_freeze, target_mon, status);
                }
            }
        }

        if let Some(volatile) = &effect.volatile_status {
            // Sweet Veil: block Yawn on the target's side.
            // Mold Breaker: TODO
            let yawn_blocked_by_sweet_veil =
                matches!(volatile, VolatileStatus::Yawn) && sweet_veil_on_side;
            // Aroma Veil: block mental volatile statuses for the target's entire side.
            // Protects against Taunt, Torment, Encore, Disable, Attract, Heal Block.
            // Does NOT block Imprison (Bulbapedia explicitly excludes it).
            // Mold Breaker: TODO
            let aroma_veil_blocked = aroma_veil_on_side && matches!(
                volatile,
                VolatileStatus::Taunt
                    | VolatileStatus::Torment
                    | VolatileStatus::Encore(_)
                    | VolatileStatus::Disable(_)
                    | VolatileStatus::Attract
                    | VolatileStatus::HealBlock
            );
            if !yawn_blocked_by_sweet_veil && !aroma_veil_blocked {
                apply_volatile_to_pokemon(&terrain_snapshot, target_mon, volatile);

                // Steadfast: +1 Speed when the holder flinches.
                // The flinch volatile has just been pushed; fire immediately so the boost
                // lands in the same "apply flinch" moment (consistent with how the ability
                // resolves in-game before the flinched Pokémon would move).
                // Suppressed abilities are skipped; Inner Focus prevents flinching (separate TODO).
                if matches!(volatile, VolatileStatus::Flinch)
                    && target_mon.ability == Ability::Steadfast
                    && !has_status_volatile(target_mon, &VolatileStatus::GastroAcid)
                {
                    apply_stat_boosts_to_pokemon(target_mon, &[0, 0, 0, 0, 1, 0, 0], items_suppressed, false);
                }
            }
        }

        // After both status and volatile are applied, check status-cure berries.
        // A single call handles Aspear/Cheri/etc. (status just set) and Persim/Lum (confusion just pushed).
        try_consume_status_cure_berry(target_mon, &target_berry_env);

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
        // Mold Breaker: TODO
        if flower_veil_on_side && target_is_grass {
            for b in &mut incoming { if *b < 0 { *b = 0; } }
        }
        if incoming != [0; 7] {
            apply_opponent_stat_drop(state, target_slot, attacker_slot, incoming, items_suppressed, false);
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
            let duration = get_side_condition_duration(&to_add);
            add_side_condition(state, side_condition_player, to_add, duration);
        }
    }

    apply_weather_effects(state, effect);
    apply_terrain_effects(state, effect);
}

/// Apply all effects from a HitEffect to the attacker pokemon.
fn apply_effect_to_attacker(
    state: &mut BattleState,
    attacker_slot: FieldSlot,
    effect: &HitEffect,
) {
    let sun_blocks_freeze = weather_is_sunlight(state);
    let items_suppressed = items_are_suppressed(state);
    let attacker_berry_env = berry_env(state, attacker_slot);
    let terrain_snapshot = state.clone();
    if let Some(attacker_mon) = get_pokemon_at_slot_mut(state, attacker_slot) {
        if let Some(status) = &effect.status {
            apply_status_to_pokemon(&terrain_snapshot, sun_blocks_freeze, attacker_mon, status);
        }

        if let Some(volatile) = &effect.volatile_status {
            apply_volatile_to_pokemon(&terrain_snapshot, attacker_mon, volatile);
        }

        // After both status and volatile are applied, check status-cure berries.
        // Covers self-inflicted confusion (e.g. Outrage rampaging) and self-status moves.
        try_consume_status_cure_berry(attacker_mon, &attacker_berry_env);

        if effect.boosts != [0; 7] {
            apply_stat_boosts_to_pokemon(attacker_mon, &effect.boosts, items_suppressed, false);
        }
    }

    // Opportunist: mirror any positive self-boost the attacker just applied to opponents
    // who hold Opportunist.  Negative drops (Leaf Storm −2 SpA, etc.) are ignored.
    if effect.boosts.iter().any(|&b| b > 0) {
        mirror_opportunist_raises(state, attacker_slot, &effect.boosts, items_suppressed);
    }

    if let Some(side_condition) = &effect.side_condition {
        if !(matches!(side_condition, SideCondition::AuroraVeil) && !weather_is_snow(state)) {
            let duration = get_side_condition_duration(side_condition);
            add_side_condition(state, attacker_slot.player, side_condition.clone(), duration);
        }
    }

    apply_weather_effects(state, effect);
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
fn apply_healing_move(bs: &mut BattleState, attacker_slot: FieldSlot, move_name: &PokemonMove, terrain_snapshot: &BattleState) -> bool {
    let branch_weather = current_weather(bs);
    let branch_harsh_sun = matches!(branch_weather, Some(Weather::ExtremeSunlight));
    let branch_sandstorm = matches!(branch_weather, Some(Weather::Sandstorm));
    let env = berry_env(bs, attacker_slot); // compute before the mutable borrow below

    let Some(attacker_mon) = get_pokemon_at_slot_mut(bs, attacker_slot) else { return false; };

    match move_name {
        PokemonMove::Rest => {
            if pokemon_is_on_terrain(terrain_snapshot, attacker_mon, &Terrain::ElectricTerrain) {
                return false;
            }
            attacker_mon.volatiles.clear();
            attacker_mon.status = Some(Status::Sleep(0));
            let max_hp = attacker_mon.stats[0].max(1);
            let heal = max_hp.saturating_sub(attacker_mon.hp);
            gain_hp(attacker_mon, heal, env);
        }
        PokemonMove::Synthesis | PokemonMove::MorningSun | PokemonMove::Moonlight => {
            let (num, den) = if branch_harsh_sun { (2u32, 3u32) }
                             else if matches!(branch_weather, None | Some(Weather::StrongWinds)) { (1u32, 2u32) }
                             else { (1u32, 4u32) };
            let max_hp = attacker_mon.stats[0].max(1);
            let heal = ((max_hp as u32 * num) / den) as u16;
            gain_hp(attacker_mon, heal, env);
        }
        PokemonMove::ShoreUp => {
            let (num, den) = if branch_sandstorm { (2u32, 3u32) } else { (1u32, 4u32) };
            let max_hp = attacker_mon.stats[0].max(1);
            let heal = ((max_hp as u32 * num) / den) as u16;
            gain_hp(attacker_mon, heal, env);
        }
        _ => return false,
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
fn apply_hazard_removal_move(bs: &mut BattleState, attacker_slot: FieldSlot, move_name: &PokemonMove) {
    let user = attacker_slot.player;
    let foe = match user { Player::P1 => Player::P2, Player::P2 => Player::P1 };
    match move_name {
        PokemonMove::RapidSpin | PokemonMove::MortalSpin => {
            // Clear the user's own side and free the user from binding + Leech Seed.
            clear_entry_hazards(bs, user);
            if let Some(mon) = get_pokemon_at_slot_mut(bs, attacker_slot) {
                remove_status_volatile(mon, &VolatileStatus::PartiallyTrapped(0));
                remove_status_volatile(mon, &VolatileStatus::LeechSeed);
            }
        }
        PokemonMove::Defog => {
            // Defog's −1 evasion (target) lives in Showdown's onHit JS, so it isn't parsed —
            // apply it here to the directly-opposing foe, unless that foe is behind a Substitute.
            let foe_target = FieldSlot { player: foe, slot_index: attacker_slot.slot_index };
            let blocked_by_sub = get_pokemon_at_slot(bs, foe_target)
                .map(|m| has_status_volatile(m, &VolatileStatus::Substitute))
                .unwrap_or(true);
            if !blocked_by_sub {
                let items_suppressed = items_are_suppressed(bs);
                apply_opponent_stat_drop(bs, foe_target, attacker_slot, [0, 0, 0, 0, 0, 0, -1], items_suppressed, false);
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
            for mon in bs.p1_active_mons.iter_mut()
                .chain(bs.p1_back_mons.iter_mut())
                .chain(bs.p2_active_mons.iter_mut())
                .chain(bs.p2_back_mons.iter_mut())
            {
                remove_status_volatile(mon, &VolatileStatus::Substitute);
            }
            // Tidy Up's +1 Atk / +1 Spe live in Showdown's onHit JS, so they aren't parsed into
            // `self_boost` — apply them here.
            let items_suppressed = items_are_suppressed(bs);
            if let Some(mon) = get_pokemon_at_slot_mut(bs, attacker_slot) {
                apply_stat_boosts_to_pokemon(mon, &[1, 0, 0, 0, 1, 0, 0], items_suppressed, false);
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
    if hits_landed == 0 { return branches; }
    if matches!(move_data.category, MoveCategory::Status) { return branches; }

    // Skip if the move already has a flinch secondary (don't double-dip).
    let move_already_flinches = move_data.secondaries.iter().any(|sec| {
        sec.effect.volatile_status == Some(VolatileStatus::Flinch)
            || sec.random_choices.iter().any(|c| c.volatile_status == Some(VolatileStatus::Flinch))
    });
    if move_already_flinches { return branches; }

    // Check that the attacker holds King's Rock and its item is active (Magic Room / Klutz).
    let eligible = branches.first().map_or(false, |(bs, _)| {
        get_pokemon_at_slot(bs, attacker_slot)
            .map_or(false, |m| item_is_active(bs, m) && m.item == Item::KingsRock)
    });
    if !eligible { return branches; }

    let chance = 1.0 - 0.9_f64.powi(hits_landed as i32);
    let flinch_effect = HitEffect {
        volatile_status: Some(VolatileStatus::Flinch),
        ..Default::default()
    };
    // side_condition_player is unused by the flinch path in apply_effect_to_target.
    let side_condition_player = target_slot.player;
    branch_on_secondary_effects(branches, chance, std::slice::from_ref(&flinch_effect), |bs, eff| {
        apply_effect_to_target(bs, attacker_slot, target_slot, eff, side_condition_player);
    })
}

// ── On-contact / on-hit reactive abilities ───────────────────────────────────

fn ability_excluded_from_mummy(ability: &Ability) -> bool {
    matches!(ability,
        Ability::AsOneGlastrier | Ability::AsOneSpectrier | Ability::BattleBond
        | Ability::Comatose | Ability::Commander | Ability::Disguise | Ability::GulpMissile
        | Ability::IceFace | Ability::LingeringAroma | Ability::Multitype
        | Ability::PowerConstruct | Ability::RKSSystem | Ability::Schooling
        | Ability::ShieldsDown | Ability::StanceChange | Ability::ZenMode
        | Ability::ZerotoHero | Ability::Mummy
    )
}

/// Blocklist for Receiver — abilities that cannot be inherited from a fainted ally.
fn ability_cannot_be_received(ability: &Ability) -> bool {
    matches!(ability,
        Ability::AsOneGlastrier | Ability::AsOneSpectrier | Ability::BattleBond
        | Ability::Comatose | Ability::Commander | Ability::Disguise | Ability::FlowerGift
        | Ability::Forecast | Ability::GulpMissile | Ability::HungerSwitch | Ability::IceFace
        | Ability::Illusion | Ability::Imposter | Ability::Multitype
        | Ability::PowerConstruct | Ability::PowerofAlchemy | Ability::Protosynthesis
        | Ability::QuarkDrive | Ability::Receiver | Ability::RKSSystem | Ability::Schooling
        | Ability::ShieldsDown | Ability::StanceChange | Ability::Trace
        | Ability::WanderingSpirit | Ability::WonderGuard | Ability::ZenMode
        | Ability::ZerotoHero
    )
}

/// Gen IX blocklist for Trace — abilities that cannot be copied.
fn ability_cannot_be_traced(ability: &Ability) -> bool {
    matches!(ability,
        Ability::AsOneGlastrier | Ability::AsOneSpectrier | Ability::BattleBond
        | Ability::Comatose | Ability::Commander | Ability::Disguise
        | Ability::EmbodyAspectCornerstone | Ability::EmbodyAspectHearthflame
        | Ability::EmbodyAspectTeal | Ability::EmbodyAspectWellspring
        | Ability::FlowerGift | Ability::Forecast | Ability::GulpMissile
        | Ability::HungerSwitch | Ability::IceFace | Ability::Illusion
        | Ability::Imposter | Ability::Multitype | Ability::NeutralizingGas
        | Ability::PoisonPuppeteer | Ability::PowerConstruct | Ability::PowerofAlchemy
        | Ability::Protosynthesis | Ability::QuarkDrive | Ability::Receiver
        | Ability::RKSSystem | Ability::Schooling | Ability::ShieldsDown
        | Ability::StanceChange | Ability::TeraShell | Ability::TeraShift
        | Ability::TeraformZero | Ability::Trace | Ability::ZenMode | Ability::ZerotoHero
    )
}

fn ability_excluded_from_wandering_spirit(ability: &Ability) -> bool {
    matches!(ability,
        Ability::AsOneGlastrier | Ability::AsOneSpectrier | Ability::BattleBond
        | Ability::Comatose | Ability::Commander | Ability::Disguise | Ability::FlowerGift
        | Ability::Forecast | Ability::GulpMissile | Ability::HungerSwitch | Ability::IceFace
        | Ability::Illusion | Ability::Imposter | Ability::Multitype | Ability::NeutralizingGas
        | Ability::PowerofAlchemy | Ability::Receiver | Ability::RKSSystem | Ability::Schooling
        | Ability::ShieldsDown | Ability::StanceChange | Ability::WonderGuard | Ability::ZenMode
        | Ability::ZerotoHero
    )
}

/// Return true if infatuation can be applied from `source_slot` to `target_slot`.
/// Requires opposite, non-genderless genders; target must not already be Attracted or Oblivious.
pub fn can_be_infatuated(state: &BattleState, source_slot: FieldSlot, target_slot: FieldSlot) -> bool {
    use crate::pokemon::PokemonGender;
    let (sg, tg, tab, already) = {
        let Some(src) = get_pokemon_at_slot(state, source_slot) else { return false };
        let Some(tgt) = get_pokemon_at_slot(state, target_slot) else { return false };
        (src.gender, tgt.gender, tgt.ability.clone(), has_status_volatile(tgt, &VolatileStatus::Attract))
    };
    if matches!(sg, PokemonGender::Genderless) || matches!(tg, PokemonGender::Genderless) { return false; }
    if sg == tg { return false; }
    if already { return false; }
    if tab == Ability::Oblivious { return false; }
    true
}

/// Apply the Attract volatile from `source_slot` to `target_slot`. Returns true if applied.
pub fn try_apply_attract(state: &mut BattleState, source_slot: FieldSlot, target_slot: FieldSlot) -> bool {
    if !can_be_infatuated(state, source_slot, target_slot) { return false; }
    let effect = HitEffect { volatile_status: Some(VolatileStatus::Attract), ..Default::default() };
    apply_effect_to_target(state, source_slot, target_slot, &effect, target_slot.player);
    true
}

/// Apply Disable to `target_slot` using its `last_used_move`. Returns true if applied.
pub fn try_apply_disable(state: &mut BattleState, source_slot: FieldSlot, target_slot: FieldSlot) -> bool {
    let (last_move, already_disabled) = {
        let Some(tgt) = get_pokemon_at_slot(state, target_slot) else { return false };
        let m = match &tgt.last_used_move {
            Some(mv) if *mv != PokemonMove::Struggle => mv.clone(),
            _ => return false,
        };
        let disabled = has_status_volatile(tgt, &VolatileStatus::Disable(PokemonMove::Struggle));
        (m, disabled)
    };
    if already_disabled { return false; }
    let effect = HitEffect { volatile_status: Some(VolatileStatus::Disable(last_move)), ..Default::default() };
    apply_effect_to_target(state, source_slot, target_slot, &effect, target_slot.player);
    true
}

/// Moves Encore cannot lock the target into. Mirrors Showdown's `failencore` flag: copying /
/// move-calling moves, Encore/Mimic/Sketch/Mirror Move, Struggle, and Transform.
pub(crate) fn encore_immune_move(mv: &PokemonMove) -> bool {
    matches!(
        mv,
        PokemonMove::Struggle
            | PokemonMove::Encore
            | PokemonMove::Mimic
            | PokemonMove::Sketch
            | PokemonMove::MirrorMove
            | PokemonMove::Transform
            | PokemonMove::Metronome
            | PokemonMove::Assist
            | PokemonMove::MeFirst
            | PokemonMove::Copycat
            | PokemonMove::NaturePower
            | PokemonMove::SleepTalk
    )
}

/// Apply Encore to `target_slot`, locking it into its `last_used_move` for 3 turns. Returns the
/// encored move on success (so the caller can also rewrite a pending queued action this turn), or
/// `None` if Encore fails (no last move, an Encore-immune move, or already Encored).
pub fn try_apply_encore(
    state: &mut BattleState,
    source_slot: FieldSlot,
    target_slot: FieldSlot,
) -> Option<PokemonMove> {
    let last_move = {
        let tgt = get_pokemon_at_slot(state, target_slot)?;
        let m = match &tgt.last_used_move {
            Some(mv) if !encore_immune_move(mv) => mv.clone(),
            _ => return None,
        };
        // Fail if the target no longer carries that move with PP (e.g. it was forgotten/0 PP).
        let has_pp = tgt.moves.iter().zip(tgt.move_pp.iter())
            .any(|(slot, pp)| slot.as_ref() == Some(&m) && *pp > 0);
        if !has_pp { return None; }
        if has_status_volatile(tgt, &VolatileStatus::Encore(PokemonMove::Struggle)) { return None; }
        m
    };
    let effect = HitEffect { volatile_status: Some(VolatileStatus::Encore(last_move.clone())), ..Default::default() };
    apply_effect_to_target(state, source_slot, target_slot, &effect, target_slot.player);
    // Confirm the volatile actually landed (Aroma Veil / Mental Herb may have blocked or cured it).
    let landed = get_pokemon_at_slot(state, target_slot)
        .is_some_and(|t| has_status_volatile(t, &VolatileStatus::Encore(PokemonMove::Struggle)));
    if landed { Some(last_move) } else { None }
}

/// Damage the attacker by `numer/denom` of its max HP (Rough Skin / Aftermath pattern).
/// Indirect HP damage paid by the attacker after using a move: Rough Skin / Iron Barbs recoil,
/// Rocky Helmet, etc.  Blocked by Magic Guard.  Handles faint bookkeeping.
///
/// Future indirect-damage sources that must also be blocked by Magic Guard:
///   - Life Orb recoil (1/10 max HP after each damaging hit) — Magic Guard: TODO
///   - Crash damage (High Jump Kick / Jump Kick miss — 1/2 max HP) — Magic Guard: TODO
///   - Leech Seed end-of-turn drain — Magic Guard: TODO (see apply_status_residual)
///   - Entry hazard switch-in damage (Spikes, Stealth Rock, Toxic Spikes) — Magic Guard: TODO
fn apply_hp_damage_to_attacker(bs: &mut BattleState, attacker_slot: FieldSlot, numer: u32, denom: u32) {
    let abilities_suppressed = abilities_are_suppressed(bs);
    let (max_hp, magic_guard) = {
        let Some(m) = get_pokemon_at_slot(bs, attacker_slot) else { return };
        if m.fainted { return; }
        let mg = !abilities_suppressed && m.ability == Ability::MagicGuard;
        (m.stats[0].max(1), mg)
    };
    if magic_guard { return; }
    let env = berry_env(bs, attacker_slot);
    let damage = ((max_hp as u32 * numer) / denom).max(1) as u16;
    let mut fainted = false;
    if let Some(atk) = get_pokemon_at_slot_mut(bs, attacker_slot) {
        take_damage(atk, damage, env);
        if atk.fainted {
            clear_pokemon_on_faint(atk);
            fainted = true;
        }
    }
    if fainted {
        handle_pokemon_faint(bs, attacker_slot.player, attacker_slot.slot_index);
    }
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
    if damage_dealt == 0 || branches.is_empty() { return branches; }

    let holder_ability = {
        let ability_opt = {
            let first_bs = &branches[0].0;
            get_pokemon_at_slot(first_bs, holder_slot)
                .filter(|m| !pokemon_ability_is_suppressed(first_bs, m))
                .map(|m| m.ability.clone())
        };
        match ability_opt {
            Some(a) => a,
            None => return branches,
        }
    };

    let is_contact = move_has_flag(move_data, &MoveFlag::Contact);
    let is_physical = matches!(move_data.category, MoveCategory::Physical);

    let mut branches = match holder_ability {
        Ability::RoughSkin => {
            if !is_contact { return branches; }
            branches.into_iter().map(|(mut bs, prob)| {
                apply_hp_damage_to_attacker(&mut bs, attacker_slot, 1, 8);
                (bs, prob)
            }).collect()
        }
        Ability::Aftermath => {
            if !is_contact { return branches; }
            branches.into_iter().map(|(mut bs, prob)| {
                let holder_fainted = get_pokemon_at_slot(&bs, holder_slot).map(|m| m.fainted).unwrap_or(false);
                if !holder_fainted { return (bs, prob); }
                if active_mons_have_ability(&bs, &Ability::Damp) { return (bs, prob); }
                apply_hp_damage_to_attacker(&mut bs, attacker_slot, 1, 4);
                (bs, prob)
            }).collect()
        }
        Ability::FlameBody => {
            if !is_contact { return branches; }
            let eff = HitEffect { status: Some(Status::Burn), ..Default::default() };
            branch_on_secondary_effects(branches, 0.30, std::slice::from_ref(&eff), |bs, e| {
                apply_effect_to_target(bs, holder_slot, attacker_slot, e, attacker_slot.player);
            })
        }
        Ability::PoisonPoint => {
            if !is_contact { return branches; }
            let eff = HitEffect { status: Some(Status::Poison), ..Default::default() };
            branch_on_secondary_effects(branches, 0.30, std::slice::from_ref(&eff), |bs, e| {
                apply_effect_to_target(bs, holder_slot, attacker_slot, e, attacker_slot.player);
            })
        }
        Ability::Static => {
            if !is_contact { return branches; }
            let eff = HitEffect { status: Some(Status::Paralysis), ..Default::default() };
            branch_on_secondary_effects(branches, 0.30, std::slice::from_ref(&eff), |bs, e| {
                apply_effect_to_target(bs, holder_slot, attacker_slot, e, attacker_slot.player);
            })
        }
        Ability::SpicySpray => {
            // Any damaging move; fires even when the holder has already fainted.
            let eff = HitEffect { status: Some(Status::Burn), ..Default::default() };
            branches.into_iter().map(|(mut bs, prob)| {
                apply_effect_to_target(&mut bs, holder_slot, attacker_slot, &eff, attacker_slot.player);
                (bs, prob)
            }).collect()
        }
        Ability::CuteCharm => {
            if !is_contact { return branches; }
            let eligible = {
                let first_bs = &branches[0].0;
                can_be_infatuated(first_bs, holder_slot, attacker_slot)
            };
            if !eligible { return branches; }
            let eff = HitEffect { volatile_status: Some(VolatileStatus::Attract), ..Default::default() };
            branch_on_secondary_effects(branches, 0.30, std::slice::from_ref(&eff), |bs, e| {
                apply_effect_to_target(bs, holder_slot, attacker_slot, e, attacker_slot.player);
            })
        }
        Ability::CursedBody => {
            // Any damaging move; Struggle cannot be disabled.
            if *move_name == PokemonMove::Struggle { return branches; }
            let eff = HitEffect { volatile_status: Some(VolatileStatus::Disable(move_name.clone())), ..Default::default() };
            branch_on_secondary_effects(branches, 0.30, std::slice::from_ref(&eff), |bs, e| {
                apply_effect_to_target(bs, holder_slot, attacker_slot, e, attacker_slot.player);
            })
        }
        Ability::Gooey => {
            if !is_contact { return branches; }
            let eff = HitEffect { boosts: [0, 0, 0, 0, -1, 0, 0], ..Default::default() };
            branches.into_iter().map(|(mut bs, prob)| {
                apply_effect_to_target(&mut bs, holder_slot, attacker_slot, &eff, attacker_slot.player);
                (bs, prob)
            }).collect()
        }
        Ability::WeakArmor => {
            if !is_physical { return branches; }
            let boosts: [i8; 7] = [0, -1, 0, 0, 2, 0, 0];
            branches.into_iter().map(|(mut bs, prob)| {
                let alive = get_pokemon_at_slot(&bs, holder_slot).map(|m| !m.fainted).unwrap_or(false);
                if !alive { return (bs, prob); }
                let items_suppressed = items_are_suppressed(&bs);
                if let Some(mon) = get_pokemon_at_slot_mut(&mut bs, holder_slot) {
                    apply_stat_boosts_to_pokemon(mon, &boosts, items_suppressed, false);
                }
                (bs, prob)
            }).collect()
        }
        // ── Stat-change reaction abilities (Subgroup B) ───────────────────────────────

        // Stamina: +1 Defense on any damaging hit; fires per hit of multi-hit moves.
        Ability::Stamina => {
            branches.into_iter().map(|(mut bs, prob)| {
                let alive = get_pokemon_at_slot(&bs, holder_slot).map(|m| !m.fainted).unwrap_or(false);
                if !alive { return (bs, prob); }
                let items_suppressed = items_are_suppressed(&bs);
                if let Some(mon) = get_pokemon_at_slot_mut(&mut bs, holder_slot) {
                    apply_stat_boosts_to_pokemon(mon, &[0, 1, 0, 0, 0, 0, 0], items_suppressed, false);
                }
                (bs, prob)
            }).collect()
        }
        // Justified: +1 Attack when hit by a Dark-type damaging move; fires per hit.
        Ability::Justified => {
            if move_data.pokemon_type != PokemonType::Dark { return branches; }
            branches.into_iter().map(|(mut bs, prob)| {
                let alive = get_pokemon_at_slot(&bs, holder_slot).map(|m| !m.fainted).unwrap_or(false);
                if !alive { return (bs, prob); }
                let items_suppressed = items_are_suppressed(&bs);
                if let Some(mon) = get_pokemon_at_slot_mut(&mut bs, holder_slot) {
                    apply_stat_boosts_to_pokemon(mon, &[1, 0, 0, 0, 0, 0, 0], items_suppressed, false);
                }
                (bs, prob)
            }).collect()
        }
        // Anger Point: on a critical hit, maximise Attack to +6 (set, not add).
        // Does not activate if the hit goes into a Substitute.
        Ability::AngerPoint => {
            if !is_crit { return branches; }
            branches.into_iter().map(|(mut bs, prob)| {
                let alive = get_pokemon_at_slot(&bs, holder_slot).map(|m| !m.fainted).unwrap_or(false);
                if !alive { return (bs, prob); }
                if let Some(mon) = get_pokemon_at_slot_mut(&mut bs, holder_slot) {
                    if mon.boosts[0] < 6 { mon.stats_raised_this_turn = true; }
                    mon.boosts[0] = 6; // maximise Attack regardless of current stage
                }
                (bs, prob)
            }).collect()
        }
        // Electromorphosis: gain the Charge status (×2 next Electric move) when hit.
        // Charge does not stack; re-hitting a charged Pokémon just refreshes it.
        Ability::Electromorphosis => {
            branches.into_iter().map(|(mut bs, prob)| {
                let alive = get_pokemon_at_slot(&bs, holder_slot).map(|m| !m.fainted).unwrap_or(false);
                if !alive { return (bs, prob); }
                if let Some(mon) = get_pokemon_at_slot_mut(&mut bs, holder_slot) {
                    // Remove any existing Charge volatile first (no stacking),
                    // then push a fresh one.
                    remove_status_volatile(mon, &VolatileStatus::Charge);
                    mon.volatiles.push(VolatileStatusState::TurnStatus(VolatileStatus::Charge, 0));
                }
                (bs, prob)
            }).collect()
        }
        // Pickpocket: steal the attacker's item when hit by a contact move while
        // empty-handed. Doesn't trigger if the holder is KO'd by the hit. Fires on the
        // first contact hit rather than the cartridge's last strike — equivalent outcome,
        // since the attacker's item is gone either way.
        Ability::Pickpocket => {
            if !is_contact { return branches; }
            branches.into_iter().map(|(mut bs, prob)| {
                let holder_alive = get_pokemon_at_slot(&bs, holder_slot)
                    .map(|m| !m.fainted)
                    .unwrap_or(false);
                if holder_alive {
                    try_steal_item(&mut bs, holder_slot, attacker_slot);
                }
                (bs, prob)
            }).collect()
        }
        Ability::Mummy => {
            if !is_contact { return branches; }
            branches.into_iter().map(|(mut bs, prob)| {
                let (atk_ability, excluded) = {
                    let Some(atk) = get_pokemon_at_slot(&bs, attacker_slot) else { return (bs, prob); };
                    if atk.fainted { return (bs, prob); }
                    (atk.ability.clone(), ability_excluded_from_mummy(&atk.ability) || atk.ability == Ability::Mummy)
                };
                if excluded { return (bs, prob); }
                if let Some(atk) = get_pokemon_at_slot_mut(&mut bs, attacker_slot) {
                    if atk.original_ability.is_none() { atk.original_ability = Some(atk_ability); }
                    atk.ability = Ability::Mummy;
                }
                (bs, prob)
            }).collect()
        }
        Ability::WanderingSpirit => {
            if !is_contact { return branches; }
            branches.into_iter().map(|(mut bs, prob)| {
                // Check attacker's ability can be swapped
                let (atk_ability, excluded) = {
                    let Some(atk) = get_pokemon_at_slot(&bs, attacker_slot) else { return (bs, prob); };
                    let excluded = atk.fainted || ability_excluded_from_wandering_spirit(&atk.ability);
                    (atk.ability.clone(), excluded)
                };
                if excluded { return (bs, prob); }
                let hld_ability = {
                    let Some(hld) = get_pokemon_at_slot(&bs, holder_slot) else { return (bs, prob); };
                    hld.ability.clone()
                };
                // Swap: attacker gets hld_ability (WanderingSpirit), holder gets atk_ability
                if let Some(atk) = get_pokemon_at_slot_mut(&mut bs, attacker_slot) {
                    if atk.original_ability.is_none() { atk.original_ability = Some(atk_ability.clone()); }
                    atk.ability = hld_ability.clone();
                }
                if let Some(hld) = get_pokemon_at_slot_mut(&mut bs, holder_slot) {
                    if hld.original_ability.is_none() { hld.original_ability = Some(hld_ability); }
                    hld.ability = atk_ability;
                }
                // Fire on-gain effects for both
                process_pokemon_gain_ability(&mut bs, attacker_slot);
                process_pokemon_gain_ability(&mut bs, holder_slot);
                (bs, prob)
            }).collect()
        }
        _ => branches,
    };

    // ── Charge consumer (attacker-side, outside the holder-ability match) ─────────────────
    // If the attacker used a damaging Electric-type move and holds the Charge volatile,
    // consume it now.  The doubling already happened inside effective_base_power.
    // Runs per-hit; on the first hit Charge is consumed, subsequent hits see it gone.
    if move_data.pokemon_type == PokemonType::Electric {
        branches = branches.into_iter().map(|(mut bs, prob)| {
            if let Some(atk) = get_pokemon_at_slot_mut(&mut bs, attacker_slot) {
                remove_status_volatile(atk, &VolatileStatus::Charge);
            }
            (bs, prob)
        }).collect();
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
/// Note: Mold Breaker bypass is not yet implemented (separate TODO).
pub(crate) fn try_absorb_move(
    state: &mut BattleState,
    target_slot: FieldSlot,
    attacker: &PokemonState,
    move_data: &MoveData,
    items_suppressed: bool,
) -> bool {
    // Fetch the target's ability; if suppressed the move hits normally.
    let target_ability = match get_pokemon_at_slot(state, target_slot) {
        Some(t) if !pokemon_ability_is_suppressed(state, t) => t.ability.clone(),
        _ => return false,
    };

    // Use the canonical move type (respects -ate abilities, Liquid Voice, etc.).
    let move_type = effective_move_type(state, attacker, move_data);
    let target_env = berry_env(state, target_slot);

    let absorbs = match (&move_type, &target_ability) {
        (PokemonType::Electric, Ability::VoltAbsorb)
        | (PokemonType::Water,   Ability::WaterAbsorb)
        | (PokemonType::Water,   Ability::DrySkin)
        | (PokemonType::Ground,  Ability::EarthEater) => {
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
                    mon.volatiles.push(
                        crate::pokemon::VolatileStatusState::MoveStatus(VolatileStatus::FlashFire, 0),
                    );
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
///
/// Note: Mold Breaker bypass is not yet implemented (separate TODO).
pub(crate) fn try_drawin_negate(
    state: &mut BattleState,
    target_slot: FieldSlot,
    attacker: &PokemonState,
    move_data: &MoveData,
    items_suppressed: bool,
) -> bool {
    let target_ability = match get_pokemon_at_slot(state, target_slot) {
        Some(t) if !pokemon_ability_is_suppressed(state, t) => t.ability.clone(),
        _ => return false,
    };

    let move_type = effective_move_type(state, attacker, move_data);

    let negated = match (&move_type, &target_ability) {
        (PokemonType::Electric, Ability::LightningRod)
        | (PokemonType::Water,   Ability::StormDrain) => {
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
        let already_bound = get_pokemon_at_slot(&bs, target_slot)
            .map_or(false, |m| has_status_volatile(m, &VolatileStatus::PartiallyTrapped(0)));
        let has_sub = get_pokemon_at_slot(&bs, target_slot)
            .map_or(false, |m| has_status_volatile(m, &VolatileStatus::Substitute));
        if already_bound || has_sub {
            new_branches.push((bs, prob));
            continue;
        }

        // Grip Claw (held by the trapper): locate the trapper by mon_id for doubles safety.
        let grip_claw = bs.p1_active_mons.iter().chain(bs.p2_active_mons.iter())
            .find(|m| m.mon_id == attacker_mon_id)
            .map_or(false, |trapper| {
                item_is_active(&bs, trapper) && trapper.item == Item::GripClaw
            });

        if grip_claw {
            let mut applied = bs.clone();
            if let Some(mon) = get_pokemon_at_slot_mut(&mut applied, target_slot) {
                mon.volatiles.push(VolatileStatusState::TurnStatus(
                    VolatileStatus::PartiallyTrapped(attacker_mon_id), 7,
                ));
            }
            new_branches.push((applied, prob));
        } else {
            // 50/50 branch: 4 turns or 5 turns.
            for duration in [4u8, 5u8] {
                let mut applied = bs.clone();
                if let Some(mon) = get_pokemon_at_slot_mut(&mut applied, target_slot) {
                    mon.volatiles.push(VolatileStatusState::TurnStatus(
                        VolatileStatus::PartiallyTrapped(attacker_mon_id), duration,
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
            let target_has_sub = get_pokemon_at_slot(bs, target_slot)
                .map_or(false, |m| has_status_volatile(m, &VolatileStatus::Substitute));
            if target_is_ghost || target_has_sub { return; }
            if let Some(mon) = get_pokemon_at_slot_mut(bs, target_slot) {
                if !has_status_volatile(mon, &VolatileStatus::Trapped(0)) {
                    mon.volatiles.push(VolatileStatusState::TurnStatus(
                        VolatileStatus::Trapped(attacker_mon_id), 0,
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
            match attacker_slot.player { Player::P1 => Player::P2, Player::P2 => Player::P1 }
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
            no_bench || get_pokemon_at_slot(bs, attacker_slot).map_or(true, |m| {
                let max_hp = m.stats[0].max(1);
                m.volatiles.iter().any(|v|
                    matches!(v, VolatileStatusState::TurnStatus(VolatileStatus::Substitute, _))
                ) || m.hp <= max_hp / 2
            })
        });
        if !failed {
            // HP cost: ceil(max_hp / 2)
            for (bs, _) in branches.iter_mut() {
                if let Some(m) = get_pokemon_at_slot_mut(bs, attacker_slot) {
                    let max_hp = m.stats[0].max(1);
                    let cost = (max_hp + 1) / 2;
                    take_damage(m, cost, attacker_env);
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
    // Mold Breaker: TODO
    let target_has_shield_dust = get_pokemon_at_slot(state, target_slot).map_or(false, |mon| {
        !pokemon_ability_is_suppressed(state, mon) && mon.ability == Ability::ShieldDust
    });

    // Burning Jealousy: the burn applies only to targets whose stats were actually raised
    // earlier this turn. The Showdown source stores this as an `onHit` callback, so the
    // parsed `secondaries` carry no status — apply it explicitly. The 70 BP is unconditional.
    let burning_jealousy_burn = move_data.name == PokemonMove::BurningJealousy
        && get_pokemon_at_slot(state, target_slot).is_some_and(|m| m.stats_raised_this_turn);

    // Branch target secondaries
    if !target_has_shield_dust {
        for secondary in &move_data.secondaries {
            // Shed Tail: skip the auto-added Substitute secondary when the move fails its HP check.
            if shed_tail_failed
                && secondary.random_choices.is_empty()
                && secondary.effect.volatile_status == Some(VolatileStatus::Substitute)
            {
                continue;
            }
            // PartiallyTrapped sentinel: handled separately below with duration branching.
            if secondary.random_choices.is_empty()
                && matches!(&secondary.effect.volatile_status, Some(VolatileStatus::PartiallyTrapped(_)))
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
            let burn = HitEffect { status: Some(Status::Burn), ..Default::default() };
            for (bs, _) in branches.iter_mut() {
                apply_effect_to_target(bs, attacker_slot, target_slot, &burn, side_condition_target);
            }
        }

        // Throat Chop: on hit, prevent the target from using sound moves for 2 turns. Showdown
        // stores this as a 100% `onHit` secondary that the parser cannot represent. Blocked by a
        // Substitute; consecutive hits do not refresh the duration (apply_volatile_to_pokemon's
        // already-has guard handles that).
        if move_data.name == PokemonMove::ThroatChop {
            let target_has_sub = get_pokemon_at_slot(state, target_slot)
                .is_some_and(|m| has_status_volatile(m, &VolatileStatus::Substitute));
            if !target_has_sub {
                let eff = HitEffect { volatile_status: Some(VolatileStatus::ThroatChop), ..Default::default() };
                for (bs, _) in branches.iter_mut() {
                    apply_effect_to_target(bs, attacker_slot, target_slot, &eff, side_condition_target);
                }
            }
        }

        // Binding trap: apply PartiallyTrapped with source-id and duration branching.
        // Handled after all other target secondaries so flinch / stat drops fire first.
        let has_bind_secondary = move_data.secondaries.iter().any(|s| {
            s.random_choices.is_empty()
                && matches!(&s.effect.volatile_status, Some(VolatileStatus::PartiallyTrapped(_)))
        });
        if has_bind_secondary {
            let attacker_mon_id = get_pokemon_at_slot(state, attacker_slot)
                .map(|m| m.mon_id)
                .unwrap_or(u8::MAX);
            branches = apply_binding_trap(branches, attacker_mon_id, target_slot);
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

    // Transform move: deterministic, no branching.
    if move_data.name == PokemonMove::Transform {
        // The default opposite slot for Transform is the directly-opposite slot index on
        // the other side (same slot_index, opposing player).
        let opp_player = match attacker_slot.player { Player::P1 => Player::P2, Player::P2 => Player::P1 };
        let opposite = FieldSlot { player: opp_player, slot_index: attacker_slot.slot_index };
        for (bs, _) in branches.iter_mut() {
            let target_snapshot = get_pokemon_at_slot(bs, opposite).cloned();
            if let Some(target) = target_snapshot {
                let success = get_pokemon_at_slot_mut(bs, attacker_slot)
                    .map(|t| transform_into(t, &target))
                    .unwrap_or(false);
                if success {
                    let new_ability = get_pokemon_at_slot(bs, attacker_slot)
                        .map(|m| m.ability.clone());
                    if let Some(ab) = new_ability {
                        // Fire the copied ability's gain effects.
                        apply_entry_ability_field_effects(bs, &ab);
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
}

/// Check if a PokÃ©mon is immune to Rage Powder based on type, ability, or item.
/// Grass-types, PokÃ©mon with Overcoat ability, and those holding Safety Googles are immune.
pub fn is_immune_to_powder(state: &BattleState, mon: &PokemonState) -> bool {
    pokemon_has_type(mon, &PokemonType::Grass)
    || (!abilities_are_suppressed(state) && mon.ability == Ability::Overcoat)
    || (item_is_active(state, mon) && matches!(mon.item, Item::SafetyGoggles))
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
    let attacker_immune_to_powder = get_pokemon_at_slot(state, user_slot)
        .map(|attacker| is_immune_to_powder(state, attacker))
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
            speed_a.partial_cmp(&speed_b).unwrap_or(std::cmp::Ordering::Equal)
        });

        if let Some((slot, _)) = best_redirector {
            return vec![slot];
        }
    }

    // --- Priority 2: Lightning Rod (Electric) / Storm Drain (Water) ---
    // These draw single-target moves of the matching type toward the ability holder.
    // Mold Breaker bypass is not yet implemented (separate TODO).
    if let Some(md) = move_data {
        if let Some(attacker) = get_pokemon_at_slot(state, user_slot) {
            let move_type = effective_move_type(state, attacker, md);
            let mut ability_redirectors: Vec<(FieldSlot, &PokemonState)> = Vec::new();

            for (idx, mon) in opposing_mons.iter().enumerate() {
                if mon.fainted { continue; }
                if pokemon_ability_is_suppressed(state, mon) { continue; }

                let draws = matches!(
                    (&move_type, &mon.ability),
                    (PokemonType::Electric, Ability::LightningRod)
                    | (PokemonType::Water, Ability::StormDrain)
                );
                if draws {
                    ability_redirectors.push((
                        FieldSlot { player: opposing_player, slot_index: idx as u8 },
                        mon,
                    ));
                }
            }

            if !ability_redirectors.is_empty() {
                let best = ability_redirectors.into_iter().max_by(|a, b| {
                    let speed_a = get_effective_speed(state, a.1);
                    let speed_b = get_effective_speed(state, b.1);
                    speed_a.partial_cmp(&speed_b).unwrap_or(std::cmp::Ordering::Equal)
                });
                if let Some((slot, _)) = best {
                    return vec![slot];
                }
            }
        }
    }

    target_slots
}

pub(crate) fn get_pokemon_at_slot_mut<'a>(state: &'a mut BattleState, slot: FieldSlot) -> Option<&'a mut PokemonState> {
    let mons = match slot.player {
        Player::P1 => &mut state.p1_active_mons,
        Player::P2 => &mut state.p2_active_mons,
    };
    mons.get_mut(slot.slot_index as usize)
}
