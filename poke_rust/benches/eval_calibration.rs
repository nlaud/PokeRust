//! Measures the leaf evaluator against played games.
//!
//! Run from `poke_rust/`:
//! ```sh
//! cargo bench --bench eval_calibration
//! ```
//!
//! # Why this benchmark exists
//!
//! `bin/train_eval` fits `eval::fitted` from labels that `solve` produced, and
//! `solve` scores its own horizon with those same weights. The held-out error
//! of that fit therefore measures agreement with the loop, not agreement with a
//! game result. It cannot accept or reject a weight change.
//!
//! This benchmark plays whole games, records what each evaluator predicted
//! before each turn, and records who won. The report compares the predicted win
//! probability against the realized rate, in ten fixed buckets.
//!
//! # What the report holds
//!
//! One table for each of three evaluators: `heuristic`, `fitted`, and
//! `fitted_mlp`. All three read the same games and the same results, so the
//! tables compare directly.
//!
//! Each row is one bucket. `n` is the position count. `games` is the count of
//! distinct games that supplied those positions.
//!
//! The game is the independent unit, not the position. 400 positions from 3
//! games hold 3 independent observations.
//!
//! # The opening set
//!
//! One base seed draws every opening. Each opening plays two games, and the
//! second game exchanges the two sides. The pair removes team strength from the
//! aggregate rate, so the reported P1 win rate must sit near 0.5. That rate is a
//! self-check on the driver and on the engine.
//!
//! A game that reaches the turn cap has no result. The benchmark drops its
//! positions and reports the dropped count.
//!
//! # Reproducibility
//!
//! `--teamsheet-mix 1` makes the report exact. Two runs of the same options
//! then print the same numbers.
//!
//! A lower mix draws part of the openings from the usage cache, and
//! `meta::generate_meta_team` reads its candidate list from a `HashMap`. That
//! order changes with the process, so a cache-drawn team changes with it. The
//! statistics still move very little. Over three runs of the default options
//! the Brier score of `fitted` stayed inside 0.004. The order of the three
//! evaluators did not change.
//!
//! Use the default mix to match the corpus of `bin/train_eval`.
//!
//! The play policy also decides whether two runs play the same games.
//! `--policy policy` reads `eval::fitted_policy_weights`, which is
//! `weights/policy_v1.json`. `bin/train_eval` writes that file, so a training
//! run changes the joint actions that this option draws.
//!
//! `--policy hand` reads `eval::HAND_POLICY_WEIGHTS`, a constant of the crate.
//! No training run moves it. Use `--policy hand --teamsheet-mix 1` to compare
//! two weight sets on identical games.
//!
//! # Options
//!
//! ```text
//! --games N              Games to play. Rounded up to an even number.
//! --policy random|policy|hand|search
//!                        `policy` reads the fitted policy weights, which a
//!                        training run rewrites. `hand` reads the constant.
//! --temperature F        Softmax temperature of the policy play.
//! --search-iterations N  Iterations of the search play.
//! --search-depth N       Depth of the search play.
//! --turn-cap N           Steps that one game may take. A replacement step
//!                        counts one.
//! --seed N               Base seed of the opening set.
//! --teamsheet-dir PATH   Archived rosters. The default is ../teamsheets.
//! --no-teamsheet-dir     Removes the archive. Every opening then draws from
//!                        the usage cache.
//! --teamsheet-mix F      Share of matchups that draw from the archive.
//! --meta-root PATH       The usage cache.
//! ```

use std::io::Write;
use std::path::PathBuf;
use std::time::Instant;

use poke_rust::VERBOSITY;
use poke_rust::meta::{MetaDex, MetaFormat};
use poke_rust::selfplay::{self, GameConfig, MatchupConfig, PolicyWeights, TeamPool, TurnPolicy};
use poke_rust::solver::calibration::{CalibrationCurve, Sample};
use poke_rust::solver::eval::{self, EvalContext, LeafEvaluator};
use poke_rust::state::battle::{BattleState, MatchState, Player};
use poke_rust::state::dex_data::{parse_learnset_dex, parse_move_dex, parse_pokemon_dex};

/// Openings between two progress lines.
const PROGRESS_STEP: usize = 25;

/// The evaluators that the report compares.
const EVALUATORS: [(&str, LeafEvaluator); 3] = [
    ("heuristic", eval::heuristic),
    ("fitted", eval::fitted),
    ("fitted_mlp", eval::fitted_mlp),
];

/// The benchmark options.
struct Options {
    games: usize,
    policy: TurnPolicy,
    turn_cap: usize,
    seed: u64,
    teamsheet_dir: Option<PathBuf>,
    teamsheet_mix: f64,
    meta_root: PathBuf,
}

impl Default for Options {
    fn default() -> Options {
        Options {
            // Two hundred openings of two games each. The default play costs
            // no turn simulation, so the whole sweep takes about two seconds.
            games: 400,
            policy: TurnPolicy::Policy {
                temperature: 1.0,
                weights: PolicyWeights::Fitted,
            },
            // A doubles game settles well inside this cap. The cap is a hang
            // guard, and a game that reaches it is dropped.
            turn_cap: 120,
            seed: 1,
            teamsheet_dir: Some(PathBuf::from("../teamsheets")),
            teamsheet_mix: 0.8,
            meta_root: PathBuf::from("../meta_scraper/data"),
        }
    }
}

/// Reads the options from the command line.
///
/// `cargo bench` passes its own flags to the binary, so an unknown flag is
/// ignored rather than an error.
fn parse_options() -> Result<Options, String> {
    let mut options = Options::default();
    let mut policy = "policy".to_string();
    let mut temperature = 1.0f64;
    let mut iterations = 64u32;
    let mut depth = 2u8;

    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let mut index = 0usize;
    while index < arguments.len() {
        let flag = arguments[index].as_str();
        // Every option below takes one value, so the reader looks one ahead.
        let value = arguments.get(index + 1).map(String::as_str);
        let taken = match flag {
            "--games" => {
                options.games = read_value(flag, value)?;
                true
            }
            "--policy" => {
                policy = need_value(flag, value)?.to_string();
                true
            }
            "--temperature" => {
                temperature = read_value(flag, value)?;
                true
            }
            "--search-iterations" => {
                iterations = read_value(flag, value)?;
                true
            }
            "--search-depth" => {
                depth = read_value(flag, value)?;
                true
            }
            "--turn-cap" => {
                options.turn_cap = read_value(flag, value)?;
                true
            }
            "--seed" => {
                options.seed = read_value(flag, value)?;
                true
            }
            "--teamsheet-dir" => {
                options.teamsheet_dir = Some(PathBuf::from(need_value(flag, value)?));
                true
            }
            "--teamsheet-mix" => {
                options.teamsheet_mix = read_value(flag, value)?;
                true
            }
            "--meta-root" => {
                options.meta_root = PathBuf::from(need_value(flag, value)?);
                true
            }
            "--no-teamsheet-dir" => {
                options.teamsheet_dir = None;
                false
            }
            // `cargo bench` supplies `--bench` and can supply a name filter.
            _ => false,
        };
        index += if taken { 2 } else { 1 };
    }

    options.policy = match policy.as_str() {
        "random" => TurnPolicy::Random,
        "policy" => TurnPolicy::Policy {
            temperature,
            weights: PolicyWeights::Fitted,
        },
        "hand" => TurnPolicy::Policy {
            temperature,
            weights: PolicyWeights::Hand,
        },
        "search" => TurnPolicy::Search { iterations, depth },
        other => {
            return Err(format!(
                "--policy takes random, policy, hand, or search, not {other}"
            ));
        }
    };
    if options.games == 0 {
        return Err("--games must be at least 1".to_string());
    }
    if options.turn_cap == 0 {
        return Err("--turn-cap must be at least 1".to_string());
    }
    if !(0.0..=1.0).contains(&options.teamsheet_mix) {
        return Err("--teamsheet-mix must be from 0 through 1".to_string());
    }
    if !temperature.is_finite() || temperature <= 0.0 {
        return Err("--temperature must be a finite positive number".to_string());
    }
    Ok(options)
}

/// The value of one option, or an error that names the option.
fn need_value<'a>(flag: &str, value: Option<&'a str>) -> Result<&'a str, String> {
    value.ok_or_else(|| format!("{flag} needs a value"))
}

/// The parsed value of one option.
fn read_value<T: std::str::FromStr>(flag: &str, value: Option<&str>) -> Result<T, String> {
    let text = need_value(flag, value)?;
    text.parse::<T>()
        .map_err(|_| format!("{flag} cannot read {text}"))
}

/// One recorded position: what each evaluator predicted, and which game it came
/// from.
struct Recorded {
    game: usize,
    predicted: [f64; EVALUATORS.len()],
}

fn main() {
    let options = match parse_options() {
        Ok(options) => options,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(2);
        }
    };
    // The sweep plays thousands of turns. Engine output would bury the report.
    let _ = VERBOSITY.set(0);

    let pokemon_dex = parse_pokemon_dex("../pokemon_info/showdownDex.txt");
    let move_dex = parse_move_dex("../pokemon_info/showdownMoves.txt");
    let learnset_dex = parse_learnset_dex("../pokemon_info/showdownLearnsets.txt");

    let matchup = MatchupConfig {
        teamsheet_mix: options.teamsheet_mix,
        ..MatchupConfig::default()
    };
    let format = MetaFormat::from_active_per_side(matchup.active_per_side);
    let meta_dex = match MetaDex::load(&options.meta_root, None, format) {
        Ok(loaded) => loaded,
        Err(error) => {
            eprintln!(
                "cannot read the usage cache at {}: {error}",
                options.meta_root.display()
            );
            eprintln!("run meta_scraper/update_meta.py first, or pass --meta-root");
            std::process::exit(1);
        }
    };

    let pool = match options.teamsheet_dir.as_ref() {
        Some(dir) => match TeamPool::load(dir, matchup.brought_per_side, &pokemon_dex, &move_dex) {
            Ok(loaded) => Some(loaded),
            Err(error) => {
                eprintln!("cannot read the teamsheet directory: {error}");
                std::process::exit(1);
            }
        },
        None => None,
    };

    // An odd game count would leave one opening without its mirror, and the
    // aggregate rate would then carry that opening's team strength.
    let openings = options.games.div_ceil(2);
    let games = openings * 2;

    println!("eval calibration");
    println!(
        "  format: {format:?}, {} active, {} brought of {}, tera off, mega on",
        matchup.active_per_side, matchup.brought_per_side, matchup.roster_size,
    );
    println!("  policy: {}", options.policy.label());
    println!(
        "  {openings} openings, {games} games, turn cap {}, seed {}",
        options.turn_cap, options.seed,
    );
    match pool.as_ref() {
        Some(pool) => {
            println!(
                "  teams: {} archived roster(s), {:.0}% archived and {:.0}% usage cache",
                pool.len(),
                100.0 * matchup.teamsheet_mix,
                100.0 * (1.0 - matchup.teamsheet_mix),
            );
            // A sheet can name a Pokemon or a move that this dex does not hold.
            // The parser drops that block, and the roster can then hold fewer
            // Pokemon than the format brings. Without this line the archive
            // looks smaller than it is for no stated reason.
            if pool.dropped_short() > 0 {
                println!(
                    "  {} file(s) held fewer than {} usable Pokemon and did not load",
                    pool.dropped_short(),
                    matchup.brought_per_side,
                );
            }
        }
        None => println!("  teams: usage cache"),
    }
    let _ = std::io::stdout().flush();

    let game_config = GameConfig {
        p1: options.policy,
        p2: options.policy,
        turn_cap: options.turn_cap,
    };
    let ctx = EvalContext::new(&pokemon_dex, &move_dex);

    let started = Instant::now();
    let mut samples: Vec<Vec<Sample>> = vec![Vec::new(); EVALUATORS.len()];
    let mut played = 0usize;
    let mut dropped_games = 0usize;
    let mut dropped_positions = 0usize;
    let mut skipped_openings = 0usize;
    let mut p1_wins = 0usize;
    let mut total_turns = 0usize;

    for opening_index in 0..openings {
        // The block lets a skipped opening leave through `break` and still reach
        // the progress line below.
        'opening: {
            let seed = options
                .seed
                .wrapping_mul(0x9E37_79B9_7F4A_7C15)
                .wrapping_add(opening_index as u64);
            let Some(opening) = selfplay::draw_opening(
                &matchup,
                &meta_dex,
                pool.as_ref(),
                &pokemon_dex,
                &learnset_dex,
                seed,
            ) else {
                skipped_openings += 1;
                break 'opening;
            };

            // The two orientations of one opening. The second exchanges the
            // sides, and each roster keeps the four Pokemon that it brought.
            let starts: Option<Vec<(u64, MatchState)>> = [opening.clone(), opening.swapped()]
                .iter()
                .enumerate()
                .map(|(side, orientation)| {
                    let game_seed = seed ^ (side as u64).wrapping_mul(0x5DEE_CE66_D125_5AB1);
                    selfplay::opening_match(
                        &matchup,
                        orientation,
                        &pokemon_dex,
                        &move_dex,
                        game_seed,
                    )
                    .map(|start| (game_seed, start))
                })
                .collect();
            // The P1 win rate is a self-check only while every game has its
            // mirror. One orientation on its own leaves that opening's team
            // strength inside the rate. The pair therefore plays together, or it
            // does not play.
            let Some(starts) = starts else {
                skipped_openings += 1;
                break 'opening;
            };

            for (game_seed, start) in starts {
                let game = played;
                played += 1;

                let mut recorded: Vec<Recorded> = Vec::new();
                let mut record = |battle: &BattleState| {
                    let mut predicted = [0.0f64; EVALUATORS.len()];
                    for (slot, (_, evaluator)) in EVALUATORS.iter().enumerate() {
                        predicted[slot] = evaluator(battle, &ctx);
                    }
                    recorded.push(Recorded { game, predicted });
                };
                let result = selfplay::play_game(
                    &start,
                    &game_config,
                    &pokemon_dex,
                    &move_dex,
                    game_seed,
                    &mut record,
                );
                total_turns += result.turns;

                // A game with no winner has no label, so its positions cannot
                // enter the curve.
                let Some(winner) = result.winner else {
                    dropped_games += 1;
                    dropped_positions += recorded.len();
                    continue;
                };
                let p1_won = winner == Player::P1;
                if p1_won {
                    p1_wins += 1;
                }
                for entry in &recorded {
                    for (slot, set) in samples.iter_mut().enumerate() {
                        set.push(Sample::new(entry.predicted[slot], p1_won, entry.game));
                    }
                }
            }
        }
        report_progress(opening_index + 1, openings, started);
    }
    let elapsed = started.elapsed();

    let scored = played - dropped_games;
    println!();
    println!(
        "played {played} game(s) in {:.1} s, {total_turns} turn(s) resolved",
        elapsed.as_secs_f64()
    );
    if skipped_openings > 0 {
        println!("  {skipped_openings} opening(s) never reached a battle");
    }
    println!("  {dropped_games} game(s) hit the turn cap; dropped {dropped_positions} position(s)",);
    if scored == 0 {
        println!("no game produced a result; the report is empty");
        return;
    }
    println!(
        "  P1 won {p1_wins} of {scored} scored game(s) ({:.3}); each opening plays both sides, so this belongs near 0.500",
        p1_wins as f64 / scored as f64,
    );
    if dropped_games > 0 {
        // A dropped game leaves its mirror alone in the count, and that
        // opening's team strength then sits inside the rate.
        println!("  {dropped_games} dropped game(s) left their mirror unpaired");
    }
    // A table counts one entry for each position, so a long game carries more
    // weight there than a short one. Its realized rate is therefore a different
    // quantity from this game rate, and neither one replaces the other. The two
    // move apart with the sample and not in a fixed direction.
    println!("  a table's realized rate weights each game by its position count");

    for (slot, (name, _)) in EVALUATORS.iter().enumerate() {
        let curve = CalibrationCurve::from_samples(&samples[slot]);
        print_curve(name, &curve);
    }
}

/// Prints the sweep's progress, so a long run shows that it still moves.
///
/// The line repeats every [`PROGRESS_STEP`] openings and at the last one. A
/// line for every opening would bury the report when the output is a file.
fn report_progress(done: usize, total: usize, started: Instant) {
    if !done.is_multiple_of(PROGRESS_STEP) && done != total {
        return;
    }
    print!(
        "\r  opening {done}/{total}, {:.1} s elapsed          ",
        started.elapsed().as_secs_f64()
    );
    let _ = std::io::stdout().flush();
}

/// Prints one evaluator's table.
fn print_curve(name: &str, curve: &CalibrationCurve) {
    println!();
    println!("{name}");
    println!("  bucket        n  games   predicted   realized       gap");
    for bucket in &curve.buckets {
        match (bucket.mean_predicted, bucket.realized, bucket.gap()) {
            (Some(predicted), Some(realized), Some(gap)) => println!(
                "  {:.1}-{:.1}  {:>7}  {:>5}  {:>10.3}  {:>9.3}  {:>8.3}",
                bucket.low, bucket.high, bucket.positions, bucket.games, predicted, realized, gap,
            ),
            // An empty bucket has no rate to print, and it enters no statistic.
            _ => println!(
                "  {:.1}-{:.1}  {:>7}  {:>5}           -          -         -",
                bucket.low, bucket.high, bucket.positions, bucket.games,
            ),
        }
    }
    println!(
        "  {} position(s) from {} game(s); mean predicted {:.3}, realized {:.3}",
        curve.positions,
        curve.games,
        curve.mean_predicted.unwrap_or(f64::NAN),
        curve.realized.unwrap_or(f64::NAN),
    );
    println!(
        "  MAE {:.4}  Brier {:.4}  LogLoss {:.4}  ECE {:.4}",
        curve.mean_absolute_error, curve.brier, curve.log_loss, curve.expected_calibration_error,
    );
}
