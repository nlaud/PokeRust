use std::collections::HashMap;
use rand::Rng;
use colored::Colorize;
use crate::state::battle::{
    MatchState, BattleState, TeamPreviewState, PlayerCommand, BattleCommand,
    AttackCommand, SwitchCommand, TeamPreviewCommand, Player, FieldSlot,
    Action, MoveAction, SwitchAction, MegaAction, TeraAction,
};
use crate::state::pokemon::{
    PokemonState, parse_team_sheet
};
use crate::state::dex_data::{MoveData, MoveFlag, MoveTarget, PokemonData, MoveCategory, SelfDestructType, SelfSwitchType, SideCondition, Status, VolatileStatus, PokemonType};
use crate::data::ability::Ability;
use crate::data::item::Item;
use crate::data::species::Species;
use crate::data::pokemon_move::PokemonMove;
use crate::information::information::{CantReason, EventKind, InformationEvent, SwitchState as InfoSwitchState};
use crate::information::unknowns::PokemonHP;
pub mod helpers;
use self::helpers as simulator_helpers;

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
fn write_back_volatiles(state: &mut BattleState, slot: FieldSlot, volatiles: Vec<crate::state::pokemon::VolatileStatusState>) {
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

fn decrement_move_pp(next_state: &mut BattleState, user_slot: FieldSlot, move_name: &PokemonMove, move_data_opt: Option<&MoveData>) {
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
        // Pre-check for DestinyBond removal so we can emit VolatileEnd after the borrow ends.
        let user_had_destiny_bond = *move_name != PokemonMove::DestinyBond
            && simulator_helpers::get_pokemon_at_slot(next_state, user_slot)
                .map(|m| simulator_helpers::has_status_volatile(m, &VolatileStatus::DestinyBond))
                .unwrap_or(false);
        // Pressure: drain 1 extra PP when an unsuppressed active opponent holds Pressure,
        // but only when the move targets opponents OR has the MustPressure flag (Imprison,
        // Snatch, Spikes, Stealth Rock, Toxic Spikes, Tera Blast). Self-targeting and
        // ally-targeting moves do not consume extra PP.
        let pressure_extra = {
            let any_pressure_opp = {
                let opp_mons = match user_slot.player {
                    Player::P1 => &next_state.p2_active_mons,
                    Player::P2 => &next_state.p1_active_mons,
                };
                opp_mons.iter().any(|m| {
                    !m.fainted
                        && m.ability == Ability::Pressure
                        && !simulator_helpers::pokemon_ability_is_suppressed(next_state, m)
                })
            };
            if !any_pressure_opp {
                false
            } else if let Some(md) = move_data_opt {
                // Correct: only trigger if the move targets foes or has MustPressure.
                let self_or_ally_only = matches!(
                    md.target,
                    MoveTarget::SelfTarget
                    | MoveTarget::AdjacentAlly
                    | MoveTarget::AdjacentAllyOrSelf
                    | MoveTarget::Allies
                    | MoveTarget::AllySide
                    | MoveTarget::AllyTeam
                );
                !self_or_ally_only
                    || simulator_helpers::move_has_flag(md, &MoveFlag::MustPressure)
            } else {
                // move_data unavailable (e.g. confusion self-hit, no-effect early exit):
                // fall back to always-trigger to preserve prior behavior.
                true
            }
        };
        if let Some(mon) = match user_slot.player {
            Player::P1 => next_state.p1_active_mons.get_mut(user_slot.slot_index as usize),
            Player::P2 => next_state.p2_active_mons.get_mut(user_slot.slot_index as usize),
        } {
            if let Some(pp) = mon.move_pp.get_mut(move_index) {
                *pp = pp.saturating_sub(1 + pressure_extra as u8);
            }
            simulator_helpers::try_consume_leppa_berry(mon, &leppa_env);

            // Choice items: lock the holder into the first move it uses.
            // Struggle is excluded — a PP-depleted mon shouldn't be locked into Struggle.
            // If already locked, no-op (lock was set by the first use this send-in).
            let is_choice = matches!(mon.item, Item::ChoiceBand | Item::ChoiceScarf | Item::ChoiceSpecs);
            let already_locked = mon.volatiles.iter().any(|v|
                matches!(v, crate::state::pokemon::VolatileStatusState::TurnStatus(VolatileStatus::ChoiceLock(_), _))
            );
            if !item_inactive && is_choice && !already_locked && *move_name != PokemonMove::Struggle {
                mon.volatiles.push(crate::state::pokemon::VolatileStatusState::TurnStatus(
                    VolatileStatus::ChoiceLock(move_name.clone()), 0,
                ));
            }
            // Track the last move used (for Disable targeting); Struggle is excluded.
            // consecutive_move_count is pre-updated in possible_damage_outcomes_for_move
            // (before branches are created) so the Metronome item sees the correct streak.
            if *move_name != PokemonMove::Struggle {
                mon.last_used_move = Some(move_name.clone());
            }

            // Track which move slots have been used for Last Resort.
            if let Some(slot) = mon.moves.iter().position(|m| m.as_ref() == Some(move_name)) {
                if slot < 4 { mon.used_moves_this_field[slot] = true; }
            }

            // Destiny Bond: remove the volatile at the start of any subsequent action
            // that is not Destiny Bond itself (mirrors Showdown's onBeforeMove removal).
            if *move_name != PokemonMove::DestinyBond {
                simulator_helpers::remove_status_volatile(mon, &VolatileStatus::DestinyBond);
            }
        }
        // Emit VolatileEnd for DestinyBond removal now that the mutable borrow has ended.
        if user_had_destiny_bond {
            simulator_helpers::emit(next_state, EventKind::VolatileEnd {
                target: user_slot,
                volatile: VolatileStatus::DestinyBond,
            });
        }
        // Update the field-level last-move tracker for Copycat; Struggle excluded.
        if *move_name != PokemonMove::Struggle {
            next_state.last_move_on_field = Some(move_name.clone());
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
        decrement_move_pp(&mut branch_state, user_slot, move_name, None);
        // Hurting itself in confusion means the chosen move never executed.
        simulator_helpers::note_move_outcome(&mut branch_state, user_slot, simulator_helpers::MoveOutcome::Cant(CantReason::Confusion));

        if let Some(game_over_state) = simulator_helpers::apply_damage_and_check_game_over(&mut branch_state, user_slot, damage) {
            outcomes.push((game_over_state, probability));
        } else {
            outcomes.push((MatchState::BattleState(branch_state), probability));
        }
    }

    outcomes
}

/// Before any action resolves this turn, give Beak Blast users a `BeakBlastCharging` volatile
/// and Focus Punch users a `FocusPunchCharging` volatile.
/// Both auto-expire at end-of-turn (TurnStatus, duration 1).
fn apply_priority_charge_volatiles(bs: &mut BattleState) {
    use crate::state::dex_data::VolatileStatus;
    use crate::state::pokemon::VolatileStatusState;

    // Collect slots for Beak Blast and Focus Punch in a single pass.
    let mut beak_blast_users: Vec<FieldSlot> = Vec::new();
    let mut focus_punch_users: Vec<FieldSlot> = Vec::new();
    for a in &bs.action_queue {
        if let Action::MoveAction(ma) = a {
            match ma.move_name {
                PokemonMove::BeakBlast   => beak_blast_users.push(ma.user_slot),
                PokemonMove::FocusPunch  => focus_punch_users.push(ma.user_slot),
                _ => {}
            }
        }
    }

    for slot in beak_blast_users {
        let mut newly_set = false;
        if let Some(mon) = mon_at_slot_mut(bs, slot) {
            if mon.fainted { continue; }
            let already_has = mon.volatiles.iter().any(|v| matches!(v,
                VolatileStatusState::TurnStatus(VolatileStatus::BeakBlastCharging, _)
            ));
            if !already_has {
                mon.volatiles.push(VolatileStatusState::TurnStatus(VolatileStatus::BeakBlastCharging, 1));
                newly_set = true;
            }
        }
        if newly_set {
            simulator_helpers::emit(bs, EventKind::SingleMoveOrTurn {
                slot,
                move_used: PokemonMove::BeakBlast,
            });
        }
    }

    // Focus Punch: set FocusPunchCharging so the damage-landing path can check it.
    // If the holder takes direct damage from an opponent before their action, the move fails.
    for slot in focus_punch_users {
        let mut newly_set = false;
        if let Some(mon) = mon_at_slot_mut(bs, slot) {
            if mon.fainted { continue; }
            let already_has = mon.volatiles.iter().any(|v| matches!(v,
                VolatileStatusState::TurnStatus(VolatileStatus::FocusPunchCharging, _)
            ));
            if !already_has {
                mon.volatiles.push(VolatileStatusState::TurnStatus(VolatileStatus::FocusPunchCharging, 1));
                newly_set = true;
            }
        }
        if newly_set {
            simulator_helpers::emit(bs, EventKind::SingleMoveOrTurn {
                slot,
                move_used: PokemonMove::FocusPunch,
            });
        }
    }
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
                    target_mon.volatiles.push(crate::state::pokemon::VolatileStatusState::TurnStatus(VolatileStatus::SkyDrop, 2));
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
    attacker.volatiles.push(crate::state::pokemon::VolatileStatusState::Charging(action.move_name.clone(), targets));
    write_back_volatiles(next_state, action.user_slot, attacker.volatiles.clone());

    // Decrement PP on the charge turn
    if let Some(pp_idx) = attacker.moves.iter().position(|m| m.as_ref() == Some(&action.move_name)) {
        if let Some(mon) = mon_at_slot_mut(next_state, action.user_slot) {
            if let Some(pp) = mon.move_pp.get_mut(pp_idx) { *pp = pp.saturating_sub(1); }
        }
    }

    simulator_helpers::emit(next_state, EventKind::ChargingMove {
        user: action.user_slot,
        move_used: action.move_name.clone(),
    });
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
    let mut move_has_charge = simulator_helpers::move_has_flag(move_data, &crate::state::dex_data::MoveFlag::Charge);
    if matches!(action.move_name, PokemonMove::SolarBeam | PokemonMove::SolarBlade)
        && simulator_helpers::weather_is_sunlight_for_slot(state, action.user_slot)
    {
        move_has_charge = false;
    }
    if action.move_name == PokemonMove::ElectroShot && simulator_helpers::weather_is_rain(state) {
        move_has_charge = false;
    }

    let move_causes_invulnerability = simulator_helpers::move_causes_invulnerability(&action.move_name);

    let charging_data = attacker.volatiles.iter().find_map(|v| {
        if let crate::state::pokemon::VolatileStatusState::Charging(mov, targets) = v {
            if mov == &action.move_name { Some((v.clone(), targets.clone())) } else { None }
        } else { None }
    });

    let is_semi_invulnerable = attacker.volatiles.iter().any(|v| {
        matches!(v, crate::state::pokemon::VolatileStatusState::MoveStatus(VolatileStatus::SemiInvulnerable(mov), _) if mov == &action.move_name)
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
    // Skull Bash: +1 Def on the charge turn (boosts[1] = Defense).
    if action.move_name == PokemonMove::SkullBash && charging_data.is_none() {
        let raised = attacker.boosts[1] < 6;
        attacker.boosts[1] = (attacker.boosts[1] + 1).clamp(-6, 6);
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

    // Power Herb: skip the charge turn for any two-turn charging move. The herb is consumed
    // immediately, and the move executes in the same turn (fall through to damage).
    // Charge-turn stat boosts (ElectroShot/MeteorBeam/Skull Bash, handled above) still apply
    // because they are gated on `charging_data.is_none()`, which is true on the Power Herb turn.
    if move_has_charge && charging_data.is_none() && !move_causes_invulnerability {
        let items_suppressed = simulator_helpers::items_are_suppressed(next_state);
        let has_power_herb = !items_suppressed
            && simulator_helpers::get_pokemon_at_slot(next_state, action.user_slot)
                .map(|m| simulator_helpers::item_is_active(next_state, m)
                    && m.item == crate::data::item::Item::PowerHerb)
                .unwrap_or(false);
        if has_power_herb {
            // Consume the herb: clear the item slot, record for Recycle, and fire Unburden/Pickup.
            if let Some(mon) = mon_at_slot_mut(next_state, action.user_slot) {
                mon.consumed_item = Some(crate::data::item::Item::PowerHerb);
                mon.item = crate::data::item::Item::None;
                mon.item_lost = true;
                next_state.items_consumed_this_turn.push((action.user_slot, crate::data::item::Item::PowerHerb));
            }
            // item_lost = true bypasses the snapshot; emit directly.
            simulator_helpers::emit(next_state, EventKind::ItemLost {
                slot: action.user_slot,
                item: Item::PowerHerb,
                consumed: true,
            });
            // Fall through to damage (skip handle_charging_first_turn).
        } else {
            return handle_charging_first_turn(attacker, action, move_data, next_state);
        }
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
            let as_ = simulator_helpers::abilities_are_suppressed(&branch_state);
            let mut busted_fainted = false;
            if let Some(mon) = simulator_helpers::get_pokemon_at_slot_mut(&mut branch_state, target_slot) {
                mon.species = Species::MimikyuBusted;
                let chip = (mon.stats[0] / 8).max(1);
                simulator_helpers::take_damage(mon, chip, env, as_);
                if mon.fainted {
                    simulator_helpers::clear_pokemon_on_faint(mon);
                    busted_fainted = true;
                }
            }
            simulator_helpers::emit(&mut branch_state, EventKind::FormeChange {
                slot: target_slot,
                into: Species::MimikyuBusted,
                permanent: true,
            });
            if busted_fainted {
                simulator_helpers::handle_pokemon_faint(&mut branch_state, target_slot.player, target_slot.slot_index);
            }
        }
    }
    // Substitute absorption: if the target has a Substitute and this attack doesn't bypass
    // it, route all damage into the sub. Sub breaks when its HP hits 0; for single-hit moves
    // excess damage is lost (does NOT fall through to the mon). For multi-hit, the sub breaking
    // on one hit is reflected in the state, and subsequent per-hit calls see no sub.
    // This block returns early: no secondary effects, no contact abilities, no endure/sash.
    // Recoil still fires — tracked via `sub_damage_dealt` consumed by apply_post_damage_move_effects.
    if damage > 0 && attack_slot.player != target_slot.player {
        let sub_hp = simulator_helpers::get_pokemon_at_slot(&branch_state, target_slot)
            .map(|m| simulator_helpers::get_substitute_hp(m))
            .unwrap_or(0);
        let bypasses = simulator_helpers::attack_bypasses_substitute(
            &branch_state, attack_slot, move_data,
        );
        if sub_hp > 0 && !bypasses {
            branch_state.sub_damage_dealt += damage as u32;
            let sub_broke = damage >= sub_hp;
            if let Some(mon) = simulator_helpers::get_pokemon_at_slot_mut(&mut branch_state, target_slot) {
                if sub_broke {
                    // Sub breaks; remove it
                    simulator_helpers::remove_status_volatile(mon, &VolatileStatus::Substitute(0));
                } else {
                    simulator_helpers::set_substitute_hp(mon, sub_hp - damage);
                }
            }
            // Emit VolatileEnd for the broken substitute after the borrow ends.
            if sub_broke {
                simulator_helpers::emit(&mut branch_state, EventKind::VolatileEnd {
                    target: target_slot,
                    volatile: VolatileStatus::Substitute(0),
                });
            }
            outcomes.push((branch_state, branch_probability));
            return outcomes;
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
                let eff = if *move_name == PokemonMove::FlyingPress {
                    simulator_helpers::flying_press_type_effectiveness(&branch_state, Some(atk), tgt)
                } else {
                    simulator_helpers::move_type_effectiveness(&branch_state, &at, tgt)
                };
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
            // Ability absorbed the move — emit Immune (the ability reveal follows naturally
            // from the ability activating inside try_absorb_move).
            simulator_helpers::emit(&mut branch_state_absorb, EventKind::Immune { target: target_slot });
            outcomes.push((branch_state_absorb, branch_probability));
            return outcomes;
        }
    }

    // Emit Crit before DamageDealt — only when the hit actually deals damage. Disguise
    // already zeroed `damage` above; Substitute and type-absorb returned early above.
    if is_crit && damage > 0 {
        simulator_helpers::emit(&mut branch_state, EventKind::Crit { target: target_slot });
    }

    // Snapshot the holder's HP before endure/damage so Innards Out can report the correct value.
    // Innards Out deals back the HP the holder had before the killing hit, which is
    // min(computed_damage, pre_hit_hp). We pass this clamped value as `damage_dealt`.
    let target_pre_hit_hp = simulator_helpers::get_pokemon_at_slot(&branch_state, target_slot)
        .map_or(0, |m| m.hp);

    // Focus Sash / Focus Band / Sturdy endure outcomes. Each entry is (eff_damage, consume_item, prob).
    // - Normal case:   one entry  (damage, false, 1.0)
    // - Sturdy KO:     one entry  (hp-1,   false, 1.0)   no item consumed
    // - Focus Sash KO: one entry  (hp-1,   true,  1.0)
    // - Focus Band KO: two entries (damage, false, 0.9) and (hp-1, false, 0.1)
    // Multi-hit calls us once per hit, so Band's 10% / Sturdy/Sash full-HP check is re-evaluated.
    let target_ability_suppressed = {
        let attacker_breaks = simulator_helpers::get_pokemon_at_slot(&branch_state, attack_slot)
            .map_or(false, |a| simulator_helpers::attacker_breaks_mold(&branch_state, a));
        simulator_helpers::get_pokemon_at_slot(&branch_state, target_slot)
            .map_or(false, |t|
                simulator_helpers::pokemon_ability_is_suppressed(&branch_state, t)
                    || (attacker_breaks && simulator_helpers::ability_is_ignorable(&t.ability)))
    };
    let endure_outcomes = simulator_helpers::get_pokemon_at_slot(&branch_state, target_slot)
        .map_or_else(
            || vec![(damage, false, 1.0)],
            |t| simulator_helpers::compute_endure_outcomes(t, damage, items_suppressed, target_ability_suppressed),
        );

    // Sheer Force: a move boosted by Sheer Force must not trigger the target's Berserk.
    // Berserk lives inside the generic `apply_damage`/`take_damage` HP-loss path (a deliberate
    // broadened-trigger divergence), so we suppress it here by snapshotting and restoring the
    // target's Sp. Atk boost around the hit — Berserk is the only effect in that path that
    // touches `boosts[2]` / `stats_raised_this_turn`, so the restore is targeted.
    let sheer_force_boosted = simulator_helpers::get_pokemon_at_slot(&branch_state, attack_slot)
        .map_or(false, |a| !simulator_helpers::pokemon_ability_is_suppressed(&branch_state, a)
            && a.ability == Ability::SheerForce
            && simulator_helpers::move_has_sheer_force_secondary(move_data));

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
        let mut smack_down_removed_magnet_rise = false;
        let mut smack_down_removed_telekinesis = false;
        // Captures (new_hp, max_hp) after take_damage so DamageDealt can be emitted once
        // the target_mon borrow has ended (same split pattern as Smack Down volatiles below).
        let mut damage_dealt_hp_info: Option<(u16, u16)> = None;
        let target_env = simulator_helpers::berry_env(&bs, target_slot);
        let as_ = simulator_helpers::abilities_are_suppressed(&bs);

        if let Some(target_mon) = match target_slot.player {
            Player::P1 => bs.p1_active_mons.get_mut(target_slot.slot_index as usize),
            Player::P2 => bs.p2_active_mons.get_mut(target_slot.slot_index as usize),
        } {
            let berserk_snapshot = if sheer_force_boosted {
                Some((target_mon.boosts[2], target_mon.stats_raised_this_turn))
            } else {
                None
            };
            simulator_helpers::take_damage(target_mon, eff_damage, target_env, as_);
            // Capture post-damage HP for DamageDealt emission below (after borrow ends).
            if bs.event_observer.is_some() && eff_damage > 0 {
                damage_dealt_hp_info = Some((target_mon.hp, target_mon.stats[0]));
            }
            if let Some((spa_boost, raised)) = berserk_snapshot {
                // Undo any Berserk Sp. Atk boost the boosted hit just triggered.
                target_mon.boosts[2] = spa_boost;
                target_mon.stats_raised_this_turn = raised;
            }

            // Rage Fist hit counter: any Physical/Special move that connects (even if HP is not
            // lost, e.g. Disguise absorption) increments the counter. Confusion self-damage and
            // Substitute-absorbed hits do not reach this site so they are naturally excluded.
            if matches!(
                move_data.category,
                crate::state::dex_data::MoveCategory::Physical | crate::state::dex_data::MoveCategory::Special
            ) {
                target_mon.times_hit = target_mon.times_hit.saturating_add(1);
            }

            // Per-turn damage tracking: Assurance reads `damaged_this_turn`; Avalanche
            // checks whether this specific attacker slot damaged the holder this turn.
            // Counter/Mirror Coat/Metal Burst/Comeuppance read the last hit by category.
            if eff_damage > 0 {
                target_mon.damaged_this_turn = true;
                if !target_mon.damaged_by_this_turn.contains(&attack_slot) {
                    target_mon.damaged_by_this_turn.push(attack_slot);
                }
                match move_data.category {
                    crate::state::dex_data::MoveCategory::Physical => {
                        target_mon.last_physical_damage_taken = eff_damage;
                        target_mon.last_physical_attacker = Some(attack_slot);
                    }
                    crate::state::dex_data::MoveCategory::Special => {
                        target_mon.last_special_damage_taken = eff_damage;
                        target_mon.last_special_attacker = Some(attack_slot);
                    }
                    crate::state::dex_data::MoveCategory::Status => {}
                }
                // Any-category tracker for Metal Burst / Comeuppance.
                target_mon.last_damage_taken = eff_damage;
                target_mon.last_damage_attacker = Some(attack_slot);
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

            simulator_helpers::handle_unfreeze_on_damage(target_mon, move_data.thaws_target, &move_data.pokemon_type, eff_damage);

            if *move_name == PokemonMove::Uproar {
                if let Some(crate::state::dex_data::Status::Sleep(_)) = target_mon.status {
                    target_mon.status = None;
                }
            }

            if *move_name == PokemonMove::SkyDrop {
                simulator_helpers::remove_status_volatile(target_mon, &VolatileStatus::SkyDrop);
            }

            // Smack Down: knock the target out of Fly/Bounce invulnerability, remove
            // MagnetRise and Telekinesis. The SmackDown grounded volatile is applied
            // through the normal secondaries pipeline.
            if *move_name == PokemonMove::SmackDown && eff_damage > 0 {
                let was_airborne = target_mon.volatiles.iter().any(|v| matches!(v,
                    crate::state::pokemon::VolatileStatusState::MoveStatus(VolatileStatus::SemiInvulnerable(m), _)
                    if matches!(m, PokemonMove::Fly | PokemonMove::Bounce)
                ));
                if was_airborne {
                    target_mon.volatiles.retain(|v| !matches!(v,
                        crate::state::pokemon::VolatileStatusState::MoveStatus(VolatileStatus::SemiInvulnerable(m), _)
                        if matches!(m, PokemonMove::Fly | PokemonMove::Bounce)
                    ));
                    bs.action_queue.retain(|a| {
                        if let Action::MoveAction(ma) = a {
                            !(ma.user_slot == target_slot && matches!(ma.move_name, PokemonMove::Fly | PokemonMove::Bounce))
                        } else { true }
                    });
                }
                smack_down_removed_magnet_rise = simulator_helpers::has_status_volatile(target_mon, &VolatileStatus::MagnetRise);
                simulator_helpers::remove_status_volatile(target_mon, &VolatileStatus::MagnetRise);
                smack_down_removed_telekinesis = simulator_helpers::has_status_volatile(target_mon, &VolatileStatus::Telekinesis);
                simulator_helpers::remove_status_volatile(target_mon, &VolatileStatus::Telekinesis);
            }

            let had_destiny_bond = target_mon.fainted
                && simulator_helpers::has_status_volatile(target_mon, &VolatileStatus::DestinyBond);
            if target_mon.fainted {
                simulator_helpers::clear_pokemon_on_faint(target_mon);
                target_fainted = true;
            }
            if had_destiny_bond {
                // Destiny Bond: attacker also faints if they're alive and on the opposing side.
                if attack_slot.player != target_slot.player {
                    if let Some(attacker) = match attack_slot.player {
                        Player::P1 => bs.p1_active_mons.get_mut(attack_slot.slot_index as usize),
                        Player::P2 => bs.p2_active_mons.get_mut(attack_slot.slot_index as usize),
                    } {
                        if !attacker.fainted {
                            attacker.hp = 0;
                            attacker.fainted = true;
                            simulator_helpers::clear_pokemon_on_faint(attacker);
                        }
                    }
                }
            }
        }

        // Emit DamageDealt now that the target_mon borrow has ended.
        if let (Some(observer), Some((new_hp, max_hp))) = (bs.event_observer, damage_dealt_hp_info) {
            let pokemon_hp = if target_slot.player == observer {
                PokemonHP::Number(new_hp)
            } else {
                PokemonHP::Percent(simulator_helpers::hp_to_percent(new_hp, max_hp))
            };
            simulator_helpers::emit(&mut bs, EventKind::DamageDealt { target: target_slot, new_hp: pokemon_hp });

            // Illusion disguise break: any direct damaging hit (eff_damage > 0) dispels Illusion.
            // Grab the true species before the mutable clear so they don't overlap.
            let illusion_species: Option<Species> = simulator_helpers::get_pokemon_at_slot(&bs, target_slot)
                .and_then(|m| m.illusion_disguise.as_ref().map(|_| m.species.clone()));
            if let Some(actual_species) = illusion_species {
                if let Some(mon) = simulator_helpers::get_pokemon_at_slot_mut(&mut bs, target_slot) {
                    mon.illusion_disguise = None;
                }
                simulator_helpers::emit(&mut bs, EventKind::IllusionEnded { slot: target_slot, actual_species });
            }
        }

        // Emit Smack Down's volatile removals now that the target_mon borrow has ended.
        if smack_down_removed_magnet_rise {
            simulator_helpers::emit(&mut bs, EventKind::VolatileEnd { target: target_slot, volatile: VolatileStatus::MagnetRise });
        }
        if smack_down_removed_telekinesis {
            simulator_helpers::emit(&mut bs, EventKind::VolatileEnd { target: target_slot, volatile: VolatileStatus::Telekinesis });
        }

        if target_fainted {
            simulator_helpers::handle_pokemon_faint(&mut bs, target_slot.player, target_slot.slot_index);

            // Moxie: +1 Attack when the attacker directly KOs a target with a damaging move.
            // Only fires if the attacker is still alive (doesn't trigger on recoil-KO).
            // Stacks naturally across multi-target / multi-hit KOs.
            let items_suppressed = simulator_helpers::items_are_suppressed(&bs);
            let attacker_alive = simulator_helpers::get_pokemon_at_slot(&bs, attack_slot)
                .map(|m| !m.fainted && !simulator_helpers::pokemon_ability_is_suppressed(&bs, m)
                    && m.ability == Ability::Moxie)
                .unwrap_or(false);
            if attacker_alive {
                let moxie_delta = if let Some(atk) = simulator_helpers::get_pokemon_at_slot_mut(&mut bs, attack_slot) {
                    simulator_helpers::apply_stat_boost_external(atk, &[1, 0, 0, 0, 0, 0, 0], items_suppressed)
                } else { [0i8; 7] };
                for (boost_idx, &stages) in moxie_delta.iter().enumerate() {
                    if stages != 0 { simulator_helpers::emit(&mut bs, EventKind::BoostChanged { target: attack_slot, boost_idx, stages }); }
                }
            }

            // Eelevate: +1 in the holder's highest non-HP base stat when it directly KOs a
            // target with a damaging move. Fires after Moxie for ordering consistency.
            let eelevate_alive = simulator_helpers::get_pokemon_at_slot(&bs, attack_slot)
                .map(|m| !m.fainted && !simulator_helpers::pokemon_ability_is_suppressed(&bs, m)
                    && m.ability == Ability::Eelevate)
                .unwrap_or(false);
            if eelevate_alive {
                let eelevate_delta = if let Some(atk) = simulator_helpers::get_pokemon_at_slot_mut(&mut bs, attack_slot) {
                    let boost_index = simulator_helpers::highest_boostable_stat_index(atk);
                    let mut boosts = [0i8; 7];
                    boosts[boost_index] = 1;
                    simulator_helpers::apply_stat_boost_external(atk, &boosts, items_suppressed)
                } else { [0i8; 7] };
                for (boost_idx, &stages) in eelevate_delta.iter().enumerate() {
                    if stages != 0 { simulator_helpers::emit(&mut bs, EventKind::BoostChanged { target: attack_slot, boost_idx, stages }); }
                }
            }

            // Fell Stinger: +3 Attack when the user directly KOs a target with this move.
            // Fires regardless of ability; only requires the attacker is still alive.
            if *move_name == PokemonMove::FellStinger {
                let attacker_still_alive = simulator_helpers::get_pokemon_at_slot(&bs, attack_slot)
                    .map(|m| !m.fainted)
                    .unwrap_or(false);
                if attacker_still_alive {
                    let fs_delta = if let Some(atk) = simulator_helpers::get_pokemon_at_slot_mut(&mut bs, attack_slot) {
                        simulator_helpers::apply_stat_boost_external(atk, &[3, 0, 0, 0, 0, 0, 0], items_suppressed)
                    } else { [0i8; 7] };
                    for (boost_idx, &stages) in fs_delta.iter().enumerate() {
                        if stages != 0 { simulator_helpers::emit(&mut bs, EventKind::BoostChanged { target: attack_slot, boost_idx, stages }); }
                    }
                }
            }

            // Destiny Bond: if the attacker was taken down by the target's Destiny Bond,
            // process their faint now (after all KO bonuses so Moxie/Fell Stinger can't
            // fire on a mon that killed itself via Destiny Bond).
            let attacker_destiny_bonded = simulator_helpers::get_pokemon_at_slot(&bs, attack_slot)
                .map(|m| m.fainted)
                .unwrap_or(false);
            if attacker_destiny_bonded {
                simulator_helpers::handle_pokemon_faint(&mut bs, attack_slot.player, attack_slot.slot_index);
            }
        }

        if sand_spit_triggered {
            // Sand Spit also respects Smooth Rock (8 turns instead of 5).
            let dur = simulator_helpers::get_pokemon_at_slot(&bs, attack_slot)
                .map(|m| simulator_helpers::weather_rock_duration(m, &crate::state::dex_data::Weather::Sandstorm))
                .unwrap_or(5);
            simulator_helpers::set_weather(&mut bs, crate::state::dex_data::Weather::Sandstorm, dur);
        }

        if seed_sower_triggered {
            simulator_helpers::set_terrain(&mut bs, crate::state::dex_data::Terrain::GrassyTerrain, 5);
        }

        if matches!(move_name, PokemonMove::IceSpinner | PokemonMove::SteelRoller) {
            simulator_helpers::clear_terrain(&mut bs);
        }

        // Eerie Spell: 80 BP Psychic; on hit remove 3 PP from target's last-used move.
        // The secondary: { onHit } is a JS function body the parser skips; applied here.
        if *move_name == PokemonMove::EerieSpell && eff_damage > 0 {
            if let Some(last_mv) = simulator_helpers::get_pokemon_at_slot(&bs, target_slot)
                .and_then(|t| t.last_used_move.clone())
            {
                let slot_idx = simulator_helpers::get_pokemon_at_slot(&bs, target_slot)
                    .and_then(|t| t.moves.iter().position(|m| m.as_ref() == Some(&last_mv)));
                if let Some(idx) = slot_idx {
                    let current_pp = simulator_helpers::get_pokemon_at_slot(&bs, target_slot)
                        .map(|t| t.move_pp[idx]).unwrap_or(0);
                    if current_pp > 0 {
                        if let Some(tgt) = match target_slot.player {
                            Player::P1 => bs.p1_active_mons.get_mut(target_slot.slot_index as usize),
                            Player::P2 => bs.p2_active_mons.get_mut(target_slot.slot_index as usize),
                        } {
                            tgt.move_pp[idx] = current_pp.saturating_sub(3);
                        }
                    }
                }
            }
        }

        let sec_branches = simulator_helpers::apply_secondary_effects(&bs, attack_slot, target_slot, move_data);
        for (sec_bs, sec_prob) in sec_branches {
            outcomes.push((sec_bs, branch_probability * endure_prob * sec_prob));
        }
    }

    // Fire reactive-ability effects on the holder (target_slot) caused by the attacker's hit.
    // `damage_dealt` is the HP actually lost by the holder (min(raw_damage, pre_hit_hp)),
    // which is what Innards Out and other "deal back damage" abilities need.
    let damage_dealt_clamped = damage.min(target_pre_hit_hp);
    outcomes = simulator_helpers::apply_contact_hit_reactions(
        outcomes, target_slot, attack_slot, move_name, move_data, damage_dealt_clamped, is_crit,
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
    // True for moves that inherently strike multiple times (Beat Up, Bullet Seed, etc.).
    // Used to decide whether to emit HitCount after the per-hit loop.
    let is_genuinely_multihit = *move_name == PokemonMove::BeatUp
        || move_data.multihit_range[0] > 0
        || move_data.multihit_range[1] > 0;
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
                    // Miss branch: emit Missed on the branch copy, then push.
                    let mut miss_state = branch_state.clone();
                    simulator_helpers::emit(&mut miss_state, EventKind::Missed { target: target_slot });
                    next_sequence_branches.push((miss_state, branch_probability * (1.0 - hit_accuracy_probability), shared_roll, hits_landed));
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

        // Apply King's Rock and Stench flinch once per move per target using the combined
        // chance P(flinch) = 1 - 0.9^hits_landed, avoiding per-hit tree blowup.
        for (mut branch_state, branch_probability, _, hits_landed) in sequence_branches {
            // Emit HitCount once per multi-hit resolution, after all individual hits resolve.
            if is_genuinely_multihit {
                simulator_helpers::emit(&mut branch_state, EventKind::HitCount {
                    target: target_slot,
                    hits: hits_landed.min(255) as u8,
                });
            }
            let branches = simulator_helpers::apply_kings_rock_flinch(
                vec![(branch_state, branch_probability)],
                attack_slot,
                target_slot,
                move_data,
                hits_landed,
            );
            let branches = simulator_helpers::apply_stench_flinch(
                branches,
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

/// Build a "move did nothing" outcome, optionally mixing in a 1/3 confusion self-hit branch (Gen VII+).
fn no_effect_outcome(
    state: &BattleState,
    action: &MoveAction,
    confusion_outcomes: &Option<Vec<(MatchState, f64)>>,
) -> Vec<(MatchState, f64)> {
    let mut no_effect_state = state.clone();
    decrement_move_pp(&mut no_effect_state, action.user_slot, &action.move_name, None);

    if let Some(confusion) = confusion_outcomes {
        let mut combined: Vec<(MatchState, f64)> = confusion.iter()
            .map(|(st, p)| (st.clone(), p * (1.0 / 3.0)))
            .collect();
        combined.push((MatchState::BattleState(no_effect_state), 2.0 / 3.0));
        simulator_helpers::coalesce_branches(combined)
    } else {
        vec![(MatchState::BattleState(no_effect_state), 1.0)]
    }
}

/// Build the standard success outcome for a status move that has already mutated `next_state`
/// (PP already decremented). Mirrors the confusion-split bookkeeping used by Attract/Disable:
/// if the user is confused, the move's success branch carries 2/3 weight alongside the 1/3
/// self-hit branches (Gen VII+ confusion probability); otherwise it is the sole 100% outcome.
fn status_move_self_outcome(
    next_state: BattleState,
    confusion_self_hit_outcomes: &Option<Vec<(MatchState, f64)>>,
) -> Vec<(MatchState, f64)> {
    let has_confusion = confusion_self_hit_outcomes.is_some();
    let mut result: Vec<(MatchState, f64)> = Vec::new();
    if let Some(c) = confusion_self_hit_outcomes {
        for (s, p) in c { result.push((s.clone(), p * (1.0 / 3.0))); }
    }
    result.push((MatchState::BattleState(next_state), if has_confusion { 2.0 / 3.0 } else { 1.0 }));
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
    // Reset per-action flag; a Cant from a PREVIOUS action must not bleed into this one.
    next_state.move_was_prevented = false;

    let Some(mut attacker) = simulator_helpers::get_pokemon_at_slot(&next_state, action.user_slot).cloned() else {
        return vec![(MatchState::BattleState(next_state), 1.0)];
    };

    // RandomNormal targeting: select a random live opponent each use (e.g. Outrage/Thrash in
    // doubles). If no concrete target was provided and there are multiple live foes, fork into one
    // equally-weighted branch per foe so the simulator properly represents the random pick.
    // The recursive calls carry a concrete `target_slot`, so this block runs only once per action.
    if move_data.target == crate::state::dex_data::MoveTarget::RandomNormal && action.target_slot.is_none() {
        let foe_player = match action.user_slot.player {
            Player::P1 => Player::P2,
            Player::P2 => Player::P1,
        };
        let live_foes: Vec<FieldSlot> = match foe_player {
            Player::P1 => next_state.p1_active_mons.iter().enumerate()
                .filter(|(_, m)| !m.fainted)
                .map(|(i, _)| FieldSlot { player: foe_player, slot_index: i as u8 })
                .collect(),
            Player::P2 => next_state.p2_active_mons.iter().enumerate()
                .filter(|(_, m)| !m.fainted)
                .map(|(i, _)| FieldSlot { player: foe_player, slot_index: i as u8 })
                .collect(),
        };
        if live_foes.len() > 1 {
            let weight = 1.0 / live_foes.len() as f64;
            let mut out: Vec<(MatchState, f64)> = Vec::new();
            for foe in live_foes {
                let mut pinned_action = action.clone();
                pinned_action.target_slot = Some(foe);
                for (st, p) in possible_damage_outcomes_for_move(
                    state, &pinned_action, move_data, config, move_dex, pokemon_dex, is_called_move,
                ) {
                    out.push((st, p * weight));
                }
            }
            return simulator_helpers::coalesce_branches(out);
        }
        // 0 or 1 live foe — fall through to existing single-target handling.
    }

    // Save pre-move state for potential failure branches (paralysis, sleep, freeze)
    let pre_move_state = next_state.clone();

    // Metronome item: pre-update consecutive_move_count *before* damage branches are created,
    // so user_power_item_multiplier (called inside the damage function) sees the current-turn
    // streak. `attacker` is a clone from line above; damage calculation reads it directly, so
    // we must update both `attacker` and `next_state` (so branches also carry the new count).
    // Uses last_used_move from the *previous* turn (set by decrement_move_pp post-damage).
    // Failure branches (paralysis/sleep) use pre_move_state (old count) — correct, since the
    // move did not execute. The miss reset later nulls last_used_move to break the streak.
    if action.move_name != PokemonMove::Struggle {
        let new_count = if attacker.last_used_move.as_ref() == Some(&action.move_name) {
            attacker.consecutive_move_count.saturating_add(1)
        } else {
            0
        };
        attacker.consecutive_move_count = new_count;
        if let Some(mon) = simulator_helpers::get_pokemon_at_slot_mut(&mut next_state, action.user_slot) {
            mon.consecutive_move_count = new_count;
        }
    }

    simulator_helpers::decrement_move_statuses(&mut attacker);
    write_back_volatiles(&mut next_state, action.user_slot, attacker.volatiles.clone());

    // Using any non-stalling move ends a consecutive-protect streak (the stalling handler below
    // manages the counter on its own success/fail branches).
    if !move_data.stalling_move {
        if let Some(mon) = mon_at_slot_mut(&mut next_state, action.user_slot) {
            mon.stall_counter = 0;
        }
    }

    // Moves with the CantUseTwice flag (Gigaton Hammer, Blood Moon) cannot be selected on
    // consecutive turns. Apply the blocking volatile on every use attempt (hit, miss, or
    // blocked) per Bulbapedia. Driven by MoveFlag::CantUseTwice rather than a move name list.
    if simulator_helpers::move_has_flag(move_data, &crate::state::dex_data::MoveFlag::CantUseTwice) {
        if let Some(mon) = mon_at_slot_mut(&mut next_state, action.user_slot) {
            let already_blocked = mon.volatiles.iter().any(|v| matches!(
                v,
                crate::state::pokemon::VolatileStatusState::MoveStatus(VolatileStatus::CantUseRepeatedly(m), _) if m == &action.move_name
            ));
            if !already_blocked {
                // Duration 1: blocks exactly the next turn's selection, then drops during
                // that turn's decrement_move_statuses. (decrement runs after selection.)
                mon.volatiles.push(crate::state::pokemon::VolatileStatusState::MoveStatus(
                    VolatileStatus::CantUseRepeatedly(action.move_name.clone()), 1,
                ));
            }
        }
    }
    // Ally Switch counter resets when any move OTHER than Ally Switch is used.
    if action.move_name != PokemonMove::AllySwitch {
        if let Some(mon) = mon_at_slot_mut(&mut next_state, action.user_slot) {
            mon.ally_switch_counter = 0;
        }
    }

    // Check Flinch
    if attacker.volatiles.iter().any(|v| matches!(v, crate::state::pokemon::VolatileStatusState::TurnStatus(VolatileStatus::Flinch, _))) {
        // Find and remove charging and semi-invulnerable volatiles
        if let Some(pos) = attacker.volatiles.iter().position(|v| matches!(v, crate::state::pokemon::VolatileStatusState::Charging(_, _) | crate::state::pokemon::VolatileStatusState::MoveStatus(VolatileStatus::SemiInvulnerable(_), _))) {
            attacker.volatiles.remove(pos);
            write_back_volatiles(&mut next_state, action.user_slot, attacker.volatiles.clone());
        }
        // A move prevented by flinching counts as failed (Stomping Tantrum / Micle Berry), and a
        // flinched stalling move couldn't execute, so its protect streak resets too.
        simulator_helpers::note_move_outcome(&mut next_state, action.user_slot, simulator_helpers::MoveOutcome::Cant(CantReason::Flinch));
        if let Some(mon) = mon_at_slot_mut(&mut next_state, action.user_slot) {
            mon.stall_counter = 0;
        }
        // Flinch disrupts a rampage lock. Pass attacks_completed (count-up counter) from `attacker`.
        let rampage_lock_turns = attacker.volatiles.iter().find_map(|v| {
            if let crate::state::pokemon::VolatileStatusState::MoveStatus(VolatileStatus::LockedMove(_), t) = v { Some(*t) } else { None }
        });
        if let Some(turns) = rampage_lock_turns {
            let is_misty = matches!(next_state.terrain, Some(crate::state::dex_data::Terrain::MistyTerrain));
            let confusion_added = if let Some(mon) = mon_at_slot_mut(&mut next_state, action.user_slot) {
                disrupt_rampage_lock(mon, turns, is_misty)
            } else { false };
            simulator_helpers::emit(&mut next_state, EventKind::VolatileEnd {
                target: action.user_slot,
                volatile: VolatileStatus::LockedMove(PokemonMove::Struggle),
            });
            if confusion_added {
                simulator_helpers::emit(&mut next_state, EventKind::VolatileStart {
                    target: action.user_slot,
                    volatile: VolatileStatus::Confusion,
                });
            }
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
        let blocked_by_throat_chop = simulator_helpers::move_has_flag(move_data, &crate::state::dex_data::MoveFlag::Sound)
            && simulator_helpers::has_status_volatile(&attacker, &VolatileStatus::ThroatChop);
        let blocked_by_disable = attacker.volatiles.iter().any(|v| matches!(
            v,
            crate::state::pokemon::VolatileStatusState::MoveStatus(VolatileStatus::Disable(m), _) if *m == action.move_name
        ));
        // Imprison: if any opposing active mon holds the Imprison volatile AND knows this move,
        // the move is blocked (no PP consumed, counts as failed).
        let opponent_player = match action.user_slot.player { Player::P1 => Player::P2, Player::P2 => Player::P1 };
        let blocked_by_imprison = {
            let opp_mons = match opponent_player {
                Player::P1 => &next_state.p1_active_mons,
                Player::P2 => &next_state.p2_active_mons,
            };
            opp_mons.iter().any(|m| !m.fainted
                && simulator_helpers::has_status_volatile(m, &VolatileStatus::Imprison)
                && m.moves.iter().any(|slot| slot.as_ref() == Some(&action.move_name)))
        };
        if blocked_by_taunt || blocked_by_throat_chop || blocked_by_disable || blocked_by_imprison {
            // Determine the most specific Cant reason (checked in priority order).
            let cant_reason = if blocked_by_taunt { CantReason::Taunt }
                else if blocked_by_throat_chop { CantReason::ThroatChop }
                else if blocked_by_disable { CantReason::Disable }
                else { CantReason::Imprison };
            simulator_helpers::note_move_outcome(&mut next_state, action.user_slot, simulator_helpers::MoveOutcome::Cant(cant_reason));
            if let Some(mon) = mon_at_slot_mut(&mut next_state, action.user_slot) {
                mon.stall_counter = 0;
            }
            // Disable/Taunt/ThroatChop disrupts a rampage lock.
            let rampage_lock_turns = attacker.volatiles.iter().find_map(|v| {
                if let crate::state::pokemon::VolatileStatusState::MoveStatus(VolatileStatus::LockedMove(_), t) = v { Some(*t) } else { None }
            });
            if let Some(turns) = rampage_lock_turns {
                let is_misty = matches!(next_state.terrain, Some(crate::state::dex_data::Terrain::MistyTerrain));
                let confusion_added = if let Some(mon) = mon_at_slot_mut(&mut next_state, action.user_slot) {
                    disrupt_rampage_lock(mon, turns, is_misty)
                } else { false };
                simulator_helpers::emit(&mut next_state, EventKind::VolatileEnd {
                    target: action.user_slot,
                    volatile: VolatileStatus::LockedMove(PokemonMove::Struggle),
                });
                if confusion_added {
                    simulator_helpers::emit(&mut next_state, EventKind::VolatileStart {
                        target: action.user_slot,
                        volatile: VolatileStatus::Confusion,
                    });
                }
            }
            return vec![(MatchState::BattleState(next_state), 1.0)];
        }
    }

    // Handle charging and semi-invulnerability mechanics
    if let Some(outcomes) = handle_charging_and_semi_invulnerability(&state, &mut attacker, action, move_data, &mut next_state) {
        return outcomes;
    }

    // Check if the move has the Recharge flag
    let move_has_recharge = simulator_helpers::move_has_flag(move_data, &crate::state::dex_data::MoveFlag::Recharge);

    // Struggle has no moveset slot — it's forced when no usable move exists.
    // For all other moves, locate the PP slot; bail if the move isn't in the moveset.
    let pp_slot = attacker
        .moves
        .iter()
        .position(|move_entry| move_entry.as_ref() == Some(&action.move_name));

    let is_struggle = action.move_name == PokemonMove::Struggle;
    // Called moves (Copycat, Sleep Talk, etc.) may not be in the attacker's moveset.
    if pp_slot.is_none() && !is_struggle && !is_called_move {
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
        decrement_move_pp(&mut next_state, action.user_slot, &action.move_name, Some(move_data));
        return vec![(MatchState::BattleState(next_state), 1.0)];
    }

    // Protean / Libero: change the attacker's type to the move's effective type before use.
    // Fires once per switch-in (tracked by ProteanActivated volatile), not when Terastallized,
    // not for Struggle, and not if already the same single type as the move.
    if move_name != PokemonMove::Struggle {
        let protean_result = simulator_helpers::get_pokemon_at_slot(&next_state, action.user_slot)
            .and_then(|mon| {
                if mon.is_tera { return None; }
                if simulator_helpers::pokemon_ability_is_suppressed(&next_state, mon) { return None; }
                if !matches!(mon.ability, Ability::Protean | Ability::Libero) { return None; }
                if simulator_helpers::has_status_volatile(mon, &VolatileStatus::ProteanActivated) { return None; }
                let move_type = simulator_helpers::effective_move_type(&next_state, mon, move_data);
                // Skip if already a single type matching the move type
                if mon.types.len() == 1 && mon.types[0] == move_type { return None; }
                Some(move_type)
            });
        if let Some(new_type) = protean_result {
            if let Some(mon) = simulator_helpers::get_pokemon_at_slot_mut(&mut next_state, action.user_slot) {
                mon.types = vec![new_type.clone()];
                mon.volatiles.retain(|v| !matches!(v,
                    crate::state::pokemon::VolatileStatusState::TurnStatus(VolatileStatus::ForestsCurse, _)
                    | crate::state::pokemon::VolatileStatusState::TurnStatus(VolatileStatus::TrickorTreat, _)
                ));
                mon.volatiles.push(crate::state::pokemon::VolatileStatusState::TurnStatus(VolatileStatus::ProteanActivated, 0));
            }
            simulator_helpers::emit(&mut next_state, EventKind::TypeChanged {
                slot: action.user_slot,
                new_types: vec![new_type],
            });
        }
    }

    if attacker.volatiles.iter().any(|volatile| matches!(volatile, crate::state::pokemon::VolatileStatusState::TurnStatus(VolatileStatus::SkyDrop, _))) {
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
            // Move fails: no PP cost, no boost.
            simulator_helpers::note_move_outcome(&mut next_state, action.user_slot, simulator_helpers::MoveOutcome::Failed);
            return vec![(MatchState::BattleState(next_state), 1.0)];
        }
    }

    // --- Status pre-move handling: Sleep, Frozen, Paralysis ---
    // Capture current status once (owned) so emit calls can use it without borrow conflicts.
    let pre_move_status = attacker.status.clone();

    // Handle moves that thaw the user on use: thaw before attempt
    if let Some(Status::Frozen(_)) = attacker.status {
        if simulator_helpers::weather_is_sunlight(&next_state)
            || simulator_helpers::move_thaws_user_on_use(&move_data)
            || attacker.ability == Ability::MagmaArmor
        {
            // thaw user
            if let Some(mon) = match action.user_slot.player { Player::P1 => next_state.p1_active_mons.get_mut(action.user_slot.slot_index as usize), Player::P2 => next_state.p2_active_mons.get_mut(action.user_slot.slot_index as usize) } {
                mon.status = None;
            }
            attacker.status = None;
            if let Some(ref s) = pre_move_status {
                simulator_helpers::emit(&mut next_state, EventKind::StatusCured { target: action.user_slot, status: s.clone() });
            }
        }
    }

    // Sleep/Frozen branching: determine chance to fail due to being frozen/asleep
    let mut status_fail_prob: f64 = 0.0;
    if let Some(status) = &attacker.status {
        match status {
            Status::Frozen(n) => {
                // If already handled (thawed), skip
                if *n >= 2 {
                    // guaranteed thaw — copy counter before borrow is released by the mutation
                    let frozen_n_copy = *n;
                    if let Some(mon) = match action.user_slot.player { Player::P1 => next_state.p1_active_mons.get_mut(action.user_slot.slot_index as usize), Player::P2 => next_state.p2_active_mons.get_mut(action.user_slot.slot_index as usize) } {
                        mon.status = None;
                    }
                    attacker.status = None;
                    simulator_helpers::emit(&mut next_state, EventKind::StatusCured { target: action.user_slot, status: Status::Frozen(frozen_n_copy) });
                } else {
                    // 25% chance to thaw and execute
                    status_fail_prob = 0.75;
                    // increment counter in pre_move_state for failure branch
                    if let Some(_mon) = match action.user_slot.player { Player::P1 => pre_move_state.p1_active_mons.get(action.user_slot.slot_index as usize), Player::P2 => pre_move_state.p2_active_mons.get(action.user_slot.slot_index as usize) } {
                        // we'll adjust failure branch later
                    }
                    // For success branch, remove status in next_state
                    let frozen_status_copy = Status::Frozen(*n);
                    if let Some(mon) = match action.user_slot.player { Player::P1 => next_state.p1_active_mons.get_mut(action.user_slot.slot_index as usize), Player::P2 => next_state.p2_active_mons.get_mut(action.user_slot.slot_index as usize) } {
                        mon.status = None;
                    }
                    attacker.status = None;
                    simulator_helpers::emit(&mut next_state, EventKind::StatusCured { target: action.user_slot, status: frozen_status_copy });
                }
            }
            Status::Sleep(n) => {
                // Early Bird halves the sleep duration (round down), effectively waking at n>=1
                // instead of n>=2. Rest is also affected: 2 turns → 1 turn.
                let early_bird = !simulator_helpers::pokemon_ability_is_suppressed(&next_state, &attacker)
                    && attacker.ability == Ability::EarlyBird;
                let wake_threshold: u8 = if early_bird { 1 } else { 2 };
                if *n >= wake_threshold {
                    let sleep_n_copy = *n; // copy counter before borrow is released by the mutation
                    if let Some(mon) = match action.user_slot.player { Player::P1 => next_state.p1_active_mons.get_mut(action.user_slot.slot_index as usize), Player::P2 => next_state.p2_active_mons.get_mut(action.user_slot.slot_index as usize) } {
                        mon.status = None;
                    }
                    attacker.status = None;
                    simulator_helpers::emit(&mut next_state, EventKind::StatusCured { target: action.user_slot, status: Status::Sleep(sleep_n_copy) });
                } else {
                    // If the move is usable while asleep (Snore), allow it to execute regardless of wake roll
                    if move_data.sleep_usable {
                        // do not set a fail probability; status remains unchanged
                    } else {
                        // First action after sleep always fails; second action has a 1/3 wake chance.
                        status_fail_prob = if *n == 0 { 1.0 } else { 2.0 / 3.0 };
                        if *n > 0 {
                            let sleep_status_copy = Status::Sleep(*n);
                            if let Some(mon) = match action.user_slot.player { Player::P1 => next_state.p1_active_mons.get_mut(action.user_slot.slot_index as usize), Player::P2 => next_state.p2_active_mons.get_mut(action.user_slot.slot_index as usize) } {
                                mon.status = None; // success branch
                            }
                            attacker.status = None;
                            simulator_helpers::emit(&mut next_state, EventKind::StatusCured { target: action.user_slot, status: sleep_status_copy });
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
                crate::state::battle::change_form(mon, form.clone(), pokemon_dex);
            }
            simulator_helpers::emit(&mut next_state, EventKind::FormeChange {
                slot: action.user_slot,
                into: form.clone(),
                permanent: false,
            });
            // Keep the local attacker copy in sync — this move's damage must already
            // use the new forme's stats.
            crate::state::battle::change_form(&mut attacker, form, pokemon_dex);
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
                    let is_charge = simulator_helpers::move_has_flag(md, &crate::state::dex_data::MoveFlag::Charge);
                    let no_sleep_talk = simulator_helpers::move_has_flag(md, &crate::state::dex_data::MoveFlag::NoSleepTalk);
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
                .map(|(state, probability)| (state, probability * (1.0 / 3.0)))
                .collect::<Vec<_>>();
            combined_confused.extend(combined.into_iter().map(|(state, probability)| (state, probability * (2.0 / 3.0))));
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
            decrement_move_pp(&mut succ, action.user_slot, &action.move_name, Some(move_data));
            if let Some(mon) = mon_at_slot_mut(&mut succ, action.user_slot) {
                mon.volatiles.push(crate::state::pokemon::VolatileStatusState::TurnStatus(vol.clone(), 1));
                mon.stall_counter = mon.stall_counter.saturating_add(1);
                mon.last_move_failed = false;
            }
            simulator_helpers::emit(&mut succ, EventKind::SingleMoveOrTurn {
                slot: action.user_slot,
                move_used: action.move_name.clone(),
            });
            result.push((MatchState::BattleState(succ), p_success));

            // Failure (only possible once the streak has decayed): the move does nothing and the
            // streak resets.
            if p_success < 1.0 {
                let mut fail = next_state.clone();
                decrement_move_pp(&mut fail, action.user_slot, &action.move_name, Some(move_data));
                if let Some(mon) = mon_at_slot_mut(&mut fail, action.user_slot) {
                    mon.stall_counter = 0;
                }
                simulator_helpers::note_move_outcome(&mut fail, action.user_slot, simulator_helpers::MoveOutcome::Failed);
                result.push((MatchState::BattleState(fail), 1.0 - p_success));
            }

            // Fold in the confusion self-hit branches, mirroring Attract/Disable.
            if let Some(c) = &confusion_self_hit_outcomes {
                let mut folded: Vec<(MatchState, f64)> =
                    c.iter().map(|(s, p)| (s.clone(), p * (1.0 / 3.0))).collect();
                folded.extend(result.into_iter().map(|(s, p)| (s, p * (2.0 / 3.0))));
                return simulator_helpers::coalesce_branches(folded);
            }
            return simulator_helpers::coalesce_branches(result);
        }
        // Out-of-scope stalling move (e.g. Max Guard) — fall through to generic handling.
    }

    // Ally Switch: swap the user with their adjacent ally. Success decays 1/3^n per
    // consecutive use (independent from stall_counter). Fails in singles (no ally).
    if move_name == PokemonMove::AllySwitch {
        let user_slot = action.user_slot;
        // Find the ally slot (same player, different slot_index, non-fainted)
        let ally_slot: Option<FieldSlot> = {
            let actives = match user_slot.player {
                Player::P1 => &next_state.p1_active_mons,
                Player::P2 => &next_state.p2_active_mons,
            };
            actives.iter().enumerate()
                .find(|(i, m)| *i as u8 != user_slot.slot_index && !m.fainted)
                .map(|(i, _)| FieldSlot { player: user_slot.player, slot_index: i as u8 })
        };
        let Some(ally_slot) = ally_slot else {
            // Singles or no valid ally — move fails
            simulator_helpers::note_move_outcome(&mut next_state, user_slot, simulator_helpers::MoveOutcome::Failed);
            return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
        };

        let counter = simulator_helpers::get_pokemon_at_slot(&next_state, user_slot)
            .map(|m| m.ally_switch_counter)
            .unwrap_or(0);
        let p_success = 1.0 / 3f64.powi(counter as i32);

        let mut result: Vec<(MatchState, f64)> = Vec::new();

        // Success branch: swap the two active PokémonState entries
        let mut succ = next_state.clone();
        decrement_move_pp(&mut succ, user_slot, &action.move_name, Some(move_data));
        {
            let actives = match user_slot.player {
                Player::P1 => &mut succ.p1_active_mons,
                Player::P2 => &mut succ.p2_active_mons,
            };
            actives.swap(user_slot.slot_index as usize, ally_slot.slot_index as usize);
        }
        // Update counters on both mons (they swapped, so each is now at the other's index)
        {
            let actives = match user_slot.player {
                Player::P1 => &mut succ.p1_active_mons,
                Player::P2 => &mut succ.p2_active_mons,
            };
            // After the swap, the user's mon is now at ally_slot.slot_index
            if let Some(m) = actives.get_mut(ally_slot.slot_index as usize) {
                m.ally_switch_counter = m.ally_switch_counter.saturating_add(1);
                m.last_move_failed = false;
            }
        }
        // Retarget queued foe actions: if a foe's queued move was targeting user_slot,
        // it now hits the ally (who slid into user_slot after the swap).
        for queued in succ.action_queue.iter_mut() {
            if let Action::MoveAction(ma) = queued {
                if let Some(ref mut ts) = ma.target_slot {
                    if *ts == user_slot { *ts = ally_slot; }
                    else if *ts == ally_slot { *ts = user_slot; }
                }
            }
        }
        result.push((MatchState::BattleState(succ), p_success));

        // Failure branch
        if p_success < 1.0 {
            let mut fail = next_state.clone();
            decrement_move_pp(&mut fail, user_slot, &action.move_name, Some(move_data));
            if let Some(m) = mon_at_slot_mut(&mut fail, user_slot) {
                m.ally_switch_counter = 0;
            }
            simulator_helpers::note_move_outcome(&mut fail, user_slot, simulator_helpers::MoveOutcome::Failed);
            result.push((MatchState::BattleState(fail), 1.0 - p_success));
        }

        if let Some(c) = &confusion_self_hit_outcomes {
            let mut folded: Vec<(MatchState, f64)> =
                c.iter().map(|(s, p)| (s.clone(), p * (1.0 / 3.0))).collect();
            folded.extend(result.into_iter().map(|(s, p)| (s, p * (2.0 / 3.0))));
            return simulator_helpers::coalesce_branches(folded);
        }
        return simulator_helpers::coalesce_branches(result);
    }

    // Resolve target list based on move's targeting type
    let mut target_slots = if move_name == PokemonMove::ExpandingForce
        && simulator_helpers::pokemon_is_on_terrain(&next_state, &attacker, &crate::state::dex_data::Terrain::PsychicTerrain)
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
            Some(slot) => {
                // Faint redirection (doubles): if the chosen target has fainted before this
                // move executes, single-target moves automatically retarget to a remaining
                // valid target. The move only fails when NO valid target remains.
                // (Bulbapedia, Double Battle: a move fails for lack of target only when all
                // eligible targets are knocked out before it attacks.)
                let target_fainted = simulator_helpers::get_pokemon_at_slot(&next_state, slot)
                    .map_or(true, |m| m.fainted);
                if target_fainted {
                    let redirected = simulator_helpers::resolve_move_targets(
                        &next_state, action.user_slot, &move_data.target);
                    if redirected.is_empty() {
                        return vec![(MatchState::BattleState(next_state), 1.0)];
                    }
                    redirected
                } else {
                    vec![slot]
                }
            }
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
        && !(move_name == PokemonMove::ExpandingForce && simulator_helpers::pokemon_is_on_terrain(&next_state, &attacker, &crate::state::dex_data::Terrain::PsychicTerrain))
    {
        target_slots = simulator_helpers::check_and_apply_redirection(&next_state, action.user_slot, target_slots, Some(move_data));
    }

    // Substitute: lose ¼ max HP to create a dummy that absorbs incoming damage.
    // Fails if the user already has a Substitute, HP ≤ cost, or max HP < 4 (Shedinja / tiny).
    if move_name == PokemonMove::Substitute {
        let attacker_slot = action.user_slot;
        let check = simulator_helpers::get_pokemon_at_slot(&next_state, attacker_slot)
            .map(|m| {
                let max_hp = m.stats[0].max(1);
                let cost = max_hp / 4;
                let already_has = simulator_helpers::has_status_volatile(m, &VolatileStatus::Substitute(0));
                (cost, m.hp, already_has)
            });
        if let Some((cost, hp, already_has)) = check {
            if cost == 0 || hp <= cost || already_has {
                // Failure: last_move_failed is set by the status-move diff check below.
                return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
            }
            let attacker_env = simulator_helpers::berry_env(&next_state, attacker_slot);
            let as_ = simulator_helpers::abilities_are_suppressed(&next_state);
            if let Some(m) = mon_at_slot_mut(&mut next_state, attacker_slot) {
                simulator_helpers::take_damage(m, cost, attacker_env, as_);
                m.volatiles.push(crate::state::pokemon::VolatileStatusState::TurnStatus(
                    VolatileStatus::Substitute(cost), 0,
                ));
            }
        }
        decrement_move_pp(&mut next_state, attacker_slot, &action.move_name, Some(move_data));
        return status_move_self_outcome(next_state, &confusion_self_hit_outcomes);
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
        decrement_move_pp(&mut next_state, action.user_slot, &action.move_name, Some(move_data));
        return status_move_self_outcome(next_state, &confusion_self_hit_outcomes);
    }

    if target_slots.is_empty() {
        return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
    }

    // Counter / Mirror Coat / Metal Burst / Comeuppance: fail if no qualifying damage was
    // received this turn. The damage computation in calculate_damage_outcomes_for_target
    // reads `attacker.last_*_damage_taken`; that field is 0 when no hit was received.
    // We gate here so the move is cleanly flagged as failed (sets last_move_failed, costs PP).
    {
        let user = simulator_helpers::get_pokemon_at_slot(&next_state, action.user_slot);
        let fails = match move_name {
            PokemonMove::Counter     => user.map_or(true, |m| m.last_physical_damage_taken == 0),
            PokemonMove::MirrorCoat  => user.map_or(true, |m| m.last_special_damage_taken == 0),
            PokemonMove::MetalBurst | PokemonMove::Comeuppance
                                     => user.map_or(true, |m| m.last_damage_taken == 0),
            _ => false,
        };
        if fails {
            simulator_helpers::note_move_outcome(&mut next_state, action.user_slot, simulator_helpers::MoveOutcome::Failed);
            return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
        }
    }

    // Counter / Mirror Coat / Metal Burst / Comeuppance: override target to the slot that
    // last damaged the user this turn, ignoring the player's chosen target.
    {
        let override_slot: Option<FieldSlot> = match move_name {
            PokemonMove::Counter => simulator_helpers::get_pokemon_at_slot(&next_state, action.user_slot)
                .and_then(|m| m.last_physical_attacker),
            PokemonMove::MirrorCoat => simulator_helpers::get_pokemon_at_slot(&next_state, action.user_slot)
                .and_then(|m| m.last_special_attacker),
            PokemonMove::MetalBurst | PokemonMove::Comeuppance
                => simulator_helpers::get_pokemon_at_slot(&next_state, action.user_slot)
                .and_then(|m| m.last_damage_attacker),
            _ => None,
        };
        if let Some(slot) = override_slot {
            target_slots = vec![slot];
        }
    }

    // Focus Punch: fails (no damage, no PP consumed) if the user was hit by a damaging move
    // before their action this turn. Detected via `damaged_this_turn` (set whenever any
    // direct hit deals effective damage). Status moves and Substitute-absorbed hits do NOT
    // break focus (Substitute absorbs the hit before the attacker's mon records damage).
    // Gen V+: PP is not consumed on a broken focus — return the pre-move state unchanged.
    if move_name == PokemonMove::FocusPunch {
        // FocusPunchCharging is present only if the user was alive at turn-start and selected
        // Focus Punch. If damaged_this_turn is set, focus was broken.
        let focus_broken = simulator_helpers::get_pokemon_at_slot(&next_state, action.user_slot)
            .map(|m| m.damaged_this_turn
                && simulator_helpers::has_status_volatile(m, &VolatileStatus::FocusPunchCharging))
            .unwrap_or(false);
        if focus_broken {
            // Remove the FocusPunchCharging volatile and return — no PP cost, no damage.
            if let Some(mon) = mon_at_slot_mut(&mut next_state, action.user_slot) {
                simulator_helpers::remove_status_volatile(mon, &VolatileStatus::FocusPunchCharging);
            }
            simulator_helpers::note_move_outcome(&mut next_state, action.user_slot, simulator_helpers::MoveOutcome::Cant(CantReason::FocusPunch));
            simulator_helpers::emit(&mut next_state, EventKind::VolatileEnd {
                target: action.user_slot,
                volatile: VolatileStatus::FocusPunchCharging,
            });
            return vec![(MatchState::BattleState(next_state), 1.0)];
        }
    }

    // Dream Eater: only works against a sleeping target (or Comatose, which permanently acts as
    // if asleep). If the target is not asleep and does not have Comatose, the move fails
    // completely (no damage, sets last_move_failed). Abilities suppression via Mold Breaker is
    // NOT considered here — Comatose's pseudo-sleep makes the move work even when sleep is faked.
    if move_name == PokemonMove::DreamEater {
        let target_slot = target_slots[0];
        let target_ok = simulator_helpers::get_pokemon_at_slot(&next_state, target_slot)
            .map(|m| matches!(m.status, Some(Status::Sleep(_))) || m.ability == Ability::Comatose)
            .unwrap_or(false);
        if !target_ok {
            simulator_helpers::note_move_outcome(&mut next_state, action.user_slot, simulator_helpers::MoveOutcome::Failed);
            return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
        }
    }

    // Memento: lower target's Attack and Sp. Atk by 2 stages each, then the user faints.
    // User does NOT faint if the move fails (Protect, no target — the is_empty check above
    // handles the no-target case). In newest-gen, Substitute does not block status moves that
    // cause stat drops, so the drops pass through; the user still faints regardless of whether
    // the drops were neutralised by Clear Body / White Smoke / etc.
    // Magic Bounce: Memento is uniquely NOT reflected by Magic Bounce (even once implemented),
    // so no Magic Bounce check is needed here.
    if move_name == PokemonMove::Memento {
        let target_slot = target_slots[0];
        let target = simulator_helpers::get_pokemon_at_slot(&next_state, target_slot).cloned();
        let Some(target) = target else {
            return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
        };
        // Protect blocks Memento: user does NOT faint if the target is protected.
        if simulator_helpers::protect_blocks_move(
            &next_state, action.user_slot, target_slot, &target, move_data, false,
        ).is_some() {
            simulator_helpers::emit(&mut next_state, EventKind::Blocked { target: target_slot });
            return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
        }
        // Apply -2 Atk / -2 Sp. Atk to the target. Respects Clear Body, White Smoke,
        // Mirror Armor, etc. The user faints even if all drops are absorbed.
        let items_suppressed = simulator_helpers::items_are_suppressed(&next_state);
        simulator_helpers::apply_opponent_stat_drop(
            &mut next_state, target_slot, action.user_slot, [-2, 0, -2, 0, 0, 0, 0],
            items_suppressed, false,
        );
        // Faint the user unconditionally now that the move has connected.
        let user_player = action.user_slot.player;
        let user_slot_index = action.user_slot.slot_index;
        if let Some(user_mon) = mon_at_slot_mut(&mut next_state, action.user_slot) {
            user_mon.hp = 0;
            user_mon.fainted = true;
            simulator_helpers::clear_pokemon_on_faint(user_mon);
        }
        simulator_helpers::handle_pokemon_faint(&mut next_state, user_player, user_slot_index);
        decrement_move_pp(&mut next_state, action.user_slot, &action.move_name, Some(move_data));
        return status_move_self_outcome(next_state, &confusion_self_hit_outcomes);
    }

    // Healing Wish: user faints; the Pokémon that enters its slot is fully healed and
    // status-cured. The move fails (user does NOT faint) if there is no healthy benched
    // Pokémon to send in as a replacement.
    if move_name == PokemonMove::HealingWish {
        if !has_healthy_bench(&next_state, action.user_slot.player) {
            return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
        }
        // Set the slot condition so the replacement entering this slot gets healed on entry.
        let slot_idx = action.user_slot.slot_index as usize;
        let conds = match action.user_slot.player {
            Player::P1 => &mut next_state.p1_slot_conditions,
            Player::P2 => &mut next_state.p2_slot_conditions,
        };
        if let Some(slot_conds) = conds.get_mut(slot_idx) {
            // Remove any pre-existing HealingWish condition on this slot (only one at a time).
            slot_conds.retain(|sc| !matches!(sc, crate::state::dex_data::SlotCondition::HealingWish));
            slot_conds.push(crate::state::dex_data::SlotCondition::HealingWish);
        }
        simulator_helpers::emit(&mut next_state, EventKind::SlotConditionStart {
            slot: action.user_slot,
            condition: crate::state::dex_data::SlotCondition::HealingWish,
        });
        // Faint the user.
        let user_player = action.user_slot.player;
        let user_slot_index = action.user_slot.slot_index;
        if let Some(user_mon) = mon_at_slot_mut(&mut next_state, action.user_slot) {
            user_mon.hp = 0;
            user_mon.fainted = true;
            simulator_helpers::clear_pokemon_on_faint(user_mon);
        }
        simulator_helpers::handle_pokemon_faint(&mut next_state, user_player, user_slot_index);
        decrement_move_pp(&mut next_state, action.user_slot, &action.move_name, Some(move_data));
        return status_move_self_outcome(next_state, &confusion_self_hit_outcomes);
    }

    // Attract move: apply infatuation to the target.
    // Magic Bounce: Attract targeting an opponent can be bounced back.
    if move_name == PokemonMove::Attract {
        let target_slot = target_slots[0];
        if target_slot.player != action.user_slot.player
            && !simulator_helpers::attacker_breaks_mold(&next_state, &attacker)
        {
            let mb = simulator_helpers::get_pokemon_at_slot(&next_state, target_slot)
                .map_or(false, |t| !simulator_helpers::pokemon_ability_is_suppressed(&next_state, t) && t.ability == Ability::MagicBounce);
            if mb {
                let applied = simulator_helpers::try_apply_attract(&mut next_state, target_slot, action.user_slot);
                if applied {
                    decrement_move_pp(&mut next_state, action.user_slot, &action.move_name, Some(move_data));
                    return vec![(MatchState::BattleState(next_state), 1.0)];
                }
                return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
            }
        }
        let applied = simulator_helpers::try_apply_attract(&mut next_state, action.user_slot, target_slot);
        if applied {
            decrement_move_pp(&mut next_state, action.user_slot, &action.move_name, Some(move_data));
            let has_confusion = confusion_self_hit_outcomes.is_some();
            let mut result = Vec::new();
            if let Some(ref c) = confusion_self_hit_outcomes {
                for (s, p) in c { result.push((s.clone(), p * (1.0 / 3.0))); }
            }
            result.push((MatchState::BattleState(next_state), if has_confusion { 2.0 / 3.0 } else { 1.0 }));
            return simulator_helpers::coalesce_branches(result);
        }
        return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
    }

    // Disable move: disable the target's last-used move for 4 turns.
    if move_name == PokemonMove::Disable {
        let target_slot = target_slots[0];
        // Magic Bounce: Disable targeting an opponent is reflected.
        if target_slot.player != action.user_slot.player
            && !simulator_helpers::attacker_breaks_mold(&next_state, &attacker)
        {
            let mb = simulator_helpers::get_pokemon_at_slot(&next_state, target_slot)
                .map_or(false, |t| !simulator_helpers::pokemon_ability_is_suppressed(&next_state, t) && t.ability == Ability::MagicBounce);
            if mb {
                let applied = simulator_helpers::try_apply_disable(&mut next_state, target_slot, action.user_slot);
                if applied {
                    decrement_move_pp(&mut next_state, action.user_slot, &action.move_name, Some(move_data));
                    return vec![(MatchState::BattleState(next_state), 1.0)];
                }
                return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
            }
        }
        let applied = simulator_helpers::try_apply_disable(&mut next_state, action.user_slot, target_slot);
        if applied {
            decrement_move_pp(&mut next_state, action.user_slot, &action.move_name, Some(move_data));
            let has_confusion = confusion_self_hit_outcomes.is_some();
            let mut result = Vec::new();
            if let Some(ref c) = confusion_self_hit_outcomes {
                for (s, p) in c { result.push((s.clone(), p * (1.0 / 3.0))); }
            }
            result.push((MatchState::BattleState(next_state), if has_confusion { 2.0 / 3.0 } else { 1.0 }));
            return simulator_helpers::coalesce_branches(result);
        }
        return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
    }

    // Type-changing moves (Soak, Magic Powder, Forest's Curse, Trick-or-Treat, Reflect Type,
    // Electrify). Each mutates a Pokémon's active types — or, for Electrify, the type of the
    // target's move this turn — and follows the standard status-move shape: apply via a
    // helper, decrement PP and coalesce on success, else `no_effect_outcome`.
    if matches!(
        move_name,
        PokemonMove::Soak
            | PokemonMove::MagicPowder
            | PokemonMove::ForestsCurse
            | PokemonMove::TrickorTreat
            | PokemonMove::ReflectType
            | PokemonMove::Electrify
    ) {
        let target_slot = target_slots[0];
        let Some(target) = simulator_helpers::get_pokemon_at_slot(&next_state, target_slot).cloned() else {
            return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
        };
        // Protect-family blocking (all six carry the protect flag; King's Shield lets status
        // moves through, handled inside protect_blocks_move).
        if simulator_helpers::protect_blocks_move(
            &next_state, action.user_slot, target_slot, &target, move_data, false,
        ).is_some() {
            simulator_helpers::emit(&mut next_state, EventKind::Blocked { target: target_slot });
            return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
        }
        // Magic Powder is a powder move: Grass-types, Overcoat, and Safety Goggles are immune.
        // Mold Breaker bypasses Overcoat only.
        if move_name == PokemonMove::MagicPowder
            && simulator_helpers::move_has_flag(move_data, &crate::state::dex_data::MoveFlag::Powder)
            && simulator_helpers::is_immune_to_powder(&next_state, &target, Some(&attacker))
        {
            return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
        }
        let applied = match move_name {
            PokemonMove::Soak => simulator_helpers::try_set_single_type(
                &mut next_state, target_slot, PokemonType::Water,
            ),
            PokemonMove::MagicPowder => simulator_helpers::try_set_single_type(
                &mut next_state, target_slot, PokemonType::Psychic,
            ),
            PokemonMove::ForestsCurse => simulator_helpers::try_add_type(
                &mut next_state, target_slot, PokemonType::Grass,
                VolatileStatus::ForestsCurse, PokemonType::Ghost, VolatileStatus::TrickorTreat,
            ),
            PokemonMove::TrickorTreat => simulator_helpers::try_add_type(
                &mut next_state, target_slot, PokemonType::Ghost,
                VolatileStatus::TrickorTreat, PokemonType::Grass, VolatileStatus::ForestsCurse,
            ),
            PokemonMove::ReflectType => simulator_helpers::try_apply_reflect_type(
                &mut next_state, action.user_slot, target_slot,
            ),
            PokemonMove::Electrify => {
                simulator_helpers::apply_electrify(&mut next_state, action.user_slot, target_slot);
                true
            }
            _ => unreachable!(),
        };
        if !applied {
            return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
        }
        // Emit TypeChanged for the affected slot (user for Reflect Type, target for all others).
        {
            let changed_slot = if move_name == PokemonMove::ReflectType { action.user_slot } else { target_slot };
            let new_types = simulator_helpers::get_pokemon_at_slot(&next_state, changed_slot)
                .map(|m| m.types.clone())
                .unwrap_or_default();
            if !new_types.is_empty() {
                simulator_helpers::emit(&mut next_state, EventKind::TypeChanged { slot: changed_slot, new_types });
            }
        }
        decrement_move_pp(&mut next_state, action.user_slot, &action.move_name, Some(move_data));
        let has_confusion = confusion_self_hit_outcomes.is_some();
        let mut result = Vec::new();
        if let Some(ref c) = confusion_self_hit_outcomes {
            for (s, p) in c { result.push((s.clone(), p * (1.0 / 3.0))); }
        }
        result.push((MatchState::BattleState(next_state), if has_confusion { 2.0 / 3.0 } else { 1.0 }));
        return simulator_helpers::coalesce_branches(result);
    }

    // Trick / Switcheroo: swap held items with the target. Fails if neither holds an item,
    // if the target is behind a Substitute or has Sticky Hold, or if either item is locked.
    if matches!(move_name, PokemonMove::Trick | PokemonMove::Switcheroo) {
        let target_slot = target_slots[0];
        let Some(target) = simulator_helpers::get_pokemon_at_slot(&next_state, target_slot).cloned() else {
            return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
        };
        if simulator_helpers::protect_blocks_move(
            &next_state, action.user_slot, target_slot, &target, move_data, false,
        ).is_some()
            || simulator_helpers::has_status_volatile(&target, &VolatileStatus::Substitute(0))
        {
            simulator_helpers::emit(&mut next_state, EventKind::Blocked { target: target_slot });
            return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
        }
        if !simulator_helpers::try_swap_items(&mut next_state, action.user_slot, target_slot) {
            return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
        }
        decrement_move_pp(&mut next_state, action.user_slot, &action.move_name, Some(move_data));
        return status_move_self_outcome(next_state, &confusion_self_hit_outcomes);
    }

    // Recycle: restore the user's most recently consumed item. Fails if the user already
    // holds an item or has nothing to recover.
    if move_name == PokemonMove::Recycle {
        if !simulator_helpers::recover_consumed_item(&mut next_state, action.user_slot) {
            return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
        }
        decrement_move_pp(&mut next_state, action.user_slot, &action.move_name, Some(move_data));
        return status_move_self_outcome(next_state, &confusion_self_hit_outcomes);
    }

    // Teatime: every Pokémon on the field eats its held Berry (bypassing Substitute and
    // Unnerve). Fails only if no Pokémon on the field is holding a Berry.
    if move_name == PokemonMove::Teatime {
        let mut any_ate = false;
        let n_p1 = next_state.p1_active_mons.len();
        let n_p2 = next_state.p2_active_mons.len();
        for (player, n) in [(Player::P1, n_p1), (Player::P2, n_p2)] {
            for i in 0..n {
                let slot = FieldSlot { player, slot_index: i as u8 };
                if simulator_helpers::force_eat_held_berry(&mut next_state, slot, true) {
                    any_ate = true;
                }
            }
        }
        if !any_ate {
            return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
        }
        decrement_move_pp(&mut next_state, action.user_slot, &action.move_name, Some(move_data));
        return status_move_self_outcome(next_state, &confusion_self_hit_outcomes);
    }

    // Corrosive Gas: destroy the held items of all adjacent foes (respecting Sticky Hold,
    // Substitute, Protect and locked items). The move itself always executes.
    if move_name == PokemonMove::CorrosiveGas {
        let opposing_player = match action.user_slot.player {
            Player::P1 => Player::P2,
            Player::P2 => Player::P1,
        };
        let n_opposing = match opposing_player {
            Player::P1 => next_state.p1_active_mons.len(),
            Player::P2 => next_state.p2_active_mons.len(),
        };
        for i in 0..n_opposing {
            let slot = FieldSlot { player: opposing_player, slot_index: i as u8 };
            let Some(target) = simulator_helpers::get_pokemon_at_slot(&next_state, slot).cloned() else {
                continue;
            };
            if simulator_helpers::protect_blocks_move(
                &next_state, action.user_slot, slot, &target, move_data, false,
            ).is_some()
                || simulator_helpers::has_status_volatile(&target, &VolatileStatus::Substitute(0))
            {
                simulator_helpers::emit(&mut next_state, EventKind::Blocked { target: slot });
                continue;
            }
            simulator_helpers::try_remove_item(&mut next_state, slot);
        }
        decrement_move_pp(&mut next_state, action.user_slot, &action.move_name, Some(move_data));
        return status_move_self_outcome(next_state, &confusion_self_hit_outcomes);
    }

    // Helping Hand: in singles there is no ally slot, so the move always fails.
    // The ×1.5 boost read-site already exists (simulator_helpers.rs HelpingHand volatile
    // check); the doubles applier can be added here when doubles support is implemented.
    if move_name == PokemonMove::HelpingHand {
        return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
    }

    // Lock-On / Mind Reader: lock the user's accuracy onto the target for the next turn.
    // Applies LockedOn volatile (MoveStatus, 2 turns) to the user, targeting the opponent by
    // mon_id. Fails if the user is already locked on. Bypasses evasion and semi-invulnerability
    // for the user's next move; does NOT bypass Protect or type immunity.
    if matches!(move_name, PokemonMove::LockOn | PokemonMove::MindReader) {
        let target_slot = target_slots[0];
        let target_mon_id = simulator_helpers::get_pokemon_at_slot(&next_state, target_slot)
            .map(|m| m.mon_id)
            .unwrap_or(u8::MAX);
        let already_locked = simulator_helpers::get_pokemon_at_slot(&next_state, action.user_slot)
            .map(|m| m.volatiles.iter().any(|v| matches!(v,
                crate::state::pokemon::VolatileStatusState::MoveStatus(VolatileStatus::LockedOn(_), _)
                | crate::state::pokemon::VolatileStatusState::TurnStatus(VolatileStatus::LockedOn(_), _)
            )))
            .unwrap_or(false);
        if already_locked {
            return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
        }
        if let Some(user) = simulator_helpers::get_pokemon_at_slot_mut(&mut next_state, action.user_slot) {
            user.volatiles.push(crate::state::pokemon::VolatileStatusState::MoveStatus(VolatileStatus::LockedOn(target_mon_id), 2));
        }
        decrement_move_pp(&mut next_state, action.user_slot, &action.move_name, Some(move_data));
        return status_move_self_outcome(next_state, &confusion_self_hit_outcomes);
    }

    // Poltergeist: fails if the target does not hold an active item (None, Magic Room, Klutz,
    // Embargo, Neutralizing Gas). The "item being held" check mirrors Showdown's behavior —
    // if the item is suppressed it still counts as present for fail purposes only in
    // certain contexts, but here we use item_is_active for simplicity (consistent with
    // how Fling checks its own item).
    if move_name == PokemonMove::Poltergeist {
        let target_slot = target_slots[0];
        let target_has_item = simulator_helpers::get_pokemon_at_slot(&next_state, target_slot)
            .map(|m| m.item != crate::data::item::Item::None)
            .unwrap_or(false);
        if !target_has_item {
            return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
        }
    }

    // Belch: cannot be used unless the user has eaten a Berry at some point this battle.
    if move_name == PokemonMove::Belch {
        let has_eaten = simulator_helpers::get_pokemon_at_slot(&next_state, action.user_slot)
            .map(|m| m.ate_berry_this_battle)
            .unwrap_or(false);
        if !has_eaten {
            return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
        }
    }

    // Fake Out / First Impression: only usable on the user's very first move after entering
    // battle. `first_move_on_field` covers both regular switch-ins and faint-replacements
    // (unlike `entered_this_turn` which is intentionally false for faint-replacements).
    if matches!(move_name, PokemonMove::FakeOut | PokemonMove::FirstImpression) {
        let eligible = simulator_helpers::get_pokemon_at_slot(&next_state, action.user_slot)
            .map(|m| m.first_move_on_field)
            .unwrap_or(false);
        if !eligible {
            return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
        }
    }

    // Burn Up: fails if the user does not currently have the Fire type. This covers chains
    // (e.g. a second use after the first already stripped the type) and non-Fire users.
    if move_name == PokemonMove::BurnUp {
        let has_fire = simulator_helpers::get_pokemon_at_slot(&next_state, action.user_slot)
            .map(|m| simulator_helpers::pokemon_has_type(m, &PokemonType::Fire))
            .unwrap_or(false);
        if !has_fire {
            return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
        }
    }

    // Snore: only usable while the user is asleep. Mirrors Sleep Talk's gate.
    if move_name == PokemonMove::Snore {
        let is_asleep = simulator_helpers::get_pokemon_at_slot(&next_state, action.user_slot)
            .map(|m| matches!(m.status, Some(crate::state::dex_data::Status::Sleep(_))))
            .unwrap_or(false);
        if !is_asleep {
            return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
        }
    }

    // Last Resort: 140 BP Normal; fails unless the user has ≥2 moves AND has used every
    // move other than Last Resort at least once since being sent in this battle.
    if move_name == PokemonMove::LastResort {
        let fails = simulator_helpers::get_pokemon_at_slot(&next_state, action.user_slot)
            .map(|m| {
                let move_slots: Vec<_> = m.moves.iter().enumerate()
                    .filter_map(|(i, slot)| slot.as_ref().map(|mv| (i, mv)))
                    .collect();
                if move_slots.len() < 2 { return true; }
                move_slots.iter().any(|(i, mv)| {
                    **mv != PokemonMove::LastResort && !m.used_moves_this_field[*i]
                })
            })
            .unwrap_or(true);
        if fails {
            return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
        }
    }

    // Sucker Punch: fails if the target chose a non-damaging (status) move, chose a
    // non-move action (switch), or has already executed their action this turn.
    if move_name == PokemonMove::SuckerPunch {
        let target_slot = target_slots[0];
        let target_queued_move: Option<PokemonMove> = next_state.action_queue.iter().find_map(|a| {
            if let Action::MoveAction(ma) = a {
                if ma.user_slot == target_slot { Some(ma.move_name.clone()) } else { None }
            } else { None }
        });
        let succeeds = match &target_queued_move {
            None => false, // target already acted or chose switch
            Some(mv) => {
                move_dex.get(mv).map_or(false, |d| !matches!(d.category, MoveCategory::Status))
            }
        };
        if !succeeds {
            return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
        }
    }

    // Upper Hand: succeeds only if the target's queued move has positive effective priority
    // and is a damaging move that the target has not yet executed this turn.
    // Per Bulbapedia, ability-granted priority (Prankster, Gale Wings) counts.
    if move_name == PokemonMove::UpperHand {
        let target_slot = target_slots[0];
        let target_queued_move: Option<PokemonMove> = next_state.action_queue.iter().find_map(|a| {
            if let Action::MoveAction(ma) = a {
                if ma.user_slot == target_slot { Some(ma.move_name.clone()) } else { None }
            } else { None }
        });
        let succeeds = match &target_queued_move {
            None => false, // target already acted
            Some(mv) => {
                let Some(md) = move_dex.get(mv) else { return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes); };
                if matches!(md.category, MoveCategory::Status) { false }
                else {
                    let target_mon = simulator_helpers::get_pokemon_at_slot(&next_state, target_slot);
                    let eff_priority = target_mon.map_or(md.priority, |tm| {
                        simulator_helpers::effective_move_priority(&next_state, tm, md)
                    });
                    eff_priority >= 1
                }
            }
        };
        if !succeeds {
            return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
        }
    }

    // Spite: remove 4 PP from the target's most recently used move.
    // Status-move path: apply effect and return (no damage).
    if move_name == PokemonMove::Spite {
        let target_slot = target_slots[0];
        // Protect check: Spite is a status move and does NOT bypass Protect.
        let protected = simulator_helpers::get_pokemon_at_slot(&next_state, target_slot)
            .map(|t| simulator_helpers::protect_blocks_move(&next_state, action.user_slot, target_slot, t, move_data, false).is_some())
            .unwrap_or(false);
        if protected {
            simulator_helpers::emit(&mut next_state, EventKind::Blocked { target: target_slot });
            return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
        }

        let last_mv = simulator_helpers::get_pokemon_at_slot(&next_state, target_slot)
            .and_then(|t| t.last_used_move.clone());
        let Some(last_mv) = last_mv else {
            return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
        };
        // Find the PP slot and verify it has PP remaining.
        let slot_idx = simulator_helpers::get_pokemon_at_slot(&next_state, target_slot)
            .and_then(|t| t.moves.iter().position(|m| m.as_ref() == Some(&last_mv)));
        let Some(slot_idx) = slot_idx else {
            return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
        };
        let current_pp = simulator_helpers::get_pokemon_at_slot(&next_state, target_slot)
            .map(|t| t.move_pp[slot_idx])
            .unwrap_or(0);
        if current_pp == 0 {
            return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
        }
        // Apply: decrement the target's PP by 4 (saturating at 0).
        if let Some(target_mon) = match target_slot.player {
            Player::P1 => next_state.p1_active_mons.get_mut(target_slot.slot_index as usize),
            Player::P2 => next_state.p2_active_mons.get_mut(target_slot.slot_index as usize),
        } {
            target_mon.move_pp[slot_idx] = current_pp.saturating_sub(4);
        }
        decrement_move_pp(&mut next_state, action.user_slot, &action.move_name, Some(move_data));
        return status_move_self_outcome(next_state, &confusion_self_hit_outcomes);
    }

    // Acupressure: raise a random stat by +2. Fails if all 7 stats are already at +6.
    // In this sim, always targets the user (doubles ally-targeting is not implemented).
    if move_name == PokemonMove::Acupressure {
        let user_slot = action.user_slot;
        let boosts = simulator_helpers::get_pokemon_at_slot(&next_state, user_slot)
            .map(|m| m.boosts)
            .unwrap_or_default();
        let eligible: Vec<usize> = (0..7usize).filter(|&i| boosts[i] < 6).collect();
        if eligible.is_empty() {
            return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
        }
        let items_suppressed = simulator_helpers::items_are_suppressed(&next_state);
        let branches: Vec<(BattleState, f64)> = vec![(next_state.clone(), 1.0)];
        let per = 1.0 / eligible.len() as f64;
        let mut new_branches = Vec::new();
        for stat_idx in &eligible {
            for (mut bs, prob) in branches.iter().cloned() {
                let mut delta = [0i8; 7];
                delta[*stat_idx] = 2;
                let contrary_delta = if let Some(mon) = simulator_helpers::get_pokemon_at_slot_mut(&mut bs, user_slot) {
                    simulator_helpers::apply_stat_boost_external(mon, &delta, items_suppressed)
                } else { [0i8; 7] };
                for (boost_idx, &stages) in contrary_delta.iter().enumerate() {
                    if stages != 0 { simulator_helpers::emit(&mut bs, EventKind::BoostChanged { target: user_slot, boost_idx, stages }); }
                }
                new_branches.push((bs, prob * per));
            }
        }
        // consume PP on the original state's branches collapsed
        for (bs, _) in &mut new_branches {
            decrement_move_pp(bs, user_slot, &action.move_name, Some(move_data));
        }
        return new_branches.into_iter()
            .map(|(bs, p)| (MatchState::BattleState(bs), p))
            .collect();
    }

    // Stuff Cheeks: consume held Berry (triggering its normal on-eat effect), then +2 Def.
    // Fails if the user holds no Berry.
    if move_name == PokemonMove::StuffCheeks {
        let user_slot = action.user_slot;
        let has_berry = simulator_helpers::get_pokemon_at_slot(&next_state, user_slot)
            .map(|m| m.item.is_berry() && simulator_helpers::item_is_active(&next_state, m))
            .unwrap_or(false);
        if !has_berry {
            return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
        }
        let env = simulator_helpers::berry_env(&next_state, user_slot);
        let items_suppressed = simulator_helpers::items_are_suppressed(&next_state);
        let berry_atk_delta = if let Some(mon) = simulator_helpers::get_pokemon_at_slot_mut(&mut next_state, user_slot) {
            let berry = mon.item.clone();
            mon.consumed_item = Some(berry.clone());
            mon.item = crate::data::item::Item::None;
            simulator_helpers::apply_berry_effect(mon, &berry, &env);
            simulator_helpers::on_berry_eaten(mon, &berry, &env);
            simulator_helpers::apply_stat_boost_external(mon, &[0, 2, 0, 0, 0, 0, 0], items_suppressed)
        } else { [0i8; 7] };
        for (boost_idx, &stages) in berry_atk_delta.iter().enumerate() {
            if stages != 0 { simulator_helpers::emit(&mut next_state, EventKind::BoostChanged { target: user_slot, boost_idx, stages }); }
        }
        decrement_move_pp(&mut next_state, user_slot, &action.move_name, Some(move_data));
        return status_move_self_outcome(next_state, &confusion_self_hit_outcomes);
    }

    // Copycat: use the last move executed by any Pokémon on the field.
    // Fails if no move has been used yet, or if the last move is uncopyable.
    if move_name == PokemonMove::Copycat {
        use crate::data::pokemon_move::PokemonMove as M;
        // Moves that the FailCopyCat flag in move data does not yet cover (Showdown data gap —
        // Gen IX column on the Copycat page is marked "needs research"). Remove these when the
        // data is updated. MirrorCoat/Obstruct/SilkTrap are intentional omissions from the flag.
        const UNCOPYABLE_EXTRA: &[M] = &[M::MirrorCoat, M::Obstruct, M::SilkTrap];
        let last_mv = next_state.last_move_on_field.clone();
        let copied = match last_mv {
            None => return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes),
            Some(ref mv) if UNCOPYABLE_EXTRA.contains(mv) => {
                return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
            }
            Some(ref mv) if move_dex.get(mv).map_or(true, |d| {
                simulator_helpers::move_has_flag(d, &crate::state::dex_data::MoveFlag::FailCopyCat)
            }) => {
                return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
            }
            Some(mv) => mv,
        };
        let Some(copied_move_data) = move_dex.get(&copied) else {
            return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
        };
        let copied_priority = copied_move_data.priority;

        // For single-target moves, compute all valid targets. Damaging moves use foe targets
        // only (never the user's partner); self/ally moves use the normal target rules.
        // In doubles with multiple valid targets, branch uniformly across each.
        let valid_targets: Vec<Option<FieldSlot>> =
            if simulator_helpers::move_target_is_multitarget(&copied_move_data.target) {
                // Multi-target: pass None; resolve_move_targets handles it inside the inner call.
                vec![None]
            } else {
                let slots = simulator_helpers::resolve_move_targets(
                    &next_state, action.user_slot, &copied_move_data.target,
                );
                if slots.is_empty() {
                    return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
                }
                slots.into_iter().map(Some).collect()
            };

        // Decrement Copycat's own PP before branching.
        decrement_move_pp(&mut next_state, action.user_slot, &action.move_name, Some(move_data));

        let n = valid_targets.len();
        let per_prob = 1.0 / n as f64;
        let mut all_branches: Vec<(MatchState, f64)> = Vec::new();
        for target_slot in valid_targets {
            let synthetic_action = MoveAction {
                move_name: copied.clone(),
                user_slot: action.user_slot,
                target_slot,
                priority: copied_priority,
                moves_first: action.moves_first,
                moves_last: action.moves_last,
            };
            // Prevent infinite recursion: Copycat cannot copy Copycat (already in UNCOPYABLE).
            let branch_results = possible_damage_outcomes_for_move(
                &next_state,
                &synthetic_action,
                copied_move_data,
                config,
                move_dex,
                pokemon_dex,
                true,
            );
            for (state, prob) in branch_results {
                all_branches.push((state, prob * per_prob));
            }
        }
        return simulator_helpers::coalesce_branches(all_branches);
    }

    // Instruct: make the target immediately re-execute its most recent move.
    // PP is deducted from the TARGET's move slot. The instructed move's semantics
    // (including whether it hits, crits, etc.) are fully independent of Instruct.
    if move_name == PokemonMove::Instruct {
        let target_slot = target_slots.first().copied().unwrap_or(action.user_slot);
        // Gather what we need from the target before any mutable borrows
        let (instructed_move, pp_slot_idx) = {
            let Some(tgt) = simulator_helpers::get_pokemon_at_slot(&next_state, target_slot) else {
                return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
            };
            let Some(ref mv) = tgt.last_used_move.clone() else {
                return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
            };
            // Fail if the last move cannot be repeated via Instruct. The FailInstruct flag is
            // the authoritative set (covers charge moves, callers, Bide, Focus Punch, etc.).
            // Recharge moves (Hyper Beam etc.) lack FailInstruct but carry the Recharge flag,
            // so both are checked. Struggle has no move data — treat it as uncallable.
            if *mv == PokemonMove::Struggle {
                return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
            }
            let md = move_dex.get(mv);
            if md.map_or(true, |d| {
                simulator_helpers::move_has_flag(d, &crate::state::dex_data::MoveFlag::FailInstruct)
                    || simulator_helpers::move_has_flag(d, &crate::state::dex_data::MoveFlag::Recharge)
            }) {
                return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
            }
            let slot_idx = tgt.moves.iter().position(|m| m.as_ref() == Some(mv));
            let pp = slot_idx.and_then(|i| tgt.move_pp.get(i).copied()).unwrap_or(0);
            if pp == 0 {
                return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
            }
            (mv.clone(), slot_idx)
        };

        let Some(instructed_data) = move_dex.get(&instructed_move) else {
            return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
        };

        // Debit Instruct's own PP
        decrement_move_pp(&mut next_state, action.user_slot, &action.move_name, Some(move_data));

        // Debit one PP from the target's instructed move slot
        if let Some(idx) = pp_slot_idx {
            if let Some(tgt_mon) = mon_at_slot_mut(&mut next_state, target_slot) {
                if let Some(pp) = tgt_mon.move_pp.get_mut(idx) {
                    *pp = pp.saturating_sub(1);
                }
            }
        }

        // Build a synthetic action: the TARGET executes the instructed move
        let target_of_instructed = simulator_helpers::resolve_move_targets(
            &next_state, target_slot, &instructed_data.target,
        );
        let target_of_instructed_slot = target_of_instructed.first().copied();
        let synthetic_action = MoveAction {
            move_name: instructed_move,
            user_slot: target_slot,
            target_slot: target_of_instructed_slot,
            priority: instructed_data.priority,
            moves_first: false,
            moves_last: false,
        };

        let results = possible_damage_outcomes_for_move(
            &next_state,
            &synthetic_action,
            instructed_data,
            config,
            move_dex,
            pokemon_dex,
            true,
        );
        return simulator_helpers::coalesce_branches(results);
    }

    // Fling fails outright if the user has no flingable item (or its item is suppressed by
    // Magic Room / Klutz / Neutralizing Gas). Otherwise it proceeds as a normal damaging
    // move whose power and added effect come from the thrown item.
    if move_name == PokemonMove::Fling {
        let can_fling = simulator_helpers::get_pokemon_at_slot(&next_state, action.user_slot)
            .map(|m| simulator_helpers::item_is_active(&next_state, m) && m.item.fling_power().is_some())
            .unwrap_or(false);
        if !can_fling {
            return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
        }
    }

    // Encore: lock the target into repeating its last move for 3 turns. If the target still has a
    // pending move action this turn (it acts after the Encore user), rewrite that queued action to
    // the encored move so it is forced THIS turn.
    if move_name == PokemonMove::Encore {
        let target_slot = target_slots[0];
        // Magic Bounce: Encore targeting an opponent is reflected back.
        if target_slot.player != action.user_slot.player
            && !simulator_helpers::attacker_breaks_mold(&next_state, &attacker)
        {
            let mb = simulator_helpers::get_pokemon_at_slot(&next_state, target_slot)
                .map_or(false, |t| !simulator_helpers::pokemon_ability_is_suppressed(&next_state, t) && t.ability == Ability::MagicBounce);
            if mb {
                match simulator_helpers::try_apply_encore(&mut next_state, target_slot, action.user_slot, move_dex) {
                    Some(_) => {
                        decrement_move_pp(&mut next_state, action.user_slot, &action.move_name, Some(move_data));
                        return vec![(MatchState::BattleState(next_state), 1.0)];
                    }
                    None => return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes),
                }
            }
        }
        match simulator_helpers::try_apply_encore(&mut next_state, action.user_slot, target_slot, move_dex) {
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
                decrement_move_pp(&mut next_state, action.user_slot, &action.move_name, Some(move_data));
                return status_move_self_outcome(next_state, &confusion_self_hit_outcomes);
            }
            None => return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes),
        }
    }

    // After You: force the target to move immediately after the user this turn. Sets moves_first
    // on the target's queued MoveAction. Bypasses accuracy. Fails if the target has already
    // acted or is semi-invulnerable.
    if move_name == PokemonMove::AfterYou {
        let target_slot = target_slots[0];
        let target_is_invulnerable = simulator_helpers::get_pokemon_at_slot(&next_state, target_slot)
            .map(|m| m.volatiles.iter().any(|v| matches!(v,
                crate::state::pokemon::VolatileStatusState::MoveStatus(VolatileStatus::SemiInvulnerable(_), _)
                | crate::state::pokemon::VolatileStatusState::TurnStatus(VolatileStatus::SkyDrop, _))))
            .unwrap_or(false);
        let target_has_queued_move = next_state.action_queue.iter().any(|a| {
            if let Action::MoveAction(ma) = a { ma.user_slot == target_slot } else { false }
        });
        if target_is_invulnerable || !target_has_queued_move {
            simulator_helpers::note_move_outcome(&mut next_state, action.user_slot, simulator_helpers::MoveOutcome::Failed);
            return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
        }
        for queued in next_state.action_queue.iter_mut() {
            if let Action::MoveAction(ma) = queued {
                if ma.user_slot == target_slot {
                    ma.moves_first = true;
                }
            }
        }
        decrement_move_pp(&mut next_state, action.user_slot, &action.move_name, Some(move_data));
        return status_move_self_outcome(next_state, &confusion_self_hit_outcomes);
    }

    // Quash: force the target to act last this turn. Sets moves_last on the target's queued
    // MoveAction. In Gen IX, multiple Quashed targets move fastest-to-slowest among themselves
    // (naturally enforced: compare_action_order falls through to speed when both have moves_last).
    // Affected by Protect. Fails if the target has already acted.
    if move_name == PokemonMove::Quash {
        let target_slot = target_slots[0];
        let target_has_queued_move = next_state.action_queue.iter().any(|a| {
            if let Action::MoveAction(ma) = a { ma.user_slot == target_slot } else { false }
        });
        if !target_has_queued_move {
            simulator_helpers::note_move_outcome(&mut next_state, action.user_slot, simulator_helpers::MoveOutcome::Failed);
            return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
        }
        for queued in next_state.action_queue.iter_mut() {
            if let Action::MoveAction(ma) = queued {
                if ma.user_slot == target_slot {
                    ma.moves_last = true;
                }
            }
        }
        decrement_move_pp(&mut next_state, action.user_slot, &action.move_name, Some(move_data));
        return status_move_self_outcome(next_state, &confusion_self_hit_outcomes);
    }

    // Future Sight / Doom Desire: queue a delayed damaging hit on the target's slot that fires
    // at the end of the second turn after this one. Damage is computed from the full attacker
    // snapshot at queue time so it remains correct if the user switches out before impact.
    // Fails if that slot already has a pending FutureMove condition.
    // Driven by MoveFlag::FutureMove rather than a hardcoded move name list.
    if simulator_helpers::move_has_flag(move_data, &crate::state::dex_data::MoveFlag::FutureMove) {
        let target_slot = target_slots[0];
        let target_player = target_slot.player;
        let target_idx = target_slot.slot_index as usize;
        let target_conds = match target_player {
            Player::P1 => &next_state.p1_slot_conditions,
            Player::P2 => &next_state.p2_slot_conditions,
        };
        let already_pending = target_conds
            .get(target_idx)
            .map(|c| c.iter().any(|sc| matches!(sc, crate::state::dex_data::SlotCondition::FutureMove { .. })))
            .unwrap_or(false);
        if already_pending {
            return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
        }
        // Snapshot the attacker's relevant state for the damage calc at impact time.
        let user_slot = action.user_slot;
        let user_player = user_slot.player;
        let (snapshot_raw_spa, snapshot_spa_boost, snapshot_level, snapshot_type1, snapshot_type2, snapshot_ability, snapshot_item, attacker_mon_id) =
            simulator_helpers::get_pokemon_at_slot(&next_state, user_slot)
            .map(|u| {
                let t1 = u.types.first().cloned();
                let t2 = u.types.get(1).cloned();
                (u.stats[3], u.boosts[2], u.level, t1, t2, u.ability.clone(), u.item.clone(), u.mon_id)
            })
            .unwrap_or((0, 0, 1, None, None, Ability::None, Item::None, 0));
        // Build the condition once; clone it into slot_conds, emit the original.
        let future_condition = crate::state::dex_data::SlotCondition::FutureMove {
            move_name: move_name.clone(),
            attacker_is_p1: user_player == Player::P1,
            attacker_slot_index: user_slot.slot_index,
            attacker_mon_id,
            snapshot_raw_spa,
            snapshot_spa_boost,
            snapshot_level,
            snapshot_type1,
            snapshot_type2,
            snapshot_ability,
            snapshot_item,
            turns_remaining: 3, // fires after 2 end-of-turns (ticks: 3→2, 2→1, 1→0=fire)
        };
        {
            let conds_mut = match target_player {
                Player::P1 => &mut next_state.p1_slot_conditions,
                Player::P2 => &mut next_state.p2_slot_conditions,
            };
            if let Some(slot_conds) = conds_mut.get_mut(target_idx) {
                slot_conds.push(future_condition.clone());
            }
        } // conds_mut borrow ends here — next_state is free for emit
        simulator_helpers::emit(&mut next_state, EventKind::SlotConditionStart {
            slot: target_slot,
            condition: future_condition,
        });
        decrement_move_pp(&mut next_state, action.user_slot, &action.move_name, Some(move_data));
        return status_move_self_outcome(next_state, &confusion_self_hit_outcomes);
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
        // Emit TeamStatusCured: the entire side was cured at once.
        simulator_helpers::emit(&mut next_state, EventKind::TeamStatusCured { side: player });
        decrement_move_pp(&mut next_state, action.user_slot, &action.move_name, Some(move_data));
        return status_move_self_outcome(next_state, &confusion_self_hit_outcomes);
    }

    // Heal Pulse: restore the target's HP by 1/2 (3/4 with Mega Launcher). Fails if the target
    // is already at full HP or behind a Substitute, or if the user or target is under Heal Block.
    // Pollen Puff (ally-heal branch): when targeting an ally, heal 50% of their max HP.
    // Heal Block on the ATTACKER fails the move entirely; on the TARGET, the heal is skipped
    // but the move still succeeds (NOT_FAIL). Substitute blocks the heal.
    // When targeting a foe, falls through to the normal 90 BP Bug Special damage pipeline.
    if move_name == PokemonMove::PollenPuff && !target_slots.is_empty() {
        let target_slot = target_slots[0];
        if target_slot.player == action.user_slot.player
            && target_slot.slot_index != action.user_slot.slot_index
        {
            let user_hb = simulator_helpers::get_pokemon_at_slot(&next_state, action.user_slot)
                .map(simulator_helpers::heal_is_blocked).unwrap_or(false);
            if user_hb {
                return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
            }
            let target_env = simulator_helpers::berry_env(&next_state, target_slot);
            if let Some(target) = simulator_helpers::get_pokemon_at_slot(&next_state, target_slot) {
                let max_hp = target.stats[0].max(1);
                let full = target.hp >= max_hp;
                let sub = simulator_helpers::has_status_volatile(target, &VolatileStatus::Substitute(0));
                let target_hb = simulator_helpers::heal_is_blocked(target);
                if !full && !sub && !target_hb {
                    let amount = (max_hp as u32 / 2) as u16;
                    let heal_result = if let Some(t) = mon_at_slot_mut(&mut next_state, target_slot) {
                        let before = t.hp;
                        simulator_helpers::gain_hp(t, amount, target_env);
                        if t.hp != before { Some((t.hp, t.stats[0])) } else { None }
                    } else { None };
                    if let Some((post_hp, mx)) = heal_result {
                        if let Some(obs) = next_state.event_observer {
                            let new_hp = if target_slot.player == obs {
                                PokemonHP::Number(post_hp)
                            } else {
                                PokemonHP::Percent(simulator_helpers::hp_to_percent(post_hp, mx))
                            };
                            simulator_helpers::emit(&mut next_state, EventKind::Healed { target: target_slot, new_hp });
                        }
                    }
                }
            }
            decrement_move_pp(&mut next_state, action.user_slot, &action.move_name, Some(move_data));
            return status_move_self_outcome(next_state, &confusion_self_hit_outcomes);
        }
        // Foe-targeting falls through to the normal damage pipeline.
    }

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
            let sub = simulator_helpers::has_status_volatile(target, &VolatileStatus::Substitute(0));
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
                let heal_result = if let Some(t) = mon_at_slot_mut(&mut next_state, target_slot) {
                    let before = t.hp;
                    simulator_helpers::gain_hp(t, amount, target_env);
                    if t.hp != before { Some((t.hp, t.stats[0])) } else { None }
                } else { None };
                if let Some((post_hp, mx)) = heal_result {
                    if let Some(obs) = next_state.event_observer {
                        let new_hp = if target_slot.player == obs {
                            PokemonHP::Number(post_hp)
                        } else {
                            PokemonHP::Percent(simulator_helpers::hp_to_percent(post_hp, mx))
                        };
                        simulator_helpers::emit(&mut next_state, EventKind::Healed { target: target_slot, new_hp });
                    }
                }
                decrement_move_pp(&mut next_state, action.user_slot, &action.move_name, Some(move_data));
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
            .map(|m| simulator_helpers::has_status_volatile(m, &VolatileStatus::Substitute(0)))
            .unwrap_or(false);
        if sub {
            return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
        }
        let user_hp = simulator_helpers::get_pokemon_at_slot(&next_state, action.user_slot)
            .map(|m| m.hp).unwrap_or(0);
        let target_hp = simulator_helpers::get_pokemon_at_slot(&next_state, target_slot)
            .map(|m| m.hp).unwrap_or(0);
        let avg = (((user_hp as u32 + target_hp as u32) / 2).max(1)) as u16;
        let user_new_hp = {
            let u = mon_at_slot_mut(&mut next_state, action.user_slot);
            if let Some(u) = u {
                u.hp = avg.min(u.stats[0]);
                (u.hp, u.stats[0])
            } else { (0, 1) }
        };
        let target_new_hp = {
            let t = mon_at_slot_mut(&mut next_state, target_slot);
            if let Some(t) = t {
                t.hp = avg.min(t.stats[0]);
                (t.hp, t.stats[0])
            } else { (0, 1) }
        };
        // Emit SetHp for both slots (borrows ended above; NLL safe).
        if let Some(observer) = next_state.event_observer {
            let (u_hp, u_max) = user_new_hp;
            let user_new_hp_ev = if action.user_slot.player == observer {
                PokemonHP::Number(u_hp)
            } else {
                PokemonHP::Percent(simulator_helpers::hp_to_percent(u_hp, u_max))
            };
            simulator_helpers::emit(&mut next_state, EventKind::SetHp { target: action.user_slot, new_hp: user_new_hp_ev });
            let (t_hp, t_max) = target_new_hp;
            let target_new_hp_ev = if target_slot.player == observer {
                PokemonHP::Number(t_hp)
            } else {
                PokemonHP::Percent(simulator_helpers::hp_to_percent(t_hp, t_max))
            };
            simulator_helpers::emit(&mut next_state, EventKind::SetHp { target: target_slot, new_hp: target_new_hp_ev });
        }
        decrement_move_pp(&mut next_state, action.user_slot, &action.move_name, Some(move_data));
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
            &next_state, target_ref, crate::state::dex_data::PokemonStat::Atk, false, false,
        ).round().max(1.0) as u16;
        let liquid_ooze = !abilities_suppressed && target_ref.ability == Ability::LiquidOoze;
        let user_env = simulator_helpers::berry_env(&next_state, action.user_slot);
        let strength_sap_heal = if let Some(user) = mon_at_slot_mut(&mut next_state, action.user_slot) {
            let amount = simulator_helpers::apply_big_root(user, target_atk, items_suppressed);
            if liquid_ooze {
                simulator_helpers::take_damage(user, amount, user_env, abilities_suppressed);
                None
            } else if !simulator_helpers::heal_is_blocked(user) {
                let before = user.hp;
                simulator_helpers::gain_hp(user, amount, user_env);
                if user.hp != before { Some((user.hp, user.stats[0])) } else { None }
            } else { None }
        } else { None };
        if let Some((post_hp, mx)) = strength_sap_heal {
            if let Some(obs) = next_state.event_observer {
                let new_hp = if action.user_slot.player == obs {
                    PokemonHP::Number(post_hp)
                } else {
                    PokemonHP::Percent(simulator_helpers::hp_to_percent(post_hp, mx))
                };
                simulator_helpers::emit(&mut next_state, EventKind::Healed { target: action.user_slot, new_hp });
            }
        }
        simulator_helpers::apply_opponent_stat_drop(
            &mut next_state, target_slot, action.user_slot, [-1, 0, 0, 0, 0, 0, 0], items_suppressed, false,
        );
        decrement_move_pp(&mut next_state, action.user_slot, &action.move_name, Some(move_data));
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
            .map(|c| c.iter().any(|sc| matches!(sc, crate::state::dex_data::SlotCondition::Wish { .. })))
            .unwrap_or(false);
        if heal_blocked || already_pending {
            return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
        }
        if let Some(slot_conds) = conds.get_mut(slot_idx) {
            slot_conds.push(crate::state::dex_data::SlotCondition::Wish {
                heal: (user_max_hp / 2).max(1),
                turns_remaining: 2,
            });
        }
        // conds no longer used — NLL releases the borrow; next_state is free for emit.
        simulator_helpers::emit(&mut next_state, EventKind::SlotConditionStart {
            slot: action.user_slot,
            condition: crate::state::dex_data::SlotCondition::Wish {
                heal: (user_max_hp / 2).max(1),
                turns_remaining: 2,
            },
        });
        decrement_move_pp(&mut next_state, action.user_slot, &action.move_name, Some(move_data));
        let has_confusion = confusion_self_hit_outcomes.is_some();
        let mut result = Vec::new();
        if let Some(ref c) = confusion_self_hit_outcomes {
            for (s, p) in c { result.push((s.clone(), p * (1.0 / 3.0))); }
        }
        result.push((MatchState::BattleState(next_state), if has_confusion { 2.0 / 3.0 } else { 1.0 }));
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
        let mut perish_song_slots: Vec<FieldSlot> = Vec::new();
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
            // Collect for post-loop emission (borrow of mons ends here; NLL safe).
            perish_song_slots.push(FieldSlot { player, slot_index: idx as u8 });
        }
        // Emit VolatileStart for every slot that newly received Perish Song.
        for slot in perish_song_slots {
            simulator_helpers::emit(&mut next_state, EventKind::VolatileStart {
                target: slot,
                volatile: VolatileStatus::PerishSong,
            });
        }
        decrement_move_pp(&mut next_state, action.user_slot, &action.move_name, Some(move_data));
        return status_move_self_outcome(next_state, &confusion_self_hit_outcomes);
    }

    // Destiny Bond: the user is flagged so that if they faint from a direct move this turn,
    // the attacker also faints. Fails if used consecutively (volatile already present).
    if move_name == PokemonMove::DestinyBond {
        let already_has = simulator_helpers::get_pokemon_at_slot(&next_state, action.user_slot)
            .map(|m| simulator_helpers::has_status_volatile(m, &VolatileStatus::DestinyBond))
            .unwrap_or(false);
        if already_has {
            // Clear the stale volatile — consecutive use fails and leaves no DB flag active.
            if let Some(user) = match action.user_slot.player {
                Player::P1 => next_state.p1_active_mons.get_mut(action.user_slot.slot_index as usize),
                Player::P2 => next_state.p2_active_mons.get_mut(action.user_slot.slot_index as usize),
            } {
                simulator_helpers::remove_status_volatile(user, &VolatileStatus::DestinyBond);
            }
            simulator_helpers::emit(&mut next_state, EventKind::VolatileEnd {
                target: action.user_slot,
                volatile: VolatileStatus::DestinyBond,
            });
            return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
        }
        let state_snapshot = next_state.clone();
        if let Some(user) = match action.user_slot.player {
            Player::P1 => next_state.p1_active_mons.get_mut(action.user_slot.slot_index as usize),
            Player::P2 => next_state.p2_active_mons.get_mut(action.user_slot.slot_index as usize),
        } {
            simulator_helpers::apply_volatile_to_pokemon_pub(&state_snapshot, user, &VolatileStatus::DestinyBond);
        }
        // Borrow ended; emit VolatileStart.
        simulator_helpers::emit(&mut next_state, EventKind::VolatileStart {
            target: action.user_slot,
            volatile: VolatileStatus::DestinyBond,
        });
        decrement_move_pp(&mut next_state, action.user_slot, &action.move_name, Some(move_data));
        return status_move_self_outcome(next_state, &confusion_self_hit_outcomes);
    }

    // Haze: reset all stat stages for every active Pokémon on the field.
    // Bypasses Substitute; not blocked by Protect (field-wide effect).
    if move_name == PokemonMove::Haze {
        // Collect slots that actually have non-zero boosts (to emit BoostsCleared only for them).
        let haze_slots: Vec<FieldSlot> = {
            let p1_slots = next_state.p1_active_mons.iter().enumerate()
                .filter(|(_, m)| !m.fainted && m.boosts.iter().any(|&b| b != 0))
                .map(|(i, _)| FieldSlot { player: Player::P1, slot_index: i as u8 });
            let p2_slots = next_state.p2_active_mons.iter().enumerate()
                .filter(|(_, m)| !m.fainted && m.boosts.iter().any(|&b| b != 0))
                .map(|(i, _)| FieldSlot { player: Player::P2, slot_index: i as u8 });
            p1_slots.chain(p2_slots).collect()
        };
        for mon in next_state.p1_active_mons.iter_mut()
            .chain(next_state.p2_active_mons.iter_mut())
        {
            mon.boosts = [0; 7];
        }
        for slot in haze_slots {
            simulator_helpers::emit(&mut next_state, EventKind::BoostsCleared { target: slot });
        }
        decrement_move_pp(&mut next_state, action.user_slot, &action.move_name, Some(move_data));
        return status_move_self_outcome(next_state, &confusion_self_hit_outcomes);
    }

    // Belly Drum: maximise Attack at the cost of ½ max HP. Fails if the user has ½ max HP
    // or less, or if Attack is already at +6. (Contrary inversion is not modelled until the
    // ability itself is implemented.)
    if move_name == PokemonMove::BellyDrum {
        let (hp, max_hp, atk_boost) = simulator_helpers::get_pokemon_at_slot(&next_state, action.user_slot)
            .map(|m| (m.hp, m.stats[0].max(1), m.boosts[0]))
            .unwrap_or((0, 1, 0));
        if max_hp <= 1 || 2 * hp <= max_hp || atk_boost >= 6 {
            return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
        }
        let cost = max_hp / 2;
        let env = simulator_helpers::berry_env(&next_state, action.user_slot);
        let as_ = simulator_helpers::abilities_are_suppressed(&next_state);
        if let Some(mon) = simulator_helpers::get_pokemon_at_slot_mut(&mut next_state, action.user_slot) {
            simulator_helpers::take_damage(mon, cost, env, as_);
            if mon.boosts[0] < 6 { mon.stats_raised_this_turn = true; }
            mon.boosts[0] = 6;
        }
        decrement_move_pp(&mut next_state, action.user_slot, &action.move_name, Some(move_data));
        return status_move_self_outcome(next_state, &confusion_self_hit_outcomes);
    }

    // Clangorous Soul: +1 to Atk/Def/SpA/SpD/Spe at the cost of ⅓ max HP. Fails if the user
    // has ⅓ max HP or less, or if all five of those stats are already at +6.
    if move_name == PokemonMove::ClangorousSoul {
        let (hp, max_hp, all_maxed) = simulator_helpers::get_pokemon_at_slot(&next_state, action.user_slot)
            .map(|m| (m.hp, m.stats[0].max(1), m.boosts[0..5].iter().all(|&b| b >= 6)))
            .unwrap_or((0, 1, true));
        if max_hp <= 1 || 3 * hp <= max_hp || all_maxed {
            return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
        }
        let cost = max_hp / 3;
        let items_suppressed = simulator_helpers::items_are_suppressed(&next_state);
        let env = simulator_helpers::berry_env(&next_state, action.user_slot);
        let as_ = simulator_helpers::abilities_are_suppressed(&next_state);
        let belly_drum_delta = if let Some(mon) = simulator_helpers::get_pokemon_at_slot_mut(&mut next_state, action.user_slot) {
            simulator_helpers::take_damage(mon, cost, env, as_);
            simulator_helpers::apply_stat_boost_external(mon, &[1, 1, 1, 1, 1, 0, 0], items_suppressed)
        } else { [0i8; 7] };
        for (boost_idx, &stages) in belly_drum_delta.iter().enumerate() {
            if stages != 0 { simulator_helpers::emit(&mut next_state, EventKind::BoostChanged { target: action.user_slot, boost_idx, stages }); }
        }
        decrement_move_pp(&mut next_state, action.user_slot, &action.move_name, Some(move_data));
        return status_move_self_outcome(next_state, &confusion_self_hit_outcomes);
    }

    // Charge: +1 Sp. Def and apply the Charge volatile (doubles the user's next Electric move).
    // Non-cumulative — re-using it just refreshes the volatile while still raising Sp. Def.
    if move_name == PokemonMove::Charge {
        let items_suppressed = simulator_helpers::items_are_suppressed(&next_state);
        let charge_delta = if let Some(mon) = simulator_helpers::get_pokemon_at_slot_mut(&mut next_state, action.user_slot) {
            let d = simulator_helpers::apply_stat_boost_external(mon, &[0, 0, 0, 1, 0, 0, 0], items_suppressed);
            simulator_helpers::remove_status_volatile(mon, &VolatileStatus::Charge);
            mon.volatiles.push(crate::state::pokemon::VolatileStatusState::TurnStatus(VolatileStatus::Charge, 0));
            d
        } else { [0i8; 7] };
        for (boost_idx, &stages) in charge_delta.iter().enumerate() {
            if stages != 0 { simulator_helpers::emit(&mut next_state, EventKind::BoostChanged { target: action.user_slot, boost_idx, stages }); }
        }
        decrement_move_pp(&mut next_state, action.user_slot, &action.move_name, Some(move_data));
        return status_move_self_outcome(next_state, &confusion_self_hit_outcomes);
    }

    // Focus Energy: +2 critical-hit stages (via the FocusEnergy volatile). Fails if the user is
    // already under Focus Energy or Dragon Cheer.
    if move_name == PokemonMove::FocusEnergy {
        let blocked = simulator_helpers::get_pokemon_at_slot(&next_state, action.user_slot)
            .map(|m| simulator_helpers::has_status_volatile(m, &VolatileStatus::FocusEnergy)
                || simulator_helpers::has_status_volatile(m, &VolatileStatus::DragonCheer(0)))
            .unwrap_or(true);
        if blocked {
            return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
        }
        if let Some(mon) = simulator_helpers::get_pokemon_at_slot_mut(&mut next_state, action.user_slot) {
            mon.volatiles.push(crate::state::pokemon::VolatileStatusState::TurnStatus(VolatileStatus::FocusEnergy, 0));
        }
        decrement_move_pp(&mut next_state, action.user_slot, &action.move_name, Some(move_data));
        return status_move_self_outcome(next_state, &confusion_self_hit_outcomes);
    }

    // Dragon Cheer: give an adjacent ally +1 critical-hit stage (+2 if that ally is Dragon-type
    // at the time of use; the amount is locked in). Fails with no adjacent ally, or if the ally
    // already has Focus Energy or Dragon Cheer.
    if move_name == PokemonMove::DragonCheer {
        let user_player = action.user_slot.player;
        let user_idx = action.user_slot.slot_index as usize;
        let actives_len = match user_player {
            Player::P1 => next_state.p1_active_mons.len(),
            Player::P2 => next_state.p2_active_mons.len(),
        };
        let ally_idx = (0..actives_len).find(|&i| {
            i != user_idx
                && simulator_helpers::get_pokemon_at_slot(&next_state, FieldSlot { player: user_player, slot_index: i as u8 })
                    .map(|m| !m.fainted)
                    .unwrap_or(false)
        });
        let Some(ally_idx) = ally_idx else {
            return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
        };
        let ally_slot = FieldSlot { player: user_player, slot_index: ally_idx as u8 };
        let ally = simulator_helpers::get_pokemon_at_slot(&next_state, ally_slot);
        let blocked = ally
            .map(|m| simulator_helpers::has_status_volatile(m, &VolatileStatus::FocusEnergy)
                || simulator_helpers::has_status_volatile(m, &VolatileStatus::DragonCheer(0)))
            .unwrap_or(true);
        if blocked {
            return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
        }
        let amount = ally
            .map(|m| if simulator_helpers::pokemon_has_type(m, &PokemonType::Dragon) { 2 } else { 1 })
            .unwrap_or(1);
        if let Some(mon) = simulator_helpers::get_pokemon_at_slot_mut(&mut next_state, ally_slot) {
            mon.volatiles.push(crate::state::pokemon::VolatileStatusState::TurnStatus(VolatileStatus::DragonCheer(amount), 0));
        }
        decrement_move_pp(&mut next_state, action.user_slot, &action.move_name, Some(move_data));
        return status_move_self_outcome(next_state, &confusion_self_hit_outcomes);
    }

    // Magnetic Flux: +1 Def/Sp. Def to every Pokémon on the user's side whose ability is Plus or
    // Minus (the user included). Fails if no such Pokémon is present.
    if move_name == PokemonMove::MagneticFlux {
        let user_player = action.user_slot.player;
        let items_suppressed = simulator_helpers::items_are_suppressed(&next_state);
        let actives_len = match user_player {
            Player::P1 => next_state.p1_active_mons.len(),
            Player::P2 => next_state.p2_active_mons.len(),
        };
        let mut any = false;
        for i in 0..actives_len {
            let slot = FieldSlot { player: user_player, slot_index: i as u8 };
            let eligible = simulator_helpers::get_pokemon_at_slot(&next_state, slot)
                .map(|m| !m.fainted
                    && matches!(m.ability, Ability::Plus | Ability::Minus)
                    && !simulator_helpers::pokemon_ability_is_suppressed(&next_state, m))
                .unwrap_or(false);
            if eligible {
                let dl = if let Some(mon) = simulator_helpers::get_pokemon_at_slot_mut(&mut next_state, slot) {
                    simulator_helpers::apply_stat_boost_external(mon, &[0, 1, 0, 1, 0, 0, 0], items_suppressed)
                } else { [0i8; 7] };
                for (boost_idx, &stages) in dl.iter().enumerate() {
                    if stages != 0 { simulator_helpers::emit(&mut next_state, EventKind::BoostChanged { target: slot, boost_idx, stages }); }
                }
                any = true;
            }
        }
        if !any {
            return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
        }
        decrement_move_pp(&mut next_state, action.user_slot, &action.move_name, Some(move_data));
        return status_move_self_outcome(next_state, &confusion_self_hit_outcomes);
    }

    // Defense Curl: +1 Def and apply the DefenseCurl volatile (Rollout/Ice Ball double their
    // power for the entire run while the user holds this volatile).
    if move_name == PokemonMove::DefenseCurl {
        let items_suppressed = simulator_helpers::items_are_suppressed(&next_state);
        let def_curl_delta = if let Some(mon) = simulator_helpers::get_pokemon_at_slot_mut(&mut next_state, action.user_slot) {
            let d = simulator_helpers::apply_stat_boost_external(mon, &[0, 1, 0, 0, 0, 0, 0], items_suppressed);
            if !simulator_helpers::has_status_volatile(mon, &VolatileStatus::DefenseCurl) {
                mon.volatiles.push(crate::state::pokemon::VolatileStatusState::TurnStatus(VolatileStatus::DefenseCurl, 0));
            }
            d
        } else { [0i8; 7] };
        for (boost_idx, &stages) in def_curl_delta.iter().enumerate() {
            if stages != 0 { simulator_helpers::emit(&mut next_state, EventKind::BoostChanged { target: action.user_slot, boost_idx, stages }); }
        }
        decrement_move_pp(&mut next_state, action.user_slot, &action.move_name, Some(move_data));
        return status_move_self_outcome(next_state, &confusion_self_hit_outcomes);
    }

    // Minimize: +2 evasiveness and apply the Minimize volatile (certain moves hit it for double
    // and never miss — handled in the damage calculation).
    if move_name == PokemonMove::Minimize {
        let items_suppressed = simulator_helpers::items_are_suppressed(&next_state);
        let minimize_delta = if let Some(mon) = simulator_helpers::get_pokemon_at_slot_mut(&mut next_state, action.user_slot) {
            let d = simulator_helpers::apply_stat_boost_external(mon, &[0, 0, 0, 0, 0, 0, 2], items_suppressed);
            if !simulator_helpers::has_status_volatile(mon, &VolatileStatus::Minimize) {
                mon.volatiles.push(crate::state::pokemon::VolatileStatusState::TurnStatus(VolatileStatus::Minimize, 0));
            }
            d
        } else { [0i8; 7] };
        for (boost_idx, &stages) in minimize_delta.iter().enumerate() {
            if stages != 0 { simulator_helpers::emit(&mut next_state, EventKind::BoostChanged { target: action.user_slot, boost_idx, stages }); }
        }
        decrement_move_pp(&mut next_state, action.user_slot, &action.move_name, Some(move_data));
        return status_move_self_outcome(next_state, &confusion_self_hit_outcomes);
    }

    // Stockpile: +1 Def/Sp. Def and raise the Stockpile level (max 3). Fails at level 3.
    if move_name == PokemonMove::Stockpile {
        let level = simulator_helpers::get_pokemon_at_slot(&next_state, action.user_slot)
            .map(simulator_helpers::stockpile_level)
            .unwrap_or(3);
        if level >= 3 {
            return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
        }
        let items_suppressed = simulator_helpers::items_are_suppressed(&next_state);
        let stockpile_delta = if let Some(mon) = simulator_helpers::get_pokemon_at_slot_mut(&mut next_state, action.user_slot) {
            let d = simulator_helpers::apply_stat_boost_external(mon, &[0, 1, 0, 1, 0, 0, 0], items_suppressed);
            simulator_helpers::remove_status_volatile(mon, &VolatileStatus::Stockpile(0));
            mon.volatiles.push(crate::state::pokemon::VolatileStatusState::TurnStatus(VolatileStatus::Stockpile(level + 1), 0));
            d
        } else { [0i8; 7] };
        for (boost_idx, &stages) in stockpile_delta.iter().enumerate() {
            if stages != 0 { simulator_helpers::emit(&mut next_state, EventKind::BoostChanged { target: action.user_slot, boost_idx, stages }); }
        }
        decrement_move_pp(&mut next_state, action.user_slot, &action.move_name, Some(move_data));
        return status_move_self_outcome(next_state, &confusion_self_hit_outcomes);
    }

    // Swallow: heal ¼ / ½ / full max HP for Stockpile level 1 / 2 / 3, then consume the Stockpile
    // charge and the Def/Sp. Def boosts it granted. Fails if the user has not used Stockpile.
    if move_name == PokemonMove::Swallow {
        let (level, max_hp, heal_blocked) = simulator_helpers::get_pokemon_at_slot(&next_state, action.user_slot)
            .map(|m| (simulator_helpers::stockpile_level(m), m.stats[0].max(1), simulator_helpers::heal_is_blocked(m)))
            .unwrap_or((0, 1, true));
        if level == 0 {
            return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
        }
        let heal = (match level { 1 => max_hp / 4, 2 => max_hp / 2, _ => max_hp }).max(1);
        let items_suppressed = simulator_helpers::items_are_suppressed(&next_state);
        let env = simulator_helpers::berry_env(&next_state, action.user_slot);
        let swallow_delta = if let Some(mon) = simulator_helpers::get_pokemon_at_slot_mut(&mut next_state, action.user_slot) {
            if !heal_blocked {
                simulator_helpers::gain_hp(mon, heal, env);
            }
            simulator_helpers::remove_status_volatile(mon, &VolatileStatus::Stockpile(0));
            let drop = -(level as i8);
            simulator_helpers::apply_stat_boost_external(mon, &[0, drop, 0, drop, 0, 0, 0], items_suppressed)
        } else { [0i8; 7] };
        for (boost_idx, &stages) in swallow_delta.iter().enumerate() {
            if stages != 0 { simulator_helpers::emit(&mut next_state, EventKind::BoostChanged { target: action.user_slot, boost_idx, stages }); }
        }
        decrement_move_pp(&mut next_state, action.user_slot, &action.move_name, Some(move_data));
        return status_move_self_outcome(next_state, &confusion_self_hit_outcomes);
    }

    // Spit Up: fails outright if the user has no Stockpile charge. On success it is an ordinary
    // damaging move — its base power (100/200/300) comes from variable_move_base_power, and the
    // Stockpile charge + its Def/Sp. Def boosts are consumed in apply_post_damage_move_effects.
    if move_name == PokemonMove::SpitUp {
        let level = simulator_helpers::get_pokemon_at_slot(&next_state, action.user_slot)
            .map(simulator_helpers::stockpile_level)
            .unwrap_or(0);
        if level == 0 {
            return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
        }
    }

    // Psych Up: copy target's stat stages and Focus Energy status to the user.
    // Bypasses Substitute; not blocked by Protect.
    if move_name == PokemonMove::PsychUp {
        let Some(&target_slot) = target_slots.first() else {
            return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
        };
        let state_snapshot = next_state.clone();
        let target_boosts = simulator_helpers::get_pokemon_at_slot(&state_snapshot, target_slot)
            .map(|m| m.boosts)
            .unwrap_or([0; 7]);
        let target_has_focus = simulator_helpers::get_pokemon_at_slot(&state_snapshot, target_slot)
            .map(|m| simulator_helpers::has_status_volatile(m, &VolatileStatus::FocusEnergy))
            .unwrap_or(false);
        // Track if FocusEnergy is newly gained (must check before the borrow block).
        let user_had_focus = simulator_helpers::get_pokemon_at_slot(&next_state, action.user_slot)
            .map(|m| simulator_helpers::has_status_volatile(m, &VolatileStatus::FocusEnergy))
            .unwrap_or(false);
        if let Some(user) = simulator_helpers::get_pokemon_at_slot_mut(&mut next_state, action.user_slot) {
            user.boosts = target_boosts;
            if target_has_focus {
                if !simulator_helpers::has_status_volatile(user, &VolatileStatus::FocusEnergy) {
                    simulator_helpers::apply_volatile_to_pokemon_pub(&state_snapshot, user, &VolatileStatus::FocusEnergy);
                }
            } else {
                simulator_helpers::remove_status_volatile(user, &VolatileStatus::FocusEnergy);
            }
        }
        // Emit VolatileStart if FocusEnergy was newly added via Psych Up.
        if target_has_focus && !user_had_focus {
            simulator_helpers::emit(&mut next_state, EventKind::VolatileStart {
                target: action.user_slot,
                volatile: VolatileStatus::FocusEnergy,
            });
        }
        // Emit VolatileEnd if Psych Up cleared FocusEnergy (target did not have it).
        if !target_has_focus && user_had_focus {
            simulator_helpers::emit(&mut next_state, EventKind::VolatileEnd {
                target: action.user_slot,
                volatile: VolatileStatus::FocusEnergy,
            });
        }
        simulator_helpers::emit(&mut next_state, EventKind::BoostsCopied {
            source: target_slot,
            target: action.user_slot,
        });
        decrement_move_pp(&mut next_state, action.user_slot, &action.move_name, Some(move_data));
        return status_move_self_outcome(next_state, &confusion_self_hit_outcomes);
    }

    // Guard Split: average the user's and target's raw Defense and Sp. Def stats (floor).
    // Blocked by Substitute; blocked by Protect. stats[]: hp=0,atk=1,def=2,spa=3,spd=4,spe=5
    if move_name == PokemonMove::GuardSplit {
        let Some(&target_slot) = target_slots.first() else {
            return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
        };
        let target_has_sub = simulator_helpers::get_pokemon_at_slot(&next_state, target_slot)
            .map(|m| simulator_helpers::has_status_volatile(m, &VolatileStatus::Substitute(0)))
            .unwrap_or(false);
        if !target_has_sub {
            let user_def = simulator_helpers::get_pokemon_at_slot(&next_state, action.user_slot).map(|m| m.stats[2]).unwrap_or(0);
            let user_spd = simulator_helpers::get_pokemon_at_slot(&next_state, action.user_slot).map(|m| m.stats[4]).unwrap_or(0);
            let target_def = simulator_helpers::get_pokemon_at_slot(&next_state, target_slot).map(|m| m.stats[2]).unwrap_or(0);
            let target_spd = simulator_helpers::get_pokemon_at_slot(&next_state, target_slot).map(|m| m.stats[4]).unwrap_or(0);
            let new_def = ((user_def as u32 + target_def as u32) / 2) as u16;
            let new_spd = ((user_spd as u32 + target_spd as u32) / 2) as u16;
            if let Some(user) = simulator_helpers::get_pokemon_at_slot_mut(&mut next_state, action.user_slot) {
                user.stats[2] = new_def;
                user.stats[4] = new_spd;
            }
            if let Some(target) = simulator_helpers::get_pokemon_at_slot_mut(&mut next_state, target_slot) {
                target.stats[2] = new_def;
                target.stats[4] = new_spd;
            }
        }
        decrement_move_pp(&mut next_state, action.user_slot, &action.move_name, Some(move_data));
        return status_move_self_outcome(next_state, &confusion_self_hit_outcomes);
    }

    // Guard Swap: swap the user's and target's Defense and Sp. Def stat stages.
    // Bypasses Substitute. boosts[]: atk=0,def=1,spa=2,spd=3,spe=4,acc=5,eva=6
    if move_name == PokemonMove::GuardSwap {
        let Some(&target_slot) = target_slots.first() else {
            return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
        };
        let (user_def_b, user_spd_b, target_def_b, target_spd_b) = {
            let user = simulator_helpers::get_pokemon_at_slot(&next_state, action.user_slot);
            let target = simulator_helpers::get_pokemon_at_slot(&next_state, target_slot);
            (user.map(|m| m.boosts[1]).unwrap_or(0),
             user.map(|m| m.boosts[3]).unwrap_or(0),
             target.map(|m| m.boosts[1]).unwrap_or(0),
             target.map(|m| m.boosts[3]).unwrap_or(0))
        };
        if let Some(user) = simulator_helpers::get_pokemon_at_slot_mut(&mut next_state, action.user_slot) {
            user.boosts[1] = target_def_b;
            user.boosts[3] = target_spd_b;
        }
        if let Some(target) = simulator_helpers::get_pokemon_at_slot_mut(&mut next_state, target_slot) {
            target.boosts[1] = user_def_b;
            target.boosts[3] = user_spd_b;
        }
        simulator_helpers::emit(&mut next_state, EventKind::BoostsSwapped {
            source: action.user_slot,
            target: target_slot,
        });
        decrement_move_pp(&mut next_state, action.user_slot, &action.move_name, Some(move_data));
        return status_move_self_outcome(next_state, &confusion_self_hit_outcomes);
    }

    // Power Split: average the user's and target's raw Attack and Sp. Atk stats (floor).
    // Blocked by Substitute; blocked by Protect. stats[]: atk=1, spa=3
    if move_name == PokemonMove::PowerSplit {
        let Some(&target_slot) = target_slots.first() else {
            return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
        };
        let target_has_sub = simulator_helpers::get_pokemon_at_slot(&next_state, target_slot)
            .map(|m| simulator_helpers::has_status_volatile(m, &VolatileStatus::Substitute(0)))
            .unwrap_or(false);
        if !target_has_sub {
            let user_atk = simulator_helpers::get_pokemon_at_slot(&next_state, action.user_slot).map(|m| m.stats[1]).unwrap_or(0);
            let user_spa = simulator_helpers::get_pokemon_at_slot(&next_state, action.user_slot).map(|m| m.stats[3]).unwrap_or(0);
            let target_atk = simulator_helpers::get_pokemon_at_slot(&next_state, target_slot).map(|m| m.stats[1]).unwrap_or(0);
            let target_spa = simulator_helpers::get_pokemon_at_slot(&next_state, target_slot).map(|m| m.stats[3]).unwrap_or(0);
            let new_atk = ((user_atk as u32 + target_atk as u32) / 2) as u16;
            let new_spa = ((user_spa as u32 + target_spa as u32) / 2) as u16;
            if let Some(user) = simulator_helpers::get_pokemon_at_slot_mut(&mut next_state, action.user_slot) {
                user.stats[1] = new_atk;
                user.stats[3] = new_spa;
            }
            if let Some(target) = simulator_helpers::get_pokemon_at_slot_mut(&mut next_state, target_slot) {
                target.stats[1] = new_atk;
                target.stats[3] = new_spa;
            }
        }
        decrement_move_pp(&mut next_state, action.user_slot, &action.move_name, Some(move_data));
        return status_move_self_outcome(next_state, &confusion_self_hit_outcomes);
    }

    // Power Swap: swap the user's and target's Attack and Sp. Atk stat stages.
    // Bypasses Substitute. boosts[]: atk=0, spa=2
    if move_name == PokemonMove::PowerSwap {
        let Some(&target_slot) = target_slots.first() else {
            return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
        };
        let (user_atk_b, user_spa_b, target_atk_b, target_spa_b) = {
            let user = simulator_helpers::get_pokemon_at_slot(&next_state, action.user_slot);
            let target = simulator_helpers::get_pokemon_at_slot(&next_state, target_slot);
            (user.map(|m| m.boosts[0]).unwrap_or(0),
             user.map(|m| m.boosts[2]).unwrap_or(0),
             target.map(|m| m.boosts[0]).unwrap_or(0),
             target.map(|m| m.boosts[2]).unwrap_or(0))
        };
        if let Some(user) = simulator_helpers::get_pokemon_at_slot_mut(&mut next_state, action.user_slot) {
            user.boosts[0] = target_atk_b;
            user.boosts[2] = target_spa_b;
        }
        if let Some(target) = simulator_helpers::get_pokemon_at_slot_mut(&mut next_state, target_slot) {
            target.boosts[0] = user_atk_b;
            target.boosts[2] = user_spa_b;
        }
        simulator_helpers::emit(&mut next_state, EventKind::BoostsSwapped {
            source: action.user_slot,
            target: target_slot,
        });
        decrement_move_pp(&mut next_state, action.user_slot, &action.move_name, Some(move_data));
        return status_move_self_outcome(next_state, &confusion_self_hit_outcomes);
    }

    // Speed Swap: exchange the user's and target's raw Speed stats.
    // Bypasses Substitute. stats[]: spe=5
    if move_name == PokemonMove::SpeedSwap {
        let Some(&target_slot) = target_slots.first() else {
            return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
        };
        let user_spe = simulator_helpers::get_pokemon_at_slot(&next_state, action.user_slot).map(|m| m.stats[5]).unwrap_or(0);
        let target_spe = simulator_helpers::get_pokemon_at_slot(&next_state, target_slot).map(|m| m.stats[5]).unwrap_or(0);
        if let Some(user) = simulator_helpers::get_pokemon_at_slot_mut(&mut next_state, action.user_slot) {
            simulator_helpers::remove_status_volatile(user, &VolatileStatus::SpeedSwap(0));
            user.volatiles.push(crate::state::pokemon::VolatileStatusState::TurnStatus(VolatileStatus::SpeedSwap(user_spe), 0));
            user.stats[5] = target_spe;
        }
        if let Some(target) = simulator_helpers::get_pokemon_at_slot_mut(&mut next_state, target_slot) {
            simulator_helpers::remove_status_volatile(target, &VolatileStatus::SpeedSwap(0));
            target.volatiles.push(crate::state::pokemon::VolatileStatusState::TurnStatus(VolatileStatus::SpeedSwap(target_spe), 0));
            target.stats[5] = user_spe;
        }
        decrement_move_pp(&mut next_state, action.user_slot, &action.move_name, Some(move_data));
        return status_move_self_outcome(next_state, &confusion_self_hit_outcomes);
    }

    // Curse: Ghost-type user loses ½ max HP and inflicts the Curse volatile on the target
    // (¼ HP drain per end of turn, Baton-Passable). Non-Ghost users gain +1 Atk / +1 Def / −1 Spe.
    // Behavior keys off the *current* type (after Terastallization / Soak / etc.).
    // Fails vs a target that already has Curse. No HP cost if the move fails.
    if move_name == PokemonMove::Curse {
        let user_is_ghost = simulator_helpers::get_pokemon_at_slot(&next_state, action.user_slot)
            .map(|m| simulator_helpers::pokemon_has_type(m, &PokemonType::Ghost))
            .unwrap_or(false);
        if user_is_ghost {
            let Some(&target_slot) = target_slots.first() else {
                return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
            };
            let target_already_cursed = simulator_helpers::get_pokemon_at_slot(&next_state, target_slot)
                .map(|m| simulator_helpers::has_status_volatile(m, &VolatileStatus::Curse))
                .unwrap_or(true);
            if target_already_cursed {
                return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
            }
            // Cost: ½ max HP (move still completes even if this faints the user).
            {
                let user_env = simulator_helpers::berry_env(&next_state, action.user_slot);
                let abilities_suppressed = simulator_helpers::abilities_are_suppressed(&next_state);
                if let Some(user) = mon_at_slot_mut(&mut next_state, action.user_slot) {
                    let cost = (user.stats[0].max(1) / 2).max(1);
                    simulator_helpers::take_damage(user, cost, user_env, abilities_suppressed);
                    if user.fainted { simulator_helpers::clear_pokemon_on_faint(user); }
                }
            }
            // Apply Curse volatile to the target (TurnStatus, permanent — Baton Pass carries TurnStatus(Curse,_)).
            if let Some(target) = simulator_helpers::get_pokemon_at_slot_mut(&mut next_state, target_slot) {
                target.volatiles.push(crate::state::pokemon::VolatileStatusState::TurnStatus(VolatileStatus::Curse, 0));
            }
        } else {
            // Non-Ghost: +1 Atk / +1 Def / −1 Spe — routed through the normal boost helpers so
            // Contrary / Simple apply, but does NOT require a target.
            let items_suppressed = simulator_helpers::items_are_suppressed(&next_state);
            let curse_delta = if let Some(user) = simulator_helpers::get_pokemon_at_slot_mut(&mut next_state, action.user_slot) {
                simulator_helpers::apply_stat_boost_external(user, &[1, 1, 0, 0, -1, 0, 0], items_suppressed)
            } else { [0i8; 7] };
            for (boost_idx, &stages) in curse_delta.iter().enumerate() {
                if stages != 0 { simulator_helpers::emit(&mut next_state, EventKind::BoostChanged { target: action.user_slot, boost_idx, stages }); }
            }
        }
        decrement_move_pp(&mut next_state, action.user_slot, &action.move_name, Some(move_data));
        return status_move_self_outcome(next_state, &confusion_self_hit_outcomes);
    }

    // Imprison: apply the Imprison volatile to the user. While active, opposing Pokémon cannot
    // select or use any move the user knows. Fails if the user already has Imprison (no double-stack).
    // Gen V+: succeeds even if no opponent shares a move (the volatile is still applied).
    // Not Baton-Passable — the volatile stays on the user until they switch out.
    if move_name == PokemonMove::Imprison {
        let already_imprisoned = simulator_helpers::get_pokemon_at_slot(&next_state, action.user_slot)
            .map(|m| simulator_helpers::has_status_volatile(m, &VolatileStatus::Imprison))
            .unwrap_or(false);
        if already_imprisoned {
            return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
        }
        if let Some(user) = simulator_helpers::get_pokemon_at_slot_mut(&mut next_state, action.user_slot) {
            user.volatiles.push(crate::state::pokemon::VolatileStatusState::TurnStatus(VolatileStatus::Imprison, 0));
        }
        decrement_move_pp(&mut next_state, action.user_slot, &action.move_name, Some(move_data));
        return status_move_self_outcome(next_state, &confusion_self_hit_outcomes);
    }

    // Topsy-Turvy: invert all of the target's stat stages (×−1 for each of the 7 indices).
    // Fails if every stage is already 0. Always hits (accuracy bypassed via dex data). Does NOT
    // trigger Defiant/Competitive and ignores Contrary/Simple/Clear Body/Mist — manipulate the
    // boost array directly without routing through apply_opponent_stat_drop.
    // boosts[]: atk=0, def=1, spa=2, spd=3, spe=4, acc=5, eva=6
    if move_name == PokemonMove::TopsyTurvy {
        let Some(&target_slot) = target_slots.first() else {
            return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
        };
        let all_zero = simulator_helpers::get_pokemon_at_slot(&next_state, target_slot)
            .map(|m| m.boosts.iter().all(|&b| b == 0))
            .unwrap_or(true);
        if all_zero {
            return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
        }
        if let Some(target) = simulator_helpers::get_pokemon_at_slot_mut(&mut next_state, target_slot) {
            for b in target.boosts.iter_mut() {
                *b = -*b;
            }
        }
        simulator_helpers::emit(&mut next_state, EventKind::BoostsInverted { target: target_slot });
        decrement_move_pp(&mut next_state, action.user_slot, &action.move_name, Some(move_data));
        return status_move_self_outcome(next_state, &confusion_self_hit_outcomes);
    }

    // Skill Swap: exchange user's and target's current abilities.
    // Bypasses Substitute (bypasssub flag). Honors Protect. Fails if either ability is in the
    // failskillswap set. Same-ability swap now succeeds (Gen VI+). Fires on-gain effects for both.
    if move_name == PokemonMove::SkillSwap {
        let Some(&target_slot) = target_slots.first() else {
            return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
        };
        let target = simulator_helpers::get_pokemon_at_slot(&next_state, target_slot).cloned();
        let Some(target) = target else {
            return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
        };
        if simulator_helpers::protect_blocks_move(
            &next_state, action.user_slot, target_slot, &target, move_data, false,
        ).is_some() {
            simulator_helpers::emit(&mut next_state, EventKind::Blocked { target: target_slot });
            return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
        }
        let (user_ability, target_ability) = {
            let user = simulator_helpers::get_pokemon_at_slot(&next_state, action.user_slot);
            (user.map(|m| m.ability.clone()).unwrap_or(Ability::None), target.ability.clone())
        };
        if simulator_helpers::ability_excluded_from_skill_swap(&user_ability)
            || simulator_helpers::ability_excluded_from_skill_swap(&target_ability)
        {
            return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
        }
        // Save copies for the AbilityRevealed events before values are moved into the swap.
        let user_ability_ev = user_ability.clone();
        let target_ability_ev = target_ability.clone();
        if let Some(user) = simulator_helpers::get_pokemon_at_slot_mut(&mut next_state, action.user_slot) {
            if user.original_ability.is_none() { user.original_ability = Some(user_ability.clone()); }
            user.ability = target_ability.clone();
        }
        if let Some(tgt) = simulator_helpers::get_pokemon_at_slot_mut(&mut next_state, target_slot) {
            if tgt.original_ability.is_none() { tgt.original_ability = Some(target_ability); }
            tgt.ability = user_ability;
        }
        // Illusion break: either mon may have lost Illusion in the swap.
        simulator_helpers::maybe_break_illusion_on_ability_change(&mut next_state, action.user_slot);
        simulator_helpers::maybe_break_illusion_on_ability_change(&mut next_state, target_slot);
        simulator_helpers::process_pokemon_gain_ability(&mut next_state, action.user_slot);
        simulator_helpers::process_pokemon_gain_ability(&mut next_state, target_slot);
        simulator_helpers::emit(&mut next_state, EventKind::AbilityRevealed { slot: action.user_slot, ability: target_ability_ev });
        simulator_helpers::emit(&mut next_state, EventKind::AbilityRevealed { slot: target_slot, ability: user_ability_ev });
        decrement_move_pp(&mut next_state, action.user_slot, &action.move_name, Some(move_data));
        return status_move_self_outcome(next_state, &confusion_self_hit_outcomes);
    }

    // Role Play: user copies the target's ability.
    // Bypasses both Substitute and Protect (no protect/bypasssub flags in Showdown data).
    // Fails if same ability; target's ability is in failroleplay set; user's ability is cantsuppress.
    if move_name == PokemonMove::RolePlay {
        let Some(&target_slot) = target_slots.first() else {
            return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
        };
        let (user_ability, target_ability) = {
            let user = simulator_helpers::get_pokemon_at_slot(&next_state, action.user_slot);
            let tgt = simulator_helpers::get_pokemon_at_slot(&next_state, target_slot);
            (
                user.map(|m| m.ability.clone()).unwrap_or(Ability::None),
                tgt.map(|m| m.ability.clone()).unwrap_or(Ability::None),
            )
        };
        if user_ability == target_ability
            || simulator_helpers::ability_cannot_be_role_played(&target_ability)
            || simulator_helpers::ability_cannot_be_suppressed(&user_ability)
        {
            return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
        }
        if let Some(user) = simulator_helpers::get_pokemon_at_slot_mut(&mut next_state, action.user_slot) {
            if user.original_ability.is_none() { user.original_ability = Some(user_ability); }
            user.ability = target_ability;
        }
        // Illusion break: user may have lost Illusion (gained a new ability).
        simulator_helpers::maybe_break_illusion_on_ability_change(&mut next_state, action.user_slot);
        simulator_helpers::process_pokemon_gain_ability(&mut next_state, action.user_slot);
        decrement_move_pp(&mut next_state, action.user_slot, &action.move_name, Some(move_data));
        return status_move_self_outcome(next_state, &confusion_self_hit_outcomes);
    }

    // Entrainment: target copies the user's ability.
    // Honors Protect and Substitute. Fails if same ability; target is in cantsuppress or Truant;
    // user's ability is in noentrain set.
    if move_name == PokemonMove::Entrainment {
        let Some(&target_slot) = target_slots.first() else {
            return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
        };
        let target = simulator_helpers::get_pokemon_at_slot(&next_state, target_slot).cloned();
        let Some(target) = target else {
            return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
        };
        if simulator_helpers::protect_blocks_move(
            &next_state, action.user_slot, target_slot, &target, move_data, false,
        ).is_some() {
            simulator_helpers::emit(&mut next_state, EventKind::Blocked { target: target_slot });
            return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
        }
        if simulator_helpers::has_status_volatile(&target, &VolatileStatus::Substitute(0)) {
            return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
        }
        let (user_ability, target_ability) = {
            let user = simulator_helpers::get_pokemon_at_slot(&next_state, action.user_slot);
            (user.map(|m| m.ability.clone()).unwrap_or(Ability::None), target.ability.clone())
        };
        if user_ability == target_ability
            || simulator_helpers::ability_cannot_be_suppressed(&target_ability)
            || target_ability == Ability::Truant
            || simulator_helpers::ability_excluded_from_entrainment_user(&user_ability)
        {
            return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
        }
        if let Some(tgt) = simulator_helpers::get_pokemon_at_slot_mut(&mut next_state, target_slot) {
            if tgt.original_ability.is_none() { tgt.original_ability = Some(target_ability); }
            tgt.ability = user_ability;
        }
        // Illusion break: target may have lost Illusion (gained a new ability).
        simulator_helpers::maybe_break_illusion_on_ability_change(&mut next_state, target_slot);
        simulator_helpers::process_pokemon_gain_ability(&mut next_state, target_slot);
        decrement_move_pp(&mut next_state, action.user_slot, &action.move_name, Some(move_data));
        return status_move_self_outcome(next_state, &confusion_self_hit_outcomes);
    }

    // Gastro Acid: suppress the target's ability via the GastroAcid volatile.
    // Does NOT overwrite the ability field; suppression is checked via pokemon_ability_is_suppressed.
    // Auto-reverts on switch-out when volatiles are cleared. Honors Protect and Substitute.
    // Fails if target's ability is in the cantsuppress set.
    if move_name == PokemonMove::GastroAcid {
        let Some(&target_slot) = target_slots.first() else {
            return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
        };
        let target = simulator_helpers::get_pokemon_at_slot(&next_state, target_slot).cloned();
        let Some(target) = target else {
            return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
        };
        if simulator_helpers::protect_blocks_move(
            &next_state, action.user_slot, target_slot, &target, move_data, false,
        ).is_some() {
            simulator_helpers::emit(&mut next_state, EventKind::Blocked { target: target_slot });
            return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
        }
        if simulator_helpers::has_status_volatile(&target, &VolatileStatus::Substitute(0)) {
            return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
        }
        if simulator_helpers::ability_cannot_be_suppressed(&target.ability) {
            return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
        }
        let state_snapshot = next_state.clone();
        if let Some(tgt) = simulator_helpers::get_pokemon_at_slot_mut(&mut next_state, target_slot) {
            // apply_volatile_to_pokemon_pub skips if already present, so no duplicate guard needed.
            simulator_helpers::apply_volatile_to_pokemon_pub(&state_snapshot, tgt, &VolatileStatus::GastroAcid);
        }
        // Borrow ended; GastroAcid blocks a protected ability (checked above), so it always lands.
        simulator_helpers::emit(&mut next_state, EventKind::VolatileStart {
            target: target_slot,
            volatile: VolatileStatus::GastroAcid,
        });
        // Illusion break: GastroAcid suppresses the target's ability, including Illusion.
        simulator_helpers::maybe_break_illusion_on_ability_change(&mut next_state, target_slot);
        decrement_move_pp(&mut next_state, action.user_slot, &action.move_name, Some(move_data));
        return status_move_self_outcome(next_state, &confusion_self_hit_outcomes);
    }

    // Worry Seed: change target's ability to Insomnia; cure sleep if affected.
    // Fails (via onTryImmunity) if target's ability is Truant or already Insomnia.
    // Fails (via onTryHit cantsuppress) for protected abilities. Honors Protect and Substitute.
    if move_name == PokemonMove::WorrySeed {
        let Some(&target_slot) = target_slots.first() else {
            return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
        };
        let target = simulator_helpers::get_pokemon_at_slot(&next_state, target_slot).cloned();
        let Some(target) = target else {
            return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
        };
        if simulator_helpers::protect_blocks_move(
            &next_state, action.user_slot, target_slot, &target, move_data, false,
        ).is_some() {
            simulator_helpers::emit(&mut next_state, EventKind::Blocked { target: target_slot });
            return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
        }
        if simulator_helpers::has_status_volatile(&target, &VolatileStatus::Substitute(0)) {
            return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
        }
        if target.ability == Ability::Truant
            || target.ability == Ability::Insomnia
            || simulator_helpers::ability_cannot_be_suppressed(&target.ability)
        {
            return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
        }
        if let Some(tgt) = simulator_helpers::get_pokemon_at_slot_mut(&mut next_state, target_slot) {
            if tgt.original_ability.is_none() { tgt.original_ability = Some(tgt.ability.clone()); }
            tgt.ability = Ability::Insomnia;
            if matches!(tgt.status, Some(Status::Sleep(_))) {
                tgt.status = None;
            }
        }
        // Illusion break: target may have lost Illusion (gained Insomnia).
        simulator_helpers::maybe_break_illusion_on_ability_change(&mut next_state, target_slot);
        decrement_move_pp(&mut next_state, action.user_slot, &action.move_name, Some(move_data));
        return status_move_self_outcome(next_state, &confusion_self_hit_outcomes);
    }

    // Simple Beam: change target's ability to Simple.
    // Fails if target's ability is already Simple or Truant, or is in cantsuppress set.
    // Honors Protect and Substitute.
    if move_name == PokemonMove::SimpleBeam {
        let Some(&target_slot) = target_slots.first() else {
            return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
        };
        let target = simulator_helpers::get_pokemon_at_slot(&next_state, target_slot).cloned();
        let Some(target) = target else {
            return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
        };
        if simulator_helpers::protect_blocks_move(
            &next_state, action.user_slot, target_slot, &target, move_data, false,
        ).is_some() {
            simulator_helpers::emit(&mut next_state, EventKind::Blocked { target: target_slot });
            return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
        }
        if simulator_helpers::has_status_volatile(&target, &VolatileStatus::Substitute(0)) {
            return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
        }
        if target.ability == Ability::Simple
            || target.ability == Ability::Truant
            || simulator_helpers::ability_cannot_be_suppressed(&target.ability)
        {
            return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
        }
        if let Some(tgt) = simulator_helpers::get_pokemon_at_slot_mut(&mut next_state, target_slot) {
            if tgt.original_ability.is_none() { tgt.original_ability = Some(tgt.ability.clone()); }
            tgt.ability = Ability::Simple;
        }
        decrement_move_pp(&mut next_state, action.user_slot, &action.move_name, Some(move_data));
        return status_move_self_outcome(next_state, &confusion_self_hit_outcomes);
    }

    // Power Trick / Power Shift: swap the user's raw Attack and Defense stats.
    // Using the move again re-swaps (restoring original values). The volatile tracks
    // whether the swap is currently active; it is reverted on switch-out.
    if move_name == PokemonMove::PowerTrick || move_name == PokemonMove::PowerShift {
        let volatile = if move_name == PokemonMove::PowerTrick {
            VolatileStatus::PowerTrick
        } else {
            VolatileStatus::PowerShift
        };
        if let Some(user) = simulator_helpers::get_pokemon_at_slot_mut(&mut next_state, action.user_slot) {
            user.stats.swap(1, 2); // Swap raw Atk and Def
            if simulator_helpers::has_status_volatile(user, &volatile) {
                simulator_helpers::remove_status_volatile(user, &volatile);
            } else {
                user.volatiles.push(crate::state::pokemon::VolatileStatusState::TurnStatus(volatile, 0));
            }
        }
        decrement_move_pp(&mut next_state, action.user_slot, &action.move_name, Some(move_data));
        return status_move_self_outcome(next_state, &confusion_self_hit_outcomes);
    }

    // Round: pull later-queued Round actions to move immediately after this one by
    // setting moves_first. round_used_this_turn is set in apply_post_damage_move_effects
    // AFTER damage resolves, so the first Round still gets BP 60 and subsequent ones 120.
    if move_name == PokemonMove::Round {
        for queued in next_state.action_queue.iter_mut() {
            if let Action::MoveAction(ma) = queued {
                if ma.move_name == PokemonMove::Round {
                    ma.moves_first = true;
                }
            }
        }
    }

    // Dragon Darts: two-strike move. In doubles with both foes valid, one strike lands
    // on each foe (no 0.75× spread penalty since each hit is single-target).
    // "Valid" = non-fainted, not Fairy-immune, not ability-immune to Dragon, not in a
    // semi-invulnerable state, not Protect-protected. Follow Me / Rage Powder already
    // redirected target_slots before we get here. In singles, falls through to the
    // standard 2-hit multi-hit path via the `is_multihit_move` flag.
    if move_name == PokemonMove::DragonDarts {
        let opposing_player = match action.user_slot.player {
            Player::P1 => Player::P2,
            Player::P2 => Player::P1,
        };
        let foe_slots: Vec<FieldSlot> = {
            let mons = match opposing_player {
                Player::P1 => &next_state.p1_active_mons,
                Player::P2 => &next_state.p2_active_mons,
            };
            mons.iter().enumerate()
                .filter(|(_, m)| !m.fainted)
                .map(|(i, _)| FieldSlot { player: opposing_player, slot_index: i as u8 })
                .collect()
        };

        if foe_slots.len() >= 2 {
            // Check type-effectiveness to see if either slot is immune (Fairy-immune etc.)
            let is_valid = |slot: FieldSlot| -> bool {
                let Some(tgt) = simulator_helpers::get_pokemon_at_slot(&next_state, slot) else { return false; };
                let eff = simulator_helpers::move_type_effectiveness(&next_state, &move_data.pokemon_type, tgt);
                eff > 0.0
            };
            let slot0_valid = is_valid(foe_slots[0]);
            let slot1_valid = is_valid(foe_slots[1]);

            let dart_targets: [FieldSlot; 2] = match (slot0_valid, slot1_valid) {
                (true, true) => [foe_slots[0], foe_slots[1]], // split
                (true, false) => [foe_slots[0], foe_slots[0]], // both to slot0
                (false, true) => [foe_slots[1], foe_slots[1]], // both to slot1
                (false, false) => {
                    // Both immune — move fails
                    decrement_move_pp(&mut next_state, action.user_slot, &action.move_name, Some(move_data));
                    return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
                }
            };

            // Execute two sequential single-hit branches, one per dart target (no spread multiplier)
            let mut outcomes: Vec<(BattleState, f64)> = vec![(next_state.clone(), 1.0)];
            for dart_slot in &dart_targets {
                let mut next_outcomes = Vec::new();
                for (bs, prob) in outcomes {
                    let Some(current_target) = simulator_helpers::get_pokemon_at_slot(&bs, *dart_slot).cloned() else {
                        next_outcomes.push((bs, prob));
                        continue;
                    };
                    let (invuln_mult, ok) = check_invulnerability_status(&attacker, &current_target, &move_name);
                    if !ok { next_outcomes.push((bs, prob)); continue; }
                    let hit_p = simulator_helpers::accuracy_hit_probability(&bs, &attacker, &current_target, action.user_slot, *dart_slot, move_data).clamp(0.0, 1.0);
                    if hit_p < 1.0 {
                        next_outcomes.push((bs.clone(), prob * (1.0 - hit_p)));
                    }
                    if hit_p > 0.0 {
                        let dmg_outcomes = simulator_helpers::calculate_damage_outcomes_for_target(&bs, &attacker, &current_target, action.user_slot, *dart_slot, move_data, config, 1.0, invuln_mult);
                        for (dmg, is_crit, dp) in dmg_outcomes {
                            for (new_bs, new_p) in apply_single_hit_branch(bs.clone(), *dart_slot, &move_name, move_data, dmg, action.user_slot, prob * hit_p * dp, is_crit) {
                                next_outcomes.push((new_bs, new_p));
                            }
                        }
                    }
                }
                outcomes = simulator_helpers::coalesce_branches(next_outcomes);
            }

            let all_outcomes: Vec<(MatchState, f64)> = outcomes.into_iter()
                .flat_map(|(bs, p)| {
                    apply_post_damage_move_effects(bs, action.user_slot, move_data, &next_state, opposing_player)
                        .into_iter()
                        .map(move |(st, w)| (st, p * w))
                        .collect::<Vec<_>>()
                })
                .collect();

            let all_outcomes: Vec<(MatchState, f64)> = all_outcomes.into_iter()
                .map(|(state, prob)| (state, prob))
                .collect();

            let move_has_recharge = simulator_helpers::move_has_flag(move_data, &crate::state::dex_data::MoveFlag::Recharge);
            let mut all_outcomes = all_outcomes;
            if move_has_recharge {
                for (state, _) in &mut all_outcomes {
                    if let MatchState::BattleState(bs) = state {
                        if let Some(mon) = mon_at_slot_mut(bs, action.user_slot) {
                            mon.volatiles.push(crate::state::pokemon::VolatileStatusState::TurnStatus(VolatileStatus::MustRecharge, 2));
                        }
                        simulator_helpers::emit(bs, EventKind::MustRecharge { slot: action.user_slot });
                    }
                }
            }
            for (state, _) in &mut all_outcomes {
                if let MatchState::BattleState(bs) = state {
                    decrement_move_pp(bs, action.user_slot, &action.move_name, Some(move_data));
                }
            }

            let has_confusion = confusion_self_hit_outcomes.is_some();
            let mut final_outcomes: Vec<(MatchState, f64)> = Vec::new();
            if let Some(ref confusion_outcomes) = confusion_self_hit_outcomes {
                for (state, prob) in confusion_outcomes {
                    final_outcomes.push((state.clone(), prob * (1.0 / 3.0)));
                }
            }
            for (state, prob) in all_outcomes {
                final_outcomes.push((state, prob * if has_confusion { 2.0 / 3.0 } else { 1.0 }));
            }
            if !final_outcomes.is_empty() {
                return simulator_helpers::coalesce_branches(final_outcomes);
            }
        }
        // Single target or not doubles — fall through to normal 2-hit multi-hit path
    }

    // Calculate targets multiplier (0.75x for 2+ targets, 1.0x for 1 target)
    let targets_mult = simulator_helpers::damage_targets_multiplier(target_slots.len());

    let is_multihit_move = move_name == PokemonMove::BeatUp
        || move_name == PokemonMove::DragonDarts // singles: both hits same target
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
            decrement_move_pp(&mut next_state, action.user_slot, &action.move_name, Some(move_data));
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
            .flat_map(|(state, prob)| match state {
                MatchState::BattleState(bs) => {
                    apply_post_damage_move_effects(bs, action.user_slot, move_data, &next_state, opposing_player)
                        .into_iter()
                        .map(move |(st, w)| (st, prob * w))
                        .collect::<Vec<_>>()
                }
                other => vec![(other, prob)],
            })
            .collect();

        let move_has_recharge = simulator_helpers::move_has_flag(move_data, &crate::state::dex_data::MoveFlag::Recharge);
        if move_has_recharge {
            for (state, _) in &mut all_outcomes {
                if let MatchState::BattleState(bs) = state {
                    if let Some(mon) = match action.user_slot.player {
                        Player::P1 => bs.p1_active_mons.get_mut(action.user_slot.slot_index as usize),
                        Player::P2 => bs.p2_active_mons.get_mut(action.user_slot.slot_index as usize),
                    } {
                        mon.volatiles.push(crate::state::pokemon::VolatileStatusState::TurnStatus(VolatileStatus::MustRecharge, 2));
                    }
                    simulator_helpers::emit(bs, EventKind::MustRecharge { slot: action.user_slot });
                }
            }
        }

        for (state, _) in &mut all_outcomes {
            if let MatchState::BattleState(bs) = state {
                decrement_move_pp(bs, action.user_slot, &action.move_name, Some(move_data));
            }
        }

        let has_confusion = confusion_self_hit_outcomes.is_some();
        let mut final_outcomes: Vec<(MatchState, f64)> = Vec::new();
        if let Some(confusion_outcomes) = confusion_self_hit_outcomes {
            for (state, prob) in confusion_outcomes {
                final_outcomes.push((state.clone(), prob * (1.0 / 3.0)));
            }
        }

        for (state, prob) in all_outcomes {
            final_outcomes.push((state, prob * if has_confusion { 2.0 / 3.0 } else { 1.0 }));
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
    // Mold Breaker bypasses Damp (Damp is an ignorable ability).
    let is_explosive_move = matches!(
        move_name,
        PokemonMove::SelfDestruct
            | PokemonMove::Explosion
            | PokemonMove::MindBlown
            | PokemonMove::MistyExplosion
    );
    if is_explosive_move {
        // Mold Breaker bypasses Damp only for opponent mons (Damp is ignorable).
        // Ally Damp (same side as attacker) still blocks explosion even with Mold Breaker.
        let attacker_breaks = simulator_helpers::attacker_breaks_mold(&next_state, &attacker);
        let attacker_player = action.user_slot.player;
        let damp_on_field = next_state
            .p1_active_mons.iter().map(|m| (m, Player::P1))
            .chain(next_state.p2_active_mons.iter().map(|m| (m, Player::P2)))
            .filter(|(m, _)| !m.fainted)
            .any(|(mon, side)| {
                // Mold Breaker skips opponent Damp (same player ↔ ally, always checked).
                let skip = attacker_breaks && side != attacker_player;
                !skip
                    && !simulator_helpers::pokemon_ability_is_suppressed(&next_state, mon)
                    && mon.ability == Ability::Damp
            });
        if damp_on_field {
            return no_effect_outcome(&next_state, action, &confusion_self_hit_outcomes);
        }
    }

    // Parental Bond: attacker hits twice; second hit at 25% BP. Excluded from: spread moves,
    // moves that already hit multiple times (handled above by is_multihit_move), fixed-damage
    // moves, self-destruct / explosion, charge moves, Fling, Uproar/Rollout/Ice Ball.
    let parental_bond_eligible = {
        let attacker_has_pb = !simulator_helpers::pokemon_ability_is_suppressed(&next_state, &attacker)
            && attacker.ability == Ability::ParentalBond;
        attacker_has_pb
            && target_slots.len() == 1  // spread moves excluded
            && matches!(move_data.category, MoveCategory::Physical | MoveCategory::Special)
            && matches!(move_data.damage_override, crate::state::dex_data::DamageOverride::None)  // exclude fixed-damage moves
            && !move_data.ohko
            && !simulator_helpers::move_has_flag(move_data, &crate::state::dex_data::MoveFlag::NoParentalBond)
            // Uproar and Beat Up lack the NoParentalBond flag in data; excluded manually.
            && !matches!(move_name, PokemonMove::Uproar | PokemonMove::BeatUp)
            && !simulator_helpers::move_has_flag(move_data, &crate::state::dex_data::MoveFlag::Charge)
    };

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
            && simulator_helpers::pokemon_is_on_terrain(&next_state, &target, &crate::state::dex_data::Terrain::PsychicTerrain)
        {
            outcomes_for_target.push((0, false, false, 1.0));
            per_target_outcomes.push((*target_slot, outcomes_for_target));
            continue;
        }

        // Queenly Majesty / Armor Tail / Dazzling: block any move with increased effective
        // priority (after Prankster / Gale Wings boosts) from an opposing mon.
        // Bypassed by Mold Breaker / Turboblaze / Teravolt (all three abilities are ignorable).
        // Exception: spread/field-targeting moves are not blocked (doubles edge case).
        let effective_priority = simulator_helpers::effective_move_priority(&next_state, &attacker, move_data);
        let target_has_priority_block_ability =
            !simulator_helpers::attacker_breaks_mold(&next_state, &attacker)
            && !simulator_helpers::pokemon_ability_is_suppressed(&next_state, &target)
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
                crate::state::pokemon::VolatileStatusState::MoveStatus(VolatileStatus::SemiInvulnerable(_), _)
                    | crate::state::pokemon::VolatileStatusState::TurnStatus(VolatileStatus::SkyDrop, _)
            )
        });

        if matches!(move_data.pokemon_type, PokemonType::Ground)
            && !simulator_helpers::pokemon_is_grounded(&next_state, &target)
            && !target_is_semi_invulnerable
            && !matches!(move_name, PokemonMove::ThousandArrows | PokemonMove::ThousandWaves)
        {
            // Mold Breaker bypasses Levitate and Eelevate specifically
            // (but not Flying-type, Air Balloon, etc.)
            let mold_breaks_levitate = simulator_helpers::attacker_breaks_mold(&next_state, &attacker)
                && !simulator_helpers::pokemon_ability_is_suppressed(&next_state, &target)
                && matches!(target.ability, Ability::Levitate | Ability::Eelevate);
            if !mold_breaks_levitate {
                outcomes_for_target.push((0, false, false, 1.0));
                per_target_outcomes.push((*target_slot, outcomes_for_target));
                continue;
            }
        }

        // Bulletproof: immune to all ball and bomb moves (MoveFlag::Bullet).
        // Blocks even an ally's Pollen Puff — no ally exemption.
        // Mold Breaker / Turboblaze / Teravolt bypass Bulletproof (it is ignorable).
        if simulator_helpers::move_has_flag(move_data, &crate::state::dex_data::MoveFlag::Bullet)
            && !simulator_helpers::attacker_breaks_mold(&next_state, &attacker)
            && !simulator_helpers::pokemon_ability_is_suppressed(&next_state, &target)
            && target.ability == Ability::Bulletproof
        {
            outcomes_for_target.push((0, false, false, 1.0));
            per_target_outcomes.push((*target_slot, outcomes_for_target));
            continue;
        }

        // Soundproof: immune to sound-based moves (MoveFlag::Sound).
        // The holder is NOT immune to its own sound moves (Gen VIII+ / Champions behaviour).
        // Mold Breaker / Turboblaze / Teravolt bypass Soundproof (it is ignorable).
        if simulator_helpers::move_has_flag(move_data, &crate::state::dex_data::MoveFlag::Sound)
            && action.user_slot != *target_slot
            && !simulator_helpers::attacker_breaks_mold(&next_state, &attacker)
            && !simulator_helpers::pokemon_ability_is_suppressed(&next_state, &target)
            && target.ability == Ability::Soundproof
        {
            outcomes_for_target.push((0, false, false, 1.0));
            per_target_outcomes.push((*target_slot, outcomes_for_target));
            continue;
        }

        // Good as Gold: immune to single-target status moves used by OTHER Pokémon
        // (including beneficial ally moves like Helping Hand / Howl). The holder's own
        // self-targeted status moves still work (action.user_slot != target_slot). Side-
        // targeting moves (Stealth Rock, Reflect, Tailwind) and whole-field moves (Haze,
        // Trick Room, weather) are NOT blocked — they are excluded by the single-target
        // gate below. Mold Breaker does NOT bypass Good as Gold (it is not a breakable
        // ability), so no attacker_breaks_mold guard. Blocked moves fail without paying any
        // self-cost because we `continue` before the move's effects are applied for this target.
        if matches!(move_data.category, MoveCategory::Status)
            && action.user_slot != *target_slot
            && matches!(
                move_data.target,
                MoveTarget::Normal
                    | MoveTarget::Any
                    | MoveTarget::AdjacentFoe
                    | MoveTarget::AdjacentAlly
                    | MoveTarget::AdjacentAllyOrSelf
                    | MoveTarget::RandomNormal
            )
            && !simulator_helpers::pokemon_ability_is_suppressed(&next_state, &target)
            && target.ability == Ability::GoodasGold
        {
            outcomes_for_target.push((0, false, false, 1.0));
            per_target_outcomes.push((*target_slot, outcomes_for_target));
            continue;
        }

        // Overcoat: immune to powder/spore moves (MoveFlag::Powder).
        // is_immune_to_powder also covers Grass-type and Safety Goggles; Mold Breaker
        // only bypasses Overcoat — not the type-based or item-based immunities.
        // Weather-damage immunity is handled separately in apply_weather_residual.
        if simulator_helpers::move_has_flag(move_data, &crate::state::dex_data::MoveFlag::Powder)
            && simulator_helpers::is_immune_to_powder(&next_state, &target, Some(&attacker))
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
            // target. Long Reach removes contact; Protective Pads blocks the punishment.
            if simulator_helpers::contact_effects_apply(&next_state, &attacker, move_data) {
                simulator_helpers::apply_protect_contact_punishment(
                    &mut next_state, action.user_slot, *target_slot, kind,
                );
            }
            simulator_helpers::emit(&mut next_state, EventKind::Blocked { target: *target_slot });
            outcomes_for_target.push((0, false, false, 1.0));
            per_target_outcomes.push((*target_slot, outcomes_for_target));
            continue;
        }

        // Magic Bounce: reflect opponent-targeted status moves back at the attacker.
        // Fires before accuracy / invulnerability checks. Mold Breaker bypasses it.
        // Only moves with the Reflectable flag are bounced (sourced from move data).
        if matches!(move_data.category, MoveCategory::Status)
            && target_slot.player != action.user_slot.player
            && simulator_helpers::move_is_reflectable(&move_data)
            && !simulator_helpers::attacker_breaks_mold(&next_state, &attacker)
        {
            let target_has_mb = simulator_helpers::get_pokemon_at_slot(&next_state, *target_slot)
                .map_or(false, |t| {
                    !simulator_helpers::pokemon_ability_is_suppressed(&next_state, t)
                    && t.ability == Ability::MagicBounce
                });
            if target_has_mb {
                // Bounce: apply the move's effects to the ATTACKER (user_slot) as if the
                // target (defender) used them. Swap attacker ↔ target slots so that
                // side-condition hazards land on the original attacker's side.
                let bounce_branches = simulator_helpers::apply_secondary_effects(
                    &next_state, *target_slot, action.user_slot, move_data,
                );
                decrement_move_pp(&mut next_state, action.user_slot, &action.move_name, Some(move_data));
                let mut final_outcomes: Vec<(MatchState, f64)> = bounce_branches
                    .into_iter().map(|(bs, p)| (MatchState::BattleState(bs), p)).collect();
                let has_confusion = confusion_self_hit_outcomes.is_some();
                if let Some(confusion_outcomes) = confusion_self_hit_outcomes {
                    for (s, p) in confusion_outcomes { final_outcomes.push((s, p * (1.0 / 3.0))); }
                    for (_, p) in &mut final_outcomes { *p *= if has_confusion { 2.0 / 3.0 } else { 1.0 }; }
                }
                return simulator_helpers::coalesce_branches(final_outcomes);
            }
        }

        if !should_continue {
            // Move is blocked by invulnerability. PP is decremented once at the end
            // (line ~4291 applies to all_outcomes), so we do NOT call decrement_move_pp
            // here — doing so would double-count it (next_state is cloned into all_outcomes
            // and then decremented again).
            outcomes_for_target.push((0, false, false, 1.0));
            per_target_outcomes.push((*target_slot, outcomes_for_target));
            continue;
        }

        // Feint / Phantom Force / Shadow Force: breaks_protect moves remove the target's protect
        // volatile and the target's side QuickGuard/WideGuard so follow-up moves can hit freely.
        if move_data.breaks_protect {
            if let Some(tgt_mon) = mon_at_slot_mut(&mut next_state, *target_slot) {
                tgt_mon.volatiles.retain(|v| !matches!(v,
                    crate::state::pokemon::VolatileStatusState::TurnStatus(VolatileStatus::Protect, _)
                    | crate::state::pokemon::VolatileStatusState::TurnStatus(VolatileStatus::KingsShield, _)
                    | crate::state::pokemon::VolatileStatusState::TurnStatus(VolatileStatus::SpikyShield, _)
                    | crate::state::pokemon::VolatileStatusState::TurnStatus(VolatileStatus::BanefulBunker, _)
                ));
            }
            // Remove QuickGuard and WideGuard from the target's side (Feint lifts these).
            let target_side = match target_slot.player {
                Player::P1 => &mut next_state.p1_side_conditions,
                Player::P2 => &mut next_state.p2_side_conditions,
            };
            target_side.retain(|c| !matches!(c,
                SideCondition::QuickGuard | SideCondition::WideGuard
            ));
        }

        let weather_blocks_move = matches!(move_data.category, MoveCategory::Physical | MoveCategory::Special)
            && ((simulator_helpers::weather_is_heavy_rain(&next_state) && matches!(move_data.pokemon_type, PokemonType::Fire))
                || (simulator_helpers::weather_is_harsh_sunlight(&next_state) && matches!(move_data.pokemon_type, PokemonType::Water)));

        if weather_blocks_move {
            if move_data.thaws_target {
                let old_target_status = match target_slot.player {
                    Player::P1 => next_state.p1_active_mons.get(target_slot.slot_index as usize),
                    Player::P2 => next_state.p2_active_mons.get(target_slot.slot_index as usize),
                }.and_then(|m| if matches!(m.status, Some(Status::Frozen(_))) { m.status.clone() } else { None });
                if let Some(target_mon) = match target_slot.player {
                    Player::P1 => next_state.p1_active_mons.get_mut(target_slot.slot_index as usize),
                    Player::P2 => next_state.p2_active_mons.get_mut(target_slot.slot_index as usize),
                } {
                    if matches!(target_mon.status, Some(Status::Frozen(_))) {
                        target_mon.status = None;
                    }
                }
                if let Some(old_status) = old_target_status {
                    simulator_helpers::emit(&mut next_state, EventKind::StatusCured { target: *target_slot, status: old_status });
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
                    // King's Rock / Stench: 10% flinch on damaging hits (combined chance = 1 - 0.9^hits).
                    let hit_branches = if *damage > 0 {
                        let b = simulator_helpers::apply_kings_rock_flinch(hit_branches, action.user_slot, *target_slot, move_data, 1);
                        simulator_helpers::apply_stench_flinch(b, action.user_slot, *target_slot, move_data, 1)
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
                        apply_forced_switch(hit_branches, action.user_slot, *target_slot, move_data, pokemon_dex, move_dex)
                    } else {
                        hit_branches
                    };
                    // Crash-damage moves (High Jump Kick, Axe Kick, Supercell Slam) also take
                    // ½ max HP when the move is a type-immune or ability-immune hit (hit==true
                    // but damage==0). Covers Ghost vs HJK/Axe Kick and Volt Absorb / Motor Drive
                    // vs Supercell Slam. Magic Guard on the user skips the damage (handled inside
                    // apply_hp_damage_to_attacker).
                    let hit_branches: Vec<(BattleState, f64)> = if move_data.has_crash_damage && *damage == 0 {
                        hit_branches.into_iter().map(|(mut bs, p)| {
                            simulator_helpers::apply_hp_damage_to_attacker(&mut bs, action.user_slot, 1, 2);
                            (bs, p)
                        }).collect()
                    } else {
                        hit_branches
                    };
                    // Parental Bond: second hit at 25% BP. Rolls independently for damage, crit,
                    // and secondary effects. Short-circuits if the target fainted on the first hit.
                    let hit_branches: Vec<(BattleState, f64)> = if parental_bond_eligible {
                        let second_bp = Some((move_data.base_power / 4).max(1));
                        let mut pb_branches = Vec::new();
                        for (first_bs, first_prob) in hit_branches {
                            let tgt_fainted = simulator_helpers::get_pokemon_at_slot(&first_bs, *target_slot)
                                .map_or(true, |t| t.fainted);
                            if tgt_fainted {
                                pb_branches.push((first_bs, first_prob));
                                continue;
                            }
                            let atk2 = simulator_helpers::get_pokemon_at_slot(&first_bs, action.user_slot).cloned();
                            let tgt2 = simulator_helpers::get_pokemon_at_slot(&first_bs, *target_slot).cloned();
                            if let (Some(atk2), Some(tgt2)) = (atk2, tgt2) {
                                let (inv2, _) = check_invulnerability_status(&atk2, &tgt2, &move_name);
                            let second_outcomes = simulator_helpers::calculate_damage_outcomes_for_target_with_options(
                                    &first_bs, &atk2, &tgt2, action.user_slot, *target_slot,
                                    move_data, config, targets_mult, inv2,
                                    second_bp, None,
                                );
                                for (s_dmg, s_crit, s_prob) in second_outcomes {
                                    let second_branches = apply_single_hit_branch(
                                        first_bs.clone(), *target_slot, &move_name, move_data,
                                        s_dmg, action.user_slot, first_prob * s_prob, s_crit,
                                    );
                                    let second_branches = if s_dmg > 0 {
                                        let b = simulator_helpers::apply_kings_rock_flinch(second_branches, action.user_slot, *target_slot, move_data, 1);
                                        simulator_helpers::apply_stench_flinch(b, action.user_slot, *target_slot, move_data, 1)
                                    } else { second_branches };
                                    pb_branches.extend(second_branches);
                                }
                            } else {
                                pb_branches.push((first_bs, first_prob));
                            }
                        }
                        pb_branches
                    } else {
                        hit_branches
                    };
                    for (bs, prob) in hit_branches {
                        new_all_outcomes.push((MatchState::BattleState(bs), prob));
                    }
                } else {
                    // Miss: only thaw a frozen target if a thaws_target move is used in harsh sun
                    let mut branch_state = branch_state;
                    if simulator_helpers::weather_is_harsh_sunlight(&branch_state)
                        && move_data.thaws_target
                    {
                        let old_frozen = mon_at_slot_mut(&mut branch_state, *target_slot)
                            .and_then(|m| if matches!(m.status, Some(Status::Frozen(_))) {
                                let s = m.status.clone();
                                m.status = None;
                                s
                            } else { None });
                        if let Some(old_status) = old_frozen {
                            simulator_helpers::emit(&mut branch_state, EventKind::StatusCured { target: *target_slot, status: old_status });
                        }
                    }
                    // Crash-damage moves (High Jump Kick, Axe Kick, Supercell Slam) take ½ max HP
                    // when the move fails to connect. This branch covers: accuracy miss,
                    // Protect-block, target in semi-invulnerable phase, and ability draw-in
                    // (Lightning Rod / Storm Drain / Volt Absorb that redirected the move).
                    // The no-valid-target early return above ensures there is no crash when there
                    // is no target. Magic Guard on the user skips the damage.
                    if move_data.has_crash_damage {
                        simulator_helpers::apply_hp_damage_to_attacker(&mut branch_state, action.user_slot, 1, 2);
                    }
                    new_all_outcomes.push((MatchState::BattleState(branch_state), combined_prob));
                }
            }
        }

        all_outcomes = new_all_outcomes;
    }

    // Apply post-damage move effects that depend on total HP damage dealt.
    let opposing_player = opposing_player(action.user_slot.player);
    let mut all_outcomes: Vec<(MatchState, f64)> = all_outcomes
        .into_iter()
        .flat_map(|(state, prob)| match state {
            MatchState::BattleState(bs) => {
                apply_post_damage_move_effects(bs, action.user_slot, move_data, &next_state, opposing_player)
                    .into_iter()
                    .map(move |(st, w)| (st, prob * w))
                    .collect::<Vec<_>>()
            }
            other => vec![(other, prob)],
        })
        .collect();

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
                    mon.volatiles.push(crate::state::pokemon::VolatileStatusState::TurnStatus(VolatileStatus::MustRecharge, 2));
                }
                simulator_helpers::emit(bs, EventKind::MustRecharge { slot: action.user_slot });
            }
        }
    }

    // Decrement PP once at the end
    for (state, _) in &mut all_outcomes {
        if let MatchState::BattleState(bs) = state {
            decrement_move_pp(bs, action.user_slot, &action.move_name, Some(move_data));
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
        // A move prevented by full paralysis.
        simulator_helpers::note_move_outcome(&mut fail_state, action.user_slot, simulator_helpers::MoveOutcome::Cant(CantReason::Paralysis));
        // Paralysis disrupts a rampage lock. Use post-decrement turns from `attacker`.
        let rampage_lock_turns = attacker.volatiles.iter().find_map(|v| {
            if let crate::state::pokemon::VolatileStatusState::MoveStatus(VolatileStatus::LockedMove(_), t) = v { Some(*t) } else { None }
        });
        if let Some(turns) = rampage_lock_turns {
            let is_misty = matches!(fail_state.terrain, Some(crate::state::dex_data::Terrain::MistyTerrain));
            let confusion_added = if let Some(mon) = mon_at_slot_mut(&mut fail_state, action.user_slot) {
                disrupt_rampage_lock(mon, turns, is_misty)
            } else { false };
            simulator_helpers::emit(&mut fail_state, EventKind::VolatileEnd {
                target: action.user_slot,
                volatile: VolatileStatus::LockedMove(PokemonMove::Struggle),
            });
            if confusion_added {
                simulator_helpers::emit(&mut fail_state, EventKind::VolatileStart {
                    target: action.user_slot,
                    volatile: VolatileStatus::Confusion,
                });
            }
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
                    crate::state::dex_data::Status::Frozen(n) => {
                        let new_n = n.saturating_add(1);
                        mon.status = Some(crate::state::dex_data::Status::Frozen(new_n));
                    }
                    crate::state::dex_data::Status::Sleep(n) => {
                        let new_n = n.saturating_add(1);
                        mon.status = Some(crate::state::dex_data::Status::Sleep(new_n));
                    }
                    _ => {}
                }
            }

        }
        // A move prevented by sleep/freeze — pick the right CantReason from the (now-updated) status.
        let sleep_freeze_reason = simulator_helpers::get_pokemon_at_slot(&status_fail_state, action.user_slot)
            .and_then(|m| m.status.as_ref())
            .map(|st| match st {
                crate::state::dex_data::Status::Frozen(_) => CantReason::Freeze,
                crate::state::dex_data::Status::Sleep(_) => CantReason::Sleep,
                _ => CantReason::Other,
            })
            .unwrap_or(CantReason::Other);
        simulator_helpers::note_move_outcome(&mut status_fail_state, action.user_slot, simulator_helpers::MoveOutcome::Cant(sleep_freeze_reason));
        // Sleep/freeze disrupts a rampage lock.
        let rampage_lock_turns = attacker.volatiles.iter().find_map(|v| {
            if let crate::state::pokemon::VolatileStatusState::MoveStatus(VolatileStatus::LockedMove(_), t) = v { Some(*t) } else { None }
        });
        if let Some(turns) = rampage_lock_turns {
            let is_misty = matches!(status_fail_state.terrain, Some(crate::state::dex_data::Terrain::MistyTerrain));
            let confusion_added = if let Some(mon) = mon_at_slot_mut(&mut status_fail_state, action.user_slot) {
                disrupt_rampage_lock(mon, turns, is_misty)
            } else { false };
            simulator_helpers::emit(&mut status_fail_state, EventKind::VolatileEnd {
                target: action.user_slot,
                volatile: VolatileStatus::LockedMove(PokemonMove::Struggle),
            });
            if confusion_added {
                simulator_helpers::emit(&mut status_fail_state, EventKind::VolatileStart {
                    target: action.user_slot,
                    volatile: VolatileStatus::Confusion,
                });
            }
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
        // A move prevented by infatuation (Attract).
        simulator_helpers::note_move_outcome(&mut fail_state, action.user_slot, simulator_helpers::MoveOutcome::Cant(CantReason::Infatuation));
        // Infatuation disrupts a rampage lock.
        let rampage_lock_turns = attacker.volatiles.iter().find_map(|v| {
            if let crate::state::pokemon::VolatileStatusState::MoveStatus(VolatileStatus::LockedMove(_), t) = v { Some(*t) } else { None }
        });
        if let Some(turns) = rampage_lock_turns {
            let is_misty = matches!(fail_state.terrain, Some(crate::state::dex_data::Terrain::MistyTerrain));
            let confusion_added = if let Some(mon) = mon_at_slot_mut(&mut fail_state, action.user_slot) {
                disrupt_rampage_lock(mon, turns, is_misty)
            } else { false };
            simulator_helpers::emit(&mut fail_state, EventKind::VolatileEnd {
                target: action.user_slot,
                volatile: VolatileStatus::LockedMove(PokemonMove::Struggle),
            });
            if confusion_added {
                simulator_helpers::emit(&mut fail_state, EventKind::VolatileStart {
                    target: action.user_slot,
                    volatile: VolatileStatus::Confusion,
                });
            }
        }
        final_outcomes.push((MatchState::BattleState(fail_state), attract_fail_prob));
    }

    // Scale normal outcomes by success probability (1 - combined_fail_prob)
    let combined_fail_prob = par_fail_prob + status_fail_prob + attract_fail_prob;
    let success_scale = (1.0 - combined_fail_prob).max(0.0);

    // Gen VII+: confusion self-hit fires 1/3 of the success probability; normal move fires 2/3.
    if let Some(confusion_outcomes) = &confusion_self_hit_outcomes {
        for (state, prob) in confusion_outcomes {
            final_outcomes.push((state.clone(), prob * success_scale * (1.0 / 3.0)));
        }
    }

    let normal_scale = success_scale * if confusion_self_hit_outcomes.is_some() { 2.0 / 3.0 } else { 1.0 };
    for (state, prob) in all_outcomes {
        final_outcomes.push((state, prob * normal_scale));
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

/// Disrupt an in-progress rampage lock, clearing the volatile and applying confusion if this is
/// the guaranteed-final locked turn (per game quirk: disruption on the final turn still confuses).
///
/// `attacks_completed`: the count-up value on the `LockedMove` volatile — number of rampage
/// attacks already landed (the current disrupted attack is NOT counted). `attacks_completed >= 2`
/// means the disrupted attack would have been the guaranteed 3rd-and-final turn, so confusion
/// fires. `attacks_completed == 1` (disrupted on the 2nd turn) does NOT confuse; in-cartridge
/// this would be 50%-confuse but branching all five fail sites is out of scope.
/// Returns `(confusion_added)` so callers can emit `VolatileEnd { LockedMove }` and
/// optionally `VolatileStart { Confusion }` after the mutable borrow ends.
fn disrupt_rampage_lock(
    mon: &mut PokemonState,
    attacks_completed: u16,
    is_misty_terrain: bool,
) -> bool {
    // Read the locked move before removing it so we can skip confusion for rolling moves.
    let locked_is_rolling = mon.volatiles.iter().any(|v| matches!(
        v,
        crate::state::pokemon::VolatileStatusState::MoveStatus(
            VolatileStatus::LockedMove(m), _
        ) if matches!(m, PokemonMove::Rollout | PokemonMove::IceBall)
    ));
    simulator_helpers::remove_status_volatile(mon, &VolatileStatus::LockedMove(
        crate::data::pokemon_move::PokemonMove::Tackle // payload ignored by discriminant match
    ));
    // Only confuse when the disrupted attack would have been the guaranteed-final 3rd turn,
    // and only for Thrash-family moves (Rollout/Ice Ball never confuse).
    let is_final_turn = attacks_completed >= 2;
    let confusion_added = is_final_turn && !locked_is_rolling && !simulator_helpers::is_confused(mon) && !is_misty_terrain;
    if confusion_added {
        let duration = rand::thread_rng().gen_range(2u16..=5u16);
        mon.volatiles.push(crate::state::pokemon::VolatileStatusState::MoveStatus(
            VolatileStatus::Confusion,
            duration,
        ));
    }
    confusion_added
}

/// Return commands for a Pokémon locked into a rampaging move (Thrash/Outrage/Petal Dance/Raging Fury).
/// The user must re-select that move and cannot switch. Target is left as `None` so that the
/// RandomNormal fan-out in `possible_damage_outcomes_for_move` re-randomizes the foe each turn
/// (relevant in doubles; in singles the sole live foe is selected by the fallback path).
fn locked_rampage_commands(mon: &PokemonState, locked_move: &PokemonMove, player: Player, state: &BattleState, slot_idx: usize) -> Vec<BattleCommand> {
    let _ = (player, state, slot_idx); // these were only used for target resolution; now None
    // Validate: rampaging move must still be in the moveset.
    for (i, move_opt) in mon.moves.iter().enumerate() {
        if move_opt.as_ref() == Some(locked_move) {
            // No tera/mega variants while locked — the lock was established without them.
            // target: None → resolved by the RandomNormal fan-out (re-randomized each turn).
            return vec![BattleCommand::Attack(AttackCommand { move_slot: i, target: None, terastallize: false, mega_evolve: false })];
        }
    }
    // Shouldn't happen, but if the move somehow isn't in the moveset, pass.
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
    if mon.volatiles.iter().any(|v| matches!(v, crate::state::pokemon::VolatileStatusState::TurnStatus(VolatileStatus::MustRecharge, _))) {
        return vec![BattleCommand::Pass];
    }

    // Locked: semi-invulnerable (e.g. mid-Fly)
    if mon.volatiles.iter().any(|v| matches!(v, crate::state::pokemon::VolatileStatusState::MoveStatus(VolatileStatus::SemiInvulnerable(_), _))) {
        return locked_semi_invulnerable_commands(mon, player);
    }

    // Locked: charging (e.g. mid-SolarBeam)
    if let Some((charged_move, charged_targets)) = mon.volatiles.iter().find_map(|v| {
        if let crate::state::pokemon::VolatileStatusState::Charging(mov, targets) = v { Some((mov.clone(), targets.clone())) } else { None }
    }) {
        return locked_charging_commands(mon, &charged_move, &charged_targets);
    }

    // Locked: rampaging (Thrash / Outrage / Petal Dance / Raging Fury).
    // Cannot switch or use a different move while the LockedMove volatile is present.
    if let Some(locked_move) = mon.volatiles.iter().find_map(|v| {
        if let crate::state::pokemon::VolatileStatusState::MoveStatus(VolatileStatus::LockedMove(m), _) = v { Some(m.clone()) } else { None }
    }) {
        return locked_rampage_commands(mon, &locked_move, player, state, slot_idx);
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
        if let crate::state::pokemon::VolatileStatusState::TurnStatus(VolatileStatus::ChoiceLock(m), _) = v {
            Some(m.clone())
        } else {
            None
        }
    });

    // Move-restriction volatiles. Encore forces a single move (unless that move has run out of PP,
    // in which case Encore has effectively ended); Taunt blocks status moves; Throat Chop blocks
    // sound moves; Torment blocks repeating the last move used.
    let encored_move: Option<PokemonMove> = mon.volatiles.iter().find_map(|v| {
        if let crate::state::pokemon::VolatileStatusState::MoveStatus(VolatileStatus::Encore(m), _) = v {
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
    // Imprison: collect moves that are blocked by an opponent's Imprison.
    // A move is imprisoned if any active opposing mon carries the Imprison volatile AND knows it.
    let opponent_player = match player { Player::P1 => Player::P2, Player::P2 => Player::P1 };
    let imprison_blocked_moves: std::collections::HashSet<PokemonMove> = {
        let opp_mons = match opponent_player {
            Player::P1 => &state.p1_active_mons,
            Player::P2 => &state.p2_active_mons,
        };
        opp_mons.iter()
            .filter(|m| !m.fainted && simulator_helpers::has_status_volatile(m, &VolatileStatus::Imprison))
            .flat_map(|m| m.moves.iter().filter_map(|s| s.clone()))
            .collect()
    };

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
            crate::state::pokemon::VolatileStatusState::MoveStatus(VolatileStatus::Disable(m), _) if m == move_name
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
            && move_dex.get(move_name).map_or(false, |d| simulator_helpers::move_has_flag(d, &crate::state::dex_data::MoveFlag::Sound))
        {
            continue;
        }

        // Heal Block: moves with the Heal flag cannot be selected.
        if simulator_helpers::heal_is_blocked(mon)
            && move_dex.get(move_name).map_or(false, |d| simulator_helpers::move_has_flag(d, &crate::state::dex_data::MoveFlag::Heal))
        {
            continue;
        }

        // Torment: the same move cannot be used twice in a row.
        if tormented && mon.last_used_move.as_ref() == Some(move_name) { continue; }

        // Belch: cannot be selected unless the user has eaten a Berry this battle.
        if *move_name == PokemonMove::Belch && !mon.ate_berry_this_battle { continue; }

        // CantUseRepeatedly volatile (e.g. Gigaton Hammer): the named move cannot be selected
        // on consecutive turns. Cleared by switch-out (volatile wipe) or after 2 turns.
        let cant_repeat = mon.volatiles.iter().any(|v| matches!(
            v,
            crate::state::pokemon::VolatileStatusState::MoveStatus(VolatileStatus::CantUseRepeatedly(m), _) if m == move_name
        ));
        if cant_repeat { continue; }

        // Imprison: moves shared with an imprisoning opponent cannot be selected.
        if imprison_blocked_moves.contains(move_name) { continue; }

        // Gravity: moves with the Gravity flag cannot be selected while Gravity is active.
        if simulator_helpers::is_gravity_active(state)
            && move_dex.get(move_name).map_or(false, |d| simulator_helpers::move_has_flag(d, &crate::state::dex_data::MoveFlag::Gravity))
        {
            continue;
        }

        let target_type = move_dex.get(move_name).map(|d| &d.target).unwrap_or(&MoveTarget::Normal);

        let valid_targets = if *move_name == PokemonMove::ExpandingForce
            && simulator_helpers::pokemon_is_on_terrain(state, mon, &crate::state::dex_data::Terrain::PsychicTerrain)
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
                    moves_last: false,
                }));
            }
            BattleCommand::Struggle { target } => {
                action_queue.push(Action::MoveAction(MoveAction {
                    move_name: PokemonMove::Struggle,
                    priority: 0,
                    user_slot,
                    target_slot: *target,
                    moves_first: false,
                    moves_last: false,
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
            matches!(v, crate::state::pokemon::VolatileStatusState::TurnStatus(VolatileStatus::Substitute(_), _))
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
/// (unsuppressed) and Ingrain prevent it.
/// `attacker_breaks` should be `attacker_breaks_mold(bs, attacker)` — Mold Breaker bypasses
/// Suction Cups and Guard Dog (both are ignorable abilities) but NOT Ingrain.
fn can_be_forced_out(bs: &BattleState, slot: FieldSlot, attacker_breaks: bool) -> bool {
    let Some(mon) = simulator_helpers::get_pokemon_at_slot(bs, slot) else { return false; };
    if !attacker_breaks
        && !simulator_helpers::pokemon_ability_is_suppressed(bs, mon)
        && matches!(mon.ability, Ability::SuctionCups | Ability::GuardDog)
    {
        return false;
    }
    mon.volatiles.iter().all(|v| !matches!(v,
        crate::state::pokemon::VolatileStatusState::TurnStatus(VolatileStatus::Ingrain, _)
            | crate::state::pokemon::VolatileStatusState::MoveStatus(VolatileStatus::Ingrain, _)))
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
    attacker_slot: FieldSlot,
    target_slot: FieldSlot,
    move_data: &MoveData,
    pokemon_dex: &HashMap<Species, PokemonData>,
    move_dex: &HashMap<PokemonMove, MoveData>,
) -> Vec<(BattleState, f64)> {
    let bypasses_sub = simulator_helpers::move_has_flag(move_data, &crate::state::dex_data::MoveFlag::BypassSub);
    let mut out = Vec::new();
    for (bs, prob) in branches {
        // Check Mold Breaker at resolution time (ability could have changed via Mummy etc.)
        let attacker_breaks = simulator_helpers::get_pokemon_at_slot(&bs, attacker_slot)
            .map_or(false, |a| simulator_helpers::attacker_breaks_mold(&bs, a));
        let target_fainted = simulator_helpers::get_pokemon_at_slot(&bs, target_slot)
            .map_or(true, |m| m.fainted);
        let switches = !target_fainted
            && can_be_forced_out(&bs, target_slot, attacker_breaks)
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
            simulator_helpers::process_pokemon_send_out(&mut clone, target_slot, move_dex);
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
) -> Vec<(MatchState, f64)> {
    let total_dmg = total_damage_to_opponent(baseline, &bs, opposing_player);
    // sub_damage_dealt tracks the full damage roll sent into a Substitute this action.
    // Recoil is based on all damage dealt (to HP or sub); drain and Shell Bell are not.
    let sub_dmg = bs.sub_damage_dealt;
    bs.sub_damage_dealt = 0; // consumed here
    let total_effective_dmg = total_dmg + sub_dmg; // for recoil / last_move_failed / move_connected
    let opponent_wiped = !simulator_helpers::team_has_remaining_pokemon(&bs, opposing_player) && total_dmg > 0;
    // Collect (slot, post-hp, max-hp) tuples for Healed events; emitted after the attacker_mon
    // borrow ends (iter_mut borrow pattern — NLL prevents inline emit inside the block).
    let mut attacker_healed: Vec<(FieldSlot, u16, u16)> = Vec::new();
    let attacker_item_active = simulator_helpers::get_pokemon_at_slot(&bs, attacker_slot)
        .map(|m| simulator_helpers::item_is_active(&bs, m))
        .unwrap_or(false);
    let attacker_env = simulator_helpers::berry_env(&bs, attacker_slot);
    let items_suppressed = simulator_helpers::items_are_suppressed(&bs);
    let abilities_suppressed = simulator_helpers::abilities_are_suppressed(&bs);
    // Capture whether any active opposing mon carries Liquid Ooze before the attacker borrow.
    // If so, drain heals are reversed into damage on the attacker (mirrors Strength Sap and
    // Leech Seed). Checked before the mutable borrow of attacker_mon to avoid borrow conflicts.
    let drain_target_has_liquid_ooze = !abilities_suppressed && {
        let opp_mons = match opposing_player {
            Player::P1 => &bs.p1_active_mons,
            Player::P2 => &bs.p2_active_mons,
        };
        opp_mons.iter().any(|m| !m.fainted && m.ability == Ability::LiquidOoze)
    };
    let mut forced_winner: Option<Player> = None;
    let mut attacker_fainted = false;
    // (cur, end_prob, disrupt_now, can_confuse) — set inside the attacker_mon block below.
    let mut rampage_decision: Option<(u16, f64, bool, bool)> = None;
    // Collected inside the attacker_mon borrow block; emitted after the borrow ends.
    let mut spit_up_delta: [i8; 7] = [0i8; 7];

    if let Some(attacker_mon) = mon_at_slot_mut(&mut bs, attacker_slot) {
        // Stomping Tantrum / Temper Flare / Micle Berry bookkeeping: a damaging move
        // that dealt no damage to any target this action (missed every target, or no
        // effect) counts as the last move failing; dealing damage clears the flag.
        // Status moves keep their explicit fail paths (e.g. failed Aura Wheel).
        if !matches!(move_data.category, MoveCategory::Status) {
            attacker_mon.last_move_failed = total_effective_dmg == 0;
            // Metronome item: a damaging move that dealt no damage (missed, protected,
            // immune) breaks the consecutive streak. Reset last_used_move to None so the
            // next pre-update (in possible_damage_outcomes_for_move) treats the next use
            // as a fresh start, even if the same move is chosen again.
            // Note: this is a Metronome-tracking null; Disable is triggered by the damage
            // path before this point, so this doesn't affect Disable on zero-damage hits.
            if total_effective_dmg == 0 {
                attacker_mon.consecutive_move_count = 0;
                attacker_mon.last_used_move = None;
            }
        }

        let max_hp = attacker_mon.stats[0].max(1);

        // Heal Block prevents any HP recovery from moves, draining moves and Shell Bell
        // (the move still works otherwise; recoil/damage are unaffected).
        let heal_blocked = simulator_helpers::heal_is_blocked(attacker_mon);

        // Unconditional self-heal
        if !heal_blocked && move_data.heal_fraction[0] > 0 && move_data.heal_fraction[1] > 0 {
            let heal = ((max_hp as u32 * move_data.heal_fraction[0] as u32) / move_data.heal_fraction[1] as u32) as u16;
            if heal > 0 {
                let before = attacker_mon.hp;
                simulator_helpers::gain_hp(attacker_mon, heal, attacker_env);
                if attacker_mon.hp != before {
                    let (hp, mx) = (attacker_mon.hp, max_hp);
                    attacker_healed.push((attacker_slot, hp, mx));
                }
            }
        }

        // Drain heal (e.g. Drain Punch, Giga Drain, Dream Eater, Leech Life).
        // Liquid Ooze on the target reverses the heal into damage on the user —
        // this mirrors the Strength Sap and Leech Seed implementations.
        if move_data.drain_fraction[0] > 0 && move_data.drain_fraction[1] > 0 {
            let heal = ((total_dmg * move_data.drain_fraction[0] as u32) / move_data.drain_fraction[1] as u32) as u16;
            if heal > 0 {
                if drain_target_has_liquid_ooze {
                    simulator_helpers::take_damage(attacker_mon, heal, attacker_env, abilities_suppressed);
                    if attacker_mon.fainted {
                        simulator_helpers::clear_pokemon_on_faint(attacker_mon);
                        attacker_fainted = true;
                        if opponent_wiped { forced_winner = Some(attacker_slot.player); }
                    }
                } else if !heal_blocked {
                    let before = attacker_mon.hp;
                    simulator_helpers::gain_hp(attacker_mon, heal, attacker_env);
                    if attacker_mon.hp != before {
                        let (hp, mx) = (attacker_mon.hp, max_hp);
                        attacker_healed.push((attacker_slot, hp, mx));
                    }
                }
            }
        }

        // Shell Bell: restore 1/8 of damage dealt (rounded down) to the attacker.
        // Does not consume the item. Based on damage dealt, not HP lost by target.
        // Sheer Force suppresses Shell Bell on a boosted move (same negated set as Life Orb recoil).
        let shell_bell_sheer_force = !abilities_suppressed
            && attacker_mon.ability == Ability::SheerForce
            && simulator_helpers::move_has_sheer_force_secondary(move_data);
        if !heal_blocked && !shell_bell_sheer_force && attacker_item_active
            && attacker_mon.item == crate::data::item::Item::ShellBell
        {
            let heal = (total_dmg / 8) as u16;
            if heal > 0 {
                let before = attacker_mon.hp;
                simulator_helpers::gain_hp(attacker_mon, heal, attacker_env);
                if attacker_mon.hp != before {
                    let (hp, mx) = (attacker_mon.hp, max_hp);
                    attacker_healed.push((attacker_slot, hp, mx));
                }
            }
        }

        // Recoil
        // Struggle recoil (¼ max HP) ignores Rock Head and Magic Guard; ordinary recoil does not.
        // Mind Blown / Steel Beam recoil (½ max HP, rounded up) fires unconditionally — even on
        // miss, Protect, or Substitute. Only Magic Guard prevents it; Rock Head does NOT.
        let has_normal_recoil = move_data.recoil_fraction[0] > 0 && move_data.recoil_fraction[1] > 0;
        let has_recoil = has_normal_recoil || move_data.struggle_recoil || move_data.mind_blown_recoil;
        let ability_blocks_recoil = if move_data.mind_blown_recoil {
            attacker_mon.ability == Ability::MagicGuard
        } else if move_data.struggle_recoil {
            false
        } else {
            attacker_mon.ability == Ability::RockHead || attacker_mon.ability == Ability::MagicGuard
        };
        // mind_blown_recoil fires even when total_dmg == 0 (miss / Protect / Substitute).
        // We apply it outside the `recoil > 0` gate below by computing it separately.
        if move_data.mind_blown_recoil && !ability_blocks_recoil && !attacker_fainted {
            let recoil = ((max_hp as u32 + 1) / 2) as u16; // ceil(max_hp / 2)
            if recoil > 0 {
                simulator_helpers::take_damage(attacker_mon, recoil, attacker_env, abilities_suppressed);
                if attacker_mon.fainted {
                    simulator_helpers::clear_pokemon_on_faint(attacker_mon);
                    attacker_fainted = true;
                    if opponent_wiped { forced_winner = Some(attacker_slot.player); }
                }
            }
        }
        if has_recoil && !ability_blocks_recoil && !move_data.mind_blown_recoil {
            let recoil = if has_normal_recoil {
                // Recoil is based on total damage dealt, including damage absorbed by a Substitute.
                ((total_effective_dmg * move_data.recoil_fraction[0] as u32) / move_data.recoil_fraction[1] as u32) as u16
            } else if move_data.struggle_recoil {
                (max_hp as u32 / 4) as u16
            } else { 0 };

            if recoil > 0 {
                simulator_helpers::take_damage(attacker_mon, recoil, attacker_env, abilities_suppressed);
                if attacker_mon.fainted {
                    simulator_helpers::clear_pokemon_on_faint(attacker_mon);
                    attacker_fainted = true;
                    if opponent_wiped { forced_winner = Some(attacker_slot.player); }
                }
            }
        }

        // Life Orb: the holder loses 1/10 of its max HP (min 1) after using a damaging move
        // that actually dealt damage.  Not triggered by status moves, nor if no damage was
        // dealt (miss / Protect / full immunity).  Magic Guard blocks the recoil entirely.
        // Sheer Force + eligible moves suppress Life Orb recoil (the BP boost cancels it).
        if !matches!(move_data.category, MoveCategory::Status)
            && total_effective_dmg > 0
            && !attacker_fainted
            && attacker_item_active
            && attacker_mon.item == crate::data::item::Item::LifeOrb
        {
            let sheer_force_suppresses = !abilities_suppressed
                && attacker_mon.ability == Ability::SheerForce
                && simulator_helpers::move_has_sheer_force_secondary(move_data);
            let magic_guard_blocks = !abilities_suppressed
                && attacker_mon.ability == Ability::MagicGuard;
            if !sheer_force_suppresses && !magic_guard_blocks {
                let recoil = (max_hp as u32 / 10).max(1) as u16;
                simulator_helpers::take_damage(attacker_mon, recoil, attacker_env, abilities_suppressed);
                if attacker_mon.fainted {
                    simulator_helpers::clear_pokemon_on_faint(attacker_mon);
                    attacker_fainted = true;
                    if opponent_wiped { forced_winner = Some(attacker_slot.player); }
                }
            }
        }

        // Spit Up: using the move consumes the Stockpile charge and removes the Def/Sp. Def
        // boosts it granted (the base power was already read from the level during damage calc).
        if move_data.name == PokemonMove::SpitUp {
            let level = simulator_helpers::stockpile_level(attacker_mon) as i8;
            if level > 0 {
                simulator_helpers::remove_status_volatile(attacker_mon, &VolatileStatus::Stockpile(0));
                let drop = -level;
                spit_up_delta = simulator_helpers::apply_stat_boost_external(attacker_mon, &[0, drop, 0, drop, 0, 0, 0], items_suppressed);
            }
        }

        // Rampaging moves (Thrash / Outrage / Petal Dance / Raging Fury):
        //
        //  The LockedMove volatile carries a COUNT-UP counter `n` = attacks completed
        //  so far (0 = no lock yet, not present):
        //
        //   cur = n_before + 1  (attacks including the current one)
        //   cur == 1 (first):   lock starts; no end branching
        //   cur == 2 (second):  50% end (remove lock + confuse), 50% continue
        //   cur >= 3 (third+):  always end (remove lock + confuse)
        //
        //  This produces a real 50/50 probability branch instead of a sampled
        //  gen_range(2..=3), matching how sleep forks the outcome tree.
        //
        //  Decision parameters are collected here so the fork can happen AFTER all
        //  other post-damage effects (self-destruct, Magician, etc.) have mutated `bs`.
        //  The actual forking happens at the bottom of this function.
        //
        //  Disruption (total_dmg == 0: miss, immune, Protect, flinch, etc.) ends the
        //  rampage immediately without confusion — UNLESS this was the guaranteed-final
        //  3rd turn (cur >= 3), in which case still confuse per game quirk.
        //  A disruption on the 2nd turn does NOT confuse (conservative approximation;
        //  the in-cartridge behaviour would be 50%-confuse but forking all five
        //  fail-branch sites is out of scope).
        let is_rampaging_move = matches!(move_data.name,
            PokemonMove::Thrash | PokemonMove::Outrage | PokemonMove::PetalDance | PokemonMove::RagingFury
        );
        // Rolling moves (Rollout/Ice Ball): same LockedMove volatile as rampaging moves but with
        // deterministic 5-turn duration (never end early except on disruption) and no confusion.
        let is_rolling_move = matches!(move_data.name,
            PokemonMove::Rollout | PokemonMove::IceBall
        );
        let is_locking_move = is_rampaging_move || is_rolling_move;

        // rampage_decision: Some((cur, end_prob, disrupt_now, can_confuse))
        //   cur          — attacks completed including this one
        //   end_prob     — probability this is the final rampage/rolling attack
        //   disrupt_now  — move failed (total_dmg==0); end without confusion unless
        //                  end_prob==1.0 (final turn quirk)
        //   can_confuse  — false for rolling moves; !is_confused && !misty_terrain for Thrash-family
        rampage_decision = if is_locking_move && !attacker_fainted {
            let n_before: Option<u16> = attacker_mon.volatiles.iter().find_map(|v| {
                if let crate::state::pokemon::VolatileStatusState::MoveStatus(VolatileStatus::LockedMove(_), t) = v {
                    Some(*t)
                } else {
                    None
                }
            });
            let cur = n_before.unwrap_or(0) + 1;
            let disrupt_now = total_dmg == 0;
            // Rolling moves never confuse on end. Thrash-family confuses unless already confused
            // or the field is misty.
            let can_confuse = !is_rolling_move
                && !simulator_helpers::is_confused(attacker_mon)
                && !attacker_env.misty_terrain;

            if cur == 1 {
                // First use: set lock to n=1 only if move connected.
                if total_dmg > 0 {
                    attacker_mon.volatiles.push(crate::state::pokemon::VolatileStatusState::MoveStatus(
                        VolatileStatus::LockedMove(move_data.name.clone()),
                        1,
                    ));
                }
                None // no end-timing branch on the first attack
            } else {
                // Already locked: remove the existing lock entry now; the fork below
                // will re-add it on the continue branch.
                simulator_helpers::remove_status_volatile(attacker_mon, &VolatileStatus::LockedMove(move_data.name.clone()));
                let end_prob = if is_rolling_move {
                    // Rolling: deterministic 5-turn sequence — always end at turn 5 (cur==5),
                    // never branch mid-run (unless disrupted via disrupt_now).
                    if cur >= 5 { 1.0 } else { 0.0 }
                } else {
                    // Thrash-family: 50% end at turn 2; guaranteed end at turn 3+.
                    if cur >= 3 { 1.0 } else { 0.5 }
                };
                Some((cur, end_prob, disrupt_now, can_confuse))
            }
        } else {
            None
        };

        // Self-destruct moves: faint the user after damage is dealt.
        //
        //  "always" (Explosion, Self-Destruct, Misty Explosion): user always faints,
        //   even if the move missed or the target was behind Protect. Damp already
        //   prevented reaching this function when Damp is on the field.
        //
        //  "ifHit" (Final Gambit): user faints only when the move actually dealt damage
        //   (total_dmg > 0). Ghost-immune and missed branches produce total_dmg == 0
        //   and correctly skip the faint.
        //
        //  Memento and Healing Wish carry SelfDestructType::IfHit in the data but are
        //  Status category moves: they are fully handled as hand-coded blocks in
        //  execute_move and never reach this function.
        //
        //  Sturdy / Focus Sash / Focus Band do NOT protect the self-fainting user.
        if !attacker_fainted {
            let should_faint = match move_data.self_destruct {
                SelfDestructType::Always => true,
                SelfDestructType::IfHit  => total_dmg > 0,
                SelfDestructType::None   => false,
            };
            if should_faint {
                attacker_mon.hp = 0;
                attacker_mon.fainted = true;
                simulator_helpers::clear_pokemon_on_faint(attacker_mon);
                attacker_fainted = true;
                if opponent_wiped { forced_winner = Some(attacker_slot.player); }
            }
        }
    }

    if attacker_fainted {
        simulator_helpers::handle_pokemon_faint(&mut bs, attacker_slot.player, attacker_slot.slot_index);
    }
    // Emit Healed events for drain/Shell Bell/unconditional heals (collected while attacker_mon
    // borrow was live; now emitted after it is released).
    simulator_helpers::emit_healed_batch(&mut bs, &attacker_healed);
    // Emit BoostChanged for Spit Up's Stockpile discharge (also collected while borrow was live).
    for (boost_idx, &stages) in spit_up_delta.iter().enumerate() {
        if stages != 0 { simulator_helpers::emit(&mut bs, EventKind::BoostChanged { target: attacker_slot, boost_idx, stages }); }
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

    // Item-manipulation damaging moves apply their item effect after damage, on the foe
    // they hit. Detection mirrors Magician: a foe whose HP dropped vs the pre-move
    // baseline was hit directly (a Substitute that absorbed the hit leaves HP unchanged,
    // so the item effect is correctly skipped). Sticky Hold and untransferable items are
    // enforced inside the helpers.
    if !attacker_fainted
        && total_dmg > 0
        && matches!(
            move_data.name,
            PokemonMove::KnockOff
                | PokemonMove::Thief
                | PokemonMove::Covet
                | PokemonMove::BugBite
                | PokemonMove::Pluck
        )
    {
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
            if !damaged {
                continue;
            }
            match move_data.name {
                PokemonMove::KnockOff => {
                    simulator_helpers::try_remove_item(&mut bs, slot);
                }
                PokemonMove::Thief | PokemonMove::Covet => {
                    simulator_helpers::try_steal_item(&mut bs, attacker_slot, slot);
                }
                PokemonMove::BugBite | PokemonMove::Pluck => {
                    simulator_helpers::try_eat_targets_berry(&mut bs, attacker_slot, slot);
                }
                _ => {}
            }
            break; // single-target moves
        }
    }

    // Fling: the thrown item is consumed whenever the move is used (even on a miss), and
    // its item-dependent added effect lands on the foe it hit.
    if !attacker_fainted && move_data.name == PokemonMove::Fling {
        let item = simulator_helpers::get_pokemon_at_slot(&bs, attacker_slot)
            .map(|m| m.item.clone())
            .unwrap_or(Item::None);
        if item != Item::None {
            if let Some(user) = mon_at_slot_mut(&mut bs, attacker_slot) {
                user.consumed_item = Some(item.clone());
                user.item = Item::None;
                user.item_lost = true;
            }
            // item_lost = true bypasses the snapshot; emit directly.
            simulator_helpers::emit(&mut bs, EventKind::ItemLost {
                slot: attacker_slot,
                item: item.clone(),
                consumed: true,
            });
            if total_dmg > 0 {
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
                    if damaged {
                        simulator_helpers::apply_fling_effect(&mut bs, attacker_slot, slot, &item);
                        break;
                    }
                }
            }
        }
    }

    // Beak Blast: remove the charging volatile after the move resolves so that
    // subsequent contacts this same turn (doubles) don't still trigger burns.
    if move_data.name == PokemonMove::BeakBlast {
        if let Some(user) = mon_at_slot_mut(&mut bs, attacker_slot) {
            simulator_helpers::remove_status_volatile(user, &VolatileStatus::BeakBlastCharging);
        }
        simulator_helpers::emit(&mut bs, EventKind::VolatileEnd {
            target: attacker_slot,
            volatile: VolatileStatus::BeakBlastCharging,
        });
    }

    let mut terminal = if let Some(winner) = forced_winner {
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
            let move_connected = total_effective_dmg > 0 || matches!(move_data.category, MoveCategory::Status);
            // For ShedTail: success ⇔ attacker now has a Substitute AND did not have one before
            // the move (baseline). Using baseline comparison instead of an HP check means items
            // like Sitrus Berry healing after the HP cost cannot mask a successful switch.
            let shed_tail_sub_created = move_data.self_switch != SelfSwitchType::ShedTail
                || (slot_has_substitute(&bs, attacker_slot) && !slot_has_substitute(baseline, attacker_slot));
            if attacker_alive && move_connected && shed_tail_sub_created {
                bs.self_switch_pending = Some((attacker_slot, move_data.self_switch));
            }
        }
        // Round: once this Round's damage is applied, mark the flag so subsequent Rounds
        // in the same turn compute doubled BP.
        if move_data.name == PokemonMove::Round {
            bs.round_used_this_turn = true;
        }
        MatchState::BattleState(bs)
    };

    // Rampage end-timing fork: split into continue vs end branches.
    // The lock volatile was already removed from `attacker_mon` above (for locked turns);
    // we write it back on the continue branch or leave it absent on the end branch.
    match rampage_decision {
        None => vec![(terminal, 1.0)],
        Some((cur, end_prob, disrupt_now, can_confuse)) => {
            let mut branches: Vec<(MatchState, f64)> = Vec::new();

            // End branch: remove lock (already done above), optionally confuse.
            // A disruption mid-rampage (not final turn: end_prob < 1.0) skips confusion.
            // A disruption on the final turn (end_prob == 1.0) still confuses (game quirk).
            let should_confuse = can_confuse && (!disrupt_now || end_prob >= 1.0);
            if end_prob > 0.0 {
                let mut end_state = terminal.clone();
                if should_confuse {
                    if let MatchState::BattleState(ref mut end_bs) = end_state {
                        if let Some(mon) = mon_at_slot_mut(end_bs, attacker_slot) {
                            let duration = rand::thread_rng().gen_range(2u16..=5u16);
                            mon.volatiles.push(crate::state::pokemon::VolatileStatusState::MoveStatus(
                                VolatileStatus::Confusion,
                                duration,
                            ));
                        }
                        // Emit VolatileEnd for LockedMove + VolatileStart for Confusion.
                        simulator_helpers::emit(end_bs, EventKind::VolatileEnd {
                            target: attacker_slot,
                            volatile: VolatileStatus::LockedMove(move_data.name.clone()),
                        });
                        simulator_helpers::emit(end_bs, EventKind::VolatileStart {
                            target: attacker_slot,
                            volatile: VolatileStatus::Confusion,
                        });
                    }
                } else if let MatchState::BattleState(ref mut end_bs) = end_state {
                    // Lock ended without confusion.
                    simulator_helpers::emit(end_bs, EventKind::VolatileEnd {
                        target: attacker_slot,
                        volatile: VolatileStatus::LockedMove(move_data.name.clone()),
                    });
                }
                branches.push((end_state, end_prob));
            }

            // Continue branch: re-add the lock volatile at `cur` (count-up).
            // Only emitted when end_prob < 1.0 and the rampage wasn't disrupted.
            let continue_prob = 1.0 - end_prob;
            if continue_prob > 0.0 && !disrupt_now {
                let mut cont_state = terminal.clone();
                if let MatchState::BattleState(ref mut cont_bs) = cont_state {
                    if let Some(mon) = mon_at_slot_mut(cont_bs, attacker_slot) {
                        mon.volatiles.push(crate::state::pokemon::VolatileStatusState::MoveStatus(
                            VolatileStatus::LockedMove(move_data.name.clone()),
                            cur,
                        ));
                    }
                    // No VolatileEnd: the lock continues — the old entry was removed and re-added
                    // as an internal tick; from the observer's perspective the lock is still active.
                }
                branches.push((cont_state, continue_prob));
            }

            if branches.is_empty() {
                // Disrupted mid-rampage (end_prob < 1.0, disrupt_now) — lock cleared, no confuse.
                if let MatchState::BattleState(ref mut term_bs) = terminal {
                    simulator_helpers::emit(term_bs, EventKind::VolatileEnd {
                        target: attacker_slot,
                        volatile: VolatileStatus::LockedMove(move_data.name.clone()),
                    });
                }
                vec![(terminal, 1.0)]
            } else {
                branches
            }
        }
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
                    // Record where this move's sub-events begin in the pending stream so
                    // the MoveUsed wrapper can adopt them as `reactions` via split_off.
                    let move_start_len = state.pending_events.len();
                    let move_user = m.user_slot;
                    let move_name = m.move_name.clone();
                    let move_targets: Vec<FieldSlot> = m.target_slot.map_or_else(Vec::new, |s| vec![s]);
                    possible_damage_outcomes_for_move(&state, &m, move_data, config, move_dex, pokemon_dex, false)
                        .into_iter()
                        .map(|(mut st, p)| {
                            if let MatchState::BattleState(ref mut bs) = st {
                                simulator_helpers::process_item_loss_events(bs, &item_snapshot);
                                // Weather / ability-altering moves may change Castform's form.
                                simulator_helpers::update_forecast_forms(bs);
                                // Wrap all events emitted during this move as reactions of MoveUsed,
                                // UNLESS the Pokémon couldn't act (Cant) — in that case Showdown
                                // emits `|cant|` with no `|move|`, so we emit the children flat.
                                if bs.event_observer.is_some() {
                                    let reactions = bs.pending_events.split_off(move_start_len);
                                    if bs.move_was_prevented {
                                        // Cant branch: the Cant event is already inside `reactions`;
                                        // push it back flat so it appears as a top-level sibling.
                                        bs.pending_events.extend(reactions);
                                    } else {
                                        bs.pending_events.push(InformationEvent {
                                            kind: EventKind::MoveUsed {
                                                user: move_user,
                                                move_used: move_name.clone(),
                                                targets: move_targets.clone(),
                                            },
                                            reactions,
                                        });
                                    }
                                }
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
            simulator_helpers::process_pokemon_send_out(&mut state, s.user_slot, move_dex);
            if simulator_helpers::get_verbosity() >= 2 {
                let user = simulator_helpers::get_pokemon_at_slot(&state, s.user_slot)
                    .map(|p| simulator_helpers::species_name_sim(&p.species))
                    .unwrap_or_else(|| format!("{:?} slot {}", s.user_slot.player, s.user_slot.slot_index + 1));
                println!("{}", format!("Executed Switch: new active at slot {} is {}", s.user_slot.slot_index + 1, user).bright_green());
            }
            // Emit Switch event.
            if let Some(observer) = state.event_observer {
                if let Some(mon) = simulator_helpers::get_pokemon_at_slot(&state, s.user_slot) {
                    let hp = if s.user_slot.player == observer {
                        PokemonHP::Number(mon.hp)
                    } else {
                        PokemonHP::Percent(simulator_helpers::hp_to_percent(mon.hp, mon.stats[0]))
                    };
                    let switch_state = InfoSwitchState {
                        slot: s.user_slot,
                        species: simulator_helpers::observed_species(mon, s.user_slot, observer),
                        level: mon.level,
                        hp,
                        status: mon.status.clone(),
                        tera_type: Some(mon.tera_type.clone()),
                    };
                    state.pending_events.push(InformationEvent {
                        kind: EventKind::Switch(switch_state),
                        reactions: vec![],
                    });
                }
            }
            vec![(MatchState::BattleState(state), 1.0)]
        }
        Action::MegaAction(m) => {
            let slot_idx = m.user_slot.slot_index as usize;
            let mons = match m.user_slot.player { Player::P1 => &mut state.p1_active_mons, Player::P2 => &mut state.p2_active_mons };
            let evolved = mons.get_mut(slot_idx).map(|mon| crate::state::battle::try_mega_evolution(mon, pokemon_dex)).unwrap_or(false);
            match m.user_slot.player { Player::P1 => state.p1_has_mega = false, Player::P2 => state.p2_has_mega = false }
            if evolved {
                // The mega form may have a different ability; trigger its on-gain effects
                // (weather/terrain setters, Intimidate) the same way a Pokémon gaining an
                // ability mid-battle does.
                simulator_helpers::process_pokemon_gain_ability(&mut state, m.user_slot);
                // Emit MegaEvolution event, then reveal the new ability if it changed.
                if let Some(mega_mon) = simulator_helpers::get_pokemon_at_slot(&state, m.user_slot) {
                    let mega_species = mega_mon.species.clone();
                    let mega_ability = mega_mon.ability.clone();
                    let had_mega_ability = mega_mon.mega_ability.is_some();
                    simulator_helpers::emit(&mut state, EventKind::MegaEvolution { slot: m.user_slot, into: mega_species });
                    if had_mega_ability {
                        simulator_helpers::emit(&mut state, EventKind::AbilityRevealed { slot: m.user_slot, ability: mega_ability });
                    }
                }
            }
            vec![(MatchState::BattleState(state), 1.0)]
        }
        Action::TeraAction(t) => {
            let slot_idx = t.user_slot.slot_index as usize;
            let mons = match t.user_slot.player { Player::P1 => &mut state.p1_active_mons, Player::P2 => &mut state.p2_active_mons };
            if let Some(mon) = mons.get_mut(slot_idx) { mon.is_tera = true; }
            match t.user_slot.player { Player::P1 => state.p1_has_tera = false, Player::P2 => state.p2_has_tera = false }
            // Emit Terastallization event.
            if let Some(mon) = simulator_helpers::get_pokemon_at_slot(&state, t.user_slot) {
                let slot = t.user_slot;
                let tera_type = mon.tera_type.clone();
                simulator_helpers::emit(&mut state, EventKind::Terastallization { slot, tera_type });
            }
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
        // Record where this end-of-turn's sub-events begin so EndOfTurn can adopt them.
        let eot_start_len = next_state.pending_events.len();
        let eot_branches = simulator_helpers::end_turn(&mut next_state, move_dex, config);
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
                // Wrap all end-of-turn events as reactions of the EndOfTurn node.
                if bs.event_observer.is_some() {
                    let reactions = bs.pending_events.split_off(eot_start_len);
                    bs.pending_events.push(InformationEvent {
                        kind: EventKind::EndOfTurn,
                        reactions,
                    });
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
    observer: Option<Player>,
) -> Vec<(MatchState, Option<Vec<InformationEvent>>, f64)> {
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

    // Beak Blast: before any action resolves, give the user a BeakBlastCharging volatile.
    // Any contact move hitting them this turn will burn the attacker (see apply_contact_hit_reactions).
    let initial_branches: Vec<(MatchState, f64)> = initial_branches
        .into_iter()
        .map(|(mut st, prob)| {
            if let MatchState::BattleState(ref mut bs) = st {
                apply_priority_charge_volatiles(bs);
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

    // Set event_observer on each initial branch so all clones during expansion carry it.
    let initial_branches: Vec<(MatchState, f64)> = initial_branches
        .into_iter()
        .map(|(mut st, prob)| {
            if let MatchState::BattleState(ref mut bs) = st {
                bs.event_observer = observer;
            }
            (st, prob)
        })
        .collect();

    let all_results: Vec<(MatchState, f64)> = initial_branches
        .into_iter()
        .flat_map(|(init_state, init_prob)| {
            expand_branch(&init_state, move_dex, pokemon_dex, config)
                .into_iter()
                .map(move |(st, p)| (st, p * init_prob))
        })
        .collect();

    // Internal coalesce uses MatchState (excludes pending_events) — same as before.
    let all_results = simulator_helpers::coalesce_branches(all_results);

    // Lift to 3-tuple: drain pending_events from each BattleState.
    if observer.is_none() {
        return all_results.into_iter().map(|(st, p)| (st, None, p)).collect();
    }

    // Event-aware coalesce: branches with identical state but divergent event histories
    // (e.g. Crit vs no-Crit on a 0-damage hit) must NOT merge.
    let triples: Vec<((MatchState, Option<Vec<InformationEvent>>), f64)> = all_results
        .into_iter()
        .map(|(mut st, p)| {
            let events = match &mut st {
                MatchState::BattleState(bs) => Some(std::mem::take(&mut bs.pending_events)),
                _ => Some(vec![]),
            };
            ((st, events), p)
        })
        .collect();
    simulator_helpers::coalesce_branches(triples)
        .into_iter()
        .map(|((st, ev), p)| (st, ev, p))
        .collect()
}

/// Public validator wrapper used by interactive UI to check legality
pub fn validate_battle_command_combination(cmds: &[BattleCommand]) -> bool {
    is_valid_command_combination(cmds)
}

/// Reset volatile statuses and boosts on a Pokémon that is switching out.
fn clear_pokemon_for_switch_out(mon: &mut PokemonState) {
    use crate::data::species::Species;
    // Revert Power Trick / Power Shift stat swap before clearing volatiles.
    let had_power_trick_swap = simulator_helpers::has_status_volatile(mon, &VolatileStatus::PowerTrick)
        || simulator_helpers::has_status_volatile(mon, &VolatileStatus::PowerShift);
    // Revert Speed Swap: restore the original Speed stored in the volatile.
    let original_spe = mon.volatiles.iter().find_map(|v| {
        if let crate::state::pokemon::VolatileStatusState::TurnStatus(VolatileStatus::SpeedSwap(spe), _) = v {
            Some(*spe)
        } else {
            None
        }
    });
    mon.volatiles.clear();
    mon.boosts.iter_mut().for_each(|b| *b = 0);
    if had_power_trick_swap {
        mon.stats.swap(1, 2);
    }
    if let Some(spe) = original_spe {
        mon.stats[5] = spe;
    }
    if matches!(mon.status, Some(Status::ToxicPoison(_))) {
        mon.status = Some(Status::ToxicPoison(0));
    }
    // Hunger Switch: Morpeko reverts to Full Belly form on switch-out.
    if mon.species == Species::MorpekoHangry {
        mon.species = Species::Morpeko;
    }
    // Clear the entry flags so they don't persist on the bench.
    mon.entered_this_turn = false;
    mon.first_move_on_field = false;
    mon.first_turn_on_field_pending = false;
    mon.cud_chew_pending = None;
    // Unburden's boost ends on switch-out.
    mon.item_lost = false;
    // Per-turn event flags don't follow a Pokémon to the bench.
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
    // All consecutive-use streaks reset on switch-out.
    mon.stall_counter = 0;
    mon.ally_switch_counter = 0;
    mon.consecutive_move_count = 0;
    // Null last_used_move so the Metronome streak doesn't carry across switch-ins.
    mon.last_used_move = None;
    // Rage Fist hit counter resets when the Pokémon leaves the field (Champions rules).
    mon.times_hit = 0;
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
    // In-battle type changes (Soak, Magic Powder, Forest's Curse, Trick-or-Treat, Reflect
    // Type) end when the Pokémon leaves the field. Restore the current forme's natural types
    // from the dex; this is a no-op for unaffected Pokémon and correct after a forme change.
    if let Some(form_data) = pokemon_dex.get(&leaving.species) {
        leaving.types = form_data.types.clone();
    }

    // The incoming replacement switched in this turn (Payback won't double against it).
    if let Some(incoming) = back.get_mut(bench_index) {
        incoming.switched_in_this_turn = true;
    }

    // Zero to Hero: Palafin permanently becomes Hero Form the first time it leaves the
    // field (any switch cause). Never reverts for the rest of the battle.
    if leaving.species == Species::Palafin && leaving.ability == Ability::ZerotoHero && !leaving.fainted {
        crate::state::battle::change_form(&mut leaving, Species::PalafinHero, pokemon_dex);
    }
    // Stance Change: Aegislash reverts to Shield Forme on switch-out.
    if leaving.species == Species::AegislashBlade && leaving.ability == Ability::StanceChange {
        crate::state::battle::change_form(&mut leaving, Species::Aegislash, pokemon_dex);
    }
    std::mem::swap(&mut active[slot_idx], &mut back[bench_index]);
    back[bench_index] = leaving;

    // SyrupBomb ends when the user leaves the field — remove it from all opponents.
    // (The target's SyrupBomb is on the opponent side relative to the one switching out.)
    {
        let opp_player = match user_slot.player { Player::P1 => Player::P2, Player::P2 => Player::P1 };
        let opp_count = match opp_player {
            Player::P1 => next_state.p1_active_mons.len(),
            Player::P2 => next_state.p2_active_mons.len(),
        };
        let mut syrup_bomb_cleared: Vec<FieldSlot> = Vec::new();
        for i in 0..opp_count {
            let opp = match opp_player {
                Player::P1 => &mut next_state.p1_active_mons[i],
                Player::P2 => &mut next_state.p2_active_mons[i],
            };
            if simulator_helpers::remove_status_volatile(opp, &VolatileStatus::SyrupBomb) {
                syrup_bomb_cleared.push(FieldSlot { player: opp_player, slot_index: i as u8 });
            }
        }
        for slot in syrup_bomb_cleared {
            simulator_helpers::emit(next_state, EventKind::VolatileEnd { target: slot, volatile: VolatileStatus::SyrupBomb });
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
                    use crate::state::pokemon::VolatileStatusState;
                    use VolatileStatus::*;
                    matches!(v,
                        VolatileStatusState::TurnStatus(Confusion, _)
                        | VolatileStatusState::TurnStatus(FocusEnergy, _)
                        | VolatileStatusState::TurnStatus(PartiallyTrapped(_), _)
                        | VolatileStatusState::TurnStatus(LeechSeed, _)
                        | VolatileStatusState::TurnStatus(Curse, _)
                        | VolatileStatusState::TurnStatus(Substitute(_), _)
                        | VolatileStatusState::TurnStatus(Ingrain, _)
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
                    matches!(v, crate::state::pokemon::VolatileStatusState::TurnStatus(VolatileStatus::Substitute(_), _))
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
fn process_sendouts_in_speed_order_branching(base_state: &BattleState, slots: &[FieldSlot], move_dex: &HashMap<PokemonMove, MoveData>) -> Vec<(BattleState, f64)> {
    if slots.is_empty() { return vec![(base_state.clone(), 1.0)]; }

    // Build (slot, switch_in_priority, effective_speed) triples.
    let trick = simulator_helpers::trick_room_is_active(base_state);
    let mut slot_keys: Vec<(FieldSlot, i8, f32)> = Vec::new();
    for slot in slots {
        if let Some(mon) = simulator_helpers::get_pokemon_at_slot(base_state, *slot) {
            let sw_prio = simulator_helpers::ability_switch_in_priority(&mon.ability);
            let speed = simulator_helpers::effective_speed_for_slot(base_state, *slot, mon);
            slot_keys.push((*slot, sw_prio, speed));
        }
    }
    // Primary sort: switch_in_priority descending (higher activates first).
    // Secondary sort: speed descending (normal) or ascending (Trick Room).
    // Trick Room reverses only the speed tiebreak, not the priority bracket.
    slot_keys.sort_by(|a, b| {
        if a.1 != b.1 {
            return b.1.cmp(&a.1); // higher priority first
        }
        if (a.2 - b.2).abs() < 0.01 { std::cmp::Ordering::Equal }
        else if trick { a.2.partial_cmp(&b.2).unwrap_or(std::cmp::Ordering::Equal) }
        else { b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal) }
    });

    // Group by equal (switch_in_priority, speed) key — ties within a group are random.
    let mut groups: Vec<Vec<FieldSlot>> = Vec::new();
    let mut current_group: Vec<FieldSlot> = Vec::new();
    let mut last_prio: Option<i8> = None;
    let mut last_speed: Option<f32> = None;
    for (slot, prio, sp) in slot_keys {
        let same_group = match (last_prio, last_speed) {
            (Some(lp), Some(ls)) => lp == prio && (sp - ls).abs() < 0.01,
            _ => false,
        };
        if same_group {
            current_group.push(slot);
        } else {
            if !current_group.is_empty() {
                groups.push(current_group.clone());
                current_group.clear();
            }
            current_group.push(slot);
            last_prio = Some(prio);
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
        // Build switch list for the event (HP observed from base_state before ability mutations).
        let switches: Vec<InfoSwitchState> = if st.event_observer.is_some() {
            order.iter().filter_map(|slot| {
                let mon = simulator_helpers::get_pokemon_at_slot(base_state, *slot)?;
                let hp = simulator_helpers::observed_hp(base_state, *slot, st.event_observer.unwrap());
                Some(InfoSwitchState {
                    slot: *slot,
                    // Illusion disguise: base_state is pre-send-out, so illusion_disguise is not
                    // yet set; compute the disguise species from party composition directly.
                    species: if slot.player != st.event_observer.unwrap() {
                        simulator_helpers::compute_illusion_disguise(base_state, *slot)
                            .unwrap_or_else(|| mon.species.clone())
                    } else {
                        mon.species.clone()
                    },
                    level: mon.level,
                    hp,
                    status: mon.status.clone(),
                    tera_type: if mon.is_tera { Some(mon.tera_type.clone()) } else { None },
                })
            }).collect()
        } else {
            vec![]
        };
        // Wrap all send-out effects (abilities, hazards, etc.) as reactions of SimultaneousSwitch.
        simulator_helpers::with_reactions(&mut st, EventKind::SimultaneousSwitch { switches }, |bs| {
            for slot in &order {
                simulator_helpers::process_pokemon_send_out(bs, *slot, move_dex);
            }
        });
        results.push((st, prob));
    }

    results
}

// Branching version of performing simultaneous switches: returns all possible resulting states with probabilities
fn perform_simultaneous_switches_branching(
    next_state: &BattleState,
    switches: &[(FieldSlot, usize)],
    pokemon_dex: &HashMap<Species, PokemonData>,
    move_dex: &HashMap<PokemonMove, MoveData>,
) -> Vec<(BattleState, f64)> {
    // First apply all swaps to a base state
    let mut base = next_state.clone();
    for (slot, bench_index) in switches {
        perform_switch_out_in(&mut base, *slot, *bench_index, pokemon_dex);
    }
    // collect slots to process send-out effects for (the slots that were switched)
    let slots: Vec<FieldSlot> = switches.iter().map(|(s, _)| *s).collect();
    simulator_helpers::coalesce_branches(process_sendouts_in_speed_order_branching(&base, &slots, move_dex))
}

// Branching version of creating battle state from preview that respects speed-order send-outs and ties
fn battle_state_from_preview_branching(
    preview: &TeamPreviewState,
    p1_preview: &TeamPreviewCommand,
    p2_preview: &TeamPreviewCommand,
    move_dex: &HashMap<PokemonMove, MoveData>,
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
        last_move_on_field: None,
        sub_damage_dealt: 0,
        round_used_this_turn: false,
        move_was_prevented: false,
        pending_events: vec![],
        event_observer: None,
    };

    // Collect all active send-out slots
    let mut slots: Vec<FieldSlot> = Vec::new();
    for slot_idx in 0..state.p1_active_mons.len() {
        slots.push(FieldSlot { player: Player::P1, slot_index: slot_idx as u8 });
    }
    for slot_idx in 0..state.p2_active_mons.len() {
        slots.push(FieldSlot { player: Player::P2, slot_index: slot_idx as u8 });
    }

    let branches = process_sendouts_in_speed_order_branching(&state, &slots, move_dex);
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
            simulator_helpers::coalesce_branches(battle_state_from_preview_branching(preview, p1_preview, p2_preview, move_dex))
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

                let branches = perform_simultaneous_switches_branching(&next_state, &queued_switches, pokemon_dex, move_dex);
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
                    simulator_helpers::process_pokemon_send_out(&mut next_state, pending_slot, move_dex);
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
