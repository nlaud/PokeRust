use clap::Parser;
use battle::MatchState;

mod dex_data;
mod data;
mod pokemon;
mod battle;
mod simulator;

use std::io::Write;

#[derive(Parser, Debug)]
#[command(author = "Blazestorm", version = "1.0", about = "Simulates Pokemon Battles")]
struct Args {
    /// Path to the first player's teamsheet
    #[arg(long)]
    p1: String,

    /// Path to the second player's teamsheet
    #[arg(long)]
    p2: String,

    /// Path to the showdown pokedex data
    #[arg(long, default_value="../pokemon_info/showdownDex.txt")]
    poke_dex: String,

    /// Path to the showdown move data
    #[arg(long, default_value="../pokemon_info/showdownMoves.txt")]
    move_dex: String,

    /// How verbose debug output is (0 = Nothing, 1 = Minimal, 2 = Debug Trace, 3 = High Debug, 4 = Max Debug)
    #[arg(short, long, default_value_t = 1)]
    verbosity: u8,
}

fn main() {
    let args = Args::parse();

    if args.verbosity >= 2 { println!("Got paths: {}, {}", args.p1, args.p2) }

    //Put dex data into a hashmap
    let pokemon_dex = dex_data::parse_pokemon_dex(&args.poke_dex);

    //Put move data into a hashmap
    let move_dex = dex_data::parse_move_dex(&args.move_dex);

    if args.verbosity >= 1 {
        println!("Loaded {} Pokemon and {} moves", pokemon_dex.len(), move_dex.len());
    }

    if args.verbosity >= 4 {
        println!("Pokemon Dex: {:#?}", pokemon_dex);
        println!("Move Dex: {:#?}", move_dex);
    }

    //Parse teamsheets
    let preview = simulator::team_preview_state_from_teamsheets(&args.p1, &args.p2, &pokemon_dex, &move_dex, 2, 4);

    if args.verbosity >= 1 {
        println!("P1 team: {} Pokemon | P2 team: {} Pokemon",
            preview.p1_mons.len(), preview.p2_mons.len());
    }

    if args.verbosity >= 3 {
        println!("P1 team: {:#?}", preview.p1_mons);
        println!("P2 team: {:#?}", preview.p2_mons);
    }
    
    let mut state = MatchState::TeamPreviewState(preview);
    if args.verbosity >= 1 {
        let (p1_cmds, p2_cmds) = simulator::get_possible_commands(&state, &move_dex, &pokemon_dex);
        
        println!("--- P1 Possible Team Preview Commands ---");
        for (i, cmd) in p1_cmds.iter().enumerate() {
            println!("{}: {:?}", i, cmd);
        }
        
        println!("\n--- P2 Possible Team Preview Commands ---");
        for (i, cmd) in p2_cmds.iter().enumerate() {
            println!("{}: {:?}", i, cmd);
        }
        
        let mut p1_input = String::new();
        let mut p2_input = String::new();
        
        print!("\nEnter P1 command index: ");
        std::io::stdout().flush().unwrap();
        std::io::stdin().read_line(&mut p1_input).unwrap();
        let p1_idx: usize = p1_input.trim().parse().unwrap_or(0);
        
        print!("Enter P2 command index: ");
        std::io::stdout().flush().unwrap();
        std::io::stdin().read_line(&mut p2_input).unwrap();
        let p2_idx: usize = p2_input.trim().parse().unwrap_or(0);
        
        println!("\nApplying commands P1: {}, P2: {}", p1_idx, p2_idx);
        let next_states = simulator::apply_player_commands(&state, &p1_cmds[p1_idx], &p2_cmds[p2_idx]);
        
        if let Some((next_state, _prob)) = next_states.into_iter().next() {
            state = next_state;
            if let MatchState::BattleState(ref battle) = state {
                println!("\nNew Battle State:\n{}", battle);
                
                let (new_p1_cmds, new_p2_cmds) = simulator::get_possible_commands(&state, &move_dex, &pokemon_dex);
                println!("--- Next Possible Battle Commands ---");
                println!("P1 possible commands count: {}", new_p1_cmds.len());
                println!("P2 possible commands count: {}", new_p2_cmds.len());
                if args.verbosity >= 3 {
                    println!("P1 commands: {:#?}", new_p1_cmds);
                    println!("P2 commands: {:#?}", new_p2_cmds);
                }
            }
        }
    }
}