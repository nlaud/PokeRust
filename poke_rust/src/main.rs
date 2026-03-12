use clap::Parser;

mod dex_data;
mod pokemon;

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
    let preview = pokemon::team_preview_state_from_teamsheets(&args.p1, &args.p2, &pokemon_dex, 2);

    if args.verbosity >= 1 {
        println!("P1 team: {} Pokemon | P2 team: {} Pokemon",
            preview.p1_mons.len(), preview.p2_mons.len());
    }

    if args.verbosity >= 3 {
        println!("P1 team: {:#?}", preview.p1_mons);
        println!("P2 team: {:#?}", preview.p2_mons);
    }

}
