//! Tests the perfect-information solver.
//!
//! [`all_algorithms_agree`] compares all search algorithms.
//! A different value identifies an invalid cutoff.
//! `solver::matrix` contains equilibrium tests.
//!
//! Debug builds make `simulate_turn` about ten times slower.
//! Tests therefore use few moves, short benches, and one damage roll.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::OnceLock;
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
use crate::solver::exploit::exploitability;
use crate::solver::ismcts::{self, IsmctsConfig};
use crate::solver::matrix::solve_matrix_game;
use crate::solver::mccfr::{self, MccfrConfig};
use crate::solver::mcts::{self, MctsConfig, SelectionPolicy, TransitionMode, Widening};
use crate::solver::preview::{
    OpenListConfig, OpenListError, PreviewCellCache, PreviewConfig, open_list_worlds,
    precompute_preview_cells, preview_cell_value, preview_choices, solve_open_list_preview,
    solve_team_preview, solve_team_preview_cached,
};
use crate::solver::{
    JointActionProb, SolveConfig, SolveError, SolveResult, SolveWarning, SolverAlgorithm, eval,
    solve, solve_seeded,
};
use crate::state::battle::{
    BattleCommand, BattleMechanics, BattleState, MatchState, Player, PlayerCommand,
    TeamPreviewState,
};
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
    LEARNSETS
        .get_or_init(|| crate::state::dex_data::parse_learnset_dex("../pokemon_info/showdownLearnsets.txt"))
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
        assert!(world.probability > 0.0, "world {index} has zero probability");
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

    let worlds = open_list_worlds(
        &belief,
        meta,
        pokemon_dex,
        move_dex,
        &config,
        &determinize,
    )
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

    let result = solve_open_list_preview(
        &belief,
        meta,
        pokemon_dex,
        move_dex,
        &config,
        &determinize,
    )
    .expect("the belief is well formed");

    assert!(
        (result.value - reference.value).abs() < 1e-6,
        "the open-list solve returned {}, the mean matrix returned {}",
        result.value,
        reference.value
    );
    assert_eq!(result.stats.cells_total, (p1_choices.len() * p2_choices.len()) as u64);
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
        assert!((total - 1.0).abs() < 1e-6, "{label} strategy sums to {total}");
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
        assert!((total - 1.0).abs() < 1e-6, "{label} strategy sums to {total}");
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
        SolveError::GameAlreadyOver {
            winner: Player::P1
        }
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
            mon(Species::Gyarados, &[PokemonMove::Waterfall, PokemonMove::IceFang]),
            mon(Species::Blastoise, &[PokemonMove::Surf, PokemonMove::IceBeam]),
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
            mon(Species::Gengar, &[PokemonMove::ShadowBall, PokemonMove::SludgeBomb]),
            mon(Species::Alakazam, &[PokemonMove::Psychic, PokemonMove::Recover]),
        ],
    ))
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
    assert_eq!(widening.allowed(0, 2), 2, "the count never exceeds the total");
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

    assert!(plain > 0.0, "the plain estimate hit the value on every seed");
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
    assert_eq!(feature(&state, "protect"), 0.0, "both sides can still stall");

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
        ismcts::IsmctsError::Position(SolveError::GameAlreadyOver {
            winner: Player::P1
        })
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
        battle.p2_active_mons[0].stats[5],
        200,
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

    let particles = ParticleBelief::from_belief(
        seed,
        &belief,
        meta,
        pokemon_dex,
        move_dex,
        4,
        &determinize,
    )
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
        mccfr::MccfrError::Position(SolveError::GameAlreadyOver {
            winner: Player::P1
        })
    );
}
