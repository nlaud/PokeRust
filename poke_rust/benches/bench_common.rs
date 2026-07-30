//! Provides shared helpers for `turn_speed.rs` and `battle_sweep.rs`.
//!
//! Each benchmark is a separate crate.
//! `autobenches = false` prevents Cargo from running this file as a benchmark.
//! Each benchmark imports this module with a path attribute.
//!
//! The command helpers match the helpers in `random_battle_tests.rs`.
//! They select legal commands with equal probability.
//! The tests confirm that this process can complete battles.
//!
//! Inference has two known soundness defects when `learnset_dex` contains data.
//! Pass 5 can cross stat bounds.
//! Learnset inference can reject a valid Illusion user.
//! `apply_information` panics for these contradictions.
//! Callers use `catch_unwind` and stop belief tracking after a panic.
//!
//! A fixed `StdRng` only reproduces the team-preview leads in a full battle.
//! The engine uses `thread_rng()` for later random effects.
//! These effects change later states and legal commands.
//! Thus, full battles can have different commands, turns, and events.
//! The single-turn benchmark completes its selection before these effects occur.

// Each benchmark imports a different helper subset.
// Permit unused helpers in each separate benchmark crate.
#![allow(dead_code)]

use std::collections::HashMap;
use std::path::PathBuf;

use rand::Rng;
use rand::rngs::StdRng;
use rand::seq::SliceRandom;

use poke_rust::data::ability::Ability;
use poke_rust::data::pokemon_move::PokemonMove;
use poke_rust::data::species::Species;
use poke_rust::information::unknowns::UnknownMatchState;
use poke_rust::simulator::{
    get_possible_commands_for_active_slot, validate_battle_command_combination,
};
use poke_rust::state::battle::{
    BattleCommand, BattleState, Player, PlayerCommand, TeamPreviewCommand,
};
use poke_rust::state::dex_data::{
    AbilityData, MoveData, PokemonData, parse_ability_dex, parse_learnset_dex, parse_move_dex,
    parse_pokemon_dex,
};

/// Hang guard only, not a soundness/quality property — real doubles games
/// settle in a few turns. This limit also permits a long PP stall.
/// Matches the precedent in `random_battle_tests.rs`.
pub const MAX_TURNS: usize = 400;

pub struct Dexes {
    pub pokemon_dex: HashMap<Species, PokemonData>,
    pub move_dex: HashMap<PokemonMove, MoveData>,
    pub ability_dex: HashMap<Ability, AbilityData>,
    pub learnset_dex: HashMap<Species, std::collections::HashSet<PokemonMove>>,
}

/// Parses all four dexes from the paths the CLI/server default to, relative
/// to `poke_rust/` (both benches run from there via `cargo bench`).
pub fn load_dexes() -> Dexes {
    Dexes {
        pokemon_dex: parse_pokemon_dex("../pokemon_info/showdownDex.txt"),
        move_dex: parse_move_dex("../pokemon_info/showdownMoves.txt"),
        ability_dex: parse_ability_dex("../pokemon_info/showdownAbilities.txt"),
        learnset_dex: parse_learnset_dex("../pokemon_info/showdownLearnsets.txt"),
    }
}

/// Every `../teamsheets/*.txt` file, sorted by filename for a stable,
/// reproducible ordering across runs (so a fixed seed reproduces the same
/// pairing sequence for any directory order).
pub fn teamsheet_paths() -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir("../teamsheets")
        .expect("../teamsheets directory should exist")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "txt"))
        .collect();
    paths.sort();
    paths
}

/// Picks a random legal team-preview pick: `brought_per_side` distinct roster
/// indices (clamped to roster size), the first `active_per_side` of which
/// lead. Mirrors the counting in `user.rs::choose_team_preview_command`
/// (`brought = min(brought_per_side, total); active_n = min(active_per_side, brought)`).
pub fn random_team_preview_command(
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
/// of `player` this turn. `get_possible_commands_for_active_slot` already
/// handles every per-slot special case on its own (self-switch-pending, a
/// fainted mon awaiting replacement, recharge/semi-invulnerable/charging/
/// rampage locks, choice lock, Encore/Taunt/Torment/Imprison, Struggle
/// fallback) — the only thing left for the caller to enforce is *joint*
/// legality across slots, which `validate_battle_command_combination` checks.
pub fn random_commands_for_player(
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

    // Deterministic fallback guaranteeing forward progress: attacks/Struggle/
    // Pass never conflict jointly, so preferring a non-Switch option per slot
    // always validates.
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

/// Re-seeds a team-preview belief into a battle-level belief on the
/// team-preview -> battle transition, mirroring `session.rs::advance_belief`'s
/// two-step dance: `into_battle_state` structurally seeds it (viewer fully
/// known, opponent's whole roster parked in `possible_back`), and the caller
/// then runs this transition's own event log through `apply_information`
/// (`is_team_preview` is `false` on every call, same as every other turn).
pub fn reseed_for_battle(
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

/// Pretty-prints a duration in seconds as µs/ms/s, whichever reads best.
pub fn fmt_time(seconds: f64) -> String {
    if seconds >= 1.0 {
        format!("{:.2} s", seconds)
    } else if seconds >= 0.001 {
        format!("{:.2} ms", seconds * 1000.0)
    } else {
        format!("{:.0} µs", seconds * 1_000_000.0)
    }
}
