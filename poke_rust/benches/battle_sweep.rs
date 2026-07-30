//! Measures complete doubles battles and fog-of-war inference.
//! It plays every ordered pair of teamsheets in `../teamsheets/`.
//! The pairs include mirror matches.
//! A seeded generator selects legal commands.
//!
//! The seed reproduces the pair order and team-preview leads.
//! The engine uses `thread_rng()` for battle effects.
//! These effects change later states and legal commands.
//! Thus, turn counts, commands, and events can change between runs.
//!
//! `sample_turn_raw` resolves each battle once and records its events.
//! The benchmark replays these events for each information mode.
//! It measures `apply_information` for both players.
//! Each mode therefore receives the same event stream.
//!
//! `apply_information` can detect an impossible observation and panic.
//! Two known inference defects can cause these panics.
//! Pass 5 can cross stat bounds.
//! Learnset inference can reject a valid Illusion user.
//! `catch_unwind` records these panics as contradictions.
//! A contradiction stops belief tracking for that mode, player, and battle.
//!
//! Run from `poke_rust/`:
//! ```sh
//! cargo bench --bench battle_sweep
//! ```

#[path = "bench_common.rs"]
mod bench_common;

use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::time::{Duration, Instant};

use rand::SeedableRng;
use rand::rngs::StdRng;

use poke_rust::data::species::Species;
use poke_rust::information::inference::{InferenceConfig, apply_information};
use poke_rust::information::information::{InformationEvent, mask_events_for};
use poke_rust::information::unknowns::{InformationMode, UnknownMatchState};
use poke_rust::simulator::{sample_turn_raw, team_preview_state_from_teamsheets};
use poke_rust::state::battle::{MatchState, Player, PlayerCommand};
use poke_rust::state::dex_data::PokemonData;
use poke_rust::state::pokemon::PokemonState;

const ACTIVE_PER_SIDE: u8 = 2;
const BROUGHT_PER_SIDE: u8 = 4;

const MODES: [InformationMode; 4] = [
    InformationMode::PerfectInformation,
    InformationMode::ClosedTeamSheet,
    InformationMode::OpenTeamSheet,
    InformationMode::OpenTeamSheetNatures,
];

fn mode_label(mode: InformationMode) -> &'static str {
    match mode {
        InformationMode::PerfectInformation => "perfect",
        InformationMode::ClosedTeamSheet => "closedSheet",
        InformationMode::OpenTeamSheet => "openSheet",
        InformationMode::OpenTeamSheetNatures => "openSheetNatures",
    }
}

/// One resolved turn, recorded once and replayed for every information mode.
struct RecordedTurn {
    was_team_preview: bool,
    p1_cmd: PlayerCommand,
    p2_cmd: PlayerCommand,
    raw_events: Vec<InformationEvent>,
}

#[derive(Default)]
struct ResolutionStats {
    battles: usize,
    turns: usize,
    min_turns: usize,
    max_turns: usize,
    stalled: usize,
    resolve_time: Duration,
}

#[derive(Default)]
struct ModeStats {
    calls: u64,
    time: Duration,
    contradictions: u64,
}

/// Seeds both players' beliefs from the team-preview roster, per `mode`.
/// `PerfectInformation` tracks no belief at all — callers skip this mode.
fn seed_beliefs(
    mode: InformationMode,
    p1_mons: &[PokemonState],
    p2_mons: &[PokemonState],
    pokemon_dex: &HashMap<Species, PokemonData>,
) -> (UnknownMatchState, UnknownMatchState) {
    match mode {
        InformationMode::PerfectInformation => {
            unreachable!("PerfectInformation tracks no belief; callers must skip it")
        }
        InformationMode::ClosedTeamSheet => (
            UnknownMatchState::team_preview_closed_sheet_from_perspective(
                Player::P1,
                p1_mons,
                p2_mons,
                pokemon_dex,
                ACTIVE_PER_SIDE,
                BROUGHT_PER_SIDE,
                50,
                true,
            ),
            UnknownMatchState::team_preview_closed_sheet_from_perspective(
                Player::P2,
                p2_mons,
                p1_mons,
                pokemon_dex,
                ACTIVE_PER_SIDE,
                BROUGHT_PER_SIDE,
                50,
                true,
            ),
        ),
        InformationMode::OpenTeamSheet | InformationMode::OpenTeamSheetNatures => (
            UnknownMatchState::team_preview_open_sheet_from_perspective(
                Player::P1,
                p1_mons,
                p2_mons,
                pokemon_dex,
                ACTIVE_PER_SIDE,
                BROUGHT_PER_SIDE,
                50,
                mode,
                true,
            ),
            UnknownMatchState::team_preview_open_sheet_from_perspective(
                Player::P2,
                p2_mons,
                p1_mons,
                pokemon_dex,
                ACTIVE_PER_SIDE,
                BROUGHT_PER_SIDE,
                50,
                mode,
                true,
            ),
        ),
    }
}

/// Advances one player's belief through one recorded turn, timing and
/// counting the `apply_information` call. Returns `(next_belief, contradicted)`.
/// The belief is cloned before the call so a caught panic still leaves a
/// valid (pre-call) value to hand back — `apply_information` consumes its
/// input by value and a panic mid-call drops it, so there is nothing else to
/// recover once a contradiction fires.
#[allow(clippy::too_many_arguments)]
fn advance_one(
    belief: UnknownMatchState,
    viewer: Player,
    rec: &RecordedTurn,
    masked_events: &[InformationEvent],
    dexes: &bench_common::Dexes,
    config: &InferenceConfig,
    stats: &mut ModeStats,
) -> (UnknownMatchState, bool) {
    let seeded = if rec.was_team_preview {
        bench_common::reseed_for_battle(belief, viewer, &rec.p1_cmd, &rec.p2_cmd)
    } else {
        belief
    };
    let backup = seeded.clone();

    let start = Instant::now();
    let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
        apply_information(
            seeded,
            masked_events,
            false,
            &dexes.pokemon_dex,
            &dexes.move_dex,
            &dexes.ability_dex,
            config,
        )
    }));
    stats.time += start.elapsed();
    stats.calls += 1;

    match result {
        Ok(next) => (next, false),
        Err(_) => {
            stats.contradictions += 1;
            (backup, true)
        }
    }
}

fn main() {
    poke_rust::VERBOSITY.set(0).ok();
    let dexes = bench_common::load_dexes();
    let paths = bench_common::teamsheet_paths();
    assert!(!paths.is_empty(), "no teamsheets found in ../teamsheets");

    // apply_information panics on a known/tracked contradiction (see this
    // file's header comment) — expected under the current inference-engine
    // bugs, caught via catch_unwind in advance_one(), and counted per mode in
    // the report below. Suppress the default panic hook's stderr dump so a
    // full sweep's output stays readable; if you're debugging a NEW/unexpected
    // panic in this bench, comment this out to get the raw message + location.
    std::panic::set_hook(Box::new(|_| {}));

    let config = InferenceConfig {
        use_stat_points: true,
        force_max_ivs: true,
        learnset_dex: dexes.learnset_dex.clone(),
        ..InferenceConfig::default()
    };

    let mut resolution = ResolutionStats {
        min_turns: usize::MAX,
        ..Default::default()
    };
    let mut mode_stats: [ModeStats; 4] = Default::default();

    for (i, p1_path) in paths.iter().enumerate() {
        for (j, p2_path) in paths.iter().enumerate() {
            // Deterministic per-pairing seed: same pairing always drives the
            // same sequence of harness choices across runs.
            let seed = (i * paths.len() + j) as u64;
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
                ACTIVE_PER_SIDE,
                BROUGHT_PER_SIDE,
                true,
            );
            let p1_mons = preview.p1_mons.clone();
            let p2_mons = preview.p2_mons.clone();

            let p1_tp = bench_common::random_team_preview_command(
                p1_mons.len(),
                ACTIVE_PER_SIDE,
                BROUGHT_PER_SIDE,
                &mut rng,
            );
            let p2_tp = bench_common::random_team_preview_command(
                p2_mons.len(),
                ACTIVE_PER_SIDE,
                BROUGHT_PER_SIDE,
                &mut rng,
            );

            let mut state = MatchState::TeamPreviewState(preview);
            let mut p1_cmd = PlayerCommand::TeamPreview(p1_tp);
            let mut p2_cmd = PlayerCommand::TeamPreview(p2_tp);

            // Step 1: resolve the battle once, recording every turn.
            let mut recorded: Vec<RecordedTurn> = Vec::new();
            let mut stalled = false;
            let mut turn = 0usize;
            loop {
                turn += 1;
                if turn > bench_common::MAX_TURNS {
                    stalled = true;
                    break;
                }

                let was_team_preview = matches!(state, MatchState::TeamPreviewState(_));

                let resolve_start = Instant::now();
                let (next_state, raw_events, _probability) = sample_turn_raw(
                    &state,
                    &p1_cmd,
                    &p2_cmd,
                    &dexes.move_dex,
                    &dexes.pokemon_dex,
                    true,
                    16,
                    Some(Player::P1),
                );
                resolution.resolve_time += resolve_start.elapsed();

                recorded.push(RecordedTurn {
                    was_team_preview,
                    p1_cmd: p1_cmd.clone(),
                    p2_cmd: p2_cmd.clone(),
                    raw_events: raw_events.unwrap_or_default(),
                });

                state = next_state;
                match &state {
                    MatchState::GameOverState { .. } => break,
                    MatchState::BattleState(bs) => {
                        p1_cmd = PlayerCommand::Battle(bench_common::random_commands_for_player(
                            bs,
                            Player::P1,
                            &dexes.move_dex,
                            &dexes.pokemon_dex,
                            &mut rng,
                        ));
                        p2_cmd = PlayerCommand::Battle(bench_common::random_commands_for_player(
                            bs,
                            Player::P2,
                            &dexes.move_dex,
                            &dexes.pokemon_dex,
                            &mut rng,
                        ));
                    }
                    MatchState::TeamPreviewState(_) => {
                        unreachable!("team preview only occurs once, at turn 1")
                    }
                }
            }

            resolution.battles += 1;
            resolution.turns += recorded.len();
            resolution.min_turns = resolution.min_turns.min(recorded.len());
            resolution.max_turns = resolution.max_turns.max(recorded.len());
            if stalled {
                resolution.stalled += 1;
            }

            // Step 2: replay the recorded trajectory once per information mode.
            for (mode_idx, &mode) in MODES.iter().enumerate() {
                if mode == InformationMode::PerfectInformation {
                    continue; // zero-overhead baseline: no belief tracked, nothing to time
                }
                let stats = &mut mode_stats[mode_idx];
                let (mut belief_p1, mut belief_p2) =
                    seed_beliefs(mode, &p1_mons, &p2_mons, &dexes.pokemon_dex);
                let mut p1_alive = true;
                let mut p2_alive = true;

                for rec in &recorded {
                    if !p1_alive && !p2_alive {
                        break;
                    }
                    let events_p1 = mask_events_for(Player::P1, &rec.raw_events);
                    let events_p2 = mask_events_for(Player::P2, &rec.raw_events);

                    if p1_alive {
                        let (next, contradicted) = advance_one(
                            belief_p1,
                            Player::P1,
                            rec,
                            &events_p1,
                            &dexes,
                            &config,
                            stats,
                        );
                        belief_p1 = next;
                        if contradicted {
                            p1_alive = false;
                        }
                    }
                    if p2_alive {
                        let (next, contradicted) = advance_one(
                            belief_p2,
                            Player::P2,
                            rec,
                            &events_p2,
                            &dexes,
                            &config,
                            stats,
                        );
                        belief_p2 = next;
                        if contradicted {
                            p2_alive = false;
                        }
                    }
                }
            }
        }
    }

    println!(
        "=== Battle resolution ({} pairings, doubles 2/4) ===",
        resolution.battles
    );
    println!(
        "battles={} turns={} avg_turns/battle={:.1} min={} max={} stalled={}",
        resolution.battles,
        resolution.turns,
        resolution.turns as f64 / resolution.battles.max(1) as f64,
        if resolution.min_turns == usize::MAX {
            0
        } else {
            resolution.min_turns
        },
        resolution.max_turns,
        resolution.stalled,
    );
    println!(
        "sample_turn_raw: total={} avg/turn={}",
        bench_common::fmt_time(resolution.resolve_time.as_secs_f64()),
        bench_common::fmt_time(
            resolution.resolve_time.as_secs_f64() / resolution.turns.max(1) as f64
        ),
    );

    println!();
    println!("=== Inference by information mode ===");
    println!(
        "(a nonzero contradiction count reflects a known, already-tracked inference-engine bug \
         — see this file's header comment — not a bench defect)"
    );
    println!(
        "{:<20} {:>10} {:>14} {:>14} {:>15}",
        "mode", "calls", "total", "avg/call", "contradictions"
    );
    for (mode_idx, &mode) in MODES.iter().enumerate() {
        if mode == InformationMode::PerfectInformation {
            println!(
                "{:<20} {:>10} {:>14} {:>14} {:>15}",
                mode_label(mode),
                0,
                "n/a (baseline)",
                "n/a",
                0
            );
            continue;
        }
        let stats = &mode_stats[mode_idx];
        let avg = stats.time.as_secs_f64() / stats.calls.max(1) as f64;
        println!(
            "{:<20} {:>10} {:>14} {:>14} {:>15}",
            mode_label(mode),
            stats.calls,
            bench_common::fmt_time(stats.time.as_secs_f64()),
            bench_common::fmt_time(avg),
            stats.contradictions,
        );
    }
}
