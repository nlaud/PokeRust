//! Battle session state and the turn-resolution port of the terminal driver
//! (`user::simulate_battle`): the server holds the authoritative `MatchState`,
//! validates submitted commands against freshly-enumerated legal options, and
//! samples one branch of `simulate_turn`'s weighted outcome set.

use std::collections::HashMap;

use poke_rust::data::pokemon_move::PokemonMove;
use poke_rust::data::species::Species;
use poke_rust::simulator;
use poke_rust::state::battle::{
    BattleCommand, BattleState, FieldSlot, MatchState, Player, PlayerCommand, SwitchCommand,
    TeamPreviewCommand,
};
use poke_rust::state::dex_data::{MoveData, PokemonData};
use poke_rust::user::replacement_commands_are_valid;

use crate::dto::*;
use crate::mapping;

pub struct Dexes {
    pub pokemon_dex: HashMap<Species, PokemonData>,
    pub move_dex: HashMap<PokemonMove, MoveData>,
}

#[derive(Clone, Copy)]
pub struct SessionConfig {
    pub active_per_side: u8,
    pub brought_per_side: u8,
    pub consider_crit: bool,
    pub damage_rolls: u8,
}

pub struct BattleSession {
    pub state: MatchState,
    pub config: SessionConfig,
    pub log: Vec<TurnLogEntry>,
}

impl BattleSession {
    pub fn view(&self) -> BattleView {
        mapping::battle_view(
            &self.state,
            self.config.active_per_side,
            self.config.brought_per_side,
        )
    }
}

fn active_mons(state: &BattleState, player: Player) -> &Vec<poke_rust::state::pokemon::PokemonState> {
    match player {
        Player::P1 => &state.p1_active_mons,
        Player::P2 => &state.p2_active_mons,
    }
}

fn healthy_bench_switches(state: &BattleState, player: Player) -> Vec<BattleCommand> {
    let back_mons = match player {
        Player::P1 => &state.p1_back_mons,
        Player::P2 => &state.p2_back_mons,
    };
    back_mons
        .iter()
        .enumerate()
        .filter(|(_, m)| !m.fainted)
        .map(|(i, _)| BattleCommand::Switch(SwitchCommand { party_index: i }))
        .collect()
}

/// Enumerate the legal commands for every slot of `player`, mirroring the phase
/// dispatch in `user::choose_battle_commands_for_player`. Slots whose only legal
/// action is Pass are marked `forced` so the frontend can auto-fill them.
pub fn legal_commands(
    session: &BattleSession,
    dexes: &Dexes,
    player: Player,
) -> LegalCommandsView {
    let phase = mapping::phase_of(&session.state);
    let mut slots = Vec::new();

    if let MatchState::BattleState(state) = &session.state {
        let active_len = active_mons(state, player).len();

        for slot_idx in 0..active_len {
            let (commands, forced) = match phase {
                PhaseDto::SelfSwitch => {
                    let pending = state.self_switch_pending.map(|(slot, _)| slot);
                    let this_slot = FieldSlot {
                        player,
                        slot_index: slot_idx as u8,
                    };
                    if pending == Some(this_slot) {
                        let switches = healthy_bench_switches(state, player);
                        if switches.is_empty() {
                            (vec![BattleCommand::Pass], true)
                        } else {
                            (switches, false)
                        }
                    } else {
                        (vec![BattleCommand::Pass], true)
                    }
                }
                PhaseDto::Replacement => {
                    let mon = &active_mons(state, player)[slot_idx];
                    if mon.fainted {
                        let switches = healthy_bench_switches(state, player);
                        if switches.is_empty() {
                            (vec![BattleCommand::Pass], true)
                        } else {
                            (switches, false)
                        }
                    } else {
                        (vec![BattleCommand::Pass], true)
                    }
                }
                _ => {
                    let commands = simulator::get_possible_commands_for_active_slot(
                        state,
                        player,
                        slot_idx,
                        &dexes.move_dex,
                        &dexes.pokemon_dex,
                    );
                    // A Pass-only slot (fainted mon with an empty bench, or a
                    // recharge turn) offers no real choice — let the UI skip it.
                    let forced =
                        commands.len() == 1 && matches!(commands[0], BattleCommand::Pass);
                    (commands, forced)
                }
            };

            slots.push(SlotCommandsDto {
                slot_index: slot_idx,
                forced,
                options: commands
                    .iter()
                    .map(|c| mapping::command_option(state, player, slot_idx, c))
                    .collect(),
            });
        }
    }

    LegalCommandsView { phase, slots }
}

/// Rebuild a `PlayerCommand` from the wire format, rejecting anything that is not
/// in the legal command set the server itself would offer for the current phase.
pub fn reconstruct_player_command(
    session: &BattleSession,
    dexes: &Dexes,
    player: Player,
    dto: &PlayerCommandDto,
) -> Result<PlayerCommand, String> {
    match (&session.state, dto) {
        (MatchState::TeamPreviewState(preview), PlayerCommandDto::TeamPreview { active_indices, back_indices }) => {
            let mons = match player {
                Player::P1 => &preview.p1_mons,
                Player::P2 => &preview.p2_mons,
            };
            let total = mons.len();
            let brought = (preview.brought_per_side as usize).min(total);
            let active_n = (preview.active_per_side as usize).min(brought);

            let mut all: Vec<usize> = active_indices.iter().chain(back_indices.iter()).copied().collect();
            all.sort_unstable();
            let distinct = all.windows(2).all(|w| w[0] != w[1]);

            if active_indices.len() != active_n {
                return Err(format!("{:?} must pick exactly {} lead(s)", player, active_n));
            }
            if active_indices.len() + back_indices.len() != brought {
                return Err(format!("{:?} must bring exactly {} Pokemon", player, brought));
            }
            if !distinct || all.iter().any(|&i| i >= total) {
                return Err(format!("{:?} team preview picks must be distinct and in range", player));
            }

            Ok(PlayerCommand::TeamPreview(TeamPreviewCommand {
                active_indices: active_indices.clone(),
                back_indices: back_indices.clone(),
            }))
        }
        (MatchState::TeamPreviewState(_), _) => {
            Err(format!("{:?}: expected a teamPreview command", player))
        }
        (MatchState::BattleState(state), PlayerCommandDto::Battle { commands }) => {
            let active_len = active_mons(state, player).len();
            if commands.len() != active_len {
                return Err(format!(
                    "{:?} must submit exactly {} command(s), got {}",
                    player,
                    active_len,
                    commands.len()
                ));
            }

            let legal = legal_commands(session, dexes, player);
            let rebuilt: Vec<BattleCommand> =
                commands.iter().map(mapping::battle_command_from_dto).collect();

            for (slot_idx, command) in rebuilt.iter().enumerate() {
                let slot_legal = &legal.slots[slot_idx].options;
                let is_legal = slot_legal
                    .iter()
                    .any(|opt| mapping::battle_command_from_dto(&opt.command) == *command);
                if !is_legal {
                    return Err(format!(
                        "{:?} slot {}: {:?} is not a legal command",
                        player,
                        slot_idx + 1,
                        command
                    ));
                }
            }

            let phase = mapping::phase_of(&session.state);
            match phase {
                PhaseDto::Replacement => {
                    if !replacement_commands_are_valid(state, player, active_mons(state, player), &rebuilt) {
                        return Err(format!("{:?}: invalid replacement command set", player));
                    }
                }
                _ => {
                    if !simulator::validate_battle_command_combination(&rebuilt) {
                        return Err(format!("{:?}: command combination is not legal", player));
                    }
                }
            }

            Ok(PlayerCommand::Battle(rebuilt))
        }
        (MatchState::BattleState(_), PlayerCommandDto::Pass) => Ok(PlayerCommand::Pass),
        (MatchState::BattleState(_), PlayerCommandDto::TeamPreview { .. }) => {
            Err(format!("{:?}: teamPreview command outside team preview", player))
        }
        (MatchState::GameOverState { .. }, _) => Err("battle is already over".to_string()),
    }
}

/// Resolve one input step: run the engine in sample mode — it walks a single
/// weighted trajectory instead of enumerating the full outcome tree (which
/// exhausts memory on doubles spread turns at 16 damage rolls) — then advance
/// the session and log the event tree.
pub fn resolve_turn(
    session: &mut BattleSession,
    dexes: &Dexes,
    p1_cmd: &PlayerCommand,
    p2_cmd: &PlayerCommand,
) -> (Vec<EventNode>, f64) {
    let label = match &session.state {
        MatchState::TeamPreviewState(_) => "Team Preview".to_string(),
        MatchState::BattleState(state) => format!("Turn {}", state.turn_number),
        MatchState::GameOverState { .. } => "Game Over".to_string(),
    };

    let (next_state, events, probability) = simulator::sample_turn(
        &session.state,
        p1_cmd,
        p2_cmd,
        &dexes.move_dex,
        &dexes.pokemon_dex,
        session.config.consider_crit,
        session.config.damage_rolls,
        Some(Player::P1),
    );

    let event_nodes: Vec<EventNode> = events
        .unwrap_or_default()
        .iter()
        .map(mapping::event_node)
        .collect();

    session.state = next_state;
    session.log.push(TurnLogEntry {
        label,
        events: event_nodes.clone(),
    });

    (event_nodes, probability)
}
