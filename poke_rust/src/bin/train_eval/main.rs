//! Fits the weights of `solver::eval` from labeled positions.
//!
//! `src/solver/TRAINING.md` holds the rerun procedure. Read it first.
//!
//! The binary has four stages.
//!
//! 1. It builds a corpus of positions by playing random legal commands from
//!    generated teams.
//! 2. It labels each position with a deeper search than the evaluator serves.
//! 3. It fits the linear value weights, the network value weights, and the
//!    policy weights, and it writes all three as JSON.
//! 4. It reports the corpus statistics that the model choice needs.
//!
//! `cargo test` never runs this binary. A corpus and its labels cost hours.
//! Run it by hand, then commit the weight files that the run produces, and
//! record the run in `benches/RESULTS.md`.
//!
//! ```sh
//! cargo run --release --bin train_eval -- --calibrate --workers 20 --seed 1
//! ```
//!
//! The corpus needs the usage cache in `meta_scraper/data`. The binary reports
//! a clear error and exits when the cache is absent.
//!
//! # The format
//!
//! The defaults describe Pokemon Champions doubles: two active Pokemon, four of
//! six brought, no Terastallization, and Mega Evolution on.
//! `MetaFormat::from_active_per_side` then reads the doubles usage table.
//!
//! # One run is one improvement step
//!
//! A `search` label comes from `solve`, and `solve` scores its own horizon with
//! the committed weights. A run therefore fits against the evaluator that the
//! tree already carries, and a second run starts from the first run's output.
//! The binary is not idempotent, and it does not converge on its own.
//! Keep a run only when the fitted weights beat the hand-set weights on the
//! held-out split.
//!
//! # The label is approximate
//!
//! A doubles side offers hundreds of joint actions, so an exact depth-two
//! doubles label costs more than ten minutes. `--label-chance`,
//! `--label-max-actions`, and dominated-action pruning bring it into reach, and
//! each of them makes the label approximate. An approximate depth-two label
//! still searches deeper than the leaf it teaches.

use std::collections::HashSet;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use clap::{Parser, ValueEnum};
use rand::rngs::StdRng;
use rand::seq::SliceRandom;
use rand::SeedableRng;

use poke_rust::VERBOSITY;
use poke_rust::data::pokemon_move::PokemonMove;
use poke_rust::data::species::Species;
use poke_rust::meta::{MetaDex, MetaFormat, generate_meta_team, render_teamsheet};
use poke_rust::simulator::{sample_turn_raw_seeded, team_preview_state_from_team_strings};
use poke_rust::solver::actions::{self, Phase};
use poke_rust::solver::chance::ChanceMode;
use poke_rust::solver::eval::{
    self, FEATURE_COUNT, FEATURE_NAMES, HAND_POLICY_WEIGHTS, HAND_WEIGHTS, MLP_HIDDEN, Mlp,
    MlpRecord, POLICY_FEATURE_COUNT, POLICY_FEATURE_NAMES, Weights,
};
use poke_rust::solver::mcts::{self, MctsConfig};
use poke_rust::solver::train::{self, CurvePoint, PolicySample, TrainConfig, ValueSample};
use poke_rust::solver::{SolveConfig, SolveWarning, solve_seeded};
use poke_rust::state::battle::{
    BattleCommand, BattleMechanics, BattleState, MatchState, Player, PlayerCommand,
    TeamPreviewCommand,
};
use poke_rust::state::dex_data::{parse_learnset_dex, parse_move_dex, parse_pokemon_dex};
use poke_rust::state::pokemon::parse_team_sheet_str;

/// The value network that this binary fits.
type ValueNetwork = Mlp<FEATURE_COUNT, MLP_HIDDEN>;

/// The training fractions of the learning curve.
const CURVE_FRACTIONS: [f64; 4] = [0.25, 0.5, 0.75, 1.0];

/// Where the value labels come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum LabelSource {
    /// Solve each position at `--label-depth`.
    /// This is the default, because it gives a lower-variance label for the
    /// same cost.
    Search,
    /// Estimate each position with the sampling search.
    Selfplay,
}

/// How much of each chance node a label descends into.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum LabelChance {
    /// Every successor at its true probability. Exact and expensive.
    Enumerate,
    /// The most likely successor alone.
    Top1,
    /// The two most likely successors.
    Top2,
    /// The four most likely successors.
    Top4,
}

impl LabelChance {
    fn mode(self) -> ChanceMode {
        match self {
            LabelChance::Enumerate => ChanceMode::Enumerate,
            LabelChance::Top1 => ChanceMode::TopK(1),
            LabelChance::Top2 => ChanceMode::TopK(2),
            LabelChance::Top4 => ChanceMode::TopK(4),
        }
    }
}

#[derive(Parser, Debug)]
#[command(about = "Fits the weights of the solver's leaf evaluator")]
struct Args {
    /// Distinct positions to collect.
    ///
    /// `--time-budget` normally stops the labeling stage first, so this figure
    /// only has to be large enough to keep the workers fed.
    #[arg(long, default_value_t = 8000)]
    positions: usize,

    /// Search depth of each label.
    #[arg(long, default_value_t = 2)]
    label_depth: u8,

    /// Depth that a label must reach to enter the corpus.
    ///
    /// Iterative deepening returns the last complete pass, so an expensive
    /// position can return a shallower label than `--label-depth`.
    #[arg(long, default_value_t = 1)]
    min_label_depth: u8,

    /// Search each depth up to `--label-depth` and keep the last complete pass.
    #[arg(long, action = clap::ArgAction::SetTrue)]
    iterative_deepening: bool,

    /// Seconds that one label may take. Zero removes the limit.
    #[arg(long, default_value_t = 0.0)]
    label_deadline: f64,

    /// Seconds that the whole labeling stage may take. Zero removes the limit.
    ///
    /// The fit then uses every label that finished.
    #[arg(long, default_value_t = 0.0)]
    time_budget: f64,

    /// Labeling threads. Zero uses one thread for each core.
    #[arg(long, default_value_t = 0)]
    workers: usize,

    /// Successors that a label keeps at each chance node.
    #[arg(long, value_enum, default_value_t = LabelChance::Top1)]
    label_chance: LabelChance,

    /// Joint actions that a label keeps for each player. Zero removes the cap.
    #[arg(long, default_value_t = 24)]
    label_max_actions: usize,

    /// Keeps every attack, including one that another attack of the same slot
    /// beats on both damage and accuracy.
    #[arg(long, action = clap::ArgAction::SetTrue)]
    no_prune_dominated: bool,

    /// Measure the label cost on a small sample, report it, and exit.
    #[arg(long, action = clap::ArgAction::SetTrue)]
    calibrate: bool,

    /// Positions that `--calibrate` labels.
    #[arg(long, default_value_t = 20)]
    calibrate_positions: usize,

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

    /// Active Pokemon per side. Champions doubles uses two.
    #[arg(long, default_value_t = 2)]
    active_per_side: u8,

    /// Team members that each side brings. Champions doubles brings four.
    #[arg(long, default_value_t = 4)]
    brought_per_side: u8,

    /// Team members on each roster. Champions uses six.
    #[arg(long, default_value_t = 6)]
    roster_size: u8,

    /// Enables Terastallization.
    ///
    /// Pokemon Champions has no Terastallization, so the corpus leaves it off.
    /// A corpus with it on holds commands that no real match can play.
    #[arg(long, action = clap::ArgAction::SetTrue)]
    tera: bool,

    /// Disables Mega Evolution. Champions keeps it.
    #[arg(long, action = clap::ArgAction::SetTrue)]
    no_mega: bool,

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

    /// Full-batch gradient steps of the network fit.
    #[arg(long, default_value_t = 600)]
    mlp_steps: usize,

    /// Step size of each network descent step.
    #[arg(long, default_value_t = 0.2)]
    mlp_learning_rate: f64,

    /// Held-out error that the network must win by to become the default.
    #[arg(long, default_value_t = 0.002)]
    mlp_margin: f64,

    /// A directory of teamsheets that the corpus plays.
    ///
    /// Each `.txt` file is one roster in teamsheet format. The collector pairs
    /// two of them for each matchup. Without this option every matchup uses a
    /// roster that `generate_meta_team` builds from the usage cache.
    ///
    /// Real teams hold the item, ability, and move combinations that players
    /// actually bring, which a per-Pokemon marginal cannot reproduce.
    #[arg(long)]
    teamsheet_dir: Option<PathBuf>,

    /// Fraction of matchups that draw from `--teamsheet-dir`.
    ///
    /// The rest draw from the usage cache. A mixture keeps the rare Pokemon that
    /// no archived team brought, so the corpus still covers them.
    #[arg(long, default_value_t = 0.8)]
    teamsheet_mix: f64,

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

    /// Where to write the fitted value network.
    #[arg(long, default_value = "weights/eval_mlp_v1.json")]
    out_mlp: PathBuf,

    /// Where to write the fitted policy weights.
    #[arg(long, default_value = "weights/policy_v1.json")]
    out_policy: PathBuf,

    /// Report the fit without writing any file.
    #[arg(long, action = clap::ArgAction::SetTrue)]
    dry_run: bool,
}

impl Args {
    /// The wanted labeling threads.
    fn worker_count(&self) -> usize {
        if self.workers > 0 {
            return self.workers;
        }
        std::thread::available_parallelism()
            .map(|count| count.get())
            .unwrap_or(1)
    }

    /// The per-label wall-clock limit.
    fn label_deadline(&self) -> Option<Duration> {
        (self.label_deadline > 0.0).then(|| Duration::from_secs_f64(self.label_deadline))
    }

    /// The whole-stage wall-clock limit.
    fn stage_budget(&self) -> Option<Duration> {
        (self.time_budget > 0.0).then(|| Duration::from_secs_f64(self.time_budget))
    }

    /// The battle rules that the corpus plays under.
    fn mechanics(&self) -> BattleMechanics {
        BattleMechanics {
            tera_enabled: self.tera,
            mega_enabled: !self.no_mega,
        }
    }

    /// The training settings of the linear and policy fits.
    fn train_config(&self) -> TrainConfig {
        TrainConfig {
            steps: self.steps,
            learning_rate: self.learning_rate,
            l2: self.l2,
        }
    }

    /// The training settings of the network fit.
    fn mlp_config(&self) -> TrainConfig {
        TrainConfig {
            steps: self.mlp_steps,
            learning_rate: self.mlp_learning_rate,
            l2: self.l2,
        }
    }
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

    let pool = match args.teamsheet_dir.as_ref() {
        Some(dir) => match TeamPool::load(dir, args.brought_per_side, &pokemon_dex, &move_dex) {
            Ok(loaded) => Some(loaded),
            Err(error) => {
                eprintln!("cannot read the teamsheet directory: {error}");
                std::process::exit(1);
            }
        },
        None => None,
    };
    if pool.is_some() {
        println!(
            "team source: {:.0}% archived teamsheets, {:.0}% usage cache",
            100.0 * args.teamsheet_mix,
            100.0 * (1.0 - args.teamsheet_mix),
        );
    } else {
        println!("team source: usage cache");
    }

    let mechanics = args.mechanics();
    println!(
        "format: {format:?}, {} active, {} brought of {}, tera {}, mega {}",
        args.active_per_side,
        args.brought_per_side,
        args.roster_size,
        on_off(mechanics.tera_enabled),
        on_off(mechanics.mega_enabled),
    );

    let wanted = if args.calibrate {
        args.calibrate_positions
    } else {
        args.positions
    };
    println!("collecting {wanted} positions");
    let started = Instant::now();
    let corpus = collect_positions(
        &args,
        wanted,
        &meta_dex,
        pool.as_ref(),
        &pokemon_dex,
        &move_dex,
        &learnset_dex,
    );
    println!(
        "collected {} distinct positions in {:.1} s",
        corpus.len(),
        started.elapsed().as_secs_f64()
    );
    if corpus.is_empty() {
        eprintln!("the corpus is empty; nothing to fit");
        std::process::exit(1);
    }

    println!(
        "labeling with {:?} at depth {} on {} workers ({:?} chance, {} action cap, prune {})",
        args.labels,
        args.label_depth,
        args.worker_count(),
        args.label_chance,
        cap_text(args.label_max_actions),
        on_off(!args.no_prune_dominated),
    );
    let labeling_started = Instant::now();
    let outcomes = label_all(&args, &corpus, &pokemon_dex, &move_dex);
    let labeling_elapsed = labeling_started.elapsed();

    if args.calibrate {
        report_calibration(&args, &outcomes, corpus.len(), labeling_elapsed);
        return;
    }

    let ctx = eval::EvalContext::new(&pokemon_dex, &move_dex);
    let (value_samples, policy_samples) =
        build_samples(&corpus, &outcomes, &ctx, &pokemon_dex, &move_dex);
    println!(
        "kept {} value labels and {} policy labels of {} positions",
        value_samples.len(),
        policy_samples.len(),
        corpus.len()
    );
    if value_samples.is_empty() {
        eprintln!("every label was dropped; nothing to fit");
        std::process::exit(1);
    }
    report_depths(&outcomes);
    report_features(&value_samples);

    let config = args.train_config();
    let (value_train, value_test) = train::split(&value_samples, args.holdout);
    let value_weights = train::fit_value(&value_train, &HAND_WEIGHTS, &config);
    report_value("hand", &value_train, &value_test, &HAND_WEIGHTS);
    report_value("fitted", &value_train, &value_test, &value_weights);

    let curve = train::learning_curve(
        &value_train,
        &value_test,
        &HAND_WEIGHTS,
        &config,
        &CURVE_FRACTIONS,
    );
    report_curve(&curve);

    let network = fit_network(&value_train, &value_weights, &args);
    let linear_error = train::value_mean_absolute_error(&value_test, &value_weights);
    let network_error = train::mlp_mean_absolute_error(&value_test, &network);
    println!(
        "network       : train loss {:.4} mae {:.4} | held-out loss {:.4} mae {:.4}",
        train::mlp_loss(&value_train, &network),
        train::mlp_mean_absolute_error(&value_train, &network),
        train::mlp_loss(&value_test, &network),
        network_error,
    );
    report_model_choice(linear_error, network_error, args.mlp_margin);

    let (policy_train, policy_test) = train::split(&policy_samples, args.holdout);
    let policy_weights = train::fit_policy(&policy_train, &HAND_POLICY_WEIGHTS, &config);
    report_policy("hand", &policy_train, &policy_test, &HAND_POLICY_WEIGHTS);
    report_policy("fitted", &policy_train, &policy_test, &policy_weights);

    if args.dry_run {
        println!("dry run: no file written");
        return;
    }

    write_weights(&args.out_eval, &FEATURE_NAMES, &value_weights);
    write_network(&args.out_mlp, &network);
    write_weights(&args.out_policy, &POLICY_FEATURE_NAMES, &policy_weights);
    println!(
        "wrote {}, {} and {}",
        args.out_eval.display(),
        args.out_mlp.display(),
        args.out_policy.display()
    );
}

/// Checks option combinations before the trainer reads data.
fn validate_args(args: &Args) -> Result<(), String> {
    if args.positions == 0 {
        return Err("--positions must be greater than 0".to_string());
    }
    if args.calibrate_positions == 0 {
        return Err("--calibrate-positions must be greater than 0".to_string());
    }
    if args.turns_per_match == 0 {
        return Err("--turns-per-match must be greater than 0".to_string());
    }
    if args.label_depth == 0 {
        return Err("--label-depth must be greater than 0".to_string());
    }
    if args.min_label_depth == 0 {
        return Err("--min-label-depth must be greater than 0".to_string());
    }
    if !(0.0..=1.0).contains(&args.teamsheet_mix) {
        return Err("--teamsheet-mix must be from 0.0 through 1.0".to_string());
    }
    if args.min_label_depth > args.label_depth {
        return Err("--min-label-depth must not be greater than --label-depth".to_string());
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
    if args.roster_size < args.brought_per_side {
        return Err("--roster-size must not be less than --brought-per-side".to_string());
    }
    if args.roster_size > 6 {
        return Err("--roster-size must not be greater than 6".to_string());
    }
    if !args.holdout.is_finite() || !(0.0..1.0).contains(&args.holdout) {
        return Err("--holdout must be at least 0 and less than 1".to_string());
    }
    if !args.label_deadline.is_finite() || args.label_deadline < 0.0 {
        return Err("--label-deadline must be a finite nonnegative number".to_string());
    }
    if !args.time_budget.is_finite() || args.time_budget < 0.0 {
        return Err("--time-budget must be a finite nonnegative number".to_string());
    }
    if !args.mlp_margin.is_finite() || args.mlp_margin < 0.0 {
        return Err("--mlp-margin must be a finite nonnegative number".to_string());
    }
    for (name, value) in [
        ("--learning-rate", args.learning_rate),
        ("--mlp-learning-rate", args.mlp_learning_rate),
    ] {
        if !value.is_finite() || value < 0.0 {
            return Err(format!("{name} must be a finite nonnegative number"));
        }
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
/// The archived rosters that a matchup can draw from.
///
/// One entry is the text of one teamsheet and the count of Pokemon that parsed
/// out of it. The count comes from the parser rather than the file, because a
/// paste can name a Pokemon or a move that this dex does not hold, and the
/// parser drops that block.
struct TeamPool {
    sheets: Vec<(String, usize)>,
}

impl TeamPool {
    /// Loads every `.txt` file of `dir` that parses into a usable roster.
    ///
    /// A roster must hold at least `brought` Pokemon. A shorter one cannot fill
    /// a team-preview command, so it never reaches a battle.
    fn load(
        dir: &Path,
        brought: u8,
        pokemon_dex: &std::collections::HashMap<Species, poke_rust::state::dex_data::PokemonData>,
        move_dex: &std::collections::HashMap<PokemonMove, poke_rust::state::dex_data::MoveData>,
    ) -> Result<TeamPool, String> {
        let listing = std::fs::read_dir(dir).map_err(|error| format!("{}: {error}", dir.display()))?;
        let mut paths: Vec<PathBuf> = listing
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "txt"))
            .collect();
        // A directory listing has no defined order, and the corpus seed must
        // pick the same pair on every run.
        paths.sort();

        let mut sheets = Vec::new();
        let mut short = 0usize;
        for path in paths {
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let parsed = parse_team_sheet_str(&text, pokemon_dex, move_dex, true);
            if parsed.len() < brought as usize {
                short += 1;
                continue;
            }
            sheets.push((text, parsed.len()));
        }
        if sheets.is_empty() {
            return Err(format!("{}: no usable teamsheet", dir.display()));
        }
        println!(
            "loaded {} teamsheet(s) from {} ({} dropped as too short)",
            sheets.len(),
            dir.display(),
            short
        );
        Ok(TeamPool { sheets })
    }

    /// Two rosters, chosen by seed. The pair is distinct when the pool holds
    /// more than one roster, so a matchup is not a mirror of one team.
    fn pair(&self, seed: u64) -> (&(String, usize), &(String, usize)) {
        let count = self.sheets.len();
        let first = (seed % count as u64) as usize;
        let second = if count == 1 {
            first
        } else {
            let offset = 1 + (seed / count as u64) % (count as u64 - 1);
            ((first as u64 + offset) % count as u64) as usize
        };
        (&self.sheets[first], &self.sheets[second])
    }
}

fn collect_positions(
    args: &Args,
    wanted: usize,
    meta_dex: &MetaDex,
    pool: Option<&TeamPool>,
    pokemon_dex: &std::collections::HashMap<Species, poke_rust::state::dex_data::PokemonData>,
    move_dex: &std::collections::HashMap<PokemonMove, poke_rust::state::dex_data::MoveData>,
    learnset_dex: &std::collections::HashMap<Species, HashSet<PokemonMove>>,
) -> Vec<Position> {
    let mut seen: HashSet<u64> = HashSet::new();
    let mut out: Vec<Position> = Vec::new();
    let mut match_index: u64 = 0;

    // A generated matchup can end before it yields its turns, so the loop needs
    // a stop that does not depend on the position count alone.
    let max_matches = wanted as u64 * 4 + 64;
    while out.len() < wanted && match_index < max_matches {
        let seed = args
            .seed
            .wrapping_mul(0x9E37_79B9_7F4A_7C15)
            .wrapping_add(match_index);
        match_index += 1;

        let Some(mut state) = start_match(
            args,
            meta_dex,
            pool,
            pokemon_dex,
            move_dex,
            learnset_dex,
            seed,
        ) else {
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
                    if out.len() >= wanted {
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
    pool: Option<&TeamPool>,
    pokemon_dex: &std::collections::HashMap<Species, poke_rust::state::dex_data::PokemonData>,
    move_dex: &std::collections::HashMap<PokemonMove, poke_rust::state::dex_data::MoveData>,
    learnset_dex: &std::collections::HashMap<Species, HashSet<PokemonMove>>,
    seed: u64,
) -> Option<MatchState> {
    let size = args.roster_size as usize;
    // The roll is a fixed function of the seed, so a rerun draws the same
    // matchup from the same source.
    let archived = pool.filter(|_| {
        let roll = (seed.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11) as f64
            / (1u64 << 53) as f64;
        roll < args.teamsheet_mix
    });

    let (p1_text, p1_len, p2_text, p2_len) = match archived {
        Some(pool) => {
            let (first, second) = pool.pair(seed);
            (first.0.clone(), first.1, second.0.clone(), second.1)
        }
        None => {
            let p1 = generate_meta_team(meta_dex, pokemon_dex, learnset_dex, size, seed).ok()?;
            let p2 =
                generate_meta_team(meta_dex, pokemon_dex, learnset_dex, size, seed ^ 0xA5A5_A5A5)
                    .ok()?;
            if p1.len() < size || p2.len() < size {
                return None;
            }
            (render_teamsheet(&p1), p1.len(), render_teamsheet(&p2), p2.len())
        }
    };

    let mut preview = team_preview_state_from_team_strings(
        &p1_text,
        &p2_text,
        pokemon_dex,
        move_dex,
        args.active_per_side,
        args.brought_per_side,
        true,
    );
    // The builder applies `BattleMechanics::default()`, which enables
    // Terastallization. Champions has none, so the corpus sets the rules itself.
    preview.mechanics = args.mechanics();

    // Each side indexes its own roster, and an archived roster can be shorter
    // than `--roster-size`. Passing `size` here would name a Pokemon that the
    // sheet does not hold.
    let p1_picks = random_preview_command(
        p1_len,
        args.active_per_side,
        args.brought_per_side,
        seed ^ 0xC3C3_C3C3_C3C3_C3C3,
    );
    let p2_picks = random_preview_command(
        p2_len,
        args.active_per_side,
        args.brought_per_side,
        seed ^ 0x3C3C_3C3C_3C3C_3C3C,
    );
    let (state, _, _) = sample_turn_raw_seeded(
        seed,
        &MatchState::TeamPreviewState(preview),
        &PlayerCommand::TeamPreview(p1_picks),
        &PlayerCommand::TeamPreview(p2_picks),
        move_dex,
        pokemon_dex,
        false,
        1,
        None,
    );
    matches!(state, MatchState::BattleState(_)).then_some(state)
}

/// Selects one legal team-preview command from a full roster.
fn random_preview_command(
    team_len: usize,
    active_per_side: u8,
    brought_per_side: u8,
    seed: u64,
) -> TeamPreviewCommand {
    let brought = (brought_per_side as usize).min(team_len);
    let active = (active_per_side as usize).min(brought);
    let mut indices: Vec<usize> = (0..team_len).collect();
    indices.shuffle(&mut StdRng::seed_from_u64(seed));
    indices.truncate(brought);
    TeamPreviewCommand {
        active_indices: indices[..active].to_vec(),
        back_indices: indices[active..].to_vec(),
    }
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

/// What one label produced.
struct LabelOutcome {
    /// The position that this label describes.
    index: usize,
    /// P1's win probability, or `None` when the label was dropped.
    value: Option<f64>,
    /// P1's root mixture over that position's joint actions.
    strategy: Vec<(Vec<BattleCommand>, f64)>,
    /// The depth of the pass that produced the label.
    depth: u8,
    /// Wall-clock seconds that the label took.
    seconds: f64,
    /// Why the label left the corpus, when it did.
    dropped: Option<&'static str>,
}

/// Labels every position, on `--workers` threads.
///
/// Each label carries its own seed, which is the run seed plus the position
/// index. The merge sorts by that index.
/// Without a wall-clock limit, the output does not depend on the thread
/// schedule.
///
/// The simulator's deterministic generator is a thread-local override, so each
/// worker installs its own inside `solve_seeded`.
fn label_all(
    args: &Args,
    corpus: &[Position],
    pokemon_dex: &std::collections::HashMap<Species, poke_rust::state::dex_data::PokemonData>,
    move_dex: &std::collections::HashMap<PokemonMove, poke_rust::state::dex_data::MoveData>,
) -> Vec<LabelOutcome> {
    let next_index = AtomicUsize::new(0);
    let finished = AtomicUsize::new(0);
    let started = Instant::now();
    let budget = args.stage_budget();
    let workers = effective_worker_count(args.worker_count(), corpus.len());

    let mut collected: Vec<LabelOutcome> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..workers)
            .map(|_| {
                let next_index = &next_index;
                let finished = &finished;
                scope.spawn(move || {
                    let mut out: Vec<LabelOutcome> = Vec::new();
                    loop {
                        let index = next_index.fetch_add(1, Ordering::Relaxed);
                        if index >= corpus.len() {
                            break;
                        }
                        if budget.is_some_and(|budget| started.elapsed() >= budget) {
                            break;
                        }
                        out.push(label_one(
                            args,
                            index,
                            &corpus[index],
                            pokemon_dex,
                            move_dex,
                        ));
                        let done = finished.fetch_add(1, Ordering::Relaxed) + 1;
                        if done.is_multiple_of(25) {
                            println!(
                                "  labeled {done} of {} in {:.0} s",
                                corpus.len(),
                                started.elapsed().as_secs_f64()
                            );
                        }
                    }
                    out
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|handle| handle.join().unwrap_or_default())
            .collect()
    });

    collected.sort_by_key(|outcome| outcome.index);
    println!(
        "labeling stage finished {} labels in {:.0} s",
        collected.len(),
        started.elapsed().as_secs_f64()
    );
    collected
}

/// Labels one position with the configured source.
fn label_one(
    args: &Args,
    index: usize,
    position: &Position,
    pokemon_dex: &std::collections::HashMap<Species, poke_rust::state::dex_data::PokemonData>,
    move_dex: &std::collections::HashMap<PokemonMove, poke_rust::state::dex_data::MoveData>,
) -> LabelOutcome {
    let seed = args.seed.wrapping_add(index as u64);
    let started = Instant::now();
    let mut outcome = LabelOutcome {
        index,
        value: None,
        strategy: Vec::new(),
        depth: 0,
        seconds: 0.0,
        dropped: Some("the search returned no result"),
    };

    match args.labels {
        LabelSource::Search => {
            let config = search_label_config(args);
            let Ok(result) = solve_seeded(seed, &position.state, pokemon_dex, move_dex, &config)
            else {
                outcome.seconds = started.elapsed().as_secs_f64();
                return outcome;
            };
            outcome.seconds = started.elapsed().as_secs_f64();
            outcome.depth = result.depth_reached;
            // A truncated search returns a static evaluation, and a static label
            // would teach the evaluator its own output.
            if let Some(reason) = truncation_reason(&result.warnings) {
                outcome.dropped = Some(reason);
                return outcome;
            }
            if result.depth_reached < args.min_label_depth {
                outcome.dropped = Some("the label stayed below --min-label-depth");
                return outcome;
            }
            outcome.value = Some(result.value.clamp(0.0, 1.0));
            outcome.strategy = result
                .p1_strategy
                .iter()
                .map(|entry| (entry.commands.clone(), entry.probability))
                .collect();
            outcome.dropped = None;
        }
        LabelSource::Selfplay => {
            let config = MctsConfig {
                depth: args.label_depth,
                ..MctsConfig::default()
            };
            let Ok(result) = mcts::search(seed, &position.state, pokemon_dex, move_dex, &config)
            else {
                outcome.seconds = started.elapsed().as_secs_f64();
                return outcome;
            };
            outcome.seconds = started.elapsed().as_secs_f64();
            outcome.depth = args.label_depth;
            outcome.value = Some(result.value.clamp(0.0, 1.0));
            outcome.strategy = result
                .p1_strategy
                .iter()
                .map(|entry| (entry.commands.clone(), entry.probability))
                .collect();
            outcome.dropped = None;
        }
    }
    outcome
}

/// Names the warning that makes a label a static evaluation.
///
/// A capped action set and a discarded chance branch make a label approximate,
/// not static, so neither drops the label.
fn truncation_reason(warnings: &[SolveWarning]) -> Option<&'static str> {
    warnings.iter().find_map(|warning| match warning {
        SolveWarning::BudgetExhausted { .. } => Some("the node budget ran out"),
        SolveWarning::DeadlineExceeded { .. } => Some("the label deadline expired"),
        _ => None,
    })
}

/// Makes the search configuration of one label.
fn search_label_config(args: &Args) -> SolveConfig {
    SolveConfig {
        depth: args.label_depth,
        iterative_deepening: args.iterative_deepening,
        chance: args.label_chance.mode(),
        max_actions_per_player: (args.label_max_actions > 0).then_some(args.label_max_actions),
        prune_dominated_actions: !args.no_prune_dominated,
        node_budget: None,
        deadline: args.label_deadline(),
        ..SolveConfig::default()
    }
}

/// Turns the kept labels into training samples.
///
/// The value label is P1's win probability. The policy label is the root
/// mixture of P1 over that position's legal joint actions.
fn build_samples(
    corpus: &[Position],
    outcomes: &[LabelOutcome],
    ctx: &eval::EvalContext<'_>,
    pokemon_dex: &std::collections::HashMap<Species, poke_rust::state::dex_data::PokemonData>,
    move_dex: &std::collections::HashMap<PokemonMove, poke_rust::state::dex_data::MoveData>,
) -> (
    Vec<ValueSample<FEATURE_COUNT>>,
    Vec<PolicySample<POLICY_FEATURE_COUNT>>,
) {
    let mut values = Vec::new();
    let mut policies = Vec::new();

    for outcome in outcomes {
        let Some(value) = outcome.value else {
            continue;
        };
        let Some(position) = corpus.get(outcome.index) else {
            continue;
        };
        values.push(ValueSample {
            features: eval::features(&position.battle, ctx),
            label: value,
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
                outcome
                    .strategy
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
                .map(|action| eval::policy_features(&position.battle, Player::P1, action, ctx))
                .collect(),
            target: target.iter().map(|value| value / mass).collect(),
        });
    }
    (values, policies)
}

/// Reports the measured label cost, then leaves the run to the operator.
fn report_calibration(
    args: &Args,
    outcomes: &[LabelOutcome],
    corpus_size: usize,
    elapsed: Duration,
) {
    let mut times: Vec<f64> = outcomes.iter().map(|outcome| outcome.seconds).collect();
    times.sort_by(f64::total_cmp);
    if times.is_empty() {
        println!("calibration: no label finished");
        return;
    }

    let kept = outcomes.iter().filter(|outcome| outcome.value.is_some()).count();
    let median = times[times.len() / 2];
    let largest = times.last().copied().unwrap_or(0.0);
    let total: f64 = times.iter().sum();
    let workers = effective_worker_count(args.worker_count(), corpus_size);
    let throughput = calibration_throughput(times.len(), elapsed);

    println!("calibration over {} of {corpus_size} positions", times.len());
    println!("  kept {kept} labels, dropped {}", times.len() - kept);
    println!("  median {median:.2} s, max {largest:.2} s, mean {:.2} s", total / times.len() as f64);
    println!("  {throughput:.2} labels per second on {workers} workers");
    if throughput > 0.0 {
        for hours in [1.0, 10.0, 12.0] {
            println!(
                "  a {hours:.0} hour budget yields about {:.0} labels",
                throughput * hours * 3600.0 * kept as f64 / times.len() as f64
            );
        }
    }
    report_drops(outcomes);
}

/// Calculates measured throughput from the labeling stage wall time.
fn calibration_throughput(labels: usize, elapsed: Duration) -> f64 {
    let seconds = elapsed.as_secs_f64();
    if seconds > 0.0 {
        labels as f64 / seconds
    } else {
        0.0
    }
}

/// Limits the worker count to the number of available jobs.
fn effective_worker_count(wanted: usize, jobs: usize) -> usize {
    wanted.max(1).min(jobs.max(1))
}

/// Fits the network from the fitted linear weights that it must beat.
fn fit_network(
    samples: &[ValueSample<FEATURE_COUNT>],
    linear_weights: &[f64; FEATURE_COUNT],
    args: &Args,
) -> ValueNetwork {
    let seed_network = ValueNetwork::seed(linear_weights);
    train::fit_mlp(samples, &seed_network, &args.mlp_config())
}

/// Reports the depth that each kept label reached.
fn report_depths(outcomes: &[LabelOutcome]) {
    let mut counts: Vec<(u8, usize)> = Vec::new();
    for outcome in outcomes.iter().filter(|outcome| outcome.value.is_some()) {
        match counts.iter_mut().find(|entry| entry.0 == outcome.depth) {
            Some(entry) => entry.1 += 1,
            None => counts.push((outcome.depth, 1)),
        }
    }
    counts.sort_by_key(|entry| entry.0);
    print!("label depths:");
    for (depth, count) in &counts {
        print!(" depth {depth}: {count}");
    }
    println!();
    report_drops(outcomes);
}

/// Reports why labels left the corpus.
fn report_drops(outcomes: &[LabelOutcome]) {
    let mut reasons: Vec<(&str, usize)> = Vec::new();
    for reason in outcomes.iter().filter_map(|outcome| outcome.dropped) {
        match reasons.iter_mut().find(|entry| entry.0 == reason) {
            Some(entry) => entry.1 += 1,
            None => reasons.push((reason, 1)),
        }
    }
    for (reason, count) in reasons {
        println!("  dropped {count}: {reason}");
    }
}

/// Reports the spread of each feature and the correlation of the kill pair.
///
/// A feature with no spread does not explain differences between samples.
fn report_features(samples: &[ValueSample<FEATURE_COUNT>]) {
    let variance = train::feature_variance(samples);
    println!("feature variance:");
    for (name, value) in FEATURE_NAMES.iter().zip(variance.iter()) {
        let mark = if *value <= 0.0 { "  constant" } else { "" };
        println!("    {name:>16} {value:.4}{mark}");
    }

    let guaranteed = index_of("guaranteed_kill");
    let possible = index_of("possible_kill");
    let correlation = train::feature_correlation(samples, guaranteed, possible);
    println!("kill feature correlation: {correlation:+.4}");
    if correlation.abs() >= 0.99 {
        println!("  the kill features are still collinear; the fit cannot separate them");
    }
}

/// The index of one named feature.
fn index_of(name: &str) -> usize {
    FEATURE_NAMES
        .iter()
        .position(|stored| *stored == name)
        .expect("every reported feature name must exist")
}

/// Reports the held-out error of each training fraction.
fn report_curve(curve: &[CurvePoint]) {
    println!("learning curve:");
    for point in curve {
        println!(
            "    {:>4.0}% of the training split ({:>5} samples) held-out mae {:.4}",
            100.0 * point.fraction,
            point.samples,
            point.holdout_error
        );
    }
    if let (Some(first), Some(last)) = (curve.first(), curve.last()) {
        let gain = first.holdout_error - last.holdout_error;
        println!("  four times the data lowered the error by {gain:+.4}");
    }
}

/// States whether the network earned the default evaluator slot.
fn report_model_choice(linear_error: f64, network_error: f64, margin: f64) {
    let gain = linear_error - network_error;
    if gain >= margin {
        println!(
            "model choice: the network wins by {gain:.4} mae; point SolveConfig and MctsConfig at eval::fitted_mlp"
        );
    } else {
        println!(
            "model choice: the network gains only {gain:.4} mae against a {margin:.4} margin; keep eval::fitted"
        );
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
fn write_weights(path: &Path, names: &[&str], values: &[f64]) {
    let record = Weights::from_array(names, values);
    write_json(path, &record);
}

/// Writes the fitted network as JSON.
fn write_network(path: &Path, network: &ValueNetwork) {
    let record = MlpRecord::from_network(&FEATURE_NAMES, network);
    write_json(path, &record);
}

/// Writes one serializable record, creating the parent directory if needed.
fn write_json<T: serde::Serialize>(path: &Path, record: &T) {
    let text = serde_json::to_string_pretty(record).expect("a weight record always serializes");
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

/// Renders a flag for the format line.
fn on_off(set: bool) -> &'static str {
    if set { "on" } else { "off" }
}

/// Renders an action cap for the labeling line.
fn cap_text(cap: usize) -> String {
    if cap == 0 {
        "no".to_string()
    } else {
        cap.to_string()
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
            vec!["--positions", "0"],
            vec!["--calibrate-positions", "0"],
            vec!["--turns-per-match", "0"],
            vec!["--active-per-side", "0"],
            vec!["--active-per-side", "3"],
            vec!["--brought-per-side", "1"],
            vec!["--brought-per-side", "7"],
            vec!["--roster-size", "3"],
            vec!["--roster-size", "7"],
            vec!["--holdout", "1"],
            vec!["--learning-rate", "NaN"],
            vec!["--mlp-learning-rate=-1"],
            vec!["--mlp-margin=-0.1"],
            vec!["--label-depth", "0"],
            vec!["--min-label-depth", "0"],
            vec!["--min-label-depth", "3", "--label-depth", "2"],
            vec!["--label-deadline=-1"],
            vec!["--time-budget=-1"],
            vec!["--l2=-0.1", "--dry-run"],
        ] {
            assert!(validate_args(&args(&options)).is_err(), "accepted {options:?}");
        }
    }

    #[test]
    fn the_defaults_describe_champions_doubles() {
        let args = args(&[]);
        assert!(validate_args(&args).is_ok());
        assert_eq!(args.active_per_side, 2);
        assert_eq!(args.brought_per_side, 4);
        assert_eq!(args.roster_size, 6);
        assert_eq!(
            MetaFormat::from_active_per_side(args.active_per_side),
            MetaFormat::Doubles
        );

        let mechanics = args.mechanics();
        assert!(!mechanics.tera_enabled, "Champions has no Terastallization");
        assert!(mechanics.mega_enabled, "Champions keeps Mega Evolution");
    }

    #[test]
    fn the_mechanics_flags_override_the_defaults() {
        assert!(args(&["--tera"]).mechanics().tera_enabled);
        assert!(!args(&["--no-mega"]).mechanics().mega_enabled);
    }

    #[test]
    fn the_label_configuration_follows_its_options() {
        let config = search_label_config(&args(&[]));
        assert_eq!(config.depth, 2);
        assert_eq!(config.node_budget, None);
        assert_eq!(config.chance, ChanceMode::TopK(1));
        assert_eq!(config.max_actions_per_player, Some(24));
        assert!(config.prune_dominated_actions);
        assert_eq!(config.deadline, None);
        assert!(!config.iterative_deepening);

        let tuned = search_label_config(&args(&[
            "--label-chance",
            "enumerate",
            "--label-max-actions",
            "0",
            "--no-prune-dominated",
            "--label-deadline",
            "30",
            "--iterative-deepening",
            "--label-depth",
            "3",
        ]));
        assert_eq!(tuned.depth, 3);
        assert_eq!(tuned.chance, ChanceMode::Enumerate);
        assert_eq!(tuned.max_actions_per_player, None);
        assert!(!tuned.prune_dominated_actions);
        assert_eq!(tuned.deadline, Some(Duration::from_secs(30)));
        assert!(tuned.iterative_deepening);
    }

    #[test]
    fn a_truncated_search_drops_its_label() {
        assert!(truncation_reason(&[SolveWarning::BudgetExhausted { budget: 10 }]).is_some());
        assert!(
            truncation_reason(&[SolveWarning::DeadlineExceeded {
                budget: Duration::from_secs(1)
            }])
            .is_some()
        );
        // An approximate label is still a search result, not a static score.
        assert!(
            truncation_reason(&[
                SolveWarning::ChanceMassDiscarded { max_fraction: 0.4 },
                SolveWarning::ActionsTruncated {
                    player: Player::P1,
                    kept: 24,
                    total: 300,
                },
                SolveWarning::DepthNotReached {
                    target: 2,
                    reached: 1
                },
            ])
            .is_none()
        );
    }

    #[test]
    fn a_zero_option_turns_off_its_limit() {
        let defaults = args(&[]);
        assert_eq!(defaults.label_deadline(), None);
        assert_eq!(defaults.stage_budget(), None);
        assert!(defaults.worker_count() >= 1);

        let limited = args(&[
            "--time-budget",
            "3600",
            "--label-deadline",
            "120",
            "--workers",
            "4",
        ]);
        assert_eq!(limited.stage_budget(), Some(Duration::from_secs(3600)));
        assert_eq!(limited.label_deadline(), Some(Duration::from_secs(120)));
        assert_eq!(limited.worker_count(), 4);
    }

    #[test]
    fn a_preview_selects_only_the_brought_members_from_the_roster() {
        let command = random_preview_command(6, 2, 4, 17);
        assert_eq!(command.active_indices.len(), 2);
        assert_eq!(command.back_indices.len(), 2);
        let mut selected = command.active_indices;
        selected.extend(command.back_indices);
        selected.sort_unstable();
        selected.dedup();
        assert_eq!(selected.len(), 4);
        assert!(selected.iter().all(|index| *index < 6));
    }

    #[test]
    fn calibration_uses_wall_time_and_the_effective_worker_count() {
        let rate = calibration_throughput(20, Duration::from_secs_f64(41.1));
        assert!((rate - 20.0 / 41.1).abs() < 1e-12);
        assert_eq!(effective_worker_count(100, 20), 20);
        assert_eq!(effective_worker_count(0, 20), 1);
    }

    #[test]
    fn the_network_fit_starts_from_the_fitted_linear_weights() {
        let args = args(&["--mlp-steps", "0"]);
        let linear = [0.25; FEATURE_COUNT];
        let fitted = fit_network(&[], &linear, &args);
        assert_eq!(fitted, ValueNetwork::seed(&linear));
        assert_ne!(fitted, ValueNetwork::seed(&HAND_WEIGHTS));
    }
}
