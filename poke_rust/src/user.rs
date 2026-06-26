use std::collections::HashMap;
use std::io::{self, Write};
use colored::Colorize;
use rand::distributions::{Distribution, WeightedIndex};
use rand::thread_rng;

use crate::state::battle::{BattleCommand, BattleState, FieldSlot, MatchState, Player, PlayerCommand, TeamPreviewCommand};
use crate::data::pokemon_move::PokemonMove;
use crate::data::species::Species;
use crate::state::dex_data::{MoveData, PokemonData};
use crate::simulator;
use crate::state::pokemon::PokemonState;

fn get_verbosity() -> u8 {
    crate::VERBOSITY.get().copied().unwrap_or(1)
}

fn format_pokemon_detailed(mon: &PokemonState) -> String {
    format!("{}", mon)
}

fn print_team_section(label: &str, mons: &[crate::state::pokemon::PokemonState]) {
    println!("{}", label);
    if mons.is_empty() {
        println!("  {}", "(none)".dimmed());
    } else {
        println!("  {}", mons.iter().map(format_pokemon_detailed).collect::<Vec<_>>().join("\n  "));
    }
}

fn print_battle_state_enhanced(state: &BattleState, chance: f64) {
    println!("\n{}", "Current Battle State:".cyan().bold());
    println!("{}", format!("Selected outcome chance: {:.4}%", chance * 100.0).bright_blue());
    println!("{}", format!("{}", state).bright_blue());

    print_team_section(&"P1 Active:".green().bold().to_string(), &state.p1_active_mons);
    print_team_section(&"P1 Back:".green().to_string(), &state.p1_back_mons);
    println!("{}", format!("P1 Has Tera: {} | Has Mega: {}", state.p1_has_tera, state.p1_has_mega).green());
    print_team_section(&"P2 Active:".magenta().bold().to_string(), &state.p2_active_mons);
    print_team_section(&"P2 Back:".magenta().to_string(), &state.p2_back_mons);
    println!("{}", format!("P2 Has Tera: {} | Has Mega: {}", state.p2_has_tera, state.p2_has_mega).magenta());

    // Field / global effects
    let mut printed_field = false;
    let mut print_field_header = || {
        if !printed_field {
            println!("\n{}", "Field / Global Effects:".yellow().bold());
            printed_field = true;
        }
    };
    if state.weather.is_some() || state.terrain.is_some() || !state.pseudo_weathers.is_empty() || state.weather_turns.is_some() || state.terrain_turns.is_some() {
        print_field_header();
    }
    if let Some(weather) = &state.weather {
        if let Some(turns) = state.weather_turns {
            println!("  Weather: {:?} ({}t)", weather, turns);
        } else {
            println!("  Weather: {:?}", weather);
        }
    }
    if let Some(terrain) = &state.terrain {
        if let Some(turns) = state.terrain_turns {
            println!("  Terrain: {:?} ({}t)", terrain, turns);
        } else {
            println!("  Terrain: {:?}", terrain);
        }
    }
    if !state.pseudo_weathers.is_empty() {
        println!("  Pseudo-weathers: {:?}", state.pseudo_weathers);
    }

    // Side conditions (only print if present)
    if !state.p1_side_conditions.is_empty() || !state.p2_side_conditions.is_empty() {
        print_field_header();
        if !state.p1_side_conditions.is_empty() {
            println!("  P1 side conditions: {:?}", state.p1_side_conditions);
            if !state.p1_side_condition_turns.is_empty() { println!("  P1 side condition turns: {:?}", state.p1_side_condition_turns); }
        }
        if !state.p2_side_conditions.is_empty() {
            println!("  P2 side conditions: {:?}", state.p2_side_conditions);
            if !state.p2_side_condition_turns.is_empty() { println!("  P2 side condition turns: {:?}", state.p2_side_condition_turns); }
        }
    }

    // Slot-specific conditions (only print slots that have conditions)
    let mut any_slot_conds = false;
    for conds in state.p1_slot_conditions.iter().chain(state.p2_slot_conditions.iter()) { if !conds.is_empty() { any_slot_conds = true; break; } }
    if any_slot_conds {
        print_field_header();
        println!("  P1 slot conditions:");
        for (i, conds) in state.p1_slot_conditions.iter().enumerate() {
            if !conds.is_empty() { println!("    Slot {}: {:?}", i+1, conds); }
        }
        println!("  P2 slot conditions:");
        for (i, conds) in state.p2_slot_conditions.iter().enumerate() {
            if !conds.is_empty() { println!("    Slot {}: {:?}", i+1, conds); }
        }
    }

    // Weathers/turns small (only if non-empty)
    if state.weather_turns.is_some() {
        print_field_header();
        println!("  Weather turns: {:?}", state.weather_turns);
    }
    if !state.pseudo_weather_turns.is_empty() {
        print_field_header();
        println!("  Pseudo-weather turns: {:?}\n", state.pseudo_weather_turns);
    }

    // Action queue and turn flags (only print queue if non-empty)
    println!("{}", "Turn & Queue:".yellow().bold());
    println!("  Turn number: {} | Started: {} | Ended: {}", state.turn_number, state.turn_started, state.turn_ended);
    if !state.action_queue.is_empty() {
        println!("  Action queue (len={}): {:?}\n", state.action_queue.len(), state.action_queue);
    }
}

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
        BattleCommand::Struggle { target } => {
            let target_label = target
                .map(|slot| format!("{}'s {}", player_name(slot.player), active_mon_name(state, slot)))
                .unwrap_or_else(|| "no target".to_string());
            format!("Struggle -> {}", target_label)
        }
        BattleCommand::Pass => {
            "Pass".to_string()
        }
    }
}

fn prompt_choice(prompt: &str, options: &[String]) -> usize {
    loop {
        println!("{}", prompt.yellow().bold());
        for (index, option) in options.iter().enumerate() {
            println!("  {}: {}", format!("{}", index + 1).cyan(), option);
        }

        print!("{}", "Choice: ".yellow());
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

        println!("{}", "Invalid choice, try again.".red());
    }
}

pub fn choose_team_preview_command(preview: &crate::state::battle::TeamPreviewState, player: Player) -> TeamPreviewCommand {
    let mons = match player {
        Player::P1 => &preview.p1_mons,
        Player::P2 => &preview.p2_mons,
    };

    let total_mons = mons.len();
    let brought = (preview.brought_per_side as usize).min(total_mons);
    let active_n = (preview.active_per_side as usize).min(brought);

    println!("{}", format!("Choose {} active (front) Pokémon for {}:", active_n, player_name(player)).bright_cyan());
    let mut chosen_active: Vec<usize> = Vec::new();
    while chosen_active.len() < active_n {
        let options: Vec<String> = (0..total_mons)
            .filter(|i| !chosen_active.contains(i))
            .map(|i| species_name(&mons[i].species))
            .collect();

        let prompt = format!("Select active Pokémon {}/{}:", chosen_active.len() + 1, active_n);
        let pick = prompt_choice(&prompt, &options);

        // Map pick back to global index (skipping already-chosen indices)
        let available_indices: Vec<usize> = (0..total_mons).filter(|i| !chosen_active.contains(i)).collect();
        let chosen_index = available_indices[pick];
        chosen_active.push(chosen_index);
        println!("{}", format!("Chosen: {}", species_name(&mons[chosen_index].species)).green());
    }

    // Choose remaining brought (back) slots from the remaining team members
    let mut chosen_brought: Vec<usize> = chosen_active.clone();
    let need_back = brought.saturating_sub(chosen_brought.len());
    if need_back > 0 {
        println!("{}", format!("Choose {} bench (back) Pokémon for {}:", need_back, player_name(player)).bright_cyan());
        while chosen_brought.len() < brought {
            let options: Vec<String> = (0..total_mons)
                .filter(|i| !chosen_brought.contains(i))
                .map(|i| species_name(&mons[i].species))
                .collect();

            let prompt = format!("Select bench Pokémon {}/{}:", chosen_brought.len() - active_n + 1, need_back);
            let pick = prompt_choice(&prompt, &options);

            let available_indices: Vec<usize> = (0..total_mons).filter(|i| !chosen_brought.contains(i)).collect();
            let chosen_index = available_indices[pick];
            chosen_brought.push(chosen_index);
            println!("{}", format!("Chosen bench: {}", species_name(&mons[chosen_index].species)).green());
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

fn choose_replacement_command_for_slot(
    state: &BattleState,
    player: Player,
    slot_idx: usize,
    mon: &PokemonState,
) -> Option<BattleCommand> {
    if mon.fainted {
        let back_mons = match player {
            Player::P1 => &state.p1_back_mons,
            Player::P2 => &state.p2_back_mons,
        };
        let healthy_bench: Vec<usize> = back_mons.iter().enumerate().filter(|(_, m)| !m.fainted).map(|(i, _)| i).collect();
        if healthy_bench.is_empty() {
            println!("{}", format!("No healthy Pokémon available to send out for {} slot {}", player_name(player), slot_idx + 1).red());
            return None;
        }
        let options: Vec<String> = healthy_bench.iter().map(|&idx| back_mon_name(state, player, idx)).collect();
        let mon_label = species_name(&mon.species);
        let choice = prompt_choice(
            &format!("{} {}'s {} {}. Choose a replacement for slot {}:", "[REPLACEMENT]".bright_magenta(), player_name(player), mon_label.bright_red(), "fainted".bright_red(), slot_idx + 1),
            &options,
        );
        Some(BattleCommand::Switch(crate::state::battle::SwitchCommand { party_index: healthy_bench[choice] }))
    } else {
        let mon_label = species_name(&mon.species);
        let _ = prompt_choice(
            &format!("{} {}'s {} has no action needed. Select Pass:", "[REPLACEMENT]".bright_magenta(), player_name(player), mon_label.cyan()),
            &["Pass".to_string()],
        );
        Some(BattleCommand::Pass)
    }
}

fn replacement_commands_are_valid(state: &BattleState, player: Player, active_mons: &[PokemonState], commands: &[BattleCommand]) -> bool {
    commands.iter().enumerate().all(|(i, cmd)| {
        let Some(mon) = active_mons.get(i) else { return false };
        if mon.fainted {
            let BattleCommand::Switch(s) = cmd else { return false };
            let back_mons = match player { Player::P1 => &state.p1_back_mons, Player::P2 => &state.p2_back_mons };
            s.party_index < back_mons.len()
        } else {
            matches!(cmd, BattleCommand::Pass)
        }
    })
}

fn choose_replacement_phase_commands(state: &BattleState, player: Player) -> PlayerCommand {
    let active_mons = match player { Player::P1 => &state.p1_active_mons, Player::P2 => &state.p2_active_mons };
    let mut commands = Vec::new();
    for (slot_idx, mon) in active_mons.iter().enumerate() {
        match choose_replacement_command_for_slot(state, player, slot_idx, mon) {
            Some(cmd) => commands.push(cmd),
            None => return PlayerCommand::Pass,
        }
    }
    if replacement_commands_are_valid(state, player, active_mons, &commands) {
        PlayerCommand::Battle(commands)
    } else {
        PlayerCommand::Pass
    }
}

fn choose_normal_battle_commands(
    state: &BattleState,
    player: Player,
    move_dex: &HashMap<PokemonMove, MoveData>,
    pokemon_dex: &HashMap<Species, PokemonData>,
) -> PlayerCommand {
    let active_len = match player { Player::P1 => state.p1_active_mons.len(), Player::P2 => state.p2_active_mons.len() };
    loop {
        let commands: Vec<BattleCommand> = (0..active_len)
            .map(|slot_idx| choose_battle_command_for_slot(state, player, slot_idx, move_dex, pokemon_dex))
            .collect();
        if simulator::validate_battle_command_combination(&commands) {
            return PlayerCommand::Battle(commands);
        }
        println!("{}", "The combination of choices is not legal; please choose again.".red().bold());
    }
}

/// Build the commands for the self-switch replacement prompt.
/// The player who owns `pending_slot` must choose a healthy bench mon for that slot.
/// All other slots (including all slots of the opposing player) pass.
fn choose_self_switch_commands(state: &BattleState, player: Player, pending_slot: FieldSlot) -> PlayerCommand {
    let active_len = match player {
        Player::P1 => state.p1_active_mons.len(),
        Player::P2 => state.p2_active_mons.len(),
    };
    let commands: Vec<BattleCommand> = (0..active_len).map(|slot_idx| {
        let this_slot = FieldSlot { player, slot_index: slot_idx as u8 };
        if this_slot == pending_slot {
            // Prompt for a replacement from the healthy bench.
            let back_mons = match player {
                Player::P1 => &state.p1_back_mons,
                Player::P2 => &state.p2_back_mons,
            };
            let healthy_bench: Vec<usize> = back_mons.iter().enumerate()
                .filter(|(_, m)| !m.fainted)
                .map(|(i, _)| i)
                .collect();
            if healthy_bench.is_empty() {
                // No target — should not happen because self_switch_pending is cleared in this case,
                // but guard defensively.
                return BattleCommand::Pass;
            }
            let leaving_mon = match player {
                Player::P1 => &state.p1_active_mons,
                Player::P2 => &state.p2_active_mons,
            }.get(slot_idx);
            let leaving_name = leaving_mon.map(|m| species_name(&m.species)).unwrap_or_default();
            let options: Vec<String> = healthy_bench.iter().map(|&idx| back_mon_name(state, player, idx)).collect();
            let choice = prompt_choice(
                &format!(
                    "{} {}'s {} used a self-switch move. Choose a replacement for slot {}:",
                    "[SELF-SWITCH]".bright_magenta(), player_name(player), leaving_name.bright_cyan(), slot_idx + 1
                ),
                &options,
            );
            BattleCommand::Switch(crate::state::battle::SwitchCommand { party_index: healthy_bench[choice] })
        } else {
            BattleCommand::Pass
        }
    }).collect();
    PlayerCommand::Battle(commands)
}

pub fn choose_battle_commands_for_player(
    state: &BattleState,
    player: Player,
    move_dex: &HashMap<PokemonMove, MoveData>,
    pokemon_dex: &HashMap<Species, PokemonData>,
) -> PlayerCommand {
    // Mid-turn self-switch: the pending slot must choose a replacement; all other slots Pass.
    if let Some((pending_slot, _)) = state.self_switch_pending {
        return choose_self_switch_commands(state, player, pending_slot);
    }
    if state.turn_started && state.turn_ended {
        return choose_replacement_phase_commands(state, player);
    }
    choose_normal_battle_commands(state, player, move_dex, pokemon_dex)
}

pub fn simulate_battle(
    mut state: MatchState,
    move_dex: &HashMap<PokemonMove, MoveData>,
    pokemon_dex: &HashMap<Species, PokemonData>,
    consider_crit: bool,
    damage_rolls: u8,
) {
    let mut state_chance = 1.0;

    loop {
        match &state {
            MatchState::TeamPreviewState(preview_state) => {
                if get_verbosity() >= 1 {
                    println!("{}", "Current Team Preview State:".bright_cyan());
                    println!("{}", format!("Selected outcome chance: {:.2}%", state_chance * 100.0).bright_blue());
                    println!("{:#?}", preview_state);
                }

                let p1_cmd = PlayerCommand::TeamPreview(choose_team_preview_command(preview_state, Player::P1));
                let p2_cmd = PlayerCommand::TeamPreview(choose_team_preview_command(preview_state, Player::P2));

                let next_states = simulator::simulate_turn(&state, &p1_cmd, &p2_cmd, move_dex, pokemon_dex, consider_crit, damage_rolls, None);
                if next_states.is_empty() {
                    state = state.clone();
                    state_chance = 1.0;
                    continue;
                }

                let weights = next_states.iter().map(|(_, _, probability)| *probability).collect::<Vec<_>>();
                let distribution = WeightedIndex::new(&weights).expect("At least one positive probability is required");
                let mut rng = thread_rng();
                let selected_index = distribution.sample(&mut rng);
                let (next_state, _, probability) = next_states[selected_index].clone();
                state = next_state;
                state_chance = probability;
            }
            MatchState::BattleState(battle_state) => {
                if get_verbosity() >= 1 {
                    print_battle_state_enhanced(battle_state, state_chance);
                }

                let p1_cmd = choose_battle_commands_for_player(battle_state, Player::P1, move_dex, pokemon_dex);
                let p2_cmd = choose_battle_commands_for_player(battle_state, Player::P2, move_dex, pokemon_dex);

                // At verbosity 4, print all possible outcomes before sampling
                let next_states = simulator::simulate_turn(&state, &p1_cmd, &p2_cmd, move_dex, pokemon_dex, consider_crit, damage_rolls, None);
                if next_states.is_empty() {
                    state = state.clone();
                    state_chance = 1.0;
                    continue;
                }

                // For verbosity 3, suppress damage logs temporarily, then show only the selected outcome
                let prev_verbosity = crate::VERBOSITY.get().copied().unwrap_or(1);

                // Sample from outcomes
                let weights = next_states.iter().map(|(_, _, probability)| *probability).collect::<Vec<_>>();
                let distribution = WeightedIndex::new(&weights).expect("At least one positive probability is required");
                let mut rng = thread_rng();
                let selected_index = distribution.sample(&mut rng);
                let (next_state, _, probability) = next_states[selected_index].clone();
                
                // At verbosity 3, print selected outcome info; at 4+, info is already printed during simulation
                if prev_verbosity >= 3 {
                    println!("{}", format!("(Selected outcome with {:.2}% probability)", probability * 100.0).bright_blue());
                }
                
                state = next_state;
                state_chance = probability;
            }
            MatchState::GameOverState { winner } => {
                println!("{}", format!("Game over. Winner: {:?}", winner).bright_green().bold());
                break;
            }
        }
    }
}