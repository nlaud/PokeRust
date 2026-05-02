use std::collections::HashMap;
use std::io::{self, Write};

use crate::battle::{BattleCommand, BattleState, FieldSlot, MatchState, Player, PlayerCommand, TeamPreviewCommand};
use crate::data::pokemon_move::PokemonMove;
use crate::data::species::Species;
use crate::dex_data::{MoveData, PokemonData};
use crate::simulator;

fn humanize_identifier(value: impl AsRef<str>) -> String {
    let value = value.as_ref();
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

fn species_name(species: &Species) -> String {
    humanize_identifier(format!("{:?}", species))
}

fn move_name(mov: &PokemonMove) -> String {
    humanize_identifier(format!("{:?}", mov))
}

fn player_name(player: Player) -> &'static str {
    match player {
        Player::P1 => "P1",
        Player::P2 => "P2",
    }
}

fn active_mon_name(state: &BattleState, slot: FieldSlot) -> String {
    let mon = match slot.player {
        Player::P1 => state.p1_active_mons.get(slot.slot_index as usize),
        Player::P2 => state.p2_active_mons.get(slot.slot_index as usize),
    };

    mon.map(|mon| species_name(&mon.species)).unwrap_or_else(|| format!("Slot {}", slot.slot_index + 1))
}

fn back_mon_name(state: &BattleState, player: Player, party_index: usize) -> String {
    let mon = match player {
        Player::P1 => state.p1_back_mons.get(party_index),
        Player::P2 => state.p2_back_mons.get(party_index),
    };

    mon.map(|mon| species_name(&mon.species)).unwrap_or_else(|| format!("Bench {}", party_index + 1))
}

fn preview_command_description(preview: &crate::battle::TeamPreviewState, player: Player, command: &TeamPreviewCommand) -> String {
    let mons = match player {
        Player::P1 => &preview.p1_mons,
        Player::P2 => &preview.p2_mons,
    };

    let active = command
        .active_indices
        .iter()
        .map(|index| species_name(&mons[*index].species))
        .collect::<Vec<_>>()
        .join(", ");

    let back = command
        .back_indices
        .iter()
        .map(|index| species_name(&mons[*index].species))
        .collect::<Vec<_>>()
        .join(", ");

    format!("Active: [{}] | Back: [{}]", active, back)
}

fn battle_command_description(state: &BattleState, player: Player, slot_idx: usize, command: &BattleCommand) -> String {
    let active_mon = match player {
        Player::P1 => state.p1_active_mons.get(slot_idx),
        Player::P2 => state.p2_active_mons.get(slot_idx),
    };

    match command {
        BattleCommand::Switch(switch) => {
            format!("Switch to {}", back_mon_name(state, player, switch.party_index))
        }
        BattleCommand::Attack(attack) => {
            let move_label = active_mon
                .and_then(|mon| mon.moves.get(attack.move_slot).and_then(|mov| mov.as_ref()))
                .map(move_name)
                .unwrap_or_else(|| format!("Move {}", attack.move_slot + 1));

            let target_label = attack
                .target
                .map(|slot| format!("{}'s {}", player_name(slot.player), active_mon_name(state, slot)))
                .unwrap_or_else(|| "no target".to_string());

            let mut label = format!("Use {} -> {}", move_label, target_label);
            if attack.terastallize {
                label.push_str(" [Tera]");
            }
            if attack.mega_evolve {
                label.push_str(" [Mega]");
            }
            label
        }
    }
}

fn prompt_choice(prompt: &str, options: &[String]) -> usize {
    loop {
        println!("{}", prompt);
        for (index, option) in options.iter().enumerate() {
            println!("{}: {}", index + 1, option);
        }

        print!("Choice: ");
        io::stdout().flush().unwrap();

        let mut input = String::new();
        io::stdin().read_line(&mut input).unwrap();
        let trimmed = input.trim();

        if let Ok(index) = trimmed.parse::<usize>() {
            if (1..=options.len()).contains(&index) {
                return index - 1;
            }
        }

        let normalized_input = humanize_identifier(trimmed).to_lowercase();
        if let Some(index) = options.iter().position(|option| humanize_identifier(option).to_lowercase() == normalized_input) {
            return index;
        }

        let matching_indices = options
            .iter()
            .enumerate()
            .filter(|(_, option)| humanize_identifier(option).to_lowercase().contains(&normalized_input))
            .map(|(index, _)| index)
            .collect::<Vec<_>>();

        if matching_indices.len() == 1 {
            return matching_indices[0];
        }

        println!("Invalid choice, try again.");
    }
}

pub fn choose_team_preview_command(preview: &crate::battle::TeamPreviewState, player: Player) -> TeamPreviewCommand {
    let mons = match player {
        Player::P1 => &preview.p1_mons,
        Player::P2 => &preview.p2_mons,
    };

    let total_mons = mons.len();
    let brought = (preview.brought_per_side as usize).min(total_mons);
    let active_n = (preview.active_per_side as usize).min(brought);

    println!("Choose {} active (front) Pokémon for {}:", active_n, player_name(player));
    let mut chosen_active: Vec<usize> = Vec::new();
    while chosen_active.len() < active_n {
        let options: Vec<String> = (0..total_mons)
            .filter(|i| !chosen_active.contains(i))
            .map(|i| species_name(&mons[i].species))
            .collect();

        let prompt = format!("Select active Pokémon {}/{}:", chosen_active.len() + 1, active_n);
        let pick = prompt_choice(&prompt, &options);

        // Map pick back to global index (skipping already-chosen indices)
        let mut available_indices: Vec<usize> = (0..total_mons).filter(|i| !chosen_active.contains(i)).collect();
        let chosen_index = available_indices[pick];
        chosen_active.push(chosen_index);
        println!("Chosen: {}", species_name(&mons[chosen_index].species));
    }

    // Choose remaining brought (back) slots from the remaining team members
    let mut chosen_brought: Vec<usize> = chosen_active.clone();
    let need_back = brought.saturating_sub(chosen_brought.len());
    if need_back > 0 {
        println!("Choose {} bench (back) Pokémon for {}:", need_back, player_name(player));
        while chosen_brought.len() < brought {
            let options: Vec<String> = (0..total_mons)
                .filter(|i| !chosen_brought.contains(i))
                .map(|i| species_name(&mons[i].species))
                .collect();

            let prompt = format!("Select bench Pokémon {}/{}:", chosen_brought.len() - active_n + 1, need_back);
            let pick = prompt_choice(&prompt, &options);

            let mut available_indices: Vec<usize> = (0..total_mons).filter(|i| !chosen_brought.contains(i)).collect();
            let chosen_index = available_indices[pick];
            chosen_brought.push(chosen_index);
            println!("Chosen bench: {}", species_name(&mons[chosen_index].species));
        }
    }

    // Back indices are the chosen brought ones that are not active
    let back_indices: Vec<usize> = chosen_brought.iter().filter(|i| !chosen_active.contains(i)).cloned().collect();

    TeamPreviewCommand { active_indices: chosen_active, back_indices }
}

pub fn choose_battle_command_for_slot(
    state: &BattleState,
    player: Player,
    slot_idx: usize,
    move_dex: &HashMap<PokemonMove, MoveData>,
    pokemon_dex: &HashMap<Species, PokemonData>,
) -> BattleCommand {
    let commands = simulator::get_possible_commands_for_active_slot(state, player, slot_idx, move_dex, pokemon_dex);
    if commands.is_empty() {
        panic!("No legal commands available for {} slot {}", player_name(player), slot_idx + 1);
    }

    let options = commands
        .iter()
        .map(|command| battle_command_description(state, player, slot_idx, command))
        .collect::<Vec<_>>();

    let mon_label = match player {
        Player::P1 => state.p1_active_mons.get(slot_idx),
        Player::P2 => state.p2_active_mons.get(slot_idx),
    }
    .map(|mon| species_name(&mon.species))
    .unwrap_or_else(|| format!("Slot {}", slot_idx + 1));

    let choice = prompt_choice(&format!("Choose an action for {}'s {}", player_name(player), mon_label), &options);
    commands[choice].clone()
}

pub fn choose_battle_commands_for_player(
    state: &BattleState,
    player: Player,
    move_dex: &HashMap<PokemonMove, MoveData>,
    pokemon_dex: &HashMap<Species, PokemonData>,
) -> PlayerCommand {
    let active_len = match player {
        Player::P1 => state.p1_active_mons.len(),
        Player::P2 => state.p2_active_mons.len(),
    };

    loop {
        let mut commands = Vec::new();
        for slot_idx in 0..active_len {
            commands.push(choose_battle_command_for_slot(state, player, slot_idx, move_dex, pokemon_dex));
        }

        if simulator::validate_battle_command_combination(&commands) {
            return PlayerCommand::Battle(commands);
        }

        println!("The combination of choices is not legal; please choose again.");
    }
}

pub fn simulate_battle(
    mut state: MatchState,
    move_dex: &HashMap<PokemonMove, MoveData>,
    pokemon_dex: &HashMap<Species, PokemonData>,
    verbosity: u8,
) {
    loop {
        match &state {
            MatchState::TeamPreviewState(preview_state) => {
                if verbosity >= 1 {
                    println!("Current Team Preview State: {:#?}", preview_state);
                }

                let p1_cmd = PlayerCommand::TeamPreview(choose_team_preview_command(preview_state, Player::P1));
                let p2_cmd = PlayerCommand::TeamPreview(choose_team_preview_command(preview_state, Player::P2));

                let next_states = simulator::simulate_turn(&state, &p1_cmd, &p2_cmd, move_dex);
                state = next_states
                    .into_iter()
                    .next()
                    .map(|(next_state, _prob)| next_state)
                    .unwrap_or_else(|| state.clone());
            }
            MatchState::BattleState(battle_state) => {
                if verbosity >= 1 {
                    println!("\nCurrent Battle State:\n{}", battle_state);
                }

                let p1_cmd = choose_battle_commands_for_player(battle_state, Player::P1, move_dex, pokemon_dex);
                let p2_cmd = choose_battle_commands_for_player(battle_state, Player::P2, move_dex, pokemon_dex);

                let next_states = simulator::simulate_turn(&state, &p1_cmd, &p2_cmd, move_dex);
                state = next_states
                    .into_iter()
                    .next()
                    .map(|(next_state, _prob)| next_state)
                    .unwrap_or_else(|| state.clone());
            }
            MatchState::GameOverState { winner } => {
                println!("Game over. Winner: {:?}", winner);
                break;
            }
        }
    }
}