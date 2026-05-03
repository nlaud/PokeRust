use std::collections::HashMap;
use colored::Colorize;
use crate::battle::{
    MatchState, BattleState, TeamPreviewState, PlayerCommand, BattleCommand,
    AttackCommand, SwitchCommand, TeamPreviewCommand, Player, FieldSlot,
    Action, MoveAction, SwitchAction, MegaAction, TeraAction,
};
use crate::pokemon::{
    PokemonState, parse_team_sheet
};
use crate::dex_data::{MoveData, MoveTarget, PokemonData};
use crate::dex_data::{MoveCategory, PokemonStat, Status, VolatileStatus};
use crate::data::ability::Ability;
use crate::data::species::Species;
use crate::data::pokemon_move::PokemonMove;
use crate::dex_data::PokemonType;

#[derive(Clone, Copy)]
struct DamageConfig {
    consider_crit: bool,
    damage_rolls: u8,
}

fn get_verbosity() -> u8 {
    crate::VERBOSITY.get().copied().unwrap_or(1)
}

fn humanize_identifier(value: &str) -> String {
    let mut result = String::new();
    let mut previous: Option<char> = None;
    for current in value.chars() {
        let insert_space = match previous {
            Some(prev) => (prev.is_ascii_lowercase() && current.is_ascii_uppercase())
                || (prev.is_ascii_digit() && current.is_ascii_alphabetic())
                || (prev.is_ascii_alphabetic() && current.is_ascii_digit()),
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

fn species_name_sim(species: &crate::data::species::Species) -> String {
    humanize_identifier(&format!("{:?}", species))
}

fn move_name_sim(mov: &crate::data::pokemon_move::PokemonMove) -> String {
    humanize_identifier(&format!("{:?}", mov))
}

fn pokemon_type_name(pokemon_type: &PokemonType) -> &'static str {
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

fn move_target_is_multitarget(target: &MoveTarget) -> bool {
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

fn stage_multiplier(stage: i8) -> f64 {
    let stage = stage.clamp(-6, 6);
    if stage >= 0 {
        (2.0 + stage as f64) / 2.0
    } else {
        2.0 / (2.0 - stage as f64)
    }
}

fn effective_stat(mon: &PokemonState, stat: PokemonStat, ignore_negative: bool, ignore_positive: bool) -> f64 {
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

fn pokemon_has_type(mon: &PokemonState, pokemon_type: &PokemonType) -> bool {
    mon.types.iter().any(|current_type| std::mem::discriminant(current_type) == std::mem::discriminant(pokemon_type))
}

fn single_type_effectiveness(move_type: &PokemonType, target_type: &PokemonType) -> f64 {
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

fn move_type_effectiveness(move_type: &PokemonType, target: &PokemonState) -> f64 {
    if target.types.is_empty() {
        return 1.0;
    }

    target
        .types
        .iter()
        .fold(1.0, |effectiveness, target_type| effectiveness * single_type_effectiveness(move_type, target_type))
}

fn stab_multiplier(attacker: &PokemonState, move_type: &PokemonType) -> f64 {
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

fn type_effectiveness_label(effectiveness: f64) -> &'static str {
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

fn crit_is_prevented(attacker: &PokemonState, target: &PokemonState, move_name: &PokemonMove) -> bool {
    if target.ability == Ability::BattleArmor || target.ability == Ability::ShellArmor {
        return true;
    }

    let target_is_poisoned = matches!(target.status, Some(Status::Poison | Status::ToxicPoison));
    let merciless_crit = attacker.ability == Ability::Merciless && target_is_poisoned;
    let laser_focus = attacker.volatiles.iter().any(|volatile| matches!(volatile, crate::pokemon::VolatileStatusState::Status(VolatileStatus::LaserFocus, _)));
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

fn critical_hit_probability(attacker: &PokemonState, target: &PokemonState, move_name: &PokemonMove, consider_crit: bool) -> Vec<(bool, f64)> {
    if !consider_crit {
        return vec![(false, 1.0)];
    }

    if target.ability == Ability::BattleArmor || target.ability == Ability::ShellArmor {
        return vec![(false, 1.0)];
    }

    if crit_is_prevented(attacker, target, move_name) {
        return vec![(true, 1.0)];
    }

    vec![(false, 23.0 / 24.0), (true, 1.0 / 24.0)]
}

fn selected_damage_rolls(count: u8) -> Vec<u8> {
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

fn move_offensive_stat(move_data: &MoveData) -> Option<PokemonStat> {
    if let Some(stat) = move_data.override_offensive_stat {
        return Some(stat);
    }

    match move_data.category {
        MoveCategory::Physical => Some(PokemonStat::Atk),
        MoveCategory::Special => Some(PokemonStat::SpA),
        MoveCategory::Status => None,
    }
}

fn move_defensive_stat(move_data: &MoveData) -> Option<PokemonStat> {
    if let Some(stat) = move_data.override_defensive_stat {
        return Some(stat);
    }

    match move_data.category {
        MoveCategory::Physical => Some(PokemonStat::Def),
        MoveCategory::Special => Some(PokemonStat::SpD),
        MoveCategory::Status => None,
    }
}

fn move_target_includes_allies(target: &MoveTarget) -> bool {
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

fn resolve_move_targets(
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

fn damage_targets_multiplier(target_count: usize) -> f64 {
    if target_count > 1 { 0.75 } else { 1.0 }
}

/// Calculate damage outcomes for a single target. Returns Vec of (damage, is_crit, probability).
fn calculate_damage_outcomes_for_target(
    _state: &BattleState,
    attacker: &PokemonState,
    target: &PokemonState,
    _user_slot: FieldSlot,
    _target_slot: FieldSlot,
    move_data: &MoveData,
    config: DamageConfig,
    targets_multiplier: f64,
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
    let critical_states = critical_hit_probability(attacker, target, &move_data.name, config.consider_crit);

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

fn damage_effectiveness_for_action(state: &BattleState, action: &MoveAction, move_data: &MoveData) -> f64 {
    let Some(target_slot) = action.target_slot else {
        return 1.0;
    };

    let Some(target) = get_pokemon_at_slot(state, target_slot) else {
        return 1.0;
    };

    move_type_effectiveness(&move_data.pokemon_type, target)
}

fn apply_damage(mon: &mut PokemonState, damage: u16) {
    mon.hp = mon.hp.saturating_sub(damage);
    mon.fainted = mon.hp == 0;
}

fn possible_damage_outcomes_for_move(
    state: &BattleState,
    action: &MoveAction,
    move_data: &MoveData,
    config: DamageConfig,
) -> Vec<(MatchState, f64)> {
    let next_state = state.clone();

    let Some(attacker) = get_pokemon_at_slot(&next_state, action.user_slot).cloned() else {
        return vec![(MatchState::BattleState(next_state), 1.0)];
    };

    let pp_slot = attacker
        .moves
        .iter()
        .position(|move_entry| move_entry.as_ref() == Some(&action.move_name));

    let Some(pp_index) = pp_slot else {
        return vec![(MatchState::BattleState(next_state), 1.0)];
    };

    let current_pp = match action.user_slot.player {
        Player::P1 => next_state.p1_active_mons.get(action.user_slot.slot_index as usize).map(|mon| mon.move_pp[pp_index]).unwrap_or(0),
        Player::P2 => next_state.p2_active_mons.get(action.user_slot.slot_index as usize).map(|mon| mon.move_pp[pp_index]).unwrap_or(0),
    };

    if current_pp == 0 {
        println!("{}", "Struggle is unimplemented.".bright_red());
        return vec![(MatchState::BattleState(next_state), 1.0)];
    }

    // Resolve target list based on move's targeting type
    let target_slots = if move_target_is_multitarget(&move_data.target) {
        resolve_move_targets(&next_state, action.user_slot, &move_data.target)
    } else {
        // Single-target move: use action.target_slot if available
        match action.target_slot {
            Some(slot) => vec![slot],
            None => {
                // Fallback: use resolve_move_targets
                let targets = resolve_move_targets(&next_state, action.user_slot, &move_data.target);
                if targets.is_empty() {
                    return vec![(MatchState::BattleState(next_state), 1.0)];
                }
                targets
            }
        }
    };

    if target_slots.is_empty() {
        return vec![(MatchState::BattleState(next_state), 1.0)];
    }

    // Calculate targets multiplier (0.75x for 2+ targets, 1.0x for 1 target)
    let targets_mult = damage_targets_multiplier(target_slots.len());

    // Calculate damage outcomes for each target independently
    let mut per_target_outcomes: Vec<Vec<(u16, bool, f64)>> = Vec::new();

    for target_slot in &target_slots {
        let Some(target) = get_pokemon_at_slot(&next_state, *target_slot).cloned() else {
            // Target is fainted or doesn't exist, skip
            continue;
        };

        let outcomes = calculate_damage_outcomes_for_target(
            &next_state,
            &attacker,
            &target,
            action.user_slot,
            *target_slot,
            move_data,
            config,
            targets_mult,
        );
        per_target_outcomes.push(outcomes);
    }

    // If no valid targets remain, return no damage
    if per_target_outcomes.is_empty() {
        return vec![(MatchState::BattleState(next_state), 1.0)];
    }

    // Log move info if verbosity >= 4
    if get_verbosity() >= 4 {
        let target_names: Vec<String> = target_slots.iter().filter_map(|slot| {
            get_pokemon_at_slot(&next_state, *slot).map(|m| species_name_sim(&m.species))
        }).collect();
        println!(
            "{}",
            format!(
                "{} uses {} | targets: {} | move type: {} | PP: {}",
                species_name_sim(&attacker.species),
                move_name_sim(&action.move_name),
                target_names.join(", "),
                pokemon_type_name(&move_data.pokemon_type),
                current_pp,
            )
            .bright_cyan()
        );
    }

    // Combine per-target outcomes via cartesian product
    let mut all_outcomes: Vec<(MatchState, f64)> = vec![(MatchState::BattleState(next_state.clone()), 1.0)];

    for (target_idx, target_outcomes) in per_target_outcomes.iter().enumerate() {
        let target_slot = target_slots[target_idx];
        let mut new_all_outcomes = Vec::new();

        for (existing_state, existing_prob) in all_outcomes {
            for (damage, _is_crit, outcome_prob) in target_outcomes {
                let mut branch_state = match existing_state.clone() {
                    MatchState::BattleState(bs) => bs,
                    _ => continue,
                };

                // Apply damage to this target
                if let Some(target_mon) = match target_slot.player {
                    Player::P1 => branch_state.p1_active_mons.get_mut(target_slot.slot_index as usize),
                    Player::P2 => branch_state.p2_active_mons.get_mut(target_slot.slot_index as usize),
                } {
                    apply_damage(target_mon, *damage);
                }

                let combined_prob = existing_prob * outcome_prob;
                new_all_outcomes.push((MatchState::BattleState(branch_state), combined_prob));
            }
        }

        all_outcomes = new_all_outcomes;
    }

    // Decrement PP once at the end
    for (state, _) in &mut all_outcomes {
        if let MatchState::BattleState(bs) = state {
            if let Some(mon) = match action.user_slot.player {
                Player::P1 => bs.p1_active_mons.get_mut(action.user_slot.slot_index as usize),
                Player::P2 => bs.p2_active_mons.get_mut(action.user_slot.slot_index as usize),
            } {
                if let Some(pp) = mon.move_pp.get_mut(pp_index) {
                    *pp = pp.saturating_sub(1);
                }
            }
        }
    }

    // Log all outcomes at verbosity 4
    if get_verbosity() >= 4 {
        println!("{}", format!("  [Verbosity 4] {} total damage outcome combinations:", all_outcomes.len()).bright_yellow());
        for (idx, (_, prob)) in all_outcomes.iter().enumerate() {
            println!("    Branch {}: {:.6} probability", idx + 1, prob);
        }
    }

    all_outcomes
}

pub fn team_preview_state_from_teamsheets(
    p1_path: &str,
    p2_path: &str,
    pokemon_dex: &HashMap<Species, PokemonData>,
    move_dex: &HashMap<PokemonMove, MoveData>,
    active_per_side: u8,
    brought_per_side: u8,
) -> TeamPreviewState {
    TeamPreviewState {
        active_per_side,
        brought_per_side,
        p1_mons: parse_team_sheet(p1_path, pokemon_dex, move_dex),
        p2_mons: parse_team_sheet(p2_path, pokemon_dex, move_dex),
    }
}

/// Helper function to generate all combinations of an array.
fn get_combinations(n: usize, k: usize) -> Vec<Vec<usize>> {
    let mut result = Vec::new();
    let mut current = Vec::new();
    combine_helper(0, n, k, &mut current, &mut result);
    result
}

fn combine_helper(start: usize, n: usize, k: usize, current: &mut Vec<usize>, result: &mut Vec<Vec<usize>>) {
    if current.len() == k {
        result.push(current.clone());
        return;
    }
    for i in start..n {
        current.push(i);
        combine_helper(i + 1, n, k, current, result);
        current.pop();
    }
}

fn battle_state_from_preview(
    preview: &TeamPreviewState,
    p1_preview: &TeamPreviewCommand,
    p2_preview: &TeamPreviewCommand,
) -> BattleState {
    let p1_active_mons: Vec<PokemonState> = p1_preview.active_indices.iter().map(|&i| preview.p1_mons[i].clone()).collect();
    let p1_back_mons: Vec<PokemonState> = p1_preview.back_indices.iter().map(|&i| preview.p1_mons[i].clone()).collect();

    let p2_active_mons: Vec<PokemonState> = p2_preview.active_indices.iter().map(|&i| preview.p2_mons[i].clone()).collect();
    let p2_back_mons: Vec<PokemonState> = p2_preview.back_indices.iter().map(|&i| preview.p2_mons[i].clone()).collect();

    BattleState {
        active_per_side: preview.active_per_side,
        p1_active_mons,
        p2_active_mons,
        p1_back_mons,
        p2_back_mons,
        action_queue: vec![],
        turn_number: 1,
        turn_started: false,
        turn_ended: false,
        p1_has_tera: true,
        p2_has_tera: true,
        p1_has_mega: true,
        p2_has_mega: true,
    }
}

/// Generates all possible team preview commands
fn team_preview_commands(state: &TeamPreviewState, player: Player) -> Vec<PlayerCommand> {
    let mons_len = match player {
        Player::P1 => state.p1_mons.len(),
        Player::P2 => state.p2_mons.len(),
    };
    
    let brought_len = (state.brought_per_side as usize).min(mons_len);
    let active_len = (state.active_per_side as usize).min(brought_len);
    
    let mut commands = Vec::new();
    if mons_len == 0 { return commands; }
    
    let brought_combos = get_combinations(mons_len, brought_len);
    for brought in brought_combos {
        let active_combos_indices = get_combinations(brought_len, active_len);
        for act_idx in active_combos_indices {
            let mut active = Vec::new();
            let mut back = Vec::new();
            for i in 0..brought_len {
                if act_idx.contains(&i) {
                    active.push(brought[i]);
                } else {
                    back.push(brought[i]);
                }
            }
            commands.push(PlayerCommand::TeamPreview(TeamPreviewCommand {
                active_indices: active,
                back_indices: back,
            }));
        }
    }
    
    commands
}

fn get_valid_targets(target_type: &MoveTarget, player: Player, state: &BattleState, slot_idx: usize) -> Vec<Option<FieldSlot>> {
    let mut targets: Vec<Option<FieldSlot>> = Vec::new();
    let (my_active, foe_active) = match player {
        Player::P1 => (&state.p1_active_mons, &state.p2_active_mons),
        Player::P2 => (&state.p2_active_mons, &state.p1_active_mons),
    };
    
    let foe_player = match player {
        Player::P1 => Player::P2,
        Player::P2 => Player::P1,
    };

    match target_type {
        MoveTarget::AdjacentAlly | MoveTarget::AdjacentAllyOrSelf | MoveTarget::AdjacentFoe | MoveTarget::Normal | MoveTarget::Any | MoveTarget::Scripted => {
            let can_target_foe = match target_type {
                MoveTarget::AdjacentFoe | MoveTarget::Normal | MoveTarget::Any | MoveTarget::Scripted => true,
                _ => false,
            };
            
            let can_target_ally = match target_type {
                MoveTarget::AdjacentAlly | MoveTarget::AdjacentAllyOrSelf | MoveTarget::Normal | MoveTarget::Any => true,
                _ => false,
            };
            
            let can_target_self = match target_type {
                MoveTarget::AdjacentAllyOrSelf => true,
                _ => false,
            };

            if can_target_foe {
                for (i, foe) in foe_active.iter().enumerate() {
                    if !foe.fainted {
                        targets.push(Some(FieldSlot { player: foe_player, slot_index: i as u8 }));
                    }
                }
            }
            if can_target_ally {
                for (i, ally) in my_active.iter().enumerate() {
                    if !ally.fainted {
                        if i == slot_idx && !can_target_self {
                            continue;
                        }
                        targets.push(Some(FieldSlot { player, slot_index: i as u8 }));
                    }
                }
            }
            
            if targets.is_empty() {
                targets.push(Some(FieldSlot { player: foe_player, slot_index: 0 })); // Fallback
            }
        },
        _ => {
            targets.push(None); // Multi-target and self-target moves don't select a target
        }
    }
    
    targets
}

fn generate_commands_for_active(
    player: Player,
    slot_idx: usize,
    state: &BattleState,
    move_dex: &HashMap<PokemonMove, MoveData>,
    pokemon_dex: &HashMap<Species, PokemonData>
) -> Vec<BattleCommand> {
    let (my_active, my_back, _has_tera) = match player {
        Player::P1 => (&state.p1_active_mons, &state.p1_back_mons, state.p1_has_tera),
        Player::P2 => (&state.p2_active_mons, &state.p2_back_mons, state.p2_has_tera),
    };
    
    let mut cmds = Vec::new();
    
    if slot_idx >= my_active.len() {
        return cmds;
    }
    
    let mon = &my_active[slot_idx];
    
    // Switches
    for (i, back_mon) in my_back.iter().enumerate() {
        if !back_mon.fainted {
            cmds.push(BattleCommand::Switch(SwitchCommand { party_index: i }));
        }
    }

    if mon.fainted {
        return cmds; // If fainted, can only switch
    }
    
    let can_tera = !_has_tera && !mon.is_tera;
    
    let mut can_mega = mon.has_mega_form;
    if can_mega {
        if let Some(mega_sp) = &mon.mega_species {
            if let Some(mega_data) = pokemon_dex.get(mega_sp) {
                if let Some(req_item) = &mega_data.required_item {
                    let held_item_str = format!("{:?}", mon.item).to_lowercase();
                    if held_item_str != *req_item {
                        can_mega = false;
                    }
                }
            }
        }
    }

    // Attacks (Moves)
    for (i, move_name_opt) in mon.moves.iter().enumerate() {
        let move_name = match move_name_opt { Some(m) => m, None => continue };
        
            
        
        let target_type = if let Some(m_data) = move_dex.get(move_name) {
            &m_data.target
        } else {
            &MoveTarget::Normal // Default
        };
        
        let valid_targets = get_valid_targets(target_type, player, state, slot_idx);
        
        for target in valid_targets {
            cmds.push(BattleCommand::Attack(AttackCommand {
                move_slot: i,
                target: target.clone(),
                terastallize: false,
                mega_evolve: false,
            }));
            
            if can_tera {
                cmds.push(BattleCommand::Attack(AttackCommand {
                    move_slot: i,
                    target: target.clone(),
                    terastallize: true,
                    mega_evolve: false,
                }));
            }
            if can_mega {
                cmds.push(BattleCommand::Attack(AttackCommand {
                    move_slot: i,
                    target: target.clone(),
                    terastallize: false,
                    mega_evolve: true,
                }));
            }
            if can_tera && can_mega {
                cmds.push(BattleCommand::Attack(AttackCommand {
                    move_slot: i,
                    target: target.clone(),
                    terastallize: true,
                    mega_evolve: true,
                }));
            }
        }
    }
    
    cmds
}

fn queue_battle_commands_for_player(
    state: &BattleState,
    player: Player,
    commands: &[BattleCommand],
    move_dex: &HashMap<PokemonMove, MoveData>,
    action_queue: &mut Vec<Action>,
) {
    let active_mons = match player {
        Player::P1 => &state.p1_active_mons,
        Player::P2 => &state.p2_active_mons,
    };

    for (slot_idx, command) in commands.iter().enumerate() {
        let user_slot = FieldSlot { player, slot_index: slot_idx as u8 };

        match command {
            BattleCommand::Switch(s) => {
                action_queue.push(Action::SwitchAction(SwitchAction {
                    user_slot,
                    switch_index: s.party_index,
                }));
            }
            BattleCommand::Attack(a) => {
                let Some(active_mon) = active_mons.get(slot_idx) else {
                    continue;
                };

                let Some(move_name) = active_mon.moves.get(a.move_slot).cloned().flatten() else {
                    continue;
                };

                let priority = move_dex.get(&move_name).map(|move_data| move_data.priority).unwrap_or(0);

                if a.terastallize {
                    action_queue.push(Action::TeraAction(TeraAction {
                        user_slot,
                    }));
                }

                if a.mega_evolve {
                    action_queue.push(Action::MegaAction(MegaAction {
                        user_slot,
                    }));
                }

                action_queue.push(Action::MoveAction(MoveAction {
                    move_name,
                    priority,
                    user_slot,
                    target_slot: a.target,
                }));
            }
            BattleCommand::Pass => {}
            BattleCommand::Pass => {}
        }
    }
}

fn cartesian_product_commands(cmd_lists: &[Vec<BattleCommand>]) -> Vec<Vec<BattleCommand>> {
    if cmd_lists.is_empty() {
        return vec![vec![]];
    }
    let first = &cmd_lists[0];
    let rest = cartesian_product_commands(&cmd_lists[1..]);
    
    let mut result = Vec::new();
    if first.is_empty() && cmd_lists.len() == 1 {
        // Edge case
        return vec![];
    }
    
    // If a slot has no commands (fainted with no backup), omit it for that slot?
    // Usually battle expects commands mapped 1:1, if empty maybe skip.
    if first.is_empty() {
        return rest;
    }

    for cmd in first {
        if rest.is_empty() {
            result.push(vec![cmd.clone()]);
        } else {
            for rem in &rest {
                let mut comb = vec![cmd.clone()];
                comb.extend(rem.iter().cloned());
                result.push(comb);
            }
        }
    }
    result
}

fn is_valid_command_combination(cmds: &[BattleCommand]) -> bool {
    let mut switch_targets = Vec::new();
    let mut tera_count = 0;
    let mut mega_count = 0;

    for cmd in cmds {
        match cmd {
            BattleCommand::Switch(s) => {
                if switch_targets.contains(&s.party_index) {
                    return false; // Can't switch two active Pokemon to the same benched Pokemon
                }
                switch_targets.push(s.party_index);
            }
            BattleCommand::Attack(a) => {
                if a.terastallize {
                    tera_count += 1;
                }
                if a.mega_evolve {
                    mega_count += 1;
                }
            }
            _ => {}
        }
    }

    if tera_count > 1 || mega_count > 1 {
        return false;
    }

    true
}

fn battle_commands(state: &BattleState, player: Player, move_dex: &HashMap<PokemonMove, MoveData>, pokemon_dex: &HashMap<Species, PokemonData>) -> Vec<PlayerCommand> {
    let active_len = match player {
        Player::P1 => state.p1_active_mons.len(),
        Player::P2 => state.p2_active_mons.len(),
    };
    
    let mut slot_cmds = Vec::new();
    for i in 0..active_len {
        let cmds = generate_commands_for_active(player, i, state, move_dex, pokemon_dex);
        slot_cmds.push(cmds);
    }
    
    let combinations = cartesian_product_commands(&slot_cmds);
    combinations.into_iter()
        .filter(|combo| is_valid_command_combination(combo))
        .map(PlayerCommand::Battle)
        .collect()
}

pub fn get_possible_commands_for_active_slot(
    state: &BattleState,
    player: Player,
    slot_idx: usize,
    move_dex: &HashMap<PokemonMove, MoveData>,
    pokemon_dex: &HashMap<Species, PokemonData>,
) -> Vec<BattleCommand> {
    generate_commands_for_active(player, slot_idx, state, move_dex, pokemon_dex)
}

fn get_pokemon_at_slot<'a>(state: &'a BattleState, slot: FieldSlot) -> Option<&'a PokemonState> {
    let mons = match slot.player {
        Player::P1 => &state.p1_active_mons,
        Player::P2 => &state.p2_active_mons,
    };
    mons.get(slot.slot_index as usize)
}

fn get_effective_speed(mon: &PokemonState) -> f32 {
    // Speed stat is at index 5 in the stats array
    // Speed boost is at index 4 in the boosts array
    let base_speed = mon.stats[5] as f32;
    let speed_boost = mon.boosts[4];
    
    // Apply boost multiplier
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
    
    // Compare with a small epsilon for floating point comparison
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

fn compare_action_order(action1: &Action, action2: &Action, state: &BattleState, move_dex: &HashMap<PokemonMove, MoveData>) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    
    let type_priority1 = get_action_type_priority(action1);
    let type_priority2 = get_action_type_priority(action2);
    
    // Different action types: order by type priority
    if type_priority1 != type_priority2 {
        return type_priority1.cmp(&type_priority2);
    }
    
    // Same type: compare by move priority and speed for move actions
    match (action1, action2) {
        (Action::MoveAction(m1), Action::MoveAction(m2)) => {
            // First compare move priority (higher priority goes first)
            if m1.priority != m2.priority {
                return m2.priority.cmp(&m1.priority);
            }
            
            // Then compare speed stats (higher speed goes first)
            let user1 = get_pokemon_at_slot(state, m1.user_slot);
            let user2 = get_pokemon_at_slot(state, m2.user_slot);
            
            match (user1, user2) {
                (Some(p1), Some(p2)) => {
                    compare_pokemon_speed(p1, p2)
                }
                _ => Ordering::Equal,
            }
        }
        _ => Ordering::Equal,
    }
}

fn step_action_queue(
    state: &BattleState,
    move_dex: &HashMap<PokemonMove, MoveData>,
    pokemon_dex: &HashMap<Species, PokemonData>,
    config: DamageConfig,
) -> Vec<(MatchState, f64)> {
    let mut next_state = state.clone();
    
    if next_state.action_queue.is_empty() {
        let mut replacement_needed = false;

        for mon in &next_state.p1_active_mons {
            if mon.fainted {
                if next_state.p1_back_mons.iter().any(|m| !m.fainted) {
                    replacement_needed = true;
                    break;
                }
            }
        }

        if !replacement_needed {
            for mon in &next_state.p2_active_mons {
                if mon.fainted {
                    if next_state.p2_back_mons.iter().any(|m| !m.fainted) {
                        replacement_needed = true;
                        break;
                    }
                }
            }
        }

        if replacement_needed {
            next_state.turn_started = true;
            next_state.turn_ended = true;
        } else {
            next_state.turn_started = false;
            next_state.turn_ended = false;
        }
        return vec![(MatchState::BattleState(next_state), 1.0)];
    }
    
    // Find the next action to execute (lowest index in type priority order)
    let mut next_action_idx = 0;
    for i in 1..next_state.action_queue.len() {
        if compare_action_order(&next_state.action_queue[next_action_idx], &next_state.action_queue[i], state, move_dex) == std::cmp::Ordering::Greater {
            next_action_idx = i;
        }
    }
    
    let action = next_state.action_queue.remove(next_action_idx);
    
    if get_verbosity() >= 4 {
        // Print a more user-friendly description including Pokémon names
        match &action {
            Action::MoveAction(m) => {
                let attacker = get_pokemon_at_slot(&next_state, m.user_slot)
                    .map(|p| species_name_sim(&p.species))
                    .unwrap_or_else(|| format!("{} slot {}", match m.user_slot.player { Player::P1 => "P1", Player::P2 => "P2" }, m.user_slot.slot_index + 1));
                let target = match m.target_slot {
                    Some(slot) => get_pokemon_at_slot(&next_state, slot)
                        .map(|p| species_name_sim(&p.species))
                        .unwrap_or_else(|| format!("{} slot {}", match slot.player { Player::P1 => "P1", Player::P2 => "P2" }, slot.slot_index + 1)),
                    None => "(no specific target)".to_string(),
                };
                println!("{}", format!("Processing Move: {} uses {} -> {}", attacker, move_name_sim(&m.move_name), target).cyan());
            }
            Action::SwitchAction(s) => {
                let user = get_pokemon_at_slot(&next_state, s.user_slot)
                    .map(|p| species_name_sim(&p.species))
                    .unwrap_or_else(|| format!("{} slot {}", match s.user_slot.player { Player::P1 => "P1", Player::P2 => "P2" }, s.user_slot.slot_index + 1));
                println!("{}", format!("Processing Switch: {} (slot {} )", user, s.switch_index + 1).blue());
            }
            Action::MegaAction(m) => {
                let mon_name = get_pokemon_at_slot(&next_state, m.user_slot)
                    .map(|p| species_name_sim(&p.species))
                    .unwrap_or_else(|| format!("{} slot {}", match m.user_slot.player { Player::P1 => "P1", Player::P2 => "P2" }, m.user_slot.slot_index + 1));
                println!("{}", format!("Processing Mega Evolution: {}", mon_name).yellow());
            }
            Action::TeraAction(t) => {
                let mon_name = get_pokemon_at_slot(&next_state, t.user_slot)
                    .map(|p| species_name_sim(&p.species))
                    .unwrap_or_else(|| format!("{} slot {}", match t.user_slot.player { Player::P1 => "P1", Player::P2 => "P2" }, t.user_slot.slot_index + 1));
                println!("{}", format!("Processing Terastallize: {}", mon_name).bright_magenta());
            }
            Action::Pass => {
                // Pass action: do nothing, no log needed
            }
        }
    }
    
    match action {
        Action::MoveAction(m) => {
            // Check if the attacker is fainted - if so, skip this move
            let attacker = get_pokemon_at_slot(&next_state, m.user_slot);
            if let Some(mon) = attacker {
                if mon.fainted {
                    return vec![(MatchState::BattleState(next_state), 1.0)];
                }
            }

            let Some(move_data) = move_dex.get(&m.move_name) else {
                return vec![(MatchState::BattleState(next_state), 1.0)];
            };

            possible_damage_outcomes_for_move(&next_state, &m, move_data, config)
        }
        Action::SwitchAction(s) => {
            // perform the switch now
            perform_switch_out_in(&mut next_state, s.user_slot, s.switch_index);
            if get_verbosity() >= 2 {
                let user = get_pokemon_at_slot(&next_state, s.user_slot)
                    .map(|p| species_name_sim(&p.species))
                    .unwrap_or_else(|| format!("{} slot {}", match s.user_slot.player { Player::P1 => "P1", Player::P2 => "P2" }, s.user_slot.slot_index + 1));
                println!("{}", format!("Executed Switch: new active at slot {} is {}", s.user_slot.slot_index + 1, user).bright_green());
            }
            vec![(MatchState::BattleState(next_state), 1.0)]
        }
        Action::MegaAction(m) => {
            let slot_idx = m.user_slot.slot_index as usize;
            let mons = match m.user_slot.player {
                Player::P1 => &mut next_state.p1_active_mons,
                Player::P2 => &mut next_state.p2_active_mons,
            };
            
            if let Some(mon) = mons.get_mut(slot_idx) {
                crate::battle::try_mega_evolution(mon, pokemon_dex);
            }
            
            match m.user_slot.player {
                Player::P1 => next_state.p1_has_mega = false,
                Player::P2 => next_state.p2_has_mega = false,
            }
            
            vec![(MatchState::BattleState(next_state), 1.0)]
        }
        Action::TeraAction(t) => {
            let slot_idx = t.user_slot.slot_index as usize;
            let mons = match t.user_slot.player {
                Player::P1 => &mut next_state.p1_active_mons,
                Player::P2 => &mut next_state.p2_active_mons,
            };
            
            if let Some(mon) = mons.get_mut(slot_idx) {
                mon.is_tera = true;
            }
            
            match t.user_slot.player {
                Player::P1 => next_state.p1_has_tera = false,
                Player::P2 => next_state.p2_has_tera = false,
            }
            
            vec![(MatchState::BattleState(next_state), 1.0)]
        }
        Action::Pass => {
            // Pass action: do nothing
            vec![(MatchState::BattleState(next_state), 1.0)]
        }
    }
}

pub fn get_possible_commands(
    state: &MatchState,
    move_dex: &HashMap<PokemonMove, MoveData>,
    pokemon_dex: &HashMap<Species, PokemonData>
) -> (Vec<PlayerCommand>, Vec<PlayerCommand>) {
    match state {
        MatchState::TeamPreviewState(preview) => {
            (
                team_preview_commands(preview, Player::P1),
                team_preview_commands(preview, Player::P2)
            )
        }
        MatchState::BattleState(battle) => {
            // If both flags are set, we're in replacement phase: players may need to send replacements
            if battle.turn_started && battle.turn_ended {
                let mut p1_options: Vec<PlayerCommand> = Vec::new();
                let mut p2_options: Vec<PlayerCommand> = Vec::new();

                // Helper to build replacement PlayerCommands for a player
                let build_replacement_commands = |player: Player, battle: &BattleState| -> Vec<PlayerCommand> {
                    let (active, back) = match player {
                        Player::P1 => (&battle.p1_active_mons, &battle.p1_back_mons),
                        Player::P2 => (&battle.p2_active_mons, &battle.p2_back_mons),
                    };

                    // collect indices of fainted active slots and healthy bench indices
                    let fainted_slots: Vec<usize> = active.iter().enumerate().filter(|(_, m)| m.fainted).map(|(i, _)| i).collect();
                    let healthy_bench: Vec<usize> = back.iter().enumerate().filter(|(_, m)| !m.fainted).map(|(i, _)| i).collect();

                    let mut results: Vec<PlayerCommand> = Vec::new();
                    // Player can always pass
                    results.push(PlayerCommand::Pass);

                    if fainted_slots.is_empty() || healthy_bench.is_empty() {
                        return results;
                    }

                    // generate injective mappings from fainted_slots -> healthy_bench
                    fn assign_recursive(slots: &[usize], benches: &Vec<usize>, used: &mut Vec<bool>, idx: usize, current: &mut Vec<Option<usize>>, out: &mut Vec<Vec<Option<usize>>>) {
                        if idx == slots.len() {
                            out.push(current.clone());
                            return;
                        }
                        for (bi, &bench_idx) in benches.iter().enumerate() {
                            if used[bi] { continue; }
                            used[bi] = true;
                            current[idx] = Some(bench_idx);
                            assign_recursive(slots, benches, used, idx + 1, current, out);
                            current[idx] = None;
                            used[bi] = false;
                        }
                    }

                    let mut used = vec![false; healthy_bench.len()];
                    let mut current: Vec<Option<usize>> = vec![None; fainted_slots.len()];
                    let mut mappings: Vec<Vec<Option<usize>>> = Vec::new();
                    assign_recursive(&fainted_slots, &healthy_bench, &mut used, 0, &mut current, &mut mappings);

                    for mapping in mappings {
                        // build a BattleCommand vector per active slot
                        let active_len = active.len();
                        let mut cmds: Vec<BattleCommand> = Vec::new();
                        for i in 0..active_len {
                            if let Some(pos) = fainted_slots.iter().position(|&s| s == i) {
                                // this slot is fainted -> pick mapped bench index
                                if let Some(Some(bench_choice)) = mapping.get(pos) {
                                    // need to convert bench_choice (index in healthy_bench vec) to actual bench index
                                    let bench_idx = healthy_bench[*bench_choice];
                                    cmds.push(BattleCommand::Switch(SwitchCommand { party_index: bench_idx }));
                                } else {
                                    // shouldn't happen
                                    cmds.push(BattleCommand::Switch(SwitchCommand { party_index: 0 }));
                                }
                            } else {
                                // healthy slot: push a dummy switch that will be ignored by apply_player_commands
                                cmds.push(BattleCommand::Switch(SwitchCommand { party_index: 0 }));
                            }
                        }
                        results.push(PlayerCommand::Battle(cmds));
                    }

                    results
                };

                p1_options = build_replacement_commands(Player::P1, battle);
                p2_options = build_replacement_commands(Player::P2, battle);

                return (p1_options, p2_options);
            }

            (
                battle_commands(battle, Player::P1, move_dex, pokemon_dex),
                battle_commands(battle, Player::P2, move_dex, pokemon_dex)
            )
        }
        MatchState::GameOverState { .. } => {
            (vec![], vec![])
        }
    }
}

pub fn apply_player_commands(
    state: &MatchState,
    p1_cmd: &PlayerCommand,
    p2_cmd: &PlayerCommand,
    move_dex: &HashMap<PokemonMove, MoveData>,
) -> MatchState {
    match state {
        MatchState::TeamPreviewState(preview) => {
            let p1_preview = match p1_cmd {
                PlayerCommand::TeamPreview(c) => c,
                _ => panic!("Expected TeamPreview command for P1"),
            };
            let p2_preview = match p2_cmd {
                PlayerCommand::TeamPreview(c) => c,
                _ => panic!("Expected TeamPreview command for P2"),
            };
            MatchState::BattleState(battle_state_from_preview(preview, p1_preview, p2_preview))
        }
        MatchState::BattleState(battle) => {
            let mut next_state = battle.clone();

            // Beginning of turn: set turn_started
            if !battle.turn_started && !battle.turn_ended {
                next_state.turn_started = true;
                // queue normal battle commands
                if let PlayerCommand::Battle(p1_battle) = p1_cmd {
                    queue_battle_commands_for_player(battle, Player::P1, p1_battle, move_dex, &mut next_state.action_queue);
                }
                if let PlayerCommand::Battle(p2_battle) = p2_cmd {
                    queue_battle_commands_for_player(battle, Player::P2, p2_battle, move_dex, &mut next_state.action_queue);
                }
                return MatchState::BattleState(next_state);
            }

            // Replacement phase: both flags are true -> players may send replacements
            if battle.turn_started && battle.turn_ended {
                // process p1
                if let PlayerCommand::Battle(cmds) = p1_cmd {
                    for (slot_idx, cmd) in cmds.iter().enumerate() {
                        match cmd {
                            BattleCommand::Switch(s) => {
                                let user_slot = FieldSlot { player: Player::P1, slot_index: slot_idx as u8 };
                                perform_switch_out_in(&mut next_state, user_slot, s.party_index);
                            }
                            _ => {}
                        }
                    }
                }
                // process p2
                if let PlayerCommand::Battle(cmds) = p2_cmd {
                    for (slot_idx, cmd) in cmds.iter().enumerate() {
                        match cmd {
                            BattleCommand::Switch(s) => {
                                let user_slot = FieldSlot { player: Player::P2, slot_index: slot_idx as u8 };
                                perform_switch_out_in(&mut next_state, user_slot, s.party_index);
                            }
                            _ => {}
                        }
                    }
                }

                // After replacements, reset turn flags (new turn will begin)
                next_state.turn_started = false;
                next_state.turn_ended = false;
                return MatchState::BattleState(next_state);
            }

            // Default: if turn_started true and turn_ended false, we're mid-turn and just queue commands
            if !battle.turn_started && battle.turn_ended {
                // shouldn't happen normally; treat as beginning
                next_state.turn_started = true;
            }

            // Mid-turn command queuing
            if let PlayerCommand::Battle(p1_battle) = p1_cmd {
                queue_battle_commands_for_player(battle, Player::P1, p1_battle, move_dex, &mut next_state.action_queue);
            }
            if let PlayerCommand::Battle(p2_battle) = p2_cmd {
                queue_battle_commands_for_player(battle, Player::P2, p2_battle, move_dex, &mut next_state.action_queue);
            }

            MatchState::BattleState(next_state)
        }
        MatchState::GameOverState { .. } => state.clone(),
    }
}

pub fn simulate_turn(
    state: &MatchState,
    p1_cmd: &PlayerCommand,
    p2_cmd: &PlayerCommand,
    move_dex: &HashMap<PokemonMove, MoveData>,
    pokemon_dex: &HashMap<Species, PokemonData>,
    consider_crit: bool,
    damage_rolls: u8,
) -> Vec<(MatchState, f64)> {
    // First, apply the player commands to populate the action queue
    let mut current_state = apply_player_commands(state, p1_cmd, p2_cmd, move_dex);

    // If the resulting BattleState has an empty action queue, set turn flags
    // appropriately: if any active slot is fainted and a healthy bench exists
    // then a replacement phase is needed -> mark turn_ended = true. Otherwise
    // start the turn with turn_started = true and turn_ended = false.
    match &mut current_state {
        MatchState::BattleState(bs) => {
            if bs.action_queue.is_empty() {
                let mut replacement_needed = false;

                for mon in &bs.p1_active_mons {
                    if mon.fainted {
                        if bs.p1_back_mons.iter().any(|m| !m.fainted) {
                            replacement_needed = true;
                            break;
                        }
                    }
                }

                if !replacement_needed {
                    for mon in &bs.p2_active_mons {
                        if mon.fainted {
                            if bs.p2_back_mons.iter().any(|m| !m.fainted) {
                                replacement_needed = true;
                                break;
                            }
                        }
                    }
                }

                if replacement_needed {
                    bs.turn_started = true;
                    bs.turn_ended = true;
                } else {
                    bs.turn_started = false;
                    bs.turn_ended = false;
                }
            }
        }
        _ => {}
    }
    let config = DamageConfig {
        consider_crit,
        damage_rolls,
    };

    fn expand_branch(
        state: &MatchState,
        move_dex: &HashMap<PokemonMove, MoveData>,
        pokemon_dex: &HashMap<Species, PokemonData>,
        config: DamageConfig,
    ) -> Vec<(MatchState, f64)> {
        match state {
            MatchState::BattleState(battle) => {
                if battle.action_queue.is_empty() {
                    return step_action_queue(battle, move_dex, pokemon_dex, config);//Handles replacement phase
                }

                let outcomes = step_action_queue(battle, move_dex, pokemon_dex, config);
                let mut aggregated = Vec::new();

                for (next_state, probability) in outcomes {
                    for (final_state, final_probability) in expand_branch(&next_state, move_dex, pokemon_dex, config) {
                        aggregated.push((final_state, probability * final_probability));
                    }
                }

                aggregated
            }
            _ => vec![(state.clone(), 1.0)],
        }
    }
    
    expand_branch(&current_state, move_dex, pokemon_dex, config)
}

/// Public validator wrapper used by interactive UI to check legality
pub fn validate_battle_command_combination(cmds: &[BattleCommand]) -> bool {
    is_valid_command_combination(cmds)
}

fn perform_switch_out_in(next_state: &mut BattleState, user_slot: FieldSlot, bench_index: usize) {
    // swap the active mon at user_slot.slot_index with the bench mon at bench_index
    let slot_idx = user_slot.slot_index as usize;
    match user_slot.player {
        Player::P1 => {
            if slot_idx >= next_state.p1_active_mons.len() || bench_index >= next_state.p1_back_mons.len() {
                return;
            }
            // clear volatiles on the switching-out mon
            let mut leaving = next_state.p1_active_mons[slot_idx].clone();
            leaving.volatiles.clear();
            leaving.boosts.iter_mut().for_each(|boost| *boost = 0);
            std::mem::swap(&mut next_state.p1_active_mons[slot_idx], &mut next_state.p1_back_mons[bench_index]);
            // ensure the benched slot gets the leaving mon with cleared volatiles
            next_state.p1_back_mons[bench_index] = leaving;
            // active slot already now holds incoming
        }
        Player::P2 => {
            if slot_idx >= next_state.p2_active_mons.len() || bench_index >= next_state.p2_back_mons.len() {
                return;
            }
            let mut leaving = next_state.p2_active_mons[slot_idx].clone();
            leaving.volatiles.clear();
            leaving.boosts.iter_mut().for_each(|boost| *boost = 0);
            std::mem::swap(&mut next_state.p2_active_mons[slot_idx], &mut next_state.p2_back_mons[bench_index]);
            next_state.p2_back_mons[bench_index] = leaving;
        }
    }
}