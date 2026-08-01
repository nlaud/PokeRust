//! Tests the perfect-information solver.
//!
//! [`all_algorithms_agree`] compares all search algorithms.
//! A different value identifies an invalid cutoff.
//! `solver::matrix` contains equilibrium tests.
//!
//! Debug builds make `simulate_turn` about ten times slower.
//! Tests therefore use few moves, short benches, and one damage roll.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use crate::data::ability::Ability;
use crate::data::pokemon_move::PokemonMove;
use crate::data::species::Species;
use crate::solver::chance::ChanceMode;
use crate::solver::matrix::solve_matrix_game;
use crate::solver::preview::{
    PreviewCellCache, PreviewConfig, precompute_preview_cells, preview_cell_value, preview_choices,
    solve_team_preview, solve_team_preview_cached,
};
use crate::solver::{
    SolveConfig, SolveError, SolveResult, SolveWarning, SolverAlgorithm, eval, solve, solve_seeded,
};
use crate::state::battle::{BattleCommand, BattleMechanics, MatchState, Player, TeamPreviewState};
use crate::state::dex_data::{MoveData, PokemonData, VolatileStatus, parse_move_dex};
use crate::state::pokemon::{Nature, PokemonState, VolatileStatusState, build_pokemon_state};
use crate::tests::simuilator_test_helpers::{battle_state_from_lists, move_dex, pokemon_dex};

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
        vec![mon(Species::Pikachu, &[PokemonMove::Thunderbolt, PokemonMove::QuickAttack])],
        vec![mon(Species::Gyarados, &[PokemonMove::Waterfall, PokemonMove::IceFang])],
        vec![mon(Species::Snorlax, &[PokemonMove::BodySlam, PokemonMove::Crunch])],
        vec![mon(Species::Gengar, &[PokemonMove::ShadowBall, PokemonMove::SludgeBomb])],
    ))
}

fn assert_valid_strategies(result: &SolveResult) {
    for (label, strategy) in [
        ("P1", &result.p1_strategy),
        ("P2", &result.p2_strategy),
    ] {
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
        (Species::Pikachu, [PokemonMove::Thunderbolt, PokemonMove::QuickAttack]),
        (Species::Snorlax, [PokemonMove::BodySlam, PokemonMove::Crunch]),
        (Species::Gengar, [PokemonMove::ShadowBall, PokemonMove::SludgeBomb]),
        (Species::Gyarados, [PokemonMove::Waterfall, PokemonMove::IceFang]),
        (Species::Machamp, [PokemonMove::CloseCombat, PokemonMove::Earthquake]),
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
        vec![mon(Species::Snorlax, &[PokemonMove::Splash, PokemonMove::Rest])],
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
        vec![mon(Species::Machamp, &[PokemonMove::CloseCombat, PokemonMove::Splash])],
        vec![],
        vec![mon(Species::Abra, &[PokemonMove::Splash, PokemonMove::Teleport])],
        vec![],
    ));
    let p2_favoured = MatchState::BattleState(battle_state_from_lists(
        vec![mon(Species::Abra, &[PokemonMove::Splash, PokemonMove::Teleport])],
        vec![],
        vec![mon(Species::Machamp, &[PokemonMove::CloseCombat, PokemonMove::Splash])],
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
        vec![mon(Species::Abra, &[PokemonMove::Splash, PokemonMove::Teleport])],
        vec![],
        vec![mon(Species::Abra, &[PokemonMove::Splash, PokemonMove::Teleport])],
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
    let mut battle = battle_state_from_lists(
        vec![mon(Species::Pikachu, &[PokemonMove::Thunderbolt, PokemonMove::QuickAttack])],
        vec![
            mon(Species::Gyarados, &[PokemonMove::Waterfall]),
            mon(Species::Gengar, &[PokemonMove::ShadowBall]),
        ],
        vec![mon(Species::Snorlax, &[PokemonMove::BodySlam, PokemonMove::Crunch])],
        vec![],
    );
    battle.p1_active_mons[0].hp = 0;
    battle.p1_active_mons[0].fainted = true;
    battle.turn_started = true;
    battle.turn_ended = true;

    let result = solve(
        &MatchState::BattleState(battle),
        pokemon_dex,
        move_dex,
        &base_config(),
    )
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
        vec![mon(Species::Abra, &[PokemonMove::Splash, PokemonMove::Teleport])],
        vec![],
        vec![mon(Species::Abra, &[PokemonMove::Splash, PokemonMove::Teleport])],
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
        Err(SolveError::GameAlreadyOver {
            winner: Player::P1
        })
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
                    preview_cell_value(
                        &state, pokemon_dex, move_dex, &config, p1_choice, p2_choice,
                    )
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

    for (label, strategy) in [
        ("P1", &result.p1_strategy),
        ("P2", &result.p2_strategy),
    ] {
        let total: f64 = strategy.iter().map(|choice| choice.probability).sum();
        assert!((total - 1.0).abs() < 1e-6, "{label} strategy sums to {total}");
        assert!(!strategy.is_empty(), "{label} strategy is empty");
    }
    assert!((result.p1_win_odds + result.p2_win_odds - 1.0).abs() < 1e-9);
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
    let changed = solve_team_preview_cached(
        &state,
        pokemon_dex,
        &changed_move_dex,
        &config,
        &mut cache,
    )
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
