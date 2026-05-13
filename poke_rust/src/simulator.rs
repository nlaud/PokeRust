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
use crate::dex_data::{MoveData, MoveTarget, PokemonData, MoveFlag};
use crate::dex_data::{MoveCategory, PokemonStat, Status, VolatileStatus};
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
    target: &PokemonState,
    move_name: &PokemonMove,
) -> (f64, bool) {
    let invulnerability_resolution = simulator_helpers::invulnerability_resolution(target, move_name);
    match invulnerability_resolution {
        simulator_helpers::InvulnerabilityResolution::Blocked => (0.0, false),
        simulator_helpers::InvulnerabilityResolution::Normal => (1.0, true),
        simulator_helpers::InvulnerabilityResolution::DoubleDamage => (2.0, true),
    }
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
    let move_has_charge = simulator_helpers::move_has_flag(move_data, &crate::dex_data::MoveFlag::Charge);
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

    // Handle semi-invulnerable moves (first turn)
    if move_causes_invulnerability && invulnerable_data.is_none() {
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
                        target_mon.volatiles.push(crate::pokemon::VolatileStatusState::TurnStatus(VolatileStatus::SkyDrop, 0));
                    }
                }
            }
        }

        // Decrement PP for semi-invulnerable moves on first turn
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
    if move_has_charge && charging_data.is_none() {
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

fn possible_damage_outcomes_for_move(
    state: &BattleState,
    action: &MoveAction,
    move_data: &MoveData,
    config: DamageConfig,
) -> Vec<(MatchState, f64)> {
    let mut next_state = state.clone();

    let Some(mut attacker) = get_pokemon_at_slot(&next_state, action.user_slot).cloned() else {
        return vec![(MatchState::BattleState(next_state), 1.0)];
    };

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

    // Resolve target list based on move's targeting type
    let target_slots = if move_target_is_multitarget(&move_data.target) {
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

    if target_slots.is_empty() {
        return vec![(MatchState::BattleState(next_state), 1.0)];
    }

    // Calculate targets multiplier (0.75x for 2+ targets, 1.0x for 1 target)
    let targets_mult = simulator_helpers::damage_targets_multiplier(target_slots.len());

    // Calculate hit/miss and damage outcomes for each target independently.
    // For spread moves this creates independent miss branches per target.
    let mut per_target_outcomes: Vec<(FieldSlot, Vec<(u16, bool, bool, f64)>)> = Vec::new();

    for target_slot in &target_slots {
        let mut outcomes_for_target: Vec<(u16, bool, bool, f64)> = Vec::new();

        let Some(target) = get_pokemon_at_slot(&next_state, *target_slot).cloned() else {
            // Target is fainted or doesn't exist, skip
            continue;
        };

        let (invulnerability_multiplier, should_continue) = check_invulnerability_status(&target, &action.move_name);
        
        if !should_continue {
            // Move is blocked by invulnerability
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
                    if let Some(target_mon) = match target_slot.player {
                        Player::P1 => branch_state.p1_active_mons.get_mut(target_slot.slot_index as usize),
                        Player::P2 => branch_state.p2_active_mons.get_mut(target_slot.slot_index as usize),
                    } {
                        simulator_helpers::apply_damage(target_mon, *damage);

                        if move_data.name == PokemonMove::SkyDrop {
                            simulator_helpers::remove_status_volatile(target_mon, &VolatileStatus::SkyDrop);
                        }
                    }

                    // Apply secondary effects, which now returns branched states with probabilities
                    let sec_branches = simulator_helpers::apply_secondary_effects(&branch_state, action.user_slot, *target_slot, move_data);
                    for (bs, sec_prob) in sec_branches {
                        let combined_prob = existing_prob * outcome_prob * sec_prob;
                        new_all_outcomes.push((MatchState::BattleState(bs), combined_prob));
                    }
                } else {
                    let combined_prob = existing_prob * outcome_prob;
                    new_all_outcomes.push((MatchState::BattleState(branch_state), combined_prob));
                }
            }
        }

        all_outcomes = new_all_outcomes;
    }

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

    BattleState {
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
        weathers: vec![],
        weather_turns: vec![],
        pseudo_weathers: vec![],
        pseudo_weather_turns: vec![],
        terrains: vec![],
        terrain_turns: vec![],
        p1_side_conditions: vec![],
        p1_side_condition_turns: vec![],
        p2_side_conditions: vec![],
        p2_side_condition_turns: vec![],
        p1_slot_conditions: vec![Vec::new(); preview.active_per_side as usize],
        p2_slot_conditions: vec![Vec::new(); preview.active_per_side as usize],
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
            // End-of-turn processing
            simulator_helpers::end_turn(&mut next_state);
            next_state.turn_started = true;
            next_state.turn_ended = true;
        } else {
            // Still call end_turn wrapper to keep behavior consistent
            simulator_helpers::end_turn(&mut next_state);
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