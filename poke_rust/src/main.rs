use clap::Parser;
use colored::Colorize;
use battle::MatchState;
use std::sync::OnceLock;
use crate::data::pokemon_move::PokemonMove;

mod dex_data;
mod data;
mod pokemon;
mod battle;
mod simulator;
mod simulator_helpers;
mod user;
mod simulator_tests;

pub static VERBOSITY: OnceLock<u8> = OnceLock::new();

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

    /// Disable critical hits while simulating damage
    #[arg(long, action = clap::ArgAction::SetTrue)]
    no_consider_crit: bool,

    /// How many damage rolls to consider per hit, from 1 to 16
    #[arg(long, default_value_t = 16, value_parser = clap::value_parser!(u8).range(1..=16))]
    damage_rolls: u8,

    /// Use stat points format instead of EVs (applies formula: EV' = ((n-4)/8)+1)
    #[arg(long, default_value_t = true, action = clap::ArgAction::SetFalse)]
    stat_points: bool,
}

fn main() {
    let args = Args::parse();

    // Initialize global verbosity
    let _ = VERBOSITY.set(args.verbosity);

    if args.verbosity >= 2 { println!("{}", format!("Got paths: {}, {}", args.p1, args.p2).cyan()) }

    //Put dex data into a hashmap
    let pokemon_dex = dex_data::parse_pokemon_dex(&args.poke_dex);

    //Put move data into a hashmap
    let move_dex = dex_data::parse_move_dex(&args.move_dex);

    // Print info about a sample move (before printing teamsheets)
    
    let sample_move = PokemonMove::Roost;
    if let Some(m) = move_dex.get(&sample_move) {
        println!("Data:{:#?}", m)
    }
    

    if args.verbosity >= 1 {
        println!("{}", format!("Loaded {} Pokemon and {} moves", pokemon_dex.len(), move_dex.len()).bright_green());
    }

    if args.verbosity >= 4 {
        println!("Pokemon Dex: {:#?}", pokemon_dex);
        println!("Move Dex: {:#?}", move_dex);
    }

    //Parse teamsheets
    let preview = simulator::team_preview_state_from_teamsheets(&args.p1, &args.p2, &pokemon_dex, &move_dex, 2, 4, args.stat_points);

    if args.verbosity >= 1 {
        println!("{}", format!("P1 team: {} Pokemon | P2 team: {} Pokemon", preview.p1_mons.len(), preview.p2_mons.len()).bright_cyan());
    }

    if args.verbosity >= 3 {
        println!("P1 team: {:#?}", preview.p1_mons);
        println!("P2 team: {:#?}", preview.p2_mons);
    }

    user::simulate_battle(
        MatchState::TeamPreviewState(preview),
        &move_dex,
        &pokemon_dex,
        !args.no_consider_crit,
        args.damage_rolls,
    );
}