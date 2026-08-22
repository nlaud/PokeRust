//! Measures one doubles depth-2 solve to the end.
//!
//! Run from `poke_rust/`:
//! ```sh
//! cargo bench --bench doubles_probe
//! ```
//!
//! `depth2_cost` stops a runaway row at a ceiling, so it reports a lower bound
//! for doubles rather than a cost. This benchmark takes the cheapest doubles
//! pairing and lets one solve finish.
//!
//! The cost law predicts `R * K * C` for depth 2, where `R` and `C` are the
//! matrix cells of the root and of one child. That prediction assumes that no
//! two children share a position. The transposition table shares them, so only a
//! finished solve says what the search really costs.

use std::collections::HashMap;
use std::io::Write;
use std::time::Instant;

use poke_rust::benchmarking::battle_position;
use poke_rust::data::pokemon_move::PokemonMove;
use poke_rust::data::species::Species;
use poke_rust::solver::chance::ChanceMode;
use poke_rust::solver::{CancelFlag, SolveConfig, SolverAlgorithm, pool, solve_seeded_cancellable};
use poke_rust::state::dex_data::{MoveData, PokemonData, parse_move_dex, parse_pokemon_dex};

/// A stop for a solve that runs away.
///
/// One turn resolution costs a few hundred microseconds, so this is minutes of
/// wall clock even across the worker pool.
const CEILING: u64 = 40_000_000;

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

fn main() {
    poke_rust::VERBOSITY.set(0).ok();
    let (pokemon_dex, move_dex) = dexes();
    let paths = teamsheet_paths();

    // The cheapest doubles pairing that `depth2_cost` reports, at 290 x 370.
    let (i, j) = (10usize, 3usize);
    let seed = (i * paths.len() + j) as u64;
    let state = battle_position(&paths, i, j, 2, 4, seed, &pokemon_dex, &move_dex)
        .expect("the pairing must produce a battle position");

    let workers = pool::shared().capacity();
    println!("doubles depth-2 probe, pairing {i}x{j}, {workers} workers");
    println!("{:<10} {:>12} {:>12} {:>12} {:>10}", "chance", "turns", "cells", "tt_hits", "time");
    std::io::stdout().flush().ok();

    for (label, chance) in [
        ("top1", ChanceMode::TopK(1)),
        ("top4", ChanceMode::TopK(4)),
        ("enumerate", ChanceMode::Enumerate),
    ] {
        let config = SolveConfig {
            depth: 2,
            iterative_deepening: true,
            damage_rolls: 1,
            consider_crit: false,
            chance,
            algorithm: SolverAlgorithm::DoubleOracle,
            policy_order: true,
            max_actions_per_player: None,
            prune_dominated_actions: false,
            node_budget: None,
            deadline: None,
            workers,
            ..SolveConfig::default()
        };

        let cancel = CancelFlag::with_simulation_turn_budget(CEILING);
        let start = Instant::now();
        let result = solve_seeded_cancellable(
            seed,
            &state,
            &pokemon_dex,
            &move_dex,
            &config,
            Some(&cancel),
        )
        .expect("the position is solvable");
        let elapsed = start.elapsed().as_secs_f64();

        println!(
            "{label:<10} {:>12} {:>12} {:>12} {:>9.1}s  reached d{}{}",
            result.stats.turns_simulated,
            result.stats.matrix_cells_evaluated,
            result.stats.tt_hits,
            elapsed,
            result.depth_reached,
            if cancel.simulation_budget_hit() {
                "  CEILING"
            } else {
                ""
            },
        );
        std::io::stdout().flush().ok();
    }
}
