//! Measures what a doubles position answers inside a wall-clock limit.
//!
//! Run from `poke_rust/`:
//! ```sh
//! cargo bench --bench doubles_wallclock
//! ```
//!
//! `depth2_cost` counts turn simulations, which is what a budget spends. This
//! benchmark asks a different question: what depth does a doubles position reach
//! in 30 seconds, and which search reaches it?
//!
//! An exact search builds a matrix, so its cost grows with the square of the
//! action count. A sampled search walks one trajectory for each iteration, so
//! its cost grows with the iteration count alone. The two therefore answer a
//! 470-action position very differently.

use std::collections::HashMap;
use std::io::Write;
use std::sync::Arc;
use std::time::{Duration, Instant};

use poke_rust::benchmarking::battle_position;
use poke_rust::data::pokemon_move::PokemonMove;
use poke_rust::data::species::Species;
use poke_rust::solver::chance::ChanceMode;
use poke_rust::solver::mcts::{self, MctsConfig, SelectionPolicy, TransitionMode, Widening};
use poke_rust::solver::{
    CancelFlag, JointActionProb, SolveConfig, SolverAlgorithm, pool,
    refine_seeded_progress_cancellable, solve_seeded_cancellable,
};
use poke_rust::state::battle::{MatchState, Player};
use poke_rust::state::dex_data::{MoveData, PokemonData, parse_move_dex, parse_pokemon_dex};

/// The wall-clock limit that one world must answer inside.
const LIMIT: Duration = Duration::from_secs(30);

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

/// Raises `flag` after `limit`, and stops the timer when the caller drops it.
struct Timer {
    done: Arc<std::sync::atomic::AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Timer {
    fn start(flag: &CancelFlag, limit: Duration) -> Timer {
        let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = flag.clone();
        let watch = Arc::clone(&done);
        let handle = std::thread::spawn(move || {
            let deadline = Instant::now() + limit;
            while Instant::now() < deadline {
                if watch.load(std::sync::atomic::Ordering::Relaxed) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            flag.cancel();
        });
        Timer {
            done,
            handle: Some(handle),
        }
    }
}

impl Drop for Timer {
    fn drop(&mut self) {
        self.done.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            handle.join().ok();
        }
    }
}

/// The strongest few actions of a strategy, for an eyeball comparison.
fn top_actions(strategy: &[JointActionProb]) -> String {
    let mut sorted: Vec<&JointActionProb> = strategy.iter().collect();
    sorted.sort_by(|a, b| b.probability.total_cmp(&a.probability));
    sorted
        .iter()
        .take(3)
        .map(|action| format!("{:.2}", action.probability))
        .collect::<Vec<_>>()
        .join(" ")
}

fn main() {
    poke_rust::VERBOSITY.set(0).ok();
    let (pokemon_dex, move_dex) = dexes();
    let paths = teamsheet_paths();
    let workers = pool::shared().capacity();

    // The cheapest doubles pairing that `depth2_cost` reports, at 290 x 370.
    let (i, j) = (10usize, 3usize);
    let seed = (i * paths.len() + j) as u64;
    let state = battle_position(&paths, i, j, 2, 4, seed, &pokemon_dex, &move_dex)
        .expect("the pairing must produce a battle position");

    println!("doubles wall-clock probe, pairing {i}x{j}, {workers} workers, {LIMIT:?} limit");
    println!();

    // ── Throughput ──────────────────────────────────────────────────────────
    // Depth 1 finishes, so it measures turns for each second without a stop.
    for parallel in [1usize, workers] {
        let config = SolveConfig {
            depth: 1,
            iterative_deepening: false,
            damage_rolls: 1,
            consider_crit: false,
            chance: ChanceMode::Enumerate,
            algorithm: SolverAlgorithm::DoubleOracle,
            policy_order: true,
            max_actions_per_player: None,
            prune_dominated_actions: false,
            node_budget: None,
            deadline: None,
            workers: parallel,
            ..SolveConfig::default()
        };
        let start = Instant::now();
        let result = solve_seeded_cancellable(seed, &state, &pokemon_dex, &move_dex, &config, None)
            .expect("the position is solvable");
        let elapsed = start.elapsed().as_secs_f64();
        println!(
            "throughput  depth 1, {parallel:>2} workers: {} turns in {:.2}s = {:.0} turns/s",
            result.stats.turns_simulated,
            elapsed,
            result.stats.turns_simulated as f64 / elapsed,
        );
        if parallel > 1 {
            let n1 = action_count(&state, Player::P1, &pokemon_dex, &move_dex);
            let n2 = action_count(&state, Player::P2, &pokemon_dex, &move_dex);
            let support = |strategy: &[JointActionProb]| {
                strategy
                    .iter()
                    .filter(|action| action.probability > 1e-9)
                    .count()
            };
            let sup1 = support(&result.p1_strategy);
            let sup2 = support(&result.p2_strategy);
            println!(
                "            depth-1 support {sup1} of {n1} and {sup2} of {n2}, value {:.4}",
                result.value
            );
            // One depth-2 cell is one turn plus a depth-1 solve of the child.
            let cell = result.stats.turns_simulated;
            println!(
                "            one depth-2 cell costs about {cell} turns, so one best-response",
            );
            println!(
                "            scan over {n1} rows costs about {} turns, and the support cells",
                n1 as u64 * cell
            );
            println!(
                "            alone cost about {} turns",
                (sup1 * sup2) as u64 * cell
            );
        }
        std::io::stdout().flush().ok();
    }
    println!();

    // ── Exact depth 2 inside the limit ──────────────────────────────────────
    {
        let config = SolveConfig {
            depth: 2,
            iterative_deepening: true,
            damage_rolls: 1,
            consider_crit: false,
            chance: ChanceMode::TopK(1),
            algorithm: SolverAlgorithm::DoubleOracle,
            policy_order: true,
            max_actions_per_player: None,
            prune_dominated_actions: false,
            node_budget: None,
            deadline: None,
            workers,
            ..SolveConfig::default()
        };
        let flag = CancelFlag::default();
        let timer = Timer::start(&flag, LIMIT);
        let start = Instant::now();
        let result = solve_seeded_cancellable(
            seed,
            &state,
            &pokemon_dex,
            &move_dex,
            &config,
            Some(&flag),
        )
        .expect("the position is solvable");
        let elapsed = start.elapsed().as_secs_f64();
        drop(timer);
        println!(
            "exact d2    top1: reached depth {} in {:.1}s, {} turns, value {:.4}",
            result.depth_reached, elapsed, result.stats.turns_simulated, result.value,
        );
        println!("            warnings: {:?}", result.warnings);
        std::io::stdout().flush().ok();
    }
    println!();

    // ── Refinement inside the limit ─────────────────────────────────────────
    // Solve depth 1, then raise the cells that decide the answer to depth 2.
    for limit in [Duration::from_secs(10), LIMIT, Duration::from_secs(60)] {
        let config = SolveConfig {
            depth: 2,
            iterative_deepening: false,
            damage_rolls: 1,
            consider_crit: false,
            chance: ChanceMode::TopK(1),
            algorithm: SolverAlgorithm::DoubleOracle,
            policy_order: true,
            max_actions_per_player: None,
            prune_dominated_actions: false,
            node_budget: None,
            deadline: None,
            workers,
            ..SolveConfig::default()
        };
        let flag = CancelFlag::default();
        let rounds = std::cell::Cell::new(0usize);
        let first = std::cell::Cell::new(f64::NAN);
        let hook = |round: poke_rust::solver::RootRound| {
            if rounds.get() == 0 {
                first.set(round.value);
            }
            rounds.set(rounds.get() + 1);
        };
        let timer = Timer::start(&flag, limit);
        let start = Instant::now();
        let (result, report) = refine_seeded_progress_cancellable(
            seed,
            &state,
            &pokemon_dex,
            &move_dex,
            &config,
            1,
            Some(&hook),
            Some(&flag),
        )
        .expect("the position is solvable");
        let elapsed = start.elapsed().as_secs_f64();
        drop(timer);
        println!(
            "refine d1->d2  {:>3}s limit: {:.1}s, {} turns, value {:.4}, {} rounds",
            limit.as_secs(),
            elapsed,
            result.stats.turns_simulated,
            result.value,
            report.rounds,
        );
        println!(
            "               verified {} of {} and {} of {} actions at depth 2, support {} and {}",
            report.verified[0],
            report.total[0],
            report.verified[1],
            report.total[1],
            result.p1_strategy.len(),
            result.p2_strategy.len(),
        );
        println!(
            "               first refined value {:.4}, P1 top [{}]",
            first.get(),
            top_actions(&result.p1_strategy),
        );
        std::io::stdout().flush().ok();
    }
    println!();

    // ── Sampled depth 2 inside the limit ────────────────────────────────────
    // A sampled search walks one trajectory for each iteration, so the action
    // count changes the branching of the tree rather than the cost of a node.
    for (label, widening) in [
        ("no widening", None),
        (
            "widening",
            Some(Widening {
                initial: 8,
                coefficient: 4.0,
                exponent: 0.5,
            }),
        ),
    ] {
        let config = MctsConfig {
            iterations: u32::MAX,
            depth: 2,
            policy: SelectionPolicy::RegretMatching,
            damage_rolls: 1,
            consider_crit: false,
            transition: TransitionMode::Enumerated(ChanceMode::TopK(1)),
            policy_prior: true,
            max_actions_per_player: None,
            prune_dominated_actions: false,
            widening,
            ..MctsConfig::default()
        };
        let flag = CancelFlag::default();
        let timer = Timer::start(&flag, LIMIT);
        let start = Instant::now();
        let result = mcts::search_cancellable(
            seed,
            &state,
            &pokemon_dex,
            &move_dex,
            &config,
            Some(&flag),
        )
        .expect("the position is solvable");
        let elapsed = start.elapsed().as_secs_f64();
        drop(timer);
        println!(
            "mcts d2     {label:<12}: {} iterations, {} turns in {:.1}s, value {:.4} +/- {}",
            result.stats.iterations,
            result.stats.turns_simulated,
            elapsed,
            result.value,
            result
                .sampling
                .standard_error
                .map(|error| format!("{error:.4}"))
                .unwrap_or_else(|| "n/a".to_string()),
        );
        println!(
            "            P1 top probabilities [{}], support {} of {}",
            top_actions(&result.p1_strategy),
            result.p1_strategy.len(),
            action_count(&state, Player::P1, &pokemon_dex, &move_dex),
        );
        std::io::stdout().flush().ok();
    }
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
