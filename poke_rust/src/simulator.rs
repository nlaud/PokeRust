#![allow(dead_code)]

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
use crate::dex_data::{MoveCategory, Status, VolatileStatus};
use crate::data::ability::Ability;
use crate::data::species::Species;
use crate::data::pokemon_move::PokemonMove;
use crate::dex_data::PokemonType;
use crate::simulator_helpers;

#[derive(Clone, Copy)]
pub struct DamageConfig {
    pub consider_crit: bool,
    pub damage_rolls: u8,
}

fn get_verbosity() -> u8 {
    simulator_helpers::get_verbosity()
}

fn species_name_sim(species: &crate::data::species::Species) -> String {
    simulator_helpers::species_name_sim(species)
}

fn move_name_sim(mov: &crate::data::pokemon_move::PokemonMove) -> String {
    simulator_helpers::move_name_sim(mov)
}

fn pokemon_type_name(pokemon_type: &PokemonType) -> &'static str {
    simulator_helpers::pokemon_type_name(pokemon_type)
}

fn move_target_is_multitarget(target: &MoveTarget) -> bool {
    simulator_helpers::move_target_is_multitarget(target)
}

/// Handles invulnerability status on the target Pokemon.
/// Returns (multiplier, should_continue) where:
/// - multiplier: damage multiplier (1.0 normal, 2.0 double damage, 0.0 blocked)
/// - should_continue: if false, move is blocked and we should return early with 0 damage
fn check_invulnerability_status(
    attacker: &PokemonState,
    target: &PokemonState,
    move_name: &PokemonMove,
) -> (f64, bool) {
    let invulnerability_resolution = simulator_helpers::invulnerability_resolution(attacker, target, move_name);
    match invulnerability_resolution {
        simulator_helpers::InvulnerabilityResolution::Blocked => (0.0, false),
        simulator_helpers::InvulnerabilityResolution::ZeroDamage => (0.0, true),
        simulator_helpers::InvulnerabilityResolution::Normal => (1.0, true),
        simulator_helpers::InvulnerabilityResolution::DoubleDamage => (2.0, true),
    }
}

fn coalesce_match_state_branches(branches: Vec<(MatchState, f64)>) -> Vec<(MatchState, f64)> {
    simulator_helpers::coalesce_branches(branches)
}

fn decrement_move_pp(next_state: &mut BattleState, user_slot: FieldSlot, move_name: &PokemonMove) {
    let move_index = match user_slot.player {
        Player::P1 => next_state
            .p1_active_mons
            .get(user_slot.slot_index as usize)
            .and_then(|mon| mon.moves.iter().position(|move_entry| move_entry.as_ref() == Some(move_name))),
        Player::P2 => next_state
            .p2_active_mons
            .get(user_slot.slot_index as usize)
            .and_then(|mon| mon.moves.iter().position(|move_entry| move_entry.as_ref() == Some(move_name))),
    };

    if let Some(move_index) = move_index {
        if let Some(mon) = match user_slot.player {
            Player::P1 => next_state.p1_active_mons.get_mut(user_slot.slot_index as usize),
            Player::P2 => next_state.p2_active_mons.get_mut(user_slot.slot_index as usize),
        } {
            if let Some(pp) = mon.move_pp.get_mut(move_index) {
                *pp = pp.saturating_sub(1);
            }
        }
    }
}

fn resolve_confusion_self_hit_outcomes(
    state: &BattleState,
    user_slot: FieldSlot,
    move_name: &PokemonMove,
    config: DamageConfig,
) -> Vec<(MatchState, f64)> {
    let Some(attacker) = get_pokemon_at_slot(state, user_slot).cloned() else {
        return vec![(MatchState::BattleState(state.clone()), 1.0)];
    };

    let damage_outcomes = simulator_helpers::confusion_self_hit_damage_outcomes(state, &attacker, config.damage_rolls);
    let mut outcomes = Vec::new();

    for (damage, probability) in damage_outcomes {
        let mut branch_state = state.clone();
        decrement_move_pp(&mut branch_state, user_slot, move_name);

        if let Some(game_over_state) = simulator_helpers::apply_damage_and_check_game_over(&mut branch_state, user_slot, damage) {
            outcomes.push((game_over_state, probability));
        } else {
            outcomes.push((MatchState::BattleState(branch_state), probability));
        }
    }

    outcomes
}

/// Handles semi-invulnerable and charging move mechanics.
/// Returns Some(outcomes) if the action is fully handled (charging/invulnerable mechanics),
/// None if normal damage calculation should proceed.
fn handle_charging_and_semi_invulnerability(
    state: &BattleState,
    attacker: &mut PokemonState,
    action: &MoveAction,
    move_data: &MoveData,
    next_state: &mut BattleState,
) -> Option<Vec<(MatchState, f64)>> {
    let mut move_has_charge = simulator_helpers::move_has_flag(move_data, &crate::dex_data::MoveFlag::Charge);
    if matches!(action.move_name, PokemonMove::SolarBeam | PokemonMove::SolarBlade)
        && simulator_helpers::weather_is_sunlight(state)
    {
        move_has_charge = false;
    }
    if action.move_name == PokemonMove::ElectroShot && simulator_helpers::weather_is_rain(state) {
        move_has_charge = false;
    }

    let move_causes_invulnerability = simulator_helpers::move_causes_invulnerability(&action.move_name);

    // Check for existing charging/invulnerable volatiles
    let charging_data = attacker.volatiles.iter().find_map(|v| {
        if let crate::pokemon::VolatileStatusState::Charging(mov, targets) = v {
            if mov == &action.move_name {
                Some((v.clone(), targets.clone()))
            } else {
                None
            }
        } else {
            None
        }
    });

    let invulnerable_data = attacker.volatiles.iter().find_map(|v| {
        if let crate::pokemon::VolatileStatusState::MoveStatus(VolatileStatus::SemiInvulnerable(mov), _) = v {
            if mov == &action.move_name {
                Some(())
            } else {
                None
            }
        } else {
            None
        }
    });

    // Electro Shot grants +1 SpA on first use turn (charging turn outside rain).
    if action.move_name == PokemonMove::ElectroShot && charging_data.is_none() {
        attacker.boosts[2] = (attacker.boosts[2] + 1).clamp(-6, 6);
        match action.user_slot.player {
            Player::P1 => {
                if let Some(mon) = next_state.p1_active_mons.get_mut(action.user_slot.slot_index as usize) {
                    mon.boosts = attacker.boosts;
                }
            }
            Player::P2 => {
                if let Some(mon) = next_state.p2_active_mons.get_mut(action.user_slot.slot_index as usize) {
                    mon.boosts = attacker.boosts;
                }
            }
        }
    }

    // Handle semi-invulnerable moves (first turn)
    if move_causes_invulnerability && invulnerable_data.is_none() {
        if action.move_name == PokemonMove::SkyDrop {
            let sky_drop_targets = if move_target_is_multitarget(&move_data.target) {
                simulator_helpers::resolve_move_targets(next_state, action.user_slot, &move_data.target)
            } else {
                match action.target_slot {
                    Some(slot) => vec![slot],
                    None => simulator_helpers::resolve_move_targets(next_state, action.user_slot, &move_data.target),
                }
            };

            let target_opt = sky_drop_targets
                .first()
                .and_then(|slot| get_pokemon_at_slot(&next_state, *slot));

            if let Some(target) = target_opt {
                if simulator_helpers::sky_drop_first_turn_fails(&next_state, target) {
                    return Some(vec![(MatchState::BattleState(next_state.clone()), 1.0)]);
                }
            }
        }

        let invulnerability_targets = if move_target_is_multitarget(&move_data.target) {
            simulator_helpers::resolve_move_targets(next_state, action.user_slot, &move_data.target)
        } else {
            match action.target_slot {
                Some(slot) => vec![slot],
                None => {
                    let targets = simulator_helpers::resolve_move_targets(next_state, action.user_slot, &move_data.target);
                    if targets.is_empty() {
                        return Some(vec![(MatchState::BattleState(next_state.clone()), 1.0)]);
                    }
                    targets
                }
            }
        };

        if invulnerability_targets.is_empty() {
            return Some(vec![(MatchState::BattleState(next_state.clone()), 1.0)]);
        }

        simulator_helpers::add_invulnerable_volatile(attacker, action.move_name.clone(), invulnerability_targets.clone());

        match action.user_slot.player {
            Player::P1 => {
                if let Some(mon) = next_state.p1_active_mons.get_mut(action.user_slot.slot_index as usize) {
                    mon.volatiles = attacker.volatiles.clone();
                }
            }
            Player::P2 => {
                if let Some(mon) = next_state.p2_active_mons.get_mut(action.user_slot.slot_index as usize) {
                    mon.volatiles = attacker.volatiles.clone();
                }
            }
        }

        if action.move_name == PokemonMove::SkyDrop {
            for target_slot in &invulnerability_targets {
                if let Some(target_mon) = match target_slot.player {
                    Player::P1 => next_state.p1_active_mons.get_mut(target_slot.slot_index as usize),
                    Player::P2 => next_state.p2_active_mons.get_mut(target_slot.slot_index as usize),
                } {
                    if !simulator_helpers::has_status_volatile(target_mon, &VolatileStatus::SkyDrop) {
                        target_mon.volatiles.push(crate::pokemon::VolatileStatusState::TurnStatus(VolatileStatus::SkyDrop, 2));
                    }
                }
            }
        }

        // Decrement PP for semi-invulnerable moves on first turn -- Don't AI Bruh
        /*let pp_slot = attacker
            .moves
            .iter()
            .position(|move_entry| move_entry.as_ref() == Some(&action.move_name));

        if let Some(pp_index) = pp_slot {
            if let Some(mon) = match action.user_slot.player {
                Player::P1 => next_state.p1_active_mons.get_mut(action.user_slot.slot_index as usize),
                Player::P2 => next_state.p2_active_mons.get_mut(action.user_slot.slot_index as usize),
            } {
                if let Some(pp) = mon.move_pp.get_mut(pp_index) {
                    *pp = pp.saturating_sub(1);
                }
            }
        }*/

        return Some(vec![(MatchState::BattleState(next_state.clone()), 1.0)]);
    }

    // Handle semi-invulnerable moves (second turn - resolve targets)
    if let Some(()) = invulnerable_data {
        simulator_helpers::remove_invulnerable_volatile(attacker, &action.move_name);

        match action.user_slot.player {
            Player::P1 => {
                if let Some(mon) = next_state.p1_active_mons.get_mut(action.user_slot.slot_index as usize) {
                    mon.volatiles = attacker.volatiles.clone();
                }
            }
            Player::P2 => {
                if let Some(mon) = next_state.p2_active_mons.get_mut(action.user_slot.slot_index as usize) {
                    mon.volatiles = attacker.volatiles.clone();
                }
            }
        }
        // Continue to normal damage calculation
    }

    // Handle charging moves (first turn)
    if move_has_charge && charging_data.is_none() && !move_causes_invulnerability{
        let charging_targets = if move_target_is_multitarget(&move_data.target) {
            simulator_helpers::resolve_move_targets(next_state, action.user_slot, &move_data.target)
        } else {
            match action.target_slot {
                Some(slot) => vec![slot],
                None => {
                    let targets = simulator_helpers::resolve_move_targets(next_state, action.user_slot, &move_data.target);
                    if targets.is_empty() {
                        return Some(vec![(MatchState::BattleState(next_state.clone()), 1.0)]);
                    }
                    targets
                }
            }
        };

        attacker.volatiles.push(crate::pokemon::VolatileStatusState::Charging(action.move_name.clone(), charging_targets));

        match action.user_slot.player {
            Player::P1 => {
                if let Some(mon) = next_state.p1_active_mons.get_mut(action.user_slot.slot_index as usize) {
                    mon.volatiles = attacker.volatiles.clone();
                }
            }
            Player::P2 => {
                if let Some(mon) = next_state.p2_active_mons.get_mut(action.user_slot.slot_index as usize) {
                    mon.volatiles = attacker.volatiles.clone();
                }
            }
        }

        // Decrement PP for charging moves on first turn
        let pp_slot = attacker
            .moves
            .iter()
            .position(|move_entry| move_entry.as_ref() == Some(&action.move_name));

        if let Some(pp_index) = pp_slot {
            if let Some(mon) = match action.user_slot.player {
                Player::P1 => next_state.p1_active_mons.get_mut(action.user_slot.slot_index as usize),
                Player::P2 => next_state.p2_active_mons.get_mut(action.user_slot.slot_index as usize),
            } {
                if let Some(pp) = mon.move_pp.get_mut(pp_index) {
                    *pp = pp.saturating_sub(1);
                }
            }
        }

        return Some(vec![(MatchState::BattleState(next_state.clone()), 1.0)]);
    }

    // Handle charging moves (second turn - fire the move)
    if let Some((volatile_state, stored_targets)) = charging_data {
        if let Some(target_slot) = action.target_slot {
            if !stored_targets.contains(&target_slot) {
                return Some(vec![(MatchState::BattleState(next_state.clone()), 1.0)]);
            }
        }

        // Remove the charging volatile
        if let Some(pos) = attacker.volatiles.iter().position(|v| std::mem::discriminant(v) == std::mem::discriminant(&volatile_state)) {
            attacker.volatiles.remove(pos);
        }

        match action.user_slot.player {
            Player::P1 => {
                if let Some(mon) = next_state.p1_active_mons.get_mut(action.user_slot.slot_index as usize) {
                    mon.volatiles = attacker.volatiles.clone();
                }
            }
            Player::P2 => {
                if let Some(mon) = next_state.p2_active_mons.get_mut(action.user_slot.slot_index as usize) {
                    mon.volatiles = attacker.volatiles.clone();
                }
            }
        }
    }

    None // Continue with normal damage calculation
}

fn multihit_hit_count_branches(
    state: &BattleState,
    attacker: &PokemonState,
    user_slot: FieldSlot,
    move_name: &PokemonMove,
    move_data: &MoveData,
) -> Vec<(u8, f64)> {
    if *move_name == PokemonMove::BeatUp {
        let team_members: Vec<&PokemonState> = match user_slot.player {
            Player::P1 => state.p1_active_mons.iter().chain(state.p1_back_mons.iter()).collect(),
            Player::P2 => state.p2_active_mons.iter().chain(state.p2_back_mons.iter()).collect(),
        };

        let eligible_count = team_members
            .iter()
            .filter(|mon| !mon.fainted && mon.status.is_none())
            .count()
            .max(1) as u8;
        return vec![(eligible_count, 1.0)];
    }

    let [min_hits, max_hits] = move_data.multihit_range;
    if min_hits == 0 && max_hits == 0 {
        return vec![(1, 1.0)];
    }

    if min_hits == max_hits {
        return vec![(min_hits.max(1), 1.0)];
    }

    let skill_link_active = !simulator_helpers::abilities_are_suppressed(state) && attacker.ability == Ability::SkillLink;
    if skill_link_active {
        return vec![(5, 1.0)];
    }

    if min_hits == 2 && max_hits == 5 {
        vec![(2, 7.0 / 20.0), (3, 7.0 / 20.0), (4, 3.0 / 20.0), (5, 3.0 / 20.0)]
    } else {
        vec![(max_hits.max(min_hits).max(1), 1.0)]
    }
}

fn multihit_hit_base_power(
    state: &BattleState,
    user_slot: FieldSlot,
    move_name: &PokemonMove,
    hit_index: u8,
    pokemon_dex: &HashMap<Species, PokemonData>,
) -> Option<u16> {
    match move_name {
        PokemonMove::TripleKick => Some(10 + (hit_index as u16 * 10)),
        PokemonMove::TripleAxel => Some(20 + (hit_index as u16 * 20)),
        PokemonMove::PopulationBomb => Some(20),
        PokemonMove::BeatUp => {
            let team_members: Vec<&PokemonState> = match user_slot.player {
                Player::P1 => state.p1_active_mons.iter().chain(state.p1_back_mons.iter()).collect(),
                Player::P2 => state.p2_active_mons.iter().chain(state.p2_back_mons.iter()).collect(),
            };

            let eligible_members: Vec<&PokemonState> = team_members
                .into_iter()
                .filter(|mon| !mon.fainted && mon.status.is_none())
                .collect();

            let hit_mon = eligible_members.get(hit_index as usize)?;
            let species_data = pokemon_dex.get(&hit_mon.species)?;
            Some((species_data.base_stats[1] / 10 + 5) as u16)
        }
        _ => None,
    }
}

fn apply_single_hit_branch(
    mut branch_state: BattleState,
    target_slot: FieldSlot,
    move_name: &PokemonMove,
    move_data: &MoveData,
    damage: u16,
    attack_slot: FieldSlot,
    branch_probability: f64,
) -> Vec<(BattleState, f64)> {
    let mut outcomes = Vec::new();
    let mut absorbed_by_dry_skin = false;
    let mut sand_spit_triggered = false;
    let mut seed_sower_triggered = false;
    let items_suppressed = simulator_helpers::items_are_suppressed(&branch_state);

    if let Some(target_mon) = match target_slot.player {
        Player::P1 => branch_state.p1_active_mons.get_mut(target_slot.slot_index as usize),
        Player::P2 => branch_state.p2_active_mons.get_mut(target_slot.slot_index as usize),
    } {
        if target_mon.ability == Ability::DrySkin && matches!(move_data.pokemon_type, PokemonType::Water) {
            let max_hp = target_mon.stats[0].max(1);
            let heal_amount = (max_hp as u32 / 4) as u16;
            target_mon.hp = target_mon.hp.saturating_add(heal_amount).min(max_hp);
            target_mon.fainted = false;
            absorbed_by_dry_skin = true;
        } else {
            simulator_helpers::apply_damage(target_mon, damage);

            if damage > 0 && !items_suppressed && matches!(target_mon.item, crate::data::item::Item::AirBalloon) {
                target_mon.item = crate::data::item::Item::None;
            }

            if target_mon.ability == Ability::SandSpit && !target_mon.fainted {
                sand_spit_triggered = true;
            }

            if target_mon.ability == Ability::SeedSower && !target_mon.fainted {
                seed_sower_triggered = true;
            }

            simulator_helpers::handle_unfreeze_on_damage(target_mon, move_name, &move_data.pokemon_type, damage);

            if *move_name == PokemonMove::Uproar {
                if let Some(crate::dex_data::Status::Sleep(_)) = target_mon.status {
                    target_mon.status = None;
                }
            }

            if *move_name == PokemonMove::SkyDrop {
                simulator_helpers::remove_status_volatile(target_mon, &VolatileStatus::SkyDrop);
            }

            if target_mon.fainted {
                simulator_helpers::clear_pokemon_on_faint(target_mon);
            }
        }
    }

    if sand_spit_triggered {
        simulator_helpers::set_weather(&mut branch_state, crate::dex_data::Weather::Sandstorm, 5);
    }

    if seed_sower_triggered {
        simulator_helpers::set_terrain(&mut branch_state, crate::dex_data::Terrain::GrassyTerrain, 5);
    }

    if matches!(move_name, PokemonMove::IceSpinner | PokemonMove::SteelRoller) {
        simulator_helpers::clear_terrain(&mut branch_state);
    }

    if absorbed_by_dry_skin {
        outcomes.push((branch_state, branch_probability));
    } else {
        let sec_branches = simulator_helpers::apply_secondary_effects(&branch_state, attack_slot, target_slot, move_data);
        for (bs, sec_prob) in sec_branches {
            outcomes.push((bs, branch_probability * sec_prob));
        }
    }

    outcomes
}

fn resolve_multihit_move_for_target(
    state: &BattleState,
    attacker: &PokemonState,
    target_slot: FieldSlot,
    move_data: &MoveData,
    move_name: &PokemonMove,
    config: DamageConfig,
    attack_slot: FieldSlot,
    targets_multiplier: f64,
    invulnerability_multiplier: f64,
    pokemon_dex: &HashMap<Species, PokemonData>,
) -> Vec<(BattleState, f64)> {
    let Some(_initial_target) = get_pokemon_at_slot(state, target_slot).cloned() else {
        return vec![];
    };

    let skill_link_active = !simulator_helpers::abilities_are_suppressed(state) && attacker.ability == Ability::SkillLink;
    let shared_rolls = simulator_helpers::shared_multihit_damage_rolls_enabled();
    let hit_count_branches = multihit_hit_count_branches(state, attacker, attack_slot, move_name, move_data);
    let mut final_outcomes: Vec<(BattleState, f64)> = Vec::new();

    for (hit_count, hit_probability) in hit_count_branches {
        let mut sequence_branches: Vec<(BattleState, f64, Option<u8>)> = vec![(state.clone(), hit_probability, None)];

        for hit_index in 0..hit_count {
            let mut next_sequence_branches: Vec<(BattleState, f64, Option<u8>)> = Vec::new();

            for (branch_state, branch_probability, shared_roll) in sequence_branches {
                let Some(current_target) = get_pokemon_at_slot(&branch_state, target_slot).cloned() else {
                    next_sequence_branches.push((branch_state, branch_probability, shared_roll));
                    continue;
                };

                if current_target.fainted {
                    next_sequence_branches.push((branch_state, branch_probability, shared_roll));
                    continue;
                }

                let needs_accuracy_check = if move_data.multihit_accuracy {
                    hit_index == 0 || !skill_link_active
                } else {
                    hit_index == 0
                };

                let hit_accuracy_probability = if needs_accuracy_check {
                    simulator_helpers::accuracy_hit_probability(
                        state,
                        attacker,
                        &current_target,
                        attack_slot,
                        target_slot,
                        move_data,
                    ).clamp(0.0, 1.0)
                } else {
                    1.0
                };

                if hit_accuracy_probability < 1.0 {
                    next_sequence_branches.push((branch_state.clone(), branch_probability * (1.0 - hit_accuracy_probability), shared_roll));
                }

                if hit_accuracy_probability <= 0.0 {
                    continue;
                }

                let base_power_override = multihit_hit_base_power(state, attack_slot, move_name, hit_index, pokemon_dex);
                let selected_rolls = if shared_rolls && shared_roll.is_none() {
                    simulator_helpers::selected_damage_rolls(config.damage_rolls)
                } else {
                    Vec::new()
                };

                if shared_rolls && shared_roll.is_none() {
                    let roll_probability = 1.0 / selected_rolls.len() as f64;
                    for roll in selected_rolls {
                        let hit_outcomes = simulator_helpers::calculate_damage_outcomes_for_target_with_options(
                            state,
                            attacker,
                            &current_target,
                            attack_slot,
                            target_slot,
                            move_data,
                            config,
                            targets_multiplier,
                            invulnerability_multiplier,
                            base_power_override,
                            Some(roll),
                        );

                        for (damage, _is_crit, damage_probability) in hit_outcomes {
                            for (next_state, next_probability) in apply_single_hit_branch(
                                branch_state.clone(),
                                target_slot,
                                move_name,
                                move_data,
                                damage,
                                attack_slot,
                                branch_probability * hit_accuracy_probability * damage_probability * roll_probability,
                            ) {
                                next_sequence_branches.push((next_state, next_probability, Some(roll)));
                            }
                        }
                    }
                } else {
                    let forced_roll = shared_roll;
                    let hit_outcomes = simulator_helpers::calculate_damage_outcomes_for_target_with_options(
                        state,
                        attacker,
                        &current_target,
                        attack_slot,
                        target_slot,
                        move_data,
                        config,
                        targets_multiplier,
                        invulnerability_multiplier,
                        base_power_override,
                        forced_roll,
                    );

                    for (damage, _is_crit, damage_probability) in hit_outcomes {
                        for (next_state, next_probability) in apply_single_hit_branch(
                            branch_state.clone(),
                            target_slot,
                            move_name,
                            move_data,
                            damage,
                            attack_slot,
                            branch_probability * hit_accuracy_probability * damage_probability,
                        ) {
                            next_sequence_branches.push((next_state, next_probability, shared_roll));
                        }
                    }
                }
            }

            sequence_branches = next_sequence_branches;
        }

        final_outcomes.extend(sequence_branches.into_iter().map(|(branch_state, branch_probability, _)| {
            (branch_state, branch_probability)
        }));
    }

    final_outcomes
}

fn possible_damage_outcomes_for_move(
    state: &BattleState,
    action: &MoveAction,
    move_data: &MoveData,
    config: DamageConfig,
    move_dex: &HashMap<PokemonMove, MoveData>,
    pokemon_dex: &HashMap<Species, PokemonData>,
) -> Vec<(MatchState, f64)> {
    let mut next_state = state.clone();

    let Some(mut attacker) = get_pokemon_at_slot(&next_state, action.user_slot).cloned() else {
        return vec![(MatchState::BattleState(next_state), 1.0)];
    };

    // Save pre-move state for potential failure branches (paralysis, sleep, freeze)
    let pre_move_state = next_state.clone();

    simulator_helpers::decrement_move_statuses(&mut attacker);
    match action.user_slot.player {
        Player::P1 => {
            if let Some(mon) = next_state.p1_active_mons.get_mut(action.user_slot.slot_index as usize) {
                mon.volatiles = attacker.volatiles.clone();
            }
        }
        Player::P2 => {
            if let Some(mon) = next_state.p2_active_mons.get_mut(action.user_slot.slot_index as usize) {
                mon.volatiles = attacker.volatiles.clone();
            }
        }
    }

    // Check Flinch
    if attacker.volatiles.iter().any(|v| matches!(v, crate::pokemon::VolatileStatusState::TurnStatus(VolatileStatus::Flinch, _))) {
        // Find and remove charging and semi-invulnerable volatiles
        if let Some(pos) = attacker.volatiles.iter().position(|v| matches!(v, crate::pokemon::VolatileStatusState::Charging(_, _) | crate::pokemon::VolatileStatusState::MoveStatus(VolatileStatus::SemiInvulnerable(_), _))) {
            attacker.volatiles.remove(pos);
            match action.user_slot.player {
                Player::P1 => {
                    if let Some(mon) = next_state.p1_active_mons.get_mut(action.user_slot.slot_index as usize) {
                        mon.volatiles = attacker.volatiles.clone();
                    }
                }
                Player::P2 => {
                    if let Some(mon) = next_state.p2_active_mons.get_mut(action.user_slot.slot_index as usize) {
                        mon.volatiles = attacker.volatiles.clone();
                    }
                }
            }
        }
        return vec![(MatchState::BattleState(next_state), 1.0)];
    }

    // Handle charging and semi-invulnerability mechanics
    if let Some(outcomes) = handle_charging_and_semi_invulnerability(&state, &mut attacker, action, move_data, &mut next_state) {
        return outcomes;
    }

    // Check if the move has the Recharge flag
    let move_has_recharge = simulator_helpers::move_has_flag(move_data, &crate::dex_data::MoveFlag::Recharge);

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

    let mut move_name = action.move_name.clone();
    let mut move_data = move_data;
    if move_name == PokemonMove::NaturePower {
        if let Some(replacement_move) = simulator_helpers::terrain_replacement_move(&next_state) {
            if let Some(replacement_data) = move_dex.get(&replacement_move) {
                move_name = replacement_move;
                move_data = replacement_data;
            }
        }
    }

    if move_name == PokemonMove::Splash {
        decrement_move_pp(&mut next_state, action.user_slot, &action.move_name);
        return vec![(MatchState::BattleState(next_state), 1.0)];
    }

    if attacker.volatiles.iter().any(|volatile| matches!(volatile, crate::pokemon::VolatileStatusState::TurnStatus(VolatileStatus::SkyDrop, _))) {
        let mut fail_state = pre_move_state.clone();
        if let Some(mon) = match action.user_slot.player {
            Player::P1 => fail_state.p1_active_mons.get_mut(action.user_slot.slot_index as usize),
            Player::P2 => fail_state.p2_active_mons.get_mut(action.user_slot.slot_index as usize),
        } {
            if let Some(pp) = mon.move_pp.get_mut(pp_index) {
                *pp = pp.saturating_sub(1);
            }
        }
        return vec![(MatchState::BattleState(fail_state), 1.0)];
    }

    // --- Status pre-move handling: Sleep, Frozen, Paralysis ---
    // Handle moves that thaw the user on use: thaw before attempt
    if let Some(Status::Frozen(_)) = attacker.status {
        if simulator_helpers::weather_is_sunlight(&next_state)
            || simulator_helpers::move_thaws_user_on_use(&action.move_name)
            || simulator_helpers::move_unfreezes_target(&action.move_name)
            || attacker.ability == Ability::MagmaArmor
        {
            // thaw user
            if let Some(mon) = match action.user_slot.player { Player::P1 => next_state.p1_active_mons.get_mut(action.user_slot.slot_index as usize), Player::P2 => next_state.p2_active_mons.get_mut(action.user_slot.slot_index as usize) } {
                mon.status = None;
            }
            attacker.status = None;
        }
    }

    // Sleep/Frozen branching: determine chance to fail due to being frozen/asleep
    let mut status_fail_prob: f64 = 0.0;
    if let Some(status) = &attacker.status {
        match status {
            Status::Frozen(n) => {
                // If already handled (thawed), skip
                if *n >= 2 {
                    // guaranteed thaw
                    if let Some(mon) = match action.user_slot.player { Player::P1 => next_state.p1_active_mons.get_mut(action.user_slot.slot_index as usize), Player::P2 => next_state.p2_active_mons.get_mut(action.user_slot.slot_index as usize) } {
                        mon.status = None;
                    }
                    attacker.status = None;
                } else {
                    // 25% chance to thaw and execute
                    status_fail_prob = 0.75;
                    // increment counter in pre_move_state for failure branch
                    if let Some(_mon) = match action.user_slot.player { Player::P1 => pre_move_state.p1_active_mons.get(action.user_slot.slot_index as usize), Player::P2 => pre_move_state.p2_active_mons.get(action.user_slot.slot_index as usize) } {
                        // we'll adjust failure branch later
                    }
                    // For success branch, remove status in next_state
                    if let Some(mon) = match action.user_slot.player { Player::P1 => next_state.p1_active_mons.get_mut(action.user_slot.slot_index as usize), Player::P2 => next_state.p2_active_mons.get_mut(action.user_slot.slot_index as usize) } {
                        mon.status = None;
                    }
                    attacker.status = None;
                }
            }
            Status::Sleep(n) => {
                if *n >= 2 {
                    if let Some(mon) = match action.user_slot.player { Player::P1 => next_state.p1_active_mons.get_mut(action.user_slot.slot_index as usize), Player::P2 => next_state.p2_active_mons.get_mut(action.user_slot.slot_index as usize) } {
                        mon.status = None;
                    }
                    attacker.status = None;
                } else {
                    // If the move is usable while asleep (Snore), allow it to execute regardless of wake roll
                    if move_data.sleep_usable {
                        // do not set a fail probability; status remains unchanged
                    } else {
                        // First action after sleep always fails; second action has a 1/3 wake chance.
                        status_fail_prob = if *n == 0 { 1.0 } else { 2.0 / 3.0 };
                        if *n > 0 {
                            if let Some(mon) = match action.user_slot.player { Player::P1 => next_state.p1_active_mons.get_mut(action.user_slot.slot_index as usize), Player::P2 => next_state.p2_active_mons.get_mut(action.user_slot.slot_index as usize) } {
                                mon.status = None; // success branch
                            }
                            attacker.status = None;
                        }
                    }
                }
            }
            _ => {}
        }
    }

    if action.move_name == PokemonMove::SleepTalk && !matches!(attacker.status, Some(Status::Sleep(_))) {
        return vec![(MatchState::BattleState(next_state), 1.0)];
    }

    let mut confusion_self_hit_outcomes: Option<Vec<(MatchState, f64)>> = None;
    if let Some(confusion_turns) = simulator_helpers::confusion_turns_remaining(&attacker) {
        // `decrement_move_statuses` has already run for this attacker, so
        // `confusion_turns` is the post-decrement value. If it's >= 1,
        // confusion can still trigger a self-hit branch.
        if confusion_turns >= 1 {
            let mut confusion_state = next_state.clone();

            match action.user_slot.player {
                Player::P1 => {
                    if let Some(mon) = confusion_state.p1_active_mons.get_mut(action.user_slot.slot_index as usize) {
                        mon.volatiles = attacker.volatiles.clone();
                    }
                }
                Player::P2 => {
                    if let Some(mon) = confusion_state.p2_active_mons.get_mut(action.user_slot.slot_index as usize) {
                        mon.volatiles = attacker.volatiles.clone();
                    }
                }
            }

            confusion_self_hit_outcomes = Some(resolve_confusion_self_hit_outcomes(
                &confusion_state,
                action.user_slot,
                &action.move_name,
                config,
            ));

            next_state = confusion_state;
        } else {
            // If post-decrement confusion is 0, the mon has snapped out; copy volatiles back.
            match action.user_slot.player {
                Player::P1 => {
                    if let Some(mon) = next_state.p1_active_mons.get_mut(action.user_slot.slot_index as usize) {
                        mon.volatiles = attacker.volatiles.clone();
                    }
                }
                Player::P2 => {
                    if let Some(mon) = next_state.p2_active_mons.get_mut(action.user_slot.slot_index as usize) {
                        mon.volatiles = attacker.volatiles.clone();
                    }
                }
            }
        }
    }

    if move_name == PokemonMove::Splash {
        if let Some(confusion_outcomes) = &confusion_self_hit_outcomes {
            let mut combined_confused = confusion_outcomes
                .iter()
                .cloned()
                .map(|(state, probability)| (state, probability * 0.5))
                .collect::<Vec<_>>();

            let mut no_effect_state = next_state.clone();
            decrement_move_pp(&mut no_effect_state, action.user_slot, &action.move_name);
            combined_confused.push((MatchState::BattleState(no_effect_state), 0.5));
            return coalesce_match_state_branches(combined_confused);
        }

        decrement_move_pp(&mut next_state, action.user_slot, &action.move_name);
        return vec![(MatchState::BattleState(next_state), 1.0)];
    }

    // Handle Sleep Talk: picks a random known move while asleep and uses it instead
    if action.move_name == PokemonMove::SleepTalk {
        let original_sleep_status = attacker.status.clone();

        // Collect candidate moves (exclude SleepTalk itself)
        let mut candidates: Vec<PokemonMove> = Vec::new();
        for mov_opt in attacker.moves.iter() {
            if let Some(mv) = mov_opt {
                if *mv == PokemonMove::SleepTalk { continue; }
                // Skip charge moves or moves flagged NoSleepTalk if we can inspect them
                if let Some(md) = move_dex.get(mv) {
                    let is_charge = md.flags.iter().any(|f| std::mem::discriminant(f) == std::mem::discriminant(&crate::dex_data::MoveFlag::Charge));
                    let no_sleep_talk = md.flags.iter().any(|f| std::mem::discriminant(f) == std::mem::discriminant(&crate::dex_data::MoveFlag::NoSleepTalk));
                    if is_charge || no_sleep_talk { continue; }
                }
                candidates.push(mv.clone());
            }
        }

        if candidates.is_empty() {
            // Consume SleepTalk PP and do nothing
            let mut fail_state = next_state.clone();
            if let Some(mon) = match action.user_slot.player { Player::P1 => fail_state.p1_active_mons.get_mut(action.user_slot.slot_index as usize), Player::P2 => fail_state.p2_active_mons.get_mut(action.user_slot.slot_index as usize) } {
                if let Some(idx) = mon.moves.iter().position(|m| m.as_ref() == Some(&PokemonMove::SleepTalk)) {
                    mon.move_pp[idx] = mon.move_pp[idx].saturating_sub(1);
                }
            }
            return vec![(MatchState::BattleState(fail_state), 1.0)];
        }

        // Branch on each candidate move, selected uniformly
        let mut combined: Vec<(MatchState, f64)> = Vec::new();
        let choice_prob = 1.0 / candidates.len() as f64;
        for cand in &candidates {
            if let Some(cand_data) = move_dex.get(cand) {
                let mut new_action = action.clone();
                new_action.move_name = cand.clone();
                let mut sleep_talk_state = next_state.clone();
                if let Some(mon) = match action.user_slot.player {
                    Player::P1 => sleep_talk_state.p1_active_mons.get_mut(action.user_slot.slot_index as usize),
                    Player::P2 => sleep_talk_state.p2_active_mons.get_mut(action.user_slot.slot_index as usize),
                } {
                    mon.status = None;
                }
                // Recursively simulate chosen move
                let branches = possible_damage_outcomes_for_move(&sleep_talk_state, &new_action, cand_data, config, move_dex, pokemon_dex);
                for (mut bs, p) in branches {
                    // For each returned state, revert PP consumption of the chosen move and consume SleepTalk PP instead
                    if let MatchState::BattleState(ref mut bstate) = bs {
                        if let Some(mon) = match action.user_slot.player { Player::P1 => bstate.p1_active_mons.get_mut(action.user_slot.slot_index as usize), Player::P2 => bstate.p2_active_mons.get_mut(action.user_slot.slot_index as usize) } {
                            mon.status = original_sleep_status.clone();
                            // Find candidate move index and increment PP back
                            if let Some(cand_idx) = mon.moves.iter().position(|m| m.as_ref() == Some(cand)) {
                                mon.move_pp[cand_idx] = mon.move_pp[cand_idx].saturating_add(1);
                            }
                            // Decrement SleepTalk PP
                            if let Some(sleep_idx) = mon.moves.iter().position(|m| m.as_ref() == Some(&PokemonMove::SleepTalk)) {
                                mon.move_pp[sleep_idx] = mon.move_pp[sleep_idx].saturating_sub(1);
                            }
                        }
                    }
                    combined.push((bs, p * choice_prob));
                }
            }
        }

        if let Some(confusion_outcomes) = confusion_self_hit_outcomes {
            let mut combined_confused = confusion_outcomes
                .into_iter()
                .map(|(state, probability)| (state, probability * 0.5))
                .collect::<Vec<_>>();
            combined_confused.extend(combined.into_iter().map(|(state, probability)| (state, probability * 0.5)));
            return coalesce_match_state_branches(combined_confused);
        }

        return coalesce_match_state_branches(combined);
    }

    if move_name == PokemonMove::SteelRoller && simulator_helpers::current_terrain(&next_state).is_none() {
        if let Some(confusion_outcomes) = &confusion_self_hit_outcomes {
            let mut combined_confused = confusion_outcomes
                .iter()
                .cloned()
                .map(|(state, probability)| (state, probability * 0.5))
                .collect::<Vec<_>>();

            let mut no_effect_state = next_state.clone();
            decrement_move_pp(&mut no_effect_state, action.user_slot, &action.move_name);
            combined_confused.push((MatchState::BattleState(no_effect_state), 0.5));
            return coalesce_match_state_branches(combined_confused);
        }

        return vec![(MatchState::BattleState(next_state), 1.0)];
    }

    // Resolve target list based on move's targeting type
    let mut target_slots = if move_name == PokemonMove::ExpandingForce
        && simulator_helpers::pokemon_is_on_terrain(&next_state, &attacker, &crate::dex_data::Terrain::PsychicTerrain)
    {
        match action.user_slot.player {
            Player::P1 => next_state
                .p2_active_mons
                .iter()
                .enumerate()
                .filter(|(_, mon)| !mon.fainted)
                .map(|(idx, _)| FieldSlot { player: Player::P2, slot_index: idx as u8 })
                .collect(),
            Player::P2 => next_state
                .p1_active_mons
                .iter()
                .enumerate()
                .filter(|(_, mon)| !mon.fainted)
                .map(|(idx, _)| FieldSlot { player: Player::P1, slot_index: idx as u8 })
                .collect(),
        }
    } else if move_target_is_multitarget(&move_data.target) {
        simulator_helpers::resolve_move_targets(&next_state, action.user_slot, &move_data.target)
    } else {
        // Single-target move: use action.target_slot if available
        match action.target_slot {
            Some(slot) => vec![slot],
            None => {
                // Fallback: use resolve_move_targets
                let targets = simulator_helpers::resolve_move_targets(&next_state, action.user_slot, &move_data.target);
                if targets.is_empty() {
                    return vec![(MatchState::BattleState(next_state), 1.0)];
                }
                targets
            }
        }
    };

    // Apply Follow Me / Rage Powder redirection for single-target moves
    if !move_target_is_multitarget(&move_data.target)
        && !(move_name == PokemonMove::ExpandingForce && simulator_helpers::pokemon_is_on_terrain(&next_state, &attacker, &crate::dex_data::Terrain::PsychicTerrain))
    {
        target_slots = simulator_helpers::check_and_apply_redirection(&next_state, action.user_slot, target_slots);
    }

    if target_slots.is_empty() {
        if let Some(confusion_outcomes) = &confusion_self_hit_outcomes {
            let mut combined_confused = confusion_outcomes
                .iter()
                .cloned()
                .map(|(state, probability)| (state, probability * 0.5))
                .collect::<Vec<_>>();

            let mut no_effect_state = next_state.clone();
            decrement_move_pp(&mut no_effect_state, action.user_slot, &action.move_name);
            combined_confused.push((MatchState::BattleState(no_effect_state), 0.5));
            return coalesce_match_state_branches(combined_confused);
        }

        return vec![(MatchState::BattleState(next_state), 1.0)];
    }

    // Calculate targets multiplier (0.75x for 2+ targets, 1.0x for 1 target)
    let targets_mult = simulator_helpers::damage_targets_multiplier(target_slots.len());

    let is_multihit_move = move_name == PokemonMove::BeatUp
        || move_data.multihit_range != [0, 0]
        || move_data.multihit_accuracy;

    if is_multihit_move {
        if target_slots.is_empty() {
            if let Some(confusion_outcomes) = &confusion_self_hit_outcomes {
                let mut combined_confused = confusion_outcomes
                    .iter()
                    .cloned()
                    .map(|(state, probability)| (state, probability * 0.5))
                    .collect::<Vec<_>>();

                let mut no_effect_state = next_state.clone();
                decrement_move_pp(&mut no_effect_state, action.user_slot, &action.move_name);
                combined_confused.push((MatchState::BattleState(no_effect_state), 0.5));
                return coalesce_match_state_branches(combined_confused);
            }

            return vec![(MatchState::BattleState(next_state), 1.0)];
        }

        let target_slot = target_slots[0];
        let Some(target) = get_pokemon_at_slot(&next_state, target_slot).cloned() else {
            return vec![(MatchState::BattleState(next_state), 1.0)];
        };

        let (invulnerability_multiplier, should_continue) = check_invulnerability_status(&attacker, &target, &move_name);
        if !should_continue {
            decrement_move_pp(&mut next_state, action.user_slot, &action.move_name);
            return vec![(MatchState::BattleState(next_state), 1.0)];
        }

        let mut all_outcomes: Vec<(MatchState, f64)> = resolve_multihit_move_for_target(
            &next_state,
            &attacker,
            target_slot,
            move_data,
            &move_name,
            config,
            action.user_slot,
            targets_mult,
            invulnerability_multiplier,
            pokemon_dex,
        )
        .into_iter()
        .map(|(bs, prob)| (MatchState::BattleState(bs), prob))
        .collect();

        let opposing_player = match action.user_slot.player {
            Player::P1 => Player::P2,
            Player::P2 => Player::P1,
        };

        let mut post_processed_outcomes: Vec<(MatchState, f64)> = Vec::new();

        for (state, prob) in all_outcomes.drain(..) {
            let mut bs = match state {
                MatchState::BattleState(bs) => bs,
                other => {
                    post_processed_outcomes.push((other, prob));
                    continue;
                }
            };

            let (before_active, before_back) = match opposing_player {
                Player::P1 => (&next_state.p1_active_mons, &next_state.p1_back_mons),
                Player::P2 => (&next_state.p2_active_mons, &next_state.p2_back_mons),
            };
            let (after_active, after_back) = match opposing_player {
                Player::P1 => (&bs.p1_active_mons, &bs.p1_back_mons),
                Player::P2 => (&bs.p2_active_mons, &bs.p2_back_mons),
            };

            let dealt_active: u32 = before_active
                .iter()
                .zip(after_active.iter())
                .map(|(before, after)| before.hp.saturating_sub(after.hp) as u32)
                .sum();
            let dealt_back: u32 = before_back
                .iter()
                .zip(after_back.iter())
                .map(|(before, after)| before.hp.saturating_sub(after.hp) as u32)
                .sum();
            let total_damage_dealt = dealt_active + dealt_back;

            let mut forced_winner: Option<Player> = None;
            let opponent_wiped_from_move = !team_has_remaining_pokemon(&bs, opposing_player) && total_damage_dealt > 0;

            if let Some(attacker_mon) = match action.user_slot.player {
                Player::P1 => bs.p1_active_mons.get_mut(action.user_slot.slot_index as usize),
                Player::P2 => bs.p2_active_mons.get_mut(action.user_slot.slot_index as usize),
            } {
                let max_hp = attacker_mon.stats[0].max(1);

                if move_data.heal_fraction[0] > 0 && move_data.heal_fraction[1] > 0 {
                    let heal = ((max_hp as u32 * move_data.heal_fraction[0] as u32)
                        / move_data.heal_fraction[1] as u32) as u16;
                    if heal > 0 {
                        attacker_mon.hp = attacker_mon.hp.saturating_add(heal).min(max_hp);
                        attacker_mon.fainted = false;
                    }
                }

                if move_data.drain_fraction[0] > 0 && move_data.drain_fraction[1] > 0 {
                    let heal = ((total_damage_dealt * move_data.drain_fraction[0] as u32)
                        / move_data.drain_fraction[1] as u32) as u16;
                    if heal > 0 {
                        attacker_mon.hp = attacker_mon.hp.saturating_add(heal).min(max_hp);
                        attacker_mon.fainted = false;
                    }
                }

                let is_recoil_move =
                    (move_data.recoil_fraction[0] > 0 && move_data.recoil_fraction[1] > 0)
                        || move_data.struggle_recoil;
                if is_recoil_move
                    && attacker_mon.ability != Ability::RockHead
                    && attacker_mon.ability != Ability::MagicGuard
                {
                    let recoil = if move_data.recoil_fraction[0] > 0 && move_data.recoil_fraction[1] > 0 {
                        ((total_damage_dealt * move_data.recoil_fraction[0] as u32)
                            / move_data.recoil_fraction[1] as u32) as u16
                    } else if move_data.struggle_recoil {
                        (max_hp as u32 / 4) as u16
                    } else {
                        0
                    };

                    if recoil > 0 {
                        simulator_helpers::apply_damage(attacker_mon, recoil);
                        if attacker_mon.fainted {
                            simulator_helpers::clear_pokemon_on_faint(attacker_mon);
                            if opponent_wiped_from_move {
                                forced_winner = Some(action.user_slot.player);
                            }
                        }
                    }
                }
            }

            if let Some(winner) = forced_winner {
                post_processed_outcomes.push((MatchState::GameOverState { winner }, prob));
            } else if let Some(game_over_state) = game_over_state_if_battle_finished(&bs) {
                post_processed_outcomes.push((game_over_state, prob));
            } else {
                post_processed_outcomes.push((MatchState::BattleState(bs), prob));
            }
        }

        let mut all_outcomes = post_processed_outcomes;

        let move_has_recharge = simulator_helpers::move_has_flag(move_data, &crate::dex_data::MoveFlag::Recharge);
        if move_has_recharge {
            for (state, _) in &mut all_outcomes {
                if let MatchState::BattleState(bs) = state {
                    if let Some(mon) = match action.user_slot.player {
                        Player::P1 => bs.p1_active_mons.get_mut(action.user_slot.slot_index as usize),
                        Player::P2 => bs.p2_active_mons.get_mut(action.user_slot.slot_index as usize),
                    } {
                        mon.volatiles.push(crate::pokemon::VolatileStatusState::TurnStatus(VolatileStatus::MustRecharge, 2));
                    }
                }
            }
        }

        for (state, _) in &mut all_outcomes {
            if let MatchState::BattleState(bs) = state {
                decrement_move_pp(bs, action.user_slot, &action.move_name);
            }
        }

        let has_confusion = confusion_self_hit_outcomes.is_some();
        let mut final_outcomes: Vec<(MatchState, f64)> = Vec::new();
        if let Some(confusion_outcomes) = confusion_self_hit_outcomes {
            for (state, prob) in confusion_outcomes {
                final_outcomes.push((state.clone(), prob * 0.5));
            }
        }

        for (state, prob) in all_outcomes {
            final_outcomes.push((state, prob * if has_confusion { 0.5 } else { 1.0 }));
        }

        if final_outcomes.is_empty() {
            return vec![(MatchState::BattleState(next_state), 1.0)];
        }

        return coalesce_match_state_branches(final_outcomes);
    }

    // Determine paralysis failure probability
    let mut par_fail_prob: f64 = 0.0;
    if matches!(attacker.status, Some(Status::Paralysis)) && attacker.ability != Ability::Limber {
        par_fail_prob = 0.125;
    }

    // Calculate hit/miss and damage outcomes for each target independently.
    // For spread moves this creates independent miss branches per target.
    let mut per_target_outcomes: Vec<(FieldSlot, Vec<(u16, bool, bool, f64)>)> = Vec::new();

    for target_slot in &target_slots {
        let mut outcomes_for_target: Vec<(u16, bool, bool, f64)> = Vec::new();

        let Some(target) = get_pokemon_at_slot(&next_state, *target_slot).cloned() else {
            // Target doesn't exist, skip
            continue;
        };

        let (invulnerability_multiplier, should_continue) = check_invulnerability_status(&attacker, &target, &move_name);

        if move_data.priority > 0
            && simulator_helpers::pokemon_is_on_terrain(&next_state, &target, &crate::dex_data::Terrain::PsychicTerrain)
        {
            outcomes_for_target.push((0, false, false, 1.0));
            per_target_outcomes.push((*target_slot, outcomes_for_target));
            continue;
        }

        let target_is_semi_invulnerable = target.volatiles.iter().any(|volatile| {
            matches!(
                volatile,
                crate::pokemon::VolatileStatusState::MoveStatus(VolatileStatus::SemiInvulnerable(_), _)
                    | crate::pokemon::VolatileStatusState::TurnStatus(VolatileStatus::SkyDrop, _)
            )
        });

        if matches!(move_data.pokemon_type, PokemonType::Ground)
            && !simulator_helpers::pokemon_is_grounded(&next_state, &target)
            && !target_is_semi_invulnerable
            && !matches!(move_name, PokemonMove::ThousandArrows | PokemonMove::ThousandWaves)
        {
            outcomes_for_target.push((0, false, false, 1.0));
            per_target_outcomes.push((*target_slot, outcomes_for_target));
            continue;
        }
        
        if !should_continue {
            // Move is blocked by invulnerability
            decrement_move_pp(&mut next_state, action.user_slot, &action.move_name);
            outcomes_for_target.push((0, false, false, 1.0));
            per_target_outcomes.push((*target_slot, outcomes_for_target));
            continue;
        }

        let weather_blocks_move = matches!(move_data.category, MoveCategory::Physical | MoveCategory::Special)
            && ((simulator_helpers::weather_is_heavy_rain(&next_state) && matches!(move_data.pokemon_type, PokemonType::Fire))
                || (simulator_helpers::weather_is_harsh_sunlight(&next_state) && matches!(move_data.pokemon_type, PokemonType::Water)));

        if weather_blocks_move {
            if matches!(move_data.name, PokemonMove::Scald | PokemonMove::SteamEruption) {
                if let Some(target_mon) = match target_slot.player {
                    Player::P1 => next_state.p1_active_mons.get_mut(target_slot.slot_index as usize),
                    Player::P2 => next_state.p2_active_mons.get_mut(target_slot.slot_index as usize),
                } {
                    if matches!(target_mon.status, Some(Status::Frozen(_))) {
                        target_mon.status = None;
                    }
                }
            }
            outcomes_for_target.push((0, false, false, 1.0));
            per_target_outcomes.push((*target_slot, outcomes_for_target));
            continue;
        }

        let hit_probability = simulator_helpers::accuracy_hit_probability(
            &next_state,
            &attacker,
            &target,
            action.user_slot,
            *target_slot,
            move_data,
        )
        .clamp(0.0, 1.0);

        // Miss branch.
        if hit_probability < 1.0 {
            outcomes_for_target.push((0, false, false, 1.0 - hit_probability));
        }

        let outcomes = simulator_helpers::calculate_damage_outcomes_for_target(
            &next_state,
            &attacker,
            &target,
            action.user_slot,
            *target_slot,
            move_data,
            config,
            targets_mult,
            invulnerability_multiplier,
        );

        // Hit branches.
        if hit_probability > 0.0 {
            for (damage, is_crit, damage_probability) in outcomes {
                outcomes_for_target.push((damage, is_crit, true, damage_probability * hit_probability));
            }
        }

        per_target_outcomes.push((*target_slot, outcomes_for_target));
    }

    // If no valid targets remain, return no damage
    if per_target_outcomes.is_empty() {
        if let Some(confusion_outcomes) = &confusion_self_hit_outcomes {
            let mut combined_confused = confusion_outcomes
                .iter()
                .cloned()
                .map(|(state, probability)| (state, probability * 0.5))
                .collect::<Vec<_>>();

            let mut no_effect_state = next_state.clone();
            decrement_move_pp(&mut no_effect_state, action.user_slot, &action.move_name);
            combined_confused.push((MatchState::BattleState(no_effect_state), 0.5));
            return coalesce_match_state_branches(combined_confused);
        }

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
                move_name_sim(&move_name),
                target_names.join(", "),
                pokemon_type_name(&move_data.pokemon_type),
                current_pp,
            )
            .bright_cyan()
        );
    }

    // Combine per-target outcomes via cartesian product
    let mut all_outcomes: Vec<(MatchState, f64)> = vec![(MatchState::BattleState(next_state.clone()), 1.0)];

    for (target_slot, target_outcomes) in &per_target_outcomes {
        let mut new_all_outcomes = Vec::new();

        for (existing_state, existing_prob) in all_outcomes {
            for (damage, _is_crit, hit, outcome_prob) in target_outcomes {
                let mut branch_state = match existing_state.clone() {
                    MatchState::BattleState(bs) => bs,
                    _ => continue,
                };

                // Apply damage and then branch on secondary effects (if hit)
                if *hit {
                    let mut absorbed_by_dry_skin = false;
                    let mut sand_spit_triggered = false;
                    let mut seed_sower_triggered = false;
                    let combined_prob = existing_prob * outcome_prob;
                    let items_suppressed = simulator_helpers::items_are_suppressed(&branch_state);

                    if let Some(target_mon) = match target_slot.player {
                        Player::P1 => branch_state.p1_active_mons.get_mut(target_slot.slot_index as usize),
                        Player::P2 => branch_state.p2_active_mons.get_mut(target_slot.slot_index as usize),
                    } {
                        if target_mon.ability == Ability::DrySkin
                            && matches!(move_data.pokemon_type, PokemonType::Water)
                        {
                            let max_hp = target_mon.stats[0].max(1);
                            let heal_amount = (max_hp as u32 / 4) as u16;
                            target_mon.hp = target_mon.hp.saturating_add(heal_amount).min(max_hp);
                            target_mon.fainted = false;
                            absorbed_by_dry_skin = true;
                        } else {
                            let _ = target_mon;

                            let Some(target_mon) = (match target_slot.player {
                                Player::P1 => branch_state.p1_active_mons.get_mut(target_slot.slot_index as usize),
                                Player::P2 => branch_state.p2_active_mons.get_mut(target_slot.slot_index as usize),
                            }) else {
                                new_all_outcomes.push((MatchState::BattleState(branch_state), combined_prob));
                                continue;
                            };

                            simulator_helpers::apply_damage(target_mon, *damage);

                            if *damage > 0 && !items_suppressed && matches!(target_mon.item, crate::data::item::Item::AirBalloon) {
                                target_mon.item = crate::data::item::Item::None;
                            }

                            if target_mon.ability == Ability::SandSpit && !target_mon.fainted {
                                sand_spit_triggered = true;
                            }

                            if target_mon.ability == Ability::SeedSower && !target_mon.fainted {
                                seed_sower_triggered = true;
                            }

                            // If this damage should unfreeze the target, handle it
                            simulator_helpers::handle_unfreeze_on_damage(target_mon, &move_data.name, &move_data.pokemon_type, *damage);

                            // Uproar wakes sleeping Pokemon
                            if move_data.name == PokemonMove::Uproar {
                                if let Some(crate::dex_data::Status::Sleep(_)) = target_mon.status {
                                    target_mon.status = None;
                                }
                            }

                            if move_data.name == PokemonMove::SkyDrop {
                                simulator_helpers::remove_status_volatile(target_mon, &VolatileStatus::SkyDrop);
                            }

                            if target_mon.fainted {
                                simulator_helpers::clear_pokemon_on_faint(target_mon);
                            }

                        }
                    }

                    if sand_spit_triggered {
                        simulator_helpers::set_weather(&mut branch_state, crate::dex_data::Weather::Sandstorm, 5);
                    }

                    if seed_sower_triggered {
                        simulator_helpers::set_terrain(&mut branch_state, crate::dex_data::Terrain::GrassyTerrain, 5);
                    }

                    if matches!(move_name, PokemonMove::IceSpinner | PokemonMove::SteelRoller) {
                        simulator_helpers::clear_terrain(&mut branch_state);
                    }

                    if absorbed_by_dry_skin {
                        new_all_outcomes.push((MatchState::BattleState(branch_state), combined_prob));
                    } else {
                        // Apply secondary effects, which now returns branched states with probabilities
                        let sec_branches = simulator_helpers::apply_secondary_effects(&branch_state, action.user_slot, *target_slot, move_data);
                        for (bs, sec_prob) in sec_branches {
                            let combined_prob = existing_prob * outcome_prob * sec_prob;
                            new_all_outcomes.push((MatchState::BattleState(bs), combined_prob));
                        }
                    }
                } else {
                    if simulator_helpers::weather_is_harsh_sunlight(&branch_state)
                        && matches!(move_data.name, PokemonMove::Scald | PokemonMove::SteamEruption)
                    {
                        if let Some(target_mon) = match target_slot.player {
                            Player::P1 => branch_state.p1_active_mons.get_mut(target_slot.slot_index as usize),
                            Player::P2 => branch_state.p2_active_mons.get_mut(target_slot.slot_index as usize),
                        } {
                            if matches!(target_mon.status, Some(Status::Frozen(_))) {
                                target_mon.status = None;
                            }
                        }
                    }

                    let combined_prob = existing_prob * outcome_prob;
                    new_all_outcomes.push((MatchState::BattleState(branch_state), combined_prob));
                }
            }
        }

        all_outcomes = new_all_outcomes;
    }

    // Apply post-damage move effects that depend on total HP damage dealt.
    let mut post_processed_outcomes: Vec<(MatchState, f64)> = Vec::new();
    let opposing_player = match action.user_slot.player {
        Player::P1 => Player::P2,
        Player::P2 => Player::P1,
    };

    for (state, prob) in all_outcomes {
        let mut bs = match state {
            MatchState::BattleState(bs) => bs,
            other => {
                post_processed_outcomes.push((other, prob));
                continue;
            }
        };

        let (before_active, before_back) = match opposing_player {
            Player::P1 => (&next_state.p1_active_mons, &next_state.p1_back_mons),
            Player::P2 => (&next_state.p2_active_mons, &next_state.p2_back_mons),
        };
        let (after_active, after_back) = match opposing_player {
            Player::P1 => (&bs.p1_active_mons, &bs.p1_back_mons),
            Player::P2 => (&bs.p2_active_mons, &bs.p2_back_mons),
        };

        let dealt_active: u32 = before_active
            .iter()
            .zip(after_active.iter())
            .map(|(before, after)| before.hp.saturating_sub(after.hp) as u32)
            .sum();
        let dealt_back: u32 = before_back
            .iter()
            .zip(after_back.iter())
            .map(|(before, after)| before.hp.saturating_sub(after.hp) as u32)
            .sum();
        let total_damage_dealt = dealt_active + dealt_back;

        let mut forced_winner: Option<Player> = None;
        let opponent_wiped_from_move = !team_has_remaining_pokemon(&bs, opposing_player) && total_damage_dealt > 0;

        if let Some(attacker_mon) = match action.user_slot.player {
            Player::P1 => bs.p1_active_mons.get_mut(action.user_slot.slot_index as usize),
            Player::P2 => bs.p2_active_mons.get_mut(action.user_slot.slot_index as usize),
        } {
            let max_hp = attacker_mon.stats[0].max(1);

            if move_data.heal_fraction[0] > 0 && move_data.heal_fraction[1] > 0 {
                let heal = ((max_hp as u32 * move_data.heal_fraction[0] as u32)
                    / move_data.heal_fraction[1] as u32) as u16;
                if heal > 0 {
                    attacker_mon.hp = attacker_mon.hp.saturating_add(heal).min(max_hp);
                    attacker_mon.fainted = false;
                }
            }

            if move_data.drain_fraction[0] > 0 && move_data.drain_fraction[1] > 0 {
                let heal = ((total_damage_dealt * move_data.drain_fraction[0] as u32)
                    / move_data.drain_fraction[1] as u32) as u16;
                if heal > 0 {
                    attacker_mon.hp = attacker_mon.hp.saturating_add(heal).min(max_hp);
                    attacker_mon.fainted = false;
                }
            }

            let is_recoil_move =
                (move_data.recoil_fraction[0] > 0 && move_data.recoil_fraction[1] > 0)
                    || move_data.struggle_recoil;
            if is_recoil_move
                && attacker_mon.ability != Ability::RockHead
                && attacker_mon.ability != Ability::MagicGuard
            {
                let recoil = if move_data.recoil_fraction[0] > 0 && move_data.recoil_fraction[1] > 0 {
                    ((total_damage_dealt * move_data.recoil_fraction[0] as u32)
                        / move_data.recoil_fraction[1] as u32) as u16
                } else if move_data.struggle_recoil {
                    (max_hp as u32 / 4) as u16
                } else {
                    0
                };

                if recoil > 0 {
                    simulator_helpers::apply_damage(attacker_mon, recoil);
                    if attacker_mon.fainted {
                        simulator_helpers::clear_pokemon_on_faint(attacker_mon);
                        if opponent_wiped_from_move {
                            forced_winner = Some(action.user_slot.player);
                        }
                    }
                }
            }
        }

        if let Some(winner) = forced_winner {
            post_processed_outcomes.push((MatchState::GameOverState { winner }, prob));
        } else if let Some(game_over_state) = game_over_state_if_battle_finished(&bs) {
            post_processed_outcomes.push((game_over_state, prob));
        } else {
            post_processed_outcomes.push((MatchState::BattleState(bs), prob));
        }
    }
    let mut all_outcomes = post_processed_outcomes;

    // Apply recharge volatile if move has recharge flag
    if move_has_recharge {
        for (state, _) in &mut all_outcomes {
            if let MatchState::BattleState(bs) = state {
                if let Some(mon) = match action.user_slot.player {
                    Player::P1 => bs.p1_active_mons.get_mut(action.user_slot.slot_index as usize),
                    Player::P2 => bs.p2_active_mons.get_mut(action.user_slot.slot_index as usize),
                } {
                    // Apply mustrecharge volatile for 2 turns
                    mon.volatiles.push(crate::pokemon::VolatileStatusState::TurnStatus(VolatileStatus::MustRecharge, 2));
                }
            }
        }
    }

    // Decrement PP once at the end
    for (state, _) in &mut all_outcomes {
        if let MatchState::BattleState(bs) = state {
            decrement_move_pp(bs, action.user_slot, &action.move_name);
        }
    }

    // Handle failure branches for paralysis / sleep / freeze (these consume PP but do nothing)
    let mut final_outcomes: Vec<(MatchState, f64)> = Vec::new();
    // paralysis fail branch
    if par_fail_prob > 0.0 {
        let mut fail_state = pre_move_state.clone();
        if let Some(mon) = match action.user_slot.player { Player::P1 => fail_state.p1_active_mons.get_mut(action.user_slot.slot_index as usize), Player::P2 => fail_state.p2_active_mons.get_mut(action.user_slot.slot_index as usize) } {
            if let Some(pp) = mon.move_pp.get_mut(pp_index) {
                *pp = pp.saturating_sub(1);
            }
        }
        final_outcomes.push((MatchState::BattleState(fail_state), par_fail_prob));
    }

    // status (sleep/frozen) fail branch: increment counters and consume PP
    if status_fail_prob > 0.0 {
        let mut status_fail_state = pre_move_state.clone();
        if let Some(mon) = match action.user_slot.player { Player::P1 => status_fail_state.p1_active_mons.get_mut(action.user_slot.slot_index as usize), Player::P2 => status_fail_state.p2_active_mons.get_mut(action.user_slot.slot_index as usize) } {
            // Decrement PP
            if let Some(pp) = mon.move_pp.get_mut(pp_index) {
                *pp = pp.saturating_sub(1);
            }

            // Increment sleep/frozen counters on failure
            if let Some(st) = &mon.status {
                match st {
                    crate::dex_data::Status::Frozen(n) => {
                        let new_n = n.saturating_add(1);
                        mon.status = Some(crate::dex_data::Status::Frozen(new_n));
                    }
                    crate::dex_data::Status::Sleep(n) => {
                        let new_n = n.saturating_add(1);
                        mon.status = Some(crate::dex_data::Status::Sleep(new_n));
                    }
                    _ => {}
                }
            }
        }
        final_outcomes.push((MatchState::BattleState(status_fail_state), status_fail_prob));
    }

    // Scale normal outcomes by success probability (1 - combined_fail_prob)
    let combined_fail_prob = par_fail_prob + status_fail_prob;
    let mut success_scale = (1.0 - combined_fail_prob).max(0.0);
    if confusion_self_hit_outcomes.is_some() {
        success_scale *= 0.5;
    }

    if let Some(confusion_outcomes) = &confusion_self_hit_outcomes {
        for (state, prob) in confusion_outcomes {
            final_outcomes.push((state.clone(), prob * success_scale));
        }
    }

    for (state, prob) in all_outcomes {
        final_outcomes.push((state, prob * success_scale));
    }

    if final_outcomes.is_empty() {
        return vec![(MatchState::BattleState(next_state), 1.0)];
    }
    // Log all outcomes at verbosity 4
    if get_verbosity() >= 4 {
        println!("{}", format!("  [Verbosity 4] {} total damage outcome combinations:", final_outcomes.len()).bright_yellow());
        for (idx, (_, prob)) in final_outcomes.iter().enumerate() {
            println!("    Branch {}: {:.6} probability", idx + 1, prob);
        }
    }

    coalesce_match_state_branches(final_outcomes)
}

pub fn team_preview_state_from_teamsheets(
    p1_path: &str,
    p2_path: &str,
    pokemon_dex: &HashMap<Species, PokemonData>,
    move_dex: &HashMap<PokemonMove, MoveData>,
    active_per_side: u8,
    brought_per_side: u8,
    use_stat_points: bool,
) -> TeamPreviewState {
    TeamPreviewState {
        active_per_side,
        brought_per_side,
        p1_mons: parse_team_sheet(p1_path, pokemon_dex, move_dex, use_stat_points),
        p2_mons: parse_team_sheet(p2_path, pokemon_dex, move_dex, use_stat_points),
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

    let mut state = BattleState {
        active_per_side: preview.active_per_side,
        p1_active_mons,
        p2_active_mons,
        p1_back_mons,
        p2_back_mons,
        action_queue: vec![],
        turn_number: 0,
        turn_started: false,
        turn_ended: false,
        p1_has_tera: true,
        p2_has_tera: true,
        p1_has_mega: true,
        p2_has_mega: true,
        weather: None,
        weather_turns: None,
        pseudo_weathers: vec![],
        pseudo_weather_turns: vec![],
        terrain: None,
        terrain_turns: None,
        p1_side_conditions: vec![],
        p1_side_condition_turns: vec![],
        p2_side_conditions: vec![],
        p2_side_condition_turns: vec![],
        p1_slot_conditions: vec![Vec::new(); preview.active_per_side as usize],
        p2_slot_conditions: vec![Vec::new(); preview.active_per_side as usize],
    };

    for slot_idx in 0..state.p1_active_mons.len() {
        simulator_helpers::process_pokemon_send_out(
            &mut state,
            FieldSlot {
                player: Player::P1,
                slot_index: slot_idx as u8,
            },
        );
    }

    for slot_idx in 0..state.p2_active_mons.len() {
        simulator_helpers::process_pokemon_send_out(
            &mut state,
            FieldSlot {
                player: Player::P2,
                slot_index: slot_idx as u8,
            },
        );
    }

    state
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
    let (my_active, my_back, has_tera, has_mega) = match player {
        Player::P1 => (&state.p1_active_mons, &state.p1_back_mons, state.p1_has_tera, state.p1_has_mega),
        Player::P2 => (&state.p2_active_mons, &state.p2_back_mons, state.p2_has_tera, state.p2_has_mega),
    };
    
    let mut cmds = Vec::new();
    
    if slot_idx >= my_active.len() {
        return cmds;
    }
    
    let mon = &my_active[slot_idx];
    
    // Check for mustrecharge volatile - if present, can only Pass
    if mon.volatiles.iter().any(|v| matches!(v, crate::pokemon::VolatileStatusState::TurnStatus(VolatileStatus::MustRecharge, _))) {
        cmds.push(BattleCommand::Pass);
        return cmds;
    }
    
    // Check for SemiInvulnerable - if present, they are locked into their semi-invulnerable move
    if mon.volatiles.iter().any(|v| matches!(v, crate::pokemon::VolatileStatusState::MoveStatus(VolatileStatus::SemiInvulnerable(_), _))) {
        // Find the move that causes invulnerability
        for (i, move_name_opt) in mon.moves.iter().enumerate() {
            if let Some(m) = move_name_opt {
                if simulator_helpers::move_causes_invulnerability(m) {
                    // We just use a target of 0 as a placeholder since it was already resolved
                    cmds.push(BattleCommand::Attack(AttackCommand {
                        move_slot: i,
                        target: Some(FieldSlot { player: player, slot_index: 0 }),
                        terastallize: false,
                        mega_evolve: false,
                    }));
                    return cmds;
                }
            }
        }
        cmds.push(BattleCommand::Pass);
        return cmds;
    }
    
    // Check for charging volatile
    let locked_move = mon.volatiles.iter().find_map(|v| {
        match v {
            crate::pokemon::VolatileStatusState::Charging(mov, targets) => {
                Some((mov.clone(), targets.clone()))
            }
            _ => None,
        }
    });
    
    // If charging, can only Pass or use the charged move with same targets
    if let Some((charged_move, charged_targets)) = locked_move {
        for (i, move_name_opt) in mon.moves.iter().enumerate() {
            if let Some(m) = move_name_opt {
                if m == &charged_move {
                    // This is the charged move, allow it with the same targets
                    for target in &charged_targets {
                        cmds.push(BattleCommand::Attack(AttackCommand {
                            move_slot: i,
                            target: Some(*target),
                            terastallize: false,
                            mega_evolve: false,
                        }));
                    }
                    return cmds; // Only allow the charged move or pass
                }
            }
        }
        cmds.push(BattleCommand::Pass);
        return cmds; // If charging move not in moveset, only allow pass (Shouldn't be possible)
    }
    
    // Normal move selection (not charging)
    // Switches
    for (i, back_mon) in my_back.iter().enumerate() {
        if !back_mon.fainted {
            cmds.push(BattleCommand::Switch(SwitchCommand { party_index: i }));
        }
    }

    if mon.fainted {
        return cmds; // If fainted, can only switch
    }
    
    let can_tera = !has_tera && !mon.is_tera;
    
    let mut can_mega = mon.has_mega_form;
    if !has_mega {
        can_mega = false;
    }
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
        
        let valid_targets = if *move_name == PokemonMove::ExpandingForce
            && simulator_helpers::pokemon_is_on_terrain(state, mon, &crate::dex_data::Terrain::PsychicTerrain)
        {
            vec![None]
        } else {
            get_valid_targets(target_type, player, state, slot_idx)
        };
        
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

                let mut priority = move_dex.get(&move_name).map(|move_data| move_data.priority).unwrap_or(0);
                if move_name == PokemonMove::GrassyGlide
                    && simulator_helpers::pokemon_is_on_terrain(state, active_mon, &crate::dex_data::Terrain::GrassyTerrain)
                {
                    priority += 1;
                }

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

fn team_has_remaining_pokemon(state: &BattleState, player: Player) -> bool {
    match player {
        Player::P1 => state.p1_active_mons.iter().chain(state.p1_back_mons.iter()).any(|mon| !mon.fainted),
        Player::P2 => state.p2_active_mons.iter().chain(state.p2_back_mons.iter()).any(|mon| !mon.fainted),
    }
}

fn game_over_state_if_battle_finished(state: &BattleState) -> Option<MatchState> {
    let p1_has_remaining = team_has_remaining_pokemon(state, Player::P1);
    let p2_has_remaining = team_has_remaining_pokemon(state, Player::P2);

    match (p1_has_remaining, p2_has_remaining) {
        (false, true) => Some(MatchState::GameOverState { winner: Player::P2 }),
        (true, false) => Some(MatchState::GameOverState { winner: Player::P1 }),
        _ => None,
    }
}

fn get_effective_speed(state: &BattleState, mon: &PokemonState) -> f32 {
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
    
    let mut speed = base_speed * multiplier;

    if mon.ability == Ability::SurgeSurfer && matches!(simulator_helpers::current_terrain(state), Some(crate::dex_data::Terrain::ElectricTerrain)) {
        speed *= 2.0;
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
        .any(|condition| matches!(condition, crate::dex_data::SideCondition::TailWind))
}

fn effective_speed_for_slot(state: &BattleState, slot: FieldSlot, mon: &PokemonState) -> f32 {
    let mut speed = get_effective_speed(state, mon);

    if mon.ability == Ability::Chlorophyll && simulator_helpers::weather_is_sunlight(state) {
        speed *= 2.0;
    }
    if mon.ability == Ability::SwiftSwim && simulator_helpers::weather_is_rain(state) {
        speed *= 2.0;
    }
    if mon.ability == Ability::SandRush
        && matches!(simulator_helpers::current_weather(state), Some(crate::dex_data::Weather::Sandstorm))
    {
        speed *= 2.0;
    }
    if mon.ability == Ability::SlushRush
        && matches!(simulator_helpers::current_weather(state), Some(crate::dex_data::Weather::Snow))
    {
        speed *= 2.0;
    }

    if side_has_tailwind(state, slot.player) {
        speed *= 2.0;
    }
    speed
}

fn trick_room_is_active(state: &BattleState) -> bool {
    state
        .pseudo_weathers
        .iter()
        .any(|pseudo_weather| matches!(pseudo_weather, crate::dex_data::PseudoWeather::TrickRoom))
}

fn compare_pokemon_speed(state: &BattleState, p1: &PokemonState, p2: &PokemonState) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let speed1 = get_effective_speed(state, p1);
    let speed2 = get_effective_speed(state, p2);
    
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
    let _ = move_dex;
    
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
        (Action::MegaAction(ma1), Action::MegaAction(ma2)) => {
            // Compare by effective speed for mega actions
            let user1 = get_pokemon_at_slot(state, ma1.user_slot);
            let user2 = get_pokemon_at_slot(state, ma2.user_slot);
            match (user1, user2) {
                (Some(p1), Some(p2)) => {
                    let speed1 = effective_speed_for_slot(state, ma1.user_slot, p1);
                    let speed2 = effective_speed_for_slot(state, ma2.user_slot, p2);
                    let trick_room = trick_room_is_active(state);
                    if (speed2 - speed1).abs() < 0.01 {
                        Ordering::Equal
                    } else if trick_room {
                        if speed1 < speed2 { Ordering::Less } else { Ordering::Greater }
                    } else if speed2 > speed1 { Ordering::Greater } else { Ordering::Less }
                }
                _ => Ordering::Equal,
            }
        }
        (Action::TeraAction(t1), Action::TeraAction(t2)) => {
            // Compare by effective speed for terastallize actions
            let user1 = get_pokemon_at_slot(state, t1.user_slot);
            let user2 = get_pokemon_at_slot(state, t2.user_slot);
            match (user1, user2) {
                (Some(p1), Some(p2)) => {
                    let speed1 = effective_speed_for_slot(state, t1.user_slot, p1);
                    let speed2 = effective_speed_for_slot(state, t2.user_slot, p2);
                    let trick_room = trick_room_is_active(state);
                    if (speed2 - speed1).abs() < 0.01 {
                        Ordering::Equal
                    } else if trick_room {
                        if speed1 < speed2 { Ordering::Less } else { Ordering::Greater }
                    } else if speed2 > speed1 { Ordering::Greater } else { Ordering::Less }
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
            // End-of-turn processing
            simulator_helpers::end_turn(&mut next_state);
            if let Some(game_over_state) = game_over_state_if_battle_finished(&next_state) {
                return vec![(game_over_state, 1.0)];
            }
            next_state.turn_started = true;
            next_state.turn_ended = true;
        } else {
            // Still call end_turn wrapper to keep behavior consistent
            simulator_helpers::end_turn(&mut next_state);
            if let Some(game_over_state) = game_over_state_if_battle_finished(&next_state) {
                return vec![(game_over_state, 1.0)];
            }
            next_state.turn_started = false;
            next_state.turn_ended = false;
        }
        return vec![(MatchState::BattleState(next_state), 1.0)];
    }
    
    // Find the next action(s) to execute according to priority/speed; branch on exact ties
    let mut best_indices: Vec<usize> = vec![0];
    for i in 1..next_state.action_queue.len() {
        let cmp = compare_action_order(&next_state.action_queue[best_indices[0]], &next_state.action_queue[i], state, move_dex);
        if cmp == std::cmp::Ordering::Greater {
            best_indices = vec![i];
        } else if cmp == std::cmp::Ordering::Equal {
            best_indices.push(i);
        }
    }

    if best_indices.len() > 1 {
        // Branch equally among the tied actions
        let mut combined_results: Vec<(MatchState, f64)> = Vec::new();
        let branch_prob = 1.0 / best_indices.len() as f64;
        for &idx in &best_indices {
            let mut branch_state = next_state.clone();
            let action = branch_state.action_queue.remove(idx);

            if get_verbosity() >= 4 {
                match &action {
                    Action::MoveAction(m) => {
                        let attacker = get_pokemon_at_slot(&branch_state, m.user_slot)
                            .map(|p| species_name_sim(&p.species))
                            .unwrap_or_else(|| format!("{} slot {}", match m.user_slot.player { Player::P1 => "P1", Player::P2 => "P2" }, m.user_slot.slot_index + 1));
                        let target = match m.target_slot {
                            Some(slot) => get_pokemon_at_slot(&branch_state, slot)
                                .map(|p| species_name_sim(&p.species))
                                .unwrap_or_else(|| format!("{} slot {}", match slot.player { Player::P1 => "P1", Player::P2 => "P2" }, slot.slot_index + 1)),
                            None => "(no specific target)".to_string(),
                        };
                        println!("{}", format!("Processing Move: {} uses {} -> {}", attacker, move_name_sim(&m.move_name), target).cyan());
                    }
                    Action::SwitchAction(s) => {
                        let user = get_pokemon_at_slot(&branch_state, s.user_slot)
                            .map(|p| species_name_sim(&p.species))
                            .unwrap_or_else(|| format!("{} slot {}", match s.user_slot.player { Player::P1 => "P1", Player::P2 => "P2" }, s.user_slot.slot_index + 1));
                        println!("{}", format!("Processing Switch: {} (slot {} )", user, s.switch_index + 1).blue());
                    }
                    Action::MegaAction(m) => {
                        let mon_name = get_pokemon_at_slot(&branch_state, m.user_slot)
                            .map(|p| species_name_sim(&p.species))
                            .unwrap_or_else(|| format!("{} slot {}", match m.user_slot.player { Player::P1 => "P1", Player::P2 => "P2" }, m.user_slot.slot_index + 1));
                        println!("{}", format!("Processing Mega Evolution: {}", mon_name).yellow());
                    }
                    Action::TeraAction(t) => {
                        let mon_name = get_pokemon_at_slot(&branch_state, t.user_slot)
                            .map(|p| species_name_sim(&p.species))
                            .unwrap_or_else(|| format!("{} slot {}", match t.user_slot.player { Player::P1 => "P1", Player::P2 => "P2" }, t.user_slot.slot_index + 1));
                        println!("{}", format!("Processing Terastallize: {}", mon_name).bright_magenta());
                    }
                    Action::Pass => {}
                }
            }

            let branch_outcomes = match action {
                Action::MoveAction(m) => {
                    // Check if the attacker is fainted - if so, skip this move
                    let attacker = get_pokemon_at_slot(&branch_state, m.user_slot);
                    if let Some(mon) = attacker {
                        if mon.fainted {
                            vec![(MatchState::BattleState(branch_state.clone()), 1.0)]
                        } else {
                            match move_dex.get(&m.move_name) {
                                Some(move_data) => possible_damage_outcomes_for_move(&branch_state, &m, move_data, config, move_dex, pokemon_dex),
                                None => vec![(MatchState::BattleState(branch_state.clone()), 1.0)],
                            }
                        }
                    } else {
                        vec![(MatchState::BattleState(branch_state.clone()), 1.0)]
                    }
                }
                Action::SwitchAction(s) => {
                    perform_switch_out_in(&mut branch_state, s.user_slot, s.switch_index);
                    simulator_helpers::process_pokemon_send_out(&mut branch_state, s.user_slot);
                    if get_verbosity() >= 2 {
                        let user = get_pokemon_at_slot(&branch_state, s.user_slot)
                            .map(|p| species_name_sim(&p.species))
                            .unwrap_or_else(|| format!("{} slot {}", match s.user_slot.player { Player::P1 => "P1", Player::P2 => "P2" }, s.user_slot.slot_index + 1));
                        println!("{}", format!("Executed Switch: new active at slot {} is {}", s.user_slot.slot_index + 1, user).bright_green());
                    }
                    vec![(MatchState::BattleState(branch_state), 1.0)]
                }
                Action::MegaAction(m) => {
                    let slot_idx = m.user_slot.slot_index as usize;
                    let mons = match m.user_slot.player { Player::P1 => &mut branch_state.p1_active_mons, Player::P2 => &mut branch_state.p2_active_mons };
                    if let Some(mon) = mons.get_mut(slot_idx) { crate::battle::try_mega_evolution(mon, pokemon_dex); }
                    match m.user_slot.player { Player::P1 => branch_state.p1_has_mega = false, Player::P2 => branch_state.p2_has_mega = false }
                    vec![(MatchState::BattleState(branch_state), 1.0)]
                }
                Action::TeraAction(t) => {
                    let slot_idx = t.user_slot.slot_index as usize;
                    let mons = match t.user_slot.player { Player::P1 => &mut branch_state.p1_active_mons, Player::P2 => &mut branch_state.p2_active_mons };
                    if let Some(mon) = mons.get_mut(slot_idx) { mon.is_tera = true; }
                    match t.user_slot.player { Player::P1 => branch_state.p1_has_tera = false, Player::P2 => branch_state.p2_has_tera = false }
                    vec![(MatchState::BattleState(branch_state), 1.0)]
                }
                Action::Pass => vec![(MatchState::BattleState(branch_state), 1.0)],
            };

            for (st, p) in branch_outcomes {
                combined_results.push((st, p * branch_prob));
            }
        }

        return coalesce_match_state_branches(combined_results);
    }

    let action = next_state.action_queue.remove(best_indices[0]);
    
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

            possible_damage_outcomes_for_move(&next_state, &m, move_data, config, move_dex, pokemon_dex)
        }
        Action::SwitchAction(s) => {
            // perform the switch now
            perform_switch_out_in(&mut next_state, s.user_slot, s.switch_index);
            simulator_helpers::process_pokemon_send_out(&mut next_state, s.user_slot);
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

                return (
                    build_replacement_commands(Player::P1, battle),
                    build_replacement_commands(Player::P2, battle),
                );
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
            if let Some(game_over_state) = game_over_state_if_battle_finished(battle) {
                return game_over_state;
            }

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
                let mut queued_switches: Vec<(FieldSlot, usize)> = Vec::new();

                if let PlayerCommand::Battle(cmds) = p1_cmd {
                    for (slot_idx, cmd) in cmds.iter().enumerate() {
                        if let BattleCommand::Switch(s) = cmd {
                            queued_switches.push((
                                FieldSlot {
                                    player: Player::P1,
                                    slot_index: slot_idx as u8,
                                },
                                s.party_index,
                            ));
                        }
                    }
                }

                if let PlayerCommand::Battle(cmds) = p2_cmd {
                    for (slot_idx, cmd) in cmds.iter().enumerate() {
                        if let BattleCommand::Switch(s) = cmd {
                            queued_switches.push((
                                FieldSlot {
                                    player: Player::P2,
                                    slot_index: slot_idx as u8,
                                },
                                s.party_index,
                            ));
                        }
                    }
                }

                perform_simultaneous_switches(&mut next_state, &queued_switches);

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
    // First, apply the player commands to populate the action queue (may branch due to speed ties on send-outs)
    let initial_branches = apply_player_commands_branching(state, p1_cmd, p2_cmd, move_dex);

    // Adjust each initial branch's turn flags when action queue is empty
    let mut adjusted_initial_branches: Vec<(MatchState, f64)> = Vec::new();
    for (mut st, prob) in initial_branches {
        if let MatchState::BattleState(bs) = &mut st {
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
        adjusted_initial_branches.push((st, prob));
    }
    let initial_branches = coalesce_match_state_branches(adjusted_initial_branches);
    println!("[simulate_turn] initial_branches.len() = {}", initial_branches.len());
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
                    return coalesce_match_state_branches(step_action_queue(battle, move_dex, pokemon_dex, config));//Handles replacement phase
                }

                let outcomes = step_action_queue(battle, move_dex, pokemon_dex, config);
                let mut aggregated = Vec::new();

                for (next_state, probability) in outcomes {
                    for (final_state, final_probability) in expand_branch(&next_state, move_dex, pokemon_dex, config) {
                        aggregated.push((final_state, probability * final_probability));
                    }
                }

                coalesce_match_state_branches(aggregated)
            }
            _ => vec![(state.clone(), 1.0)],
        }
    }
    
    // Expand each initial branch and collect results
    let mut all_results: Vec<(MatchState, f64)> = Vec::new();
    for (init_state, init_prob) in initial_branches {
        match &init_state {
            MatchState::BattleState(_) => {
                // Use the existing expand_branch to recursively resolve the branch
                for (st, p) in expand_branch(&init_state, move_dex, pokemon_dex, config) {
                    all_results.push((st, p * init_prob));
                }
            }
            _ => {
                all_results.push((init_state.clone(), init_prob));
            }
        }
    }

    println!("[simulate_turn] all_results.len() = {}", all_results.len());

    coalesce_match_state_branches(all_results)
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
            if matches!(leaving.status, Some(Status::ToxicPoison(_))) {
                leaving.status = Some(Status::ToxicPoison(0));
            }
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
            if matches!(leaving.status, Some(Status::ToxicPoison(_))) {
                leaving.status = Some(Status::ToxicPoison(0));
            }
            std::mem::swap(&mut next_state.p2_active_mons[slot_idx], &mut next_state.p2_back_mons[bench_index]);
            next_state.p2_back_mons[bench_index] = leaving;
        }
    }
}

fn perform_simultaneous_switches(next_state: &mut BattleState, switches: &[(FieldSlot, usize)]) {
    // First perform all swaps, then resolve switch-in effects for all incoming Pokemon.
    for (slot, bench_index) in switches {
        perform_switch_out_in(next_state, *slot, *bench_index);
    }

    for (slot, _) in switches {
        simulator_helpers::process_pokemon_send_out(next_state, *slot);
    }
}

// Generate all permutations of a slice
fn permutations<T: Clone>(items: &[T]) -> Vec<Vec<T>> {
    let mut results: Vec<Vec<T>> = Vec::new();
    fn helper<T: Clone>(items: &[T], current: &mut Vec<T>, used: &mut Vec<bool>, results: &mut Vec<Vec<T>>) {
        if current.len() == items.len() {
            results.push(current.clone());
            return;
        }
        for i in 0..items.len() {
            if used[i] { continue; }
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

// Process a list of send-out slots in effective-speed order, branching on speed ties.
fn process_sendouts_in_speed_order_branching(base_state: &BattleState, slots: &[FieldSlot]) -> Vec<(BattleState, f64)> {
    if slots.is_empty() { return vec![(base_state.clone(), 1.0)]; }

    // Build groups by effective speed
    let trick = trick_room_is_active(base_state);
    let mut slot_speeds: Vec<(FieldSlot, f32)> = Vec::new();
    for slot in slots {
        if let Some(mon) = get_pokemon_at_slot(base_state, *slot) {
            slot_speeds.push((*slot, effective_speed_for_slot(base_state, *slot, mon)));
        }
    }
    // Sort speeds in descending (normal) or ascending (trick room) order
    slot_speeds.sort_by(|a, b| {
        if (a.1 - b.1).abs() < 0.01 { std::cmp::Ordering::Equal }
        else if trick { a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal) }
        else { b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal) }
    });

    // Group by equal speeds
    let mut groups: Vec<Vec<FieldSlot>> = Vec::new();
    let mut current_group: Vec<FieldSlot> = Vec::new();
    let mut last_speed: Option<f32> = None;
    for (slot, sp) in slot_speeds {
        if let Some(ls) = last_speed {
            if (sp - ls).abs() < 0.01 {
                current_group.push(slot);
            } else {
                groups.push(current_group.clone());
                current_group.clear();
                current_group.push(slot);
                last_speed = Some(sp);
            }
        } else {
            current_group.push(slot);
            last_speed = Some(sp);
        }
    }
    if !current_group.is_empty() {
        groups.push(current_group);
    }

    // For each group, get all permutations
    let mut group_perms: Vec<Vec<Vec<FieldSlot>>> = Vec::new();
    for g in &groups {
        if g.len() <= 1 {
            group_perms.push(vec![g.clone()]);
        } else {
            group_perms.push(permutations(&g));
        }
    }

    // Cartesian product of group permutations to produce full orders
    let mut all_orders: Vec<(Vec<FieldSlot>, f64)> = vec![(Vec::new(), 1.0)];
    for perms in group_perms {
        let mut new_orders: Vec<(Vec<FieldSlot>, f64)> = Vec::new();
        let inv_prob = 1.0 / perms.len() as f64; // each permutation equally likely within group
        for (so_far, prob) in &all_orders {
            for p in &perms {
                let mut concat = so_far.clone();
                concat.extend(p.clone());
                new_orders.push((concat, prob * inv_prob));
            }
        }
        all_orders = new_orders;
    }

    // Apply send_out effects in each order on a cloned base state
    let mut results: Vec<(BattleState, f64)> = Vec::new();
    for (order, prob) in all_orders {
        let mut st = base_state.clone();
        for slot in order {
            simulator_helpers::process_pokemon_send_out(&mut st, slot);
        }
        results.push((st, prob));
    }

    results
}

// Branching version of performing simultaneous switches: returns all possible resulting states with probabilities
fn perform_simultaneous_switches_branching(next_state: &BattleState, switches: &[(FieldSlot, usize)]) -> Vec<(BattleState, f64)> {
    // First apply all swaps to a base state
    let mut base = next_state.clone();
    for (slot, bench_index) in switches {
        perform_switch_out_in(&mut base, *slot, *bench_index);
    }
    // collect slots to process send-out effects for (the slots that were switched)
    let slots: Vec<FieldSlot> = switches.iter().map(|(s, _)| *s).collect();
    simulator_helpers::coalesce_branches(process_sendouts_in_speed_order_branching(&base, &slots))
}

// Branching version of creating battle state from preview that respects speed-order send-outs and ties
fn battle_state_from_preview_branching(
    preview: &TeamPreviewState,
    p1_preview: &TeamPreviewCommand,
    p2_preview: &TeamPreviewCommand,
) -> Vec<(MatchState, f64)> {
    let p1_active_mons: Vec<PokemonState> = p1_preview.active_indices.iter().map(|&i| preview.p1_mons[i].clone()).collect();
    let p1_back_mons: Vec<PokemonState> = p1_preview.back_indices.iter().map(|&i| preview.p1_mons[i].clone()).collect();

    let p2_active_mons: Vec<PokemonState> = p2_preview.active_indices.iter().map(|&i| preview.p2_mons[i].clone()).collect();
    let p2_back_mons: Vec<PokemonState> = p2_preview.back_indices.iter().map(|&i| preview.p2_mons[i].clone()).collect();

    let state = BattleState {
        active_per_side: preview.active_per_side,
        p1_active_mons,
        p2_active_mons,
        p1_back_mons,
        p2_back_mons,
        action_queue: vec![],
        turn_number: 0,
        turn_started: false,
        turn_ended: false,
        p1_has_tera: true,
        p2_has_tera: true,
        p1_has_mega: true,
        p2_has_mega: true,
        weather: None,
        weather_turns: None,
        pseudo_weathers: vec![],
        pseudo_weather_turns: vec![],
        terrain: None,
        terrain_turns: None,
        p1_side_conditions: vec![],
        p1_side_condition_turns: vec![],
        p2_side_conditions: vec![],
        p2_side_condition_turns: vec![],
        p1_slot_conditions: vec![Vec::new(); preview.active_per_side as usize],
        p2_slot_conditions: vec![Vec::new(); preview.active_per_side as usize],
    };

    // Collect all active send-out slots
    let mut slots: Vec<FieldSlot> = Vec::new();
    for slot_idx in 0..state.p1_active_mons.len() {
        slots.push(FieldSlot { player: Player::P1, slot_index: slot_idx as u8 });
    }
    for slot_idx in 0..state.p2_active_mons.len() {
        slots.push(FieldSlot { player: Player::P2, slot_index: slot_idx as u8 });
    }

    let branches = process_sendouts_in_speed_order_branching(&state, &slots);
    coalesce_match_state_branches(branches.into_iter().map(|(bs, p)| (MatchState::BattleState(bs), p)).collect())
}

// Branching apply_player_commands: returns possible MatchStates with probabilities
fn apply_player_commands_branching(
    state: &MatchState,
    p1_cmd: &PlayerCommand,
    p2_cmd: &PlayerCommand,
    move_dex: &HashMap<PokemonMove, MoveData>,
) -> Vec<(MatchState, f64)> {
    match state {
        MatchState::TeamPreviewState(preview) => {
            let p1_preview = match p1_cmd { PlayerCommand::TeamPreview(c) => c, _ => panic!("Expected TeamPreview command for P1"), };
            let p2_preview = match p2_cmd { PlayerCommand::TeamPreview(c) => c, _ => panic!("Expected TeamPreview command for P2"), };
            coalesce_match_state_branches(battle_state_from_preview_branching(preview, p1_preview, p2_preview))
        }
        MatchState::BattleState(battle) => {
            if let Some(game_over_state) = game_over_state_if_battle_finished(battle) {
                return vec![(game_over_state, 1.0)];
            }

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
                return vec![(MatchState::BattleState(next_state), 1.0)];
            }

            // Replacement phase: both flags are true -> players may send replacements
            if battle.turn_started && battle.turn_ended {
                let mut queued_switches: Vec<(FieldSlot, usize)> = Vec::new();

                if let PlayerCommand::Battle(cmds) = p1_cmd {
                    for (slot_idx, cmd) in cmds.iter().enumerate() {
                        if let BattleCommand::Switch(s) = cmd {
                            queued_switches.push((FieldSlot { player: Player::P1, slot_index: slot_idx as u8, }, s.party_index,));
                        }
                    }
                }

                if let PlayerCommand::Battle(cmds) = p2_cmd {
                    for (slot_idx, cmd) in cmds.iter().enumerate() {
                        if let BattleCommand::Switch(s) = cmd {
                            queued_switches.push((FieldSlot { player: Player::P2, slot_index: slot_idx as u8, }, s.party_index,));
                        }
                    }
                }

                let branches = perform_simultaneous_switches_branching(&next_state, &queued_switches);
                return coalesce_match_state_branches(branches.into_iter().map(|(bs, p)| (MatchState::BattleState(bs), p)).collect());
            }

            // Default: just queue commands mid-turn
            if !battle.turn_started && battle.turn_ended {
                next_state.turn_started = true;
            }

            if let PlayerCommand::Battle(p1_battle) = p1_cmd {
                queue_battle_commands_for_player(battle, Player::P1, p1_battle, move_dex, &mut next_state.action_queue);
            }
            if let PlayerCommand::Battle(p2_battle) = p2_cmd {
                queue_battle_commands_for_player(battle, Player::P2, p2_battle, move_dex, &mut next_state.action_queue);
            }

            vec![(MatchState::BattleState(next_state), 1.0)]
        }
        MatchState::GameOverState { .. } => vec![(state.clone(), 1.0)],
    }
}