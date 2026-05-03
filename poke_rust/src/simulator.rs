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
use crate::data::species::Species;
use crate::data::pokemon_move::PokemonMove;

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
) -> Vec<(MatchState, f64)> {
    let mut next_state = state.clone();
    
    if next_state.action_queue.is_empty() {
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
    
    if get_verbosity() >= 2 {
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
        }
    }
    
    match action {
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
            println!("{}", format!("[UNHANDLED] Move action: {} uses {} -> {}", attacker, move_name_sim(&m.move_name), target).bright_red());
            vec![(MatchState::BattleState(next_state), 1.0)]
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
) -> Vec<(MatchState, f64)> {
    // First, apply the player commands to populate the action queue
    let mut current_state = apply_player_commands(state, p1_cmd, p2_cmd, move_dex);
    
    // Then process the action queue one step at a time
    loop {
        match &current_state {
            MatchState::BattleState(battle) => {
                if battle.action_queue.is_empty() {
                    break;
                }
                
                let outcomes = step_action_queue(battle, move_dex, pokemon_dex);
                
                // For now, just take the first outcome (deterministic processing)
                if let Some((next_match_state, _)) = outcomes.first() {
                    current_state = next_match_state.clone();
                } else {
                    break;
                }
            }
            _ => break,
        }
    }
    
    vec![(current_state, 1.0)]
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
            // swap
            let mut incoming = next_state.p1_back_mons[bench_index].clone();
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
            let mut incoming = next_state.p2_back_mons[bench_index].clone();
            std::mem::swap(&mut next_state.p2_active_mons[slot_idx], &mut next_state.p2_back_mons[bench_index]);
            next_state.p2_back_mons[bench_index] = leaving;
        }
    }
}