//! Measures the three simultaneous-move solver algorithms.
//! It varies search depth, damage rolls, chance mode, and battle format.
//! It uses the teamsheets in `../teamsheets/`.
//!
//! Run from `poke_rust/`:
//! ```sh
//! cargo bench --bench solver_speed
//! ```
//!
//! `poke_rust::benchmarking::run_solver` implements the test.
//! The web endpoint uses the same function.
//! This file prints the result table.
//!
//! ## Reading the table
//!
//! `turns` counts the expensive `simulate_turn` calls.
//! `lps` counts the faster matrix linear programs.
//! `cells` counts evaluated cells, and `total` counts all cells.
//! Backward induction evaluates every cell.
//!
//! ## Omitted tests
//!
//! The search tree grows by `(actions² · branches)` at each depth.
//! The benchmark omits a test above `MAX_ESTIMATED_TURNS`.
//! Other tests use a fixed turn budget.
//! The `pairs` column gives the number of tested teamsheet pairs.
//! A time limit stops a test when the estimate is too low.
//!
//! Doubles uses one depth and a limited joint-command set.
//! The `cap` column gives this command limit.
//! These results measure cost, not strategy quality.
//!
//! ## What reproduces, and what does not
//!
//! Outside `ChanceMode::Sample`, repeated solves match within one process.
//!
//! Work counts can vary by about one percent between processes.
//! `coalesce_branches` reads a `HashMap` in an unstable order.
//! Floating-point addition can then change close successor probabilities.
//! This change can alter the search order.
//! Reported values match exactly or differ by one unit in the last place.
//!
//! Treat small `turns` and `cells` changes as noise.
//! Time can change freely.
//! Compare rows only when they use the same number of pairs.
//!
//! Depth-first search releases each subtree before it expands the next successor.
//! Live memory therefore depends on depth and branching.
//! `SolveConfig::default` also limits the node count.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use poke_rust::benchmarking::{SolverRow, run_solver};
use poke_rust::simulator::{simulate_turn, team_preview_state_from_teamsheets};
use poke_rust::solver::chance::ChanceMode;
use poke_rust::solver::eval::{self, EvalContext};
use poke_rust::solver::{SolveConfig, solve};
use poke_rust::state::battle::{MatchState, Player, PlayerCommand, TeamPreviewCommand};
use poke_rust::state::dex_data::{parse_move_dex, parse_pokemon_dex};
use poke_rust::user::battle_command_description;

fn fmt_time(seconds: f64) -> String {
    if seconds < 1e-3 {
        format!("{:.0}us", seconds * 1e6)
    } else if seconds < 1.0 {
        format!("{:.2}ms", seconds * 1e3)
    } else {
        format!("{seconds:.2}s")
    }
}

fn fmt_count(value: f64) -> String {
    if value >= 1e6 {
        format!("{:.1}M", value / 1e6)
    } else if value >= 1e3 {
        format!("{:.1}k", value / 1e3)
    } else {
        format!("{value:.0}")
    }
}

fn print_table(rows: &[SolverRow]) {
    println!(
        "{:<8} {:<18} {:>5} {:>5} {:<9} {:>4} {:>5} {:>9} {:>8} {:>8} {:>6} {:>6}",
        "scenario",
        "algorithm",
        "depth",
        "rolls",
        "chance",
        "cap",
        "pairs",
        "time",
        "turns",
        "cells",
        "total",
        "lps"
    );
    println!("{}", "-".repeat(108));

    for row in rows {
        let cap = row
            .action_cap
            .map(|c| c.to_string())
            .unwrap_or_else(|| "-".to_string());

        if let Some(reason) = row.skipped {
            println!(
                "{:<8} {:<18} {:>5} {:>5} {:<9} {:>4} {:>5} {:>9}  skipped: {reason}",
                row.scenario,
                row.algorithm,
                row.depth,
                row.rolls,
                row.chance,
                cap,
                "-",
                "-"
            );
            continue;
        }

        println!(
            "{:<8} {:<18} {:>5} {:>5} {:<9} {:>4} {:>5} {:>9} {:>8} {:>8} {:>6} {:>6}",
            row.scenario,
            row.algorithm,
            row.depth,
            row.rolls,
            row.chance,
            cap,
            row.pairings,
            fmt_time(row.avg_time_secs),
            fmt_count(row.avg_turns_simulated),
            fmt_count(row.avg_cells_evaluated),
            fmt_count(row.avg_cells_total),
            fmt_count(row.avg_lps),
        );
    }
}

/// Backward induction versus double oracle at matched settings — the question
/// the whole sweep exists to answer, pulled out of the grid so it does not have
/// to be reconstructed by eye.
fn print_pruning_summary(rows: &[SolverRow]) {
    println!();
    println!("Pruning payoff (double oracle vs backward induction, matched settings)");
    println!(
        "{:<8} {:>5} {:>5} {:<9} {:>12} {:>12} {:>8}",
        "scenario", "depth", "rolls", "chance", "BI turns", "DO turns", "speedup"
    );
    println!("{}", "-".repeat(66));

    let comparable = |row: &&SolverRow, algorithm: &str| {
        row.algorithm == algorithm && row.skipped.is_none() && row.pairings > 0
    };

    for baseline in rows.iter().filter(|r| comparable(r, "backwardInduction")) {
        let Some(pruned) = rows.iter().find(|r| {
            comparable(r, "doubleOracle")
                && r.scenario == baseline.scenario
                && r.depth == baseline.depth
                && r.rolls == baseline.rolls
                && r.chance == baseline.chance
        }) else {
            continue;
        };

        let speedup = if pruned.avg_turns_simulated > 0.0 {
            baseline.avg_turns_simulated / pruned.avg_turns_simulated
        } else {
            f64::NAN
        };
        println!(
            "{:<8} {:>5} {:>5} {:<9} {:>12} {:>12} {:>7.2}x",
            baseline.scenario,
            baseline.depth,
            baseline.rolls,
            baseline.chance,
            fmt_count(baseline.avg_turns_simulated),
            fmt_count(pruned.avg_turns_simulated),
            speedup,
        );
    }
}

/// The teamsheet pair that the sample position and the leaf-cost measurement
/// both use.
const SAMPLE_SHEETS: (&str, &str) = (
    "../teamsheets/MA_dragonite_rain.txt",
    "../teamsheets/MB_gyarados_volcarona.txt",
);

/// Resolves one fixed teamsheet matchup into its first battle position.
///
/// The leads are fixed and the most probable branch wins the draw, so the
/// position is the same on every run.
fn sample_position(
    pokemon_dex: &std::collections::HashMap<
        poke_rust::data::species::Species,
        poke_rust::state::dex_data::PokemonData,
    >,
    move_dex: &std::collections::HashMap<
        poke_rust::data::pokemon_move::PokemonMove,
        poke_rust::state::dex_data::MoveData,
    >,
) -> Option<MatchState> {
    let (p1_sheet, p2_sheet) = SAMPLE_SHEETS;
    let preview = team_preview_state_from_teamsheets(
        p1_sheet, p2_sheet, pokemon_dex, move_dex, 1, 3, true,
    );
    let picks = || {
        PlayerCommand::TeamPreview(TeamPreviewCommand {
            active_indices: vec![0],
            back_indices: vec![1, 2],
        })
    };

    simulate_turn(
        &MatchState::TeamPreviewState(preview),
        &picks(),
        &picks(),
        move_dex,
        pokemon_dex,
        false,
        1,
        None,
    )
    .into_iter()
    .map(|(state, _, probability)| {
        let mut hasher = DefaultHasher::new();
        state.hash(&mut hasher);
        (hasher.finish(), state, probability)
    })
    .max_by(|a, b| a.2.total_cmp(&b.2).then_with(|| b.0.cmp(&a.0)))
    .map(|(_, state, _)| state)
}

/// The weights of the evaluator before the matchup features existed.
///
/// Health, status, boosts, and hazards keep their weights, and every matchup
/// weight is zero.
///
/// This vector answers the search-shape question. A more discriminating
/// evaluator gives double oracle different best responses, so the two runs
/// differ in how many cells they reach. `even` cannot answer that question,
/// because a constant evaluator makes every cell equal and the oracle stops at
/// its first pair.
///
/// It does not answer the leaf-cost question. [`features`] runs whatever the
/// weights are, so this vector costs one full feature vector like the others.
///
/// [`features`]: poke_rust::solver::eval::features
fn legacy_weights() -> poke_rust::solver::eval::Features {
    let mut weights = eval::HAND_WEIGHTS;
    for value in weights.iter_mut().skip(5) {
        *value = 0.0;
    }
    weights
}

/// Scores a position as the evaluator did before the matchup features existed.
fn legacy(
    state: &poke_rust::state::battle::BattleState,
    ctx: &EvalContext<'_>,
) -> f64 {
    eval::score_with(state, ctx, &legacy_weights())
}

/// Time one leaf evaluation of each shipped weight vector.
///
/// Search cost comes mainly from `simulate_turn`, so a leaf has to stay far
/// below one turn resolution. The threat features cost damage calculations, and
/// this row is what says how much.
fn print_leaf_cost(
    pokemon_dex: &std::collections::HashMap<
        poke_rust::data::species::Species,
        poke_rust::state::dex_data::PokemonData,
    >,
    move_dex: &std::collections::HashMap<
        poke_rust::data::pokemon_move::PokemonMove,
        poke_rust::state::dex_data::MoveData,
    >,
) {
    let Some(MatchState::BattleState(battle)) = sample_position(pokemon_dex, move_dex) else {
        eprintln!("leaf cost: preview did not resolve into a battle");
        return;
    };
    let ctx = EvalContext::new(pokemon_dex, move_dex);
    let repeats = 20_000;

    println!();
    println!("Leaf evaluation cost — one call, averaged over {repeats} calls");
    for (name, evaluator) in [
        // `even` computes no feature, so it is the floor that the feature frame
        // is measured against. The evaluator before this frame read no move
        // data, and it sat near this floor.
        ("even", eval::even as poke_rust::solver::eval::LeafEvaluator),
        ("legacy", legacy as poke_rust::solver::eval::LeafEvaluator),
        ("heuristic", eval::heuristic as poke_rust::solver::eval::LeafEvaluator),
        ("fitted", eval::fitted as poke_rust::solver::eval::LeafEvaluator),
    ] {
        // One untimed call warms the weight cache and the branch predictor.
        let mut sink = evaluator(&battle, &ctx);
        let started = std::time::Instant::now();
        for _ in 0..repeats {
            sink += evaluator(&battle, &ctx);
        }
        // A leaf costs a few microseconds, so `fmt_time` would round it to one
        // digit. Print nanoseconds instead.
        let elapsed = started.elapsed().as_secs_f64() / f64::from(repeats);
        println!(
            "  {name:>10}: {:.0}ns  (checksum {sink:.3})",
            elapsed * 1e9
        );
    }

    // A leaf is cheap on its own, and a search reaches many more leaves than
    // turns. These rows are what the feature frame costs one whole solve, and
    // how much the sharper leaf values move the double-oracle search.
    let state = MatchState::BattleState(battle);
    println!();
    println!("Depth-2 solve of the same position, one roll, exact outcomes");
    for (name, evaluator) in [
        ("legacy", legacy as poke_rust::solver::eval::LeafEvaluator),
        ("fitted", eval::fitted as poke_rust::solver::eval::LeafEvaluator),
    ] {
        let config = SolveConfig {
            depth: 2,
            damage_rolls: 1,
            chance: ChanceMode::Enumerate,
            eval: evaluator,
            ..SolveConfig::default()
        };
        match solve(&state, pokemon_dex, move_dex, &config) {
            Ok(result) => println!(
                "  {name:>10}: {:>8}  {} turns  value {:.4}",
                fmt_time(result.stats.elapsed.as_secs_f64()),
                fmt_count(result.stats.turns_simulated as f64),
                result.value,
            ),
            Err(error) => println!("  {name:>10}: failed: {error}"),
        }
    }
}

/// Solve one real teamsheet matchup and print the equilibrium.
///
/// The table above measures cost; this shows what is actually being bought. It
/// is also the sweep's end-to-end sanity check on a genuine position — the
/// probabilities are printed rather than asserted, so a strategy that failed to
/// sum to 1 or win odds outside `[0, 1]` would be visible at a glance.
fn print_sample_solve(
    pokemon_dex: &std::collections::HashMap<
        poke_rust::data::species::Species,
        poke_rust::state::dex_data::PokemonData,
    >,
    move_dex: &std::collections::HashMap<
        poke_rust::data::pokemon_move::PokemonMove,
        poke_rust::state::dex_data::MoveData,
    >,
) {
    let (p1_sheet, p2_sheet) = SAMPLE_SHEETS;
    let Some(state) = sample_position(pokemon_dex, move_dex) else {
        eprintln!("sample solve: team preview produced no battle position");
        return;
    };
    let MatchState::BattleState(battle) = &state else {
        eprintln!("sample solve: preview did not resolve into a battle");
        return;
    };

    // Exact within its roll setting: no truncation, so the printed odds carry no
    // approximation beyond the damage-roll count itself.
    let config = SolveConfig {
        depth: 2,
        damage_rolls: 4,
        chance: ChanceMode::Enumerate,
        ..SolveConfig::default()
    };
    let result = match solve(&state, pokemon_dex, move_dex, &config) {
        Ok(result) => result,
        Err(error) => {
            eprintln!("sample solve failed: {error}");
            return;
        }
    };

    println!();
    println!("Sample equilibrium — {p1_sheet} vs {p2_sheet}, depth 2, 4 rolls, exact outcomes");
    println!(
        "  P1 wins {:.1}%   |   P2 wins {:.1}%   ({} turns simulated in {})",
        100.0 * result.p1_win_odds,
        100.0 * result.p2_win_odds,
        result.stats.turns_simulated,
        fmt_time(result.stats.elapsed.as_secs_f64()),
    );
    for (player, strategy) in [
        (Player::P1, &result.p1_strategy),
        (Player::P2, &result.p2_strategy),
    ] {
        println!("  {player:?}:");
        for action in strategy {
            let described: Vec<String> = action
                .commands
                .iter()
                .enumerate()
                .map(|(slot, command)| battle_command_description(battle, player, slot, command))
                .collect();
            println!(
                "    {:>5.1}%  {}",
                100.0 * action.probability,
                described.join(" + ")
            );
        }
    }
    for warning in &result.warnings {
        println!("  note: {warning}");
    }
}

fn main() {
    // The engine's tracing is keyed off this global and a sweep performs
    // millions of turn resolutions; leaving it high would flood stdout and
    // dominate every timing in the table.
    poke_rust::VERBOSITY.set(0).ok();

    let pokemon_dex = parse_pokemon_dex("../pokemon_info/showdownDex.txt");
    let move_dex = parse_move_dex("../pokemon_info/showdownMoves.txt");

    // The sweep runs for many minutes. `--leaf-cost` re-measures the evaluator
    // alone, which is what a weight or feature change needs.
    if std::env::args().any(|argument| argument == "--leaf-cost") {
        print_leaf_cost(&pokemon_dex, &move_dex);
        return;
    }

    let mut last_reported = 0usize;
    let rows = run_solver(&pokemon_dex, &move_dex, &mut |done, total| {
        // One line per ten cells: the sweep runs for minutes and a silent
        // terminal is indistinguishable from a hang.
        if done == total || done >= last_reported + 10 {
            last_reported = done;
            eprintln!("  solver sweep: {done}/{total} cells");
        }
    })
    .expect("solver sweep should run");

    println!();
    print_table(&rows);
    print_pruning_summary(&rows);
    print_sample_solve(&pokemon_dex, &move_dex);
    print_leaf_cost(&pokemon_dex, &move_dex);
}
