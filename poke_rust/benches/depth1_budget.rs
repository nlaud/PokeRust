//! Measures what a depth-1 solve costs, and how damage rolls change it.
//!
//! Run from `poke_rust/`:
//! ```sh
//! cargo bench --bench depth1_budget
//! ```
//!
//! The server presets set a turn budget. This benchmark supplies the numbers
//! that size it. It reports one depth-1 solve of a singles position and of a
//! doubles position, over a range of damage-roll counts.
//!
//! The two questions it answers:
//!
//! 1. Does a damage-roll count change the turn count of a depth-1 solve? A
//!    matrix cell calls `simulate_turn` one time, and the rolls change the
//!    branches inside that one call.
//! 2. What does a depth-1 solve cost in seconds? A preset budget must let the
//!    slowest position finish, and a doubles position is the slowest.

use std::collections::HashMap;
use std::io::Write;
use std::time::Instant;

use poke_rust::benchmarking::battle_position;
use poke_rust::data::pokemon_move::PokemonMove;
use poke_rust::data::species::Species;
use poke_rust::solver::chance::ChanceMode;
use poke_rust::solver::mcts::{self, MctsConfig};
use poke_rust::solver::{
    CancelFlag, JointActionProb, SolveConfig, SolverAlgorithm, pool, solve_seeded_cancellable,
};
use poke_rust::state::battle::{MatchState, Player};
use poke_rust::state::dex_data::{MoveData, PokemonData, parse_move_dex, parse_pokemon_dex};

/// The damage-roll counts that the sweep reports.
const ROLLS: [u8; 7] = [1, 2, 3, 4, 6, 8, 16];

fn dexes() -> (
    HashMap<Species, PokemonData>,
    HashMap<PokemonMove, MoveData>,
) {
    (
        parse_pokemon_dex("../pokemon_info/showdownDex.txt"),
        parse_move_dex("../pokemon_info/showdownMoves.txt"),
    )
}

fn teamsheet_paths() -> Vec<std::path::PathBuf> {
    let mut paths: Vec<std::path::PathBuf> = std::fs::read_dir("../teamsheets")
        .expect("../teamsheets should exist")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "txt"))
        .collect();
    paths.sort();
    paths
}

fn action_count(
    state: &MatchState,
    player: Player,
    pokemon_dex: &HashMap<Species, PokemonData>,
    move_dex: &HashMap<PokemonMove, MoveData>,
) -> usize {
    use poke_rust::solver::actions;
    let MatchState::BattleState(battle) = state else {
        return 0;
    };
    actions::joint_actions(
        battle,
        player,
        actions::phase_of(state),
        move_dex,
        pokemon_dex,
        None,
        false,
    )
    .actions
    .len()
}

fn support(strategy: &[JointActionProb]) -> usize {
    strategy
        .iter()
        .filter(|action| action.probability > 1e-9)
        .count()
}

/// The configuration that `bin/server/bot.rs::build_search` builds, at depth 1.
fn server_config(damage_rolls: u8, workers: usize) -> SolveConfig {
    SolveConfig {
        depth: 1,
        replacement_depth: None,
        iterative_deepening: false,
        damage_rolls,
        consider_crit: false,
        chance: ChanceMode::Enumerate,
        algorithm: SolverAlgorithm::DoubleOracle,
        policy_order: true,
        max_actions_per_player: None,
        prune_dominated_actions: false,
        node_budget: None,
        deadline: None,
        workers,
        ..SolveConfig::default()
    }
}

fn sweep(
    name: &str,
    state: &MatchState,
    seed: u64,
    pokemon_dex: &HashMap<Species, PokemonData>,
    move_dex: &HashMap<PokemonMove, MoveData>,
    workers: usize,
) {
    let n1 = action_count(state, Player::P1, pokemon_dex, move_dex);
    let n2 = action_count(state, Player::P2, pokemon_dex, move_dex);
    println!("## {name}: {n1} actions against {n2}");
    println!();
    println!(
        "| Damage rolls | Turns | Nodes | Cells | Turns for each cell | Time | Support | Value |"
    );
    println!("|---|---|---|---|---|---|---|---|");
    for rolls in ROLLS {
        let config = server_config(rolls, workers);
        let start = Instant::now();
        let result = solve_seeded_cancellable(seed, state, pokemon_dex, move_dex, &config, None)
            .expect("the position is solvable");
        let elapsed = start.elapsed().as_secs_f64();
        println!(
            "| {rolls} | {} | {} | {} | {:.1} | {:.2}s | {} of {n1}, {} of {n2} | {:.4} |",
            result.stats.turns_simulated,
            result.stats.nodes_expanded,
            result.stats.matrix_cells_evaluated,
            result.stats.turns_simulated as f64
                / result.stats.matrix_cells_evaluated.max(1) as f64,
            elapsed,
            support(&result.p1_strategy),
            support(&result.p2_strategy),
            result.value,
        );
        std::io::stdout().flush().ok();
    }
    println!();
}

/// Measures the turn rate of a sampled search.
///
/// A sampled search spends whatever budget it has, so a budget does not decide
/// whether it finishes. It decides how many seconds the answer takes. This
/// reports the rate that converts one into the other.
///
/// `mcts` reads the true position, and `ismcts` runs the same iteration over a
/// drawn world. The rate is therefore the rate of both, and `ismcts` adds one
/// determinization for each particle.
fn sampled_rate(
    state: &MatchState,
    seed: u64,
    pokemon_dex: &HashMap<Species, PokemonData>,
    move_dex: &HashMap<PokemonMove, MoveData>,
) {
    println!("### Sampled search at depth 1");
    println!();
    println!("| Damage rolls | Budget | Turns | Iterations | Time | Turns for each second |");
    println!("|---|---|---|---|---|---|");
    for (rolls, budget) in [(1u8, 200_000u64), (4, 200_000), (16, 200_000)] {
        let config = MctsConfig {
            iterations: u32::MAX,
            depth: 1,
            replacement_depth: None,
            damage_rolls: rolls,
            consider_crit: false,
            max_actions_per_player: None,
            ..MctsConfig::default()
        };
        let flag = CancelFlag::with_simulation_turn_budget(budget);
        let start = Instant::now();
        let result =
            mcts::search_cancellable(seed, state, pokemon_dex, move_dex, &config, Some(&flag));
        let elapsed = start.elapsed().as_secs_f64();
        match result {
            Ok(found) => println!(
                "| {rolls} | {budget} | {} | {} | {:.2}s | {:.0} |",
                found.stats.turns_simulated,
                found.stats.iterations,
                elapsed,
                found.stats.turns_simulated as f64 / elapsed.max(1e-9),
            ),
            Err(error) => println!("| {rolls} | {budget} | error: {error:?} | | | |"),
        }
        std::io::stdout().flush().ok();
    }
    println!();
}

fn main() {
    poke_rust::VERBOSITY.set(0).ok();
    let (pokemon_dex, move_dex) = dexes();
    let paths = teamsheet_paths();
    let workers = pool::shared().capacity();

    println!("depth-1 budget probe, {workers} workers");
    println!();

    // Singles first. The pairing is the one that `depth2_cost` reports.
    for (i, j, active, brought, label) in [
        (0usize, 1usize, 1u8, 3u8, "Singles"),
        (10usize, 3usize, 2u8, 4u8, "Doubles"),
    ] {
        let seed = (i * paths.len() + j) as u64;
        let Some(state) = battle_position(
            &paths,
            i,
            j,
            active,
            brought,
            seed,
            &pokemon_dex,
            &move_dex,
        ) else {
            println!("{label}: the pairing did not produce a battle position");
            continue;
        };
        sweep(label, &state, seed, &pokemon_dex, &move_dex, workers);
        if active == 2 {
            sampled_rate(&state, seed, &pokemon_dex, &move_dex);
        }
    }
}
