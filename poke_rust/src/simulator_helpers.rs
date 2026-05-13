use crate::battle::{Action, BattleState, FieldSlot, Player, MoveAction};
use crate::data::ability::Ability;
use crate::data::item::Item;
use crate::data::pokemon_move::PokemonMove;
use crate::dex_data::{AccuracyType, MoveCategory, MoveData, MoveTarget, PokemonType, PseudoWeather, SideCondition, Terrain, Weather, HitEffect, MoveFlag, PokemonStat, Status};
use crate::pokemon::{PokemonState, VolatileStatusState};
use crate::dex_data::VolatileStatus;

pub fn get_verbosity() -> u8 {
    crate::VERBOSITY.get().copied().unwrap_or(1)
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

pub fn effective_stat(mon: &PokemonState, stat: PokemonStat, ignore_negative: bool, ignore_positive: bool) -> f64 {
    let (stat_index, boost_index) = match stat {
        PokemonStat::Atk => (1, 0),
        PokemonStat::Def => (2, 1),
        PokemonStat::SpA => (3, 2),
        PokemonStat::SpD => (4, 3),
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

    base_stat * stage_multiplier(applied_stage)
}

pub fn pokemon_has_type(mon: &PokemonState, pokemon_type: &PokemonType) -> bool {
    mon.types.iter().any(|current_type| std::mem::discriminant(current_type) == std::mem::discriminant(pokemon_type))
}

pub fn single_type_effectiveness(move_type: &PokemonType, target_type: &PokemonType) -> f64 {
    use PokemonType::*;

    match (move_type, target_type) {
        (Normal, Fighting) => 0.0,
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

pub fn move_type_effectiveness(move_type: &PokemonType, target: &PokemonState) -> f64 {
    if target.types.is_empty() {
        return 1.0;
    }

    target
        .types
        .iter()
        .fold(1.0, |effectiveness, target_type| effectiveness * single_type_effectiveness(move_type, target_type))
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
        1.5
    } else {
        1.0
    }
}

pub fn type_effectiveness_label(effectiveness: f64) -> &'static str {
    if effectiveness == 0.0 {
        "no effect"
    } else if effectiveness < 1.0 {
        "mostly ineffective"
    } else if (effectiveness - 1.0).abs() < f64::EPSILON {
        "normal effectiveness"
    } else if effectiveness < 4.0 {
        "super effective"
    } else {
        "extremely effective"
    }
}

pub fn crit_is_prevented(target: &PokemonState) -> bool {
    if target.ability == Ability::BattleArmor || target.ability == Ability::ShellArmor {
        return true;
    }
    false
}

pub fn crit_is_guaranteed(attacker: &PokemonState, target: &PokemonState, move_name: &PokemonMove) -> bool {
    let target_is_poisoned = matches!(target.status, Some(Status::Poison | Status::ToxicPoison));
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
        _ => 1.0 / 16.0,
    };

    vec![(false, 1.0 - crit_chance), (true, crit_chance)]
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

pub fn move_target_includes_allies(target: &MoveTarget) -> bool {
    matches!(
        target,
        MoveTarget::All
            | MoveTarget::AllAdjacent
            | MoveTarget::Allies
            | MoveTarget::AllySide
            | MoveTarget::AllyTeam
            | MoveTarget::AdjacentAlly
            | MoveTarget::AdjacentAllyOrSelf
    )
}

pub fn resolve_move_targets(
    state: &BattleState,
    user_slot: FieldSlot,
    target: &MoveTarget,
) -> Vec<FieldSlot> {
    let mut targets = Vec::new();
    
    match target {
        // Single target moves - these should be handled via action.target_slot, but fallback to first available
        MoveTarget::AdjacentFoe | MoveTarget::Normal | MoveTarget::Any => {
            // These require explicit targeting, should use action.target_slot
            // Fallback: first healthy opposing mon
            let opposing_mons = match user_slot.player {
                Player::P1 => &state.p2_active_mons,
                Player::P2 => &state.p1_active_mons,
            };
            for (idx, mon) in opposing_mons.iter().enumerate() {
                if !mon.fainted {
                    targets.push(FieldSlot {
                        player: match user_slot.player {
                            Player::P1 => Player::P2,
                            Player::P2 => Player::P1,
                        },
                        slot_index: idx as u8,
                    });
                    break;
                }
            }
        }
        // All adjacent foes
        MoveTarget::AllAdjacentFoes | MoveTarget::FoeSide => {
            let opposing_mons = match user_slot.player {
                Player::P1 => &state.p2_active_mons,
                Player::P2 => &state.p1_active_mons,
            };
            for (idx, mon) in opposing_mons.iter().enumerate() {
                if !mon.fainted {
                    targets.push(FieldSlot {
                        player: match user_slot.player {
                            Player::P1 => Player::P2,
                            Player::P2 => Player::P1,
                        },
                        slot_index: idx as u8,
                    });
                }
            }
        }
        // All allies (not including self)
        MoveTarget::Allies | MoveTarget::AllySide | MoveTarget::AllyTeam | MoveTarget::AdjacentAlly => {
            let ally_mons = match user_slot.player {
                Player::P1 => &state.p1_active_mons,
                Player::P2 => &state.p2_active_mons,
            };
            for (idx, mon) in ally_mons.iter().enumerate() {
                if idx as u8 != user_slot.slot_index && !mon.fainted {
                    targets.push(FieldSlot {
                        player: user_slot.player,
                        slot_index: idx as u8,
                    });
                }
            }
        }
        // All pokemon on field (including self)
        MoveTarget::All | MoveTarget::AllAdjacent => {
            // All allies
            let ally_mons = match user_slot.player {
                Player::P1 => &state.p1_active_mons,
                Player::P2 => &state.p2_active_mons,
            };
            for (idx, mon) in ally_mons.iter().enumerate() {
                if !mon.fainted {
                    targets.push(FieldSlot {
                        player: user_slot.player,
                        slot_index: idx as u8,
                    });
                }
            }
            // All opponents
            let opposing_mons = match user_slot.player {
                Player::P1 => &state.p2_active_mons,
                Player::P2 => &state.p1_active_mons,
            };
            for (idx, mon) in opposing_mons.iter().enumerate() {
                if !mon.fainted {
                    targets.push(FieldSlot {
                        player: match user_slot.player {
                            Player::P1 => Player::P2,
                            Player::P2 => Player::P1,
                        },
                        slot_index: idx as u8,
                    });
                }
            }
        }
        // Self-target
        MoveTarget::SelfTarget | MoveTarget::AdjacentAllyOrSelf => {
            targets.push(user_slot);
        }
        _ => {
            // Fallback for unknown or scripted targets
            let opposing_mons = match user_slot.player {
                Player::P1 => &state.p2_active_mons,
                Player::P2 => &state.p1_active_mons,
            };
            for (idx, mon) in opposing_mons.iter().enumerate() {
                if !mon.fainted {
                    targets.push(FieldSlot {
                        player: match user_slot.player {
                            Player::P1 => Player::P2,
                            Player::P2 => Player::P1,
                        },
                        slot_index: idx as u8,
                    });
                    break;
                }
            }
        }
    }
    
    targets
}

pub fn damage_targets_multiplier(target_count: usize) -> f64 {
    if target_count > 1 { 0.75 } else { 1.0 }
}

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
    let attacking_stat = match move_offensive_stat(move_data) {
        Some(stat) => stat,
        None => return vec![(0, false, 1.0)],
    };

    let defending_stat = match move_defensive_stat(move_data) {
        Some(stat) => stat,
        None => return vec![(0, false, 1.0)],
    };

    let attacker_stat = effective_stat(attacker, attacking_stat, false, false);
    let target_effective_defense = effective_stat(target, defending_stat, false, false);
    let effectiveness = move_type_effectiveness(&move_data.pokemon_type, target);
    let stab = stab_multiplier(attacker, &move_data.pokemon_type);

    let damage_roll_values = selected_damage_rolls(config.damage_rolls);
    let critical_states = critical_hit_probability(attacker, target, &move_data.name, config.consider_crit, move_data.crit_ratio);

    let mut outcomes = Vec::new();

    for (crit, crit_probability) in critical_states {
        let critical_multiplier = if crit { 1.5 } else { 1.0 };
        let attack_stat = if crit {
            effective_stat(attacker, attacking_stat, true, false)
        } else {
            attacker_stat
        };
        let defense_stat = if crit {
            effective_stat(target, defending_stat, false, true)
        } else {
            target_effective_defense
        };

        let base_damage = (((((2.0 * attacker.level as f64 / 5.0 + 2.0) * attack_stat * move_data.base_power as f64 / defense_stat) / 50.0) + 2.0)
            * stab
            * effectiveness
            * invulnerability_multiplier
            * critical_multiplier
            * targets_multiplier)
            .floor()
            .max(0.0);

        for roll in &damage_roll_values {
            let random_multiplier = *roll as f64 / 100.0;
            let damage = (base_damage * random_multiplier).floor().max(0.0) as u16;
            let probability = crit_probability / damage_roll_values.len() as f64;
            outcomes.push((damage, crit, probability));
        }
    }

    outcomes
}

pub fn damage_effectiveness_for_action(state: &BattleState, action: &MoveAction, move_data: &MoveData) -> f64 {
    let Some(target_slot) = action.target_slot else {
        return 1.0;
    };

    let Some(target) = get_pokemon_at_slot(state, target_slot) else {
        return 1.0;
    };

    match invulnerability_resolution(target, &move_data.name) {
        InvulnerabilityResolution::Blocked => 0.0,
        InvulnerabilityResolution::Normal => move_type_effectiveness(&move_data.pokemon_type, target),
        InvulnerabilityResolution::DoubleDamage => move_type_effectiveness(&move_data.pokemon_type, target) * 2.0,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvulnerabilityResolution {
    Blocked,
    Normal,
    DoubleDamage,
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
        PokemonMove::PhantomForce | PokemonMove::ShadowForce | PokemonMove::SkyDrop => {
            InvulnerabilityResolution::Blocked
        }
        _ => InvulnerabilityResolution::Normal,
    }
}

pub fn invulnerability_resolution(target: &PokemonState, attack_move: &PokemonMove) -> InvulnerabilityResolution {
    let source_move_opt = target.volatiles.iter().find_map(|volatile| {
        if let VolatileStatusState::MoveStatus(VolatileStatus::SemiInvulnerable(mov), _) = volatile {
            Some(mov)
        } else {
            None
        }
    });

    let Some(source_move) = source_move_opt else {
        return InvulnerabilityResolution::Normal;
    };

    invulnerability_resolution_for_source_move(source_move, attack_move)
}

pub fn add_invulnerable_volatile(mon: &mut PokemonState, move_name: PokemonMove, targets: Vec<FieldSlot>) {
    let already_has = mon.volatiles.iter().any(|volatile| {
        matches!(
            volatile,
            VolatileStatusState::MoveStatus(VolatileStatus::SemiInvulnerable(_), _)
        )
    });

    if !already_has {
        mon.volatiles.push(VolatileStatusState::MoveStatus(VolatileStatus::SemiInvulnerable(move_name), 1));
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

fn weather_is_sandstorm(state: &BattleState) -> bool {
    state
        .weathers
        .iter()
        .any(|weather| matches!(weather, Weather::Sandstorm))
}

fn weather_is_snow(state: &BattleState) -> bool {
    state
        .weathers
        .iter()
        .any(|weather| matches!(weather, Weather::Snow))
}

fn is_confused(mon: &PokemonState) -> bool {
    mon.volatiles
        .iter()
        .any(|volatile_status| matches!(volatile_status, VolatileStatusState::TurnStatus(VolatileStatus::Confusion, _)))
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
            Action::Pass => None,
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

    if target.ability == Ability::TangledFeet && is_confused(target) {
        modifier = apply_modifier_fp(modifier, 2048);
    }

    if attacker.ability == Ability::Hustle && matches!(move_data.category, MoveCategory::Physical) {
        modifier = apply_modifier_fp(modifier, 3277);
    }

    if target.ability == Ability::SandVeil && weather_is_sandstorm(state) {
        modifier = apply_modifier_fp(modifier, 3277);
    }

    if target.ability == Ability::SnowCloak && weather_is_snow(state) {
        modifier = apply_modifier_fp(modifier, 3277);
    }

    let allies = user_active_mons(state, user_slot.player);
    let victory_star_count = allies
        .iter()
        .enumerate()
        .filter(|(idx, mon)| {
            !mon.fainted
                && mon.ability == Ability::VictoryStar
                && (*idx as u8 != user_slot.slot_index || attacker.ability == Ability::VictoryStar)
        })
        .count();

    for _ in 0..victory_star_count {
        modifier = apply_modifier_fp(modifier, 4506);
    }

    if attacker.ability == Ability::CompoundEyes {
        modifier = apply_modifier_fp(modifier, 5325);
    }

    if matches!(target.item, Item::BrightPowder) {
        modifier = apply_modifier_fp(modifier, 3686);
    }

    if matches!(target.item, Item::LaxIncense) {
        modifier = apply_modifier_fp(modifier, 3686);
    }

    if matches!(attacker.item, Item::WideLens) {
        modifier = apply_modifier_fp(modifier, 4505);
    }

    if matches!(attacker.item, Item::ZoomLens) && target_has_acted_this_turn(state, target_slot) {
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

pub fn accuracy_hit_probability(
    state: &BattleState,
    attacker: &PokemonState,
    target: &PokemonState,
    user_slot: FieldSlot,
    target_slot: FieldSlot,
    move_data: &MoveData,
) -> f64 {
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

fn get_effective_speed(mon: &PokemonState) -> f32 {
    let base_speed = mon.stats[5] as f32;
    let speed_boost = mon.boosts[4];

    let multiplier = if speed_boost > 0 {
        1.0 + (0.5 * speed_boost as f32)
    } else if speed_boost < 0 {
        1.0 / (1.0 + (0.5 * (-speed_boost) as f32))
    } else {
        1.0
    };

    base_speed * multiplier
}

fn compare_pokemon_speed(p1: &PokemonState, p2: &PokemonState) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let speed1 = get_effective_speed(p1);
    let speed2 = get_effective_speed(p2);

    if (speed2 - speed1).abs() < 0.01 {
        Ordering::Equal
    } else if speed2 > speed1 {
        Ordering::Greater
    } else {
        Ordering::Less
    }
}

fn get_action_type_priority(action: &Action) -> u8 {
    match action {
        Action::SwitchAction(_) => 0,
        Action::MegaAction(_) => 1,
        Action::TeraAction(_) => 2,
        Action::MoveAction(_) => 3,
        Action::Pass => 4,
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
                (Some(p1), Some(p2)) => compare_pokemon_speed(p1, p2),
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

/// Set weather, avoiding duplicates and handling duration.
/// If the same weather is already active, ignore the new one.
pub fn set_weather(state: &mut BattleState, weather: Weather, duration: u8) {
    if state.weathers.iter().any(|w| std::mem::discriminant(w) == std::mem::discriminant(&weather)) {
        return;
    }
    state.weathers.push(weather);
    state.weather_turns.push(duration);
}

/// Set terrain. Only one terrain can be active at a time. Provide a duration in turns (0 = permanent).
pub fn set_terrain(state: &mut BattleState, terrain: Terrain, duration: u8) {
    if state.terrains.iter().any(|w| std::mem::discriminant(w) == std::mem::discriminant(&terrain)) {
        return;
    }
    state.terrains.clear();
    state.terrain_turns.clear();
    state.terrains.push(terrain);
    state.terrain_turns.push(duration);
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
            match volatile {
                VolatileStatusState::TurnStatus(effect, turns) => {
                    if turns == 0 {
                        kept.push(VolatileStatusState::TurnStatus(effect, 0));
                    } else if turns > 1 {
                        kept.push(VolatileStatusState::TurnStatus(effect, turns - 1));
                    }
                }
                otherVolatile => {kept.push(otherVolatile)}
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
            otherVolatile => {kept.push(otherVolatile)}
        }
    }

    mon.volatiles = kept;
}

/// Decrement effect timers at end of turn.
/// Call this before setting turn_ended = true.
pub fn decrement_effect_timers(state: &mut BattleState) {
    prune_timed_effects(&mut state.weathers, &mut state.weather_turns);
    prune_timed_effects(&mut state.pseudo_weathers, &mut state.pseudo_weather_turns);

    // Terrains behave like weathers with turn counters.
    prune_timed_effects(&mut state.terrains, &mut state.terrain_turns);

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

    // TODO: Apply residual damage effects and other end-of-turn logic:
    // - Poison damage (1/8 HP per turn)
    // - Burn damage (1/16 HP per turn)
    // - Leech Seed damage (1/8 HP, heal attacker)
    // - Sandstorm damage (1/8 HP if not Rock/Ground/Steel)
    // - Hail damage (1/8 HP if not Ice type)
    // - Toxic damage (cumulative 1/8, 2/8, 3/8, etc.)
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
        | VolatileStatus::MaxGuard => 1,
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
fn apply_status_to_pokemon(mon: &mut PokemonState, status: &crate::dex_data::Status) {
    if mon.status.is_none() {
        mon.status = Some(status.clone());
    }
}

/// Apply a volatile status to a pokemon (prevents duplicate volatiles of the same type).
fn apply_volatile_to_pokemon(mon: &mut PokemonState, volatile: &VolatileStatus) {
    // Check if pokemon already has this volatile status
    let already_has = has_status_volatile(mon, volatile);

    if !already_has {
        let is_move_status = matches!(
            volatile,
            VolatileStatus::Disable | VolatileStatus::Encore | VolatileStatus::GlaiveRush | VolatileStatus::Taunt | VolatileStatus::SemiInvulnerable(_)
        );

        let duration = match volatile {
            VolatileStatus::Disable => 4,
            VolatileStatus::Encore => 3,
            VolatileStatus::Taunt => 3,
            VolatileStatus::GlaiveRush => 1,
            VolatileStatus::SemiInvulnerable(_) => 1,
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
    target_slot: FieldSlot,
    effect: &HitEffect,
    side_condition_player: Player,
) {
    if let Some(target_mon) = get_pokemon_at_slot_mut(state, target_slot) {
        if let Some(status) = &effect.status {
            apply_status_to_pokemon(target_mon, status);
        }

        if let Some(volatile) = &effect.volatile_status {
            apply_volatile_to_pokemon(target_mon, volatile);
        }

        if effect.boosts != [0; 7] {
            apply_stat_boosts_to_pokemon(target_mon, &effect.boosts);
        }
    }

    if let Some(side_condition) = &effect.side_condition {
        let duration = get_side_condition_duration(side_condition);
        add_side_condition(state, side_condition_player, side_condition.clone(), duration);
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
    if let Some(attacker_mon) = get_pokemon_at_slot_mut(state, attacker_slot) {
        if let Some(status) = &effect.status {
            apply_status_to_pokemon(attacker_mon, status);
        }

        if let Some(volatile) = &effect.volatile_status {
            apply_volatile_to_pokemon(attacker_mon, volatile);
        }

        if effect.boosts != [0; 7] {
            apply_stat_boosts_to_pokemon(attacker_mon, &effect.boosts);
        }
    }

    if let Some(side_condition) = &effect.side_condition {
        let duration = get_side_condition_duration(side_condition);
        add_side_condition(state, attacker_slot.player, side_condition.clone(), duration);
    }

    apply_weather_effects(state, effect);
    apply_terrain_effects(state, effect);
}

/// Apply move secondary effects with appropriate probability.
/// This is called after a move hits to apply status, volatile status, side conditions, etc.
pub fn apply_secondary_effects(
    state: &BattleState,
    attacker_slot: FieldSlot,
    target_slot: FieldSlot,
    move_data: &MoveData,
) -> Vec<(BattleState, f64)> {
    // Start with the original state as a single branch
    let mut branches: Vec<(BattleState, f64)> = vec![(state.clone(), 1.0)];

    // Determine which side should receive side conditions based on move target
    let side_condition_target = match move_data.target {
        MoveTarget::FoeSide | MoveTarget::AllAdjacentFoes | MoveTarget::AllAdjacent => {
            // These moves affect the opponent's side
            match attacker_slot.player {
                Player::P1 => Player::P2,
                Player::P2 => Player::P1,
            }
        }
        MoveTarget::AllySide | MoveTarget::Allies | MoveTarget::AllyTeam => {
            // These moves affect the user's side
            attacker_slot.player
        }
        _ => target_slot.player, // Default to target's side
    };

    // Process target secondaries, branching on each chance
    for secondary in &move_data.secondaries {
        let mut new_branches: Vec<(BattleState, f64)> = Vec::new();
        let chance = (secondary.chance as f64) / 100.0;

        for (bs, prob) in branches.into_iter() {
            // Branch where effect does not occur
            if (1.0 - chance) > 0.0 {
                new_branches.push((bs.clone(), prob * (1.0 - chance)));
            }

            // Branch where effect occurs
            if chance > 0.0 {
                let mut applied = bs.clone();
                apply_effect_to_target(&mut applied, target_slot, &secondary.effect, side_condition_target);
                new_branches.push((applied, prob * chance));
            }
        }

        branches = new_branches;
    }

    // Apply the move's unconditional self-boosts to the attacker (applies when the move hits)
    if move_data.self_boost != [0; 7] {
        for (bs, _prob) in branches.iter_mut() {
            if let Some(attacker_mon) = get_pokemon_at_slot_mut(bs, attacker_slot) {
                apply_stat_boosts_to_pokemon(attacker_mon, &move_data.self_boost);
            }
        }
    }

    // Process self-secondaries (affecting the attacker); similar branching
    for secondary in &move_data.self_secondaries {
        let mut new_branches: Vec<(BattleState, f64)> = Vec::new();
        let chance = (secondary.chance as f64) / 100.0;

        for (bs, prob) in branches.into_iter() {
            // No-effect branch
            if (1.0 - chance) > 0.0 {
                new_branches.push((bs.clone(), prob * (1.0 - chance)));
            }

            if chance > 0.0 {
                let mut applied = bs.clone();
                apply_effect_to_attacker(&mut applied, attacker_slot, &secondary.effect);
                new_branches.push((applied, prob * chance));
            }
        }

        branches = new_branches;
    }

    // Normalize small floating point drift (optional)
    branches.into_iter().filter(|(_, p)| *p > 0.0).collect()
}

fn get_pokemon_at_slot_mut<'a>(state: &'a mut BattleState, slot: FieldSlot) -> Option<&'a mut PokemonState> {
    let mons = match slot.player {
        Player::P1 => &mut state.p1_active_mons,
        Player::P2 => &mut state.p2_active_mons,
    };
    mons.get_mut(slot.slot_index as usize)
}
