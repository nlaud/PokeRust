//! Tests for the meta-driven determinizer.
//!
//! The suite is layered. The pure-math pieces (`information::cps`,
//! `information::compositions`) carry their own unit tests; this file covers the
//! parts that only mean anything once a belief, the usage cache and the engine
//! are all in play:
//!
//! 1. *Soundness* — every determinized world must satisfy the belief it came
//!    from, checked against `subset_check`'s oracle plus `check_determinization`
//!    for the three blind spots that oracle documents.
//! 2. *Runnability* — the TODO's actual requirement, "should just be able to be
//!    put in the simulator and work". Driven by feeding worlds through
//!    `sample_turn_raw_seeded`.
//! 3. *Fidelity* — that the sampled distribution really does follow the usage
//!    data, rather than merely being legal.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::OnceLock;

use crate::data::ability::Ability;
use crate::data::item::Item;
use crate::data::pokemon_move::PokemonMove;
use crate::data::species::Species;
use crate::information::determinize::{
    DeterminizeConfig, DeterminizeError, DeterminizeWarning, check_determinization, determinize,
    determinize_seeded,
};
use crate::information::inference::InferenceConfig;
use crate::information::subset_check::collect_true_state_subset_violations;
use crate::information::unknowns::{
    PokemonHP, Unknown, UnknownBattleState, UnknownMatchState, UnknownPokemonState,
};
use crate::meta::{MetaDex, MetaFormat};
use crate::simulator::sample_turn_raw_seeded;
use crate::state::battle::{MatchState, Player, PlayerCommand};
use crate::state::pokemon::Nature;
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
