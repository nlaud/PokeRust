//! Measures what a depth-2 solve costs, so the budget planner can be calibrated.
//!
//! Run from `poke_rust/`:
//! ```sh
//! cargo bench --bench depth2_cost
//! ```
//!
//! The cost law is `turns = R + R * K * C`.
//! `R` is the root matrix cells that the search evaluated.
//! `K` is the chance successors that each root cell kept.
//! `C` is the child matrix cells for each successor.
//!
//! `K` enters one time, and the action count enters two times.
//! This benchmark reports both parts for singles and for doubles.
//!
//! The action set is complete.
//! No cap and no dominance filter apply.

use std::collections::HashMap;
use std::io::Write;
use std::time::Instant;

use poke_rust::benchmarking::battle_position;
use poke_rust::data::pokemon_move::PokemonMove;
use poke_rust::data::species::Species;
use poke_rust::solver::actions;
use poke_rust::solver::chance::ChanceMode;
use poke_rust::solver::{CancelFlag, SolveConfig, SolverAlgorithm, solve_seeded_cancellable};
use poke_rust::state::battle::{MatchState, Player};
use poke_rust::state::dex_data::{MoveData, PokemonData, parse_move_dex, parse_pokemon_dex};

/// The budget that one solve must fit inside.
///
/// The `competitive` preset gives a job 500,000 turns.
/// PIMC must complete two worlds, so one solve gets one half.
const TARGET_TURNS: u64 = 250_000;

/// A stop for a measurement that runs away.
///
/// It is above [`TARGET_TURNS`], so it never hides a result that fits.
/// A row that reaches it reports a lower bound, not a cost.
const MEASURE_CEILING: u64 = 600_000;

/// Teamsheet pairings for each row.
const PAIRINGS: usize = 4;

/// The cumulative variants that this benchmark compares.
///
/// Each entry is `(label, policy order, turn-cache capacity)`. The list is
/// cumulative, so each row adds one technique to the row above it.
const VARIANTS: [(&str, bool, usize); 3] = [
    ("baseline", false, 0),
    ("policy", true, 0),
    ("policy+cache", true, 8192),
];

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

/// The complete joint-action counts of both players at one position.
fn action_counts(
    state: &MatchState,
    pokemon_dex: &HashMap<Species, PokemonData>,
    move_dex: &HashMap<PokemonMove, MoveData>,
) -> (usize, usize) {
    let MatchState::BattleState(battle) = state else {
        return (0, 0);
    };
    let phase = actions::phase_of(state);
    let count = |player| {
        actions::joint_actions(battle, player, phase, move_dex, pokemon_dex, None, false)
            .actions
            .len()
    };
    (count(Player::P1), count(Player::P2))
}

struct Row {
    scenario: &'static str,
    depth: u8,
    chance: &'static str,
    variant: &'static str,
    pairings: usize,
    actions_p1: f64,
    actions_p2: f64,
    turns: f64,
    cells_evaluated: f64,
    cells_total: f64,
    cutoffs: f64,
    tt_hits: f64,
    secs: f64,
    over_ceiling: usize,
}

fn fmt(value: f64) -> String {
    if value >= 1e6 {
        format!("{:.1}M", value / 1e6)
    } else if value >= 1e3 {
        format!("{:.1}k", value / 1e3)
    } else {
        format!("{value:.0}")
    }
}

fn main() {
    poke_rust::VERBOSITY.set(0).ok();
    let (pokemon_dex, move_dex) = dexes();
    let paths = teamsheet_paths();
    assert!(!paths.is_empty(), "no teamsheets found");

    // `(label, active slots, brought)` — the grid that `run_solver` sweeps.
    let scenarios: [(&'static str, u8, u8); 2] = [("singles", 1, 3), ("doubles", 2, 4)];
    let chances: [(&'static str, ChanceMode); 3] = [
        ("enumerate", ChanceMode::Enumerate),
        ("top4", ChanceMode::TopK(4)),
        ("top1", ChanceMode::TopK(1)),
    ];

    // The action count decides the cost, and it needs no solve. Report it
    // first, so the expensive rows below start from a known matrix size.
    println!("=== complete joint-action counts, no cap and no dominance filter ===");
    for (scenario, active, brought) in scenarios {
        for index in 0..PAIRINGS {
            let i = (index * 5) % paths.len();
            let j = (index * 7 + 3) % paths.len();
            let seed = (i * paths.len() + j) as u64;
            let Some(state) =
                battle_position(&paths, i, j, active, brought, seed, &pokemon_dex, &move_dex)
            else {
                continue;
            };
            let (n1, n2) = action_counts(&state, &pokemon_dex, &move_dex);
            println!(
                "  {scenario:<8} pair {i:>2}x{j:<2} actions {n1:>5} x {n2:<5} matrix {:>10}",
                fmt((n1 * n2) as f64)
            );
        }
    }
    std::io::stdout().flush().ok();
    println!();

    let mut rows: Vec<Row> = Vec::new();

    for (scenario, active, brought) in scenarios {
        for depth in [1u8, 2u8] {
            for (chance_label, chance) in chances {
              for (variant, policy, cache) in VARIANTS {
                // Depth 1 scores every successor statically, so no chance node
                // reaches a reducing mode. The three modes would print one
                // number three times.
                if depth == 1 && chance_label != "enumerate" {
                    continue;
                }

                let config = SolveConfig {
                    depth,
                    iterative_deepening: true,
                    damage_rolls: 1,
                    consider_crit: false,
                    chance,
                    algorithm: SolverAlgorithm::DoubleOracle,
                    policy_order: policy,
                    turn_cache_capacity: cache,
                    // The two techniques that this work leaves off.
                    max_actions_per_player: None,
                    prune_dominated_actions: false,
                    node_budget: None,
                    deadline: None,
                    ..SolveConfig::default()
                };

                let mut row = Row {
                    scenario,
                    depth,
                    chance: chance_label,
                    variant,
                    pairings: 0,
                    actions_p1: 0.0,
                    actions_p2: 0.0,
                    turns: 0.0,
                    cells_evaluated: 0.0,
                    cells_total: 0.0,
                    cutoffs: 0.0,
                    tt_hits: 0.0,
                    secs: 0.0,
                    over_ceiling: 0,
                };

                for index in 0..PAIRINGS {
                    let i = (index * 5) % paths.len();
                    let j = (index * 7 + 3) % paths.len();
                    let seed = (i * paths.len() + j) as u64;
                    let Some(state) =
                        battle_position(&paths, i, j, active, brought, seed, &pokemon_dex, &move_dex)
                    else {
                        continue;
                    };

                    let (n1, n2) = action_counts(&state, &pokemon_dex, &move_dex);

                    let cancel = CancelFlag::with_simulation_turn_budget(MEASURE_CEILING);
                    let start = Instant::now();
                    let Ok(result) = solve_seeded_cancellable(
                        seed,
                        &state,
                        &pokemon_dex,
                        &move_dex,
                        &config,
                        Some(&cancel),
                    ) else {
                        continue;
                    };
                    let elapsed = start.elapsed().as_secs_f64();
                    let stopped = cancel.simulation_budget_hit();

                    row.pairings += 1;
                    row.actions_p1 += n1 as f64;
                    row.actions_p2 += n2 as f64;
                    row.turns += result.stats.turns_simulated as f64;
                    row.cells_evaluated += result.stats.matrix_cells_evaluated as f64;
                    row.cells_total += result.stats.matrix_cells_total as f64;
                    row.cutoffs += result.stats.ab_cutoffs as f64;
                    row.tt_hits += result.stats.tt_hits as f64;
                    row.secs += elapsed;
                    if stopped {
                        row.over_ceiling += 1;
                    }

                    println!(
                        "  {scenario:<8} d{depth} {chance_label:<9} {variant:<13} pair {i:>2}x{j:<2} actions {n1:>4}x{n2:<4} turns {:>8} cells {:>8} of {:<10} reached d{} {}",
                        fmt(result.stats.turns_simulated as f64),
                        fmt(result.stats.matrix_cells_evaluated as f64),
                        fmt(result.stats.matrix_cells_total as f64),
                        result.depth_reached,
                        if stopped { "CEILING" } else { "" },
                    );
                    // `cargo bench` pipes stdout, which makes it block
                    // buffered. A doubles row runs for minutes, so an
                    // unflushed line would not appear until the run ended.
                    std::io::stdout().flush().ok();
                }

                rows.push(row);
              }
            }
        }
    }

    println!();
    println!(
        "{:<8} {:>5} {:<9} {:<13} {:>5} {:>12} {:>9} {:>9} {:>10} {:>8} {:>8} {:>9} {:>8}",
        "scenario",
        "depth",
        "chance",
        "variant",
        "pairs",
        "actions",
        "turns",
        "cells",
        "total",
        "cutoffs",
        "tt_hits",
        "time",
        "vs 250k"
    );
    println!("{}", "-".repeat(122));
    for row in &rows {
        if row.pairings == 0 {
            println!(
                "{:<8} {:>5} {:<9} {:<13} {:>5}  no pairing produced a battle position",
                row.scenario, row.depth, row.chance, row.variant, 0
            );
            continue;
        }
        let n = row.pairings as f64;
        let turns = row.turns / n;
        println!(
            "{:<8} {:>5} {:<9} {:>6} {:>5} {:>5.0} x{:<5.0} {:>9} {:>9} {:>10} {:>8} {:>8} {:>8.2}s {:>7.1}x{}",
            row.scenario,
            row.depth,
            row.chance,
            row.variant,
            row.pairings,
            row.actions_p1 / n,
            row.actions_p2 / n,
            fmt(turns),
            fmt(row.cells_evaluated / n),
            fmt(row.cells_total / n),
            fmt(row.cutoffs / n),
            fmt(row.tt_hits / n),
            row.secs / n,
            turns / TARGET_TURNS as f64,
            if row.over_ceiling > 0 {
                format!("  ({} hit the ceiling)", row.over_ceiling)
            } else {
                String::new()
            },
        );
    }
    println!();
    println!("A `vs 250k` value below 1.0 means one solve fits the PIMC two-world rule at a 500k job budget.");
}
