//! Turn-resolution and fog-of-war-inference speed measurement, as a library
//! module so both the standalone benches (`poke_rust/benches/turn_speed.rs`,
//! `battle_sweep.rs`) and the web server's `GET /api/benchmark` endpoint
//! (`src/bin/server/routes.rs`, streamed over Server-Sent Events) can drive
//! the same scenarios. The bench binaries print a full historical sweep to
//! stdout (recorded in `benches/RESULTS.md`); this module ports the same
//! grid/timing logic and runs the exact same unbounded sweep — the full
//! ordered N×N teamsheet-pairing grid (`../teamsheets`) — rather than a
//! capped subset, so live results are directly comparable to the offline
//! `cargo bench` numbers. `on_progress` callbacks let a caller report live
//! progress on what is now a genuinely multi-minute call.
//!
//! `run_turn_speed` mirrors `turn_speed.rs`: one post-team-preview turn timed
//! across enumerate (`simulate_turn`) vs sample (`sample_turn`) mode, damage
//! rolls, crit branching, singles vs doubles. `run_inference` mirrors
//! `battle_sweep.rs`: full games played to completion — both singles and
//! doubles, mirroring `run_turn_speed`'s own scenario split (the offline
//! `battle_sweep.rs` this was ported from is doubles-only; singles coverage
//! is new here) — replaying the recorded event stream through
//! `apply_information` once per fog-of-war information mode and timing each
//! call.
//!
//! Doubles enumeration at high roll counts is still excluded via
//! `doubles_enum_ok` — the specific case CLAUDE.md documents as exceeding
//! 15 GB (see `turn_speed.rs`'s header) — that safety cap is orthogonal to
//! and unaffected by running the full pairing grid.

use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
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
use crate::solver::chance::ChanceMode;
use crate::solver::{SolveConfig, SolverAlgorithm, solve};
use crate::state::battle::{
    BattleCommand, BattleState, MatchState, Player, PlayerCommand, TeamPreviewCommand,
};
use crate::state::dex_data::{AbilityData, MoveData, PokemonData};
use crate::state::pokemon::PokemonState;

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

/// `(label, active_per_side, brought_per_side)` — shared by `run_turn_speed`
/// (inline literal, same values) and `run_inference`'s scenario loop.
const INFERENCE_SCENARIOS: [(&str, u8, u8); 2] = [("singles", 1, 3), ("doubles", 2, 4)];

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

/// One `(scenario, information mode)`'s averaged `apply_information` timing
/// across a belief-update sweep — mirrors a row of `battle_sweep.rs`'s
/// "Inference by information mode" table, plus the `scenario` split
/// `run_inference` adds on top of that (see this module's header).
/// `PerfectInformation` tracks no belief at all, so it never appears here
/// (it's the zero-overhead baseline).
#[derive(Clone, Debug)]
pub struct InferenceRow {
    pub scenario: &'static str, // "singles" | "doubles"
    pub information_mode: &'static str,
    pub calls: u64,
    pub avg_time_secs: f64,
    pub contradictions: u64,
    /// A real caught panic message from the first contradiction (see
    /// `ModeStats::sample_message`) — always an `apply_information` panic
    /// (typically `inference_contradiction!`), never a `[subset violation]`
    /// (that's a separate check, `subset_check::assert_true_state_subset_of_belief`,
    /// this module never calls). `None` when `contradictions == 0`.
    pub contradiction_sample: Option<String>,
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
/// crit × singles/doubles grid, mirroring `benches/turn_speed.rs` exactly —
/// walks the full ordered N×N teamsheet-pairing grid (`../teamsheets`; 14
/// files ⇒ 196 pairings today), the same unbounded sweep the offline
/// `cargo bench --bench turn_speed` runs (root README: "takes a couple of
/// minutes"). `on_progress(completed, total)` is called once per pairing
/// (both scenarios timed together within it).
pub fn run_turn_speed(
    pokemon_dex: &HashMap<Species, PokemonData>,
    move_dex: &HashMap<PokemonMove, MoveData>,
    on_progress: &mut dyn FnMut(usize, usize),
) -> Result<Vec<TurnSpeedRow>, String> {
    let paths = teamsheet_paths();
    if paths.is_empty() {
        return Err("no teamsheets found in ../teamsheets".to_string());
    }
    let total_pairings = paths.len() * paths.len();

    let mut cells: HashMap<(&'static str, &'static str, u8, bool), SpeedCell> = HashMap::new();
    let mut done = 0usize;

    for (i, p1_path) in paths.iter().enumerate() {
        for (j, p2_path) in paths.iter().enumerate() {
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

            done += 1;
            on_progress(done, total_pairings);
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
    /// The first caught panic message, kept as a concrete example of what
    /// "contradictions" actually are (always an `apply_information` panic —
    /// typically `inference_contradiction!`; see `advance_one`). Only the
    /// first is kept, not one per contradiction, to stay lightweight.
    sample_message: Option<String>,
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

/// Seeds both players' beliefs from the team-preview roster, per `mode` and
/// `(active_per_side, brought_per_side)` — parametrized (rather than the
/// fixed doubles constants `benches/battle_sweep.rs::seed_beliefs` hardcodes)
/// so `run_inference` can drive this for both singles and doubles. Callers
/// must skip `PerfectInformation` (it tracks no belief).
fn seed_beliefs(
    mode: InformationMode,
    p1_mons: &[PokemonState],
    p2_mons: &[PokemonState],
    pokemon_dex: &HashMap<Species, PokemonData>,
    active_per_side: u8,
    brought_per_side: u8,
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
                active_per_side,
                brought_per_side,
                50,
                true,
            ),
            UnknownMatchState::team_preview_closed_sheet_from_perspective(
                Player::P2,
                p2_mons,
                p1_mons,
                pokemon_dex,
                active_per_side,
                brought_per_side,
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
                active_per_side,
                brought_per_side,
                50,
                mode,
                true,
            ),
            UnknownMatchState::team_preview_open_sheet_from_perspective(
                Player::P2,
                p2_mons,
                p1_mons,
                pokemon_dex,
                active_per_side,
                brought_per_side,
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
            if stats.sample_message.is_none() {
                // Read via the hook-captured message (see `install_contradiction_hook`),
                // not by downcasting this `Err`'s payload directly — by the time it
                // reaches here it can be re-boxed (the self-healing machinery around
                // `apply_information` catches and re-raises internally in places), so
                // a `downcast_ref::<String>()` here can miss even though the panic
                // really was an ordinary `panic!("...")`. The hook runs at the
                // *original* panic site, before any of that rewrapping, so it always
                // sees the true message.
                stats.sample_message = LAST_PANIC_MESSAGE.with(|m| m.borrow_mut().take());
            }
            (backup, true)
        }
    }
}

thread_local! {
    /// Set by `install_contradiction_hook`'s hook at the moment a panic actually
    /// fires (not read back until `advance_one` needs it) — see that function's
    /// doc comment for why this, rather than downcasting the `catch_unwind` `Err`
    /// payload, is the reliable way to recover a caught panic's message here.
    static LAST_PANIC_MESSAGE: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
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

/// Installs the hook that both suppresses the default stderr dump and
/// records the panic's message into `LAST_PANIC_MESSAGE` — reading
/// `info.payload()` here, at the true panic site, rather than downcasting
/// `catch_unwind`'s `Err` payload later in `advance_one` (see that call
/// site's comment for why the two can disagree).
fn install_contradiction_hook() -> PanicHookGuard {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(|info| {
        let message = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| info.to_string());
        LAST_PANIC_MESSAGE.with(|m| *m.borrow_mut() = Some(message));
    }));
    PanicHookGuard { prev: Some(prev) }
}

/// Plays full games (team preview through `GameOverState`) against every
/// ordered teamsheet pairing (`../teamsheets`; 14×14 = 196 pairings today),
/// for both singles and doubles (see `INFERENCE_SCENARIOS`), then replays
/// each game's recorded event stream through `apply_information` once per
/// fog-of-war information mode, timing every call — mirroring
/// `benches/battle_sweep.rs`, extended with the singles scenario and run
/// unbounded like `run_turn_speed`. Each game is capped at `MAX_TURNS` turns
/// as a hang guard. `on_progress(completed, total)` is called once per
/// `(scenario, pairing)` unit — `total` = 196 × 2 = 392.
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
    on_progress: &mut dyn FnMut(usize, usize),
) -> Result<Vec<InferenceRow>, String> {
    let paths = teamsheet_paths();
    if paths.is_empty() {
        return Err("no teamsheets found in ../teamsheets".to_string());
    }
    let total_units = paths.len() * paths.len() * INFERENCE_SCENARIOS.len();

    let config = InferenceConfig {
        use_stat_points: true,
        force_max_ivs: true,
        learnset_dex: learnset_dex.clone(),
        ..InferenceConfig::default()
    };

    // Keyed by (scenario, mode label) rather than the `InformationMode` enum
    // itself — it derives `Eq` but not `Hash`, and adding `Hash` there is out
    // of scope for a benchmarking-only need; string keys mirror the pattern
    // `run_turn_speed`'s own `cells` map already uses.
    let mut mode_stats: HashMap<(&'static str, &'static str), ModeStats> = HashMap::new();
    let _hook_guard = install_contradiction_hook();

    let mut done = 0usize;
    for (i, p1_path) in paths.iter().enumerate() {
        for (j, p2_path) in paths.iter().enumerate() {
            let seed_base = (i * paths.len() + j) as u64;

            for (scenario_idx, &(scenario, active_per_side, brought_per_side)) in
                INFERENCE_SCENARIOS.iter().enumerate()
            {
                // Distinct seed per scenario so singles/doubles don't replay
                // identical RNG draws off the same pairing's base seed.
                let seed = seed_base * INFERENCE_SCENARIOS.len() as u64 + scenario_idx as u64;
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
                    active_per_side,
                    brought_per_side,
                    true,
                );
                let p1_mons = preview.p1_mons.clone();
                let p2_mons = preview.p2_mons.clone();

                let p1_tp = random_team_preview_command(
                    p1_mons.len(),
                    active_per_side,
                    brought_per_side,
                    &mut rng,
                );
                let p2_tp = random_team_preview_command(
                    p2_mons.len(),
                    active_per_side,
                    brought_per_side,
                    &mut rng,
                );

                let mut state = MatchState::TeamPreviewState(preview);
                let mut p1_cmd = PlayerCommand::TeamPreview(p1_tp);
                let mut p2_cmd = PlayerCommand::TeamPreview(p2_tp);

                // Step 1: resolve the game once, recording every turn.
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
                for &mode in &INFERENCE_MODES {
                    if mode == InformationMode::PerfectInformation {
                        continue; // zero-overhead baseline: no belief tracked, nothing to time
                    }
                    let stats = mode_stats
                        .entry((scenario, mode_label(mode)))
                        .or_default();
                    let (mut belief_p1, mut belief_p2) = seed_beliefs(
                        mode,
                        &p1_mons,
                        &p2_mons,
                        pokemon_dex,
                        active_per_side,
                        brought_per_side,
                    );
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

                done += 1;
                on_progress(done, total_units);
            }
        }
    }

    let mut rows = Vec::with_capacity(mode_stats.len());
    for &(scenario, _, _) in &INFERENCE_SCENARIOS {
        for &mode in &INFERENCE_MODES {
            if mode == InformationMode::PerfectInformation {
                continue;
            }
            if let Some(stats) = mode_stats.get(&(scenario, mode_label(mode))) {
                rows.push(InferenceRow {
                    scenario,
                    information_mode: mode_label(mode),
                    calls: stats.calls,
                    avg_time_secs: stats.time_secs / stats.calls.max(1) as f64,
                    contradictions: stats.contradictions,
                    contradiction_sample: stats.sample_message.clone(),
                });
            }
        }
    }
    Ok(rows)
}

// ─────────────────────────────────────────────────────────────────────────────
// Game-tree solver
// ─────────────────────────────────────────────────────────────────────────────

/// Doubles action cap. A doubles slot offers 15–20 commands, so the two slots
/// together produce a few hundred joint actions and a matrix with tens of
/// thousands of cells — every one of them a `simulate_turn` call. Capping keeps
/// the doubles rows comparable to the singles rows instead of being a single
/// measurement of "too slow"; the resulting equilibrium is over a subset of the
/// real options, which is why the cap is printed alongside the timings.
const DOUBLES_ACTION_CAP: usize = 24;

/// Typical joint-action count per player in singles. Only used to predict a
/// cell's cost before running it. Calibrated against the measured depth-1
/// backward-induction cell counts in `benches/RESULTS.md`, which are ~51 — a
/// roughly 7×7 matrix.
const SINGLES_ACTION_ESTIMATE: f64 = 7.5;

/// Skip any cell predicted to need more turn resolutions than this. At the
/// ~270 µs per resolution recorded in `benches/RESULTS.md` this is a couple of
/// minutes for a single solve; [`MAX_CELL_SECONDS`] then stops the cell from
/// taking more than one such solve.
const MAX_ESTIMATED_TURNS: f64 = 400_000.0;

/// Turn-resolution budget per grid cell, spent on as many teamsheet pairings as
/// fit. Cheap cells therefore average over many pairings and expensive ones over
/// few, which is the only way a single grid can span three orders of magnitude
/// of cost without either being noise or running for hours.
const TURN_BUDGET_PER_CELL: f64 = 120_000.0;

/// Hard wall-clock stop per cell, in case the cost model is wrong about a
/// pairing. Whatever pairings completed are still reported.
const MAX_CELL_SECONDS: f64 = 30.0;

const SOLVER_ALGORITHMS: [(&str, SolverAlgorithm); 3] = [
    ("backwardInduction", SolverAlgorithm::BackwardInduction),
    ("serializedBounds", SolverAlgorithm::SerializedBounds),
    ("doubleOracle", SolverAlgorithm::DoubleOracle),
];
const SOLVER_DEPTHS: [u8; 3] = [1, 2, 3];
const SOLVER_ROLLS: [u8; 2] = [1, 4];
const SOLVER_CHANCE: [(&str, ChanceMode); 3] = [
    ("enumerate", ChanceMode::Enumerate),
    ("top4", ChanceMode::TopK(4)),
    ("top1", ChanceMode::TopK(1)),
];

/// One `(scenario, algorithm, depth, rolls, chance)` cell of the solver sweep.
///
/// `avg_turns_simulated` is the number that matters: a `simulate_turn` call
/// costs hundreds of microseconds while a matrix LP costs a few, so wall-clock
/// time is very nearly turn count times a constant. The ratio of
/// `avg_cells_evaluated` to `avg_cells_total` is what the pruning actually
/// bought.
#[derive(Clone, Debug)]
pub struct SolverRow {
    pub scenario: &'static str, // "singles" | "doubles"
    pub algorithm: &'static str,
    pub depth: u8,
    pub rolls: u8,
    pub chance: &'static str,
    /// Joint-action cap in force, if any. `None` means the full action set.
    pub action_cap: Option<usize>,
    pub avg_time_secs: f64,
    pub avg_nodes: f64,
    pub avg_turns_simulated: f64,
    pub avg_cells_evaluated: f64,
    pub avg_cells_total: f64,
    pub avg_lps: f64,
    /// Teamsheet pairings averaged over. Zero means the cell was skipped as too
    /// expensive to attempt.
    pub pairings: usize,
    /// Why the cell was skipped, when `pairings == 0`.
    pub skipped: Option<&'static str>,
}

/// Predicted turn resolutions for one solve, as backward induction would do it.
///
/// A node evaluates `actions²` cells; each cell expands `branches` successors;
/// each successor is another node. So the tree is `(actions² · branches)^depth`
/// resolutions, give or take — and at depth 1 that correctly collapses to
/// `actions²`, since the successors are scored statically rather than expanded.
///
/// Deliberately conservative, and it does not model mid-turn decision points: a
/// faint leaves a replacement phase that recurses *without* consuming a ply, so
/// a position where Pokemon are dying costs more than this predicts. That is
/// what [`MAX_CELL_SECONDS`] backstops.
fn estimated_turns(scenario: &str, depth: u8, rolls: u8, chance: ChanceMode) -> f64 {
    let actions = if scenario == "doubles" {
        DOUBLES_ACTION_CAP as f64
    } else {
        SINGLES_ACTION_ESTIMATE
    };
    let branches = match chance {
        // Successor counts recorded for singles enumeration in
        // `benches/RESULTS.md`; every other mode caps the count itself.
        ChanceMode::Enumerate => match rolls {
            1 => 5.0,
            2 => 14.0,
            _ => 44.0,
        },
        ChanceMode::TopK(k) => (k as f64).max(1.0),
        ChanceMode::Threshold(_) => 4.0,
        ChanceMode::Sample(n) => (n as f64).max(1.0),
    };
    let per_node = actions * actions;
    (per_node * branches).powi(depth as i32) / branches
}

/// How an algorithm's real cost compares to the backward-induction estimate.
///
/// Both figures are measured, from the sweep's own `turns` column. Double oracle
/// only ever fills the cells its restricted game reaches. Serialized bounds is
/// *slower* than backward induction here — it converts turn resolutions into
/// skipped matrix cells, and in this engine a turn resolution is the expensive
/// thing — hence a factor below one.
///
/// Used only to schedule the sweep; nothing in the reported results depends on
/// these numbers.
fn algorithm_speedup(algorithm: SolverAlgorithm) -> f64 {
    match algorithm {
        SolverAlgorithm::BackwardInduction => 1.0,
        SolverAlgorithm::SerializedBounds => 0.5,
        SolverAlgorithm::DoubleOracle => 3.0,
    }
}

/// Whether a grid cell is worth attempting, and why not if it is not.
fn solver_cell_ok(
    scenario: &str,
    algorithm: SolverAlgorithm,
    depth: u8,
    rolls: u8,
    chance: ChanceMode,
) -> Result<(), &'static str> {
    // High roll counts multiply the successor count that `ChanceMode` then
    // truncates away, so pairing them with a truncating mode measures
    // `simulate_turn`'s cost rather than the solver's. Enumerate is the only
    // mode where the roll count reaches the search at all.
    if rolls > 1 && !matches!(chance, ChanceMode::Enumerate) {
        return Err("rolls>1 only informative under enumerate");
    }
    // Doubles is bounded by its action space, not its depth: even capped, one
    // ply already costs what several singles plies do.
    if scenario == "doubles" && depth > 1 {
        return Err("doubles beyond one ply");
    }
    let estimate = estimated_turns(scenario, depth, rolls, chance) / algorithm_speedup(algorithm);
    if estimate > MAX_ESTIMATED_TURNS {
        return Err("over the estimated-cost ceiling");
    }
    Ok(())
}

#[derive(Default)]
struct SolverCell {
    total_secs: f64,
    pairings: usize,
    nodes: f64,
    turns: f64,
    cells_evaluated: f64,
    cells_total: f64,
    lps: f64,
}

/// Sweep the game-tree solver across algorithms, depths, damage rolls and
/// chance-node policies, in singles and doubles.
///
/// Each cell averages over as many `../teamsheets` pairings as its cost budget
/// allows — see [`TURN_BUDGET_PER_CELL`] — using the same
/// teamsheet → team preview → highest-probability branch construction as
/// [`run_turn_speed`], so the positions are real mid-turn-one states rather than
/// synthetic ones. `on_progress(completed, total)` fires once per cell.
///
/// Set `VERBOSITY` to 0 before calling; the engine's tracing is keyed off that
/// global and a sweep performs millions of turn resolutions.
pub fn run_solver(
    pokemon_dex: &HashMap<Species, PokemonData>,
    move_dex: &HashMap<PokemonMove, MoveData>,
    on_progress: &mut dyn FnMut(usize, usize),
) -> Result<Vec<SolverRow>, String> {
    let paths = teamsheet_paths();
    if paths.is_empty() {
        return Err("no teamsheets found in ../teamsheets".to_string());
    }

    let grid: Vec<(&'static str, u8, u8)> = vec![("singles", 1, 3), ("doubles", 2, 4)];
    let total_cells =
        grid.len() * SOLVER_ALGORITHMS.len() * SOLVER_DEPTHS.len() * SOLVER_ROLLS.len()
            * SOLVER_CHANCE.len();
    let mut rows = Vec::with_capacity(total_cells);
    let mut done = 0usize;

    for (scenario, active, brought) in grid {
        for (algorithm_label, algorithm) in SOLVER_ALGORITHMS {
            for depth in SOLVER_DEPTHS {
                for rolls in SOLVER_ROLLS {
                    for (chance_label, chance) in SOLVER_CHANCE {
                        done += 1;
                        on_progress(done, total_cells);

                        let action_cap = (scenario == "doubles").then_some(DOUBLES_ACTION_CAP);
                        let mut row = SolverRow {
                            scenario,
                            algorithm: algorithm_label,
                            depth,
                            rolls,
                            chance: chance_label,
                            action_cap,
                            avg_time_secs: 0.0,
                            avg_nodes: 0.0,
                            avg_turns_simulated: 0.0,
                            avg_cells_evaluated: 0.0,
                            avg_cells_total: 0.0,
                            avg_lps: 0.0,
                            pairings: 0,
                            skipped: None,
                        };

                        if let Err(reason) =
                            solver_cell_ok(scenario, algorithm, depth, rolls, chance)
                        {
                            row.skipped = Some(reason);
                            rows.push(row);
                            continue;
                        }

                        let config = SolveConfig {
                            depth,
                            damage_rolls: rolls,
                            consider_crit: false,
                            chance,
                            algorithm,
                            max_actions_per_player: action_cap,
                            ..SolveConfig::default()
                        };
                        // Spend the cell's budget on as many pairings as fit —
                        // never fewer than one, or the cell says nothing.
                        let estimate = estimated_turns(scenario, depth, rolls, chance)
                            / algorithm_speedup(algorithm);
                        let target_pairings =
                            ((TURN_BUDGET_PER_CELL / estimate.max(1.0)) as usize).clamp(1, 24);

                        let cell = measure_solver_cell(
                            &paths,
                            active,
                            brought,
                            &config,
                            target_pairings,
                            pokemon_dex,
                            move_dex,
                        )?;

                        if cell.pairings == 0 {
                            row.skipped = Some("no pairing produced a battle position");
                        } else {
                            let n = cell.pairings as f64;
                            row.pairings = cell.pairings;
                            row.avg_time_secs = cell.total_secs / n;
                            row.avg_nodes = cell.nodes / n;
                            row.avg_turns_simulated = cell.turns / n;
                            row.avg_cells_evaluated = cell.cells_evaluated / n;
                            row.avg_cells_total = cell.cells_total / n;
                            row.avg_lps = cell.lps / n;
                        }
                        rows.push(row);
                    }
                }
            }
        }
    }

    Ok(rows)
}

/// Time one grid cell over up to `target_pairings` teamsheet pairings, stopping
/// early if the cell exceeds its wall-clock allowance.
#[allow(clippy::too_many_arguments)]
fn measure_solver_cell(
    paths: &[PathBuf],
    active: u8,
    brought: u8,
    config: &SolveConfig,
    target_pairings: usize,
    pokemon_dex: &HashMap<Species, PokemonData>,
    move_dex: &HashMap<PokemonMove, MoveData>,
) -> Result<SolverCell, String> {
    let mut cell = SolverCell::default();

    for index in 0..target_pairings {
        if cell.total_secs > MAX_CELL_SECONDS {
            break;
        }
        // Stride through the ordered pairing grid rather than taking a prefix,
        // so a cell that affords few pairings still samples across teams instead
        // of measuring one matchup repeatedly.
        let i = (index * 5) % paths.len();
        let j = (index * 7 + 3) % paths.len();
        let seed = (i * paths.len() + j) as u64;

        let Some(state) = battle_position(paths, i, j, active, brought, seed, pokemon_dex, move_dex)
        else {
            continue;
        };

        let start = Instant::now();
        let result = match solve(&state, pokemon_dex, move_dex, config) {
            Ok(result) => result,
            // A pairing that lands on an already-decided or preview position has
            // nothing to solve; skip it rather than failing the sweep.
            Err(_) => continue,
        };
        let elapsed = start.elapsed().as_secs_f64();

        cell.total_secs += elapsed;
        cell.pairings += 1;
        cell.nodes += result.stats.nodes_expanded as f64;
        cell.turns += result.stats.turns_simulated as f64;
        cell.cells_evaluated += result.stats.matrix_cells_evaluated as f64;
        cell.cells_total += result.stats.matrix_cells_total as f64;
        cell.lps += result.stats.lps_solved as f64;
    }

    Ok(cell)
}

/// Resolve one teamsheet pairing's team preview into a real mid-turn-one battle
/// position — the same construction [`run_turn_speed`] uses.
#[allow(clippy::too_many_arguments)]
fn battle_position(
    paths: &[PathBuf],
    i: usize,
    j: usize,
    active: u8,
    brought: u8,
    seed: u64,
    pokemon_dex: &HashMap<Species, PokemonData>,
    move_dex: &HashMap<PokemonMove, MoveData>,
) -> Option<MatchState> {
    let mut rng = StdRng::seed_from_u64(seed);
    let preview = team_preview_state_from_teamsheets(
        paths[i].to_str()?,
        paths[j].to_str()?,
        pokemon_dex,
        move_dex,
        active,
        brought,
        true,
    );
    let p1 = random_team_preview_command(preview.p1_mons.len(), active, brought, &mut rng);
    let p2 = random_team_preview_command(preview.p2_mons.len(), active, brought, &mut rng);

    // Break probability ties on the state hash, not on list position.
    //
    // `simulate_turn` returns its branches sorted by probability but builds them
    // by draining a `HashMap`, so equally-likely branches arrive in an order that
    // varies from run to run — and `max_by` returns the *last* maximum. Without
    // the tiebreak, two runs of the same seed can pick different leads and
    // therefore benchmark different positions, which shows up as work counts
    // that refuse to reproduce.
    let state = simulate_turn(
        &MatchState::TeamPreviewState(preview),
        &PlayerCommand::TeamPreview(p1),
        &PlayerCommand::TeamPreview(p2),
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
    .max_by(|a, b| a.2.total_cmp(&b.2).then_with(|| b.0.cmp(&a.0)))?
    .1;

    matches!(state, MatchState::BattleState(_)).then_some(state)
}
