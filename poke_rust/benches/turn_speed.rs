//! Turn-resolution speed benchmark: times a single attack-turn resolution
//! across enumerate/sample mode × damage rolls × crit branching, in singles
//! and doubles, swept over every ordered pairing of the real teamsheets in
//! `../teamsheets/` with seeded random leads and moves. Unlike
//! `battle_sweep.rs` (a multi-turn loop, where engine RNG feeds back into
//! which commands are even legal on later turns), everything measured here
//! is chosen from state reached before any engine RNG has run — team-preview
//! resolution takes the single deterministic highest-probability branch, and
//! the one post-preview move pick follows immediately from that — so the
//! full scenario (leads + moves + resulting branch counts) reproduces exactly
//! for a given seed; only wall-clock timing varies run to run. Verified by
//! diffing two runs' branch-count columns.
//!
//! Run from `poke_rust/`:
//! ```sh
//! cargo bench --bench turn_speed
//! ```
//!
//! Scenarios: singles (1 active / 3 brought) and doubles (2 active / 4
//! brought), each pairing's leads and move choice picked once per pairing via
//! `bench_common::random_team_preview_command`/`random_commands_for_player`
//! and then reused across that pairing's whole rolls×crit grid, so rolls/crit
//! stays the controlled variable within a pairing. Results are averaged
//! across all pairings that ran each grid cell.
//!
//! **Enumerate mode never runs above 4 damage rolls** (`rolls ∈ {1, 2, 4}`):
//! full enumeration (`simulate_turn` — every possible outcome, not one
//! sampled trajectory) is the path CLAUDE.md flags as exploding past 15 GB on
//! doubles spread turns, and randomized teams/moves make the branch count
//! unpredictable ahead of time, so 8/16-roll enumerate cells are never
//! attempted. The doubles-specific "no crit at 4 rolls" exclusion proven safe
//! by the original fixed-team version of this bench is kept on top of that
//! cap. Sample mode (memory-bounded — one weighted trajectory regardless of
//! roll count) keeps the full `{1, 2, 4, 8, 16}` grid.

#[path = "bench_common.rs"]
mod bench_common;

use std::collections::HashMap;
use std::time::Instant;

use rand::rngs::StdRng;
use rand::SeedableRng;

use poke_rust::simulator::{sample_turn, simulate_turn, team_preview_state_from_teamsheets};
use poke_rust::state::battle::{MatchState, Player, PlayerCommand};

const ENUM_ROLLS: [u8; 3] = [1, 2, 4];
const SAMPLE_ROLLS: [u8; 5] = [1, 2, 4, 8, 16];

/// Doubles-only enumerate exclusion, on top of the blanket ≤4-rolls cap:
/// (4 rolls, crit) was the specific cell that exceeded 15 GB in the original
/// fixed-team version of this bench; `(1, _)` and `(2, _)` and `(4, false)`
/// stayed tractable.
fn doubles_enum_ok(rolls: u8, crit: bool) -> bool {
    matches!((rolls, crit), (1, _) | (2, _) | (4, false))
}

#[derive(Default, Clone, Copy)]
struct Cell {
    total_secs: f64,
    samples: usize,
    total_branches: usize,
}

type CellKey = (&'static str, &'static str, u8, bool); // (scenario, mode, rolls, crit)

fn main() {
    poke_rust::VERBOSITY.set(0).ok();
    let dexes = bench_common::load_dexes();
    let paths = bench_common::teamsheet_paths();
    assert!(!paths.is_empty(), "no teamsheets found in ../teamsheets");

    let mut cells: HashMap<CellKey, Cell> = HashMap::new();

    for (i, p1_path) in paths.iter().enumerate() {
        for (j, p2_path) in paths.iter().enumerate() {
            let seed = (i * paths.len() + j) as u64;

            for (scenario, active, brought) in [("singles", 1u8, 3u8), ("doubles", 2u8, 4u8)] {
                let mut rng = StdRng::seed_from_u64(seed);

                let preview = team_preview_state_from_teamsheets(
                    p1_path.to_str().expect("teamsheet path should be valid UTF-8"),
                    p2_path.to_str().expect("teamsheet path should be valid UTF-8"),
                    &dexes.pokemon_dex,
                    &dexes.move_dex,
                    active,
                    brought,
                    true,
                );
                let p1_tp = bench_common::random_team_preview_command(preview.p1_mons.len(), active, brought, &mut rng);
                let p2_tp = bench_common::random_team_preview_command(preview.p2_mons.len(), active, brought, &mut rng);
                let p1_pv = PlayerCommand::TeamPreview(p1_tp);
                let p2_pv = PlayerCommand::TeamPreview(p2_tp);

                // Resolve the team-preview step (1 roll, no crit — leads are
                // deterministic once picked, only the post-preview turn is timed)
                // and take the highest-probability resulting battle state.
                let state = simulate_turn(
                    &MatchState::TeamPreviewState(preview), &p1_pv, &p2_pv, &dexes.move_dex, &dexes.pokemon_dex, false, 1, None,
                )
                .into_iter()
                .max_by(|a, b| a.2.partial_cmp(&b.2).unwrap())
                .unwrap()
                .0;

                let MatchState::BattleState(battle_state) = &state else {
                    continue; // shouldn't happen once leads are chosen, but don't let one odd pairing kill the run
                };

                // Pick each side's move once; reused across this pairing's whole grid.
                let p1_cmd = PlayerCommand::Battle(bench_common::random_commands_for_player(
                    battle_state, Player::P1, &dexes.move_dex, &dexes.pokemon_dex, &mut rng,
                ));
                let p2_cmd = PlayerCommand::Battle(bench_common::random_commands_for_player(
                    battle_state, Player::P2, &dexes.move_dex, &dexes.pokemon_dex, &mut rng,
                ));

                let enum_ok: &dyn Fn(u8, bool) -> bool =
                    if scenario == "doubles" { &doubles_enum_ok } else { &(|_, _| true) };

                for crit in [false, true] {
                    for &rolls in &ENUM_ROLLS {
                        if !enum_ok(rolls, crit) {
                            continue;
                        }
                        let start = Instant::now();
                        let branches =
                            simulate_turn(&state, &p1_cmd, &p2_cmd, &dexes.move_dex, &dexes.pokemon_dex, crit, rolls, None).len();
                        let elapsed = start.elapsed().as_secs_f64();
                        let cell = cells.entry((scenario, "enumerate", rolls, crit)).or_default();
                        cell.total_secs += elapsed;
                        cell.samples += 1;
                        cell.total_branches += branches;
                    }
                    for &rolls in &SAMPLE_ROLLS {
                        let start = Instant::now();
                        let _ = sample_turn(&state, &p1_cmd, &p2_cmd, &dexes.move_dex, &dexes.pokemon_dex, crit, rolls, None);
                        let elapsed = start.elapsed().as_secs_f64();
                        let cell = cells.entry((scenario, "sample", rolls, crit)).or_default();
                        cell.total_secs += elapsed;
                        cell.samples += 1;
                    }
                }
            }
        }
    }

    println!(
        "{:<8} {:<10} {:>5} {:>5} {:>12} {:>10} {:>10}",
        "scenario", "mode", "rolls", "crit", "avg_time", "avg_branch", "pairings"
    );
    for scenario in ["singles", "doubles"] {
        for crit in [false, true] {
            for &rolls in &ENUM_ROLLS {
                match cells.get(&(scenario, "enumerate", rolls, crit)) {
                    Some(cell) if cell.samples > 0 => println!(
                        "{:<8} {:<10} {:>5} {:>5} {:>12} {:>10} {:>10}",
                        scenario,
                        "enumerate",
                        rolls,
                        crit,
                        bench_common::fmt_time(cell.total_secs / cell.samples as f64),
                        cell.total_branches / cell.samples,
                        cell.samples,
                    ),
                    _ => println!("{:<8} {:<10} {:>5} {:>5} {:>12} {:>10} {:>10}", scenario, "enumerate", rolls, crit, "skipped", "-", 0),
                }
            }
            for &rolls in &SAMPLE_ROLLS {
                if let Some(cell) = cells.get(&(scenario, "sample", rolls, crit)) {
                    println!(
                        "{:<8} {:<10} {:>5} {:>5} {:>12} {:>10} {:>10}",
                        scenario,
                        "sample",
                        rolls,
                        crit,
                        bench_common::fmt_time(cell.total_secs / cell.samples as f64),
                        1,
                        cell.samples,
                    );
                }
            }
        }
    }
}
