//! Battle session state and the turn-resolution port of the terminal driver
//! (`user::simulate_battle`): the server holds the authoritative `MatchState`,
//! validates submitted commands against freshly-enumerated legal options, and
//! samples one branch of `simulate_turn`'s weighted outcome set.

use std::collections::{HashMap, HashSet};

use poke_rust::data::ability::Ability;
use poke_rust::data::pokemon_move::PokemonMove;
use poke_rust::data::species::Species;
use poke_rust::information::inference::{apply_information, InferenceConfig};
use poke_rust::information::unknowns::{InformationMode, UnknownMatchState};
use poke_rust::simulator;
use poke_rust::state::battle::{
    BattleCommand, BattleState, FieldSlot, MatchState, Player, PlayerCommand, SwitchCommand,
    TeamPreviewCommand,
};
use poke_rust::state::dex_data::{AbilityData, MoveData, PokemonData};
use poke_rust::user::replacement_commands_are_valid;

use crate::dto::*;
use crate::mapping;

pub struct Dexes {
    pub pokemon_dex: HashMap<Species, PokemonData>,
    pub move_dex: HashMap<PokemonMove, MoveData>,
    /// Used by the inference engine for ability absence/priority reasoning under
    /// non-Perfect information modes; unused (and fine to be empty) otherwise.
    pub ability_dex: HashMap<Ability, AbilityData>,
    /// Used by the inference engine for Illusion narrowing under non-Perfect
    /// information modes; unused (and fine to be empty) otherwise.
    pub learnset_dex: HashMap<Species, HashSet<PokemonMove>>,
}

#[derive(Clone, Copy)]
pub struct SessionConfig {
    pub active_per_side: u8,
    pub brought_per_side: u8,
    pub consider_crit: bool,
    pub damage_rolls: u8,
    /// Recorded for API introspection (e.g. a future "what mode is this battle in"
    /// endpoint); `belief.is_some()` alone already fully determines masking behavior
    /// downstream, so nothing currently reads this back.
    #[allow(dead_code)]
    pub information_mode: InformationMode,
}

pub struct BattleSession {
    pub state: MatchState,
    pub config: SessionConfig,
    pub log: Vec<TurnLogEntry>,
    /// The observer's (P1's) evolving fog-of-war belief about the opponent's team,
    /// under `config.information_mode`. `None` for `InformationMode::PerfectInformation`
    /// — a true zero-overhead no-op that keeps ground-truth behavior byte-identical;
    /// `Some` for the other modes, advanced each turn in `resolve_turn` via
    /// `into_battle_state` (team preview → battle) or `apply_information` (every
    /// turn after).
    pub belief: Option<UnknownMatchState>,
    /// Built once at session creation; `Some` exactly when `belief` is `Some`.
    pub inference_config: Option<InferenceConfig>,
}

impl BattleSession {
    pub fn view(&self) -> BattleView {
        mapping::battle_view(
            &self.state,
            self.config.active_per_side,
            self.config.brought_per_side,
            self.belief.as_ref(),
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
                        // Earlier fainted slots claim bench mons first: with two
                        // fainted actives and one healthy bench mon, slot 0 gets
                        // the switch and this slot is a forced Pass.
                        let earlier_fainted = active_mons(state, player)[..slot_idx]
                            .iter()
                            .filter(|m| m.fainted)
                            .count();
                        if switches.len() <= earlier_fainted {
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
                        "{:?} active slot {} (0-based): {:?} is not a legal command right now",
                        player, slot_idx, command
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
/// Turns a caught panic payload into a human-readable message for the API error body.
fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        s.to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "internal error (inference engine panicked)".to_string()
    }
}

/// Advance one turn, or fail without mutating `session` at all.
///
/// The fog-of-war inference engine (`apply_information`) can still panic on an
/// unresolved contradiction (see `information/AUDIT.md`) — a single malformed
/// belief update must not take the whole gateway down with it. Everything below is
/// computed into locals first; `session.state`/`session.belief`/`session.log` are
/// only written at the very end, once we know the belief update actually
/// succeeded. On failure the session is left exactly as it was before this call —
/// "most recent good information stays the source of truth" — and the caller
/// reports the error instead of silently desyncing belief from ground truth by one
/// turn (which would corrupt every future switch-in / masking decision).
pub fn resolve_turn(
    session: &mut BattleSession,
    dexes: &Dexes,
    p1_cmd: &PlayerCommand,
    p2_cmd: &PlayerCommand,
) -> Result<(Vec<EventNode>, f64), String> {
    let label = match &session.state {
        MatchState::TeamPreviewState(_) => "Team Preview".to_string(),
        MatchState::BattleState(state) => format!("Turn {}", state.turn_number),
        MatchState::GameOverState { .. } => "Game Over".to_string(),
    };
    let was_team_preview = matches!(session.state, MatchState::TeamPreviewState(_));

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

    let events = events.unwrap_or_default();
    let event_nodes: Vec<EventNode> = events.iter().map(mapping::event_node).collect();

    // Advance the fog-of-war belief in lockstep with the ground-truth transition
    // above, when a non-Perfect-Information mode is tracking one. Cloned rather than
    // `.take()`n so `session.belief` is untouched if this whole function bails out
    // early below.
    let next_belief: Option<UnknownMatchState> = match session.belief.clone() {
        None => None,
        Some(belief) => {
            // Team-preview -> battle needs two steps, not one: `into_battle_state`
            // structurally seeds the belief (P1 fully known as usual; P2's entire
            // roster parked in `possible_back`, nothing placed active yet — see its
            // doc comment), and THEN `apply_information` walks this transition's own
            // event log (the `SimultaneousSwitch` for both sides' leads, plus entry
            // abilities/hazards/weather) through the exact same Pass 1 switch-in
            // handling every mid-battle switch already gets — including Illusion
            // widening. Skipping the second step (the old behavior) left P2's belief
            // built from the true physical active index instead of what's actually
            // displayed, which is wrong the moment a lead is a disguised Zoroark.
            // Both steps use `belief` as the seed only when `was_team_preview`;
            // `reconstruct_player_command` already guarantees both commands are
            // `PlayerCommand::TeamPreview` whenever the incoming state was
            // `TeamPreviewState`.
            let seeded = if was_team_preview {
                match (&belief, p1_cmd, p2_cmd) {
                    (
                        UnknownMatchState::TeamPreview(preview),
                        PlayerCommand::TeamPreview(p1_tp),
                        PlayerCommand::TeamPreview(p2_tp),
                    ) => UnknownMatchState::Battle(preview.into_battle_state(
                        &p1_tp.active_indices,
                        &p1_tp.back_indices,
                        &p2_tp.active_indices,
                        &p2_tp.back_indices,
                    )),
                    _ => belief,
                }
            } else {
                belief
            };
            let config = session
                .inference_config
                .as_ref()
                .expect("belief is only Some alongside a built inference_config");
            let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                apply_information(
                    seeded,
                    &events,
                    false,
                    &dexes.pokemon_dex,
                    &dexes.move_dex,
                    &dexes.ability_dex,
                    config,
                )
            }));
            match caught {
                Ok(next) => Some(next),
                Err(payload) => return Err(panic_message(payload)),
            }
        }
    };

    session.state = next_state;
    session.belief = next_belief;
    session.log.push(TurnLogEntry {
        label,
        events: event_nodes.clone(),
    });

    Ok((event_nodes, probability))
}
