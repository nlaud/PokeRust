//! Tests the usage-driven determinizer.
//!
//! The math modules contain their own unit tests.
//! This file tests the complete determinizer:
//!
//! 1. Confirm that each world satisfies its source belief.
//! 2. Run each world through `sample_turn_raw_seeded`.
//! 3. Compare sampled distributions with usage data.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::OnceLock;

use crate::data::ability::Ability;
use crate::data::item::Item;
use crate::data::pokemon_move::PokemonMove;
use crate::data::species::Species;
use crate::information::determinize::{
    DeterminizeConfig, DeterminizeError, DeterminizeWarning, DrawLog, check_determinization,
    determinize, determinize_pokemon, determinize_seeded,
};
use crate::information::inference::InferenceConfig;
use crate::information::subset_check::collect_true_state_subset_violations;
use crate::information::unknowns::{
    PokemonHP, Unknown, UnknownBattleState, UnknownMatchState, UnknownPokemonState,
};
use crate::meta::{MetaDex, MetaFormat};
use crate::simulator::sample_turn_raw_seeded;
use crate::state::battle::{FieldSlot, MatchState, Player, PlayerCommand};
use crate::state::dex_data::PokemonType;
use crate::state::pokemon::{Nature, PokemonGender, PokemonState, nature_stat_modifiers};
use crate::tests::simuilator_test_helpers::{move_dex, pokemon_dex};

// ── Fixtures ─────────────────────────────────────────────────────────────────

/// The usage cache is gitignored and regenerable, so it may not exist. Tests
/// that need it skip rather than fail — otherwise a fresh clone cannot run the
/// suite.
fn meta_root() -> Option<PathBuf> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../meta_scraper/data");
    root.is_dir().then_some(root)
}

static DOUBLES: OnceLock<Option<MetaDex>> = OnceLock::new();
static LEARNSETS: OnceLock<HashMap<Species, HashSet<PokemonMove>>> = OnceLock::new();

fn learnset_dex() -> &'static HashMap<Species, HashSet<PokemonMove>> {
    LEARNSETS.get_or_init(|| {
        crate::state::dex_data::parse_learnset_dex("../pokemon_info/showdownLearnsets.txt")
    })
}

/// A config with real learnset data.
///
/// This is what lets the move sampler model the residual: usage percentages sum
/// to ~350 rather than 400, and that shortfall is real mass on moves outside the
/// top-10 table. With a learnset there is something to fill those slots with, so
/// the listed moves keep their true marginals. Without one the residual has
/// nowhere to go and the listed moves absorb it, inflating every rate by ~1.03x
/// — a documented degradation, and the reason this config exists separately.
fn config_with_learnsets() -> DeterminizeConfig {
    DeterminizeConfig {
        inference: InferenceConfig {
            learnset_dex: learnset_dex().clone(),
            ..InferenceConfig::default()
        },
        observer: Player::P1,
        ..Default::default()
    }
}

fn doubles_meta() -> Option<&'static MetaDex> {
    DOUBLES
        .get_or_init(|| {
            meta_root().and_then(|r| MetaDex::load(&r, None, MetaFormat::Doubles).ok())
        })
        .as_ref()
}

/// Skip the body unless the cache is present.
macro_rules! with_meta {
    ($meta:ident) => {
        let Some($meta) = doubles_meta() else { return };
    };
}

fn config() -> DeterminizeConfig {
    DeterminizeConfig {
        inference: InferenceConfig::default(),
        observer: Player::P1,
        ..Default::default()
    }
}

fn opponent(species: Species) -> UnknownPokemonState {
    UnknownPokemonState::from_opponent_species(species, pokemon_dex(), 50)
}

/// A fully-known Pokemon for the observer's own side.
fn own(species: Species) -> UnknownPokemonState {
    let mon = crate::state::pokemon::build_pokemon_state(
        species,
        pokemon_dex(),
        move_dex(),
        Some(50),
        Some([Some(PokemonMove::Tackle), None, None, None]),
        None,
        None,
        Some(Nature::Hardy),
        Some(Item::None),
        None,
        Some([0; 6]),
        Some([31; 6]),
        true,
    );
    UnknownPokemonState::from_known_pokemon(&mon)
}

/// A 1v1 belief: P1 (the observer) fully known, P2 species-only fog.
fn belief_1v1(p1: Species, p2: Species, bench: usize) -> UnknownBattleState {
    let mut state = UnknownBattleState {
        active_per_side: 1,
        back_mons_per_side: bench as u8,
        p1_active_mons: vec![own(p1)],
        p2_active_mons: vec![opponent(p2)],
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
    };
    // Give P1 a bench so both sides have a legal roster.
    for _ in 0..bench {
        state.p1_known_back_mons.push(own(Species::Pikachu));
    }
    state
}

// ── 1. It builds at all ──────────────────────────────────────────────────────

#[test]
fn determinizes_a_species_only_belief() {
    with_meta!(meta);
    let belief = belief_1v1(Species::Charizard, Species::Garchomp, 0);
    let world = determinize_seeded(1, &belief, meta, pokemon_dex(), move_dex(), &config())
        .expect("a species-only belief must be determinizable");

    let chomp = &world.state.p2_active_mons[0];
    assert_eq!(chomp.species, Species::Garchomp);
    assert!(world.probability > 0.0 && world.probability <= 1.0);

    // The four things `materialize_pokemon` gets wrong, all of which make a
    // state unplayable.
    assert_eq!(
        chomp.moves.iter().filter(|m| m.is_some()).count(),
        4,
        "every move slot should be filled: {:?}",
        chomp.moves
    );
    for i in 0..4 {
        assert!(chomp.max_pp[i] > 0, "move slot {i} has no PP");
        assert!(chomp.move_pp[i] > 0, "move slot {i} starts empty");
    }
    assert_eq!(chomp.hp, chomp.stats[0], "an unhurt Pokemon is at full HP");
    // EVs must agree with the stats derived from them.
    let recomputed = crate::state::pokemon::calc_stats_for_level(
        pokemon_dex()[&Species::Garchomp].base_stats,
        chomp.ivs,
        chomp.evs,
        chomp.level,
        &chomp.nature,
    );
    assert_eq!(recomputed, chomp.stats);
}

// ── 2. Soundness against the belief ──────────────────────────────────────────

#[test]
fn worlds_satisfy_the_belief_they_came_from() {
    with_meta!(meta);
    let cfg = config();

    for species in [
        Species::Garchomp,
        Species::Incineroar,
        Species::Whimsicott,
        Species::Kingambit,
        Species::Sinistcha,
    ] {
        let belief = belief_1v1(Species::Charizard, species.clone(), 2);
        for seed in 0..25u64 {
            let world = determinize_seeded(seed, &belief, meta, pokemon_dex(), move_dex(), &cfg)
                .unwrap_or_else(|e| panic!("{species:?} seed {seed}: {e}"));

            let violations = collect_true_state_subset_violations(
                &world.state,
                &UnknownMatchState::Battle(belief.clone()),
                Player::P1,
                pokemon_dex(),
                move_dex(),
            );
            assert!(
                violations.is_empty(),
                "{species:?} seed {seed}: subset violations {violations:?}"
            );

            let problems = check_determinization(&world, &belief, pokemon_dex());
            assert!(problems.is_empty(), "{species:?} seed {seed}: {problems:?}");
        }
    }
}

/// A belief with real observations, not just a species: revealed moves, a known
/// item, an excluded ability and a damaged HP bar.
#[test]
fn respects_revealed_information() {
    with_meta!(meta);
    let mut belief = belief_1v1(Species::Charizard, Species::Garchomp, 1);
    {
        let chomp = &mut belief.p2_active_mons[0];
        chomp.known_moves[0] = Some(PokemonMove::Earthquake);
        chomp.known_moves[2] = Some(PokemonMove::Protect);
        chomp.item = Unknown::Known(Item::ChoiceScarf);
        chomp.possible_abilities = Unknown::Known(Ability::RoughSkin);
        chomp.hp = PokemonHP::Percent(62);
    }

    for seed in 0..40u64 {
        let world = determinize_seeded(seed, &belief, meta, pokemon_dex(), move_dex(), &config())
            .unwrap();
        let chomp = &world.state.p2_active_mons[0];

        // Revealed moves keep their exact slots — PP, Last Resort and
        // `used_moves_this_field` are all slot-indexed.
        assert_eq!(chomp.moves[0], Some(PokemonMove::Earthquake));
        assert_eq!(chomp.moves[2], Some(PokemonMove::Protect));
        assert_eq!(chomp.item, Item::ChoiceScarf);
        assert_eq!(chomp.ability, Ability::RoughSkin);

        // HP must land in the band that displays as 62%, not merely near it.
        let (lo, hi) = crate::information::inference::percent_bucket(62, chomp.stats[0])
            .expect("62% is representable");
        assert!(
            (lo..=hi).contains(&chomp.hp),
            "seed {seed}: HP {} outside {lo}..={hi}",
            chomp.hp
        );

        assert!(check_determinization(&world, &belief, pokemon_dex()).is_empty());
    }
}

// ── 3. Determinism ───────────────────────────────────────────────────────────

/// A Rest-induced sleep must reach the concrete world, because the simulator
/// branches on it: `rest_sleep` blocks deterministically for exactly two turns,
/// while a natural sleep wakes on a 1/3-vs-2/3 weighted roll. Dropping the flag
/// silently hands a solver the wrong wake distribution for a Rest-slept
/// opponent — a legal world, but the wrong one to plan against.
#[test]
fn a_rest_sleep_reaches_the_determinized_world() {
    with_meta!(meta);
    let mut belief = belief_1v1(Species::Charizard, Species::Garchomp, 0);
    belief.p2_active_mons[0].status =
        Some(crate::state::dex_data::Status::Sleep(2));

    belief.p2_active_mons[0].rest_sleep = true;
    let world =
        determinize_seeded(3, &belief, meta, pokemon_dex(), move_dex(), &config()).unwrap();
    assert!(
        world.state.p2_active_mons[0].rest_sleep,
        "the belief said this sleep came from Rest and the world lost it"
    );

    // ...and a natural sleep must not be promoted to a deterministic one.
    belief.p2_active_mons[0].rest_sleep = false;
    let world =
        determinize_seeded(3, &belief, meta, pokemon_dex(), move_dex(), &config()).unwrap();
    assert!(
        !world.state.p2_active_mons[0].rest_sleep,
        "an ordinary sleep was wrongly marked Rest-induced"
    );
}

/// A Transformed opponent must keep its **own** HP and stay able to revert.
///
/// Transform copies every stat except HP (`transform_into` skips `stats[0]`),
/// and the belief mirrors that: after `EventKind::Transformed`, inference copies
/// the target's bounds into indices 1..6 only and leaves `min_stats[0]` /
/// `max_stats[0]` describing the *transformer*. So a post-Transform belief's
/// species and HP bounds disagree on purpose, and the fixture below has to
/// reproduce that or it is testing a state the engine never produces.
///
/// Before this was handled, building from the primary view gave a Ditto-into-
/// Garchomp the **target's** 185 HP and `pre_transform: None` — the wrong
/// Pokemon, unable to revert, with no warning of either.
#[test]
fn a_transformed_opponent_keeps_its_own_hp_and_can_revert() {
    with_meta!(meta);
    let mut belief = belief_1v1(Species::Charizard, Species::Garchomp, 0);

    // A Ditto that has Transformed into Garchomp: the belief's primary fields
    // show the copy target, `pre_transform` holds the real identity.
    let real = opponent(Species::Ditto);
    let (real_hp_lo, real_hp_hi) = (real.min_stats[0], real.max_stats[0]);
    let shown_hp_lo = belief.p2_active_mons[0].min_stats[0];
    assert!(
        real_hp_hi < shown_hp_lo,
        "fixture needs the two HP ranges disjoint to prove which one was used: \
         Ditto {real_hp_lo}..={real_hp_hi} vs Garchomp from {shown_hp_lo}"
    );

    // Match what inference really leaves behind: HP bounds stay the Ditto's.
    belief.p2_active_mons[0].min_stats[0] = real.min_stats[0];
    belief.p2_active_mons[0].max_stats[0] = real.max_stats[0];
    belief.p2_active_mons[0].min_pre_nature_stat[0] = real.min_pre_nature_stat[0];
    belief.p2_active_mons[0].max_pre_nature_stat[0] = real.max_pre_nature_stat[0];
    belief.p2_active_mons[0].pre_transform = Some(Box::new(real));

    for seed in 0..25u64 {
        let world = determinize_seeded(
            seed,
            &belief,
            meta,
            pokemon_dex(),
            move_dex(),
            &config_with_learnsets(),
        )
        .unwrap();
        let mon = &world.state.p2_active_mons[0];

        // Appearance is the copy target...
        assert_eq!(mon.species, Species::Garchomp, "seed {seed}");
        // ...but HP is its own, and it can revert.
        assert!(
            (real_hp_lo..=real_hp_hi).contains(&mon.stats[0]),
            "seed {seed}: max HP {} is not in the Ditto's range {real_hp_lo}..={real_hp_hi} \
             — Transform must not copy HP",
            mon.stats[0]
        );
        let pre = mon
            .pre_transform
            .as_ref()
            .unwrap_or_else(|| panic!("seed {seed}: no revert snapshot"));
        assert_eq!(pre.species, Species::Ditto, "seed {seed}");

        // Transform caps copied PP at 5.
        for (i, pp) in mon.move_pp.iter().enumerate() {
            if mon.moves[i].is_some() {
                assert!(*pp <= 5, "seed {seed}: slot {i} has {pp} PP, Transform caps at 5");
            }
        }

        let check_problems = check_determinization(&world, &belief, pokemon_dex());
        assert!(
            check_problems.is_empty(),
            "seed {seed}: the determinizer's own checker rejected a legal \
             Transformed world: {:?}",
            check_problems
        );
        assert!(
            !world.warnings.iter().any(|w| matches!(
                w,
                DeterminizeWarning::UnsatisfiedConstraint { .. }
            )),
            "seed {seed}: {:?}",
            world.warnings
        );
    }

    // A malformed `pre_transform` surfaces as a panic or an illegal command in
    // the engine rather than as a bad field, so the world has to be driven.
    for seed in 0..8u64 {
        let world = determinize_seeded(
            seed,
            &belief,
            meta,
            pokemon_dex(),
            move_dex(),
            &config_with_learnsets(),
        )
        .unwrap();
        let mut state = MatchState::BattleState(world.state);
        for turn in 0..3 {
            let MatchState::BattleState(battle) = &state else {
                break;
            };
            let p1 = legal_commands(battle, Player::P1);
            let p2 = legal_commands(battle, Player::P2);
            assert!(
                !p1.is_empty() && !p2.is_empty(),
                "seed {seed} turn {turn}: a side has no legal move"
            );
            let (next, _, probability) = sample_turn_raw_seeded(
                seed.wrapping_mul(31).wrapping_add(turn),
                &state,
                &PlayerCommand::Battle(vec![p1[0].clone()]),
                &PlayerCommand::Battle(vec![p2[0].clone()]),
                move_dex(),
                pokemon_dex(),
                true,
                16,
                Some(Player::P1),
            );
            assert!(
                probability > 0.0,
                "seed {seed} turn {turn}: zero-probability turn"
            );
            state = next;
        }
    }
}

/// An unresolved Zoroark hypothesis must become a real, self-consistent
/// disguise.
///
/// The load-bearing assertion is the last one. `illusion_disguise` is a stored
/// field, but the engine *derives* the disguise from the concrete roster —
/// "the last non-fainted party member" (`compute_illusion_disguise`) — and
/// recomputes it on every switch-in. If the stored value and the derived one
/// disagree, the world is fine until the Zoroark pivots out and back, at which
/// point it silently changes what it is pretending to be. So committing the
/// hypothesis has to constrain the bench *order*, not just set a field.
#[test]
fn an_unresolved_zoroark_becomes_a_self_consistent_disguise() {
    with_meta!(meta);
    let mut belief = belief_1v1(Species::Charizard, Species::Garchomp, 2);

    // This is the real unresolved representation produced by inference. The
    // active record LOOKS like Garchomp and carries the alternate Zoroark
    // identity; the explicit Zoroark roster record remains in possible_back.
    // If the alternate is committed, those identities must be swapped: the
    // active becomes Zoroark and the pristine Garchomp template becomes the
    // concrete decoy on the bench.
    belief.p2_active_mons[0].possible_mon_id = Unknown::Known(1);
    let mut zoroark = opponent(Species::Zoroark);
    zoroark.possible_mon_id = Unknown::Known(2);
    belief.p2_active_mons[0].possible_illusion_state = Some(Box::new(zoroark.clone()));
    belief.p2_unresolved_zoroark_count = 1;
    let mut incineroar = opponent(Species::Incineroar);
    incineroar.possible_mon_id = Unknown::Known(3);
    belief.p2_possible_back_mons = vec![zoroark.clone(), incineroar.clone()];
    belief.p2_roster_templates = vec![
        belief.p2_active_mons[0].clone(),
        zoroark,
        incineroar,
    ];
    for template in &mut belief.p2_roster_templates {
        template.possible_illusion_state = None;
    }

    let slot = FieldSlot {
        player: Player::P2,
        slot_index: 0,
    };

    let mut active_commits = 0;
    let mut off_field_draws = 0;
    for seed in 0..50u64 {
        let world = determinize_seeded(
            seed,
            &belief,
            meta,
            pokemon_dex(),
            move_dex(),
            &config_with_learnsets(),
        )
        .unwrap();
        let mon = &world.state.p2_active_mons[0];

        if mon.species == Species::Zoroark {
            active_commits += 1;
            assert_eq!(mon.ability, Ability::Illusion, "seed {seed}");
            assert_eq!(
                mon.illusion_disguise,
                Some(Species::Garchomp),
                "seed {seed}: must appear as what the observer has been seeing"
            );
            assert!(mon.pre_transform.is_none(), "seed {seed}");
            assert_eq!(
                world
                    .state
                    .p2_back_mons
                    .iter()
                    .filter(|back| back.species == Species::Zoroark)
                    .count(),
                0,
                "seed {seed}: the stale explicit Zoroark record was duplicated"
            );
            assert_eq!(
                crate::simulator::helpers::compute_illusion_disguise(&world.state, slot),
                mon.illusion_disguise,
                "seed {seed}: the engine derives a different disguise than the one stored — \
                 bench order {:?}",
                world
                    .state
                    .p2_back_mons
                    .iter()
                    .map(|m| (m.species.clone(), m.fainted))
                    .collect::<Vec<_>>()
            );
        } else {
            off_field_draws += 1;
            assert_eq!(mon.species, Species::Garchomp, "seed {seed}");
            assert_eq!(mon.illusion_disguise, None, "seed {seed}");
            assert!(
                world
                    .state
                    .p2_back_mons
                    .iter()
                    .any(|back| back.species == Species::Zoroark),
                "seed {seed}: the off-field draw lost the real Zoroark"
            );
        }
        let check_problems = check_determinization(&world, &belief, pokemon_dex());
        assert!(
            check_problems.is_empty(),
            "seed {seed}: checker rejected an admitted Illusion world: {:?}",
            check_problems
        );
    }
    assert!(active_commits > 0, "no seed committed the active hypothesis");
    assert!(
        off_field_draws > 0,
        "unresolved Zoroark was forced active; bench/off-field mass disappeared"
    );
}

/// ...and with the count already at zero, nothing may be invented: the side's
/// Zoroark is accounted for, so asserting a disguise would be fabrication.
#[test]
fn a_resolved_side_gets_no_disguise() {
    with_meta!(meta);
    let mut belief = belief_1v1(Species::Charizard, Species::Garchomp, 2);
    belief.p2_active_mons[0].possible_illusion_state =
        Some(Box::new(opponent(Species::Zoroark)));
    belief.p2_unresolved_zoroark_count = 0;
    belief.p2_known_back_mons.push(opponent(Species::Incineroar));
    belief.p2_known_back_mons.push(opponent(Species::Garchomp));

    for seed in 0..15u64 {
        let world = determinize_seeded(
            seed,
            &belief,
            meta,
            pokemon_dex(),
            move_dex(),
            &config_with_learnsets(),
        )
        .unwrap();
        let mon = &world.state.p2_active_mons[0];
        assert_eq!(mon.species, Species::Garchomp, "seed {seed}");
        assert_eq!(mon.illusion_disguise, None, "seed {seed}");
    }
}

#[test]
fn same_seed_gives_the_same_world() {
    with_meta!(meta);
    let belief = belief_1v1(Species::Charizard, Species::Garchomp, 2);
    let a = determinize_seeded(7, &belief, meta, pokemon_dex(), move_dex(), &config()).unwrap();
    let b = determinize_seeded(7, &belief, meta, pokemon_dex(), move_dex(), &config()).unwrap();
    assert_eq!(a.state, b.state);
    assert_eq!(a.probability, b.probability);
}

#[test]
fn different_seeds_explore_the_space() {
    with_meta!(meta);
    let belief = belief_1v1(Species::Charizard, Species::Garchomp, 0);
    let worlds: HashSet<_> = (0..200u64)
        .map(|seed| {
            determinize_seeded(seed, &belief, meta, pokemon_dex(), move_dex(), &config())
                .unwrap()
                .state
                .p2_active_mons[0]
                .clone()
        })
        .collect();
    // Garchomp's cache offers 10 moves, 10 items, 10 natures and 12 spreads, so
    // 200 seeds should be nowhere near collapsing onto one build.
    assert!(
        worlds.len() > 50,
        "only {} distinct builds from 200 seeds",
        worlds.len()
    );
}

// ── 4. Fidelity to the usage data ────────────────────────────────────────────

/// The test that shows the renormalization and the conditional-Poisson fit are
/// actually right, rather than merely self-consistent. Everything else proves
/// the worlds are *legal*; this proves they are *representative*.
///
/// Targets are read from the loaded cache rather than hardcoded. The cache is
/// regenerable and its percentages move every time the scraper runs — pinning
/// literals here would mean a test that fails on every refresh while telling us
/// nothing about the sampler. What must hold across refreshes is the
/// *relationship* between the data and the draws.
#[test]
fn sampled_builds_follow_the_usage_data() {
    with_meta!(meta);
    let belief = belief_1v1(Species::Charizard, Species::Garchomp, 0);
    let cfg = config_with_learnsets();
    let chomp_meta = meta.get(&Species::Garchomp).expect("Garchomp is in the cache");

    const DRAWS: usize = 4_000;
    let mut moves: HashMap<PokemonMove, usize> = HashMap::new();
    let mut items: HashMap<Item, usize> = HashMap::new();
    let mut natures: HashMap<Nature, usize> = HashMap::new();

    for seed in 0..DRAWS as u64 {
        let world =
            determinize_seeded(seed, &belief, meta, pokemon_dex(), move_dex(), &cfg).unwrap();
        let chomp = &world.state.p2_active_mons[0];
        for m in chomp.moves.iter().flatten() {
            *moves.entry(m.clone()).or_default() += 1;
        }
        *items.entry(chomp.item.clone()).or_default() += 1;
        *natures.entry(chomp.nature).or_default() += 1;
    }

    let rate = |count: usize| count as f64 / DRAWS as f64 * 100.0;
    // 4 sigma on a binomial proportion at this sample size, plus a little slack
    // for the conditional-Poisson fit tolerance.
    let tolerance = |expected: f64| {
        let p = (expected / 100.0).clamp(0.0, 1.0);
        400.0 * (p * (1.0 - p) / DRAWS as f64).sqrt() + 1.5
    };

    // Moves are marginal inclusion rates and must be reproduced *directly*, with
    // no renormalization: the shortfall from 400% is real mass on moves outside
    // the top-10 list, and the residual slots are what absorb it. If those were
    // normalized away instead, every rate here would come out ~3% high.
    for weighted in &chomp_meta.moves {
        let observed = rate(moves.get(&weighted.value).copied().unwrap_or(0));
        assert!(
            (observed - weighted.pct).abs() < tolerance(weighted.pct),
            "{:?}: sampled {observed:.1}%, cache says {:.1}%",
            weighted.value,
            weighted.pct
        );
    }

    // Items and natures ARE distributions, so they get renormalized over the
    // listed options — the residual is deliberately dropped, per the TODO's
    // "should not consider other options".
    let item_total: f64 = chomp_meta.items.iter().map(|w| w.pct).sum();
    for weighted in chomp_meta.items.iter().take(3) {
        let expected = weighted.pct / item_total * 100.0;
        let observed = rate(items.get(&weighted.value).copied().unwrap_or(0));
        assert!(
            (observed - expected).abs() < tolerance(expected),
            "{:?}: sampled {observed:.1}%, expected ~{expected:.1}%              ({:.1}% of a {item_total:.1}% listed total)",
            weighted.value,
            weighted.pct
        );
    }

    let nature_total: f64 = chomp_meta.natures.iter().map(|w| w.pct).sum();
    for weighted in chomp_meta.natures.iter().take(2) {
        let expected = weighted.pct / nature_total * 100.0;
        let observed = rate(natures.get(&weighted.value).copied().unwrap_or(0));
        assert!(
            (observed - expected).abs() < tolerance(expected),
            "{:?}: sampled {observed:.1}%, expected ~{expected:.1}%",
            weighted.value
        );
    }

    // The residual really is spent on unlisted moves, at the rate the shortfall
    // implies rather than being quietly folded into the listed ones.
    let listed: HashSet<PokemonMove> = chomp_meta.moves.iter().map(|w| w.value.clone()).collect();
    let listed_slots: f64 = chomp_meta.moves.iter().map(|w| w.pct / 100.0).sum();
    let expected_off_meta = (4.0 - listed_slots).max(0.0);
    let off_meta: usize = moves
        .iter()
        .filter(|(m, _)| !listed.contains(m))
        .map(|(_, c)| *c)
        .sum();
    let per_draw = off_meta as f64 / DRAWS as f64;
    assert!(
        (per_draw - expected_off_meta).abs() < 0.05,
        "off-meta moves averaged {per_draw:.3} per draw, expected ~{expected_off_meta:.3}"
    );
}

/// Coherence must suppress builds whose nature fights their investment *without*
/// moving the nature marginal.
///
/// Those two halves pull against each other, and that tension is the whole
/// reason the damping is applied inside a nature's row rather than across the
/// joint table. A Careful Dragonite is a real thing people run — it just never
/// carries 32 Attack points. Damping `P(nature)·P(spread)` jointly would buy the
/// first half by paying for it with the second, shifting mass onto whichever
/// natures happen to have fewer damped cells;
/// `sampled_builds_follow_the_usage_data` is what would catch that, and this
/// test is what catches the reverse (a formulation so marginal-preserving it
/// stopped suppressing anything).
///
/// **Dragonite, not Garchomp**, and the choice is load-bearing. A species only
/// exercises this if its nature list and its spread list actually disagree
/// somewhere. Every Garchomp spread in the cache is Atk/Spe-shaped, so each of
/// its natures is either wholly coherent or wholly incoherent and the
/// incoherent ones carry ~1.7% of the mass between them — the measurement has
/// no room to move, and `on < off` there is within noise of a coin flip.
/// Dragonite carries a genuinely mixed spread table: 46.7% of its draws are
/// incoherent undamped, 14.5% damped.
///
/// The incoherence predicate is re-derived here in EV units rather than calling
/// the sampler's own — a test that shares the implementation it is checking
/// proves nothing.
#[test]
fn incoherent_builds_are_suppressed_without_moving_the_nature_marginal() {
    with_meta!(meta);
    let subject = Species::Dragonite;
    let belief = belief_1v1(Species::Charizard, subject.clone(), 0);
    let subject_meta = meta.get(&subject).expect("Dragonite is in the cache");

    const DRAWS: usize = 4_000;

    // `ev = max(0, 8p - 4)`, so 8 authoring points is 60 EVs and 0 stays 0.
    let is_incoherent = |mon: &PokemonState| {
        nature_stat_modifiers(&mon.nature)
            .iter()
            .enumerate()
            .any(|(i, m)| {
                let ev = mon.evs[i + 1];
                (*m < 1.0 && ev >= 60) || (*m > 1.0 && ev == 0)
            })
    };

    let sweep = |lowers_invested: f64, raises_unused: f64| {
        let cfg = DeterminizeConfig {
            nature_lowers_invested: lowers_invested,
            nature_raises_unused: raises_unused,
            ..config_with_learnsets()
        };
        let mut incoherent = 0usize;
        let mut natures: HashMap<Nature, usize> = HashMap::new();
        for seed in 0..DRAWS as u64 {
            let world =
                determinize_seeded(seed, &belief, meta, pokemon_dex(), move_dex(), &cfg).unwrap();
            let mon = &world.state.p2_active_mons[0];
            if is_incoherent(mon) {
                incoherent += 1;
            }
            *natures.entry(mon.nature).or_default() += 1;
        }
        (incoherent as f64 / DRAWS as f64 * 100.0, natures)
    };

    let (off_rate, off_natures) = sweep(1.0, 1.0);
    let (on_rate, on_natures) = sweep(0.10, 0.35);

    // The gap is the point. `1.0` must actually emit these, or the fixture has
    // stopped exercising the case and the comparison below is vacuous. Measured
    // 46.7% at the time of writing; the guard sits far below that so a cache
    // refresh that shifts the spread table does not fail the build spuriously,
    // but a refresh that flattens it entirely still does.
    assert!(
        off_rate > 15.0,
        "with coherence off only {off_rate:.2}% of builds were incoherent — \
         this fixture no longer exercises the case, pick another species"
    );
    // Measured ratio ~0.11 (44.9% -> 5.0%). Asserting merely `on < off` would
    // pass on noise. The defaults bite harder here than a single flat 0.15 did
    // because most of Dragonite's incoherent draws commit *both* faults and so
    // take both factors: 0.10 * 0.35.
    assert!(
        on_rate < off_rate * 0.2,
        "the default coherence left {on_rate:.2}% incoherent, barely down from {off_rate:.2}%"
    );

    // ...and the nature marginal must survive both settings.
    let nature_total: f64 = subject_meta.natures.iter().map(|w| w.pct).sum();
    let tolerance = |expected: f64| {
        let p = (expected / 100.0).clamp(0.0, 1.0);
        400.0 * (p * (1.0 - p) / DRAWS as f64).sqrt() + 1.5
    };
    for (label, natures) in [("off", &off_natures), ("on", &on_natures)] {
        for weighted in subject_meta.natures.iter().take(2) {
            let expected = weighted.pct / nature_total * 100.0;
            let observed =
                natures.get(&weighted.value).copied().unwrap_or(0) as f64 / DRAWS as f64 * 100.0;
            assert!(
                (observed - expected).abs() < tolerance(expected),
                "coherence {label}: {:?} sampled {observed:.1}%, expected ~{expected:.1}%",
                weighted.value
            );
        }
    }
}

/// Each coherence rule must damp its own fault while the other rule is off.
///
/// The combined test above cannot tell the two apart: Dragonite's incoherent
/// draws mostly commit both faults at once, so a single knob doing all the work
/// would look identical there. Splitting the controls is only worth anything if
/// each one moves its own fault on its own, which is what these two measure.
///
/// Predicates are re-derived in EV units for the same reason as above — sharing
/// the sampler's own predicate would prove nothing. `ev = max(0, 8p - 4)`, so 8
/// authoring points is 60 EVs and 0 stays 0.
fn lowers_an_invested_stat(mon: &PokemonState) -> bool {
    nature_stat_modifiers(&mon.nature)
        .iter()
        .enumerate()
        .any(|(i, m)| *m < 1.0 && mon.evs[i + 1] >= 60)
}

fn raises_an_unused_stat(mon: &PokemonState) -> bool {
    nature_stat_modifiers(&mon.nature)
        .iter()
        .enumerate()
        .any(|(i, m)| *m > 1.0 && mon.evs[i + 1] == 0)
}

/// Runs `DRAWS` determinizations and returns the percentage matching `fault`.
///
/// `None` means the usage cache is absent, which skips the caller the same way
/// `with_meta!` skips a test body.
fn fault_rate(
    lowers_invested: f64,
    raises_unused: f64,
    fault: fn(&PokemonState) -> bool,
) -> Option<f64> {
    let meta = doubles_meta()?;
    const DRAWS: usize = 4_000;
    let belief = belief_1v1(Species::Charizard, Species::Dragonite, 0);
    let cfg = DeterminizeConfig {
        nature_lowers_invested: lowers_invested,
        nature_raises_unused: raises_unused,
        ..config_with_learnsets()
    };
    let mut hits = 0usize;
    for seed in 0..DRAWS as u64 {
        let world =
            determinize_seeded(seed, &belief, meta, pokemon_dex(), move_dex(), &cfg).unwrap();
        if fault(&world.state.p2_active_mons[0]) {
            hits += 1;
        }
    }
    Some(hits as f64 / DRAWS as f64 * 100.0)
}

#[test]
fn lowering_an_invested_stat_is_damped_on_its_own() {
    let Some(off) = fault_rate(1.0, 1.0, lowers_an_invested_stat) else {
        return;
    };
    let on = fault_rate(0.10, 1.0, lowers_an_invested_stat).expect("cache was present above");

    // The fixture has to produce the fault before suppressing it means anything.
    assert!(
        off > 10.0,
        "only {off:.2}% of undamped draws lowered an invested stat — \
         this fixture no longer exercises the rule, pick another species"
    );
    // Measured 44.5% -> 9.8%, a ratio of ~0.22. The ratio does not reach the
    // 0.10 factor itself: the row renormalization in `enumerate_nature_spreads`
    // gives back some of the mass a damped cell loses.
    assert!(
        on < off * 0.35,
        "nature_lowers_invested=0.10 left {on:.2}%, barely down from {off:.2}%"
    );
}

#[test]
fn raising_an_unused_stat_is_damped_on_its_own() {
    let Some(off) = fault_rate(1.0, 1.0, raises_an_unused_stat) else {
        return;
    };
    let on = fault_rate(1.0, 0.35, raises_an_unused_stat).expect("cache was present above");

    assert!(
        off > 10.0,
        "only {off:.2}% of undamped draws raised an unused stat — \
         this fixture no longer exercises the rule, pick another species"
    );
    // 0.35 is deliberately the gentler factor, so the drop here is smaller than
    // the one above. Measured 38.5% -> 20.9%, a ratio of ~0.54.
    assert!(
        on < off * 0.7,
        "nature_raises_unused=0.35 left {on:.2}%, barely down from {off:.2}%"
    );
}

/// A revealed move must survive into every world, keep its slot, and never be
/// duplicated by the sampler filling the remaining slots.
#[test]
fn revealed_moves_are_always_present_and_never_duplicated() {
    with_meta!(meta);
    let mut belief = belief_1v1(Species::Charizard, Species::Garchomp, 0);
    belief.p2_active_mons[0].known_moves[0] = Some(PokemonMove::SwordsDance);

    for seed in 0..300u64 {
        let world =
            determinize_seeded(seed, &belief, meta, pokemon_dex(), move_dex(), &config()).unwrap();
        let chomp = &world.state.p2_active_mons[0];
        assert_eq!(chomp.moves[0], Some(PokemonMove::SwordsDance));

        let distinct: HashSet<_> = chomp.moves.iter().flatten().collect();
        assert_eq!(
            distinct.len(),
            chomp.moves.iter().filter(|m| m.is_some()).count(),
            "seed {seed}: duplicate move in {:?}",
            chomp.moves
        );
    }
}

// ── 5. Fallbacks ─────────────────────────────────────────────────────────────

/// Per-attribute, not whole-Pokemon: ruling out every listed item must not
/// discard the usage data for natures and moves too.
#[test]
fn item_fallback_is_isolated_to_the_item() {
    with_meta!(meta);
    let mut belief = belief_1v1(Species::Charizard, Species::Garchomp, 0);
    let listed: Vec<Item> = meta
        .get(&Species::Garchomp)
        .unwrap()
        .items
        .iter()
        .map(|w| w.value.clone())
        .collect();
    belief.p2_active_mons[0].item = Unknown::Not(listed.clone());

    let mut jolly = 0;
    const DRAWS: u64 = 400;
    for seed in 0..DRAWS {
        let world =
            determinize_seeded(seed, &belief, meta, pokemon_dex(), move_dex(), &config()).unwrap();
        let chomp = &world.state.p2_active_mons[0];

        assert!(
            !listed.contains(&chomp.item),
            "seed {seed}: drew an excluded item {:?}",
            chomp.item
        );
        assert!(
            world.warnings.iter().any(|w| matches!(
                w,
                DeterminizeWarning::UniformFallback { attribute: "item", .. }
            )),
            "seed {seed}: expected an item fallback warning"
        );
        // The nature must NOT have fallen back.
        assert!(
            !world.warnings.iter().any(|w| matches!(
                w,
                DeterminizeWarning::UniformFallback { attribute: "nature_spread", .. }
            )),
            "seed {seed}: nature fell back too — the fallback is not per-attribute"
        );
        if chomp.nature == Nature::Jolly {
            jolly += 1;
        }
    }
    // Natures still track the usage data rather than going uniform (1/25 = 4%).
    let rate = jolly as f64 / DRAWS as f64;
    assert!(rate > 0.4, "Jolly rate collapsed to {rate:.2}");
}

/// The fallback must keep applying coherence.
///
/// It runs only once the belief has excluded every authored spread, and it is
/// tempting to read that as "no information left to use". The two are
/// independent: ruling out the cache's spreads says nothing about whether people
/// pair a minus-Attack nature with heavy Attack investment. So the fallback is
/// maximum-entropy over *legal* spreads, not over incoherent ones.
///
/// A 108-EV ceiling on every stat excludes every authored spread, since all of
/// them put 252 somewhere. It leaves 0..=14 points per stat, which keeps the
/// 66-point budget reachable and — because five stats at 14 clear 66 — leaves a
/// zeroed stat reachable too. Without that headroom the raise-an-unused-stat
/// fault could never appear and half the damping would go unmeasured.
#[test]
fn the_uniform_fallback_still_applies_coherence() {
    with_meta!(meta);
    let mut belief = belief_1v1(Species::Charizard, Species::Dragonite, 0);
    belief.p2_active_mons[0].max_evs = [108; 6];

    const DRAWS: u64 = 1_200;

    let sweep = |lowers: f64, raises: f64| {
        let cfg = DeterminizeConfig {
            nature_lowers_invested: lowers,
            nature_raises_unused: raises,
            ..config_with_learnsets()
        };
        let mut fell_back = 0usize;
        let mut incoherent = 0usize;
        let mut natures: HashMap<Nature, usize> = HashMap::new();
        for seed in 0..DRAWS {
            let world =
                determinize_seeded(seed, &belief, meta, pokemon_dex(), move_dex(), &cfg).unwrap();
            let mon = &world.state.p2_active_mons[0];
            if world.warnings.iter().any(|w| {
                matches!(
                    w,
                    DeterminizeWarning::UniformFallback {
                        attribute: "nature_spread",
                        ..
                    }
                )
            }) {
                fell_back += 1;
            }
            if lowers_an_invested_stat(mon) || raises_an_unused_stat(mon) {
                incoherent += 1;
            }
            *natures.entry(mon.nature).or_default() += 1;
        }
        let pct = |n: usize| n as f64 / DRAWS as f64 * 100.0;
        (pct(fell_back), pct(incoherent), natures)
    };

    let (fallback_rate, off_rate, off_natures) = sweep(1.0, 1.0);
    let (_, on_rate, on_natures) = sweep(0.10, 0.35);

    // The belief has to actually reach the fallback, or the rest is vacuous.
    assert!(
        fallback_rate > 99.0,
        "only {fallback_rate:.1}% of draws took the nature_spread fallback — \
         an authored spread now fits this belief, tighten the ceiling"
    );
    assert!(
        off_rate > 10.0,
        "the fallback produced only {off_rate:.2}% incoherent spreads undamped — \
         this belief no longer exercises the rules"
    );
    // Measured 67.8% -> 41.8%, a ratio of ~0.62. The drop is gentler than the
    // main path's because the pool is at most the eight allocations sampled for
    // that nature, and they are often all incoherent — with nothing coherent to
    // move mass onto, the weighting has no effect on that draw.
    assert!(
        on_rate < off_rate * 0.8,
        "the fallback left {on_rate:.2}% incoherent, barely down from {off_rate:.2}%"
    );

    // The nature marginal must stay flat. This is the guard on weighting inside
    // one nature instead of across natures: a cross-nature weighting would move
    // mass onto whichever nature offered more coherent allocations, and it would
    // show up here as one nature pulling far above its uniform share.
    //
    // Measured: all 25 natures appear under both settings, and the busiest one
    // takes 5.1% against a 4.0% uniform share — the same 5.1% with the damping
    // on as with it off, which is the point.
    for (label, natures) in [("off", &off_natures), ("on", &on_natures)] {
        let share = |n: &HashMap<Nature, usize>| {
            n.values().map(|c| *c as f64 / DRAWS as f64 * 100.0).fold(0.0, f64::max)
        };
        let uniform = 100.0 / natures.len() as f64;
        assert!(
            share(natures) < uniform * 2.5,
            "coherence {label}: one nature took {:.1}% of draws against a {uniform:.1}% \
             uniform share — the fallback is weighting across natures",
            share(natures)
        );
    }
}

/// The item clause has to be checked, not assumed.
///
/// The sampler threads a used-item set through its draws, so a clean run never
/// reaches this branch — which is exactly why it needs a test that plants a
/// violation instead of one that hopes to stumble on one. Every other
/// `check_determinization` assertion in this file is of the "no problems"
/// shape, and those would all still pass if the check did nothing at all.
#[test]
fn the_checker_catches_an_item_clause_violation() {
    with_meta!(meta);
    let belief = belief_1v1(Species::Charizard, Species::Garchomp, 2);
    let mut world = determinize_seeded(
        7,
        &belief,
        meta,
        pokemon_dex(),
        move_dex(),
        &config_with_learnsets(),
    )
    .unwrap();

    let item_problems = |world: &_| {
        check_determinization(world, &belief, pokemon_dex())
            .into_iter()
            .filter(|p| p.contains("item clause"))
            .collect::<Vec<_>>()
    };

    // The planted violation below proves nothing unless the world starts clean.
    assert!(
        check_determinization(&world, &belief, pokemon_dex()).is_empty(),
        "the unmodified world should already pass every check"
    );

    // Holding nothing is exempt, however many Pokemon do it — a consumed or
    // knocked-off item reads as `None` and must not look like a violation.
    world.state.p2_active_mons[0].item = Item::None;
    world.state.p2_back_mons[0].item = Item::None;
    assert!(
        item_problems(&world).is_empty(),
        "Item::None must be exempt from the clause"
    );

    // The same real item twice is not exempt.
    world.state.p2_active_mons[0].item = Item::Leftovers;
    world.state.p2_back_mons[0].item = Item::Leftovers;
    let problems = item_problems(&world);
    assert_eq!(
        problems.len(),
        1,
        "expected exactly one report for one duplicated item: {problems:?}"
    );

    // The clause is per team, so the same item on the two sides is legal.
    world.state.p2_back_mons[0].item = Item::None;
    world.state.p1_active_mons[0].item = Item::Leftovers;
    assert!(
        item_problems(&world).is_empty(),
        "one item per side is not a violation"
    );
}

/// A species outside the 235-entry cache is fully uniform but still legal.
#[test]
fn species_absent_from_the_cache_still_determinizes() {
    with_meta!(meta);
    // Ababo is in the dex but not in the usage cache.
    assert!(meta.get(&Species::Ababo).is_none());

    let mut belief = belief_1v1(Species::Charizard, Species::Ababo, 0);
    // Without a learnset there are no moves to draw, which is a hard error by
    // design; give it one so the fallback path is what gets exercised.
    let mut learnset = HashMap::new();
    learnset.insert(
        Species::Ababo,
        HashSet::from([
            PokemonMove::Tackle,
            PokemonMove::Growl,
            PokemonMove::QuickAttack,
            PokemonMove::Ember,
        ]),
    );
    belief.p2_active_mons[0].hp = PokemonHP::Percent(100);

    let cfg = DeterminizeConfig {
        inference: InferenceConfig {
            learnset_dex: learnset,
            ..InferenceConfig::default()
        },
        observer: Player::P1,
        ..Default::default()
    };

    let world = determinize_seeded(3, &belief, meta, pokemon_dex(), move_dex(), &cfg).unwrap();
    assert!(
        world
            .warnings
            .iter()
            .any(|w| matches!(w, DeterminizeWarning::NoMetaEntry { .. })),
        "expected a NoMetaEntry warning"
    );
    assert!(check_determinization(&world, &belief, pokemon_dex()).is_empty());
    assert_eq!(world.state.p2_active_mons[0].species, Species::Ababo);
}

/// With no meta entry and no learnset there is genuinely nothing to build a
/// moveset from, and that must be an error rather than a moveless Pokemon —
/// which would produce an empty legal-command set far away from here.
#[test]
fn no_move_source_is_a_hard_error() {
    with_meta!(meta);
    let belief = belief_1v1(Species::Charizard, Species::Ababo, 0);
    let err = determinize_seeded(1, &belief, meta, pokemon_dex(), move_dex(), &config())
        .expect_err("a Pokemon with no move source must not be silently built");
    assert!(matches!(err, DeterminizeError::NoLegalMoves { .. }), "{err:?}");
}

// ── Error paths ──────────────────────────────────────────────────────────────

#[test]
fn explicit_hidden_domains_replace_default_values() {
    with_meta!(meta);
    let mut belief = belief_1v1(Species::Charizard, Species::Garchomp, 0);
    let opponent = &mut belief.p2_active_mons[0];
    opponent.possible_genders = Unknown::Possibly(vec![PokemonGender::Female]);
    opponent.possible_tera_type = Unknown::Possibly(vec![PokemonType::Fire]);
    opponent.item = Unknown::Possibly(vec![Item::OranBerry]);
    opponent.ability_changed_on_field = true;
    opponent.possible_abilities = Unknown::Possibly(vec![Ability::Intimidate]);

    let world =
        determinize_seeded(2, &belief, meta, pokemon_dex(), move_dex(), &config()).unwrap();
    let mon = &world.state.p2_active_mons[0];
    assert_eq!(mon.gender, PokemonGender::Female);
    assert_eq!(mon.tera_type, PokemonType::Fire);
    assert_eq!(mon.item, Item::OranBerry);
    assert_eq!(mon.ability, Ability::Intimidate);
    assert!(check_determinization(&world, &belief, pokemon_dex()).is_empty());
}

#[test]
fn an_empty_item_domain_is_an_error() {
    with_meta!(meta);
    let mut belief = belief_1v1(Species::Charizard, Species::Garchomp, 0);
    belief.p2_active_mons[0].item = Unknown::Possibly(Vec::new());

    let err = determinize_seeded(2, &belief, meta, pokemon_dex(), move_dex(), &config())
        .expect_err("an empty item domain must not produce Item::None");
    assert!(
        matches!(
            err,
            DeterminizeError::NoCandidates {
                attribute: "item",
                ..
            }
        ),
        "{err:?}"
    );
}

#[test]
fn a_learnset_rejection_is_not_bypassed() {
    with_meta!(meta);
    let belief = belief_1v1(Species::Charizard, Species::Garchomp, 0);
    let mut learnset_dex = HashMap::new();
    learnset_dex.insert(Species::Garchomp, HashSet::new());
    let cfg = DeterminizeConfig {
        inference: InferenceConfig {
            learnset_dex,
            ..InferenceConfig::default()
        },
        observer: Player::P1,
        ..Default::default()
    };

    let err = determinize_seeded(2, &belief, meta, pokemon_dex(), move_dex(), &cfg)
        .expect_err("the determinizer must not restore rejected moves");
    assert!(matches!(err, DeterminizeError::NoLegalMoves { .. }), "{err:?}");
}

#[test]
fn a_missing_pokemon_dex_entry_is_an_error() {
    with_meta!(meta);
    let opponent = opponent(Species::Garchomp);
    let mut used_items = HashSet::new();
    let mut log = DrawLog::new();
    let err = determinize_pokemon(
        0,
        &opponent,
        meta,
        &HashMap::new(),
        move_dex(),
        &mut used_items,
        &config(),
        &mut log,
    )
    .expect_err("an unknown species must not use fabricated base stats");
    assert!(matches!(err, DeterminizeError::UnknownSpecies { .. }), "{err:?}");
}

// ── 6. Bench construction ────────────────────────────────────────────────────

#[test]
fn bench_is_drawn_from_the_belief_candidates() {
    with_meta!(meta);
    let mut belief = belief_1v1(Species::Charizard, Species::Garchomp, 3);
    for species in [
        Species::Incineroar,
        Species::Whimsicott,
        Species::Kingambit,
        Species::Sinistcha,
        Species::Sneasler,
        Species::Farigiraf,
        Species::Raichu,
        Species::Staraptor,
    ] {
        belief.p2_possible_back_mons.push(opponent(species));
    }

    let candidates: HashSet<Species> = belief
        .p2_possible_back_mons
        .iter()
        .map(|m| match &m.possible_species {
            Unknown::Known(s) => s.clone(),
            _ => unreachable!(),
        })
        .collect();

    for seed in 0..30u64 {
        let world =
            determinize_seeded(seed, &belief, meta, pokemon_dex(), move_dex(), &config()).unwrap();
        let bench = &world.state.p2_back_mons;
        assert_eq!(bench.len(), 3, "seed {seed}: bench is {}", bench.len());

        let species: Vec<Species> = bench.iter().map(|m| m.species.clone()).collect();
        for s in &species {
            assert!(candidates.contains(s), "seed {seed}: {s:?} was never a candidate");
        }
        // Species Clause.
        assert_eq!(
            species.iter().collect::<HashSet<_>>().len(),
            species.len(),
            "seed {seed}: duplicate bench species {species:?}"
        );
        assert!(
            !world
                .warnings
                .iter()
                .any(|w| matches!(w, DeterminizeWarning::InventedBenchMon { .. })),
            "seed {seed}: invented a Pokemon despite having enough candidates"
        );
    }
}

#[test]
fn bench_is_invented_when_the_belief_cannot_fill_it() {
    with_meta!(meta);
    let mut belief = belief_1v1(Species::Charizard, Species::Garchomp, 3);
    belief.p2_possible_back_mons.push(opponent(Species::Incineroar));

    let world =
        determinize_seeded(5, &belief, meta, pokemon_dex(), move_dex(), &config()).unwrap();
    assert_eq!(world.state.p2_back_mons.len(), 3);
    let invented = world
        .warnings
        .iter()
        .filter(|w| matches!(w, DeterminizeWarning::InventedBenchMon { .. }))
        .count();
    assert_eq!(invented, 2, "expected exactly the two missing bench slots");

    // Invented Pokemon vary with the seed rather than being a fixed argmax.
    let benches: HashSet<Vec<Species>> = (0..40u64)
        .map(|seed| {
            determinize_seeded(seed, &belief, meta, pokemon_dex(), move_dex(), &config())
                .unwrap()
                .state
                .p2_back_mons
                .iter()
                .map(|m| m.species.clone())
                .collect()
        })
        .collect();
    assert!(benches.len() > 1, "invented bench is identical across seeds");
}

/// Turning invention off must leave the bench short rather than fabricating.
#[test]
fn bench_invention_can_be_disabled() {
    with_meta!(meta);
    let belief = belief_1v1(Species::Charizard, Species::Garchomp, 3);
    let cfg = DeterminizeConfig {
        observer: Player::P1,
        invent_missing_bench: false,
        ..Default::default()
    };
    let world = determinize_seeded(1, &belief, meta, pokemon_dex(), move_dex(), &cfg).unwrap();
    assert!(world.state.p2_back_mons.is_empty());
    assert!(
        !world
            .warnings
            .iter()
            .any(|w| matches!(w, DeterminizeWarning::InventedBenchMon { .. }))
    );
}

// ── 7. The item clause ───────────────────────────────────────────────────────

#[test]
fn no_two_teammates_share_an_item() {
    with_meta!(meta);
    let mut belief = belief_1v1(Species::Charizard, Species::Garchomp, 3);
    for species in [Species::Incineroar, Species::Whimsicott, Species::Kingambit] {
        belief.p2_known_back_mons.push(opponent(species));
    }

    for seed in 0..50u64 {
        let world =
            determinize_seeded(seed, &belief, meta, pokemon_dex(), move_dex(), &config()).unwrap();
        let mut seen: HashSet<Item> = HashSet::new();
        for mon in world
            .state
            .p2_active_mons
            .iter()
            .chain(world.state.p2_back_mons.iter())
        {
            if mon.item == Item::None {
                continue; // exempt: any number of Pokemon may hold nothing
            }
            assert!(
                seen.insert(mon.item.clone()),
                "seed {seed}: {:?} duplicated on the team",
                mon.item
            );
        }
    }
}

/// The item clause must survive bench *invention*, not just bench selection.
///
/// `no_two_teammates_share_an_item` fills the bench from the belief, so
/// invention never runs there and this path was uncovered. It also cannot be
/// caught downstream: `subset_check` compares the world against the belief, and
/// an invented Pokemon has no belief entry to contradict, so a duplicated item
/// is invisible to every existing oracle.
#[test]
fn invented_bench_respects_the_item_clause() {
    with_meta!(meta);
    // No bench candidates at all, so all three slots are invented — and the
    // active is already holding something they must not collide with.
    let belief = belief_1v1(Species::Charizard, Species::Garchomp, 3);

    for seed in 0..200u64 {
        let world =
            determinize_seeded(seed, &belief, meta, pokemon_dex(), move_dex(), &config()).unwrap();
        let mut seen: HashSet<Item> = HashSet::new();
        for mon in world
            .state
            .p2_active_mons
            .iter()
            .chain(world.state.p2_back_mons.iter())
        {
            if mon.item == Item::None {
                continue; // exempt: any number of Pokemon may hold nothing
            }
            assert!(
                seen.insert(mon.item.clone()),
                "seed {seed}: {:?} duplicated on the team ({:?})",
                mon.item,
                world
                    .state
                    .p2_active_mons
                    .iter()
                    .chain(world.state.p2_back_mons.iter())
                    .map(|m| (m.species.clone(), m.item.clone()))
                    .collect::<Vec<_>>()
            );
        }
    }
}

/// Invented Pokemon should look like teammates of what we have already seen,
/// not merely like the format's most popular species.
///
/// Plausibility is invisible to both oracles (see the note on
/// `invented_bench_respects_the_item_clause`), so it has to be measured
/// directly. Targets are read from the cache rather than hardcoded, for the
/// same reason `sampled_builds_follow_the_usage_data` does it: the percentages
/// move on every scraper run, and what must hold across refreshes is the
/// *relationship* between the data and the draws.
#[test]
fn invented_bench_prefers_known_partners() {
    with_meta!(meta);
    let subject = Species::Garchomp;
    let Some(subject_meta) = meta.get(&subject) else {
        return;
    };
    let belief = belief_1v1(Species::Charizard, subject.clone(), 3);

    let partners: HashSet<Species> = subject_meta
        .teammates
        .iter()
        .map(|w| w.value.clone())
        .collect();
    assert!(
        partners.len() >= 5,
        "fixture needs a real teammate list, got {}",
        partners.len()
    );

    let mut invented = 0usize;
    let mut from_partners = 0usize;
    for seed in 0..500u64 {
        let world =
            determinize_seeded(seed, &belief, meta, pokemon_dex(), move_dex(), &config()).unwrap();
        for mon in &world.state.p2_back_mons {
            invented += 1;
            if partners.contains(&mon.species) {
                from_partners += 1;
            }
        }
    }
    assert!(invented > 0, "the fixture invented nothing");

    // The listed partners are ~10 of ~235 species. Uninformed drawing would put
    // them near 4%; the prior should put them far above it. The bound is loose
    // on purpose — this asserts the signal exists, not its exact strength.
    let share = from_partners as f64 / invented as f64 * 100.0;
    assert!(
        share > 25.0,
        "only {share:.1}% of invented bench mons were listed teammates of \
         {subject:?} — the co-occurrence prior is not being applied"
    );
}

/// ...but the prior must not collapse onto the teammate lists either.
///
/// Roughly half the format has no teammate data at all, and the belief has said
/// nothing to rule those species out. `POPULARITY_FLOOR` is what keeps them
/// reachable; without it they would be impossible rather than merely unlikely.
#[test]
fn invented_bench_can_still_reach_an_unlisted_species() {
    with_meta!(meta);
    let subject = Species::Garchomp;
    let Some(subject_meta) = meta.get(&subject) else {
        return;
    };
    let belief = belief_1v1(Species::Charizard, subject.clone(), 3);
    let partners: HashSet<Species> = subject_meta
        .teammates
        .iter()
        .map(|w| w.value.clone())
        .collect();

    let reached_outside = (0..500u64).any(|seed| {
        determinize_seeded(seed, &belief, meta, pokemon_dex(), move_dex(), &config())
            .unwrap()
            .state
            .p2_back_mons
            .iter()
            .any(|m| !partners.contains(&m.species))
    });
    assert!(
        reached_outside,
        "500 seeds never invented a species outside {subject:?}'s teammate list — \
         the prior has collapsed onto it"
    );
}

// ── 8. mon_id ────────────────────────────────────────────────────────────────

/// Duplicate ids would make `subset_check::build_mon_idx_map` pair belief
/// entries with the wrong Pokemon, checking one mon's constraints against
/// another's — a silent wrong answer rather than a failure.
#[test]
fn mon_ids_are_unique_per_side() {
    with_meta!(meta);
    let mut belief = belief_1v1(Species::Charizard, Species::Garchomp, 3);
    for species in [Species::Incineroar, Species::Whimsicott, Species::Kingambit] {
        belief.p2_known_back_mons.push(opponent(species));
    }
    belief.p2_known_back_mons[0].possible_mon_id = Unknown::Known(9);

    let world =
        determinize_seeded(2, &belief, meta, pokemon_dex(), move_dex(), &config()).unwrap();
    for side in [&world.state.p1_active_mons, &world.state.p2_active_mons] {
        let _ = side;
    }
    for (active, back) in [
        (&world.state.p1_active_mons, &world.state.p1_back_mons),
        (&world.state.p2_active_mons, &world.state.p2_back_mons),
    ] {
        let ids: Vec<u8> = active.iter().chain(back.iter()).map(|m| m.mon_id).collect();
        assert_eq!(
            ids.iter().collect::<HashSet<_>>().len(),
            ids.len(),
            "duplicate mon_id in {ids:?}"
        );
    }
    // A known id is preserved rather than reassigned.
    assert!(world.state.p2_back_mons.iter().any(|m| m.mon_id == 9));
}

// ── 9. Runnability ───────────────────────────────────────────────────────────

/// The TODO's actual requirement: worlds "should just be able to be put in the
/// simulator and work".
///
/// This is the test that would catch a `materialize`-style gap, because the
/// failure modes are all mechanical rather than subtle — an empty bench makes
/// every switch command illegal, a zero-PP move slot makes that move illegal,
/// and either shows up here immediately instead of somewhere downstream.
#[test]
fn worlds_can_be_played_through_the_simulator() {
    with_meta!(meta);
    let cfg = config_with_learnsets();

    for (opponent_species, bench) in [
        (Species::Garchomp, 0usize),
        (Species::Incineroar, 2),
        (Species::Kingambit, 3),
    ] {
        let belief = belief_1v1(Species::Charizard, opponent_species.clone(), bench);
        for seed in 0..8u64 {
            let world =
                determinize_seeded(seed, &belief, meta, pokemon_dex(), move_dex(), &cfg).unwrap();
            let mut state = MatchState::BattleState(world.state);

            for turn in 0..5 {
                let MatchState::BattleState(battle) = &state else {
                    break; // the battle ended, which is a legitimate outcome
                };

                let p1 = legal_commands(battle, Player::P1);
                let p2 = legal_commands(battle, Player::P2);
                assert!(
                    !p1.is_empty() && !p2.is_empty(),
                    "{opponent_species:?} seed {seed} turn {turn}: a side has no legal move"
                );

                let (next, _, probability) = sample_turn_raw_seeded(
                    seed.wrapping_mul(31).wrapping_add(turn),
                    &state,
                    &PlayerCommand::Battle(vec![p1[0].clone()]),
                    &PlayerCommand::Battle(vec![p2[0].clone()]),
                    move_dex(),
                    pokemon_dex(),
                    true,
                    16,
                    Some(Player::P1),
                );
                assert!(
                    probability > 0.0,
                    "{opponent_species:?} seed {seed} turn {turn}: zero-probability turn"
                );
                state = next;
            }
        }
    }
}

fn legal_commands(
    battle: &crate::state::battle::BattleState,
    player: Player,
) -> Vec<crate::state::battle::BattleCommand> {
    crate::simulator::get_possible_commands_for_active_slot(
        battle,
        player,
        0,
        move_dex(),
        pokemon_dex(),
    )
}

/// A determinized world must actually be able to switch, which is precisely what
/// `materialize_battle`'s always-empty bench prevents.
#[test]
fn switching_is_legal_when_the_roster_has_a_bench() {
    with_meta!(meta);
    let mut belief = belief_1v1(Species::Charizard, Species::Garchomp, 2);
    belief.p2_possible_back_mons.push(opponent(Species::Incineroar));
    belief.p2_possible_back_mons.push(opponent(Species::Whimsicott));

    let world = determinize_seeded(1, &belief, meta, pokemon_dex(), move_dex(), &config()).unwrap();
    assert_eq!(world.state.p2_back_mons.len(), 2);

    let commands = legal_commands(&world.state, Player::P2);
    assert!(
        commands
            .iter()
            .any(|c| matches!(c, crate::state::battle::BattleCommand::Switch(_))),
        "no switch command is legal despite a populated bench"
    );
}

// ── 10. Ambient-RNG entry point ──────────────────────────────────────────────

#[test]
fn the_unseeded_entry_point_works() {
    with_meta!(meta);
    let belief = belief_1v1(Species::Charizard, Species::Garchomp, 0);
    let world = determinize(&belief, meta, pokemon_dex(), move_dex(), &config()).unwrap();
    assert_eq!(world.state.p2_active_mons[0].species, Species::Garchomp);
}
