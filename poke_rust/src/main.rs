use clap::Parser;

#[derive(Parser, Debug)]
#[command(author = "Blazestorm", version = "1.0", about = "Simulates Pokemon Battles")]
struct Args {
    /// Path to the first player's teamsheet
    #[arg(long)]
    p1: String,

    /// Path to the second player's teamsheet
    #[arg(long)]
    p2: String,

    /// How verbose debug output is (0 = Nothing, 1 = Minimal, 2 = Debug Trace)
    #[arg(short, long, default_value_t = 1)]
    verbosity: u8,
}

fn main() {
    let args = Args::parse();

    if args.verbosity >= 2 { println!("Got paths: {}, {}", args.p1, args.p2) }

    //Put dex data into a hashmap
    
    //Put move data into a hashmap

    //Parse teamsheets into teams
}
