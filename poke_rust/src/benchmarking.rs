//! Turn-resolution and fog-of-war-inference speed measurement, as a library
//! module so both the standalone benches (`poke_rust/benches/turn_speed.rs`,
//! `battle_sweep.rs`) and the web server's `POST /api/benchmark` endpoint
//! (`src/bin/server/routes.rs`) can drive the same scenarios. The bench
//! binaries print a full historical sweep to stdout (recorded in
//! `benches/RESULTS.md`); this module ports the same grid/timing logic but
//! bounds how much work one call does, since it's meant to answer a live UI
//! button click, not a multi-minute offline sweep.
//!
//! `run_turn_speed` mirrors `turn_speed.rs`: one post-team-preview turn timed
//! across enumerate (`simulate_turn`) vs sample (`sample_turn`) mode, damage
//! rolls, crit branching, singles vs doubles. `run_inference` mirrors
//! `battle_sweep.rs`: full doubles games played to completion, replaying the
//! recorded event stream through `apply_information` once per fog-of-war
//! information mode and timing each call.
//!
//! Both bound the number of teamsheet pairings/games they touch — doubles
//! enumeration at high roll counts is the specific case CLAUDE.md documents
//! as exceeding 15 GB (see `turn_speed.rs`'s header), and even the tractable
//! doubles-enumerate cells are slow in wall-clock terms (RESULTS.md records
//! the `(4 rolls, no crit)` cell alone averaging ~7.5s per pairing), so a
//! live call runs a small, capped number of pairings rather than the full
//! N² sweep the offline benches do.

use std::collections::{HashMap, HashSet};
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::time::Instant;

use rand::rngs::StdRng;
use rand::seq::SliceRandom;
use rand::{Rng, SeedableRng};

use crate::data::ability::Ability;
use crate::data::pokemon_move::PokemonMove;
use crate::data::species::Species;
use crate::information::inference::{InferenceConfig, apply_information};
use crate::information::information::{InformationEvent, mask_events_for};
use crate::information::unknowns::{InformationMode, UnknownMatchState};
use crate::simulator::{
    get_possible_commands_for_active_slot, sample_turn, sample_turn_raw, simulate_turn,
    team_preview_state_from_teamsheets, validate_battle_command_combination,
};
use crate::state::battle::{
    BattleCommand, BattleState, MatchState, Player, PlayerCommand, TeamPreviewCommand,
};
use crate::state::dex_data::{AbilityData, MoveData, PokemonData};
use crate::state::pokemon::PokemonState;

/// Hard caps on live-call work, independent of what a caller requests — see
/// this module's header for why (doubles enumeration cost, full-game length).
const MAX_TURN_SPEED_PAIRINGS: usize = 4;
const MAX_INFERENCE_GAMES: usize = 3;

/// Hang guard only (mirrors `bench_common::MAX_TURNS`), not a soundness/quality
/// property — real doubles games settle in a handful of turns.
const MAX_TURNS: usize = 400;

const ENUM_ROLLS: [u8; 3] = [1, 2, 4];
const SAMPLE_ROLLS: [u8; 5] = [1, 2, 4, 8, 16];

const INFERENCE_MODES: [InformationMode; 4] = [
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

/// One `(scenario, mode, rolls, crit)` cell's averaged timing — mirrors a row
/// of `turn_speed.rs`'s printed table / `benches/RESULTS.md`.
#[derive(Clone, Debug)]
pub struct TurnSpeedRow {
    pub scenario: &'static str, // "singles" | "doubles"
    pub mode: &'static str,     // "enumerate" | "sample"
    pub rolls: u8,
    pub crit: bool,
    pub avg_time_secs: f64,
    pub avg_branches: usize,
    pub pairings: usize,
}

/// One information mode's averaged `apply_information` timing across a
/// belief-update sweep — mirrors a row of `battle_sweep.rs`'s "Inference by
/// information mode" table. `PerfectInformation` tracks no belief at all, so
/// it never appears here (it's the zero-overhead baseline).
#[derive(Clone, Debug)]
pub struct InferenceRow {
    pub information_mode: &'static str,
    pub calls: u64,
    pub avg_time_secs: f64,
    pub contradictions: u64,
}

/// Every `../teamsheets/*.txt` file, sorted for a stable, reproducible
/// ordering. Both benches and the server run from `poke_rust/`, so this
/// relative path resolves the same way for either caller.
fn teamsheet_paths() -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir("../teamsheets")
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "txt"))
        .collect();
    paths.sort();
    paths
}

/// Picks a random legal team-preview pick: `brought_per_side` distinct roster
/// indices, the first `active_per_side` of which lead. Direct port of
/// `benches/bench_common.rs::random_team_preview_command`.
fn random_team_preview_command(
    team_len: usize,
    active_per_side: u8,
    brought_per_side: u8,
    rng: &mut StdRng,
) -> TeamPreviewCommand {
    let brought = (brought_per_side as usize).min(team_len);
    let active = (active_per_side as usize).min(brought);

    let mut indices: Vec<usize> = (0..team_len).collect();
    indices.shuffle(rng);
    indices.truncate(brought);

    let active_indices = indices[..active].to_vec();
    let back_indices = indices[active..].to_vec();
    TeamPreviewCommand {
        active_indices,
        back_indices,
    }
}

/// Picks one random, jointly-legal `BattleCommand` set for every active slot
/// of `player` this turn. Direct port of
/// `benches/bench_common.rs::random_commands_for_player` — see that file's
/// doc comment for why this (rather than re-deriving from `user.rs`) is a
/// proven pattern for driving full battles.
fn random_commands_for_player(
    state: &BattleState,
    player: Player,
    move_dex: &HashMap<PokemonMove, MoveData>,
    pokemon_dex: &HashMap<Species, PokemonData>,
    rng: &mut StdRng,
) -> Vec<BattleCommand> {
    let active_len = match player {
        Player::P1 => state.p1_active_mons.len(),
        Player::P2 => state.p2_active_mons.len(),
    };

    let per_slot_options: Vec<Vec<BattleCommand>> = (0..active_len)
        .map(|slot_idx| {
            get_possible_commands_for_active_slot(state, player, slot_idx, move_dex, pokemon_dex)
        })
        .collect();

    for _ in 0..20 {
        let mut claimed_switch_targets: Vec<usize> = Vec::new();
        let mut cmds: Vec<BattleCommand> = Vec::with_capacity(active_len);
        for options in &per_slot_options {
            let available: Vec<&BattleCommand> = options
                .iter()
                .filter(|c| match c {
                    BattleCommand::Switch(s) => !claimed_switch_targets.contains(&s.party_index),
                    _ => true,
                })
                .collect();
            let chosen = match available.as_slice() {
                [] => BattleCommand::Pass,
                opts => opts[rng.gen_range(0..opts.len())].clone(),
            };
            if let BattleCommand::Switch(s) = &chosen {
                claimed_switch_targets.push(s.party_index);
            }
            cmds.push(chosen);
        }
        if validate_battle_command_combination(&cmds) {
            return cmds;
        }
    }

    per_slot_options
        .iter()
        .map(|options| {
            options
                .iter()
                .find(|c| !matches!(c, BattleCommand::Switch(_)))
                .or_else(|| options.first())
                .cloned()
                .unwrap_or(BattleCommand::Pass)
        })
        .collect()
}

/// Doubles-only enumerate exclusion, on top of the blanket ≤4-rolls cap — see
/// `turn_speed.rs`'s `doubles_enum_ok` for the full rationale (the `(4, true)`
/// cell is the one that exceeded 15 GB in the original fixed-team version).
fn doubles_enum_ok(rolls: u8, crit: bool) -> bool {
    matches!((rolls, crit), (1, _) | (2, _) | (4, false))
}

#[derive(Default, Clone, Copy)]
struct SpeedCell {
    total_secs: f64,
    samples: usize,
    total_branches: usize,
}

/// Times one post-team-preview turn across the enumerate/sample × rolls ×
/// crit × singles/doubles grid, mirroring `benches/turn_speed.rs`. Bounds
/// work to `pairings` ordered teamsheet pairings (clamped to
/// `[1, MAX_TURN_SPEED_PAIRINGS]`) — walked in the same deterministic
/// row-major order the offline bench uses, so a smaller pairing count is a
/// prefix of a larger one's work, not a different sample.
pub fn run_turn_speed(
    pokemon_dex: &HashMap<Species, PokemonData>,
    move_dex: &HashMap<PokemonMove, MoveData>,
    pairings: usize,
) -> Result<Vec<TurnSpeedRow>, String> {
    let paths = teamsheet_paths();
    if paths.is_empty() {
        return Err("no teamsheets found in ../teamsheets".to_string());
    }
    let pairings = pairings.clamp(1, MAX_TURN_SPEED_PAIRINGS);

    let mut cells: HashMap<(&'static str, &'static str, u8, bool), SpeedCell> = HashMap::new();
    let mut done = 0usize;

    'outer: for (i, p1_path) in paths.iter().enumerate() {
        for (j, p2_path) in paths.iter().enumerate() {
            if done >= pairings {
                break 'outer;
            }
            done += 1;
            let seed = (i * paths.len() + j) as u64;

            for (scenario, active, brought) in [("singles", 1u8, 3u8), ("doubles", 2u8, 4u8)] {
                let mut rng = StdRng::seed_from_u64(seed);

                let preview = team_preview_state_from_teamsheets(
                    p1_path
                        .to_str()
                        .ok_or("teamsheet path should be valid UTF-8")?,
                    p2_path
                        .to_str()
                        .ok_or("teamsheet path should be valid UTF-8")?,
                    pokemon_dex,
                    move_dex,
                    active,
                    brought,
                    true,
                );
                let p1_tp =
                    random_team_preview_command(preview.p1_mons.len(), active, brought, &mut rng);
                let p2_tp =
                    random_team_preview_command(preview.p2_mons.len(), active, brought, &mut rng);
                let p1_pv = PlayerCommand::TeamPreview(p1_tp);
                let p2_pv = PlayerCommand::TeamPreview(p2_tp);

                let state = simulate_turn(
                    &MatchState::TeamPreviewState(preview),
                    &p1_pv,
                    &p2_pv,
                    move_dex,
                    pokemon_dex,
                    false,
                    1,
                    None,
                )
                .into_iter()
                .max_by(|a, b| a.2.partial_cmp(&b.2).unwrap())
                .ok_or("team preview resolution produced no branches")?
                .0;

                let MatchState::BattleState(battle_state) = &state else {
                    continue; // shouldn't happen once leads are chosen, but don't let one odd pairing kill the run
                };

                let p1_cmd = PlayerCommand::Battle(random_commands_for_player(
                    battle_state,
                    Player::P1,
                    move_dex,
                    pokemon_dex,
                    &mut rng,
                ));
                let p2_cmd = PlayerCommand::Battle(random_commands_for_player(
                    battle_state,
                    Player::P2,
                    move_dex,
                    pokemon_dex,
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
                            move_dex,
                            pokemon_dex,
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
                            move_dex,
                            pokemon_dex,
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

    let mut rows = Vec::with_capacity(cells.len());
    for scenario in ["singles", "doubles"] {
        for crit in [false, true] {
            for &rolls in &ENUM_ROLLS {
                if let Some(cell) = cells.get(&(scenario, "enumerate", rolls, crit))
                    && cell.samples > 0
                {
                    rows.push(TurnSpeedRow {
                        scenario,
                        mode: "enumerate",
                        rolls,
                        crit,
                        avg_time_secs: cell.total_secs / cell.samples as f64,
                        avg_branches: cell.total_branches / cell.samples,
                        pairings: cell.samples,
                    });
                }
            }
            for &rolls in &SAMPLE_ROLLS {
                if let Some(cell) = cells.get(&(scenario, "sample", rolls, crit)) {
                    rows.push(TurnSpeedRow {
                        scenario,
                        mode: "sample",
                        rolls,
                        crit,
                        avg_time_secs: cell.total_secs / cell.samples as f64,
                        avg_branches: 1,
                        pairings: cell.samples,
                    });
                }
            }
        }
    }
    Ok(rows)
}

/// One resolved turn, recorded once and replayed for every information mode
/// (see `run_inference`'s doc comment). Direct port of
/// `benches/battle_sweep.rs::RecordedTurn`.
struct RecordedTurn {
    was_team_preview: bool,
    p1_cmd: PlayerCommand,
    p2_cmd: PlayerCommand,
    raw_events: Vec<InformationEvent>,
}

#[derive(Default)]
struct ModeStats {
    calls: u64,
    time_secs: f64,
    contradictions: u64,
}

/// Re-seeds a team-preview belief into a battle-level belief on the
/// team-preview -> battle transition. Direct port of
/// `benches/bench_common.rs::reseed_for_battle`.
fn reseed_for_battle(
    belief: UnknownMatchState,
    viewer: Player,
    p1_cmd: &PlayerCommand,
    p2_cmd: &PlayerCommand,
) -> UnknownMatchState {
    let (
        UnknownMatchState::TeamPreview(preview),
        PlayerCommand::TeamPreview(p1_tp),
        PlayerCommand::TeamPreview(p2_tp),
    ) = (&belief, p1_cmd, p2_cmd)
    else {
        return belief;
    };
    UnknownMatchState::Battle(preview.into_battle_state(
        viewer,
        &p1_tp.active_indices,
        &p1_tp.back_indices,
        &p2_tp.active_indices,
        &p2_tp.back_indices,
    ))
}

/// Seeds both players' beliefs from the team-preview roster, per `mode`.
/// Direct port of `benches/battle_sweep.rs::seed_beliefs`; callers must skip
/// `PerfectInformation` (it tracks no belief).
fn seed_beliefs(
    mode: InformationMode,
    p1_mons: &[PokemonState],
    p2_mons: &[PokemonState],
    pokemon_dex: &HashMap<Species, PokemonData>,
) -> (UnknownMatchState, UnknownMatchState) {
    const ACTIVE_PER_SIDE: u8 = 2;
    const BROUGHT_PER_SIDE: u8 = 4;
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
/// Direct port of `benches/battle_sweep.rs::advance_one`.
#[allow(clippy::too_many_arguments)]
fn advance_one(
    belief: UnknownMatchState,
    viewer: Player,
    rec: &RecordedTurn,
    masked_events: &[InformationEvent],
    pokemon_dex: &HashMap<Species, PokemonData>,
    move_dex: &HashMap<PokemonMove, MoveData>,
    ability_dex: &HashMap<Ability, AbilityData>,
    config: &InferenceConfig,
    stats: &mut ModeStats,
) -> (UnknownMatchState, bool) {
    let seeded = if rec.was_team_preview {
        reseed_for_battle(belief, viewer, &rec.p1_cmd, &rec.p2_cmd)
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
            pokemon_dex,
            move_dex,
            ability_dex,
            config,
        )
    }));
    stats.time_secs += start.elapsed().as_secs_f64();
    stats.calls += 1;

    match result {
        Ok(next) => (next, false),
        Err(_) => {
            stats.contradictions += 1;
            (backup, true)
        }
    }
}

/// RAII guard: suppresses the default panic hook's stderr dump while a known,
/// tracked `apply_information` contradiction is caught (see `advance_one`),
/// restoring the previous hook on drop. `benches/battle_sweep.rs` does the
/// same suppression with a bare `set_hook` since it's a one-shot process that
/// exits right after; this runs inside the long-lived server process instead,
/// where the panic hook is a process-wide global shared by every other
/// request, so it must be restored deterministically — including if some
/// *other* panic escapes this call's own `catch_unwind` wrapping.
type PanicHook = Box<dyn Fn(&std::panic::PanicHookInfo<'_>) + Sync + Send + 'static>;

struct PanicHookGuard {
    prev: Option<PanicHook>,
}

impl Drop for PanicHookGuard {
    fn drop(&mut self) {
        if let Some(prev) = self.prev.take() {
            std::panic::set_hook(prev);
        }
    }
}

fn suppress_known_contradiction_panics() -> PanicHookGuard {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    PanicHookGuard { prev: Some(prev) }
}

/// Plays `games` full doubles battles (team preview through `GameOverState`)
/// against ordered teamsheet pairings, then replays each battle's recorded
/// event stream through `apply_information` once per fog-of-war information
/// mode, timing every call — mirroring `benches/battle_sweep.rs`. Bounds work
/// to `games` pairings (clamped to `[1, MAX_INFERENCE_GAMES]`); each battle
/// is capped at `MAX_TURNS` turns as a hang guard.
///
/// `apply_information` panics on a known, already-tracked inference-engine
/// soundness bug (see `benches/bench_common.rs`'s header and
/// `TODO.md`'s S1-S58 history) whenever `learnset_dex` is populated — expected
/// and caught per-call in `advance_one`, counted as a contradiction rather
/// than failing this function.
pub fn run_inference(
    pokemon_dex: &HashMap<Species, PokemonData>,
    move_dex: &HashMap<PokemonMove, MoveData>,
    ability_dex: &HashMap<Ability, AbilityData>,
    learnset_dex: &HashMap<Species, HashSet<PokemonMove>>,
    games: usize,
) -> Result<Vec<InferenceRow>, String> {
    const ACTIVE_PER_SIDE: u8 = 2;
    const BROUGHT_PER_SIDE: u8 = 4;

    let paths = teamsheet_paths();
    if paths.is_empty() {
        return Err("no teamsheets found in ../teamsheets".to_string());
    }
    let games = games.clamp(1, MAX_INFERENCE_GAMES);

    let config = InferenceConfig {
        use_stat_points: true,
        force_max_ivs: true,
        learnset_dex: learnset_dex.clone(),
        ..InferenceConfig::default()
    };

    let mut mode_stats: [ModeStats; 4] = Default::default();
    let _hook_guard = suppress_known_contradiction_panics();

    let mut done = 0usize;
    'outer: for (i, p1_path) in paths.iter().enumerate() {
        for (j, p2_path) in paths.iter().enumerate() {
            if done >= games {
                break 'outer;
            }
            done += 1;
            let seed = (i * paths.len() + j) as u64;
            let mut rng = StdRng::seed_from_u64(seed);

            let preview = team_preview_state_from_teamsheets(
                p1_path
                    .to_str()
                    .ok_or("teamsheet path should be valid UTF-8")?,
                p2_path
                    .to_str()
                    .ok_or("teamsheet path should be valid UTF-8")?,
                pokemon_dex,
                move_dex,
                ACTIVE_PER_SIDE,
                BROUGHT_PER_SIDE,
                true,
            );
            let p1_mons = preview.p1_mons.clone();
            let p2_mons = preview.p2_mons.clone();

            let p1_tp = random_team_preview_command(
                p1_mons.len(),
                ACTIVE_PER_SIDE,
                BROUGHT_PER_SIDE,
                &mut rng,
            );
            let p2_tp = random_team_preview_command(
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
            let mut turn = 0usize;
            loop {
                turn += 1;
                if turn > MAX_TURNS {
                    break; // hang guard; treat as a stalled game and move on
                }

                let was_team_preview = matches!(state, MatchState::TeamPreviewState(_));
                let (next_state, raw_events, _probability) = sample_turn_raw(
                    &state,
                    &p1_cmd,
                    &p2_cmd,
                    move_dex,
                    pokemon_dex,
                    true,
                    16,
                    Some(Player::P1),
                );

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
                        p1_cmd = PlayerCommand::Battle(random_commands_for_player(
                            bs,
                            Player::P1,
                            move_dex,
                            pokemon_dex,
                            &mut rng,
                        ));
                        p2_cmd = PlayerCommand::Battle(random_commands_for_player(
                            bs,
                            Player::P2,
                            move_dex,
                            pokemon_dex,
                            &mut rng,
                        ));
                    }
                    MatchState::TeamPreviewState(_) => {
                        unreachable!("team preview only occurs once, at turn 1")
                    }
                }
            }

            // Step 2: replay the recorded trajectory once per information mode.
            for (mode_idx, &mode) in INFERENCE_MODES.iter().enumerate() {
                if mode == InformationMode::PerfectInformation {
                    continue; // zero-overhead baseline: no belief tracked, nothing to time
                }
                let stats = &mut mode_stats[mode_idx];
                let (mut belief_p1, mut belief_p2) =
                    seed_beliefs(mode, &p1_mons, &p2_mons, pokemon_dex);
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
                            pokemon_dex,
                            move_dex,
                            ability_dex,
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
                            pokemon_dex,
                            move_dex,
                            ability_dex,
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

    // Enumerate BEFORE filtering so `mode_idx` still lines up with
    // `mode_stats`'s indices (which mirror `INFERENCE_MODES`'s full order,
    // PerfectInformation included) — filtering first would shift every
    // later index down and pull the wrong stats for each row.
    let rows = INFERENCE_MODES
        .iter()
        .enumerate()
        .filter(|&(_, &mode)| mode != InformationMode::PerfectInformation)
        .map(|(mode_idx, &mode)| {
            let stats = &mode_stats[mode_idx];
            InferenceRow {
                information_mode: mode_label(mode),
                calls: stats.calls,
                avg_time_secs: stats.time_secs / stats.calls.max(1) as f64,
                contradictions: stats.contradictions,
            }
        })
        .collect();
    Ok(rows)
}
