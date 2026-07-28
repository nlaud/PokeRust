//! Game-tree solver benchmark: sweeps the three simultaneous-move algorithms
//! (backward induction, serialized alpha-beta bounds, double oracle) across
//! search depth, damage-roll count and chance-node policy, in singles and
//! doubles, over the real teamsheets in `../teamsheets/`.
//!
//! Run from `poke_rust/`:
//! ```sh
//! cargo bench --bench solver_speed
//! ```
//!
//! The implementation lives in `poke_rust::benchmarking::run_solver` so the web
//! server's benchmark endpoint can drive the identical sweep; this file is only
//! the table printer.
//!
//! ## Reading the table
//!
//! **`turns` is the column that matters.** A `simulate_turn` call costs
//! hundreds of microseconds while a matrix LP costs single-digit microseconds,
//! so a configuration's wall-clock time is very nearly its turn count times a
//! constant, and `lps` is nearly free either way. `cells` versus `total` is what
//! the pruning actually bought: backward induction evaluates every cell by
//! definition, so its ratio is 1.00, and anything double oracle saves shows up
//! directly there.
//!
//! ## Why cells are skipped, and why pairing counts differ
//!
//! The search tree grows as `(actions² · branches)` per ply, which spans three
//! orders of magnitude across this grid. Cells predicted to exceed
//! `MAX_ESTIMATED_TURNS` are not attempted and print their reason instead of a
//! time. The rest each get a fixed turn-resolution budget spent on as many
//! teamsheet pairings as fit, so cheap cells average over many matchups and
//! expensive ones over few — the `pairs` column reports which. A wall-clock stop
//! per cell backstops the estimate being wrong.
//!
//! Doubles is capped to one ply and to a bounded joint-action set (the `cap`
//! column). Two slots choosing together produce a few hundred joint actions and
//! a matrix with tens of thousands of cells, every one of them a turn
//! resolution; without the cap the doubles rows would measure nothing but that.
//! A capped solve is an equilibrium over a subset of the real options, so those
//! rows are a cost measurement, not a quality one.
//!
//! ## What reproduces, and what does not
//!
//! Within one process the solver is fully deterministic outside
//! `ChanceMode::Sample` — solving the same position twice gives an identical
//! value and identical work counts (`solver_tests::repeated_solves_are_identical`).
//!
//! **Across processes the count columns are stable only to about 1%.** The cause
//! is upstream of the solver: `coalesce_branches` builds each expansion level by
//! draining a `HashMap`, so intermediate branch order varies run to run, and
//! floating-point addition is not associative — a successor's probability can
//! land a few ulps apart on two runs of the same input. That is enough to
//! reorder a near-tie, change which successors a truncating `ChanceMode` keeps,
//! and shift the tree the search walks. Backward induction, which prunes
//! nothing, drifts as much as the pruning algorithms do, which is what
//! identifies the cause as the transition function rather than the search.
//! Reported *values* are unaffected in practice, agreeing bit for bit or within
//! one ulp.
//!
//! So: treat a few percent of movement in `turns`/`cells` between runs as noise,
//! and look for changes larger than that when comparing after an engine change.
//! `time` varies freely. The `pairs` column can shift too — a cell spends a
//! turn-resolution budget and stops on a wall-clock limit, both timing-dependent
//! — and a row averaged over a different number of pairings is not comparable to
//! the previous one even when nothing else changed.
//!
//! Memory is bounded by construction rather than by the grid: the search is
//! depth-first, so live state is proportional to depth times branching rather
//! than to the tree, and each successor's subtree is dropped before the next is
//! expanded. The node budget in `SolveConfig::default` is the backstop.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use poke_rust::benchmarking::{SolverRow, run_solver};
use poke_rust::simulator::{simulate_turn, team_preview_state_from_teamsheets};
use poke_rust::solver::chance::ChanceMode;
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
    let (p1_sheet, p2_sheet) = (
        "../teamsheets/MA_dragonite_rain.txt",
        "../teamsheets/MB_gyarados_volcarona.txt",
    );
    let (active, brought) = (1u8, 3u8);

    let preview =
        team_preview_state_from_teamsheets(p1_sheet, p2_sheet, pokemon_dex, move_dex, active, brought, true);
    // Fixed leads, so the printed position is the same on every run.
    let picks = || {
        PlayerCommand::TeamPreview(TeamPreviewCommand {
            active_indices: vec![0],
            back_indices: vec![1, 2],
        })
    };

    let Some(state) = simulate_turn(
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
    .map(|(_, state, _)| state) else {
        eprintln!("sample solve: team preview produced no branches");
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
}
