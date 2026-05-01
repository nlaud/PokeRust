use std::collections::HashMap;
use crate::pokemon::{
    MatchState, BattleState, TeamPreviewState, PlayerCommand, BattleCommand,
    AttackCommand, SwitchCommand, TeamPreviewCommand, Player, FieldSlot,
    PokemonState, parse_team_sheet
};
use crate::dex_data::{MoveData, MoveTarget, PokemonData};
use crate::data::species::Species;
use crate::data::ability::Ability;
use crate::data::pokemon_move::PokemonMove;

pub fn team_preview_state_from_teamsheets(
    p1_path: &str,
    p2_path: &str,
    pokemon_dex: &HashMap<Species, PokemonData>,
    active_per_side: u8,
) -> TeamPreviewState {
    TeamPreviewState {
        active_per_side,
        p1_mons: parse_team_sheet(p1_path, pokemon_dex),
        p2_mons: parse_team_sheet(p2_path, pokemon_dex),
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

/// Generates all possible team preview commands
fn team_preview_commands(state: &TeamPreviewState, player: Player) -> Vec<PlayerCommand> {
    let mons_len = match player {
        Player::P1 => state.p1_mons.len(),
        Player::P2 => state.p2_mons.len(),
    };
    
    let active_len = (state.active_per_side as usize).min(mons_len);
    
    let mut commands = Vec::new();
    if mons_len == 0 { return commands; }
    
    let combos = get_combinations(mons_len, active_len);
    for active in combos {
        let mut back = Vec::new();
        for i in 0..mons_len {
            if !active.contains(&i) {
                back.push(i);
            }
        }
        commands.push(PlayerCommand::TeamPreview(TeamPreviewCommand {
            active_indices: active,
            back_indices: back,
        }));
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
                MoveTarget::AdjacentAllyOrSelf | MoveTarget::Any => true,
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
    move_dex: &HashMap<PokemonMove, MoveData>
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
    let can_mega = mon.can_mega_evolve;

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
        }
    }

    if tera_count > 1 || mega_count > 1 {
        return false;
    }

    true
}

fn battle_commands(state: &BattleState, player: Player, move_dex: &HashMap<PokemonMove, MoveData>) -> Vec<PlayerCommand> {
    let active_len = match player {
        Player::P1 => state.p1_active_mons.len(),
        Player::P2 => state.p2_active_mons.len(),
    };
    
    let mut slot_cmds = Vec::new();
    for i in 0..active_len {
        let cmds = generate_commands_for_active(player, i, state, move_dex);
        slot_cmds.push(cmds);
    }
    
    let combinations = cartesian_product_commands(&slot_cmds);
    combinations.into_iter()
        .filter(|combo| is_valid_command_combination(combo))
        .map(PlayerCommand::Battle)
        .collect()
}

pub fn get_possible_commands(
    state: &MatchState,
    move_dex: &HashMap<PokemonMove, MoveData>
) -> (Vec<PlayerCommand>, Vec<PlayerCommand>) {
    match state {
        MatchState::TeamPreviewState(preview) => {
            (
                team_preview_commands(preview, Player::P1),
                team_preview_commands(preview, Player::P2)
            )
        }
        MatchState::BattleState(battle) => {
            (
                battle_commands(battle, Player::P1, move_dex),
                battle_commands(battle, Player::P2, move_dex)
            )
        }
        MatchState::GameOverState { .. } => {
            (vec![], vec![])
        }
    }
}