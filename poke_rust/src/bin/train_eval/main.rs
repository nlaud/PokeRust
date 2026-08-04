//! Fits the weight vectors of `solver::eval` from labeled positions.
//!
//! The binary has three stages.
//!
//! 1. It builds a corpus of positions by playing random legal commands from
//!    generated teams.
//! 2. It labels each position with a deeper search than the evaluator serves.
//! 3. It fits the value weights and the policy weights, and it writes both as
//!    JSON.
//!
//! `cargo test` never runs this binary. A corpus and its labels cost minutes.
//! Run it by hand, then commit the two weight files that the run produces, and
//! record the run in `benches/RESULTS.md`.
//!
//! ```sh
//! cargo run --release --bin train_eval -- --positions 400 --label-depth 2 --seed 1
//! ```
//!
//! The corpus needs the usage cache in `meta_scraper/data`. The binary reports
//! a clear error and exits when the cache is absent.
//!
//! # One run is one improvement step
//!
//! A `search` label comes from `solve`, and `solve` scores its own horizon with
//! the committed weights. A run therefore fits against the evaluator that the
//! tree already carries, and a second run starts from the first run's output.
//! The binary is not idempotent, and it does not converge on its own.
//! Keep a run only when the fitted weights beat the hand-set weights on the
//! held-out split.

use std::collections::HashSet;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;

use clap::{Parser, ValueEnum};

use poke_rust::VERBOSITY;
use poke_rust::data::pokemon_move::PokemonMove;
use poke_rust::data::species::Species;
use poke_rust::meta::{MetaDex, MetaFormat, generate_meta_team, render_teamsheet};
use poke_rust::simulator::{
    sample_turn_raw_seeded, team_preview_state_from_team_strings,
};
use poke_rust::solver::actions::{self, Phase};
use poke_rust::solver::eval::{
    self, FEATURE_COUNT, FEATURE_NAMES, HAND_POLICY_WEIGHTS, HAND_WEIGHTS, POLICY_FEATURE_COUNT,
    POLICY_FEATURE_NAMES, Weights,
};
use poke_rust::solver::mcts::{self, MctsConfig};
use poke_rust::solver::train::{
    self, PolicySample, TrainConfig, ValueSample,
};
use poke_rust::solver::{SolveConfig, solve_seeded};
use poke_rust::state::battle::{
    BattleCommand, BattleState, MatchState, Player, PlayerCommand, TeamPreviewCommand,
};
use poke_rust::state::dex_data::{
    parse_learnset_dex, parse_move_dex, parse_pokemon_dex,
};

/// Where the value labels come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum LabelSource {
    /// Solve each position exactly at `--label-depth`.
    /// This is the default, because it gives a lower-variance label for the
    /// same cost.
    Search,
    /// Estimate each position with the sampling search.
    Selfplay,
}

#[derive(Parser, Debug)]
#[command(about = "Fits the linear weights of the solver's leaf evaluator")]
struct Args {
    /// Distinct positions to label.
    #[arg(long, default_value_t = 400)]
    positions: usize,

    /// Search depth of each label.
    #[arg(long, default_value_t = 2)]
    label_depth: u8,

    /// Seed of the corpus and of every labeled search.
    #[arg(long, default_value_t = 1)]
    seed: u64,

    /// Where the value labels come from.
    #[arg(long, value_enum, default_value_t = LabelSource::Search)]
    labels: LabelSource,

    /// Turns to play from each generated matchup.
    ///
    /// A short match records only opening positions, where both sides are
    /// healthy and no kill is in range. The corpus then teaches the matchup
    /// features nothing.
    #[arg(long, default_value_t = 12)]
    turns_per_match: usize,

    /// Active Pokemon per side.
    #[arg(long, default_value_t = 1)]
    active_per_side: u8,

    /// Team members that each side brings.
    #[arg(long, default_value_t = 3)]
    brought_per_side: u8,

    /// Fraction of the corpus held out of the fit.
    #[arg(long, default_value_t = 0.2)]
    holdout: f64,

    /// Full-batch gradient steps.
    #[arg(long, default_value_t = 400)]
    steps: usize,

    /// Step size of each descent step.
    #[arg(long, default_value_t = 0.5)]
    learning_rate: f64,

    /// L2 penalty on the weight vector.
    #[arg(long, default_value_t = 1e-4)]
    l2: f64,

    /// The `meta_scraper/data` directory.
    #[arg(long, default_value = "../meta_scraper/data")]
    meta_root: PathBuf,

    /// Path to the showdown pokedex data.
    #[arg(long, default_value = "../pokemon_info/showdownDex.txt")]
    poke_dex: String,

    /// Path to the showdown move data.
    #[arg(long, default_value = "../pokemon_info/showdownMoves.txt")]
    move_dex: String,

    /// Path to the showdown learnset data.
    #[arg(long, default_value = "../pokemon_info/showdownLearnsets.txt")]
    learnset_dex: String,

    /// Where to write the fitted value weights.
    #[arg(long, default_value = "weights/eval_v1.json")]
    out_eval: PathBuf,

    /// Where to write the fitted policy weights.
    #[arg(long, default_value = "weights/policy_v1.json")]
    out_policy: PathBuf,

    /// Report the fit without writing either file.
    #[arg(long, action = clap::ArgAction::SetTrue)]
    dry_run: bool,
}

fn main() {
    let args = Args::parse();
    if let Err(error) = validate_args(&args) {
        eprintln!("{error}");
        std::process::exit(2);
    }
    // The corpus plays thousands of turns. Engine output would bury the report.
    let _ = VERBOSITY.set(0);

    let pokemon_dex = parse_pokemon_dex(&args.poke_dex);
    let move_dex = parse_move_dex(&args.move_dex);
    let learnset_dex = parse_learnset_dex(&args.learnset_dex);

    let format = MetaFormat::from_active_per_side(args.active_per_side);
    let meta_dex = match MetaDex::load(&args.meta_root, None, format) {
        Ok(loaded) => loaded,
        Err(error) => {
            eprintln!(
                "cannot read the usage cache at {}: {error}",
                args.meta_root.display()
            );
            eprintln!("run meta_scraper/update_meta.py first, or pass --meta-root");
            std::process::exit(1);
        }
    };

    println!("collecting {} positions", args.positions);
    let corpus = collect_positions(&args, &meta_dex, &pokemon_dex, &move_dex, &learnset_dex);
    println!("collected {} distinct positions", corpus.len());
    if corpus.is_empty() {
        eprintln!("the corpus is empty; nothing to fit");
        std::process::exit(1);
    }

    println!("labeling with {:?} at depth {}", args.labels, args.label_depth);
    let (value_samples, policy_samples) = label(&args, &corpus, &pokemon_dex, &move_dex);
    println!(
        "labeled {} positions and {} decisions",
        value_samples.len(),
        policy_samples.len()
    );

    let config = TrainConfig {
        steps: args.steps,
        learning_rate: args.learning_rate,
        l2: args.l2,
    };

    let (value_train, value_test) = train::split(&value_samples, args.holdout);
    let value_weights = train::fit_value(&value_train, &HAND_WEIGHTS, &config);
    report_value("hand", &value_train, &value_test, &HAND_WEIGHTS);
    report_value("fitted", &value_train, &value_test, &value_weights);

    let (policy_train, policy_test) = train::split(&policy_samples, args.holdout);
    let policy_weights = train::fit_policy(&policy_train, &HAND_POLICY_WEIGHTS, &config);
    report_policy("hand", &policy_train, &policy_test, &HAND_POLICY_WEIGHTS);
    report_policy("fitted", &policy_train, &policy_test, &policy_weights);

    if args.dry_run {
        println!("dry run: no file written");
        return;
    }

    write_weights(&args.out_eval, &FEATURE_NAMES, &value_weights);
    write_weights(&args.out_policy, &POLICY_FEATURE_NAMES, &policy_weights);
    println!(
        "wrote {} and {}",
        args.out_eval.display(),
        args.out_policy.display()
    );
}

/// Checks option combinations before the trainer reads data.
fn validate_args(args: &Args) -> Result<(), String> {
    if args.positions == 0 {
        return Err("--positions must be greater than 0".to_string());
    }
    if args.turns_per_match == 0 {
        return Err("--turns-per-match must be greater than 0".to_string());
    }
    if !(1..=2).contains(&args.active_per_side) {
        return Err("--active-per-side must be 1 or 2".to_string());
    }
    if args.brought_per_side < args.active_per_side {
        return Err("--brought-per-side must not be less than --active-per-side".to_string());
    }
    if args.brought_per_side > 6 {
        return Err("--brought-per-side must not be greater than 6".to_string());
    }
    if !args.holdout.is_finite() || !(0.0..1.0).contains(&args.holdout) {
        return Err("--holdout must be at least 0 and less than 1".to_string());
    }
    if !args.learning_rate.is_finite() || args.learning_rate < 0.0 {
        return Err("--learning-rate must be a finite nonnegative number".to_string());
    }
    if !args.l2.is_finite() || args.l2 < 0.0 {
        return Err("--l2 must be a finite nonnegative number".to_string());
    }
    Ok(())
}

/// One recorded position, with the side that is about to choose.
struct Position {
    battle: BattleState,
    state: MatchState,
}

/// Plays generated matchups and records the position before each turn.
///
/// A state hash removes a repeat position, so an early turn that both sides
/// keep replaying enters the corpus once.
fn collect_positions(
    args: &Args,
    meta_dex: &MetaDex,
    pokemon_dex: &std::collections::HashMap<Species, poke_rust::state::dex_data::PokemonData>,
    move_dex: &std::collections::HashMap<PokemonMove, poke_rust::state::dex_data::MoveData>,
    learnset_dex: &std::collections::HashMap<Species, HashSet<PokemonMove>>,
) -> Vec<Position> {
    let mut seen: HashSet<u64> = HashSet::new();
    let mut out: Vec<Position> = Vec::new();
    let mut match_index: u64 = 0;

    // A generated matchup can end before it yields its turns, so the loop needs
    // a stop that does not depend on the position count alone.
    let max_matches = args.positions as u64 * 4 + 64;
    while out.len() < args.positions && match_index < max_matches {
        let seed = args.seed.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(match_index);
        match_index += 1;

        let Some(mut state) = start_match(args, meta_dex, pokemon_dex, move_dex, learnset_dex, seed)
        else {
            continue;
        };

        for turn in 0..args.turns_per_match {
            let MatchState::BattleState(battle) = &state else {
                break;
            };
            let turn_seed = seed.wrapping_add(turn as u64 * 0x1000_0000_0000_0001);

            if matches!(actions::phase_of(&state), Phase::Normal) {
                let key = hash_state(&state);
                if seen.insert(key) {
                    out.push(Position {
                        battle: battle.clone(),
                        state: state.clone(),
                    });
                    if out.len() >= args.positions {
                        break;
                    }
                }
            }

            let Some(next) = play_random_turn(&state, move_dex, pokemon_dex, turn_seed) else {
                break;
            };
            state = next;
        }
    }
    out
}

/// Builds one battle position from two generated teams.
/// Returns `None` when generation or the preview turn fails.
fn start_match(
    args: &Args,
    meta_dex: &MetaDex,
    pokemon_dex: &std::collections::HashMap<Species, poke_rust::state::dex_data::PokemonData>,
    move_dex: &std::collections::HashMap<PokemonMove, poke_rust::state::dex_data::MoveData>,
    learnset_dex: &std::collections::HashMap<Species, HashSet<PokemonMove>>,
    seed: u64,
) -> Option<MatchState> {
    let size = args.brought_per_side.max(1) as usize;
    let p1 = generate_meta_team(meta_dex, pokemon_dex, learnset_dex, size, seed).ok()?;
    let p2 = generate_meta_team(meta_dex, pokemon_dex, learnset_dex, size, seed ^ 0xA5A5_A5A5).ok()?;
    if p1.len() < size || p2.len() < size {
        return None;
    }

    let preview = team_preview_state_from_team_strings(
        &render_teamsheet(&p1),
        &render_teamsheet(&p2),
        pokemon_dex,
        move_dex,
        args.active_per_side,
        args.brought_per_side,
        true,
    );

    let active = args.active_per_side as usize;
    let picks = || {
        PlayerCommand::TeamPreview(TeamPreviewCommand {
            active_indices: (0..active).collect(),
            back_indices: (active..size).collect(),
        })
    };
    let (state, _, _) = sample_turn_raw_seeded(
        seed,
        &MatchState::TeamPreviewState(preview),
        &picks(),
        &picks(),
        move_dex,
        pokemon_dex,
        false,
        1,
        None,
    );
    matches!(state, MatchState::BattleState(_)).then_some(state)
}

/// Plays one turn with a random legal joint action for each player.
fn play_random_turn(
    state: &MatchState,
    move_dex: &std::collections::HashMap<PokemonMove, poke_rust::state::dex_data::MoveData>,
    pokemon_dex: &std::collections::HashMap<Species, poke_rust::state::dex_data::PokemonData>,
    seed: u64,
) -> Option<MatchState> {
    let MatchState::BattleState(battle) = state else {
        return None;
    };
    let phase = actions::phase_of(state);
    let p1 = actions::joint_actions(battle, Player::P1, phase, move_dex, pokemon_dex, None, false);
    let p2 = actions::joint_actions(battle, Player::P2, phase, move_dex, pokemon_dex, None, false);
    if p1.actions.is_empty() || p2.actions.is_empty() {
        return None;
    }

    let p1_pick = (seed % p1.actions.len() as u64) as usize;
    let p2_pick = ((seed >> 17) % p2.actions.len() as u64) as usize;
    let (next, _, _) = sample_turn_raw_seeded(
        seed,
        state,
        &PlayerCommand::Battle(p1.actions[p1_pick].clone()),
        &PlayerCommand::Battle(p2.actions[p2_pick].clone()),
        move_dex,
        pokemon_dex,
        false,
        1,
        None,
    );
    Some(next)
}

/// Labels every recorded position.
///
/// The value label is P1's win probability. The policy label is the root
/// mixture of P1 over that position's legal joint actions.
fn label(
    args: &Args,
    corpus: &[Position],
    pokemon_dex: &std::collections::HashMap<Species, poke_rust::state::dex_data::PokemonData>,
    move_dex: &std::collections::HashMap<PokemonMove, poke_rust::state::dex_data::MoveData>,
) -> (
    Vec<ValueSample<FEATURE_COUNT>>,
    Vec<PolicySample<POLICY_FEATURE_COUNT>>,
) {
    let ctx = eval::EvalContext::new(pokemon_dex, move_dex);
    let mut values = Vec::new();
    let mut policies = Vec::new();

    for (index, position) in corpus.iter().enumerate() {
        if index % 25 == 0 {
            println!("  labeling {index} of {}", corpus.len());
        }
        let seed = args.seed.wrapping_add(index as u64);
        let Some((value, strategy)) = label_one(args, &position.state, pokemon_dex, move_dex, seed)
        else {
            continue;
        };

        values.push(ValueSample {
            features: eval::features(&position.battle, &ctx),
            label: value.clamp(0.0, 1.0),
        });

        let legal = actions::joint_actions(
            &position.battle,
            Player::P1,
            Phase::Normal,
            move_dex,
            pokemon_dex,
            None,
            false,
        );
        if legal.actions.len() < 2 {
            continue;
        }
        let target: Vec<f64> = legal
            .actions
            .iter()
            .map(|action| {
                strategy
                    .iter()
                    .find(|entry| entry.0 == *action)
                    .map(|entry| entry.1)
                    .unwrap_or(0.0)
            })
            .collect();
        let mass: f64 = target.iter().sum();
        if mass <= 0.0 {
            continue;
        }
        policies.push(PolicySample {
            actions: legal
                .actions
                .iter()
                .map(|action| eval::policy_features(&position.battle, Player::P1, action, &ctx))
                .collect(),
            target: target.iter().map(|value| value / mass).collect(),
        });
    }
    (values, policies)
}

/// The value and the P1 root mixture of one position.
type Labeled = (f64, Vec<(Vec<BattleCommand>, f64)>);

/// Labels one position with the configured source.
fn label_one(
    args: &Args,
    state: &MatchState,
    pokemon_dex: &std::collections::HashMap<Species, poke_rust::state::dex_data::PokemonData>,
    move_dex: &std::collections::HashMap<PokemonMove, poke_rust::state::dex_data::MoveData>,
    seed: u64,
) -> Option<Labeled> {
    match args.labels {
        LabelSource::Search => {
            let config = search_label_config(args.label_depth);
            let result = solve_seeded(seed, state, pokemon_dex, move_dex, &config).ok()?;
            Some((
                result.value,
                result
                    .p1_strategy
                    .iter()
                    .map(|entry| (entry.commands.clone(), entry.probability))
                    .collect(),
            ))
        }
        LabelSource::Selfplay => {
            let config = MctsConfig {
                depth: args.label_depth,
                ..MctsConfig::default()
            };
            let result = mcts::search(seed, state, pokemon_dex, move_dex, &config).ok()?;
            Some((
                result.value,
                result
                    .p1_strategy
                    .iter()
                    .map(|entry| (entry.commands.clone(), entry.probability))
                    .collect(),
            ))
        }
    }
}

/// Makes the exact-search configuration for one label.
fn search_label_config(depth: u8) -> SolveConfig {
    SolveConfig {
        depth,
        node_budget: None,
        ..SolveConfig::default()
    }
}

/// Prints the error of one value weight vector on both splits.
fn report_value(
    name: &str,
    train_set: &[ValueSample<FEATURE_COUNT>],
    test_set: &[ValueSample<FEATURE_COUNT>],
    weights: &[f64; FEATURE_COUNT],
) {
    println!(
        "value {name:>6}: train loss {:.4} mae {:.4} | held-out loss {:.4} mae {:.4}",
        train::value_loss(train_set, weights),
        train::value_mean_absolute_error(train_set, weights),
        train::value_loss(test_set, weights),
        train::value_mean_absolute_error(test_set, weights),
    );
    if name == "fitted" {
        for (feature, value) in FEATURE_NAMES.iter().zip(weights.iter()) {
            println!("    {feature:>16} {value:+.4}");
        }
    }
}

/// Prints the error of one policy weight vector on both splits.
fn report_policy(
    name: &str,
    train_set: &[PolicySample<POLICY_FEATURE_COUNT>],
    test_set: &[PolicySample<POLICY_FEATURE_COUNT>],
    weights: &[f64; POLICY_FEATURE_COUNT],
) {
    println!(
        "policy {name:>5}: train loss {:.4} top {:.3} | held-out loss {:.4} top {:.3}",
        train::policy_loss(train_set, weights),
        train::policy_top_agreement(train_set, weights),
        train::policy_loss(test_set, weights),
        train::policy_top_agreement(test_set, weights),
    );
    if name == "fitted" {
        for (feature, value) in POLICY_FEATURE_NAMES.iter().zip(weights.iter()) {
            println!("    {feature:>16} {value:+.4}");
        }
    }
}

/// Writes one named weight vector as JSON.
fn write_weights(path: &PathBuf, names: &[&str], values: &[f64]) {
    let record = Weights::from_array(names, values);
    let text = serde_json::to_string_pretty(&record).expect("a weight record always serializes");
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Err(error) = std::fs::write(path, format!("{text}\n")) {
        eprintln!("cannot write {}: {error}", path.display());
        std::process::exit(1);
    }
}

/// A stable key for one position.
fn hash_state(state: &MatchState) -> u64 {
    let mut hasher = DefaultHasher::new();
    state.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(extra: &[&str]) -> Args {
        let values = std::iter::once("train_eval").chain(extra.iter().copied());
        Args::try_parse_from(values).expect("the test options must parse")
    }

    #[test]
    fn invalid_training_ranges_fail_before_data_loading() {
        for options in [
            ["--positions", "0"],
            ["--turns-per-match", "0"],
            ["--active-per-side", "0"],
            ["--active-per-side", "3"],
            ["--brought-per-side", "0"],
            ["--brought-per-side", "7"],
            ["--holdout", "1"],
            ["--learning-rate", "NaN"],
            ["--l2=-0.1", "--dry-run"],
        ] {
            assert!(validate_args(&args(&options)).is_err(), "accepted {options:?}");
        }
    }

    #[test]
    fn search_labels_do_not_use_the_default_node_budget() {
        let config = search_label_config(2);
        assert_eq!(config.depth, 2);
        assert_eq!(config.node_budget, None);
        assert_eq!(config.max_actions_per_player, None);
        assert_eq!(config.deadline, None);
    }
}
