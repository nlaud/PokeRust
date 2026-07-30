//! Measures one turn across each mode, roll count, and critical-hit setting.
//! The benchmark tests singles and doubles with each ordered teamsheet pair.
//! A fixed seed selects the leads and moves.
//!
//! The benchmark selects all commands before the engine uses random values.
//! A seed therefore produces the same leads, moves, and branch counts.
//! Only the measured time changes between runs.
//!
//! Run from `poke_rust/`:
//! ```sh
//! cargo bench --bench turn_speed
//! ```
//!
//! Singles uses one active Pokémon and three selected Pokémon.
//! Doubles uses two active Pokémon and four selected Pokémon.
//! Each pair uses the same leads and moves for all roll and critical-hit settings.
//! Each result is the average for all tested pairs.
//!
//! Enumeration uses one, two, or four damage rolls.
//! It omits larger counts because doubles can use more than 15 GB.
//! The four-roll doubles test also disables critical hits.
//! Sample mode uses one, two, four, eight, or 16 rolls.

#[path = "bench_common.rs"]
mod bench_common;

use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::time::Instant;

use rand::SeedableRng;
use rand::rngs::StdRng;

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
                    p1_path
                        .to_str()
                        .expect("teamsheet path should be valid UTF-8"),
                    p2_path
                        .to_str()
                        .expect("teamsheet path should be valid UTF-8"),
                    &dexes.pokemon_dex,
                    &dexes.move_dex,
                    active,
                    brought,
                    true,
                );
                let p1_tp = bench_common::random_team_preview_command(
                    preview.p1_mons.len(),
                    active,
                    brought,
                    &mut rng,
                );
                let p2_tp = bench_common::random_team_preview_command(
                    preview.p2_mons.len(),
                    active,
                    brought,
                    &mut rng,
                );
                let p1_pv = PlayerCommand::TeamPreview(p1_tp);
                let p2_pv = PlayerCommand::TeamPreview(p2_tp);

                // Resolve the team-preview step (1 roll, no crit — leads are
                // deterministic once picked, only the post-preview turn is timed)
                // and take the highest-probability resulting battle state.
                let state = simulate_turn(
                    &MatchState::TeamPreviewState(preview),
                    &p1_pv,
                    &p2_pv,
                    &dexes.move_dex,
                    &dexes.pokemon_dex,
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
                .unwrap()
                .1;

                let MatchState::BattleState(battle_state) = &state else {
                    continue; // shouldn't happen once leads are chosen, but don't let one odd pairing kill the run
                };

                // Pick each side's move once; reused across this pairing's whole grid.
                let p1_cmd = PlayerCommand::Battle(bench_common::random_commands_for_player(
                    battle_state,
                    Player::P1,
                    &dexes.move_dex,
                    &dexes.pokemon_dex,
                    &mut rng,
                ));
                let p2_cmd = PlayerCommand::Battle(bench_common::random_commands_for_player(
                    battle_state,
                    Player::P2,
                    &dexes.move_dex,
                    &dexes.pokemon_dex,
                    &mut rng,
                ));

                let enum_ok: &dyn Fn(u8, bool) -> bool = if scenario == "doubles" {
                    &doubles_enum_ok
                } else {
                    &(|_, _| true)
                };

                for crit in [false, true] {
                    for &rolls in &ENUM_ROLLS {
                        if !enum_ok(rolls, crit) {
                            continue;
                        }
                        let start = Instant::now();
                        let branches = simulate_turn(
                            &state,
                            &p1_cmd,
                            &p2_cmd,
                            &dexes.move_dex,
                            &dexes.pokemon_dex,
                            crit,
                            rolls,
                            None,
                        )
                        .len();
                        let elapsed = start.elapsed().as_secs_f64();
                        let cell = cells
                            .entry((scenario, "enumerate", rolls, crit))
                            .or_default();
                        cell.total_secs += elapsed;
                        cell.samples += 1;
                        cell.total_branches += branches;
                    }
                    for &rolls in &SAMPLE_ROLLS {
                        let start = Instant::now();
                        let _ = sample_turn(
                            &state,
                            &p1_cmd,
                            &p2_cmd,
                            &dexes.move_dex,
                            &dexes.pokemon_dex,
                            crit,
                            rolls,
                            None,
                        );
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
                    _ => println!(
                        "{:<8} {:<10} {:>5} {:>5} {:>12} {:>10} {:>10}",
                        scenario, "enumerate", rolls, crit, "skipped", "-", 0
                    ),
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
