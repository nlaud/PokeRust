use crate::battle::{Action, BattleState, FieldSlot, Player, MoveAction};
use crate::data::ability::Ability;
use crate::data::item::Item;
use crate::data::pokemon_move::PokemonMove;
use crate::dex_data::{AccuracyType, MoveCategory, MoveData, MoveTarget, PokemonType, PseudoWeather, SideCondition, Terrain, Weather, HitEffect, MoveFlag, PokemonStat, Status};
use crate::pokemon::{PokemonState, VolatileStatusState};
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

pub fn move_type_effectiveness(state: &BattleState, move_type: &PokemonType, target: &PokemonState) -> f64 {
    if target.types.is_empty() {
        return 1.0;
    }

    target.types.iter().fold(1.0, |effectiveness, target_type| {
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
        // All allies, excluding self
        MoveTarget::Allies | MoveTarget::AllySide | MoveTarget::AllyTeam | MoveTarget::AdjacentAlly => {
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

fn effective_move_type(state: &BattleState, attacker: &PokemonState, move_data: &MoveData) -> PokemonType {
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
        _ => move_data.pokemon_type.clone(),
    }
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

    bp = (bp * terrain_type_bp_boost(state, attacker, &move_data.pokemon_type)).floor();
    bp = (bp * move_terrain_bp_modifier(state, attacker, target, move_data)).floor();

    if attacker.ability == Ability::Reckless
        && (move_data.recoil_fraction[0] > 0 && move_data.recoil_fraction[1] > 0 || move_data.struggle_recoil)
    {
        bp = (bp * 1.2).floor();
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

    bp
}

/// Weather damage multiplier for Fire/Water in sun/rain (HydroSteam is excluded — its bonus is in base power).
fn weather_damage_multiplier(state: &BattleState, move_data: &MoveData) -> f64 {
    let Some(weather) = current_weather(state) else { return 1.0; };
    match weather {
        Weather::Sun | Weather::ExtremeSunlight => {
            if move_data.name == PokemonMove::HydroSteam { 1.0 }
            else if matches!(move_data.pokemon_type, PokemonType::Fire) { 1.5 }
            else if matches!(move_data.pokemon_type, PokemonType::Water) { 0.5 }
            else { 1.0 }
        }
        Weather::Rain | Weather::HeavyRain => {
            if matches!(move_data.pokemon_type, PokemonType::Fire) { 0.5 }
            else if matches!(move_data.pokemon_type, PokemonType::Water) { 1.5 }
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
    let stab          = stab_multiplier(attacker, &attack_type);
    let bp            = effective_base_power(_state, attacker, target, move_data, base_power_override);
    let weather_mult  = weather_damage_multiplier(_state, move_data);
    let burn_mult     = burn_damage_multiplier(attacker, move_data);
    let dry_skin_mult = dry_skin_fire_multiplier(target, &attack_type);
    let screen_mult   = screen_damage_multiplier(_state, _target_slot, move_data, false); // overridden per-crit below

    let rolls   = forced_damage_roll.map(|r| vec![r]).unwrap_or_else(|| selected_damage_rolls(config.damage_rolls));
    let crits   = critical_hit_probability(attacker, target, &move_data.name, config.consider_crit, move_data.crit_ratio);

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
            dmg = (dmg * this_screen_mult).floor();
            dmg = (dmg * burn_mult).floor();
            dmg = (dmg * invulnerability_multiplier).floor();
            dmg = (dmg * weather_mult).floor();
            dmg = (dmg * dry_skin_mult).floor();

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
        || matches!(target.item, Item::IronBall)
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

pub fn apply_damage(mon: &mut PokemonState, damage: u16) {
    mon.hp = mon.hp.saturating_sub(damage);
    mon.fainted = mon.hp == 0;
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
    let items_suppressed = items_are_suppressed(state);
    let target_mon = match target_slot.player {
        Player::P1 => state.p1_active_mons.get_mut(target_slot.slot_index as usize),
        Player::P2 => state.p2_active_mons.get_mut(target_slot.slot_index as usize),
    }?;

    apply_damage(target_mon, damage);

    if damage > 0 && !items_suppressed && matches!(target_mon.item, Item::AirBalloon) {
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

fn pokemon_ability_is_suppressed(state: &BattleState, mon: &PokemonState) -> bool {
    abilities_are_suppressed(state) && mon.ability != Ability::NeutralizingGas
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

    !pokemon_has_type(mon, &PokemonType::Flying)
        && mon.ability != Ability::Levitate
        && (!matches!(mon.item, Item::AirBalloon) || items_are_suppressed(state))
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

    for mon in state.p1_active_mons.iter_mut().chain(state.p2_active_mons.iter_mut()) {
        if mon.item != seed_item {
            continue;
        }

        mon.item = Item::None;
        match boost_stat {
            PokemonStat::Def => mon.boosts[1] = (mon.boosts[1] + 1).clamp(-6, 6),
            PokemonStat::SpD => mon.boosts[3] = (mon.boosts[3] + 1).clamp(-6, 6),
            _ => {}
        }
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

fn is_confused(mon: &PokemonState) -> bool {
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

    if !items_are_suppressed(state) && matches!(target.item, Item::BrightPowder) {
        modifier = apply_modifier_fp(modifier, 3686);
    }

    if !items_are_suppressed(state) && matches!(target.item, Item::LaxIncense) {
        modifier = apply_modifier_fp(modifier, 3686);
    }

    if !items_are_suppressed(state) && matches!(attacker.item, Item::WideLens) {
        modifier = apply_modifier_fp(modifier, 4505);
    }

    if !items_are_suppressed(state) && matches!(attacker.item, Item::ZoomLens) && target_has_acted_this_turn(state, target_slot) {
        modifier = apply_modifier_fp(modifier, 4915);
    }

    modifier.max(0)
}

fn adjusted_accuracy_stage(attacker: &PokemonState, target: &PokemonState) -> i8 {
    let attacker_accuracy = attacker.boosts[5];
    let target_evasion = target.boosts[6];
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
    if matches!(attacker.item, Item::MicleBerry) && attacker.last_move_failed {
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

            let stage = adjusted_accuracy_stage(attacker, target);
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
    _move_dex: &std::collections::HashMap<PokemonMove, MoveData>,
) -> std::cmp::Ordering {
    use std::cmp::Ordering;

    let type_priority1 = get_action_type_priority(action1);
    let type_priority2 = get_action_type_priority(action2);

    if type_priority1 != type_priority2 {
        return type_priority1.cmp(&type_priority2);
    }

    match (action1, action2) {
        (Action::MoveAction(m1), Action::MoveAction(m2)) => {
            if m1.priority != m2.priority {
                return m2.priority.cmp(&m1.priority);
            }

            let user1 = get_pokemon_at_slot(state, m1.user_slot);
            let user2 = get_pokemon_at_slot(state, m2.user_slot);

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

pub fn process_pokemon_send_out(state: &mut BattleState, slot: FieldSlot) {
    let Some(mon) = get_pokemon_at_slot(state, slot) else {
        return;
    };

    if mon.fainted {
        return;
    }

    let ability = mon.ability.clone();

    if !pokemon_ability_is_suppressed(state, mon) {
        apply_entry_ability_field_effects(state, &ability);
        apply_entry_ability_target_effects(state, slot, &ability);
    }

    trigger_terrain_seed_items(state);

    // A Pokémon entering may bring Neutralizing Gas, which suppresses primal-weather
    // abilities and so ends the extreme weather they were maintaining.
    handle_gas_primal_weather_suppression(state);
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
        for target in collect_active_slots(state, opposing_player, None) {
            if let Some(mon) = get_pokemon_at_slot_mut(state, target) {
                apply_stat_boosts_to_pokemon(mon, &[-1, 0, 0, 0, 0, 0, 0]);
            }
        }
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

    // Apply the departing Pokémon's own switch-out ability, and note its ability for
    // the field-effect checks below.
    let departed_ability = {
        let back = match player {
            Player::P1 => &mut state.p1_back_mons,
            Player::P2 => &mut state.p2_back_mons,
        };
        let Some(departed) = back.get_mut(bench_index) else {
            return;
        };
        let ability = departed.ability.clone();
        if !abilities_suppressed {
            apply_switch_out_ability_effects(departed);
        }
        ability
    };

    // Neutralizing Gas suppression lifts when its holder leaves the field.
    if departed_ability == Ability::NeutralizingGas && !any_pokemon_has_neutralizing_gas(state) {
        handle_neutralizing_gas_lift(state);
    }

    // Primal weather ends when its source leaves, unless another holder remains.
    handle_primal_weather_departure(state, &departed_ability);
}

/// Handle field effects when the Pokémon at `slot_index` (for `player`) faints:
/// Neutralizing Gas suppression lifting and primal weather ending. Unlike switching out,
/// fainting does not trigger Natural Cure / Regenerator. The fainted Pokémon is expected
/// to still occupy its active slot (with `fainted == true`); the helpers below ignore
/// fainted Pokémon, so it is correctly treated as gone from the field.
pub fn handle_pokemon_faint(state: &mut BattleState, player: Player, slot_index: u8) {
    let fainted_ability = {
        let mons = match player {
            Player::P1 => &state.p1_active_mons,
            Player::P2 => &state.p2_active_mons,
        };
        let Some(mon) = mons.get(slot_index as usize) else {
            return;
        };
        mon.ability.clone()
    };

    // Neutralizing Gas suppression lifts when its holder faints.
    if fainted_ability == Ability::NeutralizingGas && !any_pokemon_has_neutralizing_gas(state) {
        handle_neutralizing_gas_lift(state);
    }

    // Primal weather ends when its source faints, unless another holder remains.
    handle_primal_weather_departure(state, &fainted_ability);
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
fn apply_switch_out_ability_effects(mon: &mut PokemonState) {
    if mon.fainted {
        return;
    }
    match mon.ability {
        Ability::NaturalCure => {
            mon.status = None;
        }
        Ability::Regenerator => {
            let heal = (mon.stats[0] / 3).max(1);
            mon.hp = mon.hp.saturating_add(heal).min(mon.stats[0].max(1));
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
    match player {
        Player::P1 => {
            if state
                .p1_side_conditions
                .iter()
                .any(|sc| std::mem::discriminant(sc) == std::mem::discriminant(&condition))
            {
                return;
            }
            state.p1_side_conditions.push(condition);
            state.p1_side_condition_turns.push(duration);
        }
        Player::P2 => {
            if state
                .p2_side_conditions
                .iter()
                .any(|sc| std::mem::discriminant(sc) == std::mem::discriminant(&condition))
            {
                return;
            }
            state.p2_side_conditions.push(condition);
            state.p2_side_condition_turns.push(duration);
        }
    }
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

    prune_timed_effects(&mut state.pseudo_weathers, &mut state.pseudo_weather_turns);

    prune_timed_effects(&mut state.p1_side_conditions, &mut state.p1_side_condition_turns);
    prune_timed_effects(&mut state.p2_side_conditions, &mut state.p2_side_condition_turns);

    // Volatile status duration 0 means permanent, so preserve it. (Back mons cannot have volatiles)
    decrement_volatile_statuses(&mut state.p1_active_mons);
    //decrement_volatile_statuses(&mut state.p1_back_mons);
    decrement_volatile_statuses(&mut state.p2_active_mons);
    //decrement_volatile_statuses(&mut state.p2_back_mons);

    // timers decremented; other end-of-turn effects handled by `end_turn`
}

/// Perform full end-of-turn processing.
/// This wraps timer decrementing and other end-of-turn effects (residual damage, leech seed, poison/burn, etc.).
pub fn end_turn(state: &mut BattleState) {
    // Decrement effect timers (weather, pseudo-weather, side conditions)
    decrement_effect_timers(state);

    // Advance the battle turn after end-of-turn processing is complete.
    state.turn_number = state.turn_number.saturating_add(1);

    // Apply residual damage effects (burn, poison, toxic)
    apply_end_of_turn_status_effects(state);

    // TODO: Additional end-of-turn effects: leech seed, sandstorm/hail, etc.
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
        // These last indefinitely
        SideCondition::Spikes | SideCondition::StealthRock | SideCondition::ToxicSpikes => 0,
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
fn deal_residual_damage(mon: &mut PokemonState, amount: u16) -> bool {
    if amount == 0 { return false; }
    apply_damage(mon, amount);
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

fn apply_weather_residual(mon: &mut PokemonState, ctx: &WeatherResidualCtx) {
    if mon.fainted { return; }
    let max_hp = mon.stats[0].max(1);

    if ctx.rain && !ctx.abilities_suppressed {
        if mon.ability == Ability::RainDish { heal_mon(mon, (max_hp as u32 / 16) as u16); }
        if mon.ability == Ability::DrySkin  { heal_mon(mon, (max_hp as u32 / 8)  as u16); }
    }

    if ctx.snow && !ctx.abilities_suppressed && mon.ability == Ability::IceBody {
        heal_mon(mon, (max_hp as u32 / 16) as u16);
    }

    if ctx.sun && !ctx.abilities_suppressed {
        if mon.ability == Ability::DrySkin  && deal_residual_damage(mon, (max_hp as u32 / 8) as u16) { return; }
        if mon.ability == Ability::SolarPower && deal_residual_damage(mon, (max_hp as u32 / 8) as u16) { return; }
    }

    if !ctx.sandstorm { return; }

    let sandstorm_immune = pokemon_has_type(mon, &PokemonType::Steel)
        || pokemon_has_type(mon, &PokemonType::Rock)
        || pokemon_has_type(mon, &PokemonType::Ground)
        || (!ctx.abilities_suppressed && matches!(mon.ability,
            Ability::SandForce | Ability::SandRush | Ability::SandVeil | Ability::MagicGuard | Ability::Overcoat))
        || (!ctx.items_suppressed && matches!(mon.item, Item::SafetyGoggles));

    if !sandstorm_immune {
        deal_residual_damage(mon, (mon.stats[0] as u32 / 16) as u16);
    }
}

fn apply_status_residual(mon: &mut PokemonState, abilities_suppressed: bool, rain_active: bool) {
    if mon.fainted { return; }

    if !abilities_suppressed && mon.ability == Ability::Hydration && rain_active {
        mon.status = None;
    }

    let magic_guard = !abilities_suppressed && mon.ability == Ability::MagicGuard;

    match mon.status {
        Some(Status::Burn) => {
            if !magic_guard { deal_residual_damage(mon, (mon.stats[0] as u32 / 16) as u16); }
        }
        Some(Status::Poison) => {
            if !magic_guard { deal_residual_damage(mon, (mon.stats[0] as u32 / 8) as u16); }
        }
        Some(Status::ToxicPoison(n)) => {
            let new_n = n.saturating_add(1);
            mon.status = Some(Status::ToxicPoison(new_n));
            if !magic_guard { deal_residual_damage(mon, (mon.stats[0] as u32 * new_n as u32 / 16) as u16); }
        }
        _ => {}
    }
}

/// Apply end-of-turn effects for non-volatile status conditions (Burn, Poison, ToxicPoison)
pub fn apply_end_of_turn_status_effects(state: &mut BattleState) {
    let ctx = WeatherResidualCtx {
        rain: weather_is_rain(state),
        snow: weather_is_snow(state),
        sun: weather_is_sunlight(state),
        sandstorm: weather_is_sandstorm(state),
        abilities_suppressed: abilities_are_suppressed(state),
        items_suppressed: items_are_suppressed(state),
    };

    for mon in state.p1_active_mons.iter_mut() { apply_weather_residual(mon, &ctx); }
    for mon in state.p2_active_mons.iter_mut() { apply_weather_residual(mon, &ctx); }

    // Grassy Terrain healing
    let terrain_snapshot = state.clone();
    if matches!(current_terrain(&terrain_snapshot), Some(Terrain::GrassyTerrain)) {
        for mon in state.p1_active_mons.iter_mut().chain(state.p2_active_mons.iter_mut()) {
            if !mon.fainted && pokemon_is_grounded(&terrain_snapshot, mon) {
                let max_hp = mon.stats[0].max(1);
                heal_mon(mon, (max_hp as u32 / 16) as u16);
            }
        }
    }

    for mon in state.p1_active_mons.iter_mut() { apply_status_residual(mon, ctx.abilities_suppressed, ctx.rain); }
    for mon in state.p2_active_mons.iter_mut() { apply_status_residual(mon, ctx.abilities_suppressed, ctx.rain); }
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

            let is_move_status = matches!(
                volatile,
                VolatileStatus::Disable
                    | VolatileStatus::Encore
                    | VolatileStatus::GlaiveRush
                    | VolatileStatus::Taunt
                    | VolatileStatus::SemiInvulnerable(_)
                    | VolatileStatus::Confusion
            );

        let duration = match volatile {
            VolatileStatus::Disable => 4,
            VolatileStatus::Encore => 3,
            VolatileStatus::Taunt => 3,
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
    }
}

/// Apply stat boosts to a pokemon.
fn apply_stat_boosts_to_pokemon(mon: &mut PokemonState, boosts: &[i8; 7]) {
    for i in 0..7 {
        mon.boosts[i] = (mon.boosts[i] + boosts[i]).clamp(-6, 6);
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
    let sun_blocks_freeze = weather_is_sunlight(state);
    let terrain_snapshot = state.clone();
    if let Some(target_mon) = get_pokemon_at_slot_mut(state, target_slot) {
        if let Some(status) = &effect.status {
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

        if let Some(volatile) = &effect.volatile_status {
            apply_volatile_to_pokemon(&terrain_snapshot, target_mon, volatile);
        }

        if effect.boosts != [0; 7] {
            apply_stat_boosts_to_pokemon(target_mon, &effect.boosts);
        }
    }

    if let Some(side_condition) = &effect.side_condition {
        if !(matches!(side_condition, SideCondition::AuroraVeil) && !weather_is_snow(state)) {
            let duration = get_side_condition_duration(side_condition);
            add_side_condition(state, side_condition_player, side_condition.clone(), duration);
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
    let terrain_snapshot = state.clone();
    if let Some(attacker_mon) = get_pokemon_at_slot_mut(state, attacker_slot) {
        if let Some(status) = &effect.status {
            apply_status_to_pokemon(&terrain_snapshot, sun_blocks_freeze, attacker_mon, status);
        }

        if let Some(volatile) = &effect.volatile_status {
            apply_volatile_to_pokemon(&terrain_snapshot, attacker_mon, volatile);
        }

        if effect.boosts != [0; 7] {
            apply_stat_boosts_to_pokemon(attacker_mon, &effect.boosts);
        }
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

    let Some(attacker_mon) = get_pokemon_at_slot_mut(bs, attacker_slot) else { return false; };

    match move_name {
        PokemonMove::Rest => {
            if pokemon_is_on_terrain(terrain_snapshot, attacker_mon, &Terrain::ElectricTerrain) {
                return false;
            }
            attacker_mon.volatiles.clear();
            attacker_mon.status = Some(Status::Sleep(0));
            let max_hp = attacker_mon.stats[0].max(1);
            attacker_mon.hp = max_hp;
            attacker_mon.fainted = false;
        }
        PokemonMove::Synthesis | PokemonMove::MorningSun | PokemonMove::Moonlight => {
            let (num, den) = if branch_harsh_sun { (2u32, 3u32) }
                             else if matches!(branch_weather, None | Some(Weather::StrongWinds)) { (1u32, 2u32) }
                             else { (1u32, 4u32) };
            let max_hp = attacker_mon.stats[0].max(1);
            let heal = ((max_hp as u32 * num) / den) as u16;
            heal_mon(attacker_mon, heal);
        }
        PokemonMove::ShoreUp => {
            let (num, den) = if branch_sandstorm { (2u32, 3u32) } else { (1u32, 4u32) };
            let max_hp = attacker_mon.stats[0].max(1);
            let heal = ((max_hp as u32 * num) / den) as u16;
            heal_mon(attacker_mon, heal);
        }
        _ => return false,
    }
    true
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

    // Branch target secondaries
    let mut branches: Vec<(BattleState, f64)> = vec![(state.clone(), 1.0)];
    for secondary in &move_data.secondaries {
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

    // Unconditional self-boosts
    if move_data.self_boost != [0; 7] {
        for (bs, _) in branches.iter_mut() {
            let growth_in_sun = move_data.name == PokemonMove::Growth && weather_is_sunlight(bs);
            if let Some(attacker_mon) = get_pokemon_at_slot_mut(bs, attacker_slot) {
                let mut boosts = move_data.self_boost;
                if growth_in_sun {
                    boosts[0] = boosts[0].saturating_add(1);
                    boosts[2] = boosts[2].saturating_add(1);
                }
                apply_stat_boosts_to_pokemon(attacker_mon, &boosts);
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
    || (!items_are_suppressed(state) && matches!(mon.item, Item::SafetyGoggles))
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
pub fn check_and_apply_redirection(
    state: &BattleState,
    user_slot: FieldSlot,
    target_slots: Vec<FieldSlot>,
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

    // Find all opposing PokÃ©mon with FollowMe or RagePowder, excluding those with SkyDrop+redirect
    let mut redirectors: Vec<(FieldSlot, &PokemonState)> = Vec::new();

    for (idx, mon) in opposing_mons.iter().enumerate() {
        // Skip fainted PokÃ©mon
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

    // If there are redirectors, pick the one with the highest effective speed
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

    target_slots
}

pub(crate) fn get_pokemon_at_slot_mut<'a>(state: &'a mut BattleState, slot: FieldSlot) -> Option<&'a mut PokemonState> {
    let mons = match slot.player {
        Player::P1 => &mut state.p1_active_mons,
        Player::P2 => &mut state.p2_active_mons,
    };
    mons.get_mut(slot.slot_index as usize)
}
