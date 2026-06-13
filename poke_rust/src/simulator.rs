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
use crate::dex_data::{MoveCategory, SelfSwitchType, Status, VolatileStatus};
use crate::data::ability::Ability;
use crate::data::item::Item;
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
        let item_inactive = simulator_helpers::get_pokemon_at_slot(next_state, user_slot)
            .map(|m| !simulator_helpers::item_is_active(next_state, m))
            .unwrap_or(true);
        let leppa_env = simulator_helpers::berry_env(next_state, user_slot);
        if let Some(mon) = match user_slot.player {
            Player::P1 => next_state.p1_active_mons.get_mut(user_slot.slot_index as usize),
            Player::P2 => next_state.p2_active_mons.get_mut(user_slot.slot_index as usize),
        } {
            if let Some(pp) = mon.move_pp.get_mut(move_index) {
                *pp = pp.saturating_sub(1);
            }
            simulator_helpers::try_consume_leppa_berry(mon, &leppa_env);

            // Choice items: lock the holder into the first move it uses.
            // Struggle is excluded — a PP-depleted mon shouldn't be locked into Struggle.
            // If already locked, no-op (lock was set by the first use this send-in).
            let is_choice = matches!(mon.item, Item::ChoiceBand | Item::ChoiceScarf | Item::ChoiceSpecs);
            let already_locked = mon.volatiles.iter().any(|v|
                matches!(v, crate::pokemon::VolatileStatusState::TurnStatus(VolatileStatus::ChoiceLock(_), _))
            );
            if !item_inactive && is_choice && !already_locked && *move_name != PokemonMove::Struggle {
                mon.volatiles.push(crate::pokemon::VolatileStatusState::TurnStatus(
                    VolatileStatus::ChoiceLock(move_name.clone()), 0,
                ));
            }
            // Track the last move used (for Disable targeting); Struggle is excluded.
            if *move_name != PokemonMove::Struggle {
                mon.last_used_move = Some(move_name.clone());
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
        // Hurting itself in confusion means the chosen move never executed — that counts as a failed
        // move for Stomping Tantrum / Micle Berry (consistent with flinch/paralysis/sleep handling).
        if let Some(mon) = simulator_helpers::get_pokemon_at_slot_mut(&mut branch_state, user_slot) {
            mon.last_move_failed = true;
        }

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
        let raised = attacker.boosts[2] < 6;
        attacker.boosts[2] = (attacker.boosts[2] + 1).clamp(-6, 6);
        if let Some(mon) = mon_at_slot_mut(next_state, action.user_slot) {
            mon.boosts = attacker.boosts;
            if raised { mon.stats_raised_this_turn = true; }
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
    mut damage: u16,
    attack_slot: FieldSlot,
    branch_probability: f64,
    is_crit: bool,
) -> Vec<(BattleState, f64)> {
    let mut outcomes = Vec::new();

    // Disguise: an undamaged Mimikyu's disguise absorbs the damage of the first damaging
    // hit. The hit deals 0 damage; Mimikyu loses 1/8 max HP and busts to MimikyuBusted
    // (same stats/types). Secondary effects of the blocked move still apply downstream.
    // Doesn't activate when the form was copied via Transform/Imposter (pre_transform set).
    // Multi-hit moves only have their first strike blocked — the species check fails for
    // later strikes once busted.
    if damage > 0 {
        let disguise_blocks = simulator_helpers::get_pokemon_at_slot(&branch_state, target_slot)
            .map(|m| m.species == Species::Mimikyu
                && m.ability == Ability::Disguise
                && !m.fainted
                && m.pre_transform.is_none()
                && !simulator_helpers::pokemon_ability_is_suppressed(&branch_state, m))
            .unwrap_or(false);
        if disguise_blocks {
            damage = 0;
            let env = simulator_helpers::berry_env(&branch_state, target_slot);
            let mut busted_fainted = false;
            if let Some(mon) = simulator_helpers::get_pokemon_at_slot_mut(&mut branch_state, target_slot) {
                mon.species = Species::MimikyuBusted;
                let chip = (mon.stats[0] / 8).max(1);
                simulator_helpers::take_damage(mon, chip, env);
                if mon.fainted {
                    simulator_helpers::clear_pokemon_on_faint(mon);
                    busted_fainted = true;
                }
            }
            if busted_fainted {
                simulator_helpers::handle_pokemon_faint(&mut branch_state, target_slot.player, target_slot.slot_index);
            }
        }
    }
    let items_suppressed = simulator_helpers::items_are_suppressed(&branch_state);
    // Per-mon item gate for the target (Magic Room + Klutz).
    let target_item_active = simulator_helpers::get_pokemon_at_slot(&branch_state, target_slot)
        .map(|m| simulator_helpers::item_is_active(&branch_state, m))
        .unwrap_or(false);

    // Evaluate whether the target's resist berry should fire, before any mutation.
    // The number (0.5×) was already baked into `damage` by the pure damage calc;
    // here we only need to decide whether to consume the item.
    let resist_berry_consume = target_item_active && damage > 0 && {
        match (
            simulator_helpers::get_pokemon_at_slot(&branch_state, attack_slot),
            simulator_helpers::get_pokemon_at_slot(&branch_state, target_slot),
        ) {
            (Some(atk), Some(tgt)) => {
                let at = simulator_helpers::effective_move_type(&branch_state, atk, move_data);
                let eff = simulator_helpers::move_type_effectiveness(&branch_state, &at, tgt);
                simulator_helpers::resist_berry_triggers(tgt, &at, eff)
            }
            _ => false,
        }
    };

    // Type-absorption / react-on-hit abilities: absorb the hit entirely, no secondary effects,
    // no endure. Covers Volt Absorb, Water Absorb, Earth Eater, Sap Sipper, Motor Drive,
    // Flash Fire, and Dry Skin (Water).  Lightning Rod / Storm Drain are draw-in abilities
    // handled earlier (pre-accuracy gate in the per-target loop).
    let attacker_for_absorb = simulator_helpers::get_pokemon_at_slot(&branch_state, attack_slot).cloned();
    if let Some(atk) = attacker_for_absorb {
        let mut branch_state_absorb = branch_state.clone();
        if simulator_helpers::try_absorb_move(&mut branch_state_absorb, target_slot, &atk, move_data, items_suppressed) {
            outcomes.push((branch_state_absorb, branch_probability));
            return outcomes;
        }
    }

    // Focus Sash / Focus Band endure outcomes. Each entry is (eff_damage, consume_item, prob).
    // - Normal case:   one entry  (damage, false, 1.0)
    // - Focus Sash KO: one entry  (hp-1,   true,  1.0)
    // - Focus Band KO: two entries (damage, false, 0.9) and (hp-1, false, 0.1)
    // Multi-hit calls us once per hit, so Band's 10% is rolled independently each hit.
    let endure_outcomes = simulator_helpers::get_pokemon_at_slot(&branch_state, target_slot)
        .map_or_else(
            || vec![(damage, false, 1.0)],
            |t| simulator_helpers::compute_endure_outcomes(t, damage, items_suppressed),
        );

    let n = endure_outcomes.len();
    // Use Option so the last iteration can move out of branch_state without cloning.
    let mut branch_state_opt = Some(branch_state);

    for (i, (eff_damage, consume_sash, endure_prob)) in endure_outcomes.into_iter().enumerate() {
        let mut bs = if i < n - 1 {
            branch_state_opt.as_ref().unwrap().clone()
        } else {
            branch_state_opt.take().unwrap()
        };

        let mut sand_spit_triggered = false;
        let mut seed_sower_triggered = false;
        let mut target_fainted = false;
        let target_env = simulator_helpers::berry_env(&bs, target_slot);

        if let Some(target_mon) = match target_slot.player {
            Player::P1 => bs.p1_active_mons.get_mut(target_slot.slot_index as usize),
            Player::P2 => bs.p2_active_mons.get_mut(target_slot.slot_index as usize),
        } {
            simulator_helpers::take_damage(target_mon, eff_damage, target_env);

            // Per-turn damage tracking: Assurance reads `damaged_this_turn`; Avalanche
            // checks whether this specific attacker slot damaged the holder this turn.
            if eff_damage > 0 {
                target_mon.damaged_this_turn = true;
                if !target_mon.damaged_by_this_turn.contains(&attack_slot) {
                    target_mon.damaged_by_this_turn.push(attack_slot);
                }
            }

            // Focus Sash was spent to survive this hit.
            if consume_sash {
                target_mon.item = crate::data::item::Item::None;
            }

            // Air Balloon pops on any hit (use original `damage` as the "was hit" signal).
            if damage > 0 && target_item_active && matches!(target_mon.item, crate::data::item::Item::AirBalloon) {
                target_mon.item = crate::data::item::Item::None;
            }

            if resist_berry_consume {
                target_mon.item = crate::data::item::Item::None;
            }

            if target_mon.ability == Ability::SandSpit && !target_mon.fainted {
                sand_spit_triggered = true;
            }

            if target_mon.ability == Ability::SeedSower && !target_mon.fainted {
                seed_sower_triggered = true;
            }

            simulator_helpers::handle_unfreeze_on_damage(target_mon, move_name, &move_data.pokemon_type, eff_damage);

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
                target_fainted = true;
            }
        }

        if target_fainted {
            simulator_helpers::handle_pokemon_faint(&mut bs, target_slot.player, target_slot.slot_index);

            // Moxie: +1 Attack when the attacker directly KOs a target with a damaging move.
            // Only fires if the attacker is still alive (doesn't trigger on recoil-KO).
            // Stacks naturally across multi-target / multi-hit KOs.
            // (Future: Chilling Neigh / Beast Boost belong here too.)
            let items_suppressed = simulator_helpers::items_are_suppressed(&bs);
            let attacker_alive = simulator_helpers::get_pokemon_at_slot(&bs, attack_slot)
                .map(|m| !m.fainted && !simulator_helpers::pokemon_ability_is_suppressed(&bs, m)
                    && m.ability == Ability::Moxie)
                .unwrap_or(false);
            if attacker_alive {
                if let Some(atk) = simulator_helpers::get_pokemon_at_slot_mut(&mut bs, attack_slot) {
                    simulator_helpers::apply_stat_boost_external(atk, &[1, 0, 0, 0, 0, 0, 0], items_suppressed);
                }
            }
        }

        if sand_spit_triggered {
            simulator_helpers::set_weather(&mut bs, crate::dex_data::Weather::Sandstorm, 5);
        }

        if seed_sower_triggered {
            simulator_helpers::set_terrain(&mut bs, crate::dex_data::Terrain::GrassyTerrain, 5);
        }

        if matches!(move_name, PokemonMove::IceSpinner | PokemonMove::SteelRoller) {
            simulator_helpers::clear_terrain(&mut bs);
        }

        let sec_branches = simulator_helpers::apply_secondary_effects(&bs, attack_slot, target_slot, move_data);
        for (sec_bs, sec_prob) in sec_branches {
            outcomes.push((sec_bs, branch_probability * endure_prob * sec_prob));
        }
    }

    // Fire reactive-ability effects on the holder (target_slot) caused by the attacker's hit.
    outcomes = simulator_helpers::apply_contact_hit_reactions(
        outcomes, target_slot, attack_slot, move_name, move_data, damage, is_crit,
    );

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
        // Tuple: (state, probability, shared_roll, hits_landed)
        // hits_landed tracks damaging hits per branch for King's Rock combined-chance flinch.
        let mut sequence_branches: Vec<(BattleState, f64, Option<u8>, u32)> = vec![(state.clone(), hit_probability, None, 0)];

        for hit_index in 0..hit_count {
            let mut next_sequence_branches: Vec<(BattleState, f64, Option<u8>, u32)> = Vec::new();

            for (branch_state, branch_probability, shared_roll, hits_landed) in sequence_branches {
                let Some(current_target) = simulator_helpers::get_pokemon_at_slot(&branch_state, target_slot).cloned() else {
                    next_sequence_branches.push((branch_state, branch_probability, shared_roll, hits_landed));
                    continue;
                };

                if current_target.fainted {
                    next_sequence_branches.push((branch_state, branch_probability, shared_roll, hits_landed));
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
                    // Miss branch: hits_landed unchanged.
                    next_sequence_branches.push((branch_state.clone(), branch_probability * (1.0 - hit_accuracy_probability), shared_roll, hits_landed));
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

                        for (damage, is_crit, damage_probability) in hit_outcomes {
                            for (next_state, next_probability) in apply_single_hit_branch(
                                branch_state.clone(),
                                target_slot,
                                move_name,
                                move_data,
                                damage,
                                attack_slot,
                                branch_probability * hit_accuracy_probability * damage_probability * roll_probability,
                                is_crit,
                            ) {
                                // Count only damaging hits toward King's Rock combined chance.
                                let new_hits = if damage > 0 { hits_landed + 1 } else { hits_landed };
                                next_sequence_branches.push((next_state, next_probability, Some(roll), new_hits));
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

                    for (damage, is_crit, damage_probability) in hit_outcomes {
                        for (next_state, next_probability) in apply_single_hit_branch(
                            branch_state.clone(),
                            target_slot,
                            move_name,
                            move_data,
                            damage,
                            attack_slot,
                            branch_probability * hit_accuracy_probability * damage_probability,
                            is_crit,
                        ) {
                            // Count only damaging hits toward King's Rock combined chance.
                            let new_hits = if damage > 0 { hits_landed + 1 } else { hits_landed };
                            next_sequence_branches.push((next_state, next_probability, shared_roll, new_hits));
                        }
                    }
                }
            }

            sequence_branches = next_sequence_branches;
        }

        // Apply King's Rock flinch once per move per target using the combined chance
        // P(flinch) = 1 - 0.9^hits_landed, avoiding per-hit tree blowup.
        for (branch_state, branch_probability, _, hits_landed) in sequence_branches {
            let branches = simulator_helpers::apply_kings_rock_flinch(
                vec![(branch_state, branch_probability)],
                attack_slot,
                target_slot,
                move_data,
                hits_landed,
            );
            final_outcomes.extend(branches);
        }
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

/// Build the standard success outcome for a status move that has already mutated `next_state`
/// (PP already decremented). Mirrors the confusion-split bookkeeping used by Attract/Disable:
/// if the user is confused, the move's success branch carries 50% weight alongside the 50%
/// self-hit branches; otherwise it is the sole 100% outcome.
fn status_move_self_outcome(
    next_state: BattleState,
    confusion_self_hit_outcomes: &Option<Vec<(MatchState, f64)>>,
) -> Vec<(MatchState, f64)> {
    let has_confusion = confusion_self_hit_outcomes.is_some();
    let mut result: Vec<(MatchState, f64)> = Vec::new();
    if let Some(c) = confusion_self_hit_outcomes {
        for (s, p) in c { result.push((s.clone(), p * 0.5)); }
    }
    result.push((MatchState::BattleState(next_state), if has_confusion { 0.5 } else { 1.0 }));
    simulator_helpers::coalesce_branches(result)
}

fn possible_damage_outcomes_for_move(
    state: &BattleState,
    action: &MoveAction,
    move_data: &MoveData,
    config: DamageConfig,
    move_dex: &HashMap<PokemonMove, MoveData>,
    pokemon_dex: &HashMap<Species, PokemonData>,
    // True when this move was invoked by another move (Sleep Talk). Called moves do not
    // trigger Stance Change.
    is_called_move: bool,
) -> Vec<(MatchState, f64)> {
    let mut next_state = state.clone();

    let Some(mut attacker) = simulator_helpers::get_pokemon_at_slot(&next_state, action.user_slot).cloned() else {
        return vec![(MatchState::BattleState(next_state), 1.0)];
    };

    // Save pre-move state for potential failure branches (paralysis, sleep, freeze)
    let pre_move_state = next_state.clone();

    simulator_helpers::decrement_move_statuses(&mut attacker);
    write_back_volatiles(&mut next_state, action.user_slot, attacker.volatiles.clone());

    // Using any non-stalling move ends a consecutive-protect streak (the stalling handler below
    // manages the counter on its own success/fail branches).
    if !move_data.stalling_move {
        if let Some(mon) = mon_at_slot_mut(&mut next_state, action.user_slot) {
            mon.stall_counter = 0;
        }
    }

    // Check Flinch
    if attacker.volatiles.iter().any(|v| matches!(v, crate::pokemon::VolatileStatusState::TurnStatus(VolatileStatus::Flinch, _))) {
        // Find and remove charging and semi-invulnerable volatiles
        if let Some(pos) = attacker.volatiles.iter().position(|v| matches!(v, crate::pokemon::VolatileStatusState::Charging(_, _) | crate::pokemon::VolatileStatusState::MoveStatus(VolatileStatus::SemiInvulnerable(_), _))) {
            attacker.volatiles.remove(pos);
            write_back_volatiles(&mut next_state, action.user_slot, attacker.volatiles.clone());
        }
        // A move prevented by flinching counts as failed (Stomping Tantrum / Micle Berry), and a
        // flinched stalling move couldn't execute, so its protect streak resets too.
        if let Some(mon) = mon_at_slot_mut(&mut next_state, action.user_slot) {
            mon.last_move_failed = true;
            mon.stall_counter = 0;
        }
        return vec![(MatchState::BattleState(next_state), 1.0)];
    }

    // Move-restriction enforcement at execution time: a move that became illegal AFTER it was
    // selected — a faster Taunt or Throat Chop applied this turn, or a Disable from Cursed Body —
    // fails here. Mirrors flinch handling: the move is prevented, counts as failed, and consumes no
    // PP. Struggle is exempt; Torment and Encore are intentionally NOT checked here (Torment still
    // permits the move selected the turn it lands, and Encore forces a move rather than failing one).
    if action.move_name != PokemonMove::Struggle {
        let blocked_by_taunt = matches!(move_data.category, MoveCategory::Status)
            && simulator_helpers::has_status_volatile(&attacker, &VolatileStatus::Taunt);
        let blocked_by_throat_chop = simulator_helpers::move_has_flag(move_data, &crate::dex_data::MoveFlag::Sound)
            && simulator_helpers::has_status_volatile(&attacker, &VolatileStatus::ThroatChop);
        let blocked_by_disable = attacker.volatiles.iter().any(|v| matches!(
            v,
            crate::pokemon::VolatileStatusState::MoveStatus(VolatileStatus::Disable(m), _) if *m == action.move_name
        ));
        if blocked_by_taunt || blocked_by_throat_chop || blocked_by_disable {
            if let Some(mon) = mon_at_slot_mut(&mut next_state, action.user_slot) {
                mon.last_move_failed = true;
                mon.stall_counter = 0;
            }
            return vec![(MatchState::BattleState(next_state), 1.0)];
        }
    }

    // Handle charging and semi-invulnerability mechanics
    if let Some(outcomes) = handle_charging_and_semi_invulnerability(&state, &mut attacker, action, move_data, &mut next_state) {
        return outcomes;
    }

    // Check if the move has the Recharge flag
    let move_has_recharge = simulator_helpers::move_has_flag(move_data, &crate::dex_data::MoveFlag::Recharge);

    // Struggle has no moveset slot — it's forced when no usable move exists.
    // For all other moves, locate the PP slot; bail if the move isn't in the moveset.
    let pp_slot = attacker
        .moves
        .iter()
        .position(|move_entry| move_entry.as_ref() == Some(&action.move_name));

    let is_struggle = action.move_name == PokemonMove::Struggle;
    if pp_slot.is_none() && !is_struggle {
        return vec![(MatchState::BattleState(next_state), 1.0)];
    }
    let pp_index = pp_slot; // Option<usize>; None iff is_struggle

    // Verify the move still has PP (unless it's Struggle, which skips PP tracking).
    if let Some(idx) = pp_index {
        let current_pp = match action.user_slot.player {
            Player::P1 => next_state.p1_active_mons.get(action.user_slot.slot_index as usize).map(|mon| mon.move_pp[idx]).unwrap_or(0),
            Player::P2 => next_state.p2_active_mons.get(action.user_slot.slot_index as usize).map(|mon| mon.move_pp[idx]).unwrap_or(0),
        };
        if current_pp == 0 {
            // Should not be reachable — generate_commands_for_active filters 0-PP moves,
            // and choice-lock + 0-PP already routes to Struggle. Guard defensively.
            return vec![(MatchState::BattleState(next_state), 1.0)];
        }
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
        if let Some(idx) = pp_index {
            if let Some(mon) = match action.user_slot.player {
                Player::P1 => fail_state.p1_active_mons.get_mut(action.user_slot.slot_index as usize),
                Player::P2 => fail_state.p2_active_mons.get_mut(action.user_slot.slot_index as usize),
            } {
                if let Some(pp) = mon.move_pp.get_mut(idx) {
                    *pp = pp.saturating_sub(1);
                }
            }
        }
        return vec![(MatchState::BattleState(fail_state), 1.0)];
    }

    // Aura Wheel: only usable by Morpeko (either form). Any other user fails the move.
    // The +1 Speed self-boost comes from the parsed move data (self_secondaries, chance 100).
    if move_name == PokemonMove::AuraWheel {
        let is_morpeko = matches!(attacker.species, Species::Morpeko | Species::MorpekoHangry)
            || attacker.pre_transform.as_ref().map_or(false, |pre| {
                matches!(pre.species, Species::Morpeko | Species::MorpekoHangry)
            });
        if !is_morpeko {
            // Move fails: no PP cost, no boost, set last_move_failed for Stomping Tantrum.
            if let Some(mon) = simulator_helpers::get_pokemon_at_slot_mut(&mut next_state, action.user_slot) {
                mon.last_move_failed = true;
            }
            return vec![(MatchState::BattleState(next_state), 1.0)];
        }
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

    // Stance Change: Aegislash changes forme just before successfully executing a move —
    // sleep / freeze / flinch checks have all passed at this point, and the form changes
    // even if the move then misses. The confusion self-hit branch was cloned above, so it
    // correctly keeps the pre-change forme. Called moves (Sleep Talk) never trigger this.
    if !is_called_move
        && attacker.ability == Ability::StanceChange
        && !simulator_helpers::pokemon_ability_is_suppressed(&next_state, &attacker)
    {
        let new_form = if attacker.species == Species::Aegislash
            && !matches!(move_data.category, MoveCategory::Status)
        {
            Some(Species::AegislashBlade)
        } else if attacker.species == Species::AegislashBlade
            && move_name == PokemonMove::KingsShield
        {
            Some(Species::Aegislash)
        } else {
            None
        };
        if let Some(form) = new_form {
            if let Some(mon) = simulator_helpers::get_pokemon_at_slot_mut(&mut next_state, action.user_slot) {
                crate::battle::change_form(mon, form.clone(), pokemon_dex);
            }
            // Keep the local attacker copy in sync — this move's damage must already
            // use the new forme's stats.
            crate::battle::change_form(&mut attacker, form, pokemon_dex);
        }
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
                let branches = possible_damage_outcomes_for_move(&sleep_talk_state, &new_action, cand_data, config, move_dex, pokemon_dex, true);
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

    // Stalling protect-family moves (Protect/Detect/Endure/King's Shield/Spiky Shield/Baneful
    // Bunker). The shared "stall" counter makes consecutive uses succeed with probability
    // 1/3^streak; the roll happens here. These moves are +4 priority, so they resolve before the
    // attacks they block — once the volatile is set, blocking is deterministic for the turn.
    // Resolving here (before target/effect processing) avoids double-adding the volatile via the
    // generic self_secondary path.
    if move_data.stalling_move {
        if let Some(vol) = simulator_helpers::protect_volatile_for_move(&move_name) {
            let counter = simulator_helpers::get_pokemon_at_slot(&next_state, action.user_slot)
                .map(|m| m.stall_counter)
                .unwrap_or(0);
            let p_success = 1.0 / 3f64.powi(counter as i32);

            let mut result: Vec<(MatchState, f64)> = Vec::new();

            // Success: set the protect volatile and grow the streak.
            let mut succ = next_state.clone();
            decrement_move_pp(&mut succ, action.user_slot, &action.move_name);
            if let Some(mon) = mon_at_slot_mut(&mut succ, action.user_slot) {
                mon.volatiles.push(crate::pokemon::VolatileStatusState::TurnStatus(vol.clone(), 1));
                mon.stall_counter = mon.stall_counter.saturating_add(1);
                mon.last_move_failed = false;
            }
            result.push((MatchState::BattleState(succ), p_success));

            // Failure (only possible once the streak has decayed): the move does nothing and the
            // streak resets.
            if p_success < 1.0 {
                let mut fail = next_state.clone();
                decrement_move_pp(&mut fail, action.user_slot, &action.move_name);
                if let Some(mon) = mon_at_slot_mut(&mut fail, action.user_slot) {
                    mon.stall_counter = 0;
                    mon.last_move_failed = true;
                }
                result.push((MatchState::BattleState(fail), 1.0 - p_success));
            }

            // Fold in the confusion self-hit branches, mirroring Attract/Disable.
            if let Some(c) = &confusion_self_hit_outcomes {
                let mut folded: Vec<(MatchState, f64)> =
                    c.iter().map(|(s, p)| (s.clone(), p * 0.5)).collect();
                folded.extend(result.into_iter().map(|(s, p)| (s, p * 0.5)));
                return simulator_helpers::coalesce_branches(folded);
            }
            return simulator_helpers::coalesce_branches(result);
        }
        // Out-of-scope stalling move (e.g. Max Guard) — fall through to generic handling.
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

    // Apply Follow Me / Rage Powder / Lightning Rod / Storm Drain redirection for single-target moves
    if !simulator_helpers::move_target_is_multitarget(&move_data.target)
        && !(move_name == PokemonMove::ExpandingForce && simulator_helpers::pokemon_is_on_terrain(&next_state, &attacker, &crate::dex_data::Terrain::PsychicTerrain))
    {
        target_slots = simulator_helpers::check_and_apply_redirection(&next_state, action.user_slot, target_slots, Some(move_data));
    }

    // Life Dew: restore 1/4 max HP to the user and every ally currently in battle. Heal Block
    // suppresses the heal per-recipient. Targets "allies" in the data — which excludes the user
    // and resolves to an empty target list in singles — so it is handled here before the
    // empty-target early return below.
    if move_name == PokemonMove::LifeDew {
        let player = action.user_slot.player;
        let slot_count = match player {
            Player::P1 => next_state.p1_active_mons.len(),
            Player::P2 => next_state.p2_active_mons.len(),
        };
        let envs: Vec<_> = (0..slot_count)
            .map(|i| simulator_helpers::berry_env(&next_state, FieldSlot { player, slot_index: i as u8 }))
            .collect();
        let actives = match player {
            Player::P1 => &mut next_state.p1_active_mons,
            Player::P2 => &mut next_state.p2_active_mons,
        };
        for (i, mon) in actives.iter_mut().enumerate() {
            if mon.fainted || simulator_helpers::heal_is_blocked(mon) { continue; }
            let heal = (mon.stats[0].max(1) as u32 / 4) as u16;
            simulator_helpers::gain_hp(mon, heal, envs[i]);
        }
        decrement_move_pp(&mut next_state, action.user_slot, &action.move_name);
        return status_move_self_outcome(next_state, &confusion_self_hit_outcomes);
    }

    if target_slots.is_empty() {
        return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
    }

    // Attract move: apply infatuation to the target.
    if move_name == PokemonMove::Attract {
        let target_slot = target_slots[0];
        let applied = simulator_helpers::try_apply_attract(&mut next_state, action.user_slot, target_slot);
        if applied {
            decrement_move_pp(&mut next_state, action.user_slot, &action.move_name);
            let has_confusion = confusion_self_hit_outcomes.is_some();
            let mut result = Vec::new();
            if let Some(ref c) = confusion_self_hit_outcomes {
                for (s, p) in c { result.push((s.clone(), p * 0.5)); }
            }
            result.push((MatchState::BattleState(next_state), if has_confusion { 0.5 } else { 1.0 }));
            return simulator_helpers::coalesce_branches(result);
        }
        return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
    }

    // Disable move: disable the target's last-used move for 4 turns.
    if move_name == PokemonMove::Disable {
        let target_slot = target_slots[0];
        let applied = simulator_helpers::try_apply_disable(&mut next_state, action.user_slot, target_slot);
        if applied {
            decrement_move_pp(&mut next_state, action.user_slot, &action.move_name);
            let has_confusion = confusion_self_hit_outcomes.is_some();
            let mut result = Vec::new();
            if let Some(ref c) = confusion_self_hit_outcomes {
                for (s, p) in c { result.push((s.clone(), p * 0.5)); }
            }
            result.push((MatchState::BattleState(next_state), if has_confusion { 0.5 } else { 1.0 }));
            return simulator_helpers::coalesce_branches(result);
        }
        return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
    }

    // Encore: lock the target into repeating its last move for 3 turns. If the target still has a
    // pending move action this turn (it acts after the Encore user), rewrite that queued action to
    // the encored move so it is forced THIS turn.
    if move_name == PokemonMove::Encore {
        let target_slot = target_slots[0];
        match simulator_helpers::try_apply_encore(&mut next_state, action.user_slot, target_slot) {
            Some(encored_move) => {
                // Mid-turn replacement: rewrite the target's still-queued MoveAction.
                let new_priority = move_dex.get(&encored_move).map(|d| d.priority).unwrap_or(0);
                for queued in next_state.action_queue.iter_mut() {
                    if let Action::MoveAction(ma) = queued {
                        if ma.user_slot == target_slot {
                            ma.move_name = encored_move.clone();
                            ma.priority = new_priority;
                        }
                    }
                }
                decrement_move_pp(&mut next_state, action.user_slot, &action.move_name);
                return status_move_self_outcome(next_state, &confusion_self_hit_outcomes);
            }
            None => return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes),
        }
    }

    // Heal Bell: a sound-based move that cures the status of the user, its entire party
    // (including reserves) and any allies. The user is cured even if it has Soundproof; other
    // active allies with Soundproof are not. Reserves are cured regardless of their ability.
    if move_name == PokemonMove::HealBell {
        let player = action.user_slot.player;
        let user_idx = action.user_slot.slot_index as usize;
        let abilities_suppressed = simulator_helpers::abilities_are_suppressed(&next_state);
        let (actives, backs) = match player {
            Player::P1 => (&mut next_state.p1_active_mons, &mut next_state.p1_back_mons),
            Player::P2 => (&mut next_state.p2_active_mons, &mut next_state.p2_back_mons),
        };
        for (i, mon) in actives.iter_mut().enumerate() {
            if mon.fainted { continue; }
            let soundproof = !abilities_suppressed && mon.ability == Ability::Soundproof;
            if i == user_idx || !soundproof {
                mon.status = None;
            }
        }
        for mon in backs.iter_mut() {
            if !mon.fainted { mon.status = None; }
        }
        decrement_move_pp(&mut next_state, action.user_slot, &action.move_name);
        return status_move_self_outcome(next_state, &confusion_self_hit_outcomes);
    }

    // Heal Pulse: restore the target's HP by 1/2 (3/4 with Mega Launcher). Fails if the target
    // is already at full HP or behind a Substitute, or if the user or target is under Heal Block.
    if move_name == PokemonMove::HealPulse {
        let target_slot = target_slots[0];
        let abilities_suppressed = simulator_helpers::abilities_are_suppressed(&next_state);
        let user = simulator_helpers::get_pokemon_at_slot(&next_state, action.user_slot);
        let user_hb = user.map(simulator_helpers::heal_is_blocked).unwrap_or(false);
        let mega_launcher = user
            .map(|m| !abilities_suppressed && m.ability == Ability::MegaLauncher)
            .unwrap_or(false);
        let target_env = simulator_helpers::berry_env(&next_state, target_slot);
        let mut heal: Option<u16> = None;
        if let Some(target) = simulator_helpers::get_pokemon_at_slot(&next_state, target_slot) {
            let max_hp = target.stats[0].max(1);
            let full = target.hp >= max_hp;
            let sub = simulator_helpers::has_status_volatile(target, &VolatileStatus::Substitute);
            if !user_hb && !simulator_helpers::heal_is_blocked(target) && !full && !sub {
                heal = Some(if mega_launcher {
                    (max_hp as u32 * 3 / 4) as u16
                } else {
                    (max_hp as u32 / 2) as u16
                });
            }
        }
        match heal {
            Some(amount) => {
                if let Some(t) = mon_at_slot_mut(&mut next_state, target_slot) {
                    simulator_helpers::gain_hp(t, amount, target_env);
                }
                decrement_move_pp(&mut next_state, action.user_slot, &action.move_name);
                return status_move_self_outcome(next_state, &confusion_self_hit_outcomes);
            }
            None => return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes),
        }
    }

    // Pain Split: average the user's and target's current HP, capped at each one's max. Fails if
    // the target is behind a Substitute. Sets HP directly (ignores type effectiveness; not subject
    // to Counter/Bide) and is NOT prevented by Heal Block.
    if move_name == PokemonMove::PainSplit {
        let target_slot = target_slots[0];
        let sub = simulator_helpers::get_pokemon_at_slot(&next_state, target_slot)
            .map(|m| simulator_helpers::has_status_volatile(m, &VolatileStatus::Substitute))
            .unwrap_or(false);
        if sub {
            return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
        }
        let user_hp = simulator_helpers::get_pokemon_at_slot(&next_state, action.user_slot)
            .map(|m| m.hp).unwrap_or(0);
        let target_hp = simulator_helpers::get_pokemon_at_slot(&next_state, target_slot)
            .map(|m| m.hp).unwrap_or(0);
        let avg = (((user_hp as u32 + target_hp as u32) / 2).max(1)) as u16;
        if let Some(u) = mon_at_slot_mut(&mut next_state, action.user_slot) {
            u.hp = avg.min(u.stats[0]);
        }
        if let Some(t) = mon_at_slot_mut(&mut next_state, target_slot) {
            t.hp = avg.min(t.stats[0]);
        }
        decrement_move_pp(&mut next_state, action.user_slot, &action.move_name);
        return status_move_self_outcome(next_state, &confusion_self_hit_outcomes);
    }

    // Strength Sap: heal the user by the target's effective Attack stat (Big Root ×1.3), then
    // lower the target's Attack by 1. Fails only if the target's Attack is already at -6 (it still
    // succeeds and heals when the drop is a no-op, e.g. Clear Body). Liquid Ooze on the target
    // turns the heal into damage to the user; Heal Block prevents only the heal.
    if move_name == PokemonMove::StrengthSap {
        let target_slot = target_slots[0];
        let target = simulator_helpers::get_pokemon_at_slot(&next_state, target_slot);
        let at_min = target.map(|m| m.boosts[0] <= -6).unwrap_or(true);
        if at_min {
            return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
        }
        let abilities_suppressed = simulator_helpers::abilities_are_suppressed(&next_state);
        let items_suppressed = simulator_helpers::items_are_suppressed(&next_state);
        let target_ref = target.expect("checked above");
        let target_atk = simulator_helpers::effective_stat(
            &next_state, target_ref, crate::dex_data::PokemonStat::Atk, false, false,
        ).round().max(1.0) as u16;
        let liquid_ooze = !abilities_suppressed && target_ref.ability == Ability::LiquidOoze;
        let user_env = simulator_helpers::berry_env(&next_state, action.user_slot);
        if let Some(user) = mon_at_slot_mut(&mut next_state, action.user_slot) {
            let amount = simulator_helpers::apply_big_root(user, target_atk, items_suppressed);
            if liquid_ooze {
                simulator_helpers::take_damage(user, amount, user_env);
            } else if !simulator_helpers::heal_is_blocked(user) {
                simulator_helpers::gain_hp(user, amount, user_env);
            }
        }
        simulator_helpers::apply_opponent_stat_drop(
            &mut next_state, target_slot, action.user_slot, [-1, 0, 0, 0, 0, 0, 0], items_suppressed, false,
        );
        decrement_move_pp(&mut next_state, action.user_slot, &action.move_name);
        return status_move_self_outcome(next_state, &confusion_self_hit_outcomes);
    }

    // Wish: queue a delayed heal of half the user's max HP on the user's slot, resolved at the
    // end of the NEXT turn to whoever occupies that slot. Fails if a Wish is already pending for
    // the slot or the user is prevented from healing by Heal Block.
    if move_name == PokemonMove::Wish {
        let slot_idx = action.user_slot.slot_index as usize;
        let user = simulator_helpers::get_pokemon_at_slot(&next_state, action.user_slot);
        let user_max_hp = user.map(|m| m.stats[0].max(1)).unwrap_or(1);
        let heal_blocked = user.map(simulator_helpers::heal_is_blocked).unwrap_or(false);
        let conds = match action.user_slot.player {
            Player::P1 => &mut next_state.p1_slot_conditions,
            Player::P2 => &mut next_state.p2_slot_conditions,
        };
        let already_pending = conds
            .get(slot_idx)
            .map(|c| c.iter().any(|sc| matches!(sc, crate::dex_data::SlotCondition::Wish { .. })))
            .unwrap_or(false);
        if heal_blocked || already_pending {
            return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
        }
        if let Some(slot_conds) = conds.get_mut(slot_idx) {
            slot_conds.push(crate::dex_data::SlotCondition::Wish {
                heal: (user_max_hp / 2).max(1),
                turns_remaining: 2,
            });
        }
        decrement_move_pp(&mut next_state, action.user_slot, &action.move_name);
        let has_confusion = confusion_self_hit_outcomes.is_some();
        let mut result = Vec::new();
        if let Some(ref c) = confusion_self_hit_outcomes {
            for (s, p) in c { result.push((s.clone(), p * 0.5)); }
        }
        result.push((MatchState::BattleState(next_state), if has_confusion { 0.5 } else { 1.0 }));
        return simulator_helpers::coalesce_branches(result);
    }

    // Perish Song: sound-based field move — applies PerishSong volatile (counter=4) to every
    // active Pokémon. Soundproof protects other Pokémon (not the user). Skips mons that
    // already have PerishSong. Always hits (no accuracy check).
    if move_name == PokemonMove::PerishSong {
        let abilities_suppressed = simulator_helpers::abilities_are_suppressed(&next_state);
        let user_slot_idx = action.user_slot.slot_index as usize;
        let user_player = action.user_slot.player;
        let all_slots: Vec<(Player, usize)> = {
            let p1_slots: Vec<_> = (0..next_state.p1_active_mons.len()).map(|i| (Player::P1, i)).collect();
            let p2_slots: Vec<_> = (0..next_state.p2_active_mons.len()).map(|i| (Player::P2, i)).collect();
            p1_slots.into_iter().chain(p2_slots).collect()
        };
        let state_snapshot = next_state.clone();
        for (player, idx) in all_slots {
            let is_user = player == user_player && idx == user_slot_idx;
            let mons = match player {
                Player::P1 => &mut next_state.p1_active_mons,
                Player::P2 => &mut next_state.p2_active_mons,
            };
            let Some(mon) = mons.get_mut(idx) else { continue };
            if mon.fainted { continue; }
            // Soundproof blocks Perish Song for non-users.
            if !is_user && !abilities_suppressed && mon.ability == Ability::Soundproof { continue; }
            // Skip if already afflicted.
            if simulator_helpers::has_status_volatile(mon, &VolatileStatus::PerishSong) { continue; }
            simulator_helpers::apply_volatile_to_pokemon_pub(&state_snapshot, mon, &VolatileStatus::PerishSong);
        }
        decrement_move_pp(&mut next_state, action.user_slot, &action.move_name);
        return status_move_self_outcome(next_state, &confusion_self_hit_outcomes);
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

        let all_outcomes: Vec<(MatchState, f64)> = resolve_multihit_move_for_target(
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

    // Attract: 50% chance to fail to act when infatuated.
    let attract_fail_prob: f64 = if simulator_helpers::has_status_volatile(&attacker, &VolatileStatus::Attract)
        && attacker.ability != Ability::Oblivious
    {
        0.50
    } else {
        0.0
    };

    // Damp: any active Pokémon on either side with unsuppressed Damp causes explosive
    // moves to fail entirely. The user does NOT faint or take damage.
    // Blast Burn / Powder / Pollen Puff / Shell Trap are NOT explosive and are unaffected.
    // Mold Breaker: TODO
    let is_explosive_move = matches!(
        move_name,
        PokemonMove::SelfDestruct
            | PokemonMove::Explosion
            | PokemonMove::MindBlown
            | PokemonMove::MistyExplosion
    );
    if is_explosive_move {
        let damp_on_field = next_state
            .p1_active_mons
            .iter()
            .chain(next_state.p2_active_mons.iter())
            .filter(|mon| !mon.fainted)
            .any(|mon| {
                !simulator_helpers::pokemon_ability_is_suppressed(&next_state, mon)
                    && mon.ability == Ability::Damp
            });
        if damp_on_field {
            return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
        }
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

        // Queenly Majesty / Armor Tail / Dazzling: block any move with increased effective
        // priority (after Prankster / Gale Wings boosts) from an opposing mon.
        // Bypassed by Mold Breaker (TODO: implement when Mold Breaker is added).
        // Exception: spread/field-targeting moves are not blocked (TODO for doubles).
        let effective_priority = simulator_helpers::effective_move_priority(&next_state, &attacker, move_data);
        let target_has_priority_block_ability =
            !simulator_helpers::pokemon_ability_is_suppressed(&next_state, &target)
            && matches!(target.ability, Ability::QueenlyMajesty | Ability::ArmorTail | Ability::Dazzling);
        if effective_priority > 0
            && action.user_slot.player != target_slot.player
            && target_has_priority_block_ability
        {
            outcomes_for_target.push((0, false, false, 1.0));
            per_target_outcomes.push((*target_slot, outcomes_for_target));
            continue;
        }

        // Prankster — Dark-type immunity (Gen VII+): opposing Dark-type targets are
        // immune to moves that gained priority from Prankster. Ally Dark-types are
        // unaffected. Mold Breaker does NOT bypass this immunity.
        if attacker.ability == Ability::Prankster
            && !simulator_helpers::pokemon_ability_is_suppressed(&next_state, &attacker)
            && matches!(move_data.category, MoveCategory::Status)
            && action.user_slot.player != target_slot.player
            && simulator_helpers::pokemon_has_type(&target, &PokemonType::Dark)
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

        // Bulletproof: immune to all ball and bomb moves (MoveFlag::Bullet).
        // Blocks even an ally's Pollen Puff — no ally exemption.
        // Mold Breaker: TODO
        if simulator_helpers::move_has_flag(move_data, &crate::dex_data::MoveFlag::Bullet)
            && !simulator_helpers::pokemon_ability_is_suppressed(&next_state, &target)
            && target.ability == Ability::Bulletproof
        {
            outcomes_for_target.push((0, false, false, 1.0));
            per_target_outcomes.push((*target_slot, outcomes_for_target));
            continue;
        }

        // Soundproof: immune to sound-based moves (MoveFlag::Sound).
        // The holder is NOT immune to its own sound moves (Gen VIII+ / Champions behaviour).
        // Mold Breaker: TODO
        if simulator_helpers::move_has_flag(move_data, &crate::dex_data::MoveFlag::Sound)
            && action.user_slot != *target_slot
            && !simulator_helpers::pokemon_ability_is_suppressed(&next_state, &target)
            && target.ability == Ability::Soundproof
        {
            outcomes_for_target.push((0, false, false, 1.0));
            per_target_outcomes.push((*target_slot, outcomes_for_target));
            continue;
        }

        // Overcoat: immune to powder/spore moves (MoveFlag::Powder).
        // is_immune_to_powder also covers Grass-type and Safety Goggles, which is correct
        // for a general powder-immunity gate (mirrors the Rage Powder redirect logic).
        // Weather-damage immunity is handled separately in apply_weather_residual.
        // Mold Breaker: TODO
        if simulator_helpers::move_has_flag(move_data, &crate::dex_data::MoveFlag::Powder)
            && simulator_helpers::is_immune_to_powder(&next_state, &target)
        {
            outcomes_for_target.push((0, false, false, 1.0));
            per_target_outcomes.push((*target_slot, outcomes_for_target));
            continue;
        }

        // Telepathy: holder dodges damaging moves used by its own allies (doubles/triples).
        // Ally status moves are NOT dodged. No effect in singles (no ally slots exist).
        // Mold Breaker does not bypass Telepathy.
        if action.user_slot.player == target_slot.player
            && action.user_slot.slot_index != target_slot.slot_index
            && matches!(
                move_data.category,
                MoveCategory::Physical | MoveCategory::Special
            )
            && !simulator_helpers::pokemon_ability_is_suppressed(&next_state, &target)
            && target.ability == Ability::Telepathy
        {
            outcomes_for_target.push((0, false, false, 1.0));
            per_target_outcomes.push((*target_slot, outcomes_for_target));
            continue;
        }

        // Protect-family blocking: self-protect volatiles (Protect/Detect/King's Shield/Spiky
        // Shield/Baneful Bunker) and the Quick Guard / Wide Guard side conditions. The stall-success
        // roll already happened when the protection was raised (those moves are +4/+3 priority), so
        // blocking here is deterministic.
        let is_spread = simulator_helpers::move_is_spread_target(&move_data.target);
        if let Some(kind) = simulator_helpers::protect_blocks_move(
            &next_state, action.user_slot, *target_slot, &target, move_data, is_spread,
        ) {
            // Punish a blocked CONTACT move (Spiky Shield chip / Baneful Bunker poison / King's
            // Shield −1 Atk). Deterministic, so applied once to the shared next_state per blocked
            // target. TODO: Long Reach / Protective Pads should make these moves non-contact.
            if simulator_helpers::move_has_flag(move_data, &crate::dex_data::MoveFlag::Contact) {
                simulator_helpers::apply_protect_contact_punishment(
                    &mut next_state, action.user_slot, *target_slot, kind,
                );
            }
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

        // Lightning Rod / Storm Drain draw-in: negate the move and apply +1 Sp. Atk to the
        // target BEFORE the accuracy roll.  The ability fires even on a miss or through Protect.
        // `try_drawin_negate` also handles the case where the target was redirected to this slot.
        {
            let items_suppressed = simulator_helpers::items_are_suppressed(&next_state);
            if simulator_helpers::try_drawin_negate(&mut next_state, *target_slot, &attacker, move_data, items_suppressed) {
                outcomes_for_target.push((0, false, false, 1.0));
                per_target_outcomes.push((*target_slot, outcomes_for_target));
                continue;
            }
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
        let pp_display = pp_index
            .and_then(|idx| next_state.p1_active_mons.get(action.user_slot.slot_index as usize)
                .or_else(|| next_state.p2_active_mons.get(action.user_slot.slot_index as usize))
                .and_then(|mon| mon.move_pp.get(idx)))
            .map(|pp| pp.to_string())
            .unwrap_or_else(|| "—".to_string());
        println!(
            "{}",
            format!(
                "{} uses {} | targets: {} | move type: {} | PP: {}",
                simulator_helpers::species_name_sim(&attacker.species),
                simulator_helpers::move_name_sim(&move_name),
                target_names.join(", "),
                simulator_helpers::pokemon_type_name(&move_data.pokemon_type),
                pp_display,
            )
            .bright_cyan()
        );
    }

    // Combine per-target outcomes via cartesian product
    let mut all_outcomes: Vec<(MatchState, f64)> = vec![(MatchState::BattleState(next_state.clone()), 1.0)];

    for (target_slot, target_outcomes) in &per_target_outcomes {
        let mut new_all_outcomes = Vec::new();

        for (existing_state, existing_prob) in all_outcomes {
            for (damage, is_crit, hit, outcome_prob) in target_outcomes {
                let branch_state = match existing_state.clone() {
                    MatchState::BattleState(bs) => bs,
                    _ => continue,
                };
                let combined_prob = existing_prob * outcome_prob;

                if *hit {
                    // Delegate to the already-extracted single-hit helper.
                    let hit_branches = apply_single_hit_branch(branch_state, *target_slot, &move_name, move_data, *damage, action.user_slot, combined_prob, *is_crit);
                    // King's Rock: 10% flinch on damaging hits (combined chance = 1 - 0.9^hits).
                    let hit_branches = if *damage > 0 {
                        simulator_helpers::apply_kings_rock_flinch(hit_branches, action.user_slot, *target_slot, move_data, 1)
                    } else {
                        hit_branches
                    };
                    // Phazing moves (Roar/Whirlwind/Dragon Tail/Circle Throw): on a connecting hit,
                    // force the target to switch to a random eligible bench mon. Gate on damage>0 for
                    // damaging phazers (a type-immune Dragon Tail deals 0 → no switch) while letting
                    // the no-damage status phazers through.
                    let hit_branches = if move_data.force_switch
                        && (*damage > 0 || matches!(move_data.category, MoveCategory::Status))
                    {
                        apply_forced_switch(hit_branches, *target_slot, move_data, pokemon_dex)
                    } else {
                        hit_branches
                    };
                    for (bs, prob) in hit_branches {
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

    // Status moves: set `last_move_failed` (Stomping Tantrum / Micle Berry) via a state diff against
    // the pre-move baseline — a status move "failed" if it changed nothing battle-meaningful. This
    // also covers a phazing status move (Roar/Whirlwind) that found no legal switch target. Damaging
    // moves are handled separately via `total_dmg == 0` in `apply_post_damage_move_effects`. Genuine
    // no-op successes (Splash/Celebrate/Hold Hands) are whitelisted so they don't mis-flag.
    if matches!(move_data.category, MoveCategory::Status) {
        let always_ok = status_move_always_succeeds(&move_data.name);
        for (state, _) in &mut all_outcomes {
            if let MatchState::BattleState(bs) = state {
                let failed = !always_ok && !status_move_changed_state(&next_state, bs);
                if let Some(mon) = mon_at_slot_mut(bs, action.user_slot) {
                    mon.last_move_failed = failed;
                }
            }
        }
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
            decrement_move_pp(bs, action.user_slot, &action.move_name);
        }
    }

    // Handle failure branches for paralysis / sleep / freeze (these consume PP but do nothing)
    let mut final_outcomes: Vec<(MatchState, f64)> = Vec::new();
    // paralysis fail branch
    if par_fail_prob > 0.0 {
        let mut fail_state = pre_move_state.clone();
        if let Some(idx) = pp_index {
            if let Some(mon) = match action.user_slot.player { Player::P1 => fail_state.p1_active_mons.get_mut(action.user_slot.slot_index as usize), Player::P2 => fail_state.p2_active_mons.get_mut(action.user_slot.slot_index as usize) } {
                if let Some(pp) = mon.move_pp.get_mut(idx) {
                    *pp = pp.saturating_sub(1);
                }
            }
        }
        // A move prevented by full paralysis counts as failed (Stomping Tantrum / Micle Berry).
        if let Some(mon) = mon_at_slot_mut(&mut fail_state, action.user_slot) {
            mon.last_move_failed = true;
        }
        final_outcomes.push((MatchState::BattleState(fail_state), par_fail_prob));
    }

    // status (sleep/frozen) fail branch: increment counters and consume PP
    if status_fail_prob > 0.0 {
        let mut status_fail_state = pre_move_state.clone();
        if let Some(mon) = match action.user_slot.player { Player::P1 => status_fail_state.p1_active_mons.get_mut(action.user_slot.slot_index as usize), Player::P2 => status_fail_state.p2_active_mons.get_mut(action.user_slot.slot_index as usize) } {
            // Decrement PP (Struggle has no PP slot to track)
            if let Some(idx) = pp_index {
                if let Some(pp) = mon.move_pp.get_mut(idx) {
                    *pp = pp.saturating_sub(1);
                }
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

            // A move prevented by sleep/freeze counts as failed (Stomping Tantrum / Micle Berry).
            mon.last_move_failed = true;
        }
        final_outcomes.push((MatchState::BattleState(status_fail_state), status_fail_prob));
    }

    // Attract fail branch (infatuated; consumes PP, does nothing)
    if attract_fail_prob > 0.0 {
        let mut fail_state = pre_move_state.clone();
        if let Some(idx) = pp_index {
            if let Some(mon) = match action.user_slot.player { Player::P1 => fail_state.p1_active_mons.get_mut(action.user_slot.slot_index as usize), Player::P2 => fail_state.p2_active_mons.get_mut(action.user_slot.slot_index as usize) } {
                if let Some(pp) = mon.move_pp.get_mut(idx) { *pp = pp.saturating_sub(1); }
            }
        }
        // A move prevented by infatuation counts as failed (Stomping Tantrum / Micle Berry).
        if let Some(mon) = mon_at_slot_mut(&mut fail_state, action.user_slot) {
            mon.last_move_failed = true;
        }
        final_outcomes.push((MatchState::BattleState(fail_state), attract_fail_prob));
    }

    // Scale normal outcomes by success probability (1 - combined_fail_prob)
    let combined_fail_prob = par_fail_prob + status_fail_prob + attract_fail_prob;
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
    let p1_mons = parse_team_sheet(p1_path, pokemon_dex, move_dex, use_stat_points);
    let p1_size = p1_mons.len() as u8;
    let mut p2_mons = parse_team_sheet(p2_path, pokemon_dex, move_dex, use_stat_points);
    // Offset P2's mon_ids so they are globally unique across both teams (P1 = 0..n, P2 = n..2n).
    // This lets a single u8 identify any Pokémon on the field, which is needed to track the
    // source of binding/trapping volatiles (ends when the trapper leaves the field).
    for mon in &mut p2_mons { mon.mon_id += p1_size; }
    TeamPreviewState {
        active_per_side,
        brought_per_side,
        p1_mons,
        p2_mons,
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

    // Mid-turn self-switch: the pending slot may only switch to a healthy bench mon;
    // every other active slot must Pass.
    if let Some((pending_slot, _)) = state.self_switch_pending {
        let this_slot = FieldSlot { player, slot_index: slot_idx as u8 };
        if this_slot == pending_slot {
            return my_back.iter().enumerate()
                .filter(|(_, m)| !m.fainted)
                .map(|(i, _)| BattleCommand::Switch(SwitchCommand { party_index: i }))
                .collect();
        } else {
            return vec![BattleCommand::Pass];
        }
    }

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

    // Normal turn: switches first (suppressed while the mon is trapped).
    let trapped = simulator_helpers::is_trapped(state, mon);
    let mut cmds: Vec<BattleCommand> = if !trapped {
        my_back.iter().enumerate()
            .filter(|(_, m)| !m.fainted)
            .map(|(i, _)| BattleCommand::Switch(SwitchCommand { party_index: i }))
            .collect()
    } else {
        vec![]
    };

    if mon.fainted { return cmds; }

    let can_tera = !has_tera && !mon.is_tera;
    let can_mega = has_mega && mon.has_mega_form && {
        let item_ok = mon.mega_species.as_ref()
            .and_then(|sp| pokemon_dex.get(sp))
            .and_then(|data| data.required_item.as_ref())
            .map_or(true, |req| format!("{:?}", mon.item).to_lowercase() == *req);
        item_ok
    };

    // Determine the choice-locked move (if any).
    let choice_locked_move: Option<PokemonMove> = mon.volatiles.iter().find_map(|v| {
        if let crate::pokemon::VolatileStatusState::TurnStatus(VolatileStatus::ChoiceLock(m), _) = v {
            Some(m.clone())
        } else {
            None
        }
    });

    // Move-restriction volatiles. Encore forces a single move (unless that move has run out of PP,
    // in which case Encore has effectively ended); Taunt blocks status moves; Throat Chop blocks
    // sound moves; Torment blocks repeating the last move used.
    let encored_move: Option<PokemonMove> = mon.volatiles.iter().find_map(|v| {
        if let crate::pokemon::VolatileStatusState::MoveStatus(VolatileStatus::Encore(m), _) = v {
            Some(m.clone())
        } else {
            None
        }
    }).filter(|m| {
        mon.moves.iter().zip(mon.move_pp.iter()).any(|(slot, pp)| slot.as_ref() == Some(m) && *pp > 0)
    });
    let taunted = simulator_helpers::has_status_volatile(mon, &VolatileStatus::Taunt);
    let throat_chopped = simulator_helpers::has_status_volatile(mon, &VolatileStatus::ThroatChop);
    let tormented = simulator_helpers::has_status_volatile(mon, &VolatileStatus::Torment);
    // Uproar: while active, the user must keep using Uproar.
    let uproar_locked = simulator_helpers::has_status_volatile(mon, &VolatileStatus::Uproar);

    // Attacks: filter by choice-lock and 0-PP, then fall back to Struggle.
    let mut emitted_attack = false;
    for (i, move_name_opt) in mon.moves.iter().enumerate() {
        let Some(move_name) = move_name_opt else { continue; };

        // Uproar lock: only Uproar is selectable while the volatile is active.
        if uproar_locked && *move_name != PokemonMove::Uproar { continue; }

        // Choice lock: only the locked move is selectable.
        if let Some(ref locked) = choice_locked_move {
            if move_name != locked { continue; }
        }

        // Moves with 0 PP are not selectable.
        if mon.move_pp.get(i).copied().unwrap_or(0) == 0 { continue; }

        // Disabled moves are not selectable (Disable volatile carries the blocked move name).
        let is_disabled = mon.volatiles.iter().any(|v| matches!(
            v,
            crate::pokemon::VolatileStatusState::MoveStatus(VolatileStatus::Disable(m), _) if m == move_name
        ));
        if is_disabled { continue; }

        // Encore: only the encored move is selectable.
        if let Some(ref enc) = encored_move {
            if move_name != enc { continue; }
        }

        // Taunt: status moves cannot be selected.
        if taunted && move_dex.get(move_name).map_or(false, |d| matches!(d.category, MoveCategory::Status)) {
            continue;
        }

        // Throat Chop: sound-based moves cannot be selected.
        if throat_chopped
            && move_dex.get(move_name).map_or(false, |d| simulator_helpers::move_has_flag(d, &crate::dex_data::MoveFlag::Sound))
        {
            continue;
        }

        // Torment: the same move cannot be used twice in a row.
        if tormented && mon.last_used_move.as_ref() == Some(move_name) { continue; }

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
            emitted_attack = true;
        }
    }

    // If no attack could be emitted (all PP exhausted, or locked move has no PP),
    // force Struggle. Struggle targets a random opponent (Normal targeting).
    if !emitted_attack {
        for target in get_valid_targets(&MoveTarget::Normal, player, state, slot_idx) {
            cmds.push(BattleCommand::Struggle { target });
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

                // Store raw base priority only. Dynamic boosts (Grassy Glide, Prankster,
                // Gale Wings) are computed at compare time via effective_move_priority so
                // that mid-turn HP changes (e.g. Fake Out) are reflected correctly.
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
                    moves_first: false,
                }));
            }
            BattleCommand::Struggle { target } => {
                action_queue.push(Action::MoveAction(MoveAction {
                    move_name: PokemonMove::Struggle,
                    priority: 0,
                    user_slot,
                    target_slot: *target,
                    moves_first: false,
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
/// Returns true if `player` has at least one non-fainted Pokémon on the bench.
fn has_healthy_bench(bs: &BattleState, player: Player) -> bool {
    match player {
        Player::P1 => bs.p1_back_mons.iter().any(|m| !m.fainted),
        Player::P2 => bs.p2_back_mons.iter().any(|m| !m.fainted),
    }
}

fn slot_has_substitute(bs: &BattleState, slot: FieldSlot) -> bool {
    simulator_helpers::get_pokemon_at_slot(bs, slot).map_or(false, |m| {
        m.volatiles.iter().any(|v|
            matches!(v, crate::pokemon::VolatileStatusState::TurnStatus(VolatileStatus::Substitute, _))
        )
    })
}

/// The slot that owns an action (every `Action` variant carries a `user_slot`).
fn action_user_slot(action: &Action) -> FieldSlot {
    match action {
        Action::MoveAction(a) => a.user_slot,
        Action::SwitchAction(a) => a.user_slot,
        Action::MegaAction(a) => a.user_slot,
        Action::TeraAction(a) => a.user_slot,
    }
}

/// Whether the Pokémon in `slot` can be forced out by a phazing move. Suction Cups / Guard Dog
/// (unsuppressed) and Ingrain prevent it. TODO: Mold Breaker bypasses Suction Cups / Guard Dog.
fn can_be_forced_out(bs: &BattleState, slot: FieldSlot) -> bool {
    let Some(mon) = simulator_helpers::get_pokemon_at_slot(bs, slot) else { return false; };
    if !simulator_helpers::pokemon_ability_is_suppressed(bs, mon)
        && matches!(mon.ability, Ability::SuctionCups | Ability::GuardDog)
    {
        return false;
    }
    mon.volatiles.iter().all(|v| !matches!(v,
        crate::pokemon::VolatileStatusState::TurnStatus(VolatileStatus::Ingrain, _)
            | crate::pokemon::VolatileStatusState::MoveStatus(VolatileStatus::Ingrain, _)))
}

/// Status moves that legitimately succeed without changing any battle state (so the success-by-diff
/// heuristic below must not mis-flag them as failures).
fn status_move_always_succeeds(name: &PokemonMove) -> bool {
    matches!(name, PokemonMove::Splash | PokemonMove::Celebrate | PokemonMove::HoldHands)
}

/// Compare the battle-meaningful fields of two `PokemonState` lists (HP, status, boosts, volatiles,
/// item, species, fainted), ignoring per-turn bookkeeping (PP, last-used move, the `*_this_turn`
/// flags, stall counter, etc.).
fn mons_meaningful_equal(a: &[PokemonState], b: &[PokemonState]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(x, y)| {
            x.hp == y.hp
                && x.status == y.status
                && x.boosts == y.boosts
                && x.volatiles == y.volatiles
                && x.item == y.item
                && x.species == y.species
                && x.fainted == y.fainted
        })
}

/// Whether a status move changed anything battle-meaningful between the pre-move baseline and the
/// resulting state. Used to set `last_move_failed` for status moves (Stomping Tantrum / Micle).
fn status_move_changed_state(before: &BattleState, after: &BattleState) -> bool {
    !(mons_meaningful_equal(&before.p1_active_mons, &after.p1_active_mons)
        && mons_meaningful_equal(&before.p1_back_mons, &after.p1_back_mons)
        && mons_meaningful_equal(&before.p2_active_mons, &after.p2_active_mons)
        && mons_meaningful_equal(&before.p2_back_mons, &after.p2_back_mons)
        && before.weather == after.weather
        && before.weather_turns == after.weather_turns
        && before.terrain == after.terrain
        && before.terrain_turns == after.terrain_turns
        && before.pseudo_weathers == after.pseudo_weathers
        && before.pseudo_weather_turns == after.pseudo_weather_turns
        && before.p1_side_conditions == after.p1_side_conditions
        && before.p2_side_conditions == after.p2_side_conditions
        && before.p1_slot_conditions == after.p1_slot_conditions
        && before.p2_slot_conditions == after.p2_slot_conditions)
}

/// Apply a phazing move's forced switch to `target_slot`: branch over each eligible bench mon of the
/// target's side at equal probability, swapping it in and running its send-out (entry hazards +
/// abilities). Branches where the switch can't happen — target fainted, blocked by Suction
/// Cups/Guard Dog/Ingrain, behind a Substitute (for non-`bypasssub` moves), or no eligible bench —
/// pass through unchanged. Deterministic at this point: the move already connected.
fn apply_forced_switch(
    branches: Vec<(BattleState, f64)>,
    target_slot: FieldSlot,
    move_data: &MoveData,
    pokemon_dex: &HashMap<Species, PokemonData>,
) -> Vec<(BattleState, f64)> {
    let bypasses_sub = simulator_helpers::move_has_flag(move_data, &crate::dex_data::MoveFlag::BypassSub);
    let mut out = Vec::new();
    for (bs, prob) in branches {
        let target_fainted = simulator_helpers::get_pokemon_at_slot(&bs, target_slot)
            .map_or(true, |m| m.fainted);
        let switches = !target_fainted
            && can_be_forced_out(&bs, target_slot)
            && (bypasses_sub || !slot_has_substitute(&bs, target_slot))
            && has_healthy_bench(&bs, target_slot.player);
        if !switches {
            out.push((bs, prob));
            continue;
        }
        let bench: Vec<usize> = match target_slot.player {
            Player::P1 => bs.p1_back_mons.iter().enumerate().filter(|(_, m)| !m.fainted).map(|(i, _)| i).collect(),
            Player::P2 => bs.p2_back_mons.iter().enumerate().filter(|(_, m)| !m.fainted).map(|(i, _)| i).collect(),
        };
        let n = bench.len() as f64;
        for idx in bench {
            let mut clone = bs.clone();
            perform_switch_out_in(&mut clone, target_slot, idx, pokemon_dex);
            simulator_helpers::process_pokemon_send_out(&mut clone, target_slot);
            // The switched-out target's still-queued action (e.g. a slower −7 move) must not run for
            // the replacement that just entered.
            clone.action_queue.retain(|a| action_user_slot(a) != target_slot);
            out.push((clone, prob / n));
        }
    }
    out
}

fn apply_post_damage_move_effects(
    mut bs: BattleState,
    attacker_slot: FieldSlot,
    move_data: &MoveData,
    baseline: &BattleState,
    opposing_player: Player,
) -> MatchState {
    let total_dmg = total_damage_to_opponent(baseline, &bs, opposing_player);
    let opponent_wiped = !simulator_helpers::team_has_remaining_pokemon(&bs, opposing_player) && total_dmg > 0;
    let attacker_item_active = simulator_helpers::get_pokemon_at_slot(&bs, attacker_slot)
        .map(|m| simulator_helpers::item_is_active(&bs, m))
        .unwrap_or(false);
    let attacker_env = simulator_helpers::berry_env(&bs, attacker_slot);
    let mut forced_winner: Option<Player> = None;
    let mut attacker_fainted = false;

    if let Some(attacker_mon) = mon_at_slot_mut(&mut bs, attacker_slot) {
        // Stomping Tantrum / Temper Flare / Micle Berry bookkeeping: a damaging move
        // that dealt no damage to any target this action (missed every target, or no
        // effect) counts as the last move failing; dealing damage clears the flag.
        // Status moves keep their explicit fail paths (e.g. failed Aura Wheel).
        if !matches!(move_data.category, MoveCategory::Status) {
            attacker_mon.last_move_failed = total_dmg == 0;
        }

        let max_hp = attacker_mon.stats[0].max(1);

        // Heal Block prevents any HP recovery from moves, draining moves and Shell Bell
        // (the move still works otherwise; recoil/damage are unaffected).
        let heal_blocked = simulator_helpers::heal_is_blocked(attacker_mon);

        // Unconditional self-heal
        if !heal_blocked && move_data.heal_fraction[0] > 0 && move_data.heal_fraction[1] > 0 {
            let heal = ((max_hp as u32 * move_data.heal_fraction[0] as u32) / move_data.heal_fraction[1] as u32) as u16;
            if heal > 0 { simulator_helpers::gain_hp(attacker_mon, heal, attacker_env); }
        }

        // Drain heal
        if !heal_blocked && move_data.drain_fraction[0] > 0 && move_data.drain_fraction[1] > 0 {
            let heal = ((total_dmg * move_data.drain_fraction[0] as u32) / move_data.drain_fraction[1] as u32) as u16;
            if heal > 0 { simulator_helpers::gain_hp(attacker_mon, heal, attacker_env); }
        }

        // Shell Bell: restore 1/8 of damage dealt (rounded down) to the attacker.
        // Does not consume the item. Based on damage dealt, not HP lost by target.
        if !heal_blocked && attacker_item_active && attacker_mon.item == crate::data::item::Item::ShellBell {
            let heal = (total_dmg / 8) as u16;
            if heal > 0 { simulator_helpers::gain_hp(attacker_mon, heal, attacker_env); }
        }

        // Recoil
        // Struggle recoil (¼ max HP) ignores Rock Head and Magic Guard; ordinary recoil does not.
        let has_normal_recoil = move_data.recoil_fraction[0] > 0 && move_data.recoil_fraction[1] > 0;
        let has_recoil = has_normal_recoil || move_data.struggle_recoil;
        let ability_blocks_recoil = !move_data.struggle_recoil
            && (attacker_mon.ability == Ability::RockHead || attacker_mon.ability == Ability::MagicGuard);
        if has_recoil && !ability_blocks_recoil {
            let recoil = if has_normal_recoil {
                ((total_dmg * move_data.recoil_fraction[0] as u32) / move_data.recoil_fraction[1] as u32) as u16
            } else if move_data.struggle_recoil {
                (max_hp as u32 / 4) as u16
            } else { 0 };

            if recoil > 0 {
                simulator_helpers::take_damage(attacker_mon, recoil, attacker_env);
                if attacker_mon.fainted {
                    simulator_helpers::clear_pokemon_on_faint(attacker_mon);
                    attacker_fainted = true;
                    if opponent_wiped { forced_winner = Some(attacker_slot.player); }
                }
            }
        }
    }

    if attacker_fainted {
        simulator_helpers::handle_pokemon_faint(&mut bs, attacker_slot.player, attacker_slot.slot_index);
    }

    // Magician: an empty-handed attacker steals the held item of a target it damaged with
    // this move. Runs once per move — matching the after-the-last-strike timing of the
    // cartridge. Sticky Hold and untransferable items are handled inside try_steal_item.
    if !attacker_fainted && total_dmg > 0 {
        let magician = simulator_helpers::get_pokemon_at_slot(&bs, attacker_slot)
            .map(|m| !m.fainted
                && m.item == Item::None
                && m.ability == Ability::Magician
                && !simulator_helpers::pokemon_ability_is_suppressed(&bs, m))
            .unwrap_or(false);
        if magician {
            let n_opposing = match opposing_player {
                Player::P1 => bs.p1_active_mons.len(),
                Player::P2 => bs.p2_active_mons.len(),
            };
            for i in 0..n_opposing {
                let slot = FieldSlot { player: opposing_player, slot_index: i as u8 };
                let damaged = {
                    let before = simulator_helpers::get_pokemon_at_slot(baseline, slot).map(|m| m.hp);
                    let after = simulator_helpers::get_pokemon_at_slot(&bs, slot).map(|m| m.hp);
                    matches!((before, after), (Some(b), Some(a)) if a < b)
                };
                if damaged && simulator_helpers::try_steal_item(&mut bs, attacker_slot, slot) {
                    break; // only one item can be held
                }
            }
        }
    }

    if let Some(winner) = forced_winner {
        MatchState::GameOverState { winner }
    } else if let Some(game_over) = game_over_state_if_battle_finished(&bs) {
        game_over
    } else {
        // Set self_switch_pending if:
        //  - the move has a self-switch type
        //  - the attacker is still alive
        //  - the attacker has a healthy bench mon to switch to
        //  - the move actually connected (total_dmg > 0) OR it's a status/non-damaging move
        //    (so a missed damaging self-switch like a missed U-turn does NOT trigger)
        //  - for Shed Tail specifically: the Substitute was actually created this step
        //    (user now carries a Substitute volatile)
        if move_data.self_switch != SelfSwitchType::None && !attacker_fainted && has_healthy_bench(&bs, attacker_slot.player) {
            let attacker_alive = match attacker_slot.player {
                Player::P1 => bs.p1_active_mons.get(attacker_slot.slot_index as usize).map(|m| !m.fainted).unwrap_or(false),
                Player::P2 => bs.p2_active_mons.get(attacker_slot.slot_index as usize).map(|m| !m.fainted).unwrap_or(false),
            };
            let move_connected = total_dmg > 0 || matches!(move_data.category, MoveCategory::Status);
            // For ShedTail: success ⇔ attacker now has a Substitute AND did not have one before
            // the move (baseline). Using baseline comparison instead of an HP check means items
            // like Sitrus Berry healing after the HP cost cannot mask a successful switch.
            let shed_tail_sub_created = move_data.self_switch != SelfSwitchType::ShedTail
                || (slot_has_substitute(&bs, attacker_slot) && !slot_has_substitute(baseline, attacker_slot));
            if attacker_alive && move_connected && shed_tail_sub_created {
                bs.self_switch_pending = Some((attacker_slot, move_data.self_switch));
            }
        }
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
                Some(move_data) => {
                    // Item-loss ledger: diff held items across the whole action so that
                    // berries eaten deep inside damage application fire Unburden /
                    // Pickup-pool / Symbiosis reactions.
                    let item_snapshot = simulator_helpers::snapshot_active_items(&state);
                    possible_damage_outcomes_for_move(&state, &m, move_data, config, move_dex, pokemon_dex, false)
                        .into_iter()
                        .map(|(mut st, p)| {
                            if let MatchState::BattleState(ref mut bs) = st {
                                simulator_helpers::process_item_loss_events(bs, &item_snapshot);
                                // Weather / ability-altering moves may change Castform's form.
                                simulator_helpers::update_forecast_forms(bs);
                            }
                            (st, p)
                        })
                        .collect()
                }
                None => vec![(MatchState::BattleState(state), 1.0)],
            }
        }
        Action::SwitchAction(s) => {
            perform_switch_out_in(&mut state, s.user_slot, s.switch_index, pokemon_dex);
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
            let evolved = mons.get_mut(slot_idx).map(|mon| crate::battle::try_mega_evolution(mon, pokemon_dex)).unwrap_or(false);
            match m.user_slot.player { Player::P1 => state.p1_has_mega = false, Player::P2 => state.p2_has_mega = false }
            if evolved {
                // The mega form may have a different ability; trigger its on-gain effects
                // (weather/terrain setters, Intimidate) the same way a Pokémon gaining an
                // ability mid-battle does.
                simulator_helpers::process_pokemon_gain_ability(&mut state, m.user_slot);
            }
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

    // Empty queue — end of turn / replacement phase.
    // end_turn now returns Vec<(BattleState, f64)> because probabilistic abilities
    // (Shed Skin, Healer, Moody, Harvest) can branch the outcome tree.
    if next_state.action_queue.is_empty() {
        let eot_branches = simulator_helpers::end_turn(&mut next_state);
        let mut result: Vec<(MatchState, f64)> = Vec::with_capacity(eot_branches.len());
        for (mut bs, prob) in eot_branches {
            if let Some(game_over) = game_over_state_if_battle_finished(&bs) {
                result.push((game_over, prob));
            } else {
                if replacement_needed(&bs) {
                    bs.turn_started = true;
                    bs.turn_ended = true;
                } else {
                    bs.turn_started = false;
                    bs.turn_ended = false;
                }
                result.push((MatchState::BattleState(bs), prob));
            }
        }
        return simulator_helpers::coalesce_branches(result);
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
    let initial_branches = apply_player_commands_branching(state, p1_cmd, p2_cmd, move_dex, pokemon_dex);

    // moves_first flag: combines Quick Claw (item, 20%) and Quick Draw (ability, 30%).
    //
    // Per-mon combined activation probability:
    //   p_first = 1 − (1 − p_qc) × (1 − p_qd)
    // A Quick-Claw-only mon gets 0.20; a Quick-Draw-only mon gets 0.30; both = 0.44.
    //
    // Each eligible MoveAction is branched independently (active vs inactive), and the
    // two branches are weighted by p_first and (1 − p_first) respectively.
    // coalesce_branches merges structurally identical outcomes afterward.
    let initial_branches: Vec<(MatchState, f64)> = initial_branches
        .into_iter()
        .flat_map(|(st, prob)| {
            let MatchState::BattleState(ref bs) = st else {
                return vec![(st, prob)];
            };
            // Collect (queue_index, p_first) for each eligible MoveAction.
            let eligible: Vec<(usize, f64)> = bs.action_queue.iter().enumerate()
                .filter_map(|(i, action)| {
                    if let Action::MoveAction(ma) = action {
                        let user = simulator_helpers::get_pokemon_at_slot(bs, ma.user_slot)?;
                        let move_data = move_dex.get(&ma.move_name);
                        let items_ok = simulator_helpers::item_is_active(bs, user);
                        let abilities_ok = !simulator_helpers::pokemon_ability_is_suppressed(bs, user);

                        let p_qc = if items_ok && user.item == Item::QuickClaw { 0.2 } else { 0.0 };
                        let p_qd = if abilities_ok
                            && user.ability == Ability::QuickDraw
                            && move_data.map_or(false, |md| matches!(md.category, MoveCategory::Physical | MoveCategory::Special))
                        { 0.3 } else { 0.0 };

                        let p_first = 1.0 - (1.0 - p_qc) * (1.0 - p_qd);
                        if p_first > 0.0 { Some((i, p_first)) } else { None }
                    } else {
                        None
                    }
                })
                .collect();
            if eligible.is_empty() {
                return vec![(st, prob)];
            }
            // Expand independently: cartesian product of (active, inactive) for each holder.
            let n = eligible.len();
            let total = 1usize << n;  // 2^n combinations
            let mut branches: Vec<(MatchState, f64)> = Vec::with_capacity(total);
            for mask in 0..total {
                let mut branch = st.clone();
                let MatchState::BattleState(ref mut bbs) = branch else { continue; };
                let mut branch_prob = prob;
                for (bit, &(queue_idx, p_first)) in eligible.iter().enumerate() {
                    let active = (mask >> bit) & 1 == 1;
                    if let Action::MoveAction(ref mut ma) = bbs.action_queue[queue_idx] {
                        ma.moves_first = active;
                    }
                    branch_prob *= if active { p_first } else { 1.0 - p_first };
                }
                branches.push((branch, branch_prob));
            }
            branches
        })
        .collect();
    let initial_branches = simulator_helpers::coalesce_branches(initial_branches);

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

        // A self-switch move resolved and the player must choose a replacement.
        // Return the state as-is to the caller; action_queue is preserved so the
        // turn resumes once the replacement is sent in.
        if battle.self_switch_pending.is_some() {
            return vec![(state.clone(), 1.0)];
        }

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
    use crate::data::species::Species;
    mon.volatiles.clear();
    mon.boosts.iter_mut().for_each(|b| *b = 0);
    if matches!(mon.status, Some(Status::ToxicPoison(_))) {
        mon.status = Some(Status::ToxicPoison(0));
    }
    // Hunger Switch: Morpeko reverts to Full Belly form on switch-out.
    if mon.species == Species::MorpekoHangry {
        mon.species = Species::Morpeko;
    }
    // Clear the entry flag so it doesn't persist on the bench.
    mon.entered_this_turn = false;
    mon.cud_chew_pending = None;
    // Unburden's boost ends on switch-out.
    mon.item_lost = false;
    // Per-turn event flags don't follow a Pokémon to the bench.
    mon.damaged_this_turn = false;
    mon.damaged_by_this_turn.clear();
    mon.stats_raised_this_turn = false;
    mon.stats_lowered_this_turn = false;
    mon.switched_in_this_turn = false;
    // The consecutive-protect streak ends when the Pokémon leaves the field.
    mon.stall_counter = 0;
}

fn perform_switch_out_in(
    next_state: &mut BattleState,
    user_slot: FieldSlot,
    bench_index: usize,
    pokemon_dex: &HashMap<Species, PokemonData>,
) {
    let slot_idx = user_slot.slot_index as usize;
    let (active, back) = match user_slot.player {
        Player::P1 => (&mut next_state.p1_active_mons, &mut next_state.p1_back_mons),
        Player::P2 => (&mut next_state.p2_active_mons, &mut next_state.p2_back_mons),
    };
    if slot_idx >= active.len() || bench_index >= back.len() { return; }

    let mut leaving = active[slot_idx].clone();
    clear_pokemon_for_switch_out(&mut leaving);

    // The incoming replacement switched in this turn (Payback won't double against it).
    if let Some(incoming) = back.get_mut(bench_index) {
        incoming.switched_in_this_turn = true;
    }

    // Zero to Hero: Palafin permanently becomes Hero Form the first time it leaves the
    // field (any switch cause). Never reverts for the rest of the battle.
    if leaving.species == Species::Palafin && leaving.ability == Ability::ZerotoHero && !leaving.fainted {
        crate::battle::change_form(&mut leaving, Species::PalafinHero, pokemon_dex);
    }
    // Stance Change: Aegislash reverts to Shield Forme on switch-out.
    if leaving.species == Species::AegislashBlade && leaving.ability == Ability::StanceChange {
        crate::battle::change_form(&mut leaving, Species::Aegislash, pokemon_dex);
    }
    std::mem::swap(&mut active[slot_idx], &mut back[bench_index]);
    back[bench_index] = leaving;

    // SyrupBomb ends when the user leaves the field — remove it from all opponents.
    // (The target's SyrupBomb is on the opponent side relative to the one switching out.)
    {
        let opp_mons = match user_slot.player {
            Player::P1 => &mut next_state.p2_active_mons,
            Player::P2 => &mut next_state.p1_active_mons,
        };
        for opp in opp_mons.iter_mut() {
            simulator_helpers::remove_status_volatile(opp, &VolatileStatus::SyrupBomb);
        }
    }

    // All switch-out side effects (switch-out abilities, Neutralizing Gas lift, primal
    // weather ending) are handled here, after the departing Pokémon has reached the bench.
    simulator_helpers::handle_pokemon_switch_out(next_state, user_slot.player, bench_index);
}

/// Perform a self-switch (U-turn, Baton Pass, Shed Tail, etc.) from `user_slot` to
/// `bench_index`.  Always calls `perform_switch_out_in` first so switch-out ability hooks
/// (Desolate Land, Neutralizing Gas, …) always fire, then:
///
/// - `Normal`: nothing extra — replacement enters cleared.
/// - `BatonPass`: boost table and passable volatile statuses are snapshot *before* the base
///   call, then restored onto the replacement after the swap.
/// - `ShedTail`: only the Substitute volatile (if present) is forwarded; all other boosts
///   and volatiles are cleared by the base call.
fn perform_self_switch(
    next_state: &mut BattleState,
    user_slot: FieldSlot,
    bench_index: usize,
    switch_type: SelfSwitchType,
    pokemon_dex: &HashMap<Species, PokemonData>,
) {
    let slot_idx = user_slot.slot_index as usize;

    // Snapshot the leaving mon's transferable state BEFORE the base call clears it.
    let (boosts_to_pass, volatiles_to_pass) = match switch_type {
        SelfSwitchType::BatonPass => {
            let mons = match user_slot.player {
                Player::P1 => &next_state.p1_active_mons,
                Player::P2 => &next_state.p2_active_mons,
            };
            if let Some(mon) = mons.get(slot_idx) {
                let boosts = mon.boosts;
                // Passable volatiles per Bulbapedia (newest generation).
                let passable = mon.volatiles.iter().filter(|v| {
                    use crate::pokemon::VolatileStatusState;
                    use VolatileStatus::*;
                    matches!(v,
                        VolatileStatusState::TurnStatus(Confusion, _)
                        | VolatileStatusState::TurnStatus(FocusEnergy, _)
                        | VolatileStatusState::TurnStatus(PartiallyTrapped(_), _)
                        | VolatileStatusState::TurnStatus(LeechSeed, _)
                        | VolatileStatusState::TurnStatus(Curse, _)
                        | VolatileStatusState::TurnStatus(Substitute, _)
                        | VolatileStatusState::TurnStatus(Ingrain, _)
                        | VolatileStatusState::TurnStatus(PowerTrick, _)
                        | VolatileStatusState::TurnStatus(HealBlock, _)
                        | VolatileStatusState::TurnStatus(Embargo, _)
                        | VolatileStatusState::TurnStatus(MagnetRise, _)
                        | VolatileStatusState::TurnStatus(Telekinesis, _)
                        | VolatileStatusState::TurnStatus(GastroAcid, _)
                        | VolatileStatusState::TurnStatus(PerishSong, _)
                    )
                }).cloned().collect::<Vec<_>>();
                (Some(boosts), passable)
            } else {
                (None, vec![])
            }
        }
        SelfSwitchType::ShedTail => {
            // Pass only the Substitute volatile; boosts and all other volatiles are cleared.
            let mons = match user_slot.player {
                Player::P1 => &next_state.p1_active_mons,
                Player::P2 => &next_state.p2_active_mons,
            };
            let sub = mons.get(slot_idx).and_then(|mon| {
                mon.volatiles.iter().find(|v|
                    matches!(v, crate::pokemon::VolatileStatusState::TurnStatus(VolatileStatus::Substitute, _))
                ).cloned()
            });
            (None, sub.into_iter().collect())
        }
        SelfSwitchType::Normal | SelfSwitchType::None => (None, vec![]),
    };

    // Base swap + switch-out ability hooks (always runs so Desolate Land / Neutralizing Gas fire).
    perform_switch_out_in(next_state, user_slot, bench_index, pokemon_dex);

    // Apply the snapshot to the now-active replacement (which just came out of the bench, cleared).
    if boosts_to_pass.is_some() || !volatiles_to_pass.is_empty() {
        let mons = match user_slot.player {
            Player::P1 => &mut next_state.p1_active_mons,
            Player::P2 => &mut next_state.p2_active_mons,
        };
        if let Some(replacement) = mons.get_mut(slot_idx) {
            if let Some(boosts) = boosts_to_pass {
                replacement.boosts = boosts;
            }
            replacement.volatiles.extend(volatiles_to_pass);
        }
    }
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
fn perform_simultaneous_switches_branching(
    next_state: &BattleState,
    switches: &[(FieldSlot, usize)],
    pokemon_dex: &HashMap<Species, PokemonData>,
) -> Vec<(BattleState, f64)> {
    // First apply all swaps to a base state
    let mut base = next_state.clone();
    for (slot, bench_index) in switches {
        perform_switch_out_in(&mut base, *slot, *bench_index, pokemon_dex);
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
        self_switch_pending: None,
        items_consumed_this_turn: vec![],
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
    pokemon_dex: &HashMap<Species, PokemonData>,
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

                let branches = perform_simultaneous_switches_branching(&next_state, &queued_switches, pokemon_dex);
                return simulator_helpers::coalesce_branches(branches.into_iter().map(|(bs, p)| (MatchState::BattleState(bs), p)).collect());
            }

            // Self-switch pending: a self-switch move resolved mid-turn and the owning player
            // must choose a replacement.  The pending slot's player sends a Switch command; every
            // other active slot sends Pass (which queues no Action).  Once the replacement is in,
            // self_switch_pending is cleared and the remaining action_queue drains as normal.
            if let Some((pending_slot, switch_type)) = battle.self_switch_pending {
                // Extract the chosen bench index from the owning player's command.
                let owning_cmd = match pending_slot.player {
                    Player::P1 => p1_cmd,
                    Player::P2 => p2_cmd,
                };
                let party_index = if let PlayerCommand::Battle(cmds) = owning_cmd {
                    cmds.get(pending_slot.slot_index as usize)
                        .and_then(|cmd| if let BattleCommand::Switch(s) = cmd { Some(s.party_index) } else { None })
                } else {
                    None
                };
                if let Some(bench_idx) = party_index {
                    perform_self_switch(&mut next_state, pending_slot, bench_idx, switch_type, pokemon_dex);
                    simulator_helpers::process_pokemon_send_out(&mut next_state, pending_slot);
                    next_state.self_switch_pending = None;
                }
                return vec![(MatchState::BattleState(next_state), 1.0)];
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