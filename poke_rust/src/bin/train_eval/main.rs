//! Fits the weights of `solver::eval` from labeled positions.
//!
//! `src/solver/TRAINING.md` holds the rerun procedure. Read it first.
//!
//! The binary has four stages.
//!
//! 1. It builds a corpus of positions.
//! 2. It labels each position.
//! 3. It fits the linear value weights, the network value weights, and the
//!    policy weights, and it writes them as JSON.
//! 4. It reports the corpus statistics that the model choice needs.
//!
//! `--labels` chooses the first two stages. Read *Two label sources* below.
//!
//! `cargo test` never runs this binary. A corpus and its labels cost hours.
//! Run it by hand, then commit the weight files that the run produces, and
//! record the run in `benches/RESULTS.md`.
//!
//! ```sh
//! cargo run --release --bin train_eval -- --calibrate --workers 20 --seed 7
//! ```
//!
//! # Two label sources
//!
//! `--labels search` and `--labels selfplay` build the corpus with random legal
//! commands, then label each position with a search that runs deeper than the
//! evaluator serves.
//!
//! `--labels rollout` plays whole games with the search bot on both sides. Every
//! position of one game takes that game's result as its label, so a label is 1
//! or 0 and not a search value. This source runs one stage and not two: the play
//! collects the positions and the result labels them.
//!
//! A rollout run reads no search-label option, and a search run reads no rollout
//! option. The binary refuses an option that its source ignores, because a
//! silent no-op would cost the whole run.
//!
//! # The rollout seed
//!
//! `collect_positions`, `play_rollouts`, and `benches/eval_calibration` build an
//! opening seed with one formula. Seed 1 therefore gives all three the same
//! openings.
//!
//! `benches/eval_calibration` is the accept rule of a training run, so a
//! training run must not use the seed that the accept-rule bench uses.
//! `validate_args` refuses `ACCEPT_RULE_BENCH_SEED`. Record both seeds in
//! `benches/RESULTS.md`.
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
//! A `rollout` label holds no evaluator output, so it does not carry that
//! feedback. The play still does: both sides search with the committed weights,
//! so a run changes the games that the next run plays.
//!
//! # The rollout split
//!
//! Every position of one game carries that game's one result, so two positions
//! of one game are not independent. A sample split would put both sides of one
//! result in the training set and the held-out set.
//!
//! The two games of one opening are not independent either. They start from one
//! drawn position, and the second game exchanges the two sides.
//! `eval::features` is antisymmetric, so the first recorded position of the
//! second game is the negated first recorded position of the first game.
//!
//! The held-out split therefore holds whole openings. Read `rollout_samples`.
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

use clap::parser::ValueSource;
use clap::{ArgMatches, CommandFactory, FromArgMatches, Parser, ValueEnum};

use poke_rust::VERBOSITY;
use poke_rust::data::pokemon_move::PokemonMove;
use poke_rust::data::species::Species;
use poke_rust::meta::{MetaDex, MetaFormat};
use poke_rust::selfplay::{self, GameConfig, MatchupConfig, TeamPool, TurnPolicy};
use poke_rust::solver::actions::{self, Phase};
use poke_rust::solver::chance::ChanceMode;
use poke_rust::solver::eval::{
    self, FEATURE_COUNT, FEATURE_NAMES, HAND_POLICY_WEIGHTS, HAND_WEIGHTS, MLP_HIDDEN, Mlp,
    MlpRecord, POLICY_FEATURE_COUNT, POLICY_FEATURE_NAMES, Weights,
};
use poke_rust::solver::mcts::{self, MctsConfig};
use poke_rust::solver::train::{self, CurvePoint, PolicySample, TrainConfig, ValueSample};
use poke_rust::solver::{SolveConfig, SolveWarning, solve_seeded};
use poke_rust::state::battle::{BattleCommand, BattleMechanics, BattleState, MatchState, Player};
use poke_rust::state::dex_data::{parse_learnset_dex, parse_move_dex, parse_pokemon_dex};

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
    /// Play whole games with the search bot on both sides, and label every
    /// recorded position with the result of its own game.
    ///
    /// The label is 1 or 0, so it holds no evaluator output. This is the source
    /// that a depth-1 leaf actually has to predict.
    Rollout,
}

impl LabelSource {
    /// The `--labels` value that names this source.
    fn name(self) -> &'static str {
        match self {
            LabelSource::Search => "search",
            LabelSource::Selfplay => "selfplay",
            LabelSource::Rollout => "rollout",
        }
    }
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
    ///
    /// `--labels rollout` counts kept labels here, and it does not drop a
    /// repeated position. One opening yields many labels, so the play stage
    /// can go above this figure by one opening for each worker.
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
    ///
    /// The binary refuses [`ACCEPT_RULE_BENCH_SEED`]. Read *The rollout seed*
    /// at the top of this file.
    #[arg(long, default_value_t = 7)]
    seed: u64,

    /// Where the value labels come from.
    #[arg(long, value_enum, default_value_t = LabelSource::Search)]
    labels: LabelSource,

    /// Turns to play from each generated matchup.
    ///
    /// A short match records only opening positions, where both sides are
    /// healthy and no kill is in range. The corpus then teaches the matchup
    /// features nothing.
    ///
    /// `--labels rollout` plays each game to its end, so it reads `--turn-cap`
    /// instead.
    #[arg(long, default_value_t = 12)]
    turns_per_match: usize,

    /// Search iterations of each turn of a rollout game.
    #[arg(long, default_value_t = 64)]
    rollout_iterations: u32,

    /// Search depth of each turn of a rollout game.
    #[arg(long, default_value_t = 2)]
    rollout_depth: u8,

    /// Steps that one rollout game may take.
    ///
    /// A replacement step and a self-switch step each consume one. A game that
    /// is still running at the cap has no result, so the rollout drops every
    /// position of it.
    #[arg(long, default_value_t = 120)]
    turn_cap: usize,

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

    /// The matchup that `selfplay` builds for the corpus.
    fn matchup(&self) -> MatchupConfig {
        MatchupConfig {
            active_per_side: self.active_per_side,
            brought_per_side: self.brought_per_side,
            roster_size: self.roster_size,
            mechanics: self.mechanics(),
            teamsheet_mix: self.teamsheet_mix,
        }
    }

    /// The play settings of one rollout game.
    ///
    /// Both sides carry the same settings, so [`selfplay::play_turn`] runs one
    /// search for the turn and reads both root strategies out of it.
    fn game_config(&self) -> GameConfig {
        let policy = TurnPolicy::Search {
            iterations: self.rollout_iterations,
            depth: self.rollout_depth,
        };
        GameConfig {
            p1: policy,
            p2: policy,
            turn_cap: self.turn_cap,
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
    // `ArgMatches` says which options the command line set, and
    // `validate_label_source` needs that to refuse an option that the chosen
    // source ignores. A default value is not a request.
    let matches = Args::command().get_matches();
    let args = match Args::from_arg_matches(&matches) {
        Ok(parsed) => parsed,
        Err(error) => error.exit(),
    };
    if let Err(error) =
        validate_args(&args).and_then(|()| validate_label_source(&matches, args.labels))
    {
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
            Ok(loaded) => {
                println!(
                    "loaded {} teamsheet(s) from {} ({} dropped as too short)",
                    loaded.len(),
                    dir.display(),
                    loaded.dropped_short(),
                );
                Some(loaded)
            }
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
    let ctx = eval::EvalContext::new(&pokemon_dex, &move_dex);

    // Each entry is one independent unit of the held-out split. A search label
    // is independent on its own, so it forms a group of one. A rollout label is
    // not. Every position of one game shares that game's result, and the two
    // games of one opening share one drawn position.
    let (value_groups, policy_samples) = if args.labels == LabelSource::Rollout {
        println!(
            "playing rollouts for {wanted} positions on {} workers ({})",
            args.worker_count(),
            args.game_config().p1.label(),
        );
        let rollout = play_rollouts(
            &args,
            wanted,
            &meta_dex,
            pool.as_ref(),
            &pokemon_dex,
            &move_dex,
            &learnset_dex,
            &ctx,
        );
        report_rollout(&rollout);
        if args.calibrate {
            report_calibration(&rollout.calibration());
            return;
        }
        (rollout_samples(&rollout), Vec::new())
    } else {
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
            report_calibration(&search_calibration(
                &args,
                &outcomes,
                corpus.len(),
                labeling_elapsed,
            ));
            report_drops(&outcomes);
            return;
        }

        let (values, policies) = build_samples(&corpus, &outcomes, &ctx, &pokemon_dex, &move_dex);
        println!(
            "kept {} value labels and {} policy labels of {} positions",
            values.len(),
            policies.len(),
            corpus.len()
        );
        report_depths(&outcomes);
        (
            values.into_iter().map(|sample| vec![sample]).collect(),
            policies,
        )
    };

    let value_samples: Vec<ValueSample<FEATURE_COUNT>> =
        value_groups.iter().flatten().cloned().collect();
    if value_samples.is_empty() {
        eprintln!("every label was dropped; nothing to fit");
        std::process::exit(1);
    }
    report_features(&value_samples);

    let config = args.train_config();
    let (train_groups, test_groups) = train::split(&value_groups, args.holdout);
    let value_train: Vec<ValueSample<FEATURE_COUNT>> =
        train_groups.iter().flatten().cloned().collect();
    let value_test: Vec<ValueSample<FEATURE_COUNT>> =
        test_groups.iter().flatten().cloned().collect();
    report_split(&train_groups, &test_groups, args.labels);
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

    // A rollout holds no root mixture. A one-hot target of the played action
    // would teach the policy head its own draw, so the run leaves the committed
    // policy weights in place.
    let policy_weights = (!policy_samples.is_empty()).then(|| {
        let (policy_train, policy_test) = train::split(&policy_samples, args.holdout);
        let weights = train::fit_policy(&policy_train, &HAND_POLICY_WEIGHTS, &config);
        report_policy("hand", &policy_train, &policy_test, &HAND_POLICY_WEIGHTS);
        report_policy("fitted", &policy_train, &policy_test, &weights);
        weights
    });
    if policy_weights.is_none() {
        println!(
            "policy       : no policy label. {} keeps its committed values",
            args.out_policy.display()
        );
    }

    if args.dry_run {
        println!("dry run: no file written");
        return;
    }

    write_weights(&args.out_eval, &FEATURE_NAMES, &value_weights);
    write_network(&args.out_mlp, &network);
    let mut written = format!("{} and {}", args.out_eval.display(), args.out_mlp.display());
    if let Some(weights) = policy_weights.as_ref() {
        write_weights(&args.out_policy, &POLICY_FEATURE_NAMES, weights);
        written = format!(
            "{}, {} and {}",
            args.out_eval.display(),
            args.out_mlp.display(),
            args.out_policy.display()
        );
    }
    println!("wrote {written}");
}

/// The default seed of `benches/eval_calibration`.
///
/// That bench is the accept rule of a training run. The bench and this binary
/// build an opening seed with one formula, so a run at this seed gives the fit
/// the openings that the accept rule then reads.
const ACCEPT_RULE_BENCH_SEED: u64 = 1;

/// Checks option combinations before the trainer reads data.
fn validate_args(args: &Args) -> Result<(), String> {
    if args.seed == ACCEPT_RULE_BENCH_SEED {
        return Err(format!(
            "--seed {ACCEPT_RULE_BENCH_SEED} gives the fit the openings that \
             benches/eval_calibration reads. Pick another seed."
        ));
    }
    if args.positions == 0 {
        return Err("--positions must be greater than 0".to_string());
    }
    if args.calibrate_positions == 0 {
        return Err("--calibrate-positions must be greater than 0".to_string());
    }
    if args.turns_per_match == 0 {
        return Err("--turns-per-match must be greater than 0".to_string());
    }
    if args.turn_cap == 0 {
        return Err("--turn-cap must be greater than 0".to_string());
    }
    if args.rollout_iterations == 0 {
        return Err("--rollout-iterations must be greater than 0".to_string());
    }
    if args.rollout_depth == 0 {
        return Err("--rollout-depth must be greater than 0".to_string());
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

/// The options that a `--labels rollout` run does not read.
///
/// A rollout collects its own positions and labels them from the result, so it
/// runs neither the random-command collector nor a labeling search.
const NOT_FOR_ROLLOUT: [&str; 8] = [
    "turns_per_match",
    "label_depth",
    "min_label_depth",
    "iterative_deepening",
    "label_deadline",
    "label_chance",
    "label_max_actions",
    "no_prune_dominated",
];

/// The options that only a `--labels rollout` run reads.
const ROLLOUT_ONLY: [&str; 3] = ["rollout_iterations", "rollout_depth", "turn_cap"];

/// Refuses an option that the chosen label source ignores.
///
/// A silent no-op costs the whole run. An operator who passed
/// `--label-deadline` to a rollout run would believe a per-label limit was in
/// force, and would find out after the hours had gone.
fn validate_label_source(matches: &ArgMatches, labels: LabelSource) -> Result<(), String> {
    let table = match labels {
        LabelSource::Rollout => NOT_FOR_ROLLOUT.as_slice(),
        LabelSource::Search | LabelSource::Selfplay => ROLLOUT_ONLY.as_slice(),
    };
    let misplaced: Vec<String> = table
        .iter()
        .filter(|id| matches.value_source(id) == Some(ValueSource::CommandLine))
        .map(|id| format!("--{}", id.replace('_', "-")))
        .collect();
    if misplaced.is_empty() {
        return Ok(());
    }
    Err(format!(
        "--labels {} does not read {}",
        labels.name(),
        misplaced.join(", ")
    ))
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

        let Some(mut state) = selfplay::start_match(
            &args.matchup(),
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

            let Some(next) = selfplay::play_random_turn(&state, move_dex, pokemon_dex, turn_seed)
            else {
                break;
            };
            state = next;
        }
    }
    out
}

// ── The rollout source ──────────────────────────────────────────────────────

/// Games that one opening plays. The second game exchanges the two sides.
///
/// [`RolloutGame::index`] holds `opening * GAMES_PER_OPENING + side`, so
/// [`RolloutGame::opening`] reads the opening back out of it.
const GAMES_PER_OPENING: usize = 2;

/// One played game, and the label that its result gives every position of it.
struct RolloutGame {
    /// The play order of this game across the whole stage.
    ///
    /// Workers finish out of order, and the split reads the corpus order, so
    /// the merge sorts by this key.
    ///
    /// The key is `opening * GAMES_PER_OPENING + side`.
    index: usize,
    /// The feature vector of each position before an ordinary turn, in play
    /// order.
    ///
    /// The worker scores the position and drops it. A long run records hundreds
    /// of thousands of positions, and a `BattleState` is far larger than the
    /// 23 numbers that the fit reads.
    features: Vec<eval::Features>,
    /// P1's label. The winner decides it, so every position shares one value.
    label: f64,
}

impl RolloutGame {
    /// The opening that played this game.
    ///
    /// The two games of one opening return the same value.
    fn opening(&self) -> usize {
        self.index / GAMES_PER_OPENING
    }
}

/// What the rollout stage produced.
#[derive(Default)]
struct Rollout {
    /// The games that finished with a winner, in play order.
    games: Vec<RolloutGame>,
    /// Openings that played both of their orientations.
    openings: usize,
    /// Openings that never reached a battle.
    skipped_openings: usize,
    /// Games that played, with a result and without one.
    played: usize,
    /// Games that were still running at `--turn-cap`.
    dropped_games: usize,
    /// Positions that a dropped game recorded.
    dropped_positions: usize,
    /// Games that P1 won.
    p1_wins: usize,
    /// Steps that every played game resolved.
    turns: usize,
    /// Wall-clock seconds of each played game.
    seconds: Vec<f64>,
    /// Wall time of the whole stage.
    elapsed: Duration,
    /// Threads that played the stage.
    workers: usize,
}

impl Rollout {
    /// The labels that the stage kept.
    fn kept_positions(&self) -> usize {
        self.games.iter().map(|game| game.features.len()).sum()
    }

    /// The games that produced a result.
    fn scored(&self) -> usize {
        self.played - self.dropped_games
    }

    /// The cost figures that `--calibrate` reports.
    ///
    /// `--positions` counts kept labels for this source, so the sizing count is
    /// the kept count and not the game count.
    fn calibration(&self) -> Calibration {
        Calibration {
            unit: "game",
            times: self.seconds.clone(),
            kept: self.kept_positions(),
            dropped: self.dropped_positions,
            sized: self.kept_positions(),
            attempted: self.played,
            elapsed: self.elapsed,
            workers: self.workers,
        }
    }

    /// Folds one worker's part into this total.
    fn absorb(&mut self, part: Rollout) {
        self.games.extend(part.games);
        self.openings += part.openings;
        self.skipped_openings += part.skipped_openings;
        self.played += part.played;
        self.dropped_games += part.dropped_games;
        self.dropped_positions += part.dropped_positions;
        self.p1_wins += part.p1_wins;
        self.turns += part.turns;
        self.seconds.extend(part.seconds);
    }
}

/// Plays whole games and keeps the positions of every game that had a winner.
///
/// One job is one opening, and one opening plays two games. The second game
/// exchanges the two sides, so team strength cancels out of the aggregate P1
/// win rate. That rate is the self-check of the stage.
///
/// A worker takes the whole opening, because a stop between the two
/// orientations would leave that opening's team strength inside the rate.
///
/// The stage stops at the first of three events: the kept positions reach
/// `wanted`, `--time-budget` expires, or the opening ceiling runs out. The
/// ceiling only binds when opening after opening fails to reach a battle.
///
/// The opening seed uses the formula that `collect_positions` and
/// `benches/eval_calibration` use. Read *The rollout seed* at the top of this
/// file before you choose `--seed`.
#[allow(clippy::too_many_arguments)]
fn play_rollouts(
    args: &Args,
    wanted: usize,
    meta_dex: &MetaDex,
    pool: Option<&TeamPool>,
    pokemon_dex: &std::collections::HashMap<Species, poke_rust::state::dex_data::PokemonData>,
    move_dex: &std::collections::HashMap<PokemonMove, poke_rust::state::dex_data::MoveData>,
    learnset_dex: &std::collections::HashMap<Species, HashSet<PokemonMove>>,
    ctx: &eval::EvalContext<'_>,
) -> Rollout {
    let next_opening = AtomicUsize::new(0);
    let kept = AtomicUsize::new(0);
    let finished = AtomicUsize::new(0);
    let started = Instant::now();
    let budget = args.stage_budget();
    let matchup = args.matchup();
    let config = args.game_config();
    // One opening yields many positions, so this ceiling never binds on a
    // healthy run. It stops a run whose every opening fails to start.
    let max_openings = wanted + 64;
    let workers = effective_worker_count(args.worker_count(), max_openings);

    let parts: Vec<Rollout> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..workers)
            .map(|_| {
                let next_opening = &next_opening;
                let kept = &kept;
                let finished = &finished;
                scope.spawn(move || {
                    let mut part = Rollout::default();
                    loop {
                        if kept.load(Ordering::Relaxed) >= wanted {
                            break;
                        }
                        if budget.is_some_and(|budget| started.elapsed() >= budget) {
                            break;
                        }
                        let opening_index = next_opening.fetch_add(1, Ordering::Relaxed);
                        if opening_index >= max_openings {
                            break;
                        }
                        play_one_opening(
                            &mut part,
                            args,
                            &matchup,
                            &config,
                            opening_index,
                            meta_dex,
                            pool,
                            pokemon_dex,
                            move_dex,
                            learnset_dex,
                            ctx,
                            kept,
                        );
                        let done = finished.fetch_add(1, Ordering::Relaxed) + 1;
                        if done.is_multiple_of(5) {
                            println!(
                                "  played {done} opening(s), {} label(s) in {:.0} s",
                                kept.load(Ordering::Relaxed),
                                started.elapsed().as_secs_f64()
                            );
                        }
                    }
                    part
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().unwrap_or_default())
            .collect()
    });

    let mut total = Rollout {
        elapsed: started.elapsed(),
        workers,
        ..Rollout::default()
    };
    for part in parts {
        total.absorb(part);
    }
    total.games.sort_by_key(|game| game.index);
    total
}

/// Plays both orientations of one opening into `part`.
///
/// The pair plays together, or it does not play. One orientation on its own
/// leaves that opening's team strength inside the aggregate win rate.
#[allow(clippy::too_many_arguments)]
fn play_one_opening(
    part: &mut Rollout,
    args: &Args,
    matchup: &MatchupConfig,
    config: &GameConfig,
    opening_index: usize,
    meta_dex: &MetaDex,
    pool: Option<&TeamPool>,
    pokemon_dex: &std::collections::HashMap<Species, poke_rust::state::dex_data::PokemonData>,
    move_dex: &std::collections::HashMap<PokemonMove, poke_rust::state::dex_data::MoveData>,
    learnset_dex: &std::collections::HashMap<Species, HashSet<PokemonMove>>,
    ctx: &eval::EvalContext<'_>,
    kept: &AtomicUsize,
) {
    let seed = args
        .seed
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(opening_index as u64);
    let Some(opening) =
        selfplay::draw_opening(matchup, meta_dex, pool, pokemon_dex, learnset_dex, seed)
    else {
        part.skipped_openings += 1;
        return;
    };

    let starts: Option<Vec<(u64, MatchState)>> = [opening.clone(), opening.swapped()]
        .iter()
        .enumerate()
        .map(|(side, orientation)| {
            let game_seed = seed ^ (side as u64).wrapping_mul(0x5DEE_CE66_D125_5AB1);
            selfplay::opening_match(matchup, orientation, pokemon_dex, move_dex, game_seed)
                .map(|start| (game_seed, start))
        })
        .collect();
    let Some(starts) = starts else {
        part.skipped_openings += 1;
        return;
    };
    part.openings += 1;

    for (side, (game_seed, start)) in starts.into_iter().enumerate() {
        let game_started = Instant::now();
        let mut features: Vec<eval::Features> = Vec::new();
        let mut record = |battle: &BattleState| features.push(eval::features(battle, ctx));
        let result = selfplay::play_game(
            &start,
            config,
            pokemon_dex,
            move_dex,
            game_seed,
            &mut record,
        );
        part.seconds.push(game_started.elapsed().as_secs_f64());
        part.played += 1;
        part.turns += result.turns;

        match game_outcome(&result) {
            GameOutcome::Drop => {
                part.dropped_games += 1;
                part.dropped_positions += features.len();
            }
            GameOutcome::Keep(label) => {
                if result.winner == Some(Player::P1) {
                    part.p1_wins += 1;
                }
                kept.fetch_add(features.len(), Ordering::Relaxed);
                part.games.push(RolloutGame {
                    index: opening_index * GAMES_PER_OPENING + side,
                    features,
                    label,
                });
            }
        }
    }
}

/// What the rollout does with one played game.
#[derive(Debug, Clone, Copy, PartialEq)]
enum GameOutcome {
    /// Keep every position of the game, at this label.
    Keep(f64),
    /// Drop every position of the game.
    Drop,
}

/// Decides the fate of one played game.
///
/// The label is P1's realized win probability, which is 1 or 0. A game with no
/// winner has no label at all: it reached `--turn-cap` or it offered no legal
/// joint action, and neither says who was ahead. Its positions cannot enter the
/// corpus.
fn game_outcome(result: &selfplay::GameResult) -> GameOutcome {
    match result.winner {
        Some(Player::P1) => GameOutcome::Keep(1.0),
        Some(Player::P2) => GameOutcome::Keep(0.0),
        None => GameOutcome::Drop,
    }
}

/// Reports what the rollout stage played.
fn report_rollout(rollout: &Rollout) {
    println!(
        "played {} game(s) from {} opening(s) in {:.1} s, {} step(s) resolved",
        rollout.played,
        rollout.openings,
        rollout.elapsed.as_secs_f64(),
        rollout.turns,
    );
    if rollout.skipped_openings > 0 {
        println!(
            "  {} opening(s) never reached a battle",
            rollout.skipped_openings
        );
    }
    println!(
        "  {} game(s) hit the turn cap; dropped {} position(s)",
        rollout.dropped_games, rollout.dropped_positions,
    );
    let scored = rollout.scored();
    if scored == 0 {
        println!("  no game produced a result");
        return;
    }
    println!(
        "  P1 won {} of {scored} scored game(s) ({:.3}); each opening plays both sides, so this belongs near 0.500",
        rollout.p1_wins,
        rollout.p1_wins as f64 / scored as f64,
    );
    if rollout.dropped_games > 0 {
        // A dropped game leaves its mirror alone in the count, and that
        // opening's team strength then sits inside the rate.
        println!(
            "  {} dropped game(s) left their mirror unpaired",
            rollout.dropped_games
        );
    }
    println!(
        "  kept {} label(s) from {scored} game(s)",
        rollout.kept_positions()
    );
}

/// Turns each played opening into one group of value samples.
///
/// The group is the unit of the held-out split. The opening is the group, and
/// not the game, for two reasons.
///
/// 1. Every position of one game carries the one result of that game.
/// 2. The two games of one opening start from one drawn position. The second
///    game exchanges the two sides, and `eval::features` is antisymmetric, so
///    the first recorded position of the second game is the negated first
///    recorded position of the first game.
///
/// A split by game would therefore train on the negated opening position that
/// it then holds out, and the held-out error would measure that opening again.
///
/// The caller must sort the games by [`RolloutGame::index`], which
/// [`play_rollouts`] does. The two games of one opening then sit together.
fn rollout_samples(rollout: &Rollout) -> Vec<Vec<ValueSample<FEATURE_COUNT>>> {
    let mut groups: Vec<Vec<ValueSample<FEATURE_COUNT>>> = Vec::new();
    let mut current: Option<usize> = None;
    for game in &rollout.games {
        if current != Some(game.opening()) {
            current = Some(game.opening());
            groups.push(Vec::new());
        }
        let group = groups.last_mut().expect("the loop pushed a group");
        group.extend(game.features.iter().map(|features| ValueSample {
            features: *features,
            label: game.label,
        }));
    }
    groups
}

/// Reports the shape of the held-out split.
///
/// A rollout split holds whole openings, so the group count and the sample
/// count differ. Naming both makes the independent observation count visible.
fn report_split(
    train_groups: &[Vec<ValueSample<FEATURE_COUNT>>],
    test_groups: &[Vec<ValueSample<FEATURE_COUNT>>],
    labels: LabelSource,
) {
    let unit = if labels == LabelSource::Rollout {
        "opening"
    } else {
        "position"
    };
    let count = |groups: &[Vec<ValueSample<FEATURE_COUNT>>]| -> (usize, usize) {
        (groups.len(), groups.iter().map(Vec::len).sum())
    };
    let (train_units, train_samples) = count(train_groups);
    let (test_units, test_samples) = count(test_groups);
    println!(
        "split: {train_samples} sample(s) from {train_units} {unit}(s) train, {test_samples} sample(s) from {test_units} {unit}(s) held out"
    );
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
        // A rollout labels from a game result and never reaches this function.
        // `main` routes it into `play_rollouts` instead.
        LabelSource::Rollout => {
            outcome.dropped = Some("a rollout label does not come from a search");
        }
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

/// The measured cost of one labeling sample.
struct Calibration {
    /// What one entry of `times` measures.
    unit: &'static str,
    /// Wall-clock seconds of each unit.
    times: Vec<f64>,
    /// Labels that the sample kept.
    kept: usize,
    /// Labels that the sample threw away.
    dropped: usize,
    /// The count that `--positions` sizes.
    ///
    /// A search run sizes `--positions` by attempted positions, and a rollout
    /// run sizes it by kept labels. `runbook/refresh_and_train.py` multiplies
    /// the reported rate by the wanted run time to fill that option, so the
    /// rate must count the same thing that the option counts.
    sized: usize,
    /// Units that the sample tried.
    attempted: usize,
    /// Wall time of the whole stage.
    elapsed: Duration,
    /// Threads that produced the sample.
    workers: usize,
}

/// The cost figures of a search-labeled sample.
fn search_calibration(
    args: &Args,
    outcomes: &[LabelOutcome],
    corpus_size: usize,
    elapsed: Duration,
) -> Calibration {
    let kept = outcomes
        .iter()
        .filter(|outcome| outcome.value.is_some())
        .count();
    Calibration {
        unit: "position",
        times: outcomes.iter().map(|outcome| outcome.seconds).collect(),
        kept,
        dropped: outcomes.len() - kept,
        sized: outcomes.len(),
        attempted: corpus_size,
        elapsed,
        workers: effective_worker_count(args.worker_count(), corpus_size),
    }
}

/// Reports the measured label cost, then leaves the run to the operator.
fn report_calibration(calibration: &Calibration) {
    let mut times: Vec<f64> = calibration.times.clone();
    times.sort_by(f64::total_cmp);
    if times.is_empty() {
        println!("calibration: no {} finished", calibration.unit);
        return;
    }

    let unit = calibration.unit;
    let median = times[times.len() / 2];
    let largest = times.last().copied().unwrap_or(0.0);
    let total: f64 = times.iter().sum();
    let rate = calibration_throughput(calibration.sized, calibration.elapsed);
    let kept_rate = calibration_throughput(calibration.kept, calibration.elapsed);

    println!(
        "calibration over {} of {} {unit}(s)",
        times.len(),
        calibration.attempted
    );
    println!(
        "  kept {} labels, dropped {}",
        calibration.kept, calibration.dropped
    );
    println!(
        "  median {median:.2} s, max {largest:.2} s, mean {:.2} s",
        total / times.len() as f64
    );
    println!(
        "  {rate:.2} labels per second on {} workers",
        calibration.workers
    );
    if kept_rate > 0.0 {
        for hours in [1.0, 10.0, 12.0] {
            println!(
                "  a {hours:.0} hour budget yields about {:.0} labels",
                kept_rate * hours * 3600.0
            );
        }
    }
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
        parse(extra).0
    }

    /// The parsed options, with the match record that names the options that
    /// this command line set.
    fn parse(extra: &[&str]) -> (Args, ArgMatches) {
        let values = std::iter::once("train_eval").chain(extra.iter().copied());
        let matches = Args::command()
            .try_get_matches_from(values)
            .expect("the test options must parse");
        let args = Args::from_arg_matches(&matches).expect("the test options must map");
        (args, matches)
    }

    /// One played game with the given winner.
    fn game(winner: Option<Player>) -> selfplay::GameResult {
        selfplay::GameResult { winner, turns: 42 }
    }

    /// `count` samples that all carry `id` in their first feature.
    fn group(id: usize, count: usize) -> Vec<ValueSample<FEATURE_COUNT>> {
        let mut features = [0.0; FEATURE_COUNT];
        features[0] = id as f64;
        (0..count)
            .map(|_| ValueSample {
                features,
                label: 1.0,
            })
            .collect()
    }

    #[test]
    fn invalid_training_ranges_fail_before_data_loading() {
        for options in [
            vec!["--positions", "0"],
            vec!["--calibrate-positions", "0"],
            vec!["--turns-per-match", "0"],
            vec!["--turn-cap", "0"],
            vec!["--rollout-iterations", "0"],
            vec!["--rollout-depth", "0"],
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
    fn calibration_uses_wall_time_and_the_effective_worker_count() {
        let rate = calibration_throughput(20, Duration::from_secs_f64(41.1));
        assert!((rate - 20.0 / 41.1).abs() < 1e-12);
        assert_eq!(effective_worker_count(100, 20), 20);
        assert_eq!(effective_worker_count(0, 20), 1);
    }

    #[test]
    fn a_rollout_label_reads_the_result_of_its_own_game() {
        assert_eq!(
            game_outcome(&game(Some(Player::P1))),
            GameOutcome::Keep(1.0)
        );
        assert_eq!(
            game_outcome(&game(Some(Player::P2))),
            GameOutcome::Keep(0.0)
        );
    }

    #[test]
    fn a_game_with_no_winner_drops_every_position_of_that_game() {
        assert_eq!(game_outcome(&game(None)), GameOutcome::Drop);

        // The stage counts the dropped positions and keeps no game.
        let mut rollout = Rollout::default();
        for winner in [Some(Player::P1), None, Some(Player::P2)] {
            rollout.played += 1;
            match game_outcome(&game(winner)) {
                GameOutcome::Drop => {
                    rollout.dropped_games += 1;
                    rollout.dropped_positions += 7;
                }
                GameOutcome::Keep(label) => rollout.games.push(RolloutGame {
                    index: rollout.games.len(),
                    features: Vec::new(),
                    label,
                }),
            }
        }
        assert_eq!(rollout.games.len(), 2);
        assert_eq!(rollout.dropped_positions, 7);
        assert_eq!(rollout.scored(), 2);
    }

    #[test]
    fn a_group_split_keeps_every_position_of_one_group_on_one_side() {
        let groups: Vec<Vec<ValueSample<FEATURE_COUNT>>> =
            (0..10).map(|id| group(id, 4)).collect();
        let (train, test) = train::split(&groups, 0.2);
        assert_eq!(train.len() + test.len(), groups.len());
        assert!(!test.is_empty(), "the split held nothing out");

        // A group keeps all four of its positions, and it enters one side
        // alone.
        let ids = |set: &[Vec<ValueSample<FEATURE_COUNT>>]| -> Vec<f64> {
            set.iter()
                .map(|entry| {
                    assert_eq!(entry.len(), 4, "the split tore a group apart");
                    entry[0].features[0]
                })
                .collect()
        };
        let train_ids = ids(&train);
        for id in ids(&test) {
            assert!(
                !train_ids.contains(&id),
                "group {id} entered both sides of the split"
            );
        }
    }

    #[test]
    fn a_rollout_group_holds_both_orientations_of_one_opening() {
        // The two games of one opening start from one drawn position, and
        // `eval::features` is antisymmetric. A split by game would train on
        // the negated opening position that it then holds out.
        let mut rollout = Rollout::default();
        for opening in 0..3 {
            for side in 0..GAMES_PER_OPENING {
                let mut features = [0.0; FEATURE_COUNT];
                features[0] = if side == 0 { 1.0 } else { -1.0 };
                rollout.games.push(RolloutGame {
                    index: opening * GAMES_PER_OPENING + side,
                    features: vec![features; 2],
                    label: side as f64,
                });
            }
        }

        let groups = rollout_samples(&rollout);
        assert_eq!(groups.len(), 3, "one group for each opening");
        for group in &groups {
            assert_eq!(group.len(), 4, "two games of two positions each");
            let first: Vec<f64> = group.iter().map(|sample| sample.features[0]).collect();
            assert!(first.contains(&1.0) && first.contains(&-1.0));
            let labels: Vec<f64> = group.iter().map(|sample| sample.label).collect();
            assert!(labels.contains(&0.0) && labels.contains(&1.0));
        }

        // The split then holds every position of the opening on one side.
        let (train, test) = train::split(&groups, 0.34);
        assert_eq!(test.len(), 1);
        assert_eq!(train.len(), 2);
        assert!(test[0].iter().all(|sample| sample.features[0].abs() == 1.0));
    }

    #[test]
    fn a_dropped_mirror_leaves_its_opening_a_group_of_one_game() {
        // The turn cap drops one orientation and keeps the other, so the game
        // index of an opening can be absent.
        let mut rollout = Rollout::default();
        for index in [1usize, 2, 5] {
            rollout.games.push(RolloutGame {
                index,
                features: vec![[0.0; FEATURE_COUNT]; 3],
                label: 1.0,
            });
        }
        let groups = rollout_samples(&rollout);
        assert_eq!(groups.len(), 3, "openings 0, 1 and 2");
        assert!(groups.iter().all(|group| group.len() == 3));
    }

    #[test]
    fn the_accept_rule_bench_seed_is_refused() {
        // `benches/eval_calibration` reads the openings of this seed, and that
        // bench decides whether the run commits.
        let refused = args(&["--seed", &ACCEPT_RULE_BENCH_SEED.to_string()]);
        assert!(validate_args(&refused).is_err());
        assert_ne!(args(&[]).seed, ACCEPT_RULE_BENCH_SEED);
        assert!(validate_args(&args(&[])).is_ok());
    }

    #[test]
    fn a_label_source_refuses_an_option_that_it_ignores() {
        for options in [
            vec!["--labels", "rollout", "--label-deadline", "30"],
            vec!["--labels", "rollout", "--label-depth", "3"],
            vec!["--labels", "rollout", "--min-label-depth", "1"],
            vec!["--labels", "rollout", "--iterative-deepening"],
            vec!["--labels", "rollout", "--label-chance", "top4"],
            vec!["--labels", "rollout", "--label-max-actions", "8"],
            vec!["--labels", "rollout", "--no-prune-dominated"],
            vec!["--labels", "rollout", "--turns-per-match", "20"],
            vec!["--turn-cap", "60"],
            vec!["--rollout-iterations", "8"],
            vec!["--labels", "selfplay", "--rollout-depth", "1"],
        ] {
            let (args, matches) = parse(&options);
            assert!(
                validate_label_source(&matches, args.labels).is_err(),
                "accepted {options:?}"
            );
        }
    }

    #[test]
    fn a_label_source_accepts_its_own_options_and_the_shared_ones() {
        for options in [
            vec!["--labels", "rollout", "--turn-cap", "60"],
            vec!["--labels", "rollout", "--rollout-iterations", "8"],
            vec!["--labels", "rollout", "--rollout-depth", "1"],
            // Every option below belongs to no one source.
            vec!["--labels", "rollout", "--positions", "40"],
            vec!["--labels", "rollout", "--seed", "7"],
            vec!["--labels", "rollout", "--time-budget", "60"],
            vec!["--labels", "rollout", "--workers", "4"],
            vec!["--labels", "rollout", "--teamsheet-mix", "1"],
            vec!["--label-deadline", "30"],
            vec!["--labels", "selfplay", "--label-depth", "3"],
        ] {
            let (args, matches) = parse(&options);
            assert!(
                validate_label_source(&matches, args.labels).is_ok(),
                "refused {options:?}"
            );
        }
    }

    #[test]
    fn a_rollout_plays_the_search_bot_on_both_sides() {
        let config = args(&["--labels", "rollout"]).game_config();
        assert_eq!(config.turn_cap, 120);
        assert_eq!(
            config.p1,
            TurnPolicy::Search {
                iterations: 64,
                depth: 2
            }
        );
        // One search answers both sides only while the two settings agree.
        assert_eq!(config.p1, config.p2);

        let tuned = args(&[
            "--labels",
            "rollout",
            "--rollout-iterations",
            "16",
            "--rollout-depth",
            "1",
            "--turn-cap",
            "60",
        ])
        .game_config();
        assert_eq!(tuned.turn_cap, 60);
        assert_eq!(
            tuned.p1,
            TurnPolicy::Search {
                iterations: 16,
                depth: 1
            }
        );
    }

    #[test]
    fn the_calibration_rate_counts_what_positions_counts() {
        // A rollout sizes `--positions` by kept labels, so the rate that the
        // runbook reads must count labels and not games.
        let rollout = Rollout {
            games: vec![
                RolloutGame {
                    index: 0,
                    features: Vec::new(),
                    label: 1.0,
                },
                RolloutGame {
                    index: 1,
                    features: Vec::new(),
                    label: 0.0,
                },
            ],
            played: 2,
            seconds: vec![1.0, 3.0],
            elapsed: Duration::from_secs(4),
            workers: 2,
            ..Rollout::default()
        };
        let calibration = rollout.calibration();
        assert_eq!(calibration.unit, "game");
        assert_eq!(calibration.sized, calibration.kept);
        assert_eq!(calibration.attempted, 2);
        assert_eq!(calibration.times.len(), 2);
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
