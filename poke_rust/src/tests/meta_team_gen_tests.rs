//! Tests for the Meta Team Generator (`meta::team_gen`).
//!
//! Per `meta::dex`'s own doc comment, the usage cache's *contents* legitimately
//! change every time the scraper refreshes — percentages move, options enter
//! and leave top-N lists. So these tests assert structural invariants
//! (clauses hold, output round-trips, output is deterministic) rather than
//! anything about which species or sets come out, mirroring
//! `meta::dex`'s and `determinize_tests`'s own testing philosophy.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::OnceLock;

use crate::data::pokemon_move::PokemonMove;
use crate::data::species::Species;
use crate::meta::team_gen::{generate_meta_team, render_teamsheet};
use crate::meta::{MetaDex, MetaFormat};
use crate::state::pokemon::{
    nature_stat_modifiers, parse_team_sheet_str, scale_evs_for_stat_points,
};
use crate::tests::simuilator_test_helpers::{move_dex, pokemon_dex};

// ── Fixtures ─────────────────────────────────────────────────────────────────

/// The usage cache is gitignored and regenerable, so it may not exist. Tests
/// that need it skip rather than fail — otherwise a fresh clone cannot run the
/// suite. Mirrors `determinize_tests::meta_root`.
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

// ── Structural invariants ────────────────────────────────────────────────────

/// A generated set must not pair a nature with a spread that fights it.
///
/// The cache stores natures and spreads as separate marginals, so drawing the
/// two independently — as this generator did — leaves nothing to rule out a
/// minus-Attack nature over 32 Attack points. Conditioning the spread on the
/// nature is what removes that, and this measures the result end to end.
///
/// Measured 11.1% with the conditioning disabled, 5.3% with it on, so the
/// threshold sits between them. Note that 11.1% is *not* the 19.4% figure in
/// `TODO.md`: that one is a flat average across 235 species, while a generated
/// team draws species by popularity and lands on better-behaved spread tables.
///
/// This is a rate, not a structural invariant, so the threshold is loose enough
/// to survive a cache refresh.
///
/// The predicate is re-derived in EV units rather than calling the sampler's
/// own. `ev = max(0, 8p - 4)`, so 8 authoring points is 60 EVs and 0 stays 0.
#[test]
fn generated_sets_do_not_fight_their_own_nature() {
    with_meta!(meta);
    const TEAMS: u64 = 200;

    let mut total = 0usize;
    let mut incoherent = 0usize;
    for seed in 0..TEAMS {
        let team = generate_meta_team(meta, pokemon_dex(), learnset_dex(), 6, seed)
            .unwrap_or_else(|e| panic!("seed {seed}: {e}"));
        for mon in &team {
            let evs = scale_evs_for_stat_points(mon.points);
            total += 1;
            let fights = nature_stat_modifiers(&mon.nature)
                .iter()
                .enumerate()
                .any(|(i, m)| (*m < 1.0 && evs[i + 1] >= 60) || (*m > 1.0 && evs[i + 1] == 0));
            if fights {
                incoherent += 1;
            }
        }
    }

    let rate = incoherent as f64 / total as f64 * 100.0;
    assert!(
        rate < 8.0,
        "{rate:.2}% of generated sets fight their own nature, against 5.3% when \
         the spread is conditioned on the nature and 11.1% when it is not"
    );
}

#[test]
fn generates_a_full_team_honoring_both_clauses() {
    with_meta!(meta);
    for seed in [1u64, 2, 3, 42, 12345] {
        let team = generate_meta_team(meta, pokemon_dex(), learnset_dex(), 6, seed)
            .unwrap_or_else(|e| panic!("seed {seed}: {e}"));

        assert_eq!(team.len(), 6, "seed {seed}: expected a full 6-mon team");

        let species: HashSet<&Species> = team.iter().map(|m| &m.species).collect();
        assert_eq!(species.len(), 6, "seed {seed}: species clause violated (duplicate species)");

        let items: Vec<_> = team
            .iter()
            .filter(|m| m.item != crate::data::item::Item::None)
            .map(|m| &m.item)
            .collect();
        let unique_items: HashSet<_> = items.iter().collect();
        assert_eq!(
            items.len(),
            unique_items.len(),
            "seed {seed}: item clause violated (duplicate held item)"
        );

        for mon in &team {
            assert!(pokemon_dex().contains_key(&mon.species), "seed {seed}: {:?} not in dex", mon.species);
            assert!(
                !mon.moves.is_empty() && mon.moves.len() <= 4,
                "seed {seed}: {:?} has {} moves",
                mon.species,
                mon.moves.len()
            );
            let unique_moves: HashSet<_> = mon.moves.iter().collect();
            assert_eq!(unique_moves.len(), mon.moves.len(), "seed {seed}: {:?} repeats a move", mon.species);
            if let Some(learnset) = learnset_dex().get(&mon.species) {
                for mv in &mon.moves {
                    assert!(
                        learnset.contains(mv),
                        "seed {seed}: {:?} does not learn {:?}",
                        mon.species,
                        mv
                    );
                }
            }
            let total_points: u32 = mon.points.iter().map(|p| *p as u32).sum();
            assert!(
                mon.points.iter().all(|p| *p <= 32) && total_points <= 66,
                "seed {seed}: {:?} spread {:?} out of range",
                mon.species,
                mon.points
            );
        }
    }
}

#[test]
fn same_seed_yields_the_same_team() {
    with_meta!(meta);
    let a = generate_meta_team(meta, pokemon_dex(), learnset_dex(), 6, 777).unwrap();
    let b = generate_meta_team(meta, pokemon_dex(), learnset_dex(), 6, 777).unwrap();

    let species_a: Vec<_> = a.iter().map(|m| m.species.clone()).collect();
    let species_b: Vec<_> = b.iter().map(|m| m.species.clone()).collect();
    assert_eq!(species_a, species_b, "same seed should draw the same roster");

    // Rendering is a pure function of the draw, so the same seed must also
    // render to byte-identical text.
    assert_eq!(render_teamsheet(&a), render_teamsheet(&b));
}

#[test]
fn different_seeds_usually_differ() {
    with_meta!(meta);
    let sheets: HashSet<String> = (0..8)
        .map(|seed| {
            let team = generate_meta_team(meta, pokemon_dex(), learnset_dex(), 6, seed).unwrap();
            render_teamsheet(&team)
        })
        .collect();
    assert!(sheets.len() > 1, "8 different seeds produced only one distinct team");
}

/// The core correctness check: a rendered team must parse back into a
/// `PokemonState` list matching what was generated. Exercises the exact path
/// `routes::resolve_team_text` puts a generated team through.
#[test]
fn rendered_teamsheet_round_trips_through_the_real_parser() {
    with_meta!(meta);
    let team = generate_meta_team(meta, pokemon_dex(), learnset_dex(), 6, 99).unwrap();
    let sheet_text = render_teamsheet(&team);

    let parsed = parse_team_sheet_str(&sheet_text, pokemon_dex(), move_dex(), true);
    assert_eq!(parsed.len(), team.len(), "every generated mon should parse back");

    for (generated, mon) in team.iter().zip(parsed.iter()) {
        assert_eq!(mon.species, generated.species);
        assert_eq!(mon.item, generated.item);
        assert_eq!(mon.ability, generated.ability);
        assert_eq!(mon.nature, generated.nature);
        let parsed_moves: Vec<PokemonMove> = mon.moves.iter().flatten().cloned().collect();
        assert_eq!(parsed_moves, generated.moves);
    }
}

#[test]
fn shorter_teams_are_supported() {
    with_meta!(meta);
    let team = generate_meta_team(meta, pokemon_dex(), learnset_dex(), 4, 5).unwrap();
    assert_eq!(team.len(), 4);
}

/// A loose statistical check that teammate affinity actually biases the draw:
/// species that co-occur heavily with an early pick should show up on the
/// generated team noticeably more often than a same-format species with no
/// recorded affinity at all.
#[test]
fn teammate_affinity_biases_the_later_picks() {
    with_meta!(meta);
    let Some(anchor) = meta.species().next().cloned() else { return };
    let Some(anchor_meta) = meta.get(&anchor) else { return };
    let Some(top_teammate) = anchor_meta.teammates.first().map(|w| w.value.clone()) else {
        return; // nothing to compare against for this particular anchor
    };

    // A species meta never lists as anyone's teammate co-occurs with nothing.
    let Some(stranger) = meta
        .species()
        .find(|s| **s != anchor && **s != top_teammate && meta.teammate_score(&anchor, s) == 0.0)
        .cloned()
    else {
        return;
    };

    let mut top_hits = 0u32;
    let mut stranger_hits = 0u32;
    let trials = 60u64;
    for seed in 0..trials {
        let team = generate_meta_team(meta, pokemon_dex(), learnset_dex(), 6, seed * 991 + 1).unwrap();
        if team.first().map(|m| &m.species) != Some(&anchor) {
            continue; // only compare draws that actually anchored on this species
        }
        if team.iter().any(|m| m.species == top_teammate) {
            top_hits += 1;
        }
        if team.iter().any(|m| m.species == stranger) {
            stranger_hits += 1;
        }
    }
    // Only meaningful once the anchor was actually drawn first a few times;
    // otherwise this is testing sample noise, not the generator.
    if top_hits + stranger_hits >= 5 {
        assert!(
            top_hits >= stranger_hits,
            "top-ranked teammate ({top_hits} hits) should co-occur at least as often as \
             an unrelated species ({stranger_hits} hits) across {trials} seeds"
        );
    }
}

/// Not an assertion — a human-readable sample for eyeballing sanity, since
/// there is no preview UI in front of this generator. Run with
/// `cargo test rendered_sample_looks_sane -- --nocapture`.
#[test]
fn rendered_sample_looks_sane() {
    with_meta!(meta);
    let team = generate_meta_team(meta, pokemon_dex(), learnset_dex(), 6, 2026).unwrap();
    println!("{}", render_teamsheet(&team));
}
