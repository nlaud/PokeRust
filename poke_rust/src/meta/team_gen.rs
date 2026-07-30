//! Samples a complete team from usage data.
//!
//! It uses the same sampling rules as the determinizer without a belief.
//! It returns teamsheet values instead of a full battle state.
//! This prevents duplicate stat-point conversion.
//! Team generation enforces the Champions species and item clauses.

use std::collections::{HashMap, HashSet};

use crate::data::ability::Ability;
use crate::data::item::Item;
use crate::data::pokemon_move::PokemonMove;
use crate::data::species::Species;
use crate::information::cps::sample_fixed_size_subset;
use crate::meta::dex::{MetaDex, SpeciesMeta, StatPoints};
use crate::meta::names::ALL_NATURES;
use crate::simulator::helpers::sample_one_weighted;
use crate::simulator::scoped_sample_rng;
use crate::state::dex_data::PokemonData;
use crate::state::pokemon::{Nature, is_mega_dex_entry};
use crate::user::humanize_identifier;

/// Stores one generated teamsheet set.
/// `points` use the authoring scale from zero through 32.
#[derive(Debug, Clone)]
pub struct GeneratedSet {
    pub species: Species,
    pub item: Item,
    pub ability: Ability,
    pub nature: Nature,
    pub points: StatPoints,
    pub moves: Vec<PokemonMove>,
}

#[derive(Debug)]
pub enum TeamGenError {
    /// No dex-selectable species survived filtering the usage cache — an
    /// empty or corrupt `MetaDex`, or one built for a dex with nothing in common.
    NoSelectableSpecies,
}

impl std::fmt::Display for TeamGenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TeamGenError::NoSelectableSpecies => {
                write!(f, "meta usage cache has no selectable species to sample from")
            }
        }
    }
}

impl std::error::Error for TeamGenError {}

/// Sample a full team of `size` Pokemon from usage data, seeded for
/// reproducibility.
///
/// 1. The first Pokemon is drawn by format-wide popularity
///    (`MetaDex::popularity`).
/// 2. Every subsequent slot is drawn from the combined teammate affinity of
///    every mon already on the team — `MetaDex::teammate_score` summed over
///    the roster so far, popularity breaking ties. Mirrors
///    `information::determinize::select_bench_indices`'s scoring.
/// 3. Each mon's item/ability/nature/spread/moves are drawn independently
///    from that species' usage marginals.
///
/// Species clause and item clause are enforced by construction: a species or
/// item already on the team is never offered as a candidate for a later slot.
/// If the cache has fewer selectable species than `size`, the returned team
/// is simply shorter — the caller (the server's roster-length check already
/// applied to pasted teams) is what turns that into a user-facing error.
pub fn generate_meta_team(
    meta_dex: &MetaDex,
    pokemon_dex: &HashMap<Species, PokemonData>,
    learnset_dex: &HashMap<Species, HashSet<PokemonMove>>,
    size: usize,
    seed: u64,
) -> Result<Vec<GeneratedSet>, TeamGenError> {
    let candidates: Vec<Species> = meta_dex
        .species()
        .filter(|s| is_selectable_species(s, pokemon_dex))
        .cloned()
        .collect();
    if candidates.is_empty() {
        return Err(TeamGenError::NoSelectableSpecies);
    }

    // Seeds every `sample_one_weighted` draw below (through `draw_weighted`/
    // `draw_uniform`), so the whole team is reproducible from `seed` alone.
    let _guard = scoped_sample_rng(seed);

    let mut team_species: Vec<Species> = Vec::with_capacity(size);
    let mut used_items: HashSet<Item> = HashSet::new();
    let mut team: Vec<GeneratedSet> = Vec::with_capacity(size);

    for _ in 0..size {
        let remaining: Vec<&Species> = candidates
            .iter()
            .filter(|s| !team_species.contains(s))
            .collect();
        let Some(species) = pick_next_species(meta_dex, &remaining, &team_species) else {
            break;
        };

        team_species.push(species.clone());
        let species_meta = meta_dex.get(&species);

        let item = sample_item(meta_dex, species_meta, &used_items);
        if item != Item::None {
            used_items.insert(item.clone());
        }
        let ability = sample_ability(&species, species_meta, pokemon_dex);
        let nature = sample_nature(species_meta);
        let points = sample_spread(species_meta);
        let moves = sample_moves(&species, species_meta, learnset_dex);

        team.push(GeneratedSet {
            species,
            item,
            ability,
            nature,
            points,
            moves,
        });
    }

    Ok(team)
}

/// Render a generated team as Showdown teamsheet text.
///
/// Every enum name goes through `humanize_identifier`, matching
/// `routes::get_species_list`'s convention — but that's cosmetic, not
/// load-bearing: every `from_str` in `data/` (and `meta::names`' resolvers)
/// normalizes by stripping non-alphanumerics and lowercasing before matching,
/// so spacing here can't break the round trip through
/// `state::pokemon::parse_team_sheet_str`. Tera Type and IVs lines are omitted
/// — the cache carries neither, and the parser's own defaults (Normal, all-31)
/// are exactly what this generator assumes when sampling a spread.
pub fn render_teamsheet(team: &[GeneratedSet]) -> String {
    team.iter()
        .map(render_one)
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn render_one(mon: &GeneratedSet) -> String {
    let species = humanize_identifier(format!("{:?}", mon.species));
    let mut lines = Vec::new();

    if mon.item == Item::None {
        lines.push(species);
    } else {
        let item = humanize_identifier(format!("{:?}", mon.item));
        lines.push(format!("{species} @ {item}"));
    }
    lines.push(format!(
        "Ability: {}",
        humanize_identifier(format!("{:?}", mon.ability))
    ));
    lines.push("Level: 50".to_string());
    if let Some(evs) = render_stat_line(&mon.points) {
        lines.push(format!("EVs: {evs}"));
    }
    // Nature variants are single words, so the exact-cased `{:?}` matches
    // `parse_nature_str`'s literal match arms with no humanizing needed.
    lines.push(format!("{:?} Nature", mon.nature));
    for mv in &mon.moves {
        lines.push(format!("- {}", humanize_identifier(format!("{mv:?}"))));
    }

    lines.join("\n")
}

/// `"20 HP / 12 Atk / ..."`, omitting zero-point stats. `None` when every stat
/// is zero, so a degenerate fallback spread doesn't emit an empty `EVs:` line.
fn render_stat_line(points: &StatPoints) -> Option<String> {
    const LABELS: [&str; 6] = ["HP", "Atk", "Def", "SpA", "SpD", "Spe"];
    let parts: Vec<String> = points
        .iter()
        .zip(LABELS)
        .filter(|(p, _)| **p > 0)
        .map(|(p, label)| format!("{p} {label}"))
        .collect();
    (!parts.is_empty()).then(|| parts.join(" / "))
}

// ── Species selection ────────────────────────────────────────────────────────

/// A species a team can actually lead with: has a dex entry, isn't a
/// battle-only forme (Mega/Terastal-triggered, Zen Mode, ...), isn't a Mega or
/// Gigantamax forme in its own right (those are selected via held item /
/// in-battle command, never listed directly on a teamsheet). Mirrors
/// `routes::get_species_list`'s filter.
fn is_selectable_species(species: &Species, pokemon_dex: &HashMap<Species, PokemonData>) -> bool {
    let Some(data) = pokemon_dex.get(species) else {
        return false;
    };
    if data.battle_only.is_some() {
        return false;
    }
    if is_mega_dex_entry(species, data) {
        return false;
    }
    // Gigantamax formes carry neither `battleOnly` nor a mega `forme` marker —
    // identified by the enum variant's own name, same as
    // `routes::is_gigantamax_dex_entry`.
    if format!("{species:?}").to_lowercase().ends_with("gmax") {
        return false;
    }
    true
}

/// Pick the next teammate: popularity alone for the first slot, teammate
/// affinity summed over the roster so far (popularity breaking ties)
/// thereafter.
fn pick_next_species(
    meta_dex: &MetaDex,
    remaining: &[&Species],
    team_species: &[Species],
) -> Option<Species> {
    if remaining.is_empty() {
        return None;
    }
    let weighted: Vec<(Species, f64)> = if team_species.is_empty() {
        remaining
            .iter()
            .map(|s| ((*s).clone(), meta_dex.popularity(s)))
            .collect()
    } else {
        remaining
            .iter()
            .map(|s| {
                let affinity: f64 = team_species
                    .iter()
                    .map(|k| meta_dex.teammate_score(k, s))
                    .sum();
                // Popularity only breaks ties; co-occurrence is the real signal —
                // same weighting `determinize::select_bench_indices` uses.
                ((*s).clone(), affinity + 0.05 * meta_dex.popularity(s))
            })
            .collect()
    };
    draw_weighted(weighted).or_else(|| remaining.first().map(|s| (*s).clone()))
}

// ── Weighted / uniform draw helpers ─────────────────────────────────────────

/// Draw one option, weighted by its usage share, renormalizing over whatever
/// candidates were actually offered (never dividing by the raw, top-N-truncated
/// 100 — see `meta::dex`'s module doc). Equivalent to
/// `information::determinize::draw_weighted`, which is private to that module.
fn draw_weighted<T: Clone>(candidates: Vec<(T, f64)>) -> Option<T> {
    let total: f64 = candidates.iter().map(|(_, w)| w.max(0.0)).sum();
    if candidates.is_empty() || total <= 0.0 {
        return None;
    }
    let mut drawn = sample_one_weighted(candidates, |(_, w)| w.max(0.0));
    drawn.pop().map(|(value, _)| value)
}

/// Draw uniformly from a fallback domain.
fn draw_uniform<T: Clone>(candidates: Vec<T>) -> Option<T> {
    if candidates.is_empty() {
        return None;
    }
    let mut drawn = sample_one_weighted(candidates, |_| 1.0);
    drawn.pop()
}

/// Draw `n` distinct items uniformly, without replacement.
fn draw_uniform_subset<T: Clone>(mut pool: Vec<T>, n: usize) -> Vec<T> {
    let mut out = Vec::with_capacity(n.min(pool.len()));
    while !pool.is_empty() && out.len() < n {
        let mut drawn = sample_one_weighted(
            pool.iter().cloned().enumerate().collect::<Vec<_>>(),
            |_| 1.0,
        );
        let Some((idx, value)) = drawn.pop() else {
            break;
        };
        out.push(value);
        pool.remove(idx);
    }
    out
}

// ── Per-attribute sampling ───────────────────────────────────────────────────

/// Pick a held item. `used_items` is the item clause: no two Pokemon on the
/// team hold the same item, `Item::None` exempt. Falls back to the format-wide
/// item pool (`MetaDex::item_pool`) when every listed item for this species is
/// already taken, then to `Item::None` if even that pool is exhausted.
fn sample_item(meta_dex: &MetaDex, species_meta: Option<&SpeciesMeta>, used_items: &HashSet<Item>) -> Item {
    let admissible = |item: &Item| *item == Item::None || !used_items.contains(item);

    if let Some(meta) = species_meta {
        let candidates: Vec<(Item, f64)> = meta
            .items
            .iter()
            .filter(|w| admissible(&w.value))
            .map(|w| (w.value.clone(), w.pct))
            .collect();
        if let Some(item) = draw_weighted(candidates) {
            return item;
        }
    }

    let pool: Vec<Item> = meta_dex
        .item_pool()
        .iter()
        .filter(|i| admissible(i))
        .cloned()
        .collect();
    draw_uniform(pool).unwrap_or(Item::None)
}

/// Pick an ability, preferring usage data and falling back to the species' own
/// legal ability slots (a handful of species carry no ability rows at all).
fn sample_ability(
    species: &Species,
    species_meta: Option<&SpeciesMeta>,
    pokemon_dex: &HashMap<Species, PokemonData>,
) -> Ability {
    if let Some(meta) = species_meta {
        let candidates: Vec<(Ability, f64)> =
            meta.abilities.iter().map(|w| (w.value.clone(), w.pct)).collect();
        if let Some(ability) = draw_weighted(candidates) {
            return ability;
        }
    }
    let pool: Vec<Ability> = pokemon_dex
        .get(species)
        .map(|d| d.abilities.clone())
        .unwrap_or_default();
    // Matches `build_pokemon_state`'s own default, so a species with no dex
    // data at least stays internally consistent.
    draw_uniform(pool).unwrap_or(Ability::Illuminate)
}

/// Pick a nature, falling back to a uniform draw over every nature.
fn sample_nature(species_meta: Option<&SpeciesMeta>) -> Nature {
    if let Some(meta) = species_meta {
        let candidates: Vec<(Nature, f64)> =
            meta.natures.iter().map(|w| (w.value, w.pct)).collect();
        if let Some(nature) = draw_weighted(candidates) {
            return nature;
        }
    }
    draw_uniform(ALL_NATURES.to_vec()).unwrap_or(Nature::Hardy)
}

/// Pick a stat-point spread, falling back to no investment at all (a
/// maximum-entropy answer, and never out of range) when the cache has none.
fn sample_spread(species_meta: Option<&SpeciesMeta>) -> StatPoints {
    if let Some(meta) = species_meta {
        let candidates: Vec<(StatPoints, f64)> =
            meta.spreads.iter().map(|w| (w.value, w.pct)).collect();
        if let Some(points) = draw_weighted(candidates) {
            return points;
        }
    }
    [0; 6]
}

/// Pick a 4-move set from marginal inclusion rates.
///
/// Adapted from `information::determinize::sample_moves` with the belief
/// (revealed slots, per-slot exclusions) stripped out — there is no belief
/// here, every slot starts free. See that function for the residual-mass
/// rationale: `moves` are marginal inclusion rates summing to roughly 4x100%,
/// not a distribution, so the slots the listed moves don't collectively
/// account for are filled from off-meta learnset moves rather than being
/// renormalized away.
fn sample_moves(
    species: &Species,
    species_meta: Option<&SpeciesMeta>,
    learnset_dex: &HashMap<Species, HashSet<PokemonMove>>,
) -> Vec<PokemonMove> {
    const SLOTS: usize = 4;
    let learnset = learnset_dex.get(species);

    let listed: Vec<(PokemonMove, f64)> = species_meta
        .map(|m| {
            m.moves
                .iter()
                .filter(|w| learnset.is_none_or(|l| l.contains(&w.value)))
                .map(|w| (w.value.clone(), (w.pct / 100.0).clamp(0.0, 1.0)))
                .collect()
        })
        .unwrap_or_default();

    let listed_mass: f64 = listed.iter().map(|(_, p)| *p).sum();
    let residual = if learnset.is_some() {
        (SLOTS as f64 - listed_mass).max(0.0)
    } else {
        0.0
    };
    let residual_slots = if residual > 0.0 { SLOTS } else { 0 };

    let mut marginals: Vec<f64> = listed.iter().map(|(_, p)| *p).collect();
    for _ in 0..residual_slots {
        marginals.push(residual / residual_slots as f64);
    }

    let mut chosen: Vec<PokemonMove> = Vec::with_capacity(SLOTS);
    let mut off_meta_wanted = 0usize;
    if !marginals.is_empty() {
        let (picked, _probability) = sample_fixed_size_subset(&marginals, SLOTS);
        for index in picked {
            match listed.get(index) {
                Some((m, _)) => chosen.push(m.clone()),
                None => off_meta_wanted += 1,
            }
        }
    } else {
        off_meta_wanted = SLOTS;
    }

    let shortfall = SLOTS.saturating_sub(chosen.len());
    let wanted = off_meta_wanted.max(shortfall);
    if wanted > 0 {
        let taken: HashSet<PokemonMove> = chosen
            .iter()
            .chain(listed.iter().map(|(m, _)| m))
            .cloned()
            .collect();
        let mut pool: Vec<PokemonMove> = learnset
            .map(|l| l.iter().filter(|m| !taken.contains(m)).cloned().collect())
            .unwrap_or_default();
        // Deterministic order before the seeded draw, so a HashSet's iteration
        // order cannot make the pick irreproducible.
        pool.sort_by_key(|m| format!("{m:?}"));
        chosen.extend(draw_uniform_subset(pool, wanted));
    }

    // Last resort: reuse listed moves the learnset filter rejected rather than
    // emitting a moveless Pokemon (mirrors a learnset gap, not a real dex gap).
    if chosen.is_empty()
        && let Some(meta) = species_meta
    {
        chosen.extend(meta.moves.iter().take(SLOTS).map(|w| w.value.clone()));
    }
    chosen.truncate(SLOTS);
    chosen
}
