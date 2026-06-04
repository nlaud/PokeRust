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

/// Get a mutable reference to the active Pokémon at `slot`.
fn mon_at_slot_mut(state: &mut BattleState, slot: FieldSlot) -> Option<&mut PokemonState> {
    simulator_helpers::get_pokemon_at_slot_mut(state, slot)
}

/// Copy `volatiles` into the slot in-place (write-back after local mutation).
fn write_back_volatiles(state: &mut BattleState, slot: FieldSlot, volatiles: Vec<crate::pokemon::VolatileStatusState>) {
    if let Some(mon) = mon_at_slot_mut(state, slot) {
        mon.volatiles = volatiles;
    }
}

fn opposing_player(player: Player) -> Player {
    match player {
        Player::P1 => Player::P2,
        Player::P2 => Player::P1,
    }
}

/// True if any fainted active Pokémon has a healthy bench-mate to replace it.
fn replacement_needed(state: &BattleState) -> bool {
    for mon in &state.p1_active_mons {
        if mon.fainted && state.p1_back_mons.iter().any(|m| !m.fainted) {
            return true;
        }
    }
    for mon in &state.p2_active_mons {
        if mon.fainted && state.p2_back_mons.iter().any(|m| !m.fainted) {
            return true;
        }
    }
    false
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
    let Some(attacker) = simulator_helpers::get_pokemon_at_slot(state, user_slot).cloned() else {
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

/// Resolve the targets for a charging or semi-invulnerable move, returning `None` (→ no-op branch)
/// if there are none.
fn resolve_charge_targets(
    next_state: &BattleState,
    action: &MoveAction,
    move_data: &MoveData,
) -> Option<Vec<FieldSlot>> {
    if simulator_helpers::move_target_is_multitarget(&move_data.target) {
        let targets = simulator_helpers::resolve_move_targets(next_state, action.user_slot, &move_data.target);
        Some(targets)
    } else {
        match action.target_slot {
            Some(slot) => Some(vec![slot]),
            None => {
                let targets = simulator_helpers::resolve_move_targets(next_state, action.user_slot, &move_data.target);
                if targets.is_empty() { None } else { Some(targets) }
            }
        }
    }
}

fn handle_semi_invulnerable_first_turn(
    attacker: &mut PokemonState,
    action: &MoveAction,
    move_data: &MoveData,
    next_state: &mut BattleState,
) -> Option<Vec<(MatchState, f64)>> {
    // Sky Drop: check that the target can be grabbed
    if action.move_name == PokemonMove::SkyDrop {
        let sky_targets = resolve_charge_targets(next_state, action, move_data).unwrap_or_default();
        if let Some(target) = sky_targets.first().and_then(|s| simulator_helpers::get_pokemon_at_slot(next_state, *s)) {
            if simulator_helpers::sky_drop_first_turn_fails(next_state, target) {
                return Some(vec![(MatchState::BattleState(next_state.clone()), 1.0)]);
            }
        }
    }

    let targets = resolve_charge_targets(next_state, action, move_data)?;

    simulator_helpers::add_invulnerable_volatile(attacker, action.move_name.clone(), targets.clone());
    write_back_volatiles(next_state, action.user_slot, attacker.volatiles.clone());

    if action.move_name == PokemonMove::SkyDrop {
        for target_slot in &targets {
            if let Some(target_mon) = mon_at_slot_mut(next_state, *target_slot) {
                if !simulator_helpers::has_status_volatile(target_mon, &VolatileStatus::SkyDrop) {
                    target_mon.volatiles.push(crate::pokemon::VolatileStatusState::TurnStatus(VolatileStatus::SkyDrop, 2));
                }
            }
        }
    }

    Some(vec![(MatchState::BattleState(next_state.clone()), 1.0)])
}

fn handle_charging_first_turn(
    attacker: &mut PokemonState,
    action: &MoveAction,
    move_data: &MoveData,
    next_state: &mut BattleState,
) -> Option<Vec<(MatchState, f64)>> {
    let targets = resolve_charge_targets(next_state, action, move_data)?;
    attacker.volatiles.push(crate::pokemon::VolatileStatusState::Charging(action.move_name.clone(), targets));
    write_back_volatiles(next_state, action.user_slot, attacker.volatiles.clone());

    // Decrement PP on the charge turn
    if let Some(pp_idx) = attacker.moves.iter().position(|m| m.as_ref() == Some(&action.move_name)) {
        if let Some(mon) = mon_at_slot_mut(next_state, action.user_slot) {
            if let Some(pp) = mon.move_pp.get_mut(pp_idx) { *pp = pp.saturating_sub(1); }
        }
    }

    Some(vec![(MatchState::BattleState(next_state.clone()), 1.0)])
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

    let charging_data = attacker.volatiles.iter().find_map(|v| {
        if let crate::pokemon::VolatileStatusState::Charging(mov, targets) = v {
            if mov == &action.move_name { Some((v.clone(), targets.clone())) } else { None }
        } else { None }
    });

    let is_semi_invulnerable = attacker.volatiles.iter().any(|v| {
        matches!(v, crate::pokemon::VolatileStatusState::MoveStatus(VolatileStatus::SemiInvulnerable(mov), _) if mov == &action.move_name)
    });

    // ElectroShot / MeteorBeam: +1 SpA on the charge turn
    if matches!(action.move_name, PokemonMove::ElectroShot | PokemonMove::MeteorBeam) && charging_data.is_none() {
        attacker.boosts[2] = (attacker.boosts[2] + 1).clamp(-6, 6);
        if let Some(mon) = mon_at_slot_mut(next_state, action.user_slot) {
            mon.boosts = attacker.boosts;
        }
    }

    // Semi-invulnerable: first turn → enter invulnerability
    if move_causes_invulnerability && !is_semi_invulnerable {
        return handle_semi_invulnerable_first_turn(attacker, action, move_data, next_state);
    }

    // Semi-invulnerable: second turn → remove volatile, then fall through to normal damage
    if is_semi_invulnerable {
        simulator_helpers::remove_invulnerable_volatile(attacker, &action.move_name);
        write_back_volatiles(next_state, action.user_slot, attacker.volatiles.clone());
    }

    // Charging: first turn → store volatile and wait
    if move_has_charge && charging_data.is_none() && !move_causes_invulnerability {
        return handle_charging_first_turn(attacker, action, move_data, next_state);
    }

    // Charging: second turn → validate target, remove volatile, fall through
    if let Some((volatile_state, stored_targets)) = charging_data {
        if let Some(target_slot) = action.target_slot {
            if !stored_targets.contains(&target_slot) {
                return Some(vec![(MatchState::BattleState(next_state.clone()), 1.0)]);
            }
        }
        if let Some(pos) = attacker.volatiles.iter().position(|v| std::mem::discriminant(v) == std::mem::discriminant(&volatile_state)) {
            attacker.volatiles.remove(pos);
        }
        write_back_volatiles(next_state, action.user_slot, attacker.volatiles.clone());
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
    let Some(_initial_target) = simulator_helpers::get_pokemon_at_slot(state, target_slot).cloned() else {
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
                let Some(current_target) = simulator_helpers::get_pokemon_at_slot(&branch_state, target_slot).cloned() else {
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

/// Build a "move did nothing" outcome, optionally mixing in a 50/50 confusion self-hit branch.
fn no_effect_outcome(
    state: &BattleState,
    action: &MoveAction,
    confusion_outcomes: &Option<Vec<(MatchState, f64)>>,
) -> Vec<(MatchState, f64)> {
    let mut no_effect_state = state.clone();
    decrement_move_pp(&mut no_effect_state, action.user_slot, &action.move_name);

    if let Some(confusion) = confusion_outcomes {
        let mut combined: Vec<(MatchState, f64)> = confusion.iter()
            .map(|(st, p)| (st.clone(), p * 0.5))
            .collect();
        combined.push((MatchState::BattleState(no_effect_state), 0.5));
        simulator_helpers::coalesce_branches(combined)
    } else {
        vec![(MatchState::BattleState(no_effect_state), 1.0)]
    }
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

    let Some(mut attacker) = simulator_helpers::get_pokemon_at_slot(&next_state, action.user_slot).cloned() else {
        return vec![(MatchState::BattleState(next_state), 1.0)];
    };

    // Save pre-move state for potential failure branches (paralysis, sleep, freeze)
    let pre_move_state = next_state.clone();

    simulator_helpers::decrement_move_statuses(&mut attacker);
    write_back_volatiles(&mut next_state, action.user_slot, attacker.volatiles.clone());

    // Check Flinch
    if attacker.volatiles.iter().any(|v| matches!(v, crate::pokemon::VolatileStatusState::TurnStatus(VolatileStatus::Flinch, _))) {
        // Find and remove charging and semi-invulnerable volatiles
        if let Some(pos) = attacker.volatiles.iter().position(|v| matches!(v, crate::pokemon::VolatileStatusState::Charging(_, _) | crate::pokemon::VolatileStatusState::MoveStatus(VolatileStatus::SemiInvulnerable(_), _))) {
            attacker.volatiles.remove(pos);
            write_back_volatiles(&mut next_state, action.user_slot, attacker.volatiles.clone());
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
            write_back_volatiles(&mut confusion_state, action.user_slot, attacker.volatiles.clone());

            confusion_self_hit_outcomes = Some(resolve_confusion_self_hit_outcomes(
                &confusion_state,
                action.user_slot,
                &action.move_name,
                config,
            ));

            next_state = confusion_state;
        } else {
            // Confusion expired; write back so the volatile is cleared in next_state too.
            write_back_volatiles(&mut next_state, action.user_slot, attacker.volatiles.clone());
        }
    }

    if move_name == PokemonMove::Splash {
        return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
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
            return simulator_helpers::coalesce_branches(combined_confused);
        }

        return simulator_helpers::coalesce_branches(combined);
    }

    if move_name == PokemonMove::SteelRoller && simulator_helpers::current_terrain(&next_state).is_none() {
        return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
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
    } else if simulator_helpers::move_target_is_multitarget(&move_data.target) {
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
    if !simulator_helpers::move_target_is_multitarget(&move_data.target)
        && !(move_name == PokemonMove::ExpandingForce && simulator_helpers::pokemon_is_on_terrain(&next_state, &attacker, &crate::dex_data::Terrain::PsychicTerrain))
    {
        target_slots = simulator_helpers::check_and_apply_redirection(&next_state, action.user_slot, target_slots);
    }

    if target_slots.is_empty() {
        return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
    }

    // Calculate targets multiplier (0.75x for 2+ targets, 1.0x for 1 target)
    let targets_mult = simulator_helpers::damage_targets_multiplier(target_slots.len());

    let is_multihit_move = move_name == PokemonMove::BeatUp
        || move_data.multihit_range != [0, 0]
        || move_data.multihit_accuracy;

    if is_multihit_move {
        if target_slots.is_empty() {
            return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
        }

        let target_slot = target_slots[0];
        let Some(target) = simulator_helpers::get_pokemon_at_slot(&next_state, target_slot).cloned() else {
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

        let mut all_outcomes: Vec<(MatchState, f64)> = all_outcomes
            .into_iter()
            .map(|(state, prob)| match state {
                MatchState::BattleState(bs) => (
                    apply_post_damage_move_effects(bs, action.user_slot, move_data, &next_state, opposing_player),
                    prob,
                ),
                other => (other, prob),
            })
            .collect();

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

        return simulator_helpers::coalesce_branches(final_outcomes);
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

        let Some(target) = simulator_helpers::get_pokemon_at_slot(&next_state, *target_slot).cloned() else {
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
        return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
    }

    // Log move info if verbosity >= 4
    if simulator_helpers::get_verbosity() >= 4 {
        let target_names: Vec<String> = target_slots.iter().filter_map(|slot| {
            simulator_helpers::get_pokemon_at_slot(&next_state, *slot).map(|m| simulator_helpers::species_name_sim(&m.species))
        }).collect();
        println!(
            "{}",
            format!(
                "{} uses {} | targets: {} | move type: {} | PP: {}",
                simulator_helpers::species_name_sim(&attacker.species),
                simulator_helpers::move_name_sim(&move_name),
                target_names.join(", "),
                simulator_helpers::pokemon_type_name(&move_data.pokemon_type),
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
                let branch_state = match existing_state.clone() {
                    MatchState::BattleState(bs) => bs,
                    _ => continue,
                };
                let combined_prob = existing_prob * outcome_prob;

                if *hit {
                    // Delegate to the already-extracted single-hit helper
                    for (bs, prob) in apply_single_hit_branch(branch_state, *target_slot, &move_name, move_data, *damage, action.user_slot, combined_prob) {
                        new_all_outcomes.push((MatchState::BattleState(bs), prob));
                    }
                } else {
                    // Miss: only thaw a frozen target if Scald/SteamEruption is used in harsh sun
                    let mut branch_state = branch_state;
                    if simulator_helpers::weather_is_harsh_sunlight(&branch_state)
                        && matches!(move_data.name, PokemonMove::Scald | PokemonMove::SteamEruption)
                    {
                        if let Some(target_mon) = mon_at_slot_mut(&mut branch_state, *target_slot) {
                            if matches!(target_mon.status, Some(Status::Frozen(_))) {
                                target_mon.status = None;
                            }
                        }
                    }
                    new_all_outcomes.push((MatchState::BattleState(branch_state), combined_prob));
                }
            }
        }

        all_outcomes = new_all_outcomes;
    }

    // Apply post-damage move effects that depend on total HP damage dealt.
    let opposing_player = opposing_player(action.user_slot.player);
    let all_outcomes: Vec<(MatchState, f64)> = all_outcomes
        .into_iter()
        .map(|(state, prob)| match state {
            MatchState::BattleState(bs) => (
                apply_post_damage_move_effects(bs, action.user_slot, move_data, &next_state, opposing_player),
                prob,
            ),
            other => (other, prob),
        })
        .collect();
    let mut all_outcomes: Vec<(MatchState, f64)> = all_outcomes;

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
    if simulator_helpers::get_verbosity() >= 4 {
        println!("{}", format!("  [Verbosity 4] {} total damage outcome combinations:", final_outcomes.len()).bright_yellow());
        for (idx, (_, prob)) in final_outcomes.iter().enumerate() {
            println!("    Branch {}: {:.6} probability", idx + 1, prob);
        }
    }

    simulator_helpers::coalesce_branches(final_outcomes)
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

/// Push all (tera/mega) attack command variants for `move_slot` × `target` into `cmds`.
fn push_attack_variants(cmds: &mut Vec<BattleCommand>, move_slot: usize, target: Option<FieldSlot>, can_tera: bool, can_mega: bool) {
    for (tera, mega) in [(false, false), (true, false), (false, true), (true, true)] {
        if tera && !can_tera { continue; }
        if mega && !can_mega { continue; }
        cmds.push(BattleCommand::Attack(AttackCommand { move_slot, target, terastallize: tera, mega_evolve: mega }));
    }
}

/// Return commands for a Pokémon that is locked into a semi-invulnerable move (or pass if not found).
fn locked_semi_invulnerable_commands(mon: &PokemonState, player: Player) -> Vec<BattleCommand> {
    for (i, move_opt) in mon.moves.iter().enumerate() {
        if let Some(m) = move_opt {
            if simulator_helpers::move_causes_invulnerability(m) {
                return vec![BattleCommand::Attack(AttackCommand {
                    move_slot: i,
                    target: Some(FieldSlot { player, slot_index: 0 }),
                    terastallize: false,
                    mega_evolve: false,
                })];
            }
        }
    }
    vec![BattleCommand::Pass]
}

/// Return commands for a Pokémon locked into a charging move (or pass if move not found).
fn locked_charging_commands(mon: &PokemonState, charged_move: &PokemonMove, charged_targets: &[FieldSlot]) -> Vec<BattleCommand> {
    for (i, move_opt) in mon.moves.iter().enumerate() {
        if let Some(m) = move_opt {
            if m == charged_move {
                return charged_targets.iter()
                    .map(|t| BattleCommand::Attack(AttackCommand { move_slot: i, target: Some(*t), terastallize: false, mega_evolve: false }))
                    .collect();
            }
        }
    }
    vec![BattleCommand::Pass]
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

    if slot_idx >= my_active.len() { return Vec::new(); }
    let mon = &my_active[slot_idx];

    // Locked: must recharge
    if mon.volatiles.iter().any(|v| matches!(v, crate::pokemon::VolatileStatusState::TurnStatus(VolatileStatus::MustRecharge, _))) {
        return vec![BattleCommand::Pass];
    }

    // Locked: semi-invulnerable (e.g. mid-Fly)
    if mon.volatiles.iter().any(|v| matches!(v, crate::pokemon::VolatileStatusState::MoveStatus(VolatileStatus::SemiInvulnerable(_), _))) {
        return locked_semi_invulnerable_commands(mon, player);
    }

    // Locked: charging (e.g. mid-SolarBeam)
    if let Some((charged_move, charged_targets)) = mon.volatiles.iter().find_map(|v| {
        if let crate::pokemon::VolatileStatusState::Charging(mov, targets) = v { Some((mov.clone(), targets.clone())) } else { None }
    }) {
        return locked_charging_commands(mon, &charged_move, &charged_targets);
    }

    // Normal turn: switches first
    let mut cmds: Vec<BattleCommand> = my_back.iter().enumerate()
        .filter(|(_, m)| !m.fainted)
        .map(|(i, _)| BattleCommand::Switch(SwitchCommand { party_index: i }))
        .collect();

    if mon.fainted { return cmds; }

    let can_tera = !has_tera && !mon.is_tera;
    let can_mega = has_mega && mon.has_mega_form && {
        let item_ok = mon.mega_species.as_ref()
            .and_then(|sp| pokemon_dex.get(sp))
            .and_then(|data| data.required_item.as_ref())
            .map_or(true, |req| format!("{:?}", mon.item).to_lowercase() == *req);
        item_ok
    };

    // Attacks
    for (i, move_name_opt) in mon.moves.iter().enumerate() {
        let Some(move_name) = move_name_opt else { continue; };
        let target_type = move_dex.get(move_name).map(|d| &d.target).unwrap_or(&MoveTarget::Normal);

        let valid_targets = if *move_name == PokemonMove::ExpandingForce
            && simulator_helpers::pokemon_is_on_terrain(state, mon, &crate::dex_data::Terrain::PsychicTerrain)
        {
            vec![None]
        } else {
            get_valid_targets(target_type, player, state, slot_idx)
        };

        for target in valid_targets {
            push_attack_variants(&mut cmds, i, target, can_tera, can_mega);
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

pub fn get_possible_commands_for_active_slot(
    state: &BattleState,
    player: Player,
    slot_idx: usize,
    move_dex: &HashMap<PokemonMove, MoveData>,
    pokemon_dex: &HashMap<Species, PokemonData>,
) -> Vec<BattleCommand> {
    generate_commands_for_active(player, slot_idx, state, move_dex, pokemon_dex)
}

fn game_over_state_if_battle_finished(state: &BattleState) -> Option<MatchState> {
    let p1_has_remaining = simulator_helpers::team_has_remaining_pokemon(state, Player::P1);
    let p2_has_remaining = simulator_helpers::team_has_remaining_pokemon(state, Player::P2);

    match (p1_has_remaining, p2_has_remaining) {
        (false, true) => Some(MatchState::GameOverState { winner: Player::P2 }),
        (true, false) => Some(MatchState::GameOverState { winner: Player::P1 }),
        _ => None,
    }
}

/// Measure total HP damage dealt to `opposing_player` between `baseline` and `after`.
fn total_damage_to_opponent(baseline: &BattleState, after: &BattleState, opposing_player: Player) -> u32 {
    let (before_active, before_back) = match opposing_player {
        Player::P1 => (&baseline.p1_active_mons, &baseline.p1_back_mons),
        Player::P2 => (&baseline.p2_active_mons, &baseline.p2_back_mons),
    };
    let (after_active, after_back) = match opposing_player {
        Player::P1 => (&after.p1_active_mons, &after.p1_back_mons),
        Player::P2 => (&after.p2_active_mons, &after.p2_back_mons),
    };
    before_active.iter().zip(after_active).map(|(b, a)| b.hp.saturating_sub(a.hp) as u32).sum::<u32>()
        + before_back.iter().zip(after_back).map(|(b, a)| b.hp.saturating_sub(a.hp) as u32).sum::<u32>()
}

/// Apply the attacker's heal/drain/recoil and then resolve game-over after a move lands.
fn apply_post_damage_move_effects(
    mut bs: BattleState,
    attacker_slot: FieldSlot,
    move_data: &MoveData,
    baseline: &BattleState,
    opposing_player: Player,
) -> MatchState {
    let total_dmg = total_damage_to_opponent(baseline, &bs, opposing_player);
    let opponent_wiped = !simulator_helpers::team_has_remaining_pokemon(&bs, opposing_player) && total_dmg > 0;
    let mut forced_winner: Option<Player> = None;

    if let Some(attacker_mon) = mon_at_slot_mut(&mut bs, attacker_slot) {
        let max_hp = attacker_mon.stats[0].max(1);

        // Unconditional self-heal
        if move_data.heal_fraction[0] > 0 && move_data.heal_fraction[1] > 0 {
            let heal = ((max_hp as u32 * move_data.heal_fraction[0] as u32) / move_data.heal_fraction[1] as u32) as u16;
            if heal > 0 { attacker_mon.hp = attacker_mon.hp.saturating_add(heal).min(max_hp); attacker_mon.fainted = false; }
        }

        // Drain heal
        if move_data.drain_fraction[0] > 0 && move_data.drain_fraction[1] > 0 {
            let heal = ((total_dmg * move_data.drain_fraction[0] as u32) / move_data.drain_fraction[1] as u32) as u16;
            if heal > 0 { attacker_mon.hp = attacker_mon.hp.saturating_add(heal).min(max_hp); attacker_mon.fainted = false; }
        }

        // Recoil
        let has_recoil = (move_data.recoil_fraction[0] > 0 && move_data.recoil_fraction[1] > 0) || move_data.struggle_recoil;
        if has_recoil && attacker_mon.ability != Ability::RockHead && attacker_mon.ability != Ability::MagicGuard {
            let recoil = if move_data.recoil_fraction[0] > 0 && move_data.recoil_fraction[1] > 0 {
                ((total_dmg * move_data.recoil_fraction[0] as u32) / move_data.recoil_fraction[1] as u32) as u16
            } else if move_data.struggle_recoil {
                (max_hp as u32 / 4) as u16
            } else { 0 };

            if recoil > 0 {
                simulator_helpers::apply_damage(attacker_mon, recoil);
                if attacker_mon.fainted {
                    simulator_helpers::clear_pokemon_on_faint(attacker_mon);
                    if opponent_wiped { forced_winner = Some(attacker_slot.player); }
                }
            }
        }
    }

    if let Some(winner) = forced_winner {
        MatchState::GameOverState { winner }
    } else if let Some(game_over) = game_over_state_if_battle_finished(&bs) {
        game_over
    } else {
        MatchState::BattleState(bs)
    }
}

/// Print a human-readable description of `action` at verbosity ≥ 4.
fn log_action_verbose(state: &BattleState, action: &Action) {
    if simulator_helpers::get_verbosity() < 4 { return; }
    match action {
        Action::MoveAction(m) => {
            let attacker = simulator_helpers::get_pokemon_at_slot(state, m.user_slot)
                .map(|p| simulator_helpers::species_name_sim(&p.species))
                .unwrap_or_else(|| format!("{:?} slot {}", m.user_slot.player, m.user_slot.slot_index + 1));
            let target = m.target_slot
                .and_then(|slot| simulator_helpers::get_pokemon_at_slot(state, slot))
                .map(|p| simulator_helpers::species_name_sim(&p.species))
                .unwrap_or("(no specific target)".to_string());
            println!("{}", format!("Processing Move: {} uses {} -> {}", attacker, simulator_helpers::move_name_sim(&m.move_name), target).cyan());
        }
        Action::SwitchAction(s) => {
            let user = simulator_helpers::get_pokemon_at_slot(state, s.user_slot)
                .map(|p| simulator_helpers::species_name_sim(&p.species))
                .unwrap_or_else(|| format!("{:?} slot {}", s.user_slot.player, s.user_slot.slot_index + 1));
            println!("{}", format!("Processing Switch: {} (slot {})", user, s.switch_index + 1).blue());
        }
        Action::MegaAction(m) => {
            let mon = simulator_helpers::get_pokemon_at_slot(state, m.user_slot)
                .map(|p| simulator_helpers::species_name_sim(&p.species))
                .unwrap_or_else(|| format!("{:?} slot {}", m.user_slot.player, m.user_slot.slot_index + 1));
            println!("{}", format!("Processing Mega Evolution: {}", mon).yellow());
        }
        Action::TeraAction(t) => {
            let mon = simulator_helpers::get_pokemon_at_slot(state, t.user_slot)
                .map(|p| simulator_helpers::species_name_sim(&p.species))
                .unwrap_or_else(|| format!("{:?} slot {}", t.user_slot.player, t.user_slot.slot_index + 1));
            println!("{}", format!("Processing Terastallize: {}", mon).bright_magenta());
        }
    }
}

/// Execute a single action on `state`, returning all resulting (MatchState, probability) branches.
fn execute_action(
    mut state: BattleState,
    action: Action,
    move_dex: &HashMap<PokemonMove, MoveData>,
    pokemon_dex: &HashMap<Species, PokemonData>,
    config: DamageConfig,
) -> Vec<(MatchState, f64)> {
    match action {
        Action::MoveAction(m) => {
            let Some(mon) = simulator_helpers::get_pokemon_at_slot(&state, m.user_slot) else {
                return vec![(MatchState::BattleState(state), 1.0)];
            };
            if mon.fainted { return vec![(MatchState::BattleState(state), 1.0)]; }
            match move_dex.get(&m.move_name) {
                Some(move_data) => possible_damage_outcomes_for_move(&state, &m, move_data, config, move_dex, pokemon_dex),
                None => vec![(MatchState::BattleState(state), 1.0)],
            }
        }
        Action::SwitchAction(s) => {
            perform_switch_out_in(&mut state, s.user_slot, s.switch_index);
            simulator_helpers::process_pokemon_send_out(&mut state, s.user_slot);
            if simulator_helpers::get_verbosity() >= 2 {
                let user = simulator_helpers::get_pokemon_at_slot(&state, s.user_slot)
                    .map(|p| simulator_helpers::species_name_sim(&p.species))
                    .unwrap_or_else(|| format!("{:?} slot {}", s.user_slot.player, s.user_slot.slot_index + 1));
                println!("{}", format!("Executed Switch: new active at slot {} is {}", s.user_slot.slot_index + 1, user).bright_green());
            }
            vec![(MatchState::BattleState(state), 1.0)]
        }
        Action::MegaAction(m) => {
            let slot_idx = m.user_slot.slot_index as usize;
            let mons = match m.user_slot.player { Player::P1 => &mut state.p1_active_mons, Player::P2 => &mut state.p2_active_mons };
            if let Some(mon) = mons.get_mut(slot_idx) { crate::battle::try_mega_evolution(mon, pokemon_dex); }
            match m.user_slot.player { Player::P1 => state.p1_has_mega = false, Player::P2 => state.p2_has_mega = false }
            vec![(MatchState::BattleState(state), 1.0)]
        }
        Action::TeraAction(t) => {
            let slot_idx = t.user_slot.slot_index as usize;
            let mons = match t.user_slot.player { Player::P1 => &mut state.p1_active_mons, Player::P2 => &mut state.p2_active_mons };
            if let Some(mon) = mons.get_mut(slot_idx) { mon.is_tera = true; }
            match t.user_slot.player { Player::P1 => state.p1_has_tera = false, Player::P2 => state.p2_has_tera = false }
            vec![(MatchState::BattleState(state), 1.0)]
        }
    }
}

fn step_action_queue(
    state: &BattleState,
    move_dex: &HashMap<PokemonMove, MoveData>,
    pokemon_dex: &HashMap<Species, PokemonData>,
    config: DamageConfig,
) -> Vec<(MatchState, f64)> {
    let mut next_state = state.clone();

    // Empty queue — end of turn / replacement phase
    if next_state.action_queue.is_empty() {
        simulator_helpers::end_turn(&mut next_state);
        if let Some(game_over) = game_over_state_if_battle_finished(&next_state) {
            return vec![(game_over, 1.0)];
        }
        if replacement_needed(&next_state) {
            next_state.turn_started = true;
            next_state.turn_ended = true;
        } else {
            next_state.turn_started = false;
            next_state.turn_ended = false;
        }
        return vec![(MatchState::BattleState(next_state), 1.0)];
    }

    // Find the highest-priority action(s), branching equally on speed ties
    let mut best_indices: Vec<usize> = vec![0];
    for i in 1..next_state.action_queue.len() {
        let cmp = simulator_helpers::compare_action_order(
            &next_state.action_queue[best_indices[0]], &next_state.action_queue[i], state, move_dex,
        );
        match cmp {
            std::cmp::Ordering::Greater => best_indices = vec![i],
            std::cmp::Ordering::Equal  => best_indices.push(i),
            _ => {}
        }
    }

    if best_indices.len() > 1 {
        let branch_prob = 1.0 / best_indices.len() as f64;
        let mut combined: Vec<(MatchState, f64)> = Vec::new();
        for &idx in &best_indices {
            let mut branch_state = next_state.clone();
            let action = branch_state.action_queue.remove(idx);
            log_action_verbose(&branch_state, &action);
            for (st, p) in execute_action(branch_state, action, move_dex, pokemon_dex, config) {
                combined.push((st, p * branch_prob));
            }
        }
        return simulator_helpers::coalesce_branches(combined);
    }

    let action = next_state.action_queue.remove(best_indices[0]);
    log_action_verbose(&next_state, &action);
    execute_action(next_state, action, move_dex, pokemon_dex, config)
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
    let config = DamageConfig { consider_crit, damage_rolls };

    // Populate the action queue; may branch due to speed-tied send-outs
    let initial_branches = apply_player_commands_branching(state, p1_cmd, p2_cmd, move_dex);

    // When the queue starts empty, set turn-flag state before expanding
    let initial_branches: Vec<(MatchState, f64)> = initial_branches
        .into_iter()
        .map(|(mut st, prob)| {
            if let MatchState::BattleState(bs) = &mut st {
                if bs.action_queue.is_empty() {
                    bs.turn_started = replacement_needed(bs);
                    bs.turn_ended = bs.turn_started;
                }
            }
            (st, prob)
        })
        .collect();
    let initial_branches = simulator_helpers::coalesce_branches(initial_branches);

    fn expand_branch(
        state: &MatchState,
        move_dex: &HashMap<PokemonMove, MoveData>,
        pokemon_dex: &HashMap<Species, PokemonData>,
        config: DamageConfig,
    ) -> Vec<(MatchState, f64)> {
        let MatchState::BattleState(battle) = state else {
            return vec![(state.clone(), 1.0)];
        };

        let outcomes = step_action_queue(battle, move_dex, pokemon_dex, config);

        if battle.action_queue.is_empty() {
            return simulator_helpers::coalesce_branches(outcomes);
        }

        let mut aggregated = Vec::new();
        for (next_state, probability) in outcomes {
            for (final_state, final_prob) in expand_branch(&next_state, move_dex, pokemon_dex, config) {
                aggregated.push((final_state, probability * final_prob));
            }
        }
        simulator_helpers::coalesce_branches(aggregated)
    }

    let all_results: Vec<(MatchState, f64)> = initial_branches
        .into_iter()
        .flat_map(|(init_state, init_prob)| {
            expand_branch(&init_state, move_dex, pokemon_dex, config)
                .into_iter()
                .map(move |(st, p)| (st, p * init_prob))
        })
        .collect();

    simulator_helpers::coalesce_branches(all_results)
}

/// Public validator wrapper used by interactive UI to check legality
pub fn validate_battle_command_combination(cmds: &[BattleCommand]) -> bool {
    is_valid_command_combination(cmds)
}

/// Reset volatile statuses and boosts on a Pokémon that is switching out.
fn clear_pokemon_for_switch_out(mon: &mut PokemonState) {
    mon.volatiles.clear();
    mon.boosts.iter_mut().for_each(|b| *b = 0);
    if matches!(mon.status, Some(Status::ToxicPoison(_))) {
        mon.status = Some(Status::ToxicPoison(0));
    }
}

fn perform_switch_out_in(next_state: &mut BattleState, user_slot: FieldSlot, bench_index: usize) {
    let slot_idx = user_slot.slot_index as usize;
    let (active, back) = match user_slot.player {
        Player::P1 => (&mut next_state.p1_active_mons, &mut next_state.p1_back_mons),
        Player::P2 => (&mut next_state.p2_active_mons, &mut next_state.p2_back_mons),
    };
    if slot_idx >= active.len() || bench_index >= back.len() { return; }

    let mut leaving = active[slot_idx].clone();
    clear_pokemon_for_switch_out(&mut leaving);
    std::mem::swap(&mut active[slot_idx], &mut back[bench_index]);
    back[bench_index] = leaving;

    // All switch-out side effects (switch-out abilities, Neutralizing Gas lift, primal
    // weather ending) are handled here, after the departing Pokémon has reached the bench.
    simulator_helpers::handle_pokemon_switch_out(next_state, user_slot.player, bench_index);
}

// Process a list of send-out slots in effective-speed order, branching on speed ties.
fn process_sendouts_in_speed_order_branching(base_state: &BattleState, slots: &[FieldSlot]) -> Vec<(BattleState, f64)> {
    if slots.is_empty() { return vec![(base_state.clone(), 1.0)]; }

    // Build groups by effective speed
    let trick = simulator_helpers::trick_room_is_active(base_state);
    let mut slot_speeds: Vec<(FieldSlot, f32)> = Vec::new();
    for slot in slots {
        if let Some(mon) = simulator_helpers::get_pokemon_at_slot(base_state, *slot) {
            slot_speeds.push((*slot, simulator_helpers::effective_speed_for_slot(base_state, *slot, mon)));
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
            group_perms.push(simulator_helpers::permutations(g));
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
    simulator_helpers::coalesce_branches(branches.into_iter().map(|(bs, p)| (MatchState::BattleState(bs), p)).collect())
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
            simulator_helpers::coalesce_branches(battle_state_from_preview_branching(preview, p1_preview, p2_preview))
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
                return simulator_helpers::coalesce_branches(branches.into_iter().map(|(bs, p)| (MatchState::BattleState(bs), p)).collect());
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