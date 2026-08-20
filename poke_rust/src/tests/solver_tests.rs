//! Tests the perfect-information solver.
//!
//! [`all_algorithms_agree`] compares all search algorithms.
//! A different value identifies an invalid cutoff.
//! `solver::matrix` contains equilibrium tests.
//!
//! Debug builds make `simulate_turn` about ten times slower.
//! Tests therefore use few moves, short benches, and one damage roll.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use crate::data::ability::Ability;
use crate::data::item::Item;
use crate::data::pokemon_move::PokemonMove;
use crate::data::species::Species;
use crate::information::determinize::{DeterminizeConfig, determinize_seeded};
use crate::information::inference::InferenceConfig;
use crate::information::unknowns::{
    InformationMode, UnknownBattleState, UnknownMatchState, UnknownPokemonState,
    UnknownTeamPreviewState,
};
use crate::meta::{MetaDex, MetaFormat};
use crate::simulator::generative::{TransitionConfig, sample_transition};
use crate::simulator::scoped_sample_rng;
use crate::solver::actions as solver_actions;
use crate::solver::belief::{BeliefError, Particle, ParticleBelief};
use crate::solver::chance::ChanceMode;
use crate::solver::exploit::{ResponseMode, exploitability, respond, respond_within_budget};
use crate::solver::ismcts::{self, IsmctsConfig};
use crate::solver::matrix::solve_matrix_game;
use crate::solver::mccfr::{self, ContinualConfig, MccfrConfig};
use crate::solver::mcts::{self, MctsConfig, SelectionPolicy, TransitionMode, Widening};
use crate::solver::pimc::{self, PimcConfig};
use crate::solver::pool::{WorkerPool, job_seed};
use crate::solver::preview::{
    OpenListConfig, OpenListError, PreviewCellCache, PreviewConfig, open_list_worlds,
    precompute_preview_cells, preview_cell_value, preview_choices, solve_open_list_preview,
    solve_open_list_preview_cancellable, solve_open_list_preview_progress_cancellable,
    solve_team_preview, solve_team_preview_cached, solve_team_preview_cancellable,
    solve_team_preview_progress_cancellable,
};
use crate::solver::{
    CHAIN_MASK, CancelFlag, EXTENDED_FLAG, JointActionProb, SolveConfig, SolveError, SolveResult,
    SolveWarning, SolverAlgorithm, eval, forced_descent, root_descent, solve, solve_seeded,
    solve_seeded_cancellable, solve_seeded_progress_cancellable,
};
use crate::state::battle::{
    AttackCommand, BattleCommand, BattleMechanics, BattleState, MatchState, Player, PlayerCommand,
    TeamPreviewState,
};
use crate::state::dex_data::{MoveData, PokemonData, VolatileStatus, parse_move_dex};
use crate::state::pokemon::{Nature, PokemonState, VolatileStatusState, build_pokemon_state};
use crate::tests::simuilator_test_helpers::{battle_state_from_lists, move_dex, pokemon_dex};
use crate::user::battle_command_description;

fn dexes() -> (
    &'static HashMap<Species, PokemonData>,
    &'static HashMap<PokemonMove, MoveData>,
) {
    (pokemon_dex(), move_dex())
}

/// A level-50 Pokemon with a chosen move set, neutral nature and no item, so
/// that a test's outcome depends only on what it deliberately varies.
fn mon(species: Species, moves: &[PokemonMove]) -> PokemonState {
    let mut slots: [Option<PokemonMove>; 4] = [None, None, None, None];
    for (slot, m) in slots.iter_mut().zip(moves) {
        *slot = Some(m.clone());
    }
    let (pokemon_dex, move_dex) = dexes();
    build_pokemon_state(
        species,
        pokemon_dex,
        move_dex,
        Some(50),
        Some(slots),
        None,
        Some(Ability::None),
        Some(Nature::Serious),
        None,
        None,
        Some([0; 6]),
        Some([31; 6]),
        false,
    )
}

/// Small and cheap: one damage roll, no crit branching, one ply. Individual
/// tests widen only the axis they are about.
fn base_config() -> SolveConfig {
    SolveConfig {
        depth: 1,
        damage_rolls: 1,
        consider_crit: false,
        chance: ChanceMode::Enumerate,
        algorithm: SolverAlgorithm::DoubleOracle,
        ..SolveConfig::default()
    }
}

/// Two Pokemon a side, two moves each: enough structure for a real matrix,
/// small enough to search repeatedly in a debug build.
fn contested_position() -> MatchState {
    MatchState::BattleState(battle_state_from_lists(
        vec![mon(
            Species::Pikachu,
            &[PokemonMove::Thunderbolt, PokemonMove::QuickAttack],
        )],
        vec![mon(
            Species::Gyarados,
            &[PokemonMove::Waterfall, PokemonMove::IceFang],
        )],
        vec![mon(
            Species::Snorlax,
            &[PokemonMove::BodySlam, PokemonMove::Crunch],
        )],
        vec![mon(
            Species::Gengar,
            &[PokemonMove::ShadowBall, PokemonMove::SludgeBomb],
        )],
    ))
}

/// An automatic-target move must not display a missing target as an error.
#[test]
fn automatic_target_move_description_omits_the_target_arrow() {
    let state = battle_state_from_lists(
        vec![mon(Species::Aerodactyl, &[PokemonMove::RockSlide])],
        Vec::new(),
        vec![mon(Species::Garchomp, &[PokemonMove::DragonClaw])],
        Vec::new(),
    );
    let command = BattleCommand::Attack(AttackCommand {
        move_slot: 0,
        target: None,
        terastallize: false,
        mega_evolve: false,
    });

    assert_eq!(
        battle_command_description(&state, Player::P1, 0, &command),
        "Use Rock Slide"
    );
}

fn assert_valid_strategies(result: &SolveResult) {
    for (label, strategy) in [("P1", &result.p1_strategy), ("P2", &result.p2_strategy)] {
        let total: f64 = strategy.iter().map(|a| a.probability).sum();
        assert!(
            (total - 1.0).abs() < 1e-6,
            "{label} strategy sums to {total}"
        );
        assert!(
            strategy.iter().all(|a| a.probability > 0.0),
            "{label} strategy kept a zero-probability action"
        );
        assert!(!strategy.is_empty(), "{label} strategy is empty");
    }
    assert!(
        (0.0..=1.0).contains(&result.value),
        "value out of range: {}",
        result.value
    );
    assert!((result.p1_win_odds + result.p2_win_odds - 1.0).abs() < 1e-9);
}

/// The central soundness test.
///
/// Backward induction evaluates every cell of every matrix. Serialized bounds
/// skips whole subgames whose two serializations agree. Double oracle never
/// builds most of the matrix at all. If any of that pruning is unsound, the
/// three disagree — and they must not, because they are three ways of computing
/// the same number.
#[test]
fn all_algorithms_agree() {
    let (pokemon_dex, move_dex) = dexes();
    let state = contested_position();

    let mut values = Vec::new();
    for algorithm in [
        SolverAlgorithm::BackwardInduction,
        SolverAlgorithm::SerializedBounds,
        SolverAlgorithm::DoubleOracle,
    ] {
        let config = SolveConfig {
            algorithm,
            ..base_config()
        };
        let result = solve(&state, pokemon_dex, move_dex, &config).expect("position is solvable");
        assert_valid_strategies(&result);
        values.push((algorithm, result.value));
    }

    let reference = values[0].1;
    for (algorithm, value) in &values {
        assert!(
            (value - reference).abs() < 1e-6,
            "{algorithm:?} returned {value}, backward induction returned {reference}"
        );
    }
}

/// Double oracle with serialized bounds switched on is a distinct code path: the
/// bounds narrow the window handed to each recursive solve, so the restricted
/// game can converge against the window rather than against its own best
/// responses. That must still land on the same value.
#[test]
fn double_oracle_agrees_with_serialized_bounds_enabled() {
    let (pokemon_dex, move_dex) = dexes();
    let state = contested_position();

    let reference = solve(
        &state,
        pokemon_dex,
        move_dex,
        &SolveConfig {
            depth: 2,
            algorithm: SolverAlgorithm::BackwardInduction,
            ..base_config()
        },
    )
    .unwrap()
    .value;

    let bounded = solve(
        &state,
        pokemon_dex,
        move_dex,
        &SolveConfig {
            depth: 2,
            algorithm: SolverAlgorithm::DoubleOracle,
            use_serialized_bounds: true,
            ..base_config()
        },
    )
    .unwrap();

    assert_valid_strategies(&bounded);
    assert!(
        (bounded.value - reference).abs() < 1e-6,
        "bounded double oracle returned {}, backward induction returned {reference}",
        bounded.value
    );
}

/// The same equivalence one ply deeper, where the algorithms diverge much more
/// in how much of the tree they touch.
#[test]
fn all_algorithms_agree_at_depth_two() {
    let (pokemon_dex, move_dex) = dexes();
    let state = contested_position();

    let solve_with = |algorithm| {
        let config = SolveConfig {
            depth: 2,
            algorithm,
            ..base_config()
        };
        solve(&state, pokemon_dex, move_dex, &config)
            .expect("position is solvable")
            .value
    };

    let reference = solve_with(SolverAlgorithm::BackwardInduction);
    let double_oracle = solve_with(SolverAlgorithm::DoubleOracle);
    assert!(
        (double_oracle - reference).abs() < 1e-6,
        "double oracle returned {double_oracle}, backward induction returned {reference}"
    );
}

/// A progress hook must report the root rounds while the search runs, and it
/// must not move the answer. The solver panel publishes each of those rounds.
#[test]
fn the_root_progress_hook_reports_each_round_without_moving_the_value() {
    let (pokemon_dex, move_dex) = dexes();
    let state = contested_position();
    let config = SolveConfig {
        depth: 2,
        algorithm: SolverAlgorithm::DoubleOracle,
        workers: 4,
        ..base_config()
    };

    let reference =
        solve_seeded_cancellable(7, &state, pokemon_dex, move_dex, &config, None).unwrap();

    let rounds = Mutex::new(Vec::new());
    let hook = |round: crate::solver::RootRound| {
        rounds.lock().unwrap().push(round);
    };
    let reported = solve_seeded_progress_cancellable(
        7,
        &state,
        pokemon_dex,
        move_dex,
        &config,
        Some(&hook),
        None,
    )
    .unwrap();

    assert!(
        (reported.value - reference.value).abs() < 1e-9,
        "the hook moved the value from {} to {}",
        reference.value,
        reported.value
    );

    let rounds = rounds.into_inner().unwrap();
    assert!(!rounds.is_empty(), "the hook reported no round");
    // Iterative deepening runs one pass for each depth, and each pass reports
    // its own rounds, so the depth never falls.
    for pair in rounds.windows(2) {
        assert!(pair[1].depth >= pair[0].depth);
    }
    assert_eq!(
        rounds.last().unwrap().depth,
        reported.depth_reached,
        "the last round must belong to the returned pass"
    );
    let last_stats = &rounds.last().unwrap().stats;
    assert_eq!(last_stats.nodes_expanded, reported.stats.nodes_expanded);
    assert_eq!(last_stats.turns_simulated, reported.stats.turns_simulated);
    assert_eq!(
        last_stats.matrix_cells_evaluated,
        reported.stats.matrix_cells_evaluated
    );
    for round in &rounds {
        assert!((0.0..=1.0).contains(&round.value), "{}", round.value);
        // A round holds a complete strategy over the actions that it reached.
        let total: f64 = round.p1_strategy.iter().map(|a| a.probability).sum();
        assert!((total - 1.0).abs() < 1e-6, "P1 rates sum to {total}");
        let total: f64 = round.p2_strategy.iter().map(|a| a.probability).sum();
        assert!((total - 1.0).abs() < 1e-6, "P2 rates sum to {total}");
        // The statistics grow with the search, so a round can never report an
        // empty count.
        assert!(round.stats.matrix_cells_evaluated > 0);
    }
    // The last round of the last pass holds the answer that the search returns.
    let last = rounds.last().unwrap();
    assert!((last.value - reported.value).abs() < 1e-6);
}

/// Double oracle must reach the same answer having looked at strictly fewer
/// cells — otherwise it is backward induction with extra steps.
#[test]
fn double_oracle_evaluates_fewer_cells() {
    let (pokemon_dex, move_dex) = dexes();
    let state = contested_position();

    let full = solve(
        &state,
        pokemon_dex,
        move_dex,
        &SolveConfig {
            depth: 2,
            algorithm: SolverAlgorithm::BackwardInduction,
            ..base_config()
        },
    )
    .unwrap();
    let pruned = solve(
        &state,
        pokemon_dex,
        move_dex,
        &SolveConfig {
            depth: 2,
            algorithm: SolverAlgorithm::DoubleOracle,
            ..base_config()
        },
    )
    .unwrap();

    assert!(
        pruned.stats.turns_simulated < full.stats.turns_simulated,
        "double oracle simulated {} turns, backward induction {}",
        pruned.stats.turns_simulated,
        full.stats.turns_simulated
    );
    assert!(pruned.stats.matrix_cells_evaluated <= pruned.stats.matrix_cells_total);
}

/// Breadth over a spread of matchups, at the settings that exercise the most
/// code: double oracle plus serialized bounds, two plies.
///
/// Narrow hand-built positions miss floating-point edge cases that only appear
/// once the numbers are messy — the first version of this suite passed while the
/// benchmark panicked on a real position, because two best-response values that
/// are the same quantity summed in different orders crossed by one ulp and
/// inverted the alpha-beta bracket.
#[test]
fn varied_matchups_solve_without_panicking() {
    let (pokemon_dex, move_dex) = dexes();
    let roster = [
        (
            Species::Pikachu,
            [PokemonMove::Thunderbolt, PokemonMove::QuickAttack],
        ),
        (
            Species::Snorlax,
            [PokemonMove::BodySlam, PokemonMove::Crunch],
        ),
        (
            Species::Gengar,
            [PokemonMove::ShadowBall, PokemonMove::SludgeBomb],
        ),
        (
            Species::Gyarados,
            [PokemonMove::Waterfall, PokemonMove::IceFang],
        ),
        (
            Species::Machamp,
            [PokemonMove::CloseCombat, PokemonMove::Earthquake],
        ),
    ];

    let config = SolveConfig {
        depth: 2,
        algorithm: SolverAlgorithm::DoubleOracle,
        use_serialized_bounds: true,
        ..base_config()
    };

    for i in 0..roster.len() {
        for j in i..roster.len() {
            let (p1, p2) = (&roster[i], &roster[j]);
            let bench = &roster[(i + 1) % roster.len()];
            let state = MatchState::BattleState(battle_state_from_lists(
                vec![mon(p1.0.clone(), &p1.1)],
                vec![mon(bench.0.clone(), &bench.1)],
                vec![mon(p2.0.clone(), &p2.1)],
                vec![mon(bench.0.clone(), &bench.1)],
            ));

            let result = solve(&state, pokemon_dex, move_dex, &config)
                .unwrap_or_else(|e| panic!("{:?} vs {:?}: {e}", p1.0, p2.0));
            assert_valid_strategies(&result);
        }
    }
}

/// A position P1 cannot lose: P2's last Pokemon is at 1 HP with only a
/// status-less filler move, and P1 threatens a kill. The search should see the
/// win outright rather than through the heuristic.
#[test]
fn a_forced_win_is_reported_as_certain() {
    let (pokemon_dex, move_dex) = dexes();
    let mut battle = battle_state_from_lists(
        vec![mon(
            Species::Machamp,
            &[PokemonMove::CloseCombat, PokemonMove::Splash],
        )],
        vec![],
        vec![mon(
            Species::Snorlax,
            &[PokemonMove::Splash, PokemonMove::Rest],
        )],
        vec![],
    );
    battle.p2_active_mons[0].hp = 1;

    let result = solve(
        &MatchState::BattleState(battle),
        pokemon_dex,
        move_dex,
        &base_config(),
    )
    .expect("position is solvable");

    assert!(
        result.p1_win_odds > 0.99,
        "expected a near-certain win, got {}",
        result.p1_win_odds
    );
    // And P1 should commit to the kill rather than mixing.
    let best = result.most_likely_action(Player::P1).expect("an action");
    assert!(best.probability > 0.99, "P1 hedged a forced win: {best:?}");
    assert!(
        matches!(best.commands[0], BattleCommand::Attack(ref a) if a.move_slot == 0),
        "P1 did not pick the killing move: {:?}",
        best.commands
    );
}

/// The mirror image: the same position from P2's side of the board must produce
/// the complementary odds. This catches any perspective bug in the evaluator or
/// in the min/max wiring, which would otherwise be invisible.
#[test]
fn mirrored_positions_give_complementary_odds() {
    let (pokemon_dex, move_dex) = dexes();

    let p1_favoured = MatchState::BattleState(battle_state_from_lists(
        vec![mon(
            Species::Machamp,
            &[PokemonMove::CloseCombat, PokemonMove::Splash],
        )],
        vec![],
        vec![mon(
            Species::Abra,
            &[PokemonMove::Splash, PokemonMove::Teleport],
        )],
        vec![],
    ));
    let p2_favoured = MatchState::BattleState(battle_state_from_lists(
        vec![mon(
            Species::Abra,
            &[PokemonMove::Splash, PokemonMove::Teleport],
        )],
        vec![],
        vec![mon(
            Species::Machamp,
            &[PokemonMove::CloseCombat, PokemonMove::Splash],
        )],
        vec![],
    ));

    let config = base_config();
    let a = solve(&p1_favoured, pokemon_dex, move_dex, &config).unwrap();
    let b = solve(&p2_favoured, pokemon_dex, move_dex, &config).unwrap();

    assert!(
        (a.p1_win_odds + b.p1_win_odds - 1.0).abs() < 1e-6,
        "{} and {} are not complementary",
        a.p1_win_odds,
        b.p1_win_odds
    );
}

/// Deeper search must not change what the search is *about*: the value stays a
/// probability and the strategies stay distributions.
#[test]
fn depth_does_not_break_the_invariants() {
    let (pokemon_dex, move_dex) = dexes();
    let state = contested_position();
    for depth in 1..=3 {
        let config = SolveConfig {
            depth,
            chance: ChanceMode::TopK(2),
            ..base_config()
        };
        let result = solve(&state, pokemon_dex, move_dex, &config).expect("solvable");
        assert_valid_strategies(&result);
    }
}

/// `TopK(usize::MAX)` cannot drop anything, so it must be indistinguishable from
/// exact enumeration — including in the absence of a discarded-mass warning.
#[test]
fn unbounded_top_k_matches_enumeration() {
    let (pokemon_dex, move_dex) = dexes();
    let state = contested_position();

    let exact = solve(
        &state,
        pokemon_dex,
        move_dex,
        &SolveConfig {
            chance: ChanceMode::Enumerate,
            ..base_config()
        },
    )
    .unwrap();
    let unbounded = solve(
        &state,
        pokemon_dex,
        move_dex,
        &SolveConfig {
            chance: ChanceMode::TopK(usize::MAX),
            ..base_config()
        },
    )
    .unwrap();

    assert!((exact.value - unbounded.value).abs() < 1e-12);
    assert!(unbounded.warnings.is_empty(), "{:?}", unbounded.warnings);
}

/// Truncating the outcome distribution has to be announced, not silently
/// applied — the whole point of the warning is that the caller can see what the
/// speed was bought with.
#[test]
fn discarding_outcome_mass_is_reported() {
    let (pokemon_dex, move_dex) = dexes();
    let state = contested_position();

    let result = solve(
        &state,
        pokemon_dex,
        move_dex,
        &SolveConfig {
            damage_rolls: 16,
            consider_crit: true,
            chance: ChanceMode::TopK(1),
            ..base_config()
        },
    )
    .unwrap();

    assert!(
        result
            .warnings
            .iter()
            .any(|w| matches!(w, SolveWarning::ChanceMassDiscarded { .. })),
        "truncation went unreported: {:?}",
        result.warnings
    );
}

/// A budget that cannot possibly cover the search must degrade gracefully — a
/// usable answer plus a warning — rather than panicking or hanging.
#[test]
fn an_exhausted_node_budget_warns_instead_of_failing() {
    let (pokemon_dex, move_dex) = dexes();
    let state = contested_position();

    let result = solve(
        &state,
        pokemon_dex,
        move_dex,
        &SolveConfig {
            depth: 3,
            node_budget: Some(2),
            ..base_config()
        },
    )
    .expect("a budgeted solve still returns an answer");

    assert_valid_strategies(&result);
    assert!(
        result
            .warnings
            .iter()
            .any(|w| matches!(w, SolveWarning::BudgetExhausted { .. })),
        "budget exhaustion went unreported: {:?}",
        result.warnings
    );
}

/// The transposition table is a memo, not a heuristic: turning it on must not
/// move the answer.
#[test]
fn the_transposition_table_does_not_change_the_value() {
    let (pokemon_dex, move_dex) = dexes();
    let state = contested_position();

    let cached = solve(
        &state,
        pokemon_dex,
        move_dex,
        &SolveConfig {
            depth: 2,
            ..base_config()
        },
    )
    .unwrap();
    let uncached = solve(
        &state,
        pokemon_dex,
        move_dex,
        &SolveConfig {
            depth: 2,
            tt_capacity: 0,
            ..base_config()
        },
    )
    .unwrap();

    assert!((cached.value - uncached.value).abs() < 1e-9);
}

/// Likewise the turn cache, which only exists to stop the serialized search from
/// re-resolving turns the main search already resolved.
#[test]
fn the_turn_cache_does_not_change_the_value() {
    let (pokemon_dex, move_dex) = dexes();
    let state = contested_position();

    let config = SolveConfig {
        algorithm: SolverAlgorithm::SerializedBounds,
        ..base_config()
    };
    let uncached = solve(&state, pokemon_dex, move_dex, &config).unwrap();
    let cached = solve(
        &state,
        pokemon_dex,
        move_dex,
        &SolveConfig {
            turn_cache_capacity: 4096,
            ..config
        },
    )
    .unwrap();

    assert!((cached.value - uncached.value).abs() < 1e-9);
    assert!(
        cached.stats.turn_cache_hits > 0,
        "the cache never hit, so it proved nothing"
    );
}

/// A constant evaluator makes every leaf identical, so any position with no
/// forced result inside the horizon must come back exactly even. This isolates
/// the search's arithmetic from the heuristic's judgement — a value that drifts
/// off 0.5 here is a bug in the expectation or the LP, not in the weights.
#[test]
fn a_constant_evaluator_yields_an_even_value() {
    let (pokemon_dex, move_dex) = dexes();
    let state = MatchState::BattleState(battle_state_from_lists(
        vec![mon(
            Species::Abra,
            &[PokemonMove::Splash, PokemonMove::Teleport],
        )],
        vec![],
        vec![mon(
            Species::Abra,
            &[PokemonMove::Splash, PokemonMove::Teleport],
        )],
        vec![],
    ));

    let result = solve(
        &state,
        pokemon_dex,
        move_dex,
        &SolveConfig {
            eval: eval::even,
            ..base_config()
        },
    )
    .unwrap();

    assert!(
        (result.value - 0.5).abs() < 1e-9,
        "expected exactly even, got {}",
        result.value
    );
}

/// Within one process, every mode except `Sample` is deterministic — not just in
/// the value, but in the work done to reach it.
///
/// This is less automatic than it looks. `simulate_turn` drains a `HashMap`
/// before sorting, so successors that tie on probability come out in a
/// run-varying order; floating-point addition is not associative, so summing a
/// cell in a different order can differ in the last bit, which is enough to flip
/// a best-response argmax and change how much of the tree gets explored. The
/// search sorts with a state-hash tiebreak to close that, and this pins it.
///
/// Note the scope. *Across* processes the engine coalesces at every intermediate
/// expansion level as well, so successor probabilities can land a few ulps apart
/// and work counts move by around a percent — see `benches/solver_speed.rs`.
/// That is a property of the transition function, not of this search, and this
/// test deliberately does not claim otherwise.
#[test]
fn repeated_solves_are_identical() {
    let (pokemon_dex, move_dex) = dexes();
    let state = contested_position();

    for chance in [ChanceMode::Enumerate, ChanceMode::TopK(2)] {
        let config = SolveConfig {
            depth: 2,
            damage_rolls: 4,
            chance,
            ..base_config()
        };
        let first = solve(&state, pokemon_dex, move_dex, &config).unwrap();
        let second = solve(&state, pokemon_dex, move_dex, &config).unwrap();

        assert_eq!(first.value, second.value, "{chance:?}: value drifted");
        assert_eq!(
            first.stats.turns_simulated, second.stats.turns_simulated,
            "{chance:?}: turn count drifted"
        );
        assert_eq!(
            first.stats.matrix_cells_evaluated, second.stats.matrix_cells_evaluated,
            "{chance:?}: evaluated-cell count drifted"
        );
    }
}

/// Sampling is the one mode with an RNG in it, so it is the one mode that owes a
/// reproducibility guarantee.
#[test]
fn seeded_sampling_is_reproducible() {
    let (pokemon_dex, move_dex) = dexes();
    let state = contested_position();
    let config = SolveConfig {
        damage_rolls: 8,
        chance: ChanceMode::Sample(2),
        ..base_config()
    };

    let first = solve_seeded(99, &state, pokemon_dex, move_dex, &config).unwrap();
    let second = solve_seeded(99, &state, pokemon_dex, move_dex, &config).unwrap();
    assert_eq!(first.value, second.value);
}

/// A replacement is a decision point but not a new turn. Charging it a ply would
/// make a deep search silently shallow whenever a Pokemon fainted, so the search
/// has to handle the phase without either consuming depth or looping.
#[test]
fn a_pending_replacement_is_searched_without_consuming_depth() {
    let (pokemon_dex, move_dex) = dexes();
    let state = pending_replacement_position();

    let result = solve(&state, pokemon_dex, move_dex, &base_config())
        .expect("a replacement position is solvable");

    assert_valid_strategies(&result);
    // P1's only decision is which Pokemon to bring in, so every action it
    // considers must be a switch.
    for action in &result.p1_strategy {
        assert!(
            matches!(action.commands[0], BattleCommand::Switch(_)),
            "expected a switch, got {:?}",
            action.commands
        );
    }
}

fn pending_replacement_position() -> MatchState {
    let mut battle = battle_state_from_lists(
        vec![mon(
            Species::Pikachu,
            &[PokemonMove::Thunderbolt, PokemonMove::QuickAttack],
        )],
        vec![
            mon(Species::Gyarados, &[PokemonMove::Waterfall]),
            mon(Species::Gengar, &[PokemonMove::ShadowBall]),
        ],
        vec![mon(
            Species::Snorlax,
            &[PokemonMove::BodySlam, PokemonMove::Crunch],
        )],
        vec![],
    );
    battle.p1_active_mons[0].hp = 0;
    battle.p1_active_mons[0].fainted = true;
    battle.turn_started = true;
    battle.turn_ended = true;
    MatchState::BattleState(battle)
}

#[test]
fn doubles_positions_are_solvable() {
    let (pokemon_dex, move_dex) = dexes();
    let state = MatchState::BattleState(battle_state_from_lists(
        vec![
            mon(Species::Pikachu, &[PokemonMove::Thunderbolt]),
            mon(Species::Gyarados, &[PokemonMove::Waterfall]),
        ],
        vec![],
        vec![
            mon(Species::Snorlax, &[PokemonMove::BodySlam]),
            mon(Species::Gengar, &[PokemonMove::ShadowBall]),
        ],
        vec![],
    ));

    let result = solve(&state, pokemon_dex, move_dex, &base_config()).expect("solvable");
    assert_valid_strategies(&result);
    // Both slots choose together: a joint action names a command per slot.
    assert!(
        result.p1_strategy.iter().all(|a| a.commands.len() == 2),
        "a doubles joint action must cover both slots"
    );
}

/// Capping the action set is an approximation, so it has to be announced.
#[test]
fn capping_the_action_set_is_reported() {
    let (pokemon_dex, move_dex) = dexes();
    let state = contested_position();

    let result = solve(
        &state,
        pokemon_dex,
        move_dex,
        &SolveConfig {
            max_actions_per_player: Some(2),
            ..base_config()
        },
    )
    .unwrap();

    assert!(
        result
            .warnings
            .iter()
            .any(|w| matches!(w, SolveWarning::ActionsTruncated { .. })),
        "capping went unreported: {:?}",
        result.warnings
    );
    assert!(result.p1_strategy.len() <= 2);
}

/// The dominance pre-filter is an approximation too, so it has to be announced.
/// Strength beats Tackle on damage and ties on accuracy, and neither move
/// carries another effect.
#[test]
fn pruning_a_dominated_action_is_reported() {
    let (pokemon_dex, move_dex) = dexes();
    let state = MatchState::BattleState(battle_state_from_lists(
        vec![mon(
            Species::Pikachu,
            &[PokemonMove::Tackle, PokemonMove::Strength],
        )],
        vec![],
        vec![mon(Species::Snorlax, &[PokemonMove::Splash])],
        vec![],
    ));

    let plain = solve(&state, pokemon_dex, move_dex, &base_config()).unwrap();
    assert!(
        !plain
            .warnings
            .iter()
            .any(|w| matches!(w, SolveWarning::ActionsTruncated { .. })),
        "the default flag truncated the set: {:?}",
        plain.warnings
    );
    let result = solve(
        &state,
        pokemon_dex,
        move_dex,
        &SolveConfig {
            prune_dominated_actions: true,
            ..base_config()
        },
    )
    .unwrap();

    assert!(
        result.warnings.iter().any(|warning| matches!(
            warning,
            SolveWarning::ActionsTruncated {
                player: Player::P1,
                kept,
                total,
            } if kept < total
        )),
        "pruning went unreported: {:?}",
        result.warnings
    );
    // The equilibrium never plays a dominated action, so the pruned solve must
    // reach the same value as the exact solve.
    assert!(
        (result.value - plain.value).abs() < 1e-9,
        "pruning moved the value from {} to {}",
        plain.value,
        result.value
    );
}

/// The root has only one move per player, but its guaranteed KO reaches a
/// replacement node with two bench choices. Truncation at that child used to be
/// silently omitted because warning collection inspected only the root sets.
#[test]
fn capping_a_child_action_set_is_reported() {
    let (pokemon_dex, move_dex) = dexes();
    let mut battle = battle_state_from_lists(
        vec![mon(Species::Pikachu, &[PokemonMove::Thunderbolt])],
        vec![],
        vec![mon(Species::Gyarados, &[PokemonMove::Splash])],
        vec![
            mon(Species::Snorlax, &[PokemonMove::BodySlam]),
            mon(Species::Gengar, &[PokemonMove::ShadowBall]),
        ],
    );
    battle.p2_active_mons[0].hp = 1;

    let result = solve(
        &MatchState::BattleState(battle),
        pokemon_dex,
        move_dex,
        &SolveConfig {
            max_actions_per_player: Some(1),
            ..base_config()
        },
    )
    .unwrap();

    assert!(
        result.warnings.iter().any(|warning| matches!(
            warning,
            SolveWarning::ActionsTruncated {
                kept: 1,
                total,
                ..
            } if *total > 1
        )),
        "child-node capping went unreported: {:?}",
        result.warnings
    );
}

/// Iterative deepening is a schedule, not a different search: the pass that
/// finishes at the requested depth has to return exactly what a single search at
/// that depth returns.
///
/// The seeded restricted game is what makes this worth pinning. A deepening pass
/// opens double oracle on the previous pass's root support rather than on action
/// 0, so it grows the restricted game in a different order and converges in a
/// different number of rounds. Double oracle terminates on a best response over
/// the full action set, so the order must not move the value — and the two
/// algorithms that ignore the seed are here to show the comparison itself is
/// sound.
#[test]
fn iterative_deepening_matches_every_algorithm() {
    // A real matrix, but only two passes: a depth-3 search of a contested
    // position costs about a minute per algorithm in a debug build.
    assert_deepening_matches_direct_search(&contested_position(), 2);

    // Three passes, so the seed is carried twice. Neither side can damage the
    // other, which keeps the branching low enough for the extra ply to be cheap.
    assert_deepening_matches_direct_search(&quiet_position(), 3);
}

/// Neither side can cause damage, and neither side has a bench.
/// A depth-1 pass expands only the root under the node budget.
/// Its successors return a static value before they check the node budget.
fn quiet_position() -> MatchState {
    MatchState::BattleState(battle_state_from_lists(
        vec![mon(
            Species::Abra,
            &[PokemonMove::Splash, PokemonMove::Teleport],
        )],
        vec![],
        vec![mon(
            Species::Abra,
            &[PokemonMove::Splash, PokemonMove::Teleport],
        )],
        vec![],
    ))
}

/// Deepen to `depth`, search straight to `depth`, and require the same answer
/// from every algorithm.
fn assert_deepening_matches_direct_search(state: &MatchState, depth: u8) {
    let (pokemon_dex, move_dex) = dexes();

    for algorithm in [
        SolverAlgorithm::BackwardInduction,
        SolverAlgorithm::SerializedBounds,
        SolverAlgorithm::DoubleOracle,
    ] {
        let direct = SolveConfig {
            algorithm,
            depth,
            ..base_config()
        };
        let deepened = SolveConfig {
            iterative_deepening: true,
            ..direct
        };

        let expected = solve(state, pokemon_dex, move_dex, &direct).expect("solvable");
        let result = solve(state, pokemon_dex, move_dex, &deepened).expect("solvable");

        assert_valid_strategies(&result);
        assert_eq!(
            result.depth_reached, depth,
            "{algorithm:?} stopped short of depth {depth} with an ample budget"
        );
        assert!(
            (result.value - expected.value).abs() < 1e-6,
            "{algorithm:?} deepened to {}, searched directly to {}",
            result.value,
            expected.value
        );
        assert!(
            !result
                .warnings
                .iter()
                .any(|w| matches!(w, SolveWarning::DepthNotReached { .. })),
            "{algorithm:?} reached the target but still warned: {:?}",
            result.warnings
        );
    }
}

/// The point of deepening: a budget that runs out part way through must fall
/// back to the last depth that finished, not hand back the half-searched one.
///
/// The budget is taken from a depth-1 solve, so it is exactly enough for the
/// first pass and nothing more. Note which warnings that implies. The returned
/// answer is a complete depth-1 search, so `BudgetExhausted` — which says the
/// answer itself is part static — must not appear. `DepthNotReached` must,
/// because the caller asked for depth 2.
#[test]
fn iterative_deepening_keeps_the_last_complete_depth() {
    let (pokemon_dex, move_dex) = dexes();
    let state = contested_position();

    let shallow = solve(
        &state,
        pokemon_dex,
        move_dex,
        &SolveConfig {
            depth: 1,
            ..base_config()
        },
    )
    .expect("solvable");

    let result = solve(
        &state,
        pokemon_dex,
        move_dex,
        &SolveConfig {
            depth: 2,
            iterative_deepening: true,
            node_budget: Some(shallow.stats.nodes_expanded),
            ..base_config()
        },
    )
    .expect("a budgeted solve still returns an answer");

    assert_valid_strategies(&result);
    assert_eq!(result.depth_reached, 1);
    assert!(
        (result.value - shallow.value).abs() < 1e-9,
        "expected the depth-1 value {}, got {}",
        shallow.value,
        result.value
    );
    assert_eq!(
        result.stats.nodes_expanded, shallow.stats.nodes_expanded,
        "the solver started another pass after it spent the node budget"
    );
    assert!(
        result.warnings.contains(&SolveWarning::DepthNotReached {
            target: 2,
            reached: 1
        }),
        "the abandoned depth went unreported: {:?}",
        result.warnings
    );
    assert!(
        !result
            .warnings
            .iter()
            .any(|w| matches!(w, SolveWarning::BudgetExhausted { .. })),
        "a complete pass was returned, so nothing in it was static: {:?}",
        result.warnings
    );
}

/// Warnings must describe the returned pass.
///
/// The first pass has one legal move because Encore is active. Encore then
/// ends. The incomplete second pass sees and caps the larger action set.
#[test]
fn iterative_deepening_discards_warnings_from_an_incomplete_pass() {
    let (pokemon_dex, move_dex) = dexes();
    let mut p1 = mon(
        Species::Pikachu,
        &[
            PokemonMove::Thunder,
            PokemonMove::QuickAttack,
            PokemonMove::Protect,
            PokemonMove::Splash,
        ],
    );
    p1.volatiles.push(VolatileStatusState::MoveStatus(
        VolatileStatus::Encore(PokemonMove::Thunder),
        1,
    ));
    let mut battle = battle_state_from_lists(
        vec![p1],
        vec![],
        vec![mon(Species::Snorlax, &[PokemonMove::Splash])],
        vec![],
    );
    battle.p1_has_tera = false;
    battle.p2_has_tera = false;

    let result = solve(
        &MatchState::BattleState(battle),
        pokemon_dex,
        move_dex,
        &SolveConfig {
            depth: 2,
            iterative_deepening: true,
            max_actions_per_player: Some(2),
            node_budget: Some(3),
            ..base_config()
        },
    )
    .expect("a budgeted solve still returns an answer");

    assert_eq!(result.depth_reached, 1);
    assert!(
        !result
            .warnings
            .iter()
            .any(|warning| matches!(warning, SolveWarning::ActionsTruncated { .. })),
        "the returned pass did not cap actions: {:?}",
        result.warnings
    );
}

/// Resolved transitions are the expensive thing the passes share. Every root
/// cell a pass evaluates was already resolved by the pass before it, and the
/// turn cache has no depth in its key, so the deeper pass must read them back
/// instead of calling `simulate_turn` again.
#[test]
fn iterative_deepening_reuses_resolved_transitions() {
    let (pokemon_dex, move_dex) = dexes();
    let state = contested_position();

    let result = solve(
        &state,
        pokemon_dex,
        move_dex,
        &SolveConfig {
            depth: 2,
            iterative_deepening: true,
            turn_cache_capacity: 4096,
            ..base_config()
        },
    )
    .expect("solvable");

    assert_eq!(result.depth_reached, 2);
    assert!(
        result.stats.turn_cache_hits > 0,
        "the passes shared nothing, so the deepening was pure overhead"
    );
}

/// When even depth 1 does not finish there is no complete pass to fall back on,
/// so the partial one is returned and `BudgetExhausted` still has to say so.
///
/// Reaching that case takes a little care. A depth-1 pass normally scores its
/// successors at depth 0, which returns before the budget is ever consulted, so
/// the pass finishes whatever the budget is. Here P2's Gyarados is on 1 HP and
/// Thunderbolt is four times effective, so the first cell reaches a replacement
/// node — a decision that does not consume a ply and so does check the budget.
#[test]
fn a_partial_first_pass_still_warns_about_the_budget() {
    let (pokemon_dex, move_dex) = dexes();

    let result = solve(
        &partial_first_pass_position(),
        pokemon_dex,
        move_dex,
        &SolveConfig {
            depth: 2,
            iterative_deepening: true,
            node_budget: Some(1),
            ..base_config()
        },
    )
    .expect("a budgeted solve still returns an answer");

    assert_valid_strategies(&result);
    assert_eq!(result.depth_reached, 1);
    assert!(
        result
            .warnings
            .iter()
            .any(|w| matches!(w, SolveWarning::BudgetExhausted { .. })),
        "the returned pass was partial and did not say so: {:?}",
        result.warnings
    );
}

/// A position whose depth-1 pass cannot finish under a spent node budget. P2's
/// Gyarados is on 1 HP and Thunderbolt is four times effective, so the first
/// cell reaches a replacement node — a decision that does not consume a ply and
/// checks the node budget.
fn partial_first_pass_position() -> MatchState {
    let mut battle = battle_state_from_lists(
        vec![mon(Species::Pikachu, &[PokemonMove::Thunderbolt])],
        vec![],
        vec![mon(Species::Gyarados, &[PokemonMove::Splash])],
        vec![
            mon(Species::Snorlax, &[PokemonMove::BodySlam]),
            mon(Species::Gengar, &[PokemonMove::ShadowBall]),
        ],
    );
    battle.p2_active_mons[0].hp = 1;
    MatchState::BattleState(battle)
}

// ── Deadlines ───────────────────────────────────────────────────────────────

/// The exact mode is the regression oracle. A deadline the search cannot
/// possibly reach must return the same answer as no deadline at all, and must
/// warn about nothing.
#[test]
fn a_generous_deadline_does_not_change_the_value() {
    let (pokemon_dex, move_dex) = dexes();
    let state = contested_position();

    let exact = solve(&state, pokemon_dex, move_dex, &base_config()).expect("solvable");
    let result = solve(
        &state,
        pokemon_dex,
        move_dex,
        &SolveConfig {
            deadline: Some(Duration::from_secs(3600)),
            ..base_config()
        },
    )
    .expect("solvable");

    assert_valid_strategies(&result);
    assert!(
        (result.value - exact.value).abs() < 1e-9,
        "the exact solve returned {}, the deadlined solve {}",
        exact.value,
        result.value
    );
    assert!(
        !result
            .warnings
            .iter()
            .any(|w| matches!(w, SolveWarning::DeadlineExceeded { .. })),
        "a deadline that never expired was reported: {:?}",
        result.warnings
    );
}

/// A zero deadline stops all algorithms before they resolve a root action pair.
/// The solver still returns a valid partial strategy.
/// The zero duration makes the result independent of machine speed.
#[test]
fn an_expired_deadline_stops_root_turn_simulation() {
    let (pokemon_dex, move_dex) = dexes();
    let state = quiet_position();

    for algorithm in [
        SolverAlgorithm::BackwardInduction,
        SolverAlgorithm::SerializedBounds,
        SolverAlgorithm::DoubleOracle,
    ] {
        let result = solve(
            &state,
            pokemon_dex,
            move_dex,
            &SolveConfig {
                depth: 2,
                iterative_deepening: true,
                algorithm,
                deadline: Some(Duration::ZERO),
                ..base_config()
            },
        )
        .expect("a solve with a deadline still returns an answer");

        assert_valid_strategies(&result);
        assert_eq!(result.depth_reached, 1);
        assert_eq!(
            result.stats.turns_simulated, 0,
            "{algorithm:?} resolved a turn after the deadline"
        );
        assert!(
            result.warnings.contains(&SolveWarning::DeadlineExceeded {
                budget: Duration::ZERO
            }),
            "{algorithm:?} did not report the partial pass: {:?}",
            result.warnings
        );
        assert!(
            result.warnings.contains(&SolveWarning::DepthNotReached {
                target: 2,
                reached: 1
            }),
            "{algorithm:?} did not report the missed depth: {:?}",
            result.warnings
        );
    }
}

/// When even depth 1 does not finish, the partial pass is the only answer there
/// is, and `DeadlineExceeded` has to say so.
#[test]
fn a_partial_first_pass_still_warns_about_the_deadline() {
    let (pokemon_dex, move_dex) = dexes();

    let result = solve(
        &partial_first_pass_position(),
        pokemon_dex,
        move_dex,
        &SolveConfig {
            depth: 2,
            iterative_deepening: true,
            deadline: Some(Duration::ZERO),
            ..base_config()
        },
    )
    .expect("a deadlined solve still returns an answer");

    assert_valid_strategies(&result);
    assert_eq!(result.depth_reached, 1);
    assert!(
        result.warnings.contains(&SolveWarning::DeadlineExceeded {
            budget: Duration::ZERO
        }),
        "the returned pass was partial and did not say so: {:?}",
        result.warnings
    );
    assert!(
        !result
            .warnings
            .iter()
            .any(|w| matches!(w, SolveWarning::BudgetExhausted { .. })),
        "the node budget was ample, so it must not be blamed: {:?}",
        result.warnings
    );
}

#[test]
fn a_finished_battle_cannot_be_solved() {
    let (pokemon_dex, move_dex) = dexes();
    let final_state = battle_state_from_lists(
        vec![mon(Species::Pikachu, &[PokemonMove::Thunderbolt])],
        vec![],
        vec![mon(Species::Snorlax, &[PokemonMove::BodySlam])],
        vec![],
    );
    let over = MatchState::GameOverState {
        winner: Player::P1,
        pending_events: Vec::new(),
        final_state: Box::new(final_state),
    };

    assert!(matches!(
        solve(&over, pokemon_dex, move_dex, &base_config()),
        Err(SolveError::GameAlreadyOver { winner: Player::P1 })
    ));
}

// ── Team preview ────────────────────────────────────────────────────────────

/// A preview state whose mon ids are unique across both teams, as the engine
/// requires for trapping-volatile source tracking.
fn preview_state(
    mut p1_mons: Vec<PokemonState>,
    mut p2_mons: Vec<PokemonState>,
    active_per_side: u8,
    brought_per_side: u8,
) -> TeamPreviewState {
    let p1_count = p1_mons.len() as u8;
    for (index, mon) in p1_mons.iter_mut().enumerate() {
        mon.mon_id = index as u8;
    }
    for (index, mon) in p2_mons.iter_mut().enumerate() {
        mon.mon_id = p1_count + index as u8;
    }
    TeamPreviewState {
        active_per_side,
        brought_per_side,
        mechanics: BattleMechanics {
            tera_enabled: false,
            mega_enabled: false,
        },
        p1_mons,
        p2_mons,
    }
}

/// Two Pokemon a side, bring one, lead one: two choices a side and four cells.
/// Small enough to solve every cell by hand in a debug build.
fn small_preview() -> TeamPreviewState {
    preview_state(
        vec![
            mon(Species::Pikachu, &[PokemonMove::Thunderbolt]),
            mon(Species::Gengar, &[PokemonMove::ShadowBall]),
        ],
        vec![
            mon(Species::Gyarados, &[PokemonMove::Waterfall]),
            mon(Species::Snorlax, &[PokemonMove::BodySlam]),
        ],
        1,
        1,
    )
}

/// One damage roll, no crit branching, one ply, and no deadline.
fn preview_config() -> PreviewConfig {
    PreviewConfig {
        battle: base_config(),
        deadline: None,
    }
}

/// The official format: six Pokemon, bring four, lead two. 15 bring sets times
/// 12 ordered lead pairs is 180 choices, and no two of them may be the same.
#[test]
fn preview_choices_count_official_doubles() {
    let team = || {
        vec![
            mon(Species::Pikachu, &[PokemonMove::Thunderbolt]),
            mon(Species::Gengar, &[PokemonMove::ShadowBall]),
            mon(Species::Gyarados, &[PokemonMove::Waterfall]),
            mon(Species::Snorlax, &[PokemonMove::BodySlam]),
            mon(Species::Machamp, &[PokemonMove::CrossChop]),
            mon(Species::Abra, &[PokemonMove::Psychic]),
        ]
    };
    let state = preview_state(team(), team(), 2, 4);

    for player in [Player::P1, Player::P2] {
        let choices = preview_choices(&state, player);
        assert_eq!(choices.len(), 180, "{player:?} choice count");

        let distinct: HashSet<(Vec<usize>, Vec<usize>)> = choices
            .iter()
            .map(|choice| (choice.active_indices.clone(), choice.back_indices.clone()))
            .collect();
        assert_eq!(distinct.len(), 180, "{player:?} repeated a choice");

        for choice in &choices {
            assert_eq!(choice.active_indices.len(), 2);
            assert_eq!(choice.back_indices.len(), 2);
            let mut brought = choice.active_indices.clone();
            brought.extend(&choice.back_indices);
            brought.sort_unstable();
            brought.dedup();
            assert_eq!(brought.len(), 4, "a choice brought the same Pokemon twice");
            assert!(choice.back_indices.windows(2).all(|pair| pair[0] < pair[1]));
        }
    }
}

/// The small case, where the count is easy to check by hand: three bring sets
/// times two lead orders.
#[test]
fn preview_choices_count_small_singles() {
    let team = || {
        vec![
            mon(Species::Pikachu, &[PokemonMove::Thunderbolt]),
            mon(Species::Gengar, &[PokemonMove::ShadowBall]),
            mon(Species::Snorlax, &[PokemonMove::BodySlam]),
        ]
    };
    let state = preview_state(team(), team(), 1, 2);
    let choices = preview_choices(&state, Player::P1);

    assert_eq!(choices.len(), 6);
    for choice in &choices {
        assert_eq!(choice.active_indices.len(), 1);
        assert_eq!(choice.back_indices.len(), 1);
    }
}

/// The soundness test for the preview solve. Double oracle reads only some of
/// the cells. Building every cell by hand and solving that matrix must give the
/// same value.
#[test]
fn preview_double_oracle_matches_full_matrix() {
    let (pokemon_dex, move_dex) = dexes();
    let state = small_preview();
    let config = preview_config();

    let p1_choices = preview_choices(&state, Player::P1);
    let p2_choices = preview_choices(&state, Player::P2);
    let payoffs: Vec<Vec<f64>> = p1_choices
        .iter()
        .map(|p1_choice| {
            p2_choices
                .iter()
                .map(|p2_choice| {
                    preview_cell_value(&state, pokemon_dex, move_dex, &config, p1_choice, p2_choice)
                })
                .collect()
        })
        .collect();
    let reference = solve_matrix_game(&payoffs);

    let result = solve_team_preview(&state, pokemon_dex, move_dex, &config)
        .expect("the preview state is well formed");

    assert!(
        (result.value - reference.value).abs() < 1e-6,
        "double oracle returned {}, the full matrix returned {}",
        result.value,
        reference.value
    );
    assert_eq!(result.stats.cells_total, 4);
    assert!(result.stats.cells_evaluated > 0);
    assert!(result.warnings.is_empty(), "{:?}", result.warnings);

    for (label, strategy) in [("P1", &result.p1_strategy), ("P2", &result.p2_strategy)] {
        let total: f64 = strategy.iter().map(|choice| choice.probability).sum();
        assert!(
            (total - 1.0).abs() < 1e-6,
            "{label} strategy sums to {total}"
        );
        assert!(!strategy.is_empty(), "{label} strategy is empty");
    }
    assert!((result.p1_win_odds + result.p2_win_odds - 1.0).abs() < 1e-9);
}

/// Preview progress must end on the strategy that the completed solve returns.
#[test]
fn team_preview_publishes_completed_rounds() {
    let (pokemon_dex, move_dex) = dexes();
    let rounds = RefCell::new(Vec::new());
    let progress = |round| rounds.borrow_mut().push(round);
    let result = solve_team_preview_progress_cancellable(
        &small_preview(),
        pokemon_dex,
        move_dex,
        &preview_config(),
        Some(&progress),
        None,
    )
    .expect("the preview state is well formed");

    let rounds = rounds.into_inner();
    let last = rounds.last().expect("the preview reported no round");
    assert!((last.value - result.value).abs() < 1e-9);
    assert_eq!(last.p1_strategy.len(), result.p1_strategy.len());
    assert_eq!(last.p2_strategy.len(), result.p2_strategy.len());
    assert_eq!(last.stats.turns_simulated, result.stats.turns_simulated);
}

/// Preview deepening must finish the outer depth-1 game before it spends work
/// on depth 2. If the shared turn budget then stops depth 2, the result must
/// keep the complete depth-1 strategy.
#[test]
fn team_preview_budget_keeps_the_complete_lower_depth() {
    let (pokemon_dex, move_dex) = dexes();
    let state = small_preview();
    let shallow = solve_team_preview(&state, pokemon_dex, move_dex, &preview_config())
        .expect("the preview state is well formed");
    let budget = CancelFlag::with_simulation_turn_budget(shallow.stats.turns_simulated + 1);
    let rounds = RefCell::new(Vec::new());
    let progress = |round| rounds.borrow_mut().push(round);
    let config = PreviewConfig {
        battle: SolveConfig {
            depth: 2,
            iterative_deepening: true,
            ..base_config()
        },
        deadline: None,
    };

    let result = solve_team_preview_progress_cancellable(
        &state,
        pokemon_dex,
        move_dex,
        &config,
        Some(&progress),
        Some(&budget),
    )
    .expect("the preview state is well formed");

    assert_eq!(result.depth_reached, 1);
    assert!((result.value - shallow.value).abs() < 1e-12);
    assert_eq!(result.p1_strategy.len(), shallow.p1_strategy.len());
    assert_eq!(result.p2_strategy.len(), shallow.p2_strategy.len());
    for (actual, expected) in result.p1_strategy.iter().zip(&shallow.p1_strategy) {
        assert_eq!(actual.choice.active_indices, expected.choice.active_indices);
        assert_eq!(actual.choice.back_indices, expected.choice.back_indices);
        assert!((actual.probability - expected.probability).abs() < 1e-12);
    }
    for (actual, expected) in result.p2_strategy.iter().zip(&shallow.p2_strategy) {
        assert_eq!(actual.choice.active_indices, expected.choice.active_indices);
        assert_eq!(actual.choice.back_indices, expected.choice.back_indices);
        assert!((actual.probability - expected.probability).abs() < 1e-12);
    }
    assert!(
        result
            .warnings
            .contains(&SolveWarning::SimulationTurnBudgetExhausted {
                budget: shallow.stats.turns_simulated + 1
            }),
        "{:?}",
        result.warnings
    );
    assert!(
        result.warnings.contains(&SolveWarning::DepthNotReached {
            target: 2,
            reached: 1
        }),
        "{:?}",
        result.warnings
    );
    assert!(rounds.borrow().iter().all(|round| round.depth == 1));
}

/// A shared cache must remove the cell work of a repeated solve. Double oracle
/// is deterministic, so the second solve asks for exactly the cells the first
/// solve stored.
#[test]
fn preview_cache_removes_repeat_cell_work() {
    let (pokemon_dex, move_dex) = dexes();
    let state = small_preview();
    let config = preview_config();
    let mut cache = PreviewCellCache::new();

    let first = solve_team_preview_cached(&state, pokemon_dex, move_dex, &config, &mut cache)
        .expect("the preview state is well formed");
    assert!(first.stats.cells_evaluated > 0);
    assert_eq!(first.stats.cell_cache_hits, 0);
    assert_eq!(cache.len() as u64, first.stats.cells_evaluated);

    let second = solve_team_preview_cached(&state, pokemon_dex, move_dex, &config, &mut cache)
        .expect("the preview state is well formed");
    assert_eq!(
        second.stats.cells_evaluated, 0,
        "the second solve computed a cell that the cache already held"
    );
    assert!(second.stats.cell_cache_hits > 0);
    assert!((second.value - first.value).abs() < 1e-9);
}

/// A cache entry must not answer a solve that uses different move data.
#[test]
fn preview_cache_separates_move_dex_contents() {
    let (pokemon_dex, move_dex) = dexes();
    let state = preview_state(
        vec![mon(Species::Pikachu, &[PokemonMove::Thunderbolt])],
        vec![mon(Species::Gyarados, &[PokemonMove::Waterfall])],
        1,
        1,
    );
    let config = preview_config();
    let mut cache = PreviewCellCache::new();

    let original = solve_team_preview_cached(&state, pokemon_dex, move_dex, &config, &mut cache)
        .expect("the preview state is well formed");

    let mut changed_move_dex = parse_move_dex("../pokemon_info/showdownMoves.txt");
    changed_move_dex
        .get_mut(&PokemonMove::Thunderbolt)
        .expect("Thunderbolt exists")
        .base_power = 0;
    let changed =
        solve_team_preview_cached(&state, pokemon_dex, &changed_move_dex, &config, &mut cache)
            .expect("the preview state is well formed");
    let reference = solve_team_preview(&state, pokemon_dex, &changed_move_dex, &config)
        .expect("the preview state is well formed");

    assert!((original.value - reference.value).abs() > 1e-6);
    assert!((changed.value - reference.value).abs() < 1e-9);
    assert_eq!(changed.stats.cell_cache_hits, 0);
}

/// Random samples and time-limited battle solves must not enter the cache.
#[test]
fn preview_cache_rejects_unstable_cells() {
    let (pokemon_dex, move_dex) = dexes();
    let state = small_preview();

    for battle in [
        SolveConfig {
            chance: ChanceMode::Sample(1),
            ..base_config()
        },
        SolveConfig {
            deadline: Some(Duration::ZERO),
            ..base_config()
        },
    ] {
        let config = PreviewConfig {
            battle,
            deadline: None,
        };
        let mut cache = PreviewCellCache::new();
        solve_team_preview_cached(&state, pokemon_dex, move_dex, &config, &mut cache)
            .expect("the preview state is well formed");

        assert!(cache.is_empty(), "an unstable cell entered the cache");
    }
}

/// A cache hit must replay the warnings that belong to the cached value.
#[test]
fn preview_cache_preserves_battle_warnings() {
    let (pokemon_dex, move_dex) = dexes();
    let state = small_preview();
    let config = PreviewConfig {
        battle: SolveConfig {
            depth: 2,
            node_budget: Some(0),
            ..base_config()
        },
        deadline: None,
    };
    let mut cache = PreviewCellCache::new();

    let first = solve_team_preview_cached(&state, pokemon_dex, move_dex, &config, &mut cache)
        .expect("the preview state is well formed");
    let second = solve_team_preview_cached(&state, pokemon_dex, move_dex, &config, &mut cache)
        .expect("the preview state is well formed");

    let warning = SolveWarning::BudgetExhausted { budget: 0 };
    assert!(first.warnings.contains(&warning));
    assert!(second.warnings.contains(&warning));
    assert_eq!(second.stats.cells_evaluated, 0);
    assert!(second.stats.cell_cache_hits > 0);
}

/// `precompute_preview_cells` must fill exactly the requested cells, and a later
/// solve must read them.
#[test]
fn preview_precompute_fills_requested_cells() {
    let (pokemon_dex, move_dex) = dexes();
    let state = small_preview();
    let config = preview_config();
    let mut cache = PreviewCellCache::new();

    // The last pair is out of range and must be skipped rather than counted.
    let requested = [(0, 0), (1, 1), (9, 9)];
    let stats = precompute_preview_cells(
        &state,
        pokemon_dex,
        move_dex,
        &config,
        &requested,
        &mut cache,
    )
    .expect("the preview state is well formed");

    assert_eq!(stats.cells_evaluated, 2);
    assert_eq!(cache.len(), 2);
    assert_eq!(stats.cells_total, 4);

    // Double oracle opens on cell (0, 0), so the solve must read at least that
    // one from the cache.
    let solved = solve_team_preview_cached(&state, pokemon_dex, move_dex, &config, &mut cache)
        .expect("the preview state is well formed");
    assert!(solved.stats.cell_cache_hits > 0);
}

/// A spent deadline must score cells with the leaf evaluator and say so. It must
/// also leave the cache empty, because an evaluated cell is not an exact cell.
#[test]
fn preview_deadline_returns_a_warning() {
    let (pokemon_dex, move_dex) = dexes();
    let state = small_preview();
    let config = PreviewConfig {
        battle: base_config(),
        deadline: Some(Duration::ZERO),
    };
    let mut cache = PreviewCellCache::new();

    let result = solve_team_preview_cached(&state, pokemon_dex, move_dex, &config, &mut cache)
        .expect("the preview state is well formed");

    assert!(
        result.warnings.contains(&SolveWarning::DeadlineExceeded {
            budget: Duration::ZERO
        }),
        "the deadline expired and the result did not say so: {:?}",
        result.warnings
    );
    assert_eq!(result.stats.battles_solved, 0);
    assert!(cache.is_empty(), "an approximate cell entered the cache");
    assert!((0.0..=1.0).contains(&result.value));
}

/// Two identical teams give a symmetric game, whose value is exactly even. A
/// failure here means the engine favors one side in a mirror.
#[test]
fn preview_mirror_position_is_even() {
    let (pokemon_dex, move_dex) = dexes();
    let team = || {
        vec![
            mon(Species::Pikachu, &[PokemonMove::Thunderbolt]),
            mon(Species::Gengar, &[PokemonMove::ShadowBall]),
        ]
    };
    let state = preview_state(team(), team(), 1, 1);
    let config = PreviewConfig {
        battle: SolveConfig {
            eval: eval::even,
            ..base_config()
        },
        deadline: None,
    };

    let result = solve_team_preview(&state, pokemon_dex, move_dex, &config)
        .expect("the preview state is well formed");

    assert!(
        (result.value - 0.5).abs() < 1e-6,
        "the mirror value is {}",
        result.value
    );
}

// ── Open-list team preview ──────────────────────────────────────────────────

/// The usage cache is gitignored and regenerable, so it may not exist. Tests
/// that need it skip rather than fail. Mirrors `determinize_tests::meta_root`.
fn meta_root() -> Option<PathBuf> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../meta_scraper/data");
    root.is_dir().then_some(root)
}

static DOUBLES: OnceLock<Option<MetaDex>> = OnceLock::new();
static LEARNSETS: OnceLock<HashMap<Species, HashSet<PokemonMove>>> = OnceLock::new();

fn doubles_meta() -> Option<&'static MetaDex> {
    DOUBLES
        .get_or_init(|| meta_root().and_then(|r| MetaDex::load(&r, None, MetaFormat::Doubles).ok()))
        .as_ref()
}

fn learnset_dex() -> &'static HashMap<Species, HashSet<PokemonMove>> {
    LEARNSETS.get_or_init(|| {
        crate::state::dex_data::parse_learnset_dex("../pokemon_info/showdownLearnsets.txt")
    })
}

/// Skip the body unless the cache is present.
macro_rules! with_meta {
    ($meta:ident) => {
        let Some($meta) = doubles_meta() else { return };
    };
}

/// P1 observes, so P1's own team is copied and P2's stats are drawn.
fn open_list_determinize_config() -> DeterminizeConfig {
    DeterminizeConfig {
        inference: InferenceConfig {
            learnset_dex: learnset_dex().clone(),
            ..InferenceConfig::default()
        },
        observer: Player::P1,
        ..Default::default()
    }
}

/// An open-list belief over `p1` and `p2`.
fn open_list_belief(p1: Vec<PokemonState>, p2: Vec<PokemonState>) -> UnknownTeamPreviewState {
    // Bring one and lead one, so the choice count stays at the team size.
    let belief = UnknownMatchState::team_preview_open_sheet_from_perspective(
        Player::P1,
        &p1,
        &p2,
        pokemon_dex(),
        1,
        1,
        50,
        InformationMode::OpenTeamSheetNatures,
        true,
    );
    match belief {
        UnknownMatchState::TeamPreview(preview) => preview,
        _ => panic!("the constructor returns a team-preview belief"),
    }
}

/// One Pokemon on each side with one move on each Pokemon.
fn open_list_belief_1v1() -> UnknownTeamPreviewState {
    open_list_belief(
        vec![mon(Species::Pikachu, &[PokemonMove::Thunderbolt])],
        vec![mon(Species::Snorlax, &[PokemonMove::BodySlam])],
    )
}

/// Two choices per side, for open-list matrix and budget tests.
fn open_list_belief_2v2() -> UnknownTeamPreviewState {
    open_list_belief(
        vec![
            mon(Species::Pikachu, &[PokemonMove::Thunderbolt]),
            mon(Species::Gengar, &[PokemonMove::ShadowBall]),
        ],
        vec![
            mon(Species::Snorlax, &[PokemonMove::BodySlam]),
            mon(Species::Gyarados, &[PokemonMove::Waterfall]),
        ],
    )
}

fn open_list_config(worlds: usize) -> OpenListConfig {
    OpenListConfig {
        preview: preview_config(),
        worlds,
        seed: 20_260_801,
    }
}

/// The determinizer must supply what the sheet hides and nothing else. Different
/// seeds must give the opponent different stats, and the observer's own team must
/// be identical in every world.
#[test]
fn open_list_preview_draws_distinct_worlds() {
    with_meta!(meta);
    let (pokemon_dex, move_dex) = dexes();
    let belief = open_list_belief_1v1();
    let determinize = open_list_determinize_config();

    let worlds = open_list_worlds(
        &belief,
        meta,
        pokemon_dex,
        move_dex,
        &open_list_config(8),
        &determinize,
    )
    .expect("the belief is well formed");
    assert_eq!(worlds.len(), 8);

    let own_stats = worlds[0].state.p1_mons[0].stats;
    for (index, world) in worlds.iter().enumerate() {
        assert_eq!(
            world.state.p1_mons[0].stats, own_stats,
            "world {index} changed the observer's own team"
        );
        assert_eq!(world.state.p2_mons[0].species, Species::Snorlax);
        assert!(
            world.probability > 0.0,
            "world {index} has zero probability"
        );
    }

    let opponent_stats: HashSet<[u16; 6]> =
        worlds.iter().map(|w| w.state.p2_mons[0].stats).collect();
    assert!(
        opponent_stats.len() > 1,
        "every world drew the same opponent stats: {opponent_stats:?}"
    );
}

#[test]
fn open_list_preview_preserves_revealed_empty_move_slots() {
    with_meta!(meta);
    let (pokemon_dex, move_dex) = dexes();
    let belief = open_list_belief_1v1();

    let worlds = open_list_worlds(
        &belief,
        meta,
        pokemon_dex,
        move_dex,
        &open_list_config(3),
        &open_list_determinize_config(),
    )
    .expect("the belief is well formed");

    for world in worlds {
        assert_eq!(
            world.state.p2_mons[0].moves,
            [Some(PokemonMove::BodySlam), None, None, None]
        );
        assert_eq!(world.state.p2_mons[0].max_pp[1..], [0, 0, 0]);
    }
}

/// The soundness test. Build every cell of every world by hand, average them,
/// and solve that matrix. The open-list solve must return the same value.
#[test]
fn open_list_preview_matches_mean_matrix() {
    with_meta!(meta);
    let (pokemon_dex, move_dex) = dexes();
    let belief = open_list_belief(
        vec![
            mon(Species::Pikachu, &[PokemonMove::Thunderbolt]),
            mon(Species::Gengar, &[PokemonMove::ShadowBall]),
        ],
        vec![
            mon(Species::Snorlax, &[PokemonMove::BodySlam]),
            mon(Species::Gyarados, &[PokemonMove::Waterfall]),
        ],
    );
    let determinize = open_list_determinize_config();
    let config = open_list_config(2);

    let worlds = open_list_worlds(&belief, meta, pokemon_dex, move_dex, &config, &determinize)
        .expect("the belief is well formed");

    let p1_choices = preview_choices(&worlds[0].state, Player::P1);
    let p2_choices = preview_choices(&worlds[0].state, Player::P2);
    let mut mean_matrix = vec![vec![0.0; p2_choices.len()]; p1_choices.len()];
    for world in &worlds {
        for (row, p1_choice) in p1_choices.iter().enumerate() {
            for (col, p2_choice) in p2_choices.iter().enumerate() {
                mean_matrix[row][col] += preview_cell_value(
                    &world.state,
                    pokemon_dex,
                    move_dex,
                    &config.preview,
                    p1_choice,
                    p2_choice,
                ) / worlds.len() as f64;
            }
        }
    }
    let reference = solve_matrix_game(&mean_matrix);

    let result =
        solve_open_list_preview(&belief, meta, pokemon_dex, move_dex, &config, &determinize)
            .expect("the belief is well formed");

    assert!(
        (result.value - reference.value).abs() < 1e-6,
        "the open-list solve returned {}, the mean matrix returned {}",
        result.value,
        reference.value
    );
    assert_eq!(
        result.stats.cells_total,
        (p1_choices.len() * p2_choices.len()) as u64
    );
    assert!(result.warnings.is_empty(), "{:?}", result.warnings);
}

/// One world has no spread to measure, so the standard error must be absent
/// rather than zero.
#[test]
fn open_list_preview_one_world_has_no_sampling_error() {
    with_meta!(meta);
    let (pokemon_dex, move_dex) = dexes();
    let belief = open_list_belief_1v1();

    let result = solve_open_list_preview(
        &belief,
        meta,
        pokemon_dex,
        move_dex,
        &open_list_config(1),
        &open_list_determinize_config(),
    )
    .expect("the belief is well formed");

    assert_eq!(result.sampling.worlds, 1);
    assert_eq!(result.sampling.per_world_values.len(), 1);
    assert!(result.sampling.standard_error.is_none());
    assert!((result.sampling.mean - result.value).abs() < 1e-9);
}

/// Open-list preview progress must report the mean-matrix strategy.
#[test]
fn open_list_preview_publishes_completed_rounds() {
    with_meta!(meta);
    let (pokemon_dex, move_dex) = dexes();
    let belief = open_list_belief_1v1();
    let rounds = RefCell::new(Vec::new());
    let progress = |round| rounds.borrow_mut().push(round);
    let result = solve_open_list_preview_progress_cancellable(
        &belief,
        meta,
        pokemon_dex,
        move_dex,
        &open_list_config(2),
        &open_list_determinize_config(),
        Some(&progress),
        None,
    )
    .expect("the belief is well formed");

    let rounds = rounds.into_inner();
    let last = rounds.last().expect("the preview reported no round");
    assert!((last.value - result.value).abs() < 1e-9);
    assert_eq!(last.p1_strategy.len(), result.p1_strategy.len());
    assert_eq!(last.p2_strategy.len(), result.p2_strategy.len());
    assert_eq!(last.stats.turns_simulated, result.stats.turns_simulated);
}

/// Open-list preview must keep its completed lower-depth mean-matrix strategy
/// when the shared turn budget stops the next depth.
#[test]
fn open_list_preview_budget_keeps_the_complete_lower_depth() {
    with_meta!(meta);
    let (pokemon_dex, move_dex) = dexes();
    let belief = open_list_belief_2v2();
    let shallow_config = open_list_config(2);
    let determinize = open_list_determinize_config();
    let shallow = solve_open_list_preview(
        &belief,
        meta,
        pokemon_dex,
        move_dex,
        &shallow_config,
        &determinize,
    )
    .expect("the belief is well formed");
    let budget = CancelFlag::with_simulation_turn_budget(shallow.stats.turns_simulated + 1);
    let rounds = RefCell::new(Vec::new());
    let progress = |round| rounds.borrow_mut().push(round);
    let config = OpenListConfig {
        preview: PreviewConfig {
            battle: SolveConfig {
                depth: 2,
                iterative_deepening: true,
                ..base_config()
            },
            deadline: None,
        },
        ..shallow_config
    };

    let result = solve_open_list_preview_progress_cancellable(
        &belief,
        meta,
        pokemon_dex,
        move_dex,
        &config,
        &determinize,
        Some(&progress),
        Some(&budget),
    )
    .expect("the belief is well formed");

    assert_eq!(result.depth_reached, 1);
    assert!((result.value - shallow.value).abs() < 1e-12);
    assert_eq!(result.p1_strategy.len(), shallow.p1_strategy.len());
    assert_eq!(result.p2_strategy.len(), shallow.p2_strategy.len());
    assert!(
        result.warnings.contains(&SolveWarning::DepthNotReached {
            target: 2,
            reached: 1
        }),
        "{:?}",
        result.warnings
    );
    assert!(rounds.borrow().iter().all(|round| round.depth == 1));
}

/// Several worlds must report a finite, non-negative standard error. The mean of
/// the per-world values must also equal the returned value, because the solver
/// solves the mean of those same cells.
#[test]
fn open_list_preview_reports_sampling_error() {
    with_meta!(meta);
    let (pokemon_dex, move_dex) = dexes();
    let belief = open_list_belief_1v1();

    let result = solve_open_list_preview(
        &belief,
        meta,
        pokemon_dex,
        move_dex,
        &open_list_config(4),
        &open_list_determinize_config(),
    )
    .expect("the belief is well formed");

    assert_eq!(result.sampling.worlds, 4);
    assert_eq!(result.sampling.per_world_values.len(), 4);
    for value in &result.sampling.per_world_values {
        assert!((0.0..=1.0).contains(value), "a world value is {value}");
    }
    let error = result
        .sampling
        .standard_error
        .expect("four worlds report a standard error");
    assert!(error.is_finite() && error >= 0.0, "the error is {error}");
    assert!(
        (result.sampling.mean - result.value).abs() < 1e-6,
        "the mean is {}, the value is {}",
        result.sampling.mean,
        result.value
    );
    assert!((result.p1_win_odds + result.p2_win_odds - 1.0).abs() < 1e-9);
}

/// The answer must hold one strategy pair, not one pair per world. Each strategy
/// must also be a distribution over the choices of a single world.
#[test]
fn open_list_preview_returns_one_strategy() {
    with_meta!(meta);
    let (pokemon_dex, move_dex) = dexes();
    let belief = open_list_belief(
        vec![
            mon(Species::Pikachu, &[PokemonMove::Thunderbolt]),
            mon(Species::Gengar, &[PokemonMove::ShadowBall]),
        ],
        vec![
            mon(Species::Snorlax, &[PokemonMove::BodySlam]),
            mon(Species::Gyarados, &[PokemonMove::Waterfall]),
        ],
    );
    let config = open_list_config(3);

    let result = solve_open_list_preview(
        &belief,
        meta,
        pokemon_dex,
        move_dex,
        &config,
        &open_list_determinize_config(),
    )
    .expect("the belief is well formed");

    let choice_count = preview_choices(
        &open_list_worlds(
            &belief,
            meta,
            pokemon_dex,
            move_dex,
            &config,
            &open_list_determinize_config(),
        )
        .expect("the belief is well formed")[0]
            .state,
        Player::P1,
    )
    .len();

    for (label, strategy) in [("P1", &result.p1_strategy), ("P2", &result.p2_strategy)] {
        let total: f64 = strategy.iter().map(|choice| choice.probability).sum();
        assert!(
            (total - 1.0).abs() < 1e-6,
            "{label} strategy sums to {total}"
        );
        assert!(!strategy.is_empty(), "{label} strategy is empty");
        assert!(
            strategy.len() <= choice_count,
            "{label} returned {} entries for {choice_count} choices",
            strategy.len()
        );
    }
    assert_eq!(result.sampling.worlds, 3);
}

// ── The sampling search ─────────────────────────────────────────────────────

/// One turn of lookahead, one damage roll, and exact outcome enumeration. The
/// exact search with [`base_config`] then solves the same game, so it is the
/// oracle of these tests.
fn mcts_config() -> MctsConfig {
    MctsConfig {
        iterations: 600,
        depth: 1,
        damage_rolls: 1,
        consider_crit: false,
        transition: TransitionMode::Enumerated(ChanceMode::Enumerate),
        ..MctsConfig::default()
    }
}

/// The exact value of `contested_position` at the same depth.
fn exact_value() -> f64 {
    let (pokemon_dex, move_dex) = dexes();
    let config = SolveConfig {
        algorithm: SolverAlgorithm::BackwardInduction,
        ..base_config()
    };
    solve(&contested_position(), pokemon_dex, move_dex, &config)
        .expect("the position is playable")
        .value
}

fn run_mcts(seed: u64, config: &MctsConfig) -> mcts::MctsResult {
    let (pokemon_dex, move_dex) = dexes();
    mcts::search(seed, &contested_position(), pokemon_dex, move_dex, config)
        .expect("the position is playable")
}

/// A policy error hides behind engine noise, so this test removes the engine.
/// The matrix has no saddle point, so only a mixed strategy reaches its value.
#[test]
fn mcts_matrix_game_finds_the_equilibrium() {
    let payoffs = vec![vec![0.7, 0.2], vec![0.3, 0.6]];
    let exact = solve_matrix_game(&payoffs).value;

    for policy in [SelectionPolicy::RegretMatching, SelectionPolicy::Exp3] {
        let learned = mcts::learn_matrix_game(9, &payoffs, 20_000, policy, 0.1, false);
        assert!(
            (learned.value - exact).abs() < 0.05,
            "{policy:?} learned {}, the exact value is {exact}",
            learned.value
        );
        for (label, strategy) in [
            ("row", &learned.row_strategy),
            ("column", &learned.col_strategy),
        ] {
            let total: f64 = strategy.iter().sum();
            assert!(
                (total - 1.0).abs() < 1e-9,
                "{policy:?} {label} strategy sums to {total}"
            );
        }
    }
}

/// The sampling search must agree with the exact search on a small position.
/// Explicit exploration biases the mean, so the tolerance is wider than the
/// standard error alone.
#[test]
fn mcts_approaches_the_exact_value() {
    let exact = exact_value();
    let result = run_mcts(3, &mcts_config());

    assert!(
        (result.value - exact).abs() < 0.08,
        "the search returned {}, the exact value is {exact}",
        result.value
    );
    assert!(result.stats.turns_simulated > 0);
    assert!(result.warnings.is_empty(), "{:?}", result.warnings);
}

/// Two learners of the same game must reach the same value.
#[test]
fn mcts_policies_both_converge() {
    let exact = exact_value();

    let mut values = Vec::new();
    for policy in [SelectionPolicy::RegretMatching, SelectionPolicy::Exp3] {
        let config = MctsConfig {
            policy,
            ..mcts_config()
        };
        let value = run_mcts(21, &config).value;
        assert!(
            (value - exact).abs() < 0.08,
            "{policy:?} returned {value}, the exact value is {exact}"
        );
        values.push(value);
    }

    assert!(
        (values[0] - values[1]).abs() < 0.08,
        "the policies returned {} and {}",
        values[0],
        values[1]
    );
}

/// One seed must give one answer. The search draws actions, successors, and
/// engine outcomes, so every draw has to come from the seeded generator.
#[test]
fn mcts_is_seed_reproducible() {
    let config = MctsConfig {
        iterations: 120,
        ..mcts_config()
    };

    let first = run_mcts(77, &config);
    let second = run_mcts(77, &config);

    assert_eq!(first.value, second.value);
    assert_eq!(first.p1_strategy.len(), second.p1_strategy.len());
    for (left, right) in first.p1_strategy.iter().zip(&second.p1_strategy) {
        assert_eq!(left.probability, right.probability);
    }
    assert_eq!(first.stats.turns_simulated, second.stats.turns_simulated);
}

/// The caller has to see how far to trust the value.
#[test]
fn mcts_reports_sampling_error() {
    let config = MctsConfig {
        iterations: 60,
        ..mcts_config()
    };
    let result = run_mcts(5, &config);

    assert_eq!(result.sampling.iterations, 60);
    assert_eq!(result.stats.iterations, 60);
    assert!((result.sampling.mean - result.value).abs() < 1e-12);
    let error = result
        .sampling
        .standard_error
        .expect("sixty iterations report a standard error");
    assert!(error.is_finite() && error >= 0.0, "the error is {error}");

    let single = run_mcts(
        5,
        &MctsConfig {
            iterations: 1,
            ..mcts_config()
        },
    );
    assert_eq!(single.sampling.iterations, 1);
    assert_eq!(single.sampling.standard_error, None);
}

/// The root exists before the first iteration, so every requested iteration
/// samples a path. An iteration that only created the root would cost a turn of
/// the budget and learn nothing.
#[test]
fn mcts_one_iteration_samples_one_root_path() {
    let result = run_mcts(
        5,
        &MctsConfig {
            iterations: 1,
            ..mcts_config()
        },
    );

    assert_eq!(result.stats.iterations, 1);
    assert_eq!(result.stats.turns_simulated, 1);
}

/// A sparse chance mode drops outcome mass. The result must say so, because the
/// dropped mass is a second source of error beside the sampling error.
#[test]
fn mcts_sparse_chance_reports_discarded_mass() {
    let config = MctsConfig {
        iterations: 60,
        damage_rolls: 4,
        transition: TransitionMode::Enumerated(ChanceMode::TopK(1)),
        ..mcts_config()
    };
    let result = run_mcts(13, &config);

    let discarded = result
        .warnings
        .iter()
        .find_map(|warning| match warning {
            SolveWarning::ChanceMassDiscarded { max_fraction } => Some(*max_fraction),
            _ => None,
        })
        .expect("one kept branch of four discards mass");
    assert!(
        discarded > 0.0 && discarded < 1.0,
        "the search discarded {discarded}"
    );

    let enumerated = MctsConfig {
        transition: TransitionMode::Enumerated(ChanceMode::Enumerate),
        ..config
    };
    let unreduced = run_mcts(13, &enumerated);
    assert!(
        unreduced
            .warnings
            .iter()
            .all(|warning| !matches!(warning, SolveWarning::ChanceMassDiscarded { .. })),
        "{:?}",
        unreduced.warnings
    );
}

/// The generative transition mode with the same lookahead as [`mcts_config`].
/// A batch of one keeps the independent draw of each visit.
fn generative_mcts_config() -> MctsConfig {
    MctsConfig {
        transition: TransitionMode::Generative { batch: 1 },
        ..mcts_config()
    }
}

/// The generative mode samples inside turn resolution instead of enumerating the
/// outcome distribution. The draw stays unbiased, so the search must still reach
/// the value that backward induction computes.
///
/// The tolerance matches `mcts_approaches_the_exact_value`: explicit exploration
/// biases the mean by more than the sampling error alone, so a three-error band
/// would fail on the bias rather than on the transition mode.
#[test]
fn generative_mcts_agrees_with_the_exact_search() {
    let exact = exact_value();
    let result = run_mcts(3, &generative_mcts_config());

    assert!(
        (result.value - exact).abs() < 0.08,
        "the generative search returned {}, the exact value is {exact}",
        result.value
    );
    assert!(result.stats.turns_simulated > 0);
    let error = result
        .sampling
        .standard_error
        .expect("the search ran many iterations");
    assert!(error.is_finite() && error >= 0.0, "the error is {error}");
}

/// The generative mode never builds an outcome list, so it cannot drop outcome
/// mass. A sparse [`ChanceMode`] is what discards mass, and this mode has none.
#[test]
fn generative_mcts_discards_no_outcome_mass() {
    let config = MctsConfig {
        iterations: 60,
        damage_rolls: 16,
        consider_crit: true,
        ..generative_mcts_config()
    };
    let result = run_mcts(13, &config);

    assert!(
        result
            .warnings
            .iter()
            .all(|warning| !matches!(warning, SolveWarning::ChanceMassDiscarded { .. })),
        "{:?}",
        result.warnings
    );

    // The same budget under a sparse enumerated mode does discard mass, which is
    // what keeps the check above from passing for a trivial reason.
    let sparse = MctsConfig {
        transition: TransitionMode::Enumerated(ChanceMode::TopK(1)),
        ..config
    };
    assert!(
        run_mcts(13, &sparse)
            .warnings
            .iter()
            .any(|warning| matches!(warning, SolveWarning::ChanceMassDiscarded { .. })),
        "the sparse mode must still report discarded mass"
    );
}

/// One seed must give one answer in the generative mode too. Every draw of the
/// transition comes from the seeded generator of the search.
#[test]
fn generative_mcts_repeats_under_one_seed() {
    let config = MctsConfig {
        iterations: 120,
        ..generative_mcts_config()
    };

    let first = run_mcts(77, &config);
    let second = run_mcts(77, &config);

    assert_eq!(first.value, second.value);
    assert_eq!(first.p1_strategy.len(), second.p1_strategy.len());
    for (left, right) in first.p1_strategy.iter().zip(&second.p1_strategy) {
        assert_eq!(left.probability, right.probability);
    }
    assert_eq!(first.stats.turns_simulated, second.stats.turns_simulated);
}

/// The average strategy of each root learner must be a distribution over that
/// player's actions.
#[test]
fn mcts_strategy_is_a_distribution() {
    let result = run_mcts(31, &mcts_config());

    for (label, strategy) in [("P1", &result.p1_strategy), ("P2", &result.p2_strategy)] {
        let total: f64 = strategy.iter().map(|action| action.probability).sum();
        assert!(
            (total - 1.0).abs() < 1e-6,
            "{label} strategy sums to {total}"
        );
        assert!(!strategy.is_empty(), "{label} strategy is empty");
        assert!(
            strategy.iter().all(|action| action.probability > 0.0),
            "{label} strategy kept a zero-probability action"
        );
    }
    assert!((result.p1_win_odds + result.p2_win_odds - 1.0).abs() < 1e-9);
    assert!((0.0..=1.0).contains(&result.value));
}

/// The sampling search refuses the same positions that the exact search refuses.
#[test]
fn mcts_refuses_a_preview_and_a_finished_battle() {
    let (pokemon_dex, move_dex) = dexes();
    let config = mcts_config();

    let preview = MatchState::TeamPreviewState(small_preview());
    assert_eq!(
        mcts::search(1, &preview, pokemon_dex, move_dex, &config).unwrap_err(),
        SolveError::TeamPreviewUnsupported
    );

    let finished = MatchState::GameOverState {
        winner: Player::P1,
        pending_events: Vec::new(),
        final_state: Box::new(battle_state_from_lists(
            vec![mon(Species::Pikachu, &[PokemonMove::Thunderbolt])],
            vec![],
            vec![mon(Species::Snorlax, &[PokemonMove::BodySlam])],
            vec![],
        )),
    };
    assert_eq!(
        mcts::search(1, &finished, pokemon_dex, move_dex, &config).unwrap_err(),
        SolveError::GameAlreadyOver { winner: Player::P1 }
    );
}

/// Zero worlds is a configuration error, not an empty answer.
#[test]
fn open_list_preview_rejects_zero_worlds() {
    with_meta!(meta);
    let (pokemon_dex, move_dex) = dexes();
    let belief = open_list_belief_1v1();

    let result = solve_open_list_preview(
        &belief,
        meta,
        pokemon_dex,
        move_dex,
        &open_list_config(0),
        &open_list_determinize_config(),
    );

    assert_eq!(result.unwrap_err(), OpenListError::NoWorlds);
}

// ── Progressive widening and exploitability ─────────────────────────────────

/// Four moves and two bench Pokemon a side. Each player then holds ten joint
/// actions, which is enough room for a prefix to leave real choices out.
fn wide_position() -> MatchState {
    MatchState::BattleState(battle_state_from_lists(
        vec![mon(
            Species::Pikachu,
            &[
                PokemonMove::Thunderbolt,
                PokemonMove::QuickAttack,
                PokemonMove::IronTail,
                PokemonMove::Protect,
            ],
        )],
        vec![
            mon(
                Species::Gyarados,
                &[PokemonMove::Waterfall, PokemonMove::IceFang],
            ),
            mon(
                Species::Blastoise,
                &[PokemonMove::Surf, PokemonMove::IceBeam],
            ),
        ],
        vec![mon(
            Species::Snorlax,
            &[
                PokemonMove::BodySlam,
                PokemonMove::Crunch,
                PokemonMove::Earthquake,
                PokemonMove::Rest,
            ],
        )],
        vec![
            mon(
                Species::Gengar,
                &[PokemonMove::ShadowBall, PokemonMove::SludgeBomb],
            ),
            mon(
                Species::Alakazam,
                &[PokemonMove::Psychic, PokemonMove::Recover],
            ),
        ],
    ))
}

/// A completed exact search must keep a mixed strategy when the battle needs
/// one. This checks the search result after the matrix solver and root mapping.
#[test]
fn a_completed_battle_search_keeps_its_mixed_strategy() {
    let (pokemon_dex, move_dex) = dexes();
    let result = solve(&wide_position(), pokemon_dex, move_dex, &base_config())
        .expect("the position is playable");

    assert!(result.p1_strategy.len() > 1, "{:?}", result.p1_strategy);
    assert!(result.p2_strategy.len() > 1, "{:?}", result.p2_strategy);
    assert_valid_strategies(&result);
}

/// The complete joint actions of one player, with no cap and no dominance
/// filter.
fn complete_actions(state: &MatchState, player: Player) -> Vec<Vec<BattleCommand>> {
    let (pokemon_dex, move_dex) = dexes();
    let battle = match state {
        MatchState::BattleState(battle) => battle,
        _ => panic!("the helper needs a battle position"),
    };
    solver_actions::joint_actions(
        battle,
        player,
        solver_actions::phase_of(state),
        move_dex,
        pokemon_dex,
        None,
        false,
    )
    .actions
}

/// One slot command without its target or its resource flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum BaseCommand {
    Pass,
    Struggle,
    Switch(usize),
    Attack(usize),
}

fn base_command(command: &BattleCommand) -> BaseCommand {
    match command {
        BattleCommand::Pass => BaseCommand::Pass,
        BattleCommand::Struggle { .. } => BaseCommand::Struggle,
        BattleCommand::Switch(switch) => BaseCommand::Switch(switch.party_index),
        BattleCommand::Attack(attack) => BaseCommand::Attack(attack.move_slot),
    }
}

/// The distinct base commands that a list of joint actions offers.
fn base_commands(actions: &[Vec<BattleCommand>]) -> HashSet<Vec<BaseCommand>> {
    actions
        .iter()
        .map(|combo| combo.iter().map(base_command).collect())
        .collect()
}

/// A strategy that plays every joint action equally often.
fn uniform_strategy(actions: &[Vec<BattleCommand>]) -> Vec<JointActionProb> {
    actions
        .iter()
        .map(|commands| JointActionProb {
            commands: commands.clone(),
            probability: 1.0 / actions.len() as f64,
        })
        .collect()
}

/// A strategy that always plays one joint action.
fn pure_strategy(commands: &[BattleCommand]) -> Vec<JointActionProb> {
    vec![JointActionProb {
        commands: commands.to_vec(),
        probability: 1.0,
    }]
}

/// The exploitability gap of one strategy pair at `state`.
fn gap_of(
    state: &MatchState,
    p1_strategy: &[JointActionProb],
    p2_strategy: &[JointActionProb],
) -> f64 {
    let (pokemon_dex, move_dex) = dexes();
    let report = exploitability(
        state,
        pokemon_dex,
        move_dex,
        &base_config(),
        p1_strategy,
        p2_strategy,
    )
    .expect("the position is playable");
    assert_eq!(
        report.unmatched,
        [0, 0],
        "the check could not place every strategy entry"
    );
    report.gap
}

/// The order has to be a permutation. A widened node reads a prefix of it, and
/// a lost index would remove a legal action from the search for good.
#[test]
fn coverage_order_is_a_permutation() {
    let actions = complete_actions(&wide_position(), Player::P1);
    assert!(actions.len() >= 6, "the position offers {}", actions.len());

    let order = solver_actions::coverage_order(&actions);

    assert_eq!(order.len(), actions.len());
    let distinct: HashSet<usize> = order.iter().copied().collect();
    assert_eq!(distinct.len(), actions.len(), "the order repeats an index");
    assert!(order.iter().all(|&index| index < actions.len()));
}

/// A prefix of the coverage order must hold more distinct choices than the
/// generated order of the same length. Row-major generation puts every resource
/// variant of one move next to it, so its prefix repeats a move slot before it
/// reaches the next one.
#[test]
fn coverage_order_prefix_covers_each_slot_command() {
    let actions = complete_actions(&wide_position(), Player::P1);
    let order = solver_actions::coverage_order(&actions);
    let distinct = base_commands(&actions).len();

    let ordered: Vec<Vec<BattleCommand>> = order[..distinct]
        .iter()
        .map(|&index| actions[index].clone())
        .collect();

    assert_eq!(
        base_commands(&ordered).len(),
        distinct,
        "the coverage prefix missed a choice: {ordered:?}"
    );
    assert!(
        base_commands(&actions[..distinct]).len() < distinct,
        "the generated order already covered every choice"
    );
}

/// The allowed count starts at `initial`, never falls, and reaches the total.
#[test]
fn widening_grows_with_the_visit_count() {
    let widening = Widening {
        initial: 3,
        coefficient: 2.0,
        exponent: 0.5,
    };
    let total = 10;

    assert_eq!(widening.allowed(0, total), 3);
    assert_eq!(
        widening.allowed(0, 2),
        2,
        "the count never exceeds the total"
    );
    assert_eq!(widening.allowed(1_000, 0), 0, "an empty set stays empty");

    let mut previous = widening.allowed(0, total);
    let mut reached = false;
    for visits in 0..1_000u64 {
        let allowed = widening.allowed(visits, total);
        assert!(
            allowed >= previous,
            "the count fell from {previous} to {allowed} at {visits} visits"
        );
        assert!((1..=total).contains(&allowed));
        previous = allowed;
        reached |= allowed == total;
    }
    assert!(reached, "the count never reached {total}");
}

/// The exact equilibrium of a position cannot be exploited, so its gap is zero.
/// This is the calibration of the check itself.
#[test]
fn exploitability_is_zero_for_the_exact_equilibrium() {
    let (pokemon_dex, move_dex) = dexes();
    let state = contested_position();
    let config = SolveConfig {
        algorithm: SolverAlgorithm::BackwardInduction,
        ..base_config()
    };
    let solved = solve(&state, pokemon_dex, move_dex, &config).expect("the position is playable");

    let gap = gap_of(&state, &solved.p1_strategy, &solved.p2_strategy);

    assert!(gap > -1e-9, "the gap is negative: {gap}");
    assert!(gap < 1e-6, "the equilibrium reported a gap of {gap}");
}

/// A pure strategy on one action is exploitable, and the check has to see it
/// even though both players stayed inside the legal action set.
#[test]
fn exploitability_finds_a_neglected_action() {
    let state = contested_position();
    let (pokemon_dex, move_dex) = dexes();
    let config = SolveConfig {
        algorithm: SolverAlgorithm::BackwardInduction,
        ..base_config()
    };
    let solved = solve(&state, pokemon_dex, move_dex, &config).expect("the position is playable");
    let equilibrium = gap_of(&state, &solved.p1_strategy, &solved.p2_strategy);

    let p1 = complete_actions(&state, Player::P1);
    let p2 = complete_actions(&state, Player::P2);
    let pure = gap_of(&state, &pure_strategy(&p1[0]), &pure_strategy(&p2[0]));

    assert!(
        pure > equilibrium + 0.05,
        "the pure pair gave {pure}, the equilibrium gave {equilibrium}"
    );
}

/// The report must include stop-limit warnings and complete cost statistics.
#[test]
fn exploitability_reports_approximation_and_complete_costs() {
    let state = contested_position();
    let p1 = complete_actions(&state, Player::P1);
    let p2 = complete_actions(&state, Player::P2);
    let (pokemon_dex, move_dex) = dexes();
    let report = exploitability(
        &state,
        pokemon_dex,
        move_dex,
        &SolveConfig {
            depth: 2,
            node_budget: Some(0),
            ..base_config()
        },
        &uniform_strategy(&p1),
        &uniform_strategy(&p2),
    )
    .expect("the position is playable");

    assert!(
        report
            .warnings
            .contains(&SolveWarning::BudgetExhausted { budget: 0 })
    );
    assert!(report.stats.elapsed > Duration::ZERO);
    assert!(report.stats.matrix_cells_evaluated > 0);
    assert!(report.stats.matrix_cells_total >= (report.actions[0] * report.actions[1]) as u64);
    assert!(report.stats.matrix_cells_total >= report.stats.matrix_cells_evaluated);

    let reduced = exploitability(
        &state,
        pokemon_dex,
        move_dex,
        &SolveConfig {
            damage_rolls: 4,
            chance: ChanceMode::TopK(1),
            ..base_config()
        },
        &uniform_strategy(&p1),
        &uniform_strategy(&p2),
    )
    .expect("the position is playable");
    assert!(
        reduced
            .warnings
            .iter()
            .any(|warning| matches!(warning, SolveWarning::ChanceMassDiscarded { .. }))
    );
}

/// The Nash mode must reproduce the equilibrium of the same position, for both
/// players. It therefore spends no exploitability budget.
#[test]
fn nash_mode_returns_the_equilibrium_value() {
    let state = contested_position();
    let (pokemon_dex, move_dex) = dexes();
    let config = SolveConfig {
        algorithm: SolverAlgorithm::BackwardInduction,
        ..base_config()
    };
    let solved = solve(&state, pokemon_dex, move_dex, &config).expect("the position is playable");

    for (exploiter, model) in [
        (Player::P1, &solved.p2_strategy),
        (Player::P2, &solved.p1_strategy),
    ] {
        let report = respond(
            &state,
            pokemon_dex,
            move_dex,
            &base_config(),
            exploiter,
            model,
            ResponseMode::Nash,
        )
        .expect("the position is playable");

        assert_eq!(report.unmatched, 0, "{exploiter:?} lost a model entry");
        assert_eq!(report.confidence, 0.0);
        assert!(
            (report.nash_value - solved.value).abs() < 1e-6,
            "{exploiter:?} reported {} against {}",
            report.nash_value,
            solved.value
        );
        assert!(
            report.budget_spent.abs() < 1e-6,
            "{exploiter:?} spent {}",
            report.budget_spent
        );
        assert!(
            report.stats.nodes_expanded > 0,
            "the response omitted the root node"
        );
        let total: f64 = report.strategy.iter().map(|a| a.probability).sum();
        assert!((total - 1.0).abs() < 1e-6, "the strategy sums to {total}");
    }
}

/// The response statistics must include the root decision node.
#[test]
fn response_stats_include_the_root_node() {
    let state = contested_position();
    let (pokemon_dex, move_dex) = dexes();
    let p1 = complete_actions(&state, Player::P1);
    let p2 = complete_actions(&state, Player::P2);
    let config = base_config();
    let baseline = exploitability(
        &state,
        pokemon_dex,
        move_dex,
        &config,
        &uniform_strategy(&p1),
        &uniform_strategy(&p2),
    )
    .expect("the position is playable");
    let response = respond(
        &state,
        pokemon_dex,
        move_dex,
        &config,
        Player::P1,
        &uniform_strategy(&p2),
        ResponseMode::Nash,
    )
    .expect("the position is playable");

    assert_eq!(
        response.stats.nodes_expanded,
        baseline.stats.nodes_expanded + 1
    );
}

/// Invalid numeric inputs must not put non-finite values in a response.
#[test]
fn response_sanitizes_non_finite_probabilities() {
    let state = contested_position();
    let (pokemon_dex, move_dex) = dexes();
    let p1 = complete_actions(&state, Player::P1);
    let model: Vec<JointActionProb> = p1
        .iter()
        .map(|commands| JointActionProb {
            commands: commands.clone(),
            probability: f64::INFINITY,
        })
        .collect();

    let report = respond(
        &state,
        pokemon_dex,
        move_dex,
        &base_config(),
        Player::P2,
        &model,
        ResponseMode::Exploit {
            confidence: f64::NAN,
        },
    )
    .expect("the position is playable");

    assert_eq!(report.confidence, 0.0);
    assert!(report.nash_value.is_finite());
    assert!(report.model_value.is_finite());
    assert!(report.worst_case.is_finite());
    assert!(report.budget_spent.is_finite());
    assert!(
        report
            .strategy
            .iter()
            .all(|action| action.probability.is_finite())
    );

    let mut huge_model = Vec::new();
    for commands in &p1 {
        for _ in 0..2 {
            huge_model.push(JointActionProb {
                commands: commands.clone(),
                probability: f64::MAX,
            });
        }
    }
    let full_response = |model: &[JointActionProb]| {
        respond(
            &state,
            pokemon_dex,
            move_dex,
            &base_config(),
            Player::P2,
            model,
            ResponseMode::Exploit { confidence: 1.0 },
        )
        .expect("the position is playable")
    };
    let uniform = full_response(&[]);
    let huge = full_response(&huge_model);
    assert!((huge.model_value - uniform.model_value).abs() < 1e-9);
}

/// Full confidence must take more value from a pure model than the Nash
/// strategy does, and it must pay for that with a positive budget.
///
/// P2 reads the model here. P1 holds one action that answers every P2 action of
/// this position, so a P1 exploiter has nothing left to take, and only P2 shows
/// the trade. Every value is a P1 win probability, so P2 takes value by moving
/// the value down.
#[test]
fn full_confidence_beats_nash_against_the_model() {
    let state = contested_position();
    let (pokemon_dex, move_dex) = dexes();
    let p1 = complete_actions(&state, Player::P1);
    let model = pure_strategy(&p1[1]);

    let answer = |mode| {
        respond(
            &state,
            pokemon_dex,
            move_dex,
            &base_config(),
            Player::P2,
            &model,
            mode,
        )
        .expect("the position is playable")
    };

    let nash = answer(ResponseMode::Nash);
    let exploit = answer(ResponseMode::Exploit { confidence: 1.0 });

    assert_eq!(exploit.unmatched, 0, "the check lost a model entry");
    assert!(
        exploit.model_value < nash.model_value - 0.01,
        "full confidence held P1 to {} and Nash held P1 to {}",
        exploit.model_value,
        nash.model_value
    );
    assert!(
        exploit.budget_spent > 0.0,
        "the answer spent {}",
        exploit.budget_spent
    );
    assert!(
        exploit.worst_case > nash.worst_case - 1e-9,
        "the worst case fell from {} to {}",
        nash.worst_case,
        exploit.worst_case
    );
}

/// The budget scan must never spend more than its limit, and it must never take
/// less from the model than the Nash strategy does. P2 reads the model, for the
/// reason that `full_confidence_beats_nash_against_the_model` gives.
#[test]
fn a_budget_limit_bounds_the_worst_case_loss() {
    let state = contested_position();
    let (pokemon_dex, move_dex) = dexes();
    let p1 = complete_actions(&state, Player::P1);
    let model = pure_strategy(&p1[1]);
    let limit = 0.05;

    let nash = respond(
        &state,
        pokemon_dex,
        move_dex,
        &base_config(),
        Player::P2,
        &model,
        ResponseMode::Nash,
    )
    .expect("the position is playable");

    let bounded = respond_within_budget(
        &state,
        pokemon_dex,
        move_dex,
        &base_config(),
        Player::P2,
        &model,
        limit,
    )
    .expect("the position is playable");

    assert!(
        bounded.budget_spent <= limit + 1e-9,
        "the scan spent {} of {limit}",
        bounded.budget_spent
    );
    assert!(
        bounded.model_value <= nash.model_value + 1e-9,
        "the scan held P1 to {} and Nash held P1 to {}",
        bounded.model_value,
        nash.model_value
    );
    assert!(
        bounded.confidence > 0.0,
        "the limit of {limit} bought no confidence"
    );

    // A budget of zero can buy nothing beyond a free gain, so the answer must
    // keep the Nash worst case.
    let none = respond_within_budget(
        &state,
        pokemon_dex,
        move_dex,
        &base_config(),
        Player::P2,
        &model,
        0.0,
    )
    .expect("the position is playable");
    assert!(
        none.budget_spent.abs() < 1e-6,
        "a budget of zero spent {}",
        none.budget_spent
    );
    assert!(
        none.model_value <= nash.model_value + 1e-9,
        "the zero budget took less than Nash"
    );
}

/// One turn of lookahead over `wide_position`, widened from four actions.
fn widening_config() -> MctsConfig {
    MctsConfig {
        iterations: 3_000,
        depth: 1,
        damage_rolls: 1,
        consider_crit: false,
        transition: TransitionMode::Enumerated(ChanceMode::Enumerate),
        // The root holds ten actions, so this schedule plays four of them for
        // the first hundred visits and reaches all ten at four hundred.
        widening: Some(Widening {
            initial: 4,
            coefficient: 0.5,
            exponent: 0.5,
        }),
        ..MctsConfig::default()
    }
}

fn run_wide_mcts(seed: u64, config: &MctsConfig) -> mcts::MctsResult {
    let (pokemon_dex, move_dex) = dexes();
    mcts::search(seed, &wide_position(), pokemon_dex, move_dex, config)
        .expect("the position is playable")
}

/// The exact value of `wide_position` at the same depth.
fn wide_exact_value() -> f64 {
    let (pokemon_dex, move_dex) = dexes();
    let config = SolveConfig {
        algorithm: SolverAlgorithm::BackwardInduction,
        ..base_config()
    };
    solve(&wide_position(), pokemon_dex, move_dex, &config)
        .expect("the position is playable")
        .value
}

/// A widened search must still find the value of the game. Widening changes the
/// order in which the search meets the actions, not the game.
#[test]
fn mcts_widening_approaches_the_exact_value() {
    let exact = wide_exact_value();
    let result = run_wide_mcts(3, &widening_config());

    assert!(
        (result.value - exact).abs() < 0.08,
        "the widened search returned {}, the exact value is {exact}",
        result.value
    );
    assert!(result.stats.turns_simulated > 0);
}

/// The point of the whole feature: the widened strategy has to hold up against
/// the complete action set, not only against the prefix that it played.
#[test]
fn mcts_widening_keeps_a_small_exploitability_gap() {
    let state = wide_position();
    let result = run_wide_mcts(3, &widening_config());

    let widened = gap_of(&state, &result.p1_strategy, &result.p2_strategy);

    let p1 = complete_actions(&state, Player::P1);
    let p2 = complete_actions(&state, Player::P2);
    let pure = gap_of(&state, &pure_strategy(&p1[0]), &pure_strategy(&p2[0]));
    let uniform = gap_of(&state, &uniform_strategy(&p1), &uniform_strategy(&p2));

    assert!(
        widened >= 0.0,
        "the gap of a strategy pair is never negative: {widened}"
    );
    assert!(
        widened < pure,
        "the widened pair gave {widened}, one pure action gave {pure}"
    );
    assert!(
        widened < uniform,
        "the widened pair gave {widened}, uniform play gave {uniform}"
    );
}

/// One seed must give one answer, as it does without widening. The coverage
/// order and the allowed count are both pure functions of the position and the
/// visit count, so nothing here may vary between runs.
#[test]
fn mcts_widening_is_seed_reproducible() {
    let config = MctsConfig {
        iterations: 200,
        ..widening_config()
    };

    let first = run_wide_mcts(41, &config);
    let second = run_wide_mcts(41, &config);

    assert_eq!(first.value, second.value);
    assert_eq!(first.stats.turns_simulated, second.stats.turns_simulated);
    assert_eq!(first.p1_strategy.len(), second.p1_strategy.len());
    for (left, right) in first.p1_strategy.iter().zip(&second.p1_strategy) {
        assert_eq!(left.probability, right.probability);
        assert_eq!(
            format!("{:?}", left.commands),
            format!("{:?}", right.commands)
        );
    }
}

/// A root that never widened to its complete set played a subset, so the result
/// must say so. A caller that reads the strategy alone would otherwise believe
/// that the search rejected the omitted actions.
#[test]
fn mcts_widening_reports_a_truncated_root() {
    let config = MctsConfig {
        iterations: 20,
        widening: Some(Widening {
            initial: 2,
            coefficient: 0.1,
            exponent: 0.5,
        }),
        ..widening_config()
    };
    let result = run_wide_mcts(9, &config);

    let truncated: Vec<&SolveWarning> = result
        .warnings
        .iter()
        .filter(|warning| matches!(warning, SolveWarning::ActionsTruncated { .. }))
        .collect();
    assert_eq!(truncated.len(), 2, "{:?}", result.warnings);
    for warning in truncated {
        let SolveWarning::ActionsTruncated { kept, total, .. } = warning else {
            unreachable!("the filter kept only truncations");
        };
        assert!(kept < total, "the warning reports {kept} of {total}");
    }

    // The same search without widening reports nothing.
    let complete = MctsConfig {
        widening: None,
        ..config
    };
    let unwidened = run_wide_mcts(9, &complete);
    assert!(
        unwidened
            .warnings
            .iter()
            .all(|warning| !matches!(warning, SolveWarning::ActionsTruncated { .. })),
        "{:?}",
        unwidened.warnings
    );
}

/// The warning must describe the last prefix that contributed to the strategy.
#[test]
fn mcts_widening_warning_does_not_count_the_next_prefix() {
    let config = MctsConfig {
        iterations: 10,
        widening: Some(Widening {
            initial: 1,
            coefficient: 1.0,
            exponent: 1.0,
        }),
        ..widening_config()
    };
    let result = run_wide_mcts(9, &config);

    for player in [Player::P1, Player::P2] {
        let warning = result
            .warnings
            .iter()
            .find(|warning| {
                matches!(
                    warning,
                    SolveWarning::ActionsTruncated {
                        player: warned,
                        ..
                    } if *warned == player
                )
            })
            .expect("the last prefix omits one action");
        let SolveWarning::ActionsTruncated { kept, total, .. } = warning else {
            unreachable!("the search found a truncation warning")
        };
        assert_eq!((*kept, *total), (9, 10));
        assert!(warning.to_string().contains("limited to 9 of 10"));
    }
}

// ── The stratified batch ────────────────────────────────────────────────────

/// A position where each player holds exactly one joint action.
///
/// Action selection then adds no noise, so the transition is the only source of
/// sampling error. Both moves always hit and carry no secondary effect, so the
/// damage roll of each attack is the only chokepoint of the turn. Neither attack
/// comes close to a knockout, so every roll leaves a different position for the
/// evaluator to score.
///
/// Both sides have spent their Tera, because a Tera of the one move would be a
/// second joint action.
fn single_action_position() -> MatchState {
    let mut battle = battle_state_from_lists(
        vec![mon(Species::Blastoise, &[PokemonMove::Surf])],
        vec![],
        vec![mon(Species::Snorlax, &[PokemonMove::Earthquake])],
        vec![],
    );
    battle.p1_has_tera = false;
    battle.p2_has_tera = false;
    battle.p1_has_mega = false;
    battle.p2_has_mega = false;
    MatchState::BattleState(battle)
}

/// One turn, sixteen damage rolls, and a generative transition of `batch`
/// members.
fn stratified_config(batch: usize) -> MctsConfig {
    MctsConfig {
        iterations: 32,
        depth: 1,
        damage_rolls: 16,
        consider_crit: false,
        transition: TransitionMode::Generative { batch },
        ..MctsConfig::default()
    }
}

/// The exact value of `single_action_position` under [`stratified_config`].
fn single_action_exact_value() -> f64 {
    let (pokemon_dex, move_dex) = dexes();
    let config = SolveConfig {
        algorithm: SolverAlgorithm::BackwardInduction,
        depth: 1,
        damage_rolls: 16,
        consider_crit: false,
        chance: ChanceMode::Enumerate,
        ..SolveConfig::default()
    };
    solve(&single_action_position(), pokemon_dex, move_dex, &config)
        .expect("the position is playable")
        .value
}

/// The root-mean-square error of the search against `exact`, over `seeds`.
fn single_action_rmse(seeds: &[u64], batch: usize, exact: f64) -> f64 {
    let (pokemon_dex, move_dex) = dexes();
    let config = stratified_config(batch);
    let squares: f64 = seeds
        .iter()
        .map(|&seed| {
            let value = mcts::search(
                seed,
                &single_action_position(),
                pokemon_dex,
                move_dex,
                &config,
            )
            .expect("the position is playable")
            .value;
            (value - exact).powi(2)
        })
        .sum();
    (squares / seeds.len() as f64).sqrt()
}

/// One batch member keeps the law of one independent draw, so the batched search
/// must still reach the value that backward induction computes.
///
/// The tolerance matches `generative_mcts_agrees_with_the_exact_search`, because
/// explicit exploration biases the mean by more than the sampling error alone.
#[test]
fn mcts_stratified_batch_matches_the_exact_value() {
    let exact = exact_value();
    let config = MctsConfig {
        transition: TransitionMode::Generative { batch: 16 },
        ..generative_mcts_config()
    };

    let result = run_mcts(3, &config);

    assert!(
        (result.value - exact).abs() < 0.08,
        "the batched search returned {}, the exact value is {exact}",
        result.value
    );
    assert!(result.stats.turns_simulated > 0);
    assert!(result.warnings.is_empty(), "{:?}", result.warnings);
}

/// A batch adds a plan seed and a cursor for every chance node. Both must come
/// from the seeded stream of the search, so one seed still gives one answer.
#[test]
fn mcts_stratified_batch_repeats_under_one_seed() {
    let config = MctsConfig {
        iterations: 120,
        transition: TransitionMode::Generative { batch: 8 },
        ..generative_mcts_config()
    };

    let first = run_mcts(77, &config);
    let second = run_mcts(77, &config);

    assert_eq!(first.value, second.value);
    assert_eq!(first.p1_strategy.len(), second.p1_strategy.len());
    for (left, right) in first.p1_strategy.iter().zip(&second.p1_strategy) {
        assert_eq!(left.probability, right.probability);
    }
    assert_eq!(first.stats.turns_simulated, second.stats.turns_simulated);
}

/// Stratified visits are dependent. The independent-sample formula does not
/// give a valid standard error for this search.
#[test]
fn mcts_stratified_batch_omits_the_standard_error() {
    let config = MctsConfig {
        iterations: 32,
        transition: TransitionMode::Generative { batch: 8 },
        ..generative_mcts_config()
    };

    let result = run_mcts(77, &config);

    assert_eq!(result.sampling.iterations, 32);
    assert_eq!(result.sampling.standard_error, None);
}

/// The point of the batch: it must beat the independent sampler on the same
/// budget.
///
/// The iteration count is a multiple of the batch size, and the batch size
/// equals the damage-roll count. Each batch therefore covers every roll of each
/// attack exactly once. The test states a margin instead of an exact bound,
/// because a Latin hypercube pins the marginal of each dimension and not the
/// joint pair.
#[test]
fn mcts_stratified_batch_lowers_the_sampling_error() {
    let position = single_action_position();
    for player in [Player::P1, Player::P2] {
        assert_eq!(
            complete_actions(&position, player).len(),
            1,
            "{player:?} needs exactly one joint action for this test"
        );
    }

    let exact = single_action_exact_value();
    let seeds: Vec<u64> = (1..=8).collect();

    let independent = single_action_rmse(&seeds, 1, exact);
    let batched = single_action_rmse(&seeds, 16, exact);

    assert!(
        independent > 0.0,
        "the independent sampler hit the exact value on every seed"
    );
    assert!(
        batched < independent * 0.75,
        "the batch scored {batched}, the independent sampler scored {independent}"
    );
}

// ── Common random numbers and control variates ──────────────────────────────

/// The payoff matrix of [`mcts_matrix_game_finds_the_equilibrium`]. It has no
/// saddle point, so only a mixed strategy reaches its value.
fn learner_payoffs() -> Vec<Vec<f64>> {
    vec![vec![0.7, 0.2], vec![0.3, 0.6]]
}

/// The root-mean-square error of [`mcts::learn_matrix_game`] against `exact`,
/// over `seeds`.
fn matrix_rmse(
    seeds: &[u64],
    iterations: u32,
    policy: SelectionPolicy,
    control_variate: bool,
    exact: f64,
) -> f64 {
    let payoffs = learner_payoffs();
    let squares: f64 = seeds
        .iter()
        .map(|&seed| {
            let learned =
                mcts::learn_matrix_game(seed, &payoffs, iterations, policy, 0.1, control_variate);
            (learned.value - exact).powi(2)
        })
        .sum();
    (squares / seeds.len() as f64).sqrt()
}

/// The mean exploitability gap of the root strategy pair that the sampling
/// search learned on `contested_position`, over `seeds`.
///
/// Both variance controls act on the action comparison of a learner, so the
/// strategy is what they improve. Read the module documentation of
/// [`mcts`](crate::solver::mcts) for the reported value, which they do not.
fn contested_mean_gap(seeds: &[u64], config: &MctsConfig) -> f64 {
    let total: f64 = seeds
        .iter()
        .map(|&seed| {
            let result = run_mcts(seed, config);
            gap_of(
                &contested_position(),
                &result.p1_strategy,
                &result.p2_strategy,
            )
        })
        .sum();
    total / seeds.len() as f64
}

/// The control variate changes the variance of the estimate, not its
/// expectation. Both learners must therefore still reach the exact value of the
/// matrix.
#[test]
fn control_variates_keep_the_matrix_value() {
    let payoffs = learner_payoffs();
    let exact = solve_matrix_game(&payoffs).value;

    for policy in [SelectionPolicy::RegretMatching, SelectionPolicy::Exp3] {
        let learned = mcts::learn_matrix_game(9, &payoffs, 20_000, policy, 0.1, true);
        assert!(
            (learned.value - exact).abs() < 0.05,
            "{policy:?} learned {}, the exact value is {exact}",
            learned.value
        );
        for (label, strategy) in [
            ("row", &learned.row_strategy),
            ("column", &learned.col_strategy),
        ] {
            let total: f64 = strategy.iter().sum();
            assert!(
                (total - 1.0).abs() < 1e-9,
                "{policy:?} {label} strategy sums to {total}"
            );
        }
    }
}

/// The point of the baseline: it must beat the plain importance weight on the
/// same iteration budget.
///
/// The measurement uses a short run, because a long run reaches the value from
/// either estimate and hides the difference. The test states a margin instead of
/// an exact bound, because the reduction depends on how close the running mean
/// sits to the value of the action.
#[test]
fn control_variates_lower_the_matrix_error() {
    let exact = solve_matrix_game(&learner_payoffs()).value;
    let seeds: Vec<u64> = (1..=8).collect();

    let plain = matrix_rmse(&seeds, 400, SelectionPolicy::RegretMatching, false, exact);
    let corrected = matrix_rmse(&seeds, 400, SelectionPolicy::RegretMatching, true, exact);

    assert!(
        plain > 0.0,
        "the plain estimate hit the value on every seed"
    );
    assert!(
        corrected < plain,
        "the baseline scored {corrected}, the plain estimate scored {plain}"
    );
}

/// The baseline must not move the value that the search reports.
#[test]
fn control_variates_keep_the_search_value() {
    let exact = exact_value();
    let config = MctsConfig {
        control_variate: true,
        ..mcts_config()
    };

    let result = run_mcts(3, &config);

    assert!(
        (result.value - exact).abs() < 0.08,
        "the corrected search returned {}, the exact value is {exact}",
        result.value
    );
    assert!(result.stats.turns_simulated > 0);
    assert!(result.warnings.is_empty(), "{:?}", result.warnings);
}

/// A universe pool adds a seed for every node. Each one must come from the
/// seeded stream of the search, so one seed still gives one answer.
#[test]
fn common_random_numbers_repeat_under_one_seed() {
    let config = MctsConfig {
        iterations: 120,
        common_random_numbers: Some(8),
        ..mcts_config()
    };

    let first = run_mcts(77, &config);
    let second = run_mcts(77, &config);

    assert_eq!(first.value, second.value);
    assert_eq!(first.p1_strategy.len(), second.p1_strategy.len());
    for (left, right) in first.p1_strategy.iter().zip(&second.p1_strategy) {
        assert_eq!(left.probability, right.probability);
    }
    assert_eq!(first.stats.turns_simulated, second.stats.turns_simulated);
}

/// One universe keeps the law of one independent draw, so the search must still
/// reach the value that backward induction computes.
///
/// The tolerance matches `mcts_approaches_the_exact_value`, because explicit
/// exploration biases the mean by more than the sampling error alone.
#[test]
fn common_random_numbers_keep_the_search_value() {
    let exact = exact_value();
    let config = MctsConfig {
        common_random_numbers: Some(16),
        ..mcts_config()
    };

    let result = run_mcts(3, &config);

    assert!(
        (result.value - exact).abs() < 0.08,
        "the shared-universe search returned {}, the exact value is {exact}",
        result.value
    );
    assert!(result.stats.turns_simulated > 0);
    assert!(result.warnings.is_empty(), "{:?}", result.warnings);
}

/// Visits that share a universe are dependent. The independent-sample formula
/// does not give a valid standard error for this search.
#[test]
fn common_random_numbers_omit_the_standard_error() {
    for pool in [1, 8] {
        let config = MctsConfig {
            iterations: 32,
            common_random_numbers: Some(pool),
            ..mcts_config()
        };

        let result = run_mcts(77, &config);

        assert_eq!(result.sampling.iterations, 32);
        assert_eq!(result.sampling.standard_error, None, "a pool of {pool}");
    }

    // A pool of zero cannot serve a visit, so that search keeps the independent
    // draw and reports its error.
    let empty = MctsConfig {
        iterations: 32,
        common_random_numbers: Some(0),
        ..mcts_config()
    };
    assert!(run_mcts(77, &empty).sampling.standard_error.is_some());
}

/// The point of the shared universes: they must beat the independent draw on
/// the same budget.
///
/// The measured quantity is the exploitability gap of the learned strategy, not
/// the error of the reported value. A pool caps the successors of one action
/// pair, so the mean root value averages fewer distinct universes and its own
/// error grows. The measurement over these seeds records that trade: the gap
/// falls, and the error of the value does not.
///
/// The test states a margin instead of an exact bound, because the size of the
/// reduction depends on how much of the noise of one comparison comes from the
/// universe.
#[test]
fn common_random_numbers_lower_the_exploitability_gap() {
    let seeds: Vec<u64> = (1..=16).collect();
    let independent = mcts_config();
    let shared = MctsConfig {
        common_random_numbers: Some(16),
        ..independent
    };

    let plain = contested_mean_gap(&seeds, &independent);
    let common = contested_mean_gap(&seeds, &shared);

    assert!(plain > 0.0, "the independent draw learned an exact answer");
    assert!(
        common < plain,
        "the shared universes scored {common}, the independent draw scored {plain}"
    );
}

/// The baseline must also beat the plain importance weight inside the search.
///
/// This test measures the same quantity as
/// [`common_random_numbers_lower_the_exploitability_gap`], for the same reason.
#[test]
fn control_variates_lower_the_exploitability_gap() {
    let seeds: Vec<u64> = (1..=16).collect();
    let plain_config = mcts_config();
    let corrected_config = MctsConfig {
        control_variate: true,
        ..plain_config
    };

    let plain = contested_mean_gap(&seeds, &plain_config);
    let corrected = contested_mean_gap(&seeds, &corrected_config);

    assert!(plain > 0.0, "the plain estimate learned an exact answer");
    assert!(
        corrected < plain,
        "the baseline scored {corrected}, the plain estimate scored {plain}"
    );
}

// ── The leaf evaluator ──────────────────────────────────────────────────────

/// The context that every evaluator test uses.
fn eval_ctx() -> eval::EvalContext<'static> {
    let (pokemon_dex, move_dex) = dexes();
    eval::EvalContext::new(pokemon_dex, move_dex)
}

/// Reads one named feature of a position.
fn feature(state: &crate::state::battle::BattleState, name: &str) -> f64 {
    let index = eval::FEATURE_NAMES
        .iter()
        .position(|stored| *stored == name)
        .unwrap_or_else(|| panic!("no feature named {name}"));
    eval::features(state, &eval_ctx())[index]
}

/// The weights of the evaluator before this feature frame existed.
///
/// Health, status, boosts, and hazards keep their weights, and every matchup
/// weight is zero. The calibration test measures the new evaluator against this
/// baseline.
fn legacy_weights() -> eval::Features {
    let mut weights = eval::HAND_WEIGHTS;
    for value in weights.iter_mut().skip(5) {
        *value = 0.0;
    }
    weights
}

/// Exchanges the two sides of a position.
///
/// Every side-owned field moves, so a mirrored position is the same game seen
/// from the other seat.
fn mirror(state: &crate::state::battle::BattleState) -> crate::state::battle::BattleState {
    let mut out = state.clone();
    std::mem::swap(&mut out.p1_active_mons, &mut out.p2_active_mons);
    std::mem::swap(&mut out.p1_back_mons, &mut out.p2_back_mons);
    std::mem::swap(&mut out.p1_side_conditions, &mut out.p2_side_conditions);
    std::mem::swap(
        &mut out.p1_side_condition_turns,
        &mut out.p2_side_condition_turns,
    );
    std::mem::swap(&mut out.p1_slot_conditions, &mut out.p2_slot_conditions);
    std::mem::swap(&mut out.p1_has_tera, &mut out.p2_has_tera);
    std::mem::swap(&mut out.p1_has_mega, &mut out.p2_has_mega);
    out
}

/// The feature frame is antisymmetric, so a mirrored position must score one
/// minus the original. This holds for every weight vector, which is why the
/// model carries no bias term. The test therefore runs both shipped vectors.
#[test]
fn side_swap_symmetry() {
    let mut state = battle_state_from_lists(
        vec![mon(
            Species::Pikachu,
            &[PokemonMove::Thunderbolt, PokemonMove::QuickAttack],
        )],
        vec![mon(Species::Gyarados, &[PokemonMove::Waterfall])],
        vec![mon(
            Species::Snorlax,
            &[PokemonMove::BodySlam, PokemonMove::Crunch],
        )],
        vec![mon(Species::Gengar, &[PokemonMove::ShadowBall])],
    );
    state.p1_active_mons[0].hp /= 2;
    state.p2_back_mons[0].status = Some(crate::state::dex_data::Status::Burn);
    state
        .p1_side_conditions
        .push(crate::state::dex_data::SideCondition::StealthRock);
    state.p1_side_condition_turns.push(0);
    state.p2_has_tera = false;
    state.p1_active_mons[0].boosts[1] = 2;

    let flipped = mirror(&state);
    for (name, score) in [
        ("heuristic", eval::heuristic as eval::LeafEvaluator),
        ("fitted", eval::fitted as eval::LeafEvaluator),
        // The network keeps this property only because neither layer carries a
        // bias term and its activation is odd.
        ("fitted_mlp", eval::fitted_mlp as eval::LeafEvaluator),
    ] {
        let original = score(&state, &eval_ctx());
        let swapped = score(&flipped, &eval_ctx());
        assert!(
            (original + swapped - 1.0).abs() < 1e-9,
            "{name}: {original} and {swapped} do not complement"
        );
    }
}

/// Every feature sums over slots or over slot pairs, so exchanging the two
/// active slots of both sides cannot move the score.
#[test]
fn slot_order_symmetry() {
    let mut state = battle_state_from_lists(
        vec![
            mon(Species::Pikachu, &[PokemonMove::Thunderbolt]),
            mon(Species::Gyarados, &[PokemonMove::Waterfall]),
        ],
        vec![],
        vec![
            mon(Species::Snorlax, &[PokemonMove::BodySlam]),
            mon(Species::Gengar, &[PokemonMove::ShadowBall]),
        ],
        vec![],
    );
    state.p1_active_mons[0].hp /= 2;

    let mut exchanged = state.clone();
    exchanged.p1_active_mons.swap(0, 1);
    exchanged.p2_active_mons.swap(0, 1);

    let original = eval::heuristic(&state, &eval_ctx());
    let swapped = eval::heuristic(&exchanged, &eval_ctx());
    assert!(
        (original - swapped).abs() < 1e-9,
        "{original} moved to {swapped} when the slots exchanged"
    );
}

/// Two identical Pokemon differ only in what their move hits.
///
/// The old evaluator read no move data, so it scored this position at exactly
/// 0.5. The threat feature is the reason the new one does not.
#[test]
fn a_super_effective_matchup_beats_a_resisted_one() {
    // Gyarados is Water/Flying: Electric hits it for four times, Water for half.
    let state = battle_state_from_lists(
        vec![mon(Species::Gyarados, &[PokemonMove::Thunderbolt])],
        vec![],
        vec![mon(Species::Gyarados, &[PokemonMove::Waterfall])],
        vec![],
    );

    assert!(
        feature(&state, "threat") > 0.0,
        "the better matchup did not read as a larger threat"
    );
    let score = eval::heuristic(&state, &eval_ctx());
    assert!(score > 0.5, "the better matchup scored {score}");
    assert!(
        (eval::score_with(&state, &eval_ctx(), &legacy_weights()) - 0.5).abs() < 1e-9,
        "the baseline is supposed to be blind to this position"
    );
}

/// A kill that every damage branch reaches is worth strictly more than a target
/// that survives the same attack.
#[test]
fn a_guaranteed_kill_beats_a_possible_kill() {
    // Tackle cannot remove a healthy Snorlax, so only the low-HP position
    // reaches a kill.
    let build = |target_hp: Option<u16>| {
        let mut state = battle_state_from_lists(
            vec![mon(Species::Snorlax, &[PokemonMove::Tackle])],
            vec![],
            vec![mon(Species::Snorlax, &[PokemonMove::Tackle])],
            vec![],
        );
        if let Some(hp) = target_hp {
            state.p2_active_mons[0].hp = hp;
        }
        state
    };

    let healthy = build(None);
    let doomed = build(Some(1));

    assert_eq!(feature(&healthy, "guaranteed_kill"), 0.0);
    assert_eq!(feature(&doomed, "guaranteed_kill"), 1.0);
    assert_eq!(feature(&doomed, "possible_kill"), 1.0);
    assert!(eval::heuristic(&doomed, &eval_ctx()) > eval::heuristic(&healthy, &eval_ctx()));
}

/// One Speed stat decides the speed feature, and nothing else in the position
/// moves.
#[test]
fn the_faster_side_scores_higher() {
    let mut state = battle_state_from_lists(
        vec![mon(Species::Snorlax, &[PokemonMove::BodySlam])],
        vec![],
        vec![mon(Species::Snorlax, &[PokemonMove::BodySlam])],
        vec![],
    );
    assert_eq!(feature(&state, "speed"), 0.0, "a speed tie scores nobody");

    state.p1_active_mons[0].stats[5] += 20;
    assert_eq!(feature(&state, "speed"), 1.0);
    assert!(eval::heuristic(&state, &eval_ctx()) > 0.5);
}

/// Trick Room reverses the comparison, so the same position must change hands.
#[test]
fn trick_room_inverts_the_speed_feature() {
    let mut state = battle_state_from_lists(
        vec![mon(Species::Snorlax, &[PokemonMove::BodySlam])],
        vec![],
        vec![mon(Species::Snorlax, &[PokemonMove::BodySlam])],
        vec![],
    );
    state.p1_active_mons[0].stats[5] += 20;
    let plain = feature(&state, "speed");

    state
        .pseudo_weathers
        .push(crate::state::dex_data::PseudoWeather::TrickRoom);
    state.pseudo_weather_turns.push(5);

    assert_eq!(plain, 1.0);
    assert_eq!(feature(&state, "speed"), -1.0);
    assert!(eval::heuristic(&state, &eval_ctx()) < 0.5);
}

/// A Tera that a side has already spent is no longer a resource.
#[test]
fn an_unused_tera_is_worth_more_than_a_spent_one() {
    let mut state = battle_state_from_lists(
        vec![mon(Species::Snorlax, &[PokemonMove::BodySlam])],
        vec![],
        vec![mon(Species::Snorlax, &[PokemonMove::BodySlam])],
        vec![],
    );
    assert_eq!(feature(&state, "tera"), 0.0, "both sides still hold it");

    state.p2_has_tera = false;
    assert_eq!(feature(&state, "tera"), 1.0);
    assert!(eval::heuristic(&state, &eval_ctx()) > 0.5);
}

/// A Mega resource exists only while a living team member can use it.
#[test]
fn the_mega_feature_requires_an_eligible_team_member() {
    let (pokemon_dex, move_dex) = dexes();
    let build = |item| {
        build_pokemon_state(
            Species::Aerodactyl,
            pokemon_dex,
            move_dex,
            Some(50),
            Some([Some(PokemonMove::Tackle), None, None, None]),
            None,
            Some(Ability::None),
            Some(Nature::Serious),
            Some(item),
            None,
            Some([0; 6]),
            Some([31; 6]),
            false,
        )
    };
    let mut state = battle_state_from_lists(
        vec![build(Item::Aerodactylite)],
        vec![],
        vec![build(Item::None)],
        vec![],
    );
    state.p1_has_mega = true;
    state.p2_has_mega = true;

    assert_eq!(feature(&state, "mega"), 1.0);

    state.p1_active_mons[0].hp = 0;
    state.p1_active_mons[0].fainted = true;
    assert_eq!(feature(&state, "mega"), 0.0);
}

/// An attack with no PP is not a threat at the search horizon.
#[test]
fn the_threat_feature_ignores_an_exhausted_move() {
    let mut state = battle_state_from_lists(
        vec![mon(Species::Gyarados, &[PokemonMove::Thunderbolt])],
        vec![],
        vec![mon(Species::Gyarados, &[PokemonMove::Waterfall])],
        vec![],
    );
    assert!(feature(&state, "threat") > 0.0);

    state.p1_active_mons[0].move_pp[0] = 0;
    assert!(feature(&state, "threat") < 0.0);
}

/// The policy must use the Mega form for a Mega attack estimate.
#[test]
fn the_policy_scores_a_mega_attack_with_mega_stats() {
    let (pokemon_dex, move_dex) = dexes();
    let attacker = build_pokemon_state(
        Species::Aerodactyl,
        pokemon_dex,
        move_dex,
        Some(50),
        Some([Some(PokemonMove::Tackle), None, None, None]),
        None,
        Some(Ability::None),
        Some(Nature::Serious),
        Some(Item::Aerodactylite),
        None,
        Some([0; 6]),
        Some([31; 6]),
        false,
    );
    let mut state = battle_state_from_lists(
        vec![attacker],
        vec![],
        vec![mon(Species::Snorlax, &[PokemonMove::Tackle])],
        vec![],
    );
    state.p1_has_tera = false;
    state.p1_has_mega = true;

    let legal = solver_actions::joint_actions(
        &state,
        Player::P1,
        solver_actions::Phase::Normal,
        move_dex,
        pokemon_dex,
        None,
        false,
    );
    let features = |mega_evolve| {
        let action = legal
            .actions
            .iter()
            .find(|action| {
                matches!(
                    action.as_slice(),
                    [BattleCommand::Attack(attack)]
                        if attack.move_slot == 0 && attack.mega_evolve == mega_evolve
                )
            })
            .expect("the attack variant must be legal");
        eval::policy_features(&state, Player::P1, action, &eval_ctx())
    };

    let base = features(false);
    let mega = features(true);
    assert!(mega[0] > base[0], "Mega damage did not increase");
}

/// The protecting move succeeds with probability `1 / 3^streak`, so the feature
/// decays with the same rule.
#[test]
fn protect_pressure_decays_with_the_stall_counter() {
    let mut state = battle_state_from_lists(
        vec![mon(
            Species::Snorlax,
            &[PokemonMove::BodySlam, PokemonMove::Protect],
        )],
        vec![],
        vec![mon(
            Species::Snorlax,
            &[PokemonMove::BodySlam, PokemonMove::Protect],
        )],
        vec![],
    );
    assert_eq!(
        feature(&state, "protect"),
        0.0,
        "both sides can still stall"
    );

    state.p1_active_mons[0].stall_counter = 2;
    let expected = 1.0 / 9.0 - 1.0;
    assert!((feature(&state, "protect") - expected).abs() < 1e-12);
    assert!(eval::heuristic(&state, &eval_ctx()) < 0.5);
}

/// The batch entry point must agree with the scalar evaluator, and it must hand
/// over to a supplied batch pointer.
#[test]
fn score_batch_matches_the_scalar_evaluator() {
    let first = battle_state_from_lists(
        vec![mon(Species::Pikachu, &[PokemonMove::Thunderbolt])],
        vec![],
        vec![mon(Species::Gyarados, &[PokemonMove::Waterfall])],
        vec![],
    );
    let mut second = first.clone();
    second.p2_active_mons[0].hp /= 4;
    let states = [&first, &second];

    let mut out = vec![7.0; 5];
    eval::score_batch(&states, &eval_ctx(), eval::heuristic, None, &mut out);
    assert_eq!(out.len(), 2, "the loop must clear the buffer first");
    assert_eq!(out[0], eval::heuristic(&first, &eval_ctx()));
    assert_eq!(out[1], eval::heuristic(&second, &eval_ctx()));

    fn constant(
        states: &[&crate::state::battle::BattleState],
        _: &eval::EvalContext<'_>,
        out: &mut Vec<f64>,
    ) {
        out.clear();
        out.extend(std::iter::repeat_n(0.25, states.len()));
    }
    eval::score_batch(
        &states,
        &eval_ctx(),
        eval::heuristic,
        Some(constant),
        &mut out,
    );
    assert_eq!(out, vec![0.25, 0.25]);
}

/// Six positions whose health is equal and whose matchup is not.
///
/// The exact search at depth 2 gives each one a value. The baseline evaluator
/// reads no move data, so it answers 0.5 everywhere and cannot order them at
/// all. The shipped evaluator must beat it on both counts.
#[test]
fn calibration_tracks_the_exact_search() {
    let (pokemon_dex, move_dex) = dexes();
    let matchups: [(Species, PokemonMove, Species, PokemonMove); 6] = [
        (
            Species::Gyarados,
            PokemonMove::Thunderbolt,
            Species::Gyarados,
            PokemonMove::Waterfall,
        ),
        (
            Species::Snorlax,
            PokemonMove::BodySlam,
            Species::Snorlax,
            PokemonMove::BodySlam,
        ),
        (
            Species::Gengar,
            PokemonMove::ShadowBall,
            Species::Snorlax,
            PokemonMove::BodySlam,
        ),
        (
            Species::Pikachu,
            PokemonMove::Thunderbolt,
            Species::Gyarados,
            PokemonMove::Waterfall,
        ),
        (
            Species::Gyarados,
            PokemonMove::Waterfall,
            Species::Pikachu,
            PokemonMove::Thunderbolt,
        ),
        (
            Species::Snorlax,
            PokemonMove::Crunch,
            Species::Gengar,
            PokemonMove::ShadowBall,
        ),
    ];

    let config = SolveConfig {
        depth: 2,
        ..base_config()
    };
    let mut searched = Vec::new();
    let mut fitted = Vec::new();
    let mut baseline = Vec::new();
    for (p1_species, p1_move, p2_species, p2_move) in matchups {
        let battle = battle_state_from_lists(
            vec![mon(p1_species, &[p1_move])],
            vec![],
            vec![mon(p2_species, &[p2_move])],
            vec![],
        );
        let state = MatchState::BattleState(battle.clone());
        let result = solve(&state, pokemon_dex, move_dex, &config).expect("a solvable position");
        searched.push(result.value);
        fitted.push(eval::fitted(&battle, &eval_ctx()));
        baseline.push(eval::score_with(&battle, &eval_ctx(), &legacy_weights()));
    }

    let error = |scores: &[f64]| -> f64 {
        scores
            .iter()
            .zip(searched.iter())
            .map(|(score, truth)| (score - truth).abs())
            .sum::<f64>()
            / searched.len() as f64
    };
    // Concordant pairs: how often two positions are ordered as the search
    // orders them. The baseline ties every pair, so it scores zero.
    let concordance = |scores: &[f64]| -> i32 {
        let mut total = 0;
        for left in 0..scores.len() {
            for right in (left + 1)..scores.len() {
                let ours = scores[left].total_cmp(&scores[right]);
                let theirs = searched[left].total_cmp(&searched[right]);
                if ours == std::cmp::Ordering::Equal {
                    continue;
                }
                if ours == theirs {
                    total += 1;
                } else {
                    total -= 1;
                }
            }
        }
        total
    };

    assert!(
        error(&fitted) < error(&baseline),
        "fitted scored {:.4}, the baseline scored {:.4}",
        error(&fitted),
        error(&baseline)
    );
    assert!(
        concordance(&fitted) > concordance(&baseline),
        "fitted ordered {} pairs, the baseline ordered {}",
        concordance(&fitted),
        concordance(&baseline)
    );
}

/// The shipped weight files must cover every feature that the code names.
#[test]
fn the_fitted_weights_parse_and_hold_one_value_for_each_feature() {
    let values = eval::fitted_weights();
    assert_eq!(values.len(), eval::FEATURE_COUNT);
    assert!(values.iter().all(|value| value.is_finite()));

    let policy = eval::fitted_policy_weights();
    assert_eq!(policy.len(), eval::POLICY_FEATURE_COUNT);
    assert!(policy.iter().all(|value| value.is_finite()));

    // A name that the file omits keeps its hand-set fallback, which would hide
    // a training run that never touched it. Read the files directly instead.
    let value_file: eval::Weights =
        serde_json::from_str(include_str!("../../weights/eval_v1.json"))
            .expect("the value weight file parses");
    for name in eval::FEATURE_NAMES {
        assert!(value_file.get(name).is_some(), "the file omits {name}");
    }
    let policy_file: eval::Weights =
        serde_json::from_str(include_str!("../../weights/policy_v1.json"))
            .expect("the policy weight file parses");
    for name in eval::POLICY_FEATURE_NAMES {
        assert!(policy_file.get(name).is_some(), "the file omits {name}");
    }

    // The network file names its own columns, so a feature-order change cannot
    // silently reassign one.
    let network_file: eval::MlpRecord =
        serde_json::from_str(include_str!("../../weights/eval_mlp_v1.json"))
            .expect("the network weight file parses");
    for name in eval::FEATURE_NAMES {
        assert!(
            network_file.features.iter().any(|stored| stored == name),
            "the network file omits {name}"
        );
    }
    assert!(
        eval::fitted_network().is_some(),
        "the shipped network must load"
    );
}

/// The policy must put a killing move above a weak one.
#[test]
fn the_policy_ranks_a_killing_move_first() {
    let (pokemon_dex, move_dex) = dexes();
    let mut battle = battle_state_from_lists(
        vec![mon(
            Species::Gyarados,
            &[PokemonMove::Splash, PokemonMove::Thunderbolt],
        )],
        vec![],
        vec![mon(Species::Gyarados, &[PokemonMove::Waterfall])],
        vec![],
    );
    battle.p2_active_mons[0].hp = 1;

    let legal = solver_actions::joint_actions(
        &battle,
        Player::P1,
        solver_actions::Phase::Normal,
        move_dex,
        pokemon_dex,
        None,
        false,
    );
    let scores = eval::policy_scores(
        &battle,
        Player::P1,
        &legal.actions,
        &eval_ctx(),
        eval::fitted_policy_weights(),
    );
    let best = crate::solver::train::argmax(&scores).expect("a nonempty action list");

    let BattleCommand::Attack(attack) = &legal.actions[best][0] else {
        panic!("the policy chose a command that is not an attack");
    };
    assert_eq!(
        battle.p1_active_mons[0].moves[attack.move_slot],
        Some(PokemonMove::Thunderbolt),
        "the policy did not rank the killing move first"
    );
}

/// The policy must agree with the exact search more often than the coverage
/// order does.
///
/// The coverage order is the ordering that progressive widening uses without a
/// policy, so it is the baseline that the flag has to beat.
#[test]
fn policy_agreement_with_the_exact_search() {
    let (pokemon_dex, move_dex) = dexes();
    let matchups: [(Species, [PokemonMove; 2], Species, PokemonMove); 8] = [
        (
            Species::Gyarados,
            [PokemonMove::Splash, PokemonMove::Thunderbolt],
            Species::Gyarados,
            PokemonMove::Waterfall,
        ),
        (
            Species::Snorlax,
            [PokemonMove::Splash, PokemonMove::BodySlam],
            Species::Gengar,
            PokemonMove::ShadowBall,
        ),
        (
            Species::Pikachu,
            [PokemonMove::Splash, PokemonMove::Thunderbolt],
            Species::Gyarados,
            PokemonMove::Waterfall,
        ),
        (
            Species::Gengar,
            [PokemonMove::Splash, PokemonMove::ShadowBall],
            Species::Gengar,
            PokemonMove::ShadowBall,
        ),
        (
            Species::Gyarados,
            [PokemonMove::Splash, PokemonMove::Waterfall],
            Species::Snorlax,
            PokemonMove::BodySlam,
        ),
        (
            Species::Snorlax,
            [PokemonMove::Splash, PokemonMove::Crunch],
            Species::Gengar,
            PokemonMove::ShadowBall,
        ),
        (
            Species::Pikachu,
            [PokemonMove::Splash, PokemonMove::QuickAttack],
            Species::Pikachu,
            PokemonMove::Thunderbolt,
        ),
        (
            Species::Gengar,
            [PokemonMove::Splash, PokemonMove::SludgeBomb],
            Species::Gyarados,
            PokemonMove::Waterfall,
        ),
    ];

    let config = SolveConfig {
        depth: 1,
        ..base_config()
    };
    let mut policy_hits = 0;
    let mut coverage_hits = 0;

    for (p1_species, p1_moves, p2_species, p2_move) in matchups {
        let mut battle = battle_state_from_lists(
            vec![mon(p1_species, &p1_moves)],
            vec![],
            vec![mon(p2_species, &[p2_move])],
            vec![],
        );
        battle.p2_active_mons[0].hp = 1;
        // Terastallization duplicates every attack command. Both copies kill a
        // one-HP target, so the pair would tie and the comparison would measure
        // a tie break instead of a ranking.
        battle.p1_has_tera = false;
        battle.p2_has_tera = false;

        let legal = solver_actions::joint_actions(
            &battle,
            Player::P1,
            solver_actions::Phase::Normal,
            move_dex,
            pokemon_dex,
            None,
            false,
        );
        let state = MatchState::BattleState(battle.clone());
        let result = solve(&state, pokemon_dex, move_dex, &config).expect("a solvable position");
        let Some(searched) = result.most_likely_action(Player::P1) else {
            continue;
        };

        let scores = eval::policy_scores(
            &battle,
            Player::P1,
            &legal.actions,
            &eval_ctx(),
            eval::fitted_policy_weights(),
        );
        let policy_best = crate::solver::train::argmax(&scores).expect("a nonempty action list");
        if legal.actions[policy_best] == searched.commands {
            policy_hits += 1;
        }

        let order = solver_actions::coverage_order(&legal.actions);
        if legal.actions[order[0]] == searched.commands {
            coverage_hits += 1;
        }
    }

    assert!(
        policy_hits > coverage_hits,
        "the policy agreed {policy_hits} times, the coverage order agreed {coverage_hits}"
    );
}

// ── Fog-of-war ISMCTS ───────────────────────────────────────────────────────

/// One Pokemon a side, one move each, and no bench.
///
/// Both players have exactly one action, Tackle always hits, and Tackle has no
/// secondary effect. One damage roll therefore makes the turn deterministic, and
/// the sampling search must return the exact value.
fn certain_world() -> BattleState {
    let mut battle = battle_state_from_lists(
        vec![mon(Species::Pikachu, &[PokemonMove::Tackle])],
        vec![],
        vec![mon(Species::Snorlax, &[PokemonMove::Tackle])],
        vec![],
    );
    // A Tera or a Mega choice would give each player a second action.
    battle.p1_has_tera = false;
    battle.p2_has_tera = false;
    battle.p1_has_mega = false;
    battle.p2_has_mega = false;
    battle
}

/// [`certain_world`], with a chosen Speed on P2's Pokemon.
///
/// P1 cannot see the stats of P2, so two Speeds are two hidden worlds. The two
/// worlds differ in the order of the turn, which P1 does see.
fn world_with_opponent_speed(speed: u16) -> BattleState {
    let mut battle = certain_world();
    battle.p2_active_mons[0].stats[5] = speed;
    battle
}

fn ismcts_config(iterations: u32, particles: usize) -> IsmctsConfig {
    IsmctsConfig {
        search: MctsConfig {
            iterations,
            depth: 1,
            damage_rolls: 1,
            consider_crit: false,
            ..MctsConfig::default()
        },
        particles,
        resample_threshold: 0.5,
    }
}

fn belief_of_worlds(worlds: Vec<BattleState>) -> ParticleBelief {
    ParticleBelief::from_particles(
        worlds
            .into_iter()
            .map(|state| Particle {
                state: MatchState::BattleState(state),
                weight: 1.0,
            })
            .collect(),
    )
    .expect("the list is not empty")
}

/// The only legal joint action of one player in `battle`.
fn only_action(battle: &BattleState, player: Player) -> Vec<BattleCommand> {
    let (pokemon_dex, move_dex) = dexes();
    let joint = solver_actions::joint_actions(
        battle,
        player,
        solver_actions::Phase::Normal,
        move_dex,
        pokemon_dex,
        None,
        false,
    );
    assert_eq!(joint.actions.len(), 1, "{player:?} has {:?}", joint.actions);
    joint.actions[0].clone()
}

/// A belief that hides nothing is a perfect-information position. The search
/// must then return what the exact search returns.
#[test]
fn ismcts_matches_the_exact_value_without_hidden_data() {
    let (pokemon_dex, move_dex) = dexes();
    let world = certain_world();
    let exact = solve(
        &MatchState::BattleState(world.clone()),
        pokemon_dex,
        move_dex,
        &SolveConfig {
            algorithm: SolverAlgorithm::BackwardInduction,
            ..base_config()
        },
    )
    .expect("the position is playable")
    .value;

    let belief = belief_of_worlds(vec![world]);
    let result = ismcts::search(5, &belief, pokemon_dex, move_dex, &ismcts_config(16, 1))
        .expect("the position is playable");

    assert!(
        (result.value - exact).abs() < 1e-9,
        "the search returned {}, the exact value is {exact}",
        result.value
    );
    assert_eq!(result.p1_strategy.len(), 1);
    assert_eq!(result.p2_strategy.len(), 1);
    assert_eq!(result.particles, 1);
    assert!((result.effective_sample_size - 1.0).abs() < 1e-9);
    assert_eq!(result.stats.turns_simulated, 16);
    assert!(result.warnings.is_empty(), "{:?}", result.warnings);
}

/// One seed and one configuration must give one result.
#[test]
fn ismcts_is_reproducible() {
    let (pokemon_dex, move_dex) = dexes();
    let belief = belief_of_worlds(vec![
        world_with_opponent_speed(1),
        world_with_opponent_speed(200),
    ]);
    let config = ismcts_config(24, 2);

    let first = ismcts::search(11, &belief, pokemon_dex, move_dex, &config)
        .expect("the position is playable");
    let second = ismcts::search(11, &belief, pokemon_dex, move_dex, &config)
        .expect("the position is playable");

    assert_eq!(first.value, second.value);
    assert_eq!(first.stats.nodes_created, second.stats.nodes_created);
    let probabilities = |result: &ismcts::IsmctsResult| -> Vec<f64> {
        result
            .p1_strategy
            .iter()
            .chain(&result.p2_strategy)
            .map(|action| action.probability)
            .collect()
    };
    assert_eq!(probabilities(&first), probabilities(&second));
}

/// P1 cannot see the Speed of P2, but P2 knows its own Speed. Thus, P1 must use
/// one root information set and P2 must use two root information sets.
#[test]
fn ismcts_keeps_each_players_private_root_information() {
    let (pokemon_dex, move_dex) = dexes();
    let slow = world_with_opponent_speed(1);
    let fast = world_with_opponent_speed(200);
    assert_ne!(
        slow.p2_active_mons[0].stats, fast.p2_active_mons[0].stats,
        "the two worlds must differ"
    );

    let belief = belief_of_worlds(vec![slow, fast]);
    let result = ismcts::search(7, &belief, pokemon_dex, move_dex, &ismcts_config(24, 2))
        .expect("the position is playable");

    // P1 has one root. P2 has one root for each known private Speed.
    assert_eq!(
        result.stats.nodes_created, 3,
        "the search merged private information"
    );
}

/// A dominance test can read hidden target stats. It must not change the action
/// set of an information-set node.
#[test]
fn ismcts_does_not_filter_actions_with_hidden_target_data() {
    let (pokemon_dex, move_dex) = dexes();
    let mut physically_weak = battle_state_from_lists(
        vec![mon(
            Species::Pikachu,
            &[PokemonMove::WaterGun, PokemonMove::Strength],
        )],
        vec![],
        vec![mon(Species::Snorlax, &[PokemonMove::Splash])],
        vec![],
    );
    physically_weak.p1_has_tera = false;
    physically_weak.p2_has_tera = false;
    physically_weak.p1_has_mega = false;
    physically_weak.p2_has_mega = false;
    physically_weak.p2_active_mons[0].stats[2] = 1;
    physically_weak.p2_active_mons[0].stats[4] = 1_000;

    let mut specially_weak = physically_weak.clone();
    specially_weak.p2_active_mons[0].stats[2] = 1_000;
    specially_weak.p2_active_mons[0].stats[4] = 1;

    let pruned_slots = |battle: &BattleState| -> Vec<usize> {
        solver_actions::joint_actions(
            battle,
            Player::P1,
            solver_actions::Phase::Normal,
            move_dex,
            pokemon_dex,
            None,
            true,
        )
        .actions
        .iter()
        .filter_map(|commands| match &commands[0] {
            BattleCommand::Attack(attack) => Some(attack.move_slot),
            _ => None,
        })
        .collect()
    };
    assert_eq!(pruned_slots(&physically_weak), vec![1]);
    assert_eq!(pruned_slots(&specially_weak), vec![0]);

    let belief = belief_of_worlds(vec![physically_weak, specially_weak]);
    let mut config = ismcts_config(32, 2);
    config.search.prune_dominated_actions = true;
    let result = ismcts::search(17, &belief, pokemon_dex, move_dex, &config)
        .expect("the position is playable");

    let mut reported_slots: Vec<usize> = result
        .p1_strategy
        .iter()
        .filter_map(|action| match &action.commands[0] {
            BattleCommand::Attack(attack) => Some(attack.move_slot),
            _ => None,
        })
        .collect();
    reported_slots.sort_unstable();
    assert_eq!(reported_slots, vec![0, 1]);
    assert!(!result.warnings.iter().any(|warning| matches!(
        warning,
        SolveWarning::ActionsTruncated {
            player: Player::P1,
            ..
        }
    )));
}

/// Each reported strategy must be a distribution over the actions that the
/// worlds offered.
#[test]
fn ismcts_root_strategies_are_distributions() {
    let (pokemon_dex, move_dex) = dexes();
    let MatchState::BattleState(battle) = contested_position() else {
        panic!("the fixture builds a battle state");
    };
    let belief = belief_of_worlds(vec![battle]);

    let result = ismcts::search(3, &belief, pokemon_dex, move_dex, &ismcts_config(64, 1))
        .expect("the position is playable");

    for (label, strategy) in [("P1", &result.p1_strategy), ("P2", &result.p2_strategy)] {
        assert!(!strategy.is_empty(), "{label} played nothing");
        let total: f64 = strategy.iter().map(|action| action.probability).sum();
        assert!((total - 1.0).abs() < 1e-9, "{label} sums to {total}");
        for action in strategy {
            assert!(
                (0.0..=1.0).contains(&action.probability),
                "{label} played {:?} at {}",
                action.commands,
                action.probability
            );
        }
    }
    assert!((result.p1_win_odds + result.p2_win_odds - 1.0).abs() < 1e-9);
    assert!((0.0..=1.0).contains(&result.value));
    assert!(result.sampling.standard_error.is_some());
}

/// The fog-of-war search refuses the positions that the exact search refuses.
#[test]
fn ismcts_refuses_a_preview_and_a_finished_battle() {
    let (pokemon_dex, move_dex) = dexes();
    let config = ismcts_config(4, 1);

    let preview = ParticleBelief::from_particles(vec![Particle {
        state: MatchState::TeamPreviewState(small_preview()),
        weight: 1.0,
    }])
    .expect("one particle is a belief");
    assert_eq!(
        ismcts::search(1, &preview, pokemon_dex, move_dex, &config).unwrap_err(),
        ismcts::IsmctsError::Position(SolveError::TeamPreviewUnsupported)
    );

    let finished = ParticleBelief::from_particles(vec![Particle {
        state: MatchState::GameOverState {
            winner: Player::P1,
            pending_events: Vec::new(),
            final_state: Box::new(certain_world()),
        },
        weight: 1.0,
    }])
    .expect("one particle is a belief");
    assert_eq!(
        ismcts::search(1, &finished, pokemon_dex, move_dex, &config).unwrap_err(),
        ismcts::IsmctsError::Position(SolveError::GameAlreadyOver { winner: Player::P1 })
    );
}

/// The posterior update must keep the world that explains the observation, and
/// it must remove the world that does not.
///
/// The two worlds differ only in the Speed of P2, so they differ only in the
/// order of the turn. P1 sees that order.
#[test]
fn a_posterior_update_keeps_the_world_that_explains_the_turn() {
    let (pokemon_dex, move_dex) = dexes();
    let slow = world_with_opponent_speed(1);
    let fast = world_with_opponent_speed(200);
    let p1_cmd = PlayerCommand::Battle(only_action(&fast, Player::P1));
    let p2_cmd = PlayerCommand::Battle(only_action(&fast, Player::P2));
    let transition = TransitionConfig {
        consider_crit: false,
        damage_rolls: 1,
        observe: true,
    };

    // What P1 saw in the fast world.
    let observed = {
        let _guard = scoped_sample_rng(1);
        sample_transition(
            &MatchState::BattleState(fast.clone()),
            &p1_cmd,
            &p2_cmd,
            move_dex,
            pokemon_dex,
            transition,
        )
        .observations
        .expect("the config sets the observe flag")
        .p1
    };

    let mut belief = belief_of_worlds(vec![slow, fast]);
    let _guard = scoped_sample_rng(2);
    let update = belief
        .update_with_observation(
            Player::P1,
            &observed,
            &p1_cmd,
            &p2_cmd,
            pokemon_dex,
            move_dex,
            transition,
            2,
            0.0,
        )
        .expect("the fast world explains the observation");

    assert_eq!(update.matched, 1);
    assert_eq!(update.dropped, 1);
    assert_eq!(belief.len(), 1);
    assert!((update.effective_sample_size - 1.0).abs() < 1e-9);
    let MatchState::BattleState(battle) = &belief.particles()[0].state else {
        panic!("the successor is a battle state");
    };
    assert_eq!(
        battle.p2_active_mons[0].stats[5], 200,
        "the update kept the wrong world"
    );
}

/// An observation that no world produces leaves the belief alone and reports the
/// failure. A silent empty posterior would look like a confident answer.
#[test]
fn a_posterior_update_reports_an_impossible_observation() {
    let (pokemon_dex, move_dex) = dexes();
    let world = certain_world();
    let p1_cmd = PlayerCommand::Battle(only_action(&world, Player::P1));
    let p2_cmd = PlayerCommand::Battle(only_action(&world, Player::P2));
    let mut belief = belief_of_worlds(vec![world]);

    let _guard = scoped_sample_rng(4);
    let error = belief
        .update_with_observation(
            Player::P1,
            &[],
            &p1_cmd,
            &p2_cmd,
            pokemon_dex,
            move_dex,
            TransitionConfig {
                consider_crit: false,
                damage_rolls: 1,
                observe: true,
            },
            2,
            0.0,
        )
        .unwrap_err();

    assert_eq!(error, BeliefError::NoMatch { samples: 2 });
    assert_eq!(belief.len(), 1);
}

/// The complete fog-of-war entry point: a belief, the determinizer, and the
/// search. Only the species of the opponent is known, so every particle invents
/// a different build.
#[test]
fn ismcts_searches_a_species_only_belief() {
    with_meta!(meta);
    let (pokemon_dex, move_dex) = dexes();
    let belief = species_only_belief();
    let determinize = open_list_determinize_config();

    let result = ismcts::search_belief(
        20_260_805,
        &belief,
        meta,
        pokemon_dex,
        move_dex,
        &ismcts_config(24, 4),
        &determinize,
    )
    .expect("the belief is well formed");

    assert_eq!(result.particles, 4);
    assert!((0.0..=1.0).contains(&result.value));
    assert!(result.effective_sample_size > 0.0);
    assert!(result.effective_sample_size <= 4.0 + 1e-9);
    assert!(!result.p1_strategy.is_empty());
    assert!(!result.p2_strategy.is_empty());
    // P1 shares one root. P2 can have a separate root for each private build.
    assert!((2..=5).contains(&result.stats.nodes_created));
    let total: f64 = result.p1_strategy.iter().map(|a| a.probability).sum();
    assert!((total - 1.0).abs() < 1e-9, "P1 sums to {total}");
}

/// The determinizer already samples its target distribution. The particle set
/// must not apply each sampled draw probability a second time.
#[test]
fn determinized_particles_have_equal_empirical_weights() {
    with_meta!(meta);
    let (pokemon_dex, move_dex) = dexes();
    let belief = species_only_belief();
    let determinize = open_list_determinize_config();
    let seed = 20_260_805;
    let raw: Vec<f64> = (0..4)
        .map(|world| {
            determinize_seeded(
                seed + world,
                &belief,
                meta,
                pokemon_dex,
                move_dex,
                &determinize,
            )
            .expect("the belief is valid")
            .probability
        })
        .collect();
    assert!(
        raw.windows(2).any(|pair| pair[0] != pair[1]),
        "the fixture needs unequal draw probabilities: {raw:?}"
    );

    let particles =
        ParticleBelief::from_belief(seed, &belief, meta, pokemon_dex, move_dex, 4, &determinize)
            .expect("the belief is valid");
    for particle in particles.particles() {
        assert!((particle.weight - 0.25).abs() < 1e-12, "{particle:?}");
    }
}

/// A 1v1 battle belief: P1 is the observer and knows its own Pokemon, and only
/// the species of the Pokemon of P2 is known.
///
/// Mirrors `determinize_tests::belief_1v1`, which is the fixture that the
/// determinizer tests use.
fn species_only_belief() -> UnknownBattleState {
    UnknownBattleState {
        active_per_side: 1,
        back_mons_per_side: 0,
        p1_active_mons: vec![UnknownPokemonState::from_known_pokemon(&mon(
            Species::Pikachu,
            &[PokemonMove::Tackle],
        ))],
        p2_active_mons: vec![UnknownPokemonState::from_opponent_species(
            Species::Snorlax,
            pokemon_dex(),
            50,
        )],
        p1_known_back_mons: vec![],
        p2_known_back_mons: vec![],
        p1_possible_back_mons: vec![],
        p2_possible_back_mons: vec![],
        p1_fainted_mons: vec![],
        p2_fainted_mons: vec![],
        p1_unresolved_zoroark_count: 0,
        p2_unresolved_zoroark_count: 0,
        p1_roster_templates: vec![],
        p2_roster_templates: vec![],
        turn_number: 1,
        turn_started: false,
        turn_ended: false,
        p1_has_tera: false,
        p2_has_tera: false,
        p1_has_mega: false,
        p2_has_mega: false,
        weather: None,
        weather_turns: None,
        weather_setter_mon_idx: None,
        pseudo_weathers: vec![],
        pseudo_weather_turns: vec![],
        terrain: None,
        terrain_turns: None,
        terrain_setter_mon_idx: None,
        p1_side_conditions: vec![],
        p1_side_condition_turns: vec![],
        p1_side_condition_setters: vec![],
        p2_side_conditions: vec![],
        p2_side_condition_turns: vec![],
        p2_side_condition_setters: vec![],
        p1_slot_conditions: vec![vec![]],
        p2_slot_conditions: vec![vec![]],
        self_switch_pending: None,
        items_consumed_this_turn: vec![],
        last_move_on_field: None,
        sub_damage_dealt: 0,
        round_used_this_turn: false,
        predicates: vec![],
    }
}

// ── Fog-of-war MCCFR ────────────────────────────────────────────────────────

/// One Pokemon a side, two moves each, and no bench.
///
/// Both moves always hit and neither has a secondary effect, so one damage roll
/// makes every matrix cell deterministic. The exact solver therefore supplies an
/// oracle value for the same position.
fn small_contest() -> BattleState {
    let mut battle = battle_state_from_lists(
        vec![mon(
            Species::Pikachu,
            &[PokemonMove::WaterGun, PokemonMove::Strength],
        )],
        vec![],
        vec![mon(
            Species::Snorlax,
            &[PokemonMove::WaterGun, PokemonMove::Strength],
        )],
        vec![],
    );
    // A Tera or a Mega choice would double each action set.
    battle.p1_has_tera = false;
    battle.p2_has_tera = false;
    battle.p1_has_mega = false;
    battle.p2_has_mega = false;
    battle
}

fn mccfr_config(iterations: u32, particles: usize) -> MccfrConfig {
    // The default exploration rate belongs to the estimator, so the helper keeps
    // it and narrows only the cost axes.
    let base = MccfrConfig::default();
    MccfrConfig {
        search: MctsConfig {
            iterations,
            depth: 1,
            damage_rolls: 1,
            consider_crit: false,
            ..base.search
        },
        particles,
        ..base
    }
}

/// A belief that hides nothing is a perfect-information position. The search
/// must then return what the exact search returns.
#[test]
fn mccfr_matches_the_exact_value_without_hidden_data() {
    let (pokemon_dex, move_dex) = dexes();
    let world = certain_world();
    let exact = solve(
        &MatchState::BattleState(world.clone()),
        pokemon_dex,
        move_dex,
        &SolveConfig {
            algorithm: SolverAlgorithm::BackwardInduction,
            ..base_config()
        },
    )
    .expect("the position is playable")
    .value;

    let belief = belief_of_worlds(vec![world]);
    let result = mccfr::search(5, &belief, pokemon_dex, move_dex, &mccfr_config(16, 1))
        .expect("the position is playable");

    assert!(
        (result.value - exact).abs() < 1e-9,
        "the search returned {}, the exact value is {exact}",
        result.value
    );
    assert_eq!(result.p1_strategy.len(), 1);
    assert_eq!(result.p2_strategy.len(), 1);
    assert_eq!(result.particles, 1);
    assert_eq!(result.stats.turns_simulated, 16);
    assert_eq!(result.sampling.standard_error, None);
    assert!(result.warnings.is_empty(), "{:?}", result.warnings);
}

/// One seed and one configuration must give one result.
#[test]
fn mccfr_is_reproducible() {
    let (pokemon_dex, move_dex) = dexes();
    let belief = belief_of_worlds(vec![
        world_with_opponent_speed(1),
        world_with_opponent_speed(200),
    ]);
    let config = mccfr_config(24, 2);

    let first = mccfr::search(11, &belief, pokemon_dex, move_dex, &config)
        .expect("the position is playable");
    let second = mccfr::search(11, &belief, pokemon_dex, move_dex, &config)
        .expect("the position is playable");

    assert_eq!(first.value, second.value);
    assert_eq!(first.stats.nodes_created, second.stats.nodes_created);
    assert_eq!(first.horizon.len(), second.horizon.len());
    let probabilities = |result: &mccfr::MccfrResult| -> Vec<f64> {
        result
            .p1_strategy
            .iter()
            .chain(&result.p2_strategy)
            .map(|action| action.probability)
            .collect()
    };
    assert_eq!(probabilities(&first), probabilities(&second));
}

/// The point of an equilibrium baseline: the returned pair must give away less
/// than uniform play does. `exploit::exploitability` answers both pairs over the
/// complete action set of the same position.
#[test]
fn mccfr_beats_uniform_play_on_exploitability() {
    let (pokemon_dex, move_dex) = dexes();
    let battle = small_contest();
    let state = MatchState::BattleState(battle.clone());
    let config = base_config();

    let belief = belief_of_worlds(vec![battle.clone()]);
    let result = mccfr::search(23, &belief, pokemon_dex, move_dex, &mccfr_config(3_000, 1))
        .expect("the position is playable");

    let gap_of = |p1: &[JointActionProb], p2: &[JointActionProb]| -> f64 {
        exploitability(&state, pokemon_dex, move_dex, &config, p1, p2)
            .expect("the position is playable")
            .gap
    };
    let uniform = gap_of(
        &uniform_strategy(&complete_actions(&state, Player::P1)),
        &uniform_strategy(&complete_actions(&state, Player::P2)),
    );
    let learned = gap_of(&result.p1_strategy, &result.p2_strategy);

    assert!(
        learned < uniform,
        "MCCFR gave away {learned}, uniform play gave away {uniform}"
    );
}

/// The sampled value must sit near the value that the exact solver computes for
/// the same position, depth, and leaf evaluator.
#[test]
fn mccfr_agrees_with_double_oracle_on_a_small_position() {
    let (pokemon_dex, move_dex) = dexes();
    let battle = small_contest();
    let exact = solve(
        &MatchState::BattleState(battle.clone()),
        pokemon_dex,
        move_dex,
        &SolveConfig {
            algorithm: SolverAlgorithm::DoubleOracle,
            ..base_config()
        },
    )
    .expect("the position is playable")
    .value;

    let belief = belief_of_worlds(vec![battle]);
    let result = mccfr::search(31, &belief, pokemon_dex, move_dex, &mccfr_config(3_000, 1))
        .expect("the position is playable");

    assert!(
        (result.value - exact).abs() < 0.05,
        "MCCFR returned {}, double oracle returned {exact}",
        result.value
    );
}

/// P1 cannot see the Speed of P2, but P2 knows its own Speed. Thus, P1 must use
/// one root information set and P2 must use two root information sets.
#[test]
fn mccfr_keeps_each_players_private_root_information() {
    let (pokemon_dex, move_dex) = dexes();
    let belief = belief_of_worlds(vec![
        world_with_opponent_speed(1),
        world_with_opponent_speed(200),
    ]);

    let result = mccfr::search(21, &belief, pokemon_dex, move_dex, &mccfr_config(24, 2))
        .expect("the position is playable");

    // P1 has one root. P2 has one root for each known private Speed.
    assert_eq!(
        result.stats.nodes_created, 3,
        "the search merged private information"
    );
}

/// The horizon map is the leaf input of a later public-belief solve. Two hidden
/// worlds with one public stream must therefore share one key.
#[test]
fn mccfr_reports_horizon_counterfactual_values() {
    let (pokemon_dex, move_dex) = dexes();
    let mut hidden = certain_world();
    // Tackle is physical, so a different Special Defense changes no event of the
    // turn. The two worlds give one public stream and one P1 information set.
    hidden.p2_active_mons[0].stats[4] += 20;
    let belief = belief_of_worlds(vec![certain_world(), hidden]);

    let result = mccfr::search(13, &belief, pokemon_dex, move_dex, &mccfr_config(32, 2))
        .expect("the position is playable");

    assert!(!result.horizon.is_empty(), "the horizon holds nothing");
    for (key, value) in &result.horizon {
        assert!(value.visits > 0, "{key:?} holds {value:?}");
        assert!(value.reach_sum > 0.0, "{key:?} holds {value:?}");
        let counterfactual = value
            .counterfactual_value()
            .expect("a positive reach gives a value");
        assert!(
            (0.0..=1.0).contains(&counterfactual),
            "{key:?} holds {counterfactual}"
        );
    }

    let keys_of = |player: Player| -> usize {
        result
            .horizon
            .keys()
            .filter(|key| key.player == player)
            .count()
    };
    assert_eq!(keys_of(Player::P1), 1, "P1 cannot tell the worlds apart");
    assert_eq!(keys_of(Player::P2), 2, "P2 knows its own Special Defense");
}

/// An oracle over every reached public belief must replace the leaf evaluator.
/// A value of one at every leaf therefore pushes the root value to one.
#[test]
fn mccfr_reads_a_supplied_leaf_value() {
    let (pokemon_dex, move_dex) = dexes();
    let battle = small_contest();
    let belief = belief_of_worlds(vec![battle]);
    let config = mccfr_config(1_000, 1);

    // The first search reports the public beliefs that the position reaches.
    let plain = mccfr::search(41, &belief, pokemon_dex, move_dex, &config)
        .expect("the position is playable");
    let mut leaves = mccfr::HorizonLeaves::new();
    for key in plain.horizon.keys() {
        let player_value = if key.player == Player::P1 { 1.0 } else { 0.0 };
        leaves.insert(*key, player_value);
    }
    assert!(!leaves.is_empty(), "the search reached no public belief");

    let oracle =
        mccfr::search_with_leaves(41, &belief, pokemon_dex, move_dex, &config, Some(&leaves))
            .expect("the position is playable");

    assert_eq!(oracle.leaf_lookups.misses, 0, "a public belief was missing");
    assert_eq!(
        oracle.leaf_lookups.hits, 1_000,
        "each iteration reaches one leaf"
    );
    assert!(
        oracle.value > 0.9,
        "the oracle gave {}, the evaluator gave {}",
        oracle.value,
        plain.value
    );
    assert!(oracle.value > plain.value + 0.2);
}

/// A continuation value must use the private information set of each player.
/// One public belief can contain more than one private information set.
#[test]
fn continual_solve_keeps_private_horizon_information() {
    let (pokemon_dex, move_dex) = dexes();
    let mut hidden = certain_world();
    hidden.p2_active_mons[0].stats[4] += 20;
    let belief = belief_of_worlds(vec![certain_world(), hidden]);
    let config = ContinualConfig {
        root: MccfrConfig {
            horizon_worlds: 64,
            ..mccfr_config(256, 1)
        },
        continuation: mccfr_config(256, 1),
        max_subgames: None,
    };

    let result = mccfr::continual_solve(47, &belief, pokemon_dex, move_dex, &config)
        .expect("the position is playable");

    assert_eq!(result.steps.len(), 1, "the worlds have one public stream");
    let p1_keys = result
        .root
        .horizon
        .keys()
        .filter(|key| key.player == Player::P1)
        .count();
    let p2_keys = result
        .root
        .horizon
        .keys()
        .filter(|key| key.player == Player::P2)
        .count();
    assert_eq!(p1_keys, 1, "P1 cannot distinguish the hidden worlds");
    assert_eq!(p2_keys, 2, "P2 knows its private Special Defense");
    for key in result.root.horizon.keys() {
        assert!(
            result.leaves.get(*key).is_some(),
            "the continuation omitted {key:?}"
        );
    }
    assert_eq!(result.composed.leaf_lookups.misses, 0);
}

/// A missing public belief must fall back to the configured evaluator. An empty
/// oracle must therefore give the result of a plain search.
#[test]
fn mccfr_falls_back_to_the_evaluator_on_a_missing_leaf() {
    let (pokemon_dex, move_dex) = dexes();
    let belief = belief_of_worlds(vec![small_contest()]);
    let config = mccfr_config(64, 1);
    let empty = mccfr::HorizonLeaves::new();

    let plain = mccfr::search(43, &belief, pokemon_dex, move_dex, &config)
        .expect("the position is playable");
    let fallback =
        mccfr::search_with_leaves(43, &belief, pokemon_dex, move_dex, &config, Some(&empty))
            .expect("the position is playable");

    assert_eq!(fallback.value, plain.value);
    assert_eq!(fallback.leaf_lookups.hits, 0);
    assert_eq!(fallback.leaf_lookups.misses, 64);
    assert_eq!(plain.leaf_lookups.hits, 0);
    assert_eq!(plain.leaf_lookups.misses, 64);
}

/// The point of a continual solve: a root of depth one that reads solved
/// continuation values must land near a single search of depth two.
#[test]
fn mccfr_horizon_values_support_a_continual_solve() {
    let (pokemon_dex, move_dex) = dexes();
    let belief = belief_of_worlds(vec![small_contest()]);
    let config = ContinualConfig {
        root: MccfrConfig {
            horizon_worlds: 2,
            ..mccfr_config(3_000, 1)
        },
        continuation: mccfr_config(600, 1),
        max_subgames: Some(8),
    };

    let result = mccfr::continual_solve(53, &belief, pokemon_dex, move_dex, &config)
        .expect("the position is playable");

    assert!(!result.steps.is_empty(), "no continuation subgame ran");
    assert_eq!(
        result.leaves.len(),
        result.steps.len() * 2,
        "each public belief needs one value for each player"
    );
    for step in &result.steps {
        assert!(step.worlds > 0, "{step:?} holds no world");
        assert!((0.0..=1.0).contains(&step.value), "{step:?}");
    }
    assert!(
        result.composed.leaf_lookups.hits > 0,
        "the last pass read no continuation value"
    );

    // Two turns of one search are the reference for a root turn that reads the
    // value of the turn below it.
    let deep = MccfrConfig {
        search: MctsConfig {
            depth: 2,
            ..mccfr_config(3_000, 1).search
        },
        ..mccfr_config(3_000, 1)
    };
    let one_shot =
        mccfr::search(53, &belief, pokemon_dex, move_dex, &deep).expect("the position is playable");

    assert!(
        (result.composed.value - one_shot.value).abs() < 0.1,
        "the continual solve gave {}, the depth-two search gave {}",
        result.composed.value,
        one_shot.value
    );
}

/// One seed and one configuration must give one continual solve.
#[test]
fn continual_solve_is_reproducible() {
    let (pokemon_dex, move_dex) = dexes();
    let belief = belief_of_worlds(vec![small_contest()]);
    let config = ContinualConfig {
        root: MccfrConfig {
            horizon_worlds: 2,
            ..mccfr_config(200, 1)
        },
        continuation: mccfr_config(60, 1),
        max_subgames: Some(4),
    };

    let first = mccfr::continual_solve(59, &belief, pokemon_dex, move_dex, &config)
        .expect("the position is playable");
    let second = mccfr::continual_solve(59, &belief, pokemon_dex, move_dex, &config)
        .expect("the position is playable");

    assert_eq!(first.composed.value, second.composed.value);
    assert_eq!(first.root.value, second.root.value);
    assert_eq!(first.leaves, second.leaves);
    let values = |result: &mccfr::ContinualResult| -> Vec<(u64, f64)> {
        result
            .steps
            .iter()
            .map(|step| (step.public.raw(), step.value))
            .collect()
    };
    assert_eq!(values(&first), values(&second));
}

/// The second half of the baseline comparison: the sampled strategy pair must
/// give away about as much as the pair of the exact solver does.
#[test]
fn mccfr_matches_double_oracle_on_exploitability() {
    let (pokemon_dex, move_dex) = dexes();
    let battle = small_contest();
    let state = MatchState::BattleState(battle.clone());
    let config = base_config();

    let exact = solve(
        &state,
        pokemon_dex,
        move_dex,
        &SolveConfig {
            algorithm: SolverAlgorithm::DoubleOracle,
            ..config
        },
    )
    .expect("the position is playable");

    let belief = belief_of_worlds(vec![battle]);
    let sampled = mccfr::search(61, &belief, pokemon_dex, move_dex, &mccfr_config(3_000, 1))
        .expect("the position is playable");

    let gap_of = |p1: &[JointActionProb], p2: &[JointActionProb]| -> f64 {
        exploitability(&state, pokemon_dex, move_dex, &config, p1, p2)
            .expect("the position is playable")
            .gap
    };
    let exact_gap = gap_of(&exact.p1_strategy, &exact.p2_strategy);
    let sampled_gap = gap_of(&sampled.p1_strategy, &sampled.p2_strategy);
    let uniform_gap = gap_of(
        &uniform_strategy(&complete_actions(&state, Player::P1)),
        &uniform_strategy(&complete_actions(&state, Player::P2)),
    );

    assert!(
        (sampled_gap - exact_gap).abs() < 0.05,
        "MCCFR gave away {sampled_gap}, double oracle gave away {exact_gap}"
    );
    assert!(
        sampled_gap < uniform_gap && exact_gap < uniform_gap,
        "uniform play gave away {uniform_gap}"
    );
}

/// The equilibrium search refuses the positions that the exact search refuses.
#[test]
fn mccfr_refuses_a_preview_and_a_finished_battle() {
    let (pokemon_dex, move_dex) = dexes();
    let config = mccfr_config(4, 1);

    let preview = ParticleBelief::from_particles(vec![Particle {
        state: MatchState::TeamPreviewState(small_preview()),
        weight: 1.0,
    }])
    .expect("one particle is a belief");
    assert_eq!(
        mccfr::search(1, &preview, pokemon_dex, move_dex, &config).unwrap_err(),
        mccfr::MccfrError::Position(SolveError::TeamPreviewUnsupported)
    );

    let finished = ParticleBelief::from_particles(vec![Particle {
        state: MatchState::GameOverState {
            winner: Player::P1,
            pending_events: Vec::new(),
            final_state: Box::new(certain_world()),
        },
        weight: 1.0,
    }])
    .expect("one particle is a belief");
    assert_eq!(
        mccfr::search(1, &finished, pokemon_dex, move_dex, &config).unwrap_err(),
        mccfr::MccfrError::Position(SolveError::GameAlreadyOver { winner: Player::P1 })
    );
}

// ── Cancellation of the exact search ────────────────────────────────────────

/// `SolveConfig::eval` is a plain function pointer, so it cannot capture a flag.
/// The probe evaluator reads these statics instead. One lock guards the whole
/// group, so two tests never share the call count.
static PROBE_LOCK: Mutex<()> = Mutex::new(());
/// The flag that `probe_eval` raises.
static PROBE_FLAG: Mutex<Option<CancelFlag>> = Mutex::new(None);
/// The call at which `probe_eval` raises the flag. `None` never raises it.
static PROBE_LIMIT: Mutex<Option<u64>> = Mutex::new(None);
/// How many times `probe_eval` ran.
static PROBE_CALLS: AtomicU64 = AtomicU64::new(0);

/// [`eval::fitted`], with a call counter and a cancel trigger.
///
/// The returned value is the fitted value, so a solve with this evaluator
/// returns the answer that the same solve with `eval::fitted` returns.
fn probe_eval(state: &BattleState, ctx: &eval::EvalContext<'_>) -> f64 {
    let calls = PROBE_CALLS.fetch_add(1, AtomicOrdering::Relaxed) + 1;
    let limit = *PROBE_LIMIT.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(limit) = limit
        && calls >= limit
        && let Some(flag) = PROBE_FLAG
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
    {
        flag.cancel();
    }
    eval::fitted(state, ctx)
}

/// Clears the probe count and installs one flag and one trigger.
fn arm_probe(flag: Option<&CancelFlag>, limit: Option<u64>) {
    *PROBE_FLAG.lock().unwrap_or_else(|e| e.into_inner()) = flag.cloned();
    *PROBE_LIMIT.lock().unwrap_or_else(|e| e.into_inner()) = limit;
    PROBE_CALLS.store(0, AtomicOrdering::Relaxed);
}

/// A flag that nobody raises must not change the answer at all. The exact search
/// is the oracle of every other search, so a cancel hook that leaked into an
/// untouched solve would move every one of them.
#[test]
fn an_untouched_cancel_flag_does_not_change_the_value() {
    let (pokemon_dex, move_dex) = dexes();
    let state = contested_position();
    let config = SolveConfig {
        depth: 2,
        ..base_config()
    };
    let flag = CancelFlag::new();

    let plain = solve(&state, pokemon_dex, move_dex, &config).expect("solvable");
    let watched = solve_seeded_cancellable(1, &state, pokemon_dex, move_dex, &config, Some(&flag))
        .expect("solvable");

    assert_valid_strategies(&watched);
    assert_eq!(watched.value, plain.value);
    assert_eq!(watched.depth_reached, plain.depth_reached);
    assert_eq!(watched.stats.turns_simulated, plain.stats.turns_simulated);
    assert!(!watched.warnings.contains(&SolveWarning::Cancelled));
    assert!(!flag.is_cancelled());
}

/// A flag that is already raised must stop the search before it simulates a
/// turn, and it must still return a playable strategy.
#[test]
fn a_flag_set_before_the_solve_stops_the_first_pass() {
    let (pokemon_dex, move_dex) = dexes();
    let flag = CancelFlag::new();
    flag.cancel();

    let result = solve_seeded_cancellable(
        1,
        &contested_position(),
        pokemon_dex,
        move_dex,
        &base_config(),
        Some(&flag),
    )
    .expect("a cancel returns an answer, never an error");

    assert_valid_strategies(&result);
    assert_eq!(result.stats.turns_simulated, 0);
    assert_eq!(result.depth_reached, 1);
    assert!(
        result.warnings.contains(&SolveWarning::Cancelled),
        "{:?}",
        result.warnings
    );
}

/// A stop that arrives before the first double-oracle round leaves the uniform
/// strategy that the run started from. That answer says only that the search
/// learned nothing, and a uniform mixture is also a real equilibrium of some
/// positions, so the warning is the only thing that tells the two apart.
#[test]
fn a_solve_that_completes_no_round_names_its_uniform_fallback() {
    let (pokemon_dex, move_dex) = dexes();
    let flag = CancelFlag::new();
    flag.cancel();

    let result = solve_seeded_cancellable(
        1,
        &contested_position(),
        pokemon_dex,
        move_dex,
        &base_config(),
        Some(&flag),
    )
    .expect("a cancel returns an answer, never an error");

    assert!(
        result.warnings.contains(&SolveWarning::NoCompletedRound),
        "{:?}",
        result.warnings
    );
    assert!(!crate::solver::warnings_are_complete(&result.warnings));
    // The placeholder is a real distribution, so a caller can still draw from it.
    for strategy in [&result.p1_strategy, &result.p2_strategy] {
        let total: f64 = strategy.iter().map(|action| action.probability).sum();
        assert!((total - 1.0).abs() < 1e-9, "strategy sums to {total}");
    }
}

/// The mirror: an equilibrium that the search really computed must not carry
/// the warning, whatever its shape.
#[test]
fn a_finished_solve_names_no_uniform_fallback() {
    let (pokemon_dex, move_dex) = dexes();
    let config = SolveConfig {
        depth: 2,
        ..base_config()
    };

    let result = solve(&contested_position(), pokemon_dex, move_dex, &config).expect("solvable");

    assert!(
        !result.warnings.contains(&SolveWarning::NoCompletedRound),
        "{:?}",
        result.warnings
    );
    assert!(crate::solver::warnings_are_complete(&result.warnings));
}

/// A `pimc` world runs under a child flag that holds its own share of the job
/// budget. A spent job budget refuses every claim the child passes upward, so
/// no later turn can be simulated. The world search must read that and stop,
/// rather than run its whole depth on static scores and call the answer
/// complete.
#[test]
fn a_world_under_a_spent_job_budget_reports_a_stop() {
    let (pokemon_dex, move_dex) = dexes();
    let job = CancelFlag::with_simulation_turn_budget(1);
    assert!(job.claim_simulation_turn());
    assert!(!job.claim_simulation_turn());
    // A share far larger than anything the world can spend, so only the parent
    // can stop this search.
    let world = job.child_with_budget(1_000_000);
    let config = SolveConfig {
        depth: 2,
        iterative_deepening: true,
        ..base_config()
    };

    let result = solve_seeded_cancellable(
        1,
        &contested_position(),
        pokemon_dex,
        move_dex,
        &config,
        Some(&world),
    )
    .expect("a spent budget returns an answer, never an error");

    assert_eq!(
        result.stats.turns_simulated, 0,
        "the parent refuses every claim, so no turn resolves"
    );
    assert!(
        !crate::solver::warnings_are_complete(&result.warnings),
        "a static answer must never read as complete: {:?}",
        result.warnings
    );
}

/// A sampled search uses the simulation-turn budget as its work limit.
#[test]
fn an_unbounded_sampled_search_stops_at_the_simulation_turn_budget() {
    let (pokemon_dex, move_dex) = dexes();
    let flag = CancelFlag::with_simulation_turn_budget(5);
    let config = MctsConfig {
        iterations: u32::MAX,
        depth: 2,
        ..mcts_config()
    };

    let result = mcts::search_cancellable(
        1,
        &contested_position(),
        pokemon_dex,
        move_dex,
        &config,
        Some(&flag),
    )
    .expect("the budget returns an answer");

    for strategy in [&result.p1_strategy, &result.p2_strategy] {
        assert!(!strategy.is_empty());
        let total: f64 = strategy.iter().map(|action| action.probability).sum();
        assert!((total - 1.0).abs() < 1e-6, "strategy sums to {total}");
    }
    assert_eq!(flag.simulation_turns(), 5);
    assert_eq!(result.stats.turns_simulated, 5);
    assert!(
        result
            .warnings
            .contains(&SolveWarning::SimulationTurnBudgetExhausted { budget: 5 }),
        "{:?}",
        result.warnings
    );
}

/// Each active stop reason must appear in the warnings.
#[test]
fn an_expired_deadline_does_not_hide_a_cancel() {
    let (pokemon_dex, move_dex) = dexes();
    let flag = CancelFlag::new();
    flag.cancel();
    let config = SolveConfig {
        deadline: Some(Duration::ZERO),
        ..base_config()
    };

    let result = solve_seeded_cancellable(
        1,
        &contested_position(),
        pokemon_dex,
        move_dex,
        &config,
        Some(&flag),
    )
    .expect("a cancel returns an answer, never an error");

    assert_valid_strategies(&result);
    assert!(
        result.warnings.contains(&SolveWarning::Cancelled),
        "{:?}",
        result.warnings
    );
    assert!(
        result.warnings.contains(&SolveWarning::DeadlineExceeded {
            budget: Duration::ZERO
        }),
        "{:?}",
        result.warnings
    );
}

/// A cancel part way through a deepening solve must return the last depth that
/// finished before the cancel.
///
/// The trigger fires on the first leaf of pass 2, so pass 1 is complete and pass
/// 2 is not. `BudgetExhausted` must not appear, because the returned answer is a
/// complete depth-1 search rather than a part-static one.
#[test]
fn a_cancel_returns_the_newest_completed_round() {
    let _guard = PROBE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (pokemon_dex, move_dex) = dexes();
    let state = quiet_position();

    // The leaf cost of one complete depth-1 pass, under the same evaluator.
    arm_probe(None, None);
    let _shallow = solve(
        &state,
        pokemon_dex,
        move_dex,
        &SolveConfig {
            depth: 1,
            eval: probe_eval,
            ..base_config()
        },
    )
    .expect("solvable");
    let pass_one_leaves = PROBE_CALLS.load(AtomicOrdering::Relaxed);
    assert!(pass_one_leaves > 0);

    let flag = CancelFlag::new();
    arm_probe(Some(&flag), Some(pass_one_leaves + 1));
    let rounds = RefCell::new(Vec::new());
    let progress = |round: crate::solver::RootRound| rounds.borrow_mut().push(round);
    let result = solve_seeded_progress_cancellable(
        1,
        &state,
        pokemon_dex,
        move_dex,
        &SolveConfig {
            depth: 3,
            iterative_deepening: true,
            eval: probe_eval,
            ..base_config()
        },
        Some(&progress),
        Some(&flag),
    )
    .expect("a cancel returns an answer, never an error");
    arm_probe(None, None);

    assert_valid_strategies(&result);
    assert!(flag.is_cancelled());
    assert_eq!(result.depth_reached, 1);
    let rounds = rounds.into_inner();
    let newest = rounds.last().expect("the search completed one root round");
    assert_eq!(newest.depth, 1);
    assert!(
        (result.value - newest.value).abs() < 1e-12,
        "the cancel returned {}, the newest round returns {}",
        result.value,
        newest.value
    );
    assert!(
        result.warnings.contains(&SolveWarning::Cancelled),
        "{:?}",
        result.warnings
    );
    assert!(
        result.warnings.contains(&SolveWarning::DepthNotReached {
            target: 3,
            reached: 1
        }),
        "{:?}",
        result.warnings
    );
    assert!(
        !result
            .warnings
            .iter()
            .any(|w| matches!(w, SolveWarning::BudgetExhausted { .. })),
        "{:?}",
        result.warnings
    );
}

// ── Cancellation of the sampling searches ───────────────────────────────────

/// Sampled searches report only completed iteration checkpoints.
#[test]
fn sampled_searches_publish_deterministic_checkpoints() {
    let (pokemon_dex, move_dex) = dexes();
    let state = contested_position();
    let mcts_counts = RefCell::new(Vec::new());
    let mcts_progress = |root: mcts::SampledRoot| {
        mcts_counts.borrow_mut().push(root.stats.iterations);
    };
    let config = MctsConfig {
        iterations: 300,
        ..mcts_config()
    };
    mcts::search_progress_cancellable(
        9,
        &state,
        pokemon_dex,
        move_dex,
        &config,
        Some(&mcts_progress),
        None,
    )
    .expect("the position is playable");
    assert_eq!(*mcts_counts.borrow(), [1, 2, 256]);

    let belief = belief_of_worlds(vec![certain_world(), world_with_opponent_speed(60)]);
    let ismcts_counts = RefCell::new(Vec::new());
    let ismcts_progress = |root: mcts::SampledRoot| {
        ismcts_counts.borrow_mut().push(root.stats.iterations);
    };
    ismcts::search_progress_cancellable(
        9,
        &belief,
        pokemon_dex,
        move_dex,
        &ismcts_config(300, 2),
        Some(&ismcts_progress),
        None,
    )
    .expect("the belief is playable");
    assert_eq!(*ismcts_counts.borrow(), [1, 2, 256]);

    let mccfr_counts = RefCell::new(Vec::new());
    let mccfr_progress = |root: mcts::SampledRoot| {
        mccfr_counts.borrow_mut().push(root.stats.iterations);
    };
    mccfr::search_progress_cancellable(
        9,
        &belief,
        pokemon_dex,
        move_dex,
        &mccfr_config(300, 2),
        Some(&mccfr_progress),
        None,
    )
    .expect("the belief is playable");
    assert_eq!(*mccfr_counts.borrow(), [2, 256]);
}

/// A flag that nobody raises must leave each sampling search alone. A long
/// analysis job runs these three, so an unwanted early stop would show up as a
/// quietly worse strategy rather than as a failure.
#[test]
fn an_untouched_cancel_flag_leaves_each_sampling_search_alone() {
    let (pokemon_dex, move_dex) = dexes();
    let flag = CancelFlag::new();
    let state = contested_position();
    let belief = belief_of_worlds(vec![certain_world(), world_with_opponent_speed(60)]);

    let plain = mcts::search(4, &state, pokemon_dex, move_dex, &mcts_config()).expect("playable");
    let watched = mcts::search_cancellable(
        4,
        &state,
        pokemon_dex,
        move_dex,
        &mcts_config(),
        Some(&flag),
    )
    .expect("playable");
    assert_eq!(watched.value, plain.value);
    assert_eq!(watched.stats.iterations, plain.stats.iterations);
    assert!(!watched.warnings.contains(&SolveWarning::Cancelled));

    let config = ismcts_config(24, 2);
    let plain = ismcts::search(6, &belief, pokemon_dex, move_dex, &config).expect("playable");
    let watched =
        ismcts::search_cancellable(6, &belief, pokemon_dex, move_dex, &config, Some(&flag))
            .expect("playable");
    assert_eq!(watched.value, plain.value);
    assert_eq!(watched.stats.iterations, plain.stats.iterations);
    assert!(!watched.warnings.contains(&SolveWarning::Cancelled));

    let config = mccfr_config(24, 2);
    let plain = mccfr::search(6, &belief, pokemon_dex, move_dex, &config).expect("playable");
    let watched =
        mccfr::search_cancellable(6, &belief, pokemon_dex, move_dex, &config, Some(&flag))
            .expect("playable");
    assert_eq!(watched.value, plain.value);
    assert_eq!(watched.stats.iterations, plain.stats.iterations);
    assert!(!watched.warnings.contains(&SolveWarning::Cancelled));
    assert!(!flag.is_cancelled());
}

/// Checks one cancelled sampling answer.
///
/// The answer holds the mean and the strategy of the finished iterations, so
/// each strategy must still be a distribution and the value must still be a
/// probability.
fn assert_cancelled_sampling_answer(
    label: &str,
    value: f64,
    p1_strategy: &[JointActionProb],
    p2_strategy: &[JointActionProb],
    warnings: &[SolveWarning],
) {
    assert!(
        (0.0..=1.0).contains(&value),
        "{label} returned {value}, which is not a probability"
    );
    for (player, strategy) in [("P1", p1_strategy), ("P2", p2_strategy)] {
        assert!(!strategy.is_empty(), "{label} {player} strategy is empty");
        let total: f64 = strategy.iter().map(|action| action.probability).sum();
        assert!(
            (total - 1.0).abs() < 1e-6,
            "{label} {player} strategy sums to {total}"
        );
    }
    assert!(
        warnings.contains(&SolveWarning::Cancelled),
        "{label} reported {warnings:?}"
    );
}

/// A raised flag must stop each sampling search after a whole number of
/// iterations, and the answer must cover exactly those iterations.
///
/// Each search asks for far more iterations than it runs, so the finished count
/// proves that the loop stopped rather than ran to the end.
#[test]
fn a_cancelled_sampling_search_returns_its_finished_iterations() {
    let (pokemon_dex, move_dex) = dexes();
    let flag = CancelFlag::new();
    flag.cancel();
    let belief = belief_of_worlds(vec![certain_world(), world_with_opponent_speed(60)]);

    let config = MctsConfig {
        iterations: 5_000,
        ..mcts_config()
    };
    let result = mcts::search_cancellable(
        4,
        &contested_position(),
        pokemon_dex,
        move_dex,
        &config,
        Some(&flag),
    )
    .expect("a cancel returns an answer, never an error");
    assert_eq!(result.stats.iterations, 1);
    assert_eq!(result.sampling.iterations, 1);
    assert_cancelled_sampling_answer(
        "mcts",
        result.value,
        &result.p1_strategy,
        &result.p2_strategy,
        &result.warnings,
    );

    let config = ismcts_config(5_000, 2);
    let result =
        ismcts::search_cancellable(6, &belief, pokemon_dex, move_dex, &config, Some(&flag))
            .expect("a cancel returns an answer, never an error");
    assert_eq!(result.stats.iterations, 1);
    assert_eq!(result.sampling.iterations, 1);
    assert_cancelled_sampling_answer(
        "ismcts",
        result.value,
        &result.p1_strategy,
        &result.p2_strategy,
        &result.warnings,
    );

    let config = mccfr_config(5_000, 2);
    let result = mccfr::search_cancellable(6, &belief, pokemon_dex, move_dex, &config, Some(&flag))
        .expect("a cancel returns an answer, never an error");
    assert_eq!(result.stats.iterations, 2);
    assert_cancelled_sampling_answer(
        "mccfr",
        result.value,
        &result.p1_strategy,
        &result.p2_strategy,
        &result.warnings,
    );
}

/// The point of the flag for a sampling search: a cancel that arrives after the
/// search starts must stop the iteration loop part way.
///
/// A pre-set flag alone proves only the first check. The leaf evaluator raises
/// the flag here, so the raise happens inside the loop. Each search asks for 400
/// iterations and the trigger fires near iteration 40, so a finished count
/// between the two ends is what proves the mid-run stop.
#[test]
fn a_cancel_during_a_sampling_search_stops_the_iteration_loop() {
    let _guard = PROBE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (pokemon_dex, move_dex) = dexes();
    let belief = belief_of_worlds(vec![certain_world(), world_with_opponent_speed(60)]);
    let requested = 400;
    let trigger = 40;

    let flag = CancelFlag::new();
    arm_probe(Some(&flag), Some(trigger));
    let config = MctsConfig {
        iterations: requested,
        eval: probe_eval,
        ..mcts_config()
    };
    let mcts = mcts::search_cancellable(
        4,
        &contested_position(),
        pokemon_dex,
        move_dex,
        &config,
        Some(&flag),
    )
    .expect("a cancel returns an answer, never an error");

    let flag = CancelFlag::new();
    arm_probe(Some(&flag), Some(trigger));
    let base = ismcts_config(requested, 2);
    let config = IsmctsConfig {
        search: MctsConfig {
            eval: probe_eval,
            ..base.search
        },
        ..base
    };
    let ismcts =
        ismcts::search_cancellable(6, &belief, pokemon_dex, move_dex, &config, Some(&flag))
            .expect("a cancel returns an answer, never an error");

    let flag = CancelFlag::new();
    arm_probe(Some(&flag), Some(trigger));
    let base = mccfr_config(requested, 2);
    let config = MccfrConfig {
        search: MctsConfig {
            eval: probe_eval,
            ..base.search
        },
        ..base
    };
    let mccfr = mccfr::search_cancellable(6, &belief, pokemon_dex, move_dex, &config, Some(&flag))
        .expect("a cancel returns an answer, never an error");
    arm_probe(None, None);

    let answers = [
        (
            "mcts",
            mcts.stats.iterations,
            mcts.value,
            mcts.p1_strategy,
            mcts.p2_strategy,
            mcts.warnings,
        ),
        (
            "ismcts",
            ismcts.stats.iterations,
            ismcts.value,
            ismcts.p1_strategy,
            ismcts.p2_strategy,
            ismcts.warnings,
        ),
        (
            "mccfr",
            mccfr.stats.iterations,
            mccfr.value,
            mccfr.p1_strategy,
            mccfr.p2_strategy,
            mccfr.warnings,
        ),
    ];
    for (label, finished, value, p1, p2, warnings) in answers {
        assert!(
            finished > 1 && finished < u64::from(requested),
            "{label} finished {finished} of {requested}"
        );
        assert_cancelled_sampling_answer(label, value, &p1, &p2, &warnings);
    }
}

/// MCCFR alternates the traverser on the iteration index, and each player builds
/// its average on the iterations that traverse for the other player. A cancel
/// must therefore stop on a complete pair, so both averages read an equal number
/// of traversals.
#[test]
fn a_cancelled_mccfr_search_stops_on_a_complete_alternation() {
    let (pokemon_dex, move_dex) = dexes();
    let belief = belief_of_worlds(vec![certain_world(), world_with_opponent_speed(60)]);
    let flag = CancelFlag::new();
    flag.cancel();

    for iterations in [1, 2, 3, 40, 5_000] {
        let config = mccfr_config(iterations, 2);
        let result =
            mccfr::search_cancellable(6, &belief, pokemon_dex, move_dex, &config, Some(&flag))
                .expect("a cancel returns an answer, never an error");

        let finished = result.stats.iterations;
        assert!(
            finished <= u64::from(iterations),
            "{iterations} requested, {finished} finished"
        );
        if finished < u64::from(iterations) {
            assert_eq!(
                finished % 2,
                0,
                "{iterations} requested, the cancel stopped at {finished}"
            );
            assert!(
                result.warnings.contains(&SolveWarning::Cancelled),
                "{:?}",
                result.warnings
            );
        }
    }
}

/// Both entry points of each sampling search share one loop, so the plain name
/// must stay unwarned.
#[test]
fn a_plain_sampling_entry_point_never_reports_a_cancel() {
    let (pokemon_dex, move_dex) = dexes();
    let belief = belief_of_worlds(vec![certain_world()]);

    let mcts = mcts::search(
        2,
        &contested_position(),
        pokemon_dex,
        move_dex,
        &mcts_config(),
    )
    .expect("playable");
    let ismcts =
        ismcts::search(2, &belief, pokemon_dex, move_dex, &ismcts_config(16, 1)).expect("playable");
    let mccfr =
        mccfr::search(2, &belief, pokemon_dex, move_dex, &mccfr_config(16, 1)).expect("playable");

    for warnings in [&mcts.warnings, &ismcts.warnings, &mccfr.warnings] {
        assert!(!warnings.contains(&SolveWarning::Cancelled), "{warnings:?}");
    }
}

/// A deadline must also stop a turn simulation that already runs.
///
/// One `simulate_turn` call is the largest unit of work in an exact solve. A
/// Skill Link Cloyster resolves five Icicle Spear hits, and at 16 damage rolls
/// with critical hits the call dwarfs the whole rest of the search. The check
/// between two cells cannot help: the search reaches the check only after the
/// call returns.
///
/// The position gives each side one legal move, so the root holds one matrix
/// cell and the whole solve is that one turn. The test measures the same solve
/// twice, once without a limit and once with a short one. A deadline that binds
/// inside the simulation must return in a small fraction of the exact time.
fn deadline_overrun_position() -> MatchState {
    let mut cloyster = mon(Species::Cloyster, &[PokemonMove::IcicleSpear]);
    cloyster.ability = Ability::SkillLink;
    cloyster.original_ability = Some(Ability::SkillLink);
    let mut snorlax = mon(Species::Snorlax, &[PokemonMove::Splash]);
    snorlax.stats[0] = 1000;
    snorlax.hp = 1000;
    let mut battle = battle_state_from_lists(vec![cloyster], vec![], vec![snorlax], vec![]);
    // Terastallization would add a second command to each side, and the root
    // would hold four cells instead of one.
    battle.p1_has_tera = false;
    battle.p2_has_tera = false;
    MatchState::BattleState(battle)
}

/// The configuration that makes the single turn expensive.
fn deadline_overrun_config() -> SolveConfig {
    SolveConfig {
        depth: 1,
        damage_rolls: 16,
        consider_crit: true,
        chance: ChanceMode::Enumerate,
        algorithm: SolverAlgorithm::BackwardInduction,
        ..SolveConfig::default()
    }
}

#[test]
fn a_deadline_stops_a_turn_simulation_that_already_runs() {
    let (pokemon_dex, move_dex) = dexes();
    let state = deadline_overrun_position();

    let started = std::time::Instant::now();
    let exact = solve(&state, pokemon_dex, move_dex, &deadline_overrun_config())
        .expect("the position is solvable");
    let exact_elapsed = started.elapsed();
    assert_eq!(
        exact.stats.turns_simulated, 1,
        "the position must hold exactly one matrix cell"
    );

    let budget = Duration::from_millis(50);
    let started = std::time::Instant::now();
    let limited = solve(
        &state,
        pokemon_dex,
        move_dex,
        &SolveConfig {
            deadline: Some(budget),
            ..deadline_overrun_config()
        },
    )
    .expect("a deadlined solve still returns an answer");
    let limited_elapsed = started.elapsed();

    assert_valid_strategies(&limited);
    assert!(
        limited
            .warnings
            .contains(&SolveWarning::DeadlineExceeded { budget }),
        "the deadlined solve did not report the limit: {:?}",
        limited.warnings
    );
    assert_eq!(
        limited.stats.turns_simulated, 1,
        "the search must have started the turn before the abort"
    );
    assert!(
        limited_elapsed * 3 < exact_elapsed,
        "a deadline of {budget:?} must cut the cost of the exact solve; \
         exact took {exact_elapsed:?}, the deadlined solve took {limited_elapsed:?}"
    );
}

/// A cancel flag must reach the same place the deadline does. A flag that is
/// already raised leaves the search no work at all, so this test raises it from
/// another thread while the expensive turn resolves, and reads the abort by the
/// time it saved.
#[test]
fn a_cancel_stops_a_turn_simulation_that_already_runs() {
    let (pokemon_dex, move_dex) = dexes();
    let state = deadline_overrun_position();

    let started = std::time::Instant::now();
    let exact = solve(&state, pokemon_dex, move_dex, &deadline_overrun_config())
        .expect("the position is solvable");
    let exact_elapsed = started.elapsed();
    assert_eq!(exact.stats.turns_simulated, 1);

    let flag = CancelFlag::new();
    let raiser = flag.clone();
    let started = std::time::Instant::now();
    let watcher = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(50));
        raiser.cancel();
    });
    let cancelled = solve_seeded_cancellable(
        1,
        &state,
        pokemon_dex,
        move_dex,
        &deadline_overrun_config(),
        Some(&flag),
    )
    .expect("a cancelled solve still returns an answer");
    let cancelled_elapsed = started.elapsed();
    watcher.join().expect("the watcher thread finished");

    assert_valid_strategies(&cancelled);
    assert!(
        cancelled.warnings.contains(&SolveWarning::Cancelled),
        "the cancelled solve did not report the cancel: {:?}",
        cancelled.warnings
    );
    assert!(
        cancelled_elapsed * 3 < exact_elapsed,
        "a cancel must cut the cost of the exact solve; exact took {exact_elapsed:?}, \
         the cancelled solve took {cancelled_elapsed:?}"
    );
}

/// The abort signal must not outlive its `simulate_turn` call. A solve that hit
/// its deadline installs and drops one signal per cell, and a later solve on the
/// same thread must be exact again.
#[test]
fn an_abort_does_not_leak_into_the_next_solve() {
    let (pokemon_dex, move_dex) = dexes();
    let state = deadline_overrun_position();

    let limited = solve(
        &state,
        pokemon_dex,
        move_dex,
        &SolveConfig {
            deadline: Some(Duration::from_millis(50)),
            ..deadline_overrun_config()
        },
    )
    .expect("a deadlined solve still returns an answer");
    assert!(
        limited
            .warnings
            .iter()
            .any(|w| matches!(w, SolveWarning::DeadlineExceeded { .. })),
        "the first solve was meant to hit its deadline: {:?}",
        limited.warnings
    );

    let exact = solve(&state, pokemon_dex, move_dex, &deadline_overrun_config())
        .expect("the position is solvable");
    assert_eq!(
        exact.stats.turns_simulated, 1,
        "the second solve must resolve its cell"
    );
    assert!(
        exact.warnings.is_empty(),
        "a solve with no limit must warn about nothing: {:?}",
        exact.warnings
    );
}

// ── Cancellation of the preview search ──────────────────────────────────────

/// The tracker panel searches team preview, and a committed turn must stop that
/// search. A raised flag must therefore end the solve with no turn simulation,
/// and the answer must say that a cancel produced it.
#[test]
fn a_cancelled_preview_solve_reports_the_cancel() {
    let (pokemon_dex, move_dex) = dexes();
    let flag = CancelFlag::new();
    flag.cancel();
    let config = PreviewConfig {
        battle: SolveConfig {
            depth: 2,
            iterative_deepening: true,
            ..base_config()
        },
        deadline: None,
    };

    let result = solve_team_preview_cancellable(
        &small_preview(),
        pokemon_dex,
        move_dex,
        &config,
        Some(&flag),
    )
    .expect("a cancel returns an answer, never an error");

    assert!(
        result.warnings.contains(&SolveWarning::Cancelled),
        "the cancelled solve did not report the cancel: {:?}",
        result.warnings
    );
    assert_eq!(result.stats.turns_simulated, 0);
    assert_eq!(result.stats.battles_solved, 0);
    assert!((0.0..=1.0).contains(&result.value));
}

/// The flag that [`raise_the_flag_then_score`] raises.
static PREVIEW_MID_RUN_CANCEL: OnceLock<CancelFlag> = OnceLock::new();

/// Scores one position, and raises [`PREVIEW_MID_RUN_CANCEL`] first.
///
/// The battle solve below a preview cell calls this at its depth horizon, so
/// the flag rises inside a search that already runs.
fn raise_the_flag_then_score(state: &BattleState, ctx: &eval::EvalContext<'_>) -> f64 {
    PREVIEW_MID_RUN_CANCEL
        .get()
        .expect("the test installed the flag")
        .cancel();
    eval::fitted(state, ctx)
}

/// A commit cancels a preview search that already runs, so the flag has to
/// reach the battle solve below a cell, not only the cell loop above it.
#[test]
fn a_preview_solve_stops_inside_a_running_battle_solve() {
    let (pokemon_dex, move_dex) = dexes();
    let flag = PREVIEW_MID_RUN_CANCEL.get_or_init(CancelFlag::new).clone();
    assert!(!flag.is_cancelled(), "another test used the shared flag");
    let config = PreviewConfig {
        battle: SolveConfig {
            eval: raise_the_flag_then_score,
            ..base_config()
        },
        deadline: None,
    };

    let result = solve_team_preview_cancellable(
        &small_preview(),
        pokemon_dex,
        move_dex,
        &config,
        Some(&flag),
    )
    .expect("a cancel returns an answer, never an error");

    assert!(flag.is_cancelled(), "the evaluator never ran");
    assert!(
        result.warnings.contains(&SolveWarning::Cancelled),
        "the cancelled solve did not report the cancel: {:?}",
        result.warnings
    );
    // The matrix holds four cells. The flag rises inside the first one, so
    // every later cell has to take the even value with no work at all.
    assert_eq!(result.stats.cells_total, 4);
    assert_eq!(result.stats.cells_evaluated, 1, "{:?}", result.stats);
    assert!(result.stats.turns_simulated > 0, "{:?}", result.stats);
}

/// The new parameter must not change the answer of a caller that passes no flag.
#[test]
fn a_preview_solve_with_no_flag_keeps_its_value() {
    let (pokemon_dex, move_dex) = dexes();
    let state = small_preview();
    let config = preview_config();

    let plain =
        solve_team_preview(&state, pokemon_dex, move_dex, &config).expect("the state is solvable");
    let flag = CancelFlag::new();
    let watched =
        solve_team_preview_cancellable(&state, pokemon_dex, move_dex, &config, Some(&flag))
            .expect("the state is solvable");
    let unwatched = solve_team_preview_cancellable(&state, pokemon_dex, move_dex, &config, None)
        .expect("the state is solvable");

    for other in [&watched, &unwatched] {
        assert_eq!(other.value, plain.value);
        assert_eq!(other.stats.turns_simulated, plain.stats.turns_simulated);
        assert_eq!(other.stats.battles_solved, plain.stats.battles_solved);
        assert!(!other.warnings.contains(&SolveWarning::Cancelled));
    }
    assert!(!flag.is_cancelled());
}

/// The tracker searches the open-list entry point, so the flag must reach every
/// drawn world.
#[test]
fn a_cancelled_open_list_preview_reports_the_cancel() {
    with_meta!(meta);
    let (pokemon_dex, move_dex) = dexes();
    let flag = CancelFlag::new();
    flag.cancel();
    let config = OpenListConfig {
        preview: PreviewConfig {
            battle: SolveConfig {
                depth: 2,
                iterative_deepening: true,
                ..base_config()
            },
            deadline: None,
        },
        ..open_list_config(2)
    };

    let result = solve_open_list_preview_cancellable(
        &open_list_belief_1v1(),
        meta,
        pokemon_dex,
        move_dex,
        &config,
        &open_list_determinize_config(),
        Some(&flag),
    )
    .expect("a cancel returns an answer, never an error");

    assert!(
        result.warnings.contains(&SolveWarning::Cancelled),
        "the cancelled solve did not report the cancel: {:?}",
        result.warnings
    );
    assert_eq!(result.stats.turns_simulated, 0);
    assert!((0.0..=1.0).contains(&result.value));
}

// ── Replacement depth ───────────────────────────────────────────────────────

/// The unset field must return exactly the pair that the searches used before
/// the field existed: a forced child keeps the depth and adds one to the chain,
/// and everything else spends one turn and clears the chain.
#[test]
fn an_unset_replacement_depth_keeps_the_old_descent() {
    assert_eq!(
        forced_descent(solver_actions::Phase::Normal, 3, 2, 8, None),
        (2, 0)
    );
    assert_eq!(
        forced_descent(solver_actions::Phase::Replacement, 3, 2, 8, None),
        (3, 3)
    );
    assert_eq!(
        forced_descent(solver_actions::Phase::SelfSwitch, 3, 0, 8, None),
        (3, 1)
    );
    // A chain at its limit spends a turn instead.
    assert_eq!(
        forced_descent(solver_actions::Phase::Replacement, 3, 8, 8, None),
        (2, 0)
    );
}

/// A search can start while a replacement is pending. The replacement depth
/// must apply before that search expands its root.
#[test]
fn a_root_replacement_uses_the_replacement_depth() {
    assert_eq!(
        root_descent(solver_actions::Phase::Replacement, 4, Some(1)),
        (1, 0)
    );
    assert_eq!(
        root_descent(solver_actions::Phase::SelfSwitch, 1, Some(3)),
        (3, EXTENDED_FLAG)
    );
    assert_eq!(
        root_descent(solver_actions::Phase::Normal, 4, Some(1)),
        (4, 0)
    );

    let (pokemon_dex, move_dex) = dexes();
    let state = pending_replacement_position();
    let solve_with = |replacement_depth| {
        solve(
            &state,
            pokemon_dex,
            move_dex,
            &SolveConfig {
                depth: 3,
                replacement_depth,
                ..base_config()
            },
        )
        .expect("the replacement position is solvable")
    };

    let full = solve_with(None);
    let capped = solve_with(Some(1));
    assert!(
        capped.stats.turns_simulated < full.stats.turns_simulated,
        "a replacement root at depth 1 simulated {} turns, the unset field simulated {}",
        capped.stats.turns_simulated,
        full.stats.turns_simulated
    );

    let MatchState::BattleState(world) = state else {
        unreachable!("the fixture is a battle position")
    };
    let belief = belief_of_worlds(vec![world]);
    let run_ismcts = |replacement_depth| {
        ismcts::search(
            23,
            &belief,
            pokemon_dex,
            move_dex,
            &IsmctsConfig {
                search: MctsConfig {
                    iterations: 40,
                    depth: 3,
                    replacement_depth,
                    ..MctsConfig::default()
                },
                particles: 1,
                ..IsmctsConfig::default()
            },
        )
        .expect("the replacement belief is solvable")
    };
    let full = run_ismcts(None);
    let capped = run_ismcts(Some(1));
    assert!(
        capped.stats.turns_simulated < full.stats.turns_simulated,
        "ISMCTS ignored the replacement depth at its root"
    );

    let run_mccfr = |replacement_depth| {
        mccfr::search(
            29,
            &belief,
            pokemon_dex,
            move_dex,
            &MccfrConfig {
                search: MctsConfig {
                    iterations: 40,
                    depth: 3,
                    replacement_depth,
                    ..MccfrConfig::default().search
                },
                particles: 1,
                ..MccfrConfig::default()
            },
        )
        .expect("the replacement belief is solvable")
    };
    let full = run_mccfr(None);
    let capped = run_mccfr(Some(1));
    assert!(
        capped.stats.turns_simulated < full.stats.turns_simulated,
        "MCCFR ignored the replacement depth at its root"
    );
}

/// Sampling searches must also apply the replacement depth at the root.
#[test]
fn sampling_searches_use_the_root_replacement_depth() {
    let (pokemon_dex, move_dex) = dexes();
    let state = pending_replacement_position();
    let run_mcts = |replacement_depth| {
        mcts::search(
            19,
            &state,
            pokemon_dex,
            move_dex,
            &MctsConfig {
                iterations: 40,
                depth: 3,
                replacement_depth,
                ..MctsConfig::default()
            },
        )
        .expect("the replacement position is solvable")
    };

    let full = run_mcts(None);
    let capped = run_mcts(Some(1));
    assert!(
        capped.stats.turns_simulated < full.stats.turns_simulated,
        "a replacement root at depth 1 simulated {} turns, the unset field simulated {}",
        capped.stats.turns_simulated,
        full.stats.turns_simulated
    );
}

/// A value below the remaining depth makes the forced subtree smaller.
#[test]
fn a_low_replacement_depth_lowers_the_child_depth() {
    assert_eq!(
        forced_descent(solver_actions::Phase::Replacement, 4, 0, 8, Some(1)),
        (1, 1)
    );
    assert_eq!(
        forced_descent(solver_actions::Phase::SelfSwitch, 4, 0, 8, Some(2)),
        (2, 1)
    );
    // Zero would score a forced position with no decision at all.
    assert_eq!(
        forced_descent(solver_actions::Phase::Replacement, 4, 0, 8, Some(0)),
        (1, 1)
    );
}

/// A value above the remaining depth searches past the turn budget of the root.
/// One path does that one time, which keeps every path finite.
#[test]
fn a_high_replacement_depth_extends_the_horizon_one_time() {
    let (depth, chain) = forced_descent(solver_actions::Phase::Replacement, 1, 0, 8, Some(3));
    assert_eq!(depth, 3);
    assert_eq!(chain & CHAIN_MASK, 1);
    assert_eq!(chain & EXTENDED_FLAG, EXTENDED_FLAG);

    // A normal turn spends depth and keeps the flag.
    let (depth, chain) = forced_descent(solver_actions::Phase::Normal, depth, chain, 8, Some(3));
    assert_eq!(depth, 2);
    assert_eq!(chain, EXTENDED_FLAG);

    // The path already extended, so the next forced child takes the lower
    // value. Without this rule a faint could raise the depth forever.
    let (depth, chain) = forced_descent(solver_actions::Phase::Replacement, 1, chain, 8, Some(3));
    assert_eq!(depth, 1);
    assert_eq!(chain & CHAIN_MASK, 1);
    assert_eq!(chain & EXTENDED_FLAG, EXTENDED_FLAG);
}

/// The flag owns the high bit, so a large chain limit must not write into it.
#[test]
fn the_forced_chain_counter_stays_inside_its_mask() {
    let mut chain = 0;
    for expected in 1..=CHAIN_MASK {
        let (_, next) = forced_descent(
            solver_actions::Phase::Replacement,
            4,
            chain,
            u8::MAX,
            Some(2),
        );
        assert_eq!(next, expected, "the counter left its mask");
        chain = next;
    }
    // The counter is at its limit, so the next forced child spends a turn.
    let pair = forced_descent(
        solver_actions::Phase::Replacement,
        4,
        chain,
        u8::MAX,
        Some(2),
    );
    assert_eq!(pair, (3, 0));
}

/// The cost of a replacement is the reason for the field. A low value must cut
/// the work at a position whose first cell reaches a replacement node.
#[test]
fn a_low_replacement_depth_lowers_the_simulated_turns() {
    let (pokemon_dex, move_dex) = dexes();
    let state = partial_first_pass_position();

    let solve_with = |replacement_depth| {
        solve(
            &state,
            pokemon_dex,
            move_dex,
            &SolveConfig {
                depth: 3,
                replacement_depth,
                ..base_config()
            },
        )
        .expect("position is solvable")
    };

    let full = solve_with(None);
    let capped = solve_with(Some(1));

    assert_valid_strategies(&full);
    assert_valid_strategies(&capped);
    assert!(
        capped.stats.turns_simulated < full.stats.turns_simulated,
        "a replacement depth of 1 simulated {} turns, the unset field simulated {}",
        capped.stats.turns_simulated,
        full.stats.turns_simulated
    );
}

/// The opposite direction: a value above the remaining depth must search more,
/// and it must still return a legal strategy pair.
#[test]
fn a_high_replacement_depth_searches_past_the_turn_budget() {
    let (pokemon_dex, move_dex) = dexes();
    let state = partial_first_pass_position();

    let solve_with = |replacement_depth| {
        solve(
            &state,
            pokemon_dex,
            move_dex,
            &SolveConfig {
                depth: 1,
                replacement_depth,
                ..base_config()
            },
        )
        .expect("position is solvable")
    };

    let plain = solve_with(None);
    let extended = solve_with(Some(3));

    assert_valid_strategies(&extended);
    assert!(
        extended.stats.turns_simulated > plain.stats.turns_simulated,
        "a replacement depth of 3 simulated {} turns, the unset field simulated {}",
        extended.stats.turns_simulated,
        plain.stats.turns_simulated
    );
}

/// The field changes the shape of the tree, so the pruning of each algorithm
/// must still land on one value.
#[test]
fn all_algorithms_agree_with_a_replacement_depth() {
    let (pokemon_dex, move_dex) = dexes();
    let state = contested_position();

    let mut values = Vec::new();
    for algorithm in [
        SolverAlgorithm::BackwardInduction,
        SolverAlgorithm::SerializedBounds,
        SolverAlgorithm::DoubleOracle,
    ] {
        let config = SolveConfig {
            depth: 1,
            replacement_depth: Some(2),
            algorithm,
            ..base_config()
        };
        let result = solve(&state, pokemon_dex, move_dex, &config).expect("position is solvable");
        assert_valid_strategies(&result);
        values.push((algorithm, result.value));
    }

    let reference = values[0].1;
    for (algorithm, value) in &values {
        assert!(
            (value - reference).abs() < 1e-6,
            "{algorithm:?} returned {value}, backward induction returned {reference}"
        );
    }
}

/// The sampling search reads the same field, and a forced decision below the
/// horizon must not break its estimate.
#[test]
fn the_sampling_search_reads_the_replacement_depth() {
    let (pokemon_dex, move_dex) = dexes();
    let state = partial_first_pass_position();

    let result = mcts::search(
        7,
        &state,
        pokemon_dex,
        move_dex,
        &MctsConfig {
            iterations: 40,
            depth: 1,
            replacement_depth: Some(2),
            ..MctsConfig::default()
        },
    )
    .expect("position is solvable");

    assert!((0.0..=1.0).contains(&result.value));
    let total: f64 = result.p1_strategy.iter().map(|a| a.probability).sum();
    assert!((total - 1.0).abs() < 1e-6, "P1 strategy sums to {total}");
}

// ── The bounded worker pool ─────────────────────────────────────────────────

/// A position with enough structure to fill several matrix cells.
///
/// The pool only matters when the root matrix has more than one missing cell,
/// so these tests use the contested position rather than a forced one.
fn pool_config(workers: usize) -> SolveConfig {
    SolveConfig {
        depth: 2,
        workers,
        ..base_config()
    }
}

/// Compares two answers of the same position field by field.
fn assert_same_answer(serial: &SolveResult, parallel: &SolveResult) {
    assert_eq!(
        serial.value, parallel.value,
        "the pool moved the value from {} to {}",
        serial.value, parallel.value
    );
    assert_eq!(serial.depth_reached, parallel.depth_reached);
    assert_eq!(serial.warnings, parallel.warnings);
    for (label, one, two) in [
        ("P1", &serial.p1_strategy, &parallel.p1_strategy),
        ("P2", &serial.p2_strategy, &parallel.p2_strategy),
    ] {
        assert_eq!(
            one.len(),
            two.len(),
            "{label} strategy changed size: {one:?} against {two:?}"
        );
        for (a, b) in one.iter().zip(two) {
            assert_eq!(a.probability, b.probability, "{label} probability moved");
            assert_eq!(
                format!("{:?}", a.commands),
                format!("{:?}", b.commands),
                "{label} action order moved"
            );
        }
    }
}

/// The invariant of the whole pool: a worker answers cells, and it never changes
/// the answer. A cell value is exact, and a job seed comes from the identity of
/// the job, so the thread schedule cannot reach the value or either strategy.
#[test]
fn parallel_search_matches_serial_value() {
    let (pokemon_dex, move_dex) = dexes();
    let state = contested_position();

    let serial = solve(&state, pokemon_dex, move_dex, &pool_config(1)).expect("solvable");
    let parallel = solve(&state, pokemon_dex, move_dex, &pool_config(4)).expect("solvable");

    assert_valid_strategies(&parallel);
    assert_same_answer(&serial, &parallel);
    // The pool must not lose work from the counters that it merges.
    assert!(parallel.stats.turns_simulated > 0);
    if crate::solver::pool::shared().capacity() > 1 {
        // Prefetch reads cells that the serial best-response scan skips.
        assert!(
            parallel.stats.matrix_cells_evaluated > serial.stats.matrix_cells_evaluated,
            "the parallel path did not run: {} cells against {}",
            parallel.stats.matrix_cells_evaluated,
            serial.stats.matrix_cells_evaluated
        );
    } else {
        // A one-CPU process must use the supported serial fallback.
        assert_eq!(
            parallel.stats.matrix_cells_evaluated,
            serial.stats.matrix_cells_evaluated
        );
    }
}

/// A sampling chance mode keeps the serial path, so a worker count cannot move
/// its answer either. Without that rule the sampled value would depend on the
/// caches of the worker that ran the cell.
#[test]
fn parallel_search_matches_serial_under_sampling() {
    let (pokemon_dex, move_dex) = dexes();
    let state = contested_position();
    let sampled = |workers| SolveConfig {
        chance: ChanceMode::Sample(2),
        damage_rolls: 4,
        ..pool_config(workers)
    };

    let serial = solve_seeded(9, &state, pokemon_dex, move_dex, &sampled(1)).expect("solvable");
    let parallel = solve_seeded(9, &state, pokemon_dex, move_dex, &sampled(4)).expect("solvable");

    assert_valid_strategies(&parallel);
    assert_same_answer(&serial, &parallel);
    assert_eq!(serial.stats.turns_simulated, parallel.stats.turns_simulated);
}

/// A cancel that arrives inside a batch must reach the answer. Each worker
/// latches its own flag, and the batch merges those flags into the control
/// context. Every cancelled job still returns a static score, so the answer
/// stays playable.
#[test]
fn parallel_search_cancels() {
    let _guard = PROBE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (pokemon_dex, move_dex) = dexes();
    let state = contested_position();

    let flag = CancelFlag::new();
    arm_probe(Some(&flag), Some(12));
    let config = SolveConfig {
        eval: probe_eval,
        ..pool_config(4)
    };
    let result = solve_seeded_cancellable(3, &state, pokemon_dex, move_dex, &config, Some(&flag))
        .expect("a cancel returns an answer, never an error");
    arm_probe(None, None);

    assert!(flag.is_cancelled());
    assert_valid_strategies(&result);
    assert!(
        result.warnings.contains(&SolveWarning::Cancelled),
        "{:?}",
        result.warnings
    );
}

/// The node budget applies to all workers together.
#[test]
fn parallel_search_does_not_expand_past_the_node_budget() {
    let (pokemon_dex, move_dex) = dexes();
    let state = contested_position();
    let budget = 10;
    let result = solve(
        &state,
        pokemon_dex,
        move_dex,
        &SolveConfig {
            node_budget: Some(budget),
            ..pool_config(4)
        },
    )
    .expect("a budgeted solve returns an answer");

    assert!(
        result.stats.nodes_expanded <= budget,
        "the workers expanded {} nodes with a budget of {budget}",
        result.stats.nodes_expanded
    );
}

/// The permit count is what bounds the process. A request above the capacity
/// takes the capacity, and a request against an empty pool takes nothing.
#[test]
fn pool_permits_bound_the_worker_count() {
    // A private pool, so the count does not depend on the test schedule.
    let pool = WorkerPool::new(3);
    assert_eq!(pool.capacity(), 3);
    assert_eq!(pool.free(), 3);

    let held = pool.acquire(10);
    assert_eq!(held.count(), 3, "a request must not pass the capacity");
    assert_eq!(pool.free(), 0);

    let empty = pool.acquire(1);
    assert_eq!(
        empty.count(),
        0,
        "an empty pool must not block, and must lend nothing"
    );
    drop(empty);

    drop(held);
    assert_eq!(pool.free(), 3, "a dropped guard returns its permits");

    let part = pool.acquire(2);
    assert_eq!(part.count(), 2);
    let rest = pool.acquire(2);
    assert_eq!(rest.count(), 1, "a second request takes what is left");
}

/// The seed of a job must come from the identity of the job alone. A seed that
/// read the worker index or the completion order would make a sampled value
/// depend on the thread schedule.
#[test]
fn job_seed_is_stable() {
    assert_eq!(job_seed(7, 2, 1, 3, 4), job_seed(7, 2, 1, 3, 4));

    let base = job_seed(7, 2, 1, 3, 4);
    let others = [
        job_seed(8, 2, 1, 3, 4),
        job_seed(7, 3, 1, 3, 4),
        job_seed(7, 2, 2, 3, 4),
        job_seed(7, 2, 1, 4, 4),
        job_seed(7, 2, 1, 3, 5),
        // The fields must not commute. A row of 4 and a column of 3 name a
        // different cell than a row of 3 and a column of 4.
        job_seed(7, 2, 1, 4, 3),
    ];
    for (index, seed) in others.iter().enumerate() {
        assert_ne!(*seed, base, "field {index} did not reach the seed");
    }
}

// ── Perfect-information Monte Carlo ─────────────────────────────────────────
//
// `solver::pimc` solves each drawn world by itself and averages the strategies.
// The exact search is therefore its oracle: one world must return what `solve`
// returns, and two worlds must return the weighted mean of two `solve` calls.

fn pimc_config(particles: usize) -> PimcConfig {
    PimcConfig {
        solve: base_config(),
        particles,
        resample_threshold: 0.5,
    }
}

/// One active Pokemon and one bench Pokemon a side, two moves each.
///
/// Both players hold three actions: two attacks and one switch. That is enough
/// structure for a strategy that is not one pure action.
fn pimc_world(p2_moves: &[PokemonMove]) -> BattleState {
    let mut battle = battle_state_from_lists(
        vec![mon(
            Species::Pikachu,
            &[PokemonMove::Thunderbolt, PokemonMove::QuickAttack],
        )],
        vec![mon(Species::Gyarados, &[PokemonMove::Waterfall])],
        vec![mon(Species::Snorlax, p2_moves)],
        vec![mon(Species::Gengar, &[PokemonMove::ShadowBall])],
    );
    // A Tera or a Mega choice would multiply the action set of both sides.
    battle.p1_has_tera = false;
    battle.p2_has_tera = false;
    battle.p1_has_mega = false;
    battle.p2_has_mega = false;
    battle
}

/// One strategy as a map from the joint action to its probability.
///
/// The rows sort by probability, and two mixtures can order equal rows
/// differently. A map compares the content alone.
fn strategy_map(strategy: &[JointActionProb]) -> HashMap<String, f64> {
    strategy
        .iter()
        .map(|row| (format!("{:?}", row.commands), row.probability))
        .collect()
}

/// The exact answer for one world.
fn pimc_exact_of(world: &BattleState) -> SolveResult {
    let (pokemon_dex, move_dex) = dexes();
    solve(
        &MatchState::BattleState(world.clone()),
        pokemon_dex,
        move_dex,
        &base_config(),
    )
    .expect("the position is playable")
}

fn assert_mixtures_close(found: &HashMap<String, f64>, expected: &HashMap<String, f64>) {
    assert_eq!(
        found.keys().collect::<HashSet<_>>(),
        expected.keys().collect::<HashSet<_>>(),
        "the mixtures hold different actions"
    );
    for (action, probability) in expected {
        let got = found[action];
        assert!(
            (got - probability).abs() < 1e-9,
            "{action}: the mixture gave {got}, the average is {probability}"
        );
    }
}

/// A belief of one world hides nothing, so the mixture is that world's answer.
#[test]
fn pimc_with_one_world_returns_the_exact_answer() {
    let (pokemon_dex, move_dex) = dexes();
    let world = pimc_world(&[PokemonMove::BodySlam, PokemonMove::Crunch]);
    let exact = pimc_exact_of(&world);

    let belief = belief_of_worlds(vec![world]);
    let result = pimc::search(7, &belief, pokemon_dex, move_dex, &pimc_config(1))
        .expect("the position is playable");

    assert!(
        (result.value - exact.value).abs() < 1e-9,
        "the search returned {}, the exact value is {}",
        result.value,
        exact.value
    );
    assert!((result.p2_win_odds - (1.0 - exact.value)).abs() < 1e-9);
    assert_mixtures_close(
        &strategy_map(&result.p1_strategy),
        &strategy_map(&exact.p1_strategy),
    );
    assert_mixtures_close(
        &strategy_map(&result.p2_strategy),
        &strategy_map(&exact.p2_strategy),
    );
    assert_eq!(result.worlds_solved, 1);
    assert_eq!(result.particles, 1);
    assert_eq!(result.depth_reached, base_config().depth);
    // One world gives the sample variance no second point.
    assert_eq!(result.sampling.standard_error, None);
}

/// Each world enters the mixture at the weight of its particle, and the value
/// is the weighted mean of the world values.
#[test]
fn pimc_averages_the_world_answers_by_weight() {
    let (pokemon_dex, move_dex) = dexes();
    let first = pimc_world(&[PokemonMove::BodySlam, PokemonMove::Crunch]);
    let second = pimc_world(&[PokemonMove::Earthquake, PokemonMove::IceBeam]);
    let exact = [pimc_exact_of(&first), pimc_exact_of(&second)];
    assert!(
        (exact[0].value - exact[1].value).abs() > 1e-6,
        "the fixture needs two worlds that differ: {} and {}",
        exact[0].value,
        exact[1].value
    );

    // Three parts of the first world and one part of the second.
    let belief = ParticleBelief::from_particles(vec![
        Particle {
            state: MatchState::BattleState(first),
            weight: 3.0,
        },
        Particle {
            state: MatchState::BattleState(second),
            weight: 1.0,
        },
    ])
    .expect("the list is not empty");
    let result = pimc::search(7, &belief, pokemon_dex, move_dex, &pimc_config(2))
        .expect("the position is playable");

    let expected_value = 0.75 * exact[0].value + 0.25 * exact[1].value;
    assert!(
        (result.value - expected_value).abs() < 1e-9,
        "the search returned {}, the weighted mean is {expected_value}",
        result.value
    );

    for (player, found) in [
        (Player::P1, &result.p1_strategy),
        (Player::P2, &result.p2_strategy),
    ] {
        let mut expected: HashMap<String, f64> = HashMap::new();
        for (weight, answer) in [(0.75, &exact[0]), (0.25, &exact[1])] {
            let rows = match player {
                Player::P1 => &answer.p1_strategy,
                Player::P2 => &answer.p2_strategy,
            };
            for (action, probability) in strategy_map(rows) {
                *expected.entry(action).or_default() += weight * probability;
            }
        }
        assert_mixtures_close(&strategy_map(found), &expected);
    }
    assert_eq!(result.worlds_solved, 2);
}

/// Two worlds can offer different actions. The mixture must hold the union of
/// them, and it must still total one.
#[test]
fn pimc_mixes_the_union_of_the_world_actions() {
    let (pokemon_dex, move_dex) = dexes();
    // The second world gives Player 2 a third move, so it holds an action that
    // the first world does not.
    let first = pimc_world(&[PokemonMove::BodySlam, PokemonMove::Crunch]);
    let second = pimc_world(&[
        PokemonMove::BodySlam,
        PokemonMove::Crunch,
        PokemonMove::Earthquake,
    ]);
    let exact = [pimc_exact_of(&first), pimc_exact_of(&second)];

    let belief = belief_of_worlds(vec![first, second]);
    let result = pimc::search(7, &belief, pokemon_dex, move_dex, &pimc_config(2))
        .expect("the position is playable");

    let union: HashSet<String> = exact
        .iter()
        .flat_map(|answer| strategy_map(&answer.p2_strategy).into_keys())
        .collect();
    assert_eq!(
        strategy_map(&result.p2_strategy)
            .into_keys()
            .collect::<HashSet<_>>(),
        union,
        "the mixture must hold every action that a world played"
    );
    let total: f64 = result.p2_strategy.iter().map(|row| row.probability).sum();
    assert!((total - 1.0).abs() < 1e-9, "the mixture totals {total}");
}

/// The answer must name its own defect, whatever the position holds.
#[test]
fn pimc_always_reports_strategy_fusion() {
    let (pokemon_dex, move_dex) = dexes();
    let belief = belief_of_worlds(vec![
        pimc_world(&[PokemonMove::BodySlam, PokemonMove::Crunch]),
        pimc_world(&[PokemonMove::Earthquake, PokemonMove::IceBeam]),
    ]);
    let result = pimc::search(7, &belief, pokemon_dex, move_dex, &pimc_config(2))
        .expect("the position is playable");

    assert!(
        result
            .warnings
            .contains(&SolveWarning::StrategyFusion { worlds: 2 }),
        "{:?}",
        result.warnings
    );
}

/// One world must not spend the budget of the whole job. Each world takes an
/// equal share, so a small budget still reaches every world.
#[test]
fn pimc_splits_the_simulation_budget_between_the_worlds() {
    let (pokemon_dex, move_dex) = dexes();
    let worlds = 4;
    let budget = 8;
    let world = || pimc_world(&[PokemonMove::BodySlam, PokemonMove::Crunch]);

    // One world of this position costs more than the whole budget, so an unsplit
    // budget would leave the later worlds unsearched.
    let single = pimc::search(
        7,
        &belief_of_worlds(vec![world()]),
        pokemon_dex,
        move_dex,
        &pimc_config(1),
    )
    .expect("the position is playable");
    assert!(
        single.stats.turns_simulated > budget,
        "the fixture needs a world that costs more than {budget} turns, it cost {}",
        single.stats.turns_simulated
    );

    let belief = belief_of_worlds((0..worlds).map(|_| world()).collect());
    let flag = CancelFlag::with_simulation_turn_budget(budget);
    let result = pimc::search_progress_cancellable(
        7,
        &belief,
        pokemon_dex,
        move_dex,
        &pimc_config(worlds),
        None,
        Some(&flag),
    )
    .expect("the position is playable");

    assert_eq!(
        result.worlds_solved, worlds,
        "every world must get its own share of the budget"
    );
    assert!(
        flag.simulation_turns() <= budget,
        "the job simulated {} turns of a {budget}-turn budget",
        flag.simulation_turns()
    );
    assert!(
        result
            .warnings
            .contains(&SolveWarning::SimulationTurnBudgetExhausted { budget }),
        "the answer must name the job budget, not the share of one world: {:?}",
        result.warnings
    );
}

/// A raised flag stops the search between two worlds, and the answer then holds
/// the worlds that finished.
#[test]
fn pimc_returns_the_worlds_that_finished_after_a_cancel() {
    let (pokemon_dex, move_dex) = dexes();
    let belief = belief_of_worlds(
        (0..3)
            .map(|_| pimc_world(&[PokemonMove::BodySlam, PokemonMove::Crunch]))
            .collect(),
    );
    let flag = CancelFlag::new();
    flag.cancel();

    let result = pimc::search_progress_cancellable(
        7,
        &belief,
        pokemon_dex,
        move_dex,
        &pimc_config(3),
        None,
        Some(&flag),
    )
    .expect("the position is playable");

    // World one always runs, so the mixture is never empty.
    assert_eq!(result.worlds_solved, 1);
    assert!(result.warnings.contains(&SolveWarning::Cancelled));
    let total: f64 = result.p1_strategy.iter().map(|row| row.probability).sum();
    assert!((total - 1.0).abs() < 1e-9, "the mixture totals {total}");
}

/// A caller reads one answer for each world that the search finishes.
#[test]
fn pimc_publishes_one_answer_for_each_world() {
    let (pokemon_dex, move_dex) = dexes();
    let belief = belief_of_worlds(vec![
        pimc_world(&[PokemonMove::BodySlam, PokemonMove::Crunch]),
        pimc_world(&[PokemonMove::Earthquake, PokemonMove::IceBeam]),
    ]);
    let rounds = Mutex::new(Vec::new());
    let hook = |round: crate::solver::RootRound| {
        let total: f64 = round.p1_strategy.iter().map(|row| row.probability).sum();
        rounds.lock().unwrap().push((round.value, total));
    };

    let result = pimc::search_progress_cancellable(
        7,
        &belief,
        pokemon_dex,
        move_dex,
        &pimc_config(2),
        Some(&hook),
        None,
    )
    .expect("the position is playable");

    let published = rounds.lock().unwrap().clone();
    assert_eq!(published.len(), 2, "one answer for each world");
    for (value, total) in &published {
        assert!((0.0..=1.0).contains(value), "the value is {value}");
        assert!(
            (total - 1.0).abs() < 1e-9,
            "a published mixture totals {total}"
        );
    }
    // The last answer is the answer of the whole search.
    assert!((published[1].0 - result.value).abs() < 1e-9);
}

/// A set that holds a position with no decision must fail as a whole, as the
/// other belief searches do.
#[test]
fn pimc_refuses_a_preview_world_and_a_finished_world() {
    let (pokemon_dex, move_dex) = dexes();
    let config = pimc_config(1);

    let preview = ParticleBelief::from_particles(vec![Particle {
        state: MatchState::TeamPreviewState(small_preview()),
        weight: 1.0,
    }])
    .expect("one particle is a belief");
    assert_eq!(
        pimc::search(1, &preview, pokemon_dex, move_dex, &config).unwrap_err(),
        pimc::PimcError::Position(SolveError::TeamPreviewUnsupported)
    );

    let finished = ParticleBelief::from_particles(vec![Particle {
        state: MatchState::GameOverState {
            winner: Player::P1,
            pending_events: Vec::new(),
            final_state: Box::new(certain_world()),
        },
        weight: 1.0,
    }])
    .expect("one particle is a belief");
    assert_eq!(
        pimc::search(1, &finished, pokemon_dex, move_dex, &config).unwrap_err(),
        pimc::PimcError::Position(SolveError::GameAlreadyOver { winner: Player::P1 })
    );
}

/// The draw and every world solve read the seed alone, so one seed gives one
/// answer.
#[test]
fn pimc_is_deterministic_in_its_seed() {
    with_meta!(meta);
    let (pokemon_dex, move_dex) = dexes();
    let belief = species_only_belief();
    let determinize = open_list_determinize_config();
    let config = pimc_config(3);

    let run = |seed: u64| {
        pimc::search_belief(
            seed,
            &belief,
            meta,
            pokemon_dex,
            move_dex,
            &config,
            &determinize,
        )
        .expect("the belief is valid")
    };
    let first = run(4_242);
    let second = run(4_242);

    assert_eq!(first.value, second.value);
    assert_eq!(first.worlds_solved, 3);
    assert_eq!(
        strategy_map(&first.p1_strategy),
        strategy_map(&second.p1_strategy)
    );
    assert_eq!(
        strategy_map(&first.p2_strategy),
        strategy_map(&second.p2_strategy)
    );
    assert!((first.effective_sample_size - 3.0).abs() < 1e-9);
}
