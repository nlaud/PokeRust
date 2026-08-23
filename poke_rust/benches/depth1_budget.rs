//! Measures what a depth-1 solve costs, and how damage rolls change it.
//!
//! Run from `poke_rust/`:
//! ```sh
//! cargo bench --bench depth1_budget
//! ```
//!
//! The server presets set a turn budget. This benchmark supplies the numbers
//! that size it. It reports one depth-1 solve of a singles position and of a
//! doubles position, over a range of damage-roll counts.
//!
//! The two questions it answers:
//!
//! 1. Does a damage-roll count change the turn count of a depth-1 solve? A
//!    matrix cell calls `simulate_turn` one time, and the rolls change the
//!    branches inside that one call.
//! 2. What does a depth-1 solve cost in seconds? A preset budget must let the
//!    slowest position finish, and a doubles position is the slowest.

use std::collections::HashMap;
use std::io::Write;
use std::time::Instant;

use poke_rust::benchmarking::battle_position;
use poke_rust::data::pokemon_move::PokemonMove;
use poke_rust::data::species::Species;
use poke_rust::solver::belief::{Particle, ParticleBelief};
use poke_rust::solver::chance::ChanceMode;
use poke_rust::solver::ismcts::{self, IsmctsConfig};
use poke_rust::solver::mcts::{self, MctsConfig};
use poke_rust::solver::{
    CancelFlag, JointActionProb, SolveConfig, SolverAlgorithm, pool, solve_seeded_cancellable,
};
use poke_rust::state::battle::{MatchState, Player};
use poke_rust::state::dex_data::{MoveData, PokemonData, parse_move_dex, parse_pokemon_dex};

/// The damage-roll counts that the sweep reports.
const ROLLS: [u8; 7] = [1, 2, 3, 4, 6, 8, 16];

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

fn action_count(
    state: &MatchState,
    player: Player,
    pokemon_dex: &HashMap<Species, PokemonData>,
    move_dex: &HashMap<PokemonMove, MoveData>,
) -> usize {
    use poke_rust::solver::actions;
    let MatchState::BattleState(battle) = state else {
        return 0;
    };
    actions::joint_actions(
        battle,
        player,
        actions::phase_of(state),
        move_dex,
        pokemon_dex,
        None,
        false,
    )
    .actions
    .len()
}

fn support(strategy: &[JointActionProb]) -> usize {
    strategy
        .iter()
        .filter(|action| action.probability > 1e-9)
        .count()
}

/// The configuration that `bin/server/bot.rs::build_search` builds, at depth 1.
fn server_config(damage_rolls: u8, workers: usize) -> SolveConfig {
    SolveConfig {
        depth: 1,
        replacement_depth: None,
        iterative_deepening: false,
        damage_rolls,
        consider_crit: false,
        chance: ChanceMode::Enumerate,
        algorithm: SolverAlgorithm::DoubleOracle,
        policy_order: true,
        max_actions_per_player: None,
        prune_dominated_actions: false,
        node_budget: None,
        deadline: None,
        workers,
        ..SolveConfig::default()
    }
}

fn sweep(
    name: &str,
    state: &MatchState,
    seed: u64,
    pokemon_dex: &HashMap<Species, PokemonData>,
    move_dex: &HashMap<PokemonMove, MoveData>,
    workers: usize,
) {
    let n1 = action_count(state, Player::P1, pokemon_dex, move_dex);
    let n2 = action_count(state, Player::P2, pokemon_dex, move_dex);
    println!("## {name}: {n1} actions against {n2}");
    println!();
    println!(
        "| Damage rolls | Turns | Nodes | Cells | Turns for each cell | Time | Support | Value |"
    );
    println!("|---|---|---|---|---|---|---|---|");
    for rolls in ROLLS {
        let config = server_config(rolls, workers);
        let start = Instant::now();
        let result = solve_seeded_cancellable(seed, state, pokemon_dex, move_dex, &config, None)
            .expect("the position is solvable");
        let elapsed = start.elapsed().as_secs_f64();
        println!(
            "| {rolls} | {} | {} | {} | {:.1} | {:.2}s | {} of {n1}, {} of {n2} | {:.4} |",
            result.stats.turns_simulated,
            result.stats.nodes_expanded,
            result.stats.matrix_cells_evaluated,
            result.stats.turns_simulated as f64
                / result.stats.matrix_cells_evaluated.max(1) as f64,
            elapsed,
            support(&result.p1_strategy),
            support(&result.p2_strategy),
            result.value,
        );
        std::io::stdout().flush().ok();
    }
    println!();
}

/// Measures the turn rate of a sampled search.
///
/// A sampled search spends whatever budget it has, so a budget does not decide
/// whether it finishes. It decides how many seconds the answer takes. This
/// reports the rate that converts one into the other.
///
/// The two sampled searches do not share a rate, and a preset that assumes they
/// do sizes the fog-of-war budget from the wrong search.
///
/// `mcts` resolves a turn with `TransitionMode::Enumerated`, so it builds every
/// branch of the turn and then draws one. A damage roll multiplies that build.
///
/// `ismcts` and `mccfr` ignore `transition` and always call `sample_transition`,
/// which draws one outcome without building the rest. A damage roll only widens
/// the set that the one draw comes from, so it should cost close to nothing.
///
/// This reports both rates so the presets can size each from its own search.
fn sampled_rate(
    state: &MatchState,
    seed: u64,
    pokemon_dex: &HashMap<Species, PokemonData>,
    move_dex: &HashMap<PokemonMove, MoveData>,
) {
    println!("### Sampled search at depth 1");
    println!();
    println!("| Damage rolls | Budget | Turns | Iterations | Time | Turns for each second |");
    println!("|---|---|---|---|---|---|");
    // Each budget is sized so one row takes about a minute. A rate needs a
    // stable sample and not a fixed turn count, and this search becomes far
    // slower for each turn as the rolls rise. One 16-roll turn of a doubles
    // position builds a very large branch set, so that row reads few turns.
    for (rolls, budget) in [(1u8, 100_000u64), (4, 8_000), (16, 600)] {
        let config = MctsConfig {
            iterations: u32::MAX,
            depth: 1,
            replacement_depth: None,
            damage_rolls: rolls,
            consider_crit: false,
            max_actions_per_player: None,
            ..MctsConfig::default()
        };
        let flag = CancelFlag::with_simulation_turn_budget(budget);
        let start = Instant::now();
        let result =
            mcts::search_cancellable(seed, state, pokemon_dex, move_dex, &config, Some(&flag));
        let elapsed = start.elapsed().as_secs_f64();
        match result {
            Ok(found) => println!(
                "| {rolls} | {budget} | {} | {} | {:.2}s | {:.0} |",
                found.stats.turns_simulated,
                found.stats.iterations,
                elapsed,
                found.stats.turns_simulated as f64 / elapsed.max(1e-9),
            ),
            Err(error) => println!("| {rolls} | {budget} | error: {error:?} | | | |"),
        }
        std::io::stdout().flush().ok();
    }
    println!();
}

/// Measures the turn rate of the fog-of-war searches.
///
/// A belief search is the one that a fog-of-war session actually runs, so its
/// rate is the one that `bot::sampled_budget_for` needs.
///
/// The belief here holds copies of one concrete world. That isolates the
/// iteration rate from the determinizer, which draws one time for each particle
/// before the first iteration and never again.
fn belief_rate(
    state: &MatchState,
    seed: u64,
    pokemon_dex: &HashMap<Species, PokemonData>,
    move_dex: &HashMap<PokemonMove, MoveData>,
) {
    println!("### ISMCTS at depth 1, 24 worlds");
    println!();
    println!("| Damage rolls | Budget | Turns | Iterations | Time | Turns for each second |");
    println!("|---|---|---|---|---|---|");
    // The question is whether a roll changes the rate at all. Three points
    // across the whole range answer it.
    for rolls in [1u8, 4, 16] {
        let particles: Vec<Particle> = (0..24)
            .map(|_| Particle {
                state: state.clone(),
                weight: 1.0 / 24.0,
            })
            .collect();
        let belief = ParticleBelief::from_particles(particles).expect("24 worlds is a belief");
        let config = IsmctsConfig {
            search: MctsConfig {
                iterations: u32::MAX,
                depth: 1,
                replacement_depth: None,
                damage_rolls: rolls,
                consider_crit: false,
                max_actions_per_player: None,
                ..MctsConfig::default()
            },
            particles: 24,
            ..IsmctsConfig::default()
        };
        let budget = 100_000u64;
        let flag = CancelFlag::with_simulation_turn_budget(budget);
        let start = Instant::now();
        let result =
            ismcts::search_cancellable(seed, &belief, pokemon_dex, move_dex, &config, Some(&flag));
        let elapsed = start.elapsed().as_secs_f64();
        match result {
            Ok(found) => println!(
                "| {rolls} | {budget} | {} | {} | {:.2}s | {:.0} |",
                found.stats.turns_simulated,
                found.stats.iterations,
                elapsed,
                found.stats.turns_simulated as f64 / elapsed.max(1e-9),
            ),
            Err(error) => println!("| {rolls} | {budget} | error: {error:?} | | | |"),
        }
        std::io::stdout().flush().ok();
    }
    println!();
}

/// Measures how depth scales for the fog-of-war search.
///
/// This is the question that decides the preset depth, and the two search
/// families answer it differently.
///
/// An exact search multiplies its tree by the branch count of a turn for each
/// ply, so depth 2 costs a whole depth-1 solve for each cell of the root matrix.
/// `depth2_cost` measures that, and it is why `PRESET_DEPTH` is 1.
///
/// A belief search draws one outcome per ply, so one iteration costs one
/// `sample_transition` for each ply. Depth should therefore cost a constant
/// factor here, not an exponential one. If it does, a fog-of-war preset can
/// afford lookahead that an exact preset cannot.
fn belief_depth_scaling(
    state: &MatchState,
    seed: u64,
    pokemon_dex: &HashMap<Species, PokemonData>,
    move_dex: &HashMap<PokemonMove, MoveData>,
) {
    println!("### ISMCTS depth scaling, 16 rolls, 24 worlds, 30s of budget");
    println!();
    println!("| Depth | Turns | Iterations | Time | Iterations for each second |");
    println!("|---|---|---|---|---|");
    for depth in [1u8, 2, 3, 4] {
        let particles: Vec<Particle> = (0..24)
            .map(|_| Particle {
                state: state.clone(),
                weight: 1.0 / 24.0,
            })
            .collect();
        let belief = ParticleBelief::from_particles(particles).expect("24 worlds is a belief");
        let config = IsmctsConfig {
            search: MctsConfig {
                iterations: u32::MAX,
                depth,
                replacement_depth: None,
                damage_rolls: 16,
                consider_crit: false,
                max_actions_per_player: None,
                ..MctsConfig::default()
            },
            particles: 24,
            ..IsmctsConfig::default()
        };
        // One fixed budget, so the rows compare what the same spend buys.
        let flag = CancelFlag::with_simulation_turn_budget(100_000);
        let start = Instant::now();
        let result =
            ismcts::search_cancellable(seed, &belief, pokemon_dex, move_dex, &config, Some(&flag));
        let elapsed = start.elapsed().as_secs_f64();
        match result {
            Ok(found) => println!(
                "| {depth} | {} | {} | {:.2}s | {:.0} |",
                found.stats.turns_simulated,
                found.stats.iterations,
                elapsed,
                found.stats.iterations as f64 / elapsed.max(1e-9),
            ),
            Err(error) => println!("| {depth} | error: {error:?} | | | |"),
        }
        std::io::stdout().flush().ok();
    }
    println!();
}

/// Which sections one run reports.
///
/// The exact sweep takes about eight minutes, and the belief sections take
/// about four. A run that only needs one of them passes `exact` or `belief` as
/// the first argument. No argument runs everything.
///
/// ```sh
/// cargo bench --bench depth1_budget -- belief
/// ```
#[derive(PartialEq)]
enum Sections {
    All,
    Exact,
    Belief,
}

impl Sections {
    fn from_args() -> Sections {
        match std::env::args().nth(1).as_deref() {
            Some("exact") => Sections::Exact,
            Some("belief") => Sections::Belief,
            _ => Sections::All,
        }
    }

    fn runs_exact(&self) -> bool {
        *self != Sections::Belief
    }

    fn runs_belief(&self) -> bool {
        *self != Sections::Exact
    }
}

fn main() {
    poke_rust::VERBOSITY.set(0).ok();
    let sections = Sections::from_args();
    let (pokemon_dex, move_dex) = dexes();
    let paths = teamsheet_paths();
    let workers = pool::shared().capacity();

    println!("depth-1 budget probe, {workers} workers");
    println!();

    // Singles first. The pairing is the one that `depth2_cost` reports.
    for (i, j, active, brought, label) in [
        (0usize, 1usize, 1u8, 3u8, "Singles"),
        (10usize, 3usize, 2u8, 4u8, "Doubles"),
    ] {
        let seed = (i * paths.len() + j) as u64;
        let Some(state) = battle_position(
            &paths,
            i,
            j,
            active,
            brought,
            seed,
            &pokemon_dex,
            &move_dex,
        ) else {
            println!("{label}: the pairing did not produce a battle position");
            continue;
        };
        if sections.runs_exact() {
            sweep(label, &state, seed, &pokemon_dex, &move_dex, workers);
        }
        if active == 2 && sections.runs_belief() {
            sampled_rate(&state, seed, &pokemon_dex, &move_dex);
            belief_rate(&state, seed, &pokemon_dex, &move_dex);
            belief_depth_scaling(&state, seed, &pokemon_dex, &move_dex);
        }
    }
}
