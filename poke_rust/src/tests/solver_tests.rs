//! Tests the perfect-information solver.
//!
//! [`all_algorithms_agree`] compares all search algorithms.
//! A different value identifies an invalid cutoff.
//! `solver::matrix` contains equilibrium tests.
//!
//! Debug builds make `simulate_turn` about ten times slower.
//! Tests therefore use few moves, short benches, and one damage roll.

use std::collections::HashMap;

use crate::data::ability::Ability;
use crate::data::pokemon_move::PokemonMove;
use crate::data::species::Species;
use crate::solver::chance::ChanceMode;
use crate::solver::{
    SolveConfig, SolveError, SolveResult, SolveWarning, SolverAlgorithm, eval, solve, solve_seeded,
};
use crate::state::battle::{BattleCommand, MatchState, Player};
use crate::state::dex_data::{MoveData, PokemonData};
use crate::state::pokemon::{Nature, PokemonState, build_pokemon_state};
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
