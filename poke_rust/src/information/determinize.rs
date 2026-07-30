//! Converts a fog-of-war belief to one playable `BattleState`.
//!
//! Inference produces sets and numeric bounds.
//! `simulate_turn` requires one complete team.
//! The determinizer samples a permitted world from the usage cache in `crate::meta`.
//!
//! # Consistency
//!
//! The belief filters each choice before the code applies usage weights.
//! Thus, poor usage data changes probability but does not permit an invalid world.
//! `information::subset_check` checks this rule.
//! `check_determinization` also checks EVs, IVs, HP, and revealed move slots.
//!
//! # Sampling
//!
//! The API makes one seeded draw and does not enumerate worlds.
//! One Pokémon can have about 750,000 permitted builds.
//! Four opponents can produce more than 10^23 combinations.
//!
//! `probability` is the joint probability of the draw sequence.
//! Different draw sequences can produce the same state.
//! Compare probabilities only between draws from one belief.
//!
//! # Independence
//!
//! The sampler treats item, ability, nature, spread, and moves as independent for one species.
//! The cache contains only marginal rates.
//! Real builds can contain correlations that the cache does not describe.
//!
//! Two coherence controls reduce the weight of conflicting nature and spread pairs.
//! They keep each reported nature rate unchanged.
//! The sampler does not model item and move correlations.
//! These limits affect selection probability, not soundness.

use std::collections::{HashMap, HashSet};

use crate::data::ability::Ability;
use crate::data::item::Item;
use crate::data::pokemon_move::PokemonMove;
use crate::data::species::Species;
use crate::information::compositions::sample_bounded_composition;
use crate::information::cps::{max_exact_pool, sample_fixed_size_subset};
use crate::information::inference::{
    InferenceConfig, percent_bucket, promote_illusion_to_primary, unknown_is_excluded,
};
use crate::information::subset_check::collect_true_state_subset_violations;
use crate::information::unknowns::{
    PokemonHP, Unknown, UnknownBattleState, UnknownMatchState, UnknownPokemonState,
    is_illusion_capable_species,
};
use crate::meta::dex::{MetaDex, SpeciesMeta, StatPoints};
use crate::meta::names::ALL_NATURES;
use crate::simulator::helpers::{sample_one_weighted, transform_into};
use crate::simulator::scoped_sample_rng;
use crate::state::battle::{BattleState, Player};
use crate::state::dex_data::{MoveData, PokemonData, PokemonType};
use crate::state::pokemon::{
    Nature, PokemonState, PokemonStatsTable, build_pokemon_state, calc_hp, calc_stat,
    calc_stats_for_level, nature_stat_modifiers, scale_evs_for_stat_points,
};

/// The competitive stat-point budget. Every authored spread in the cache spends
/// at most this, and the overwhelming majority spend exactly it.
pub const STAT_POINT_BUDGET: u32 = 66;

/// Per-stat ceiling in authoring units.
pub const MAX_POINTS_PER_STAT: u8 = 32;

/// Default damping for a nature that lowers a stat the build invests in.
///
/// Named because `meta::team_gen` samples without a `DeterminizeConfig` and
/// still needs the same weights. See `coherence` for where the number is from.
pub const DEFAULT_NATURE_LOWERS_INVESTED: f64 = 0.10;

/// Default damping for a nature that raises a stat with no points.
pub const DEFAULT_NATURE_RAISES_UNUSED: f64 = 0.35;

/// How an unresolved field timer is filled in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimerPolicy {
    /// Uses the largest permitted remaining duration.
    MaxPlausible,
    /// Draw uniformly from the remaining possibilities.
    SampleUniform,
    /// Always this many turns.
    Fixed(u8),
}

pub struct DeterminizeConfig {
    /// Inference rules that the sampled world must follow.
    pub inference: InferenceConfig,
    /// Player whose belief supplies the world.
    /// Copies this player's known team and samples the opponent.
    pub observer: Player,
    /// Creates plausible bench Pokémon when the belief has too few candidates.
    pub invent_missing_bench: bool,
    /// How many times to redraw a world that violates a cross-Pokemon constraint
    /// before accepting it with a warning.
    pub max_repair_passes: u8,
    /// Damping for a nature that lowers a stat the build invests in.
    /// The value stays from zero through one.
    /// One disables this rule.
    pub nature_lowers_invested: f64,
    /// Damping for a nature that raises a stat with no points.
    /// The value stays from zero through one.
    /// One disables this rule.
    pub nature_raises_unused: f64,
    pub timer_policy: TimerPolicy,
    /// When false, an attribute whose meta options are all excluded is an error
    /// rather than a uniform draw.
    pub allow_uniform_fallback: bool,
}

impl Default for DeterminizeConfig {
    fn default() -> Self {
        DeterminizeConfig {
            inference: InferenceConfig::default(),
            observer: Player::P1,
            invent_missing_bench: true,
            max_repair_passes: 8,
            nature_lowers_invested: DEFAULT_NATURE_LOWERS_INVESTED,
            nature_raises_unused: DEFAULT_NATURE_RAISES_UNUSED,
            timer_policy: TimerPolicy::MaxPlausible,
            allow_uniform_fallback: true,
        }
    }
}

/// Reports a sample that used weaker evidence.
/// The returned world remains consistent with the belief.
#[derive(Debug, Clone, PartialEq)]
pub enum DeterminizeWarning {
    /// Every meta option for this attribute was excluded by the belief, so it
    /// was drawn uniformly from the legal domain instead.
    UniformFallback {
        mon_idx: usize,
        attribute: &'static str,
        reason: String,
    },
    /// The species is not in the usage cache at all, so nothing about it is
    /// meta-informed.
    NoMetaEntry { mon_idx: usize, species: Species },
    /// Move slots were filled from the learnset because the meta list ran out.
    LearnsetTopUp {
        mon_idx: usize,
        moves: Vec<PokemonMove>,
    },
    /// An opponent Pokemon was invented to fill a bench the belief could not.
    InventedBenchMon { species: Species },
    /// The belief said this Pokemon was Transformed, but the engine refused the
    /// splice (`transform_into` rejects an Illusion/Imposter copy target). The
    /// mon is returned in its real, untransformed form rather than as a
    /// half-applied hybrid.
    TransformNotApplied { mon_idx: usize, species: Species },
    /// A slot was drawn as the side's disguised Zoroark, but the disguise could
    /// not be made self-consistent — the decoy species is not on the concrete
    /// bench for `compute_illusion_disguise` to find. The Zoroark's true
    /// identity still stands; only the appearance is dropped.
    IllusionUncommitted { decoy: Species },
    /// A cross-Pokemon constraint (item clause, speed ordering, a CNF clause)
    /// still failed after the redraw budget was exhausted. The world is returned
    /// anyway: a warned-but-usable world beats an error in exactly the mid-game
    /// situations where beliefs are richest.
    UnsatisfiedConstraint { detail: String },
}

#[derive(Debug, Clone, PartialEq)]
pub enum DeterminizeError {
    /// The species is not pinned down and has no enumerable candidate set, so
    /// there is nothing to build.
    SpeciesUndetermined { mon_idx: usize },
    /// No move source at all: no meta entry, no learnset, no revealed moves.
    /// Deliberately fatal — a moveless Pokemon produces an empty legal-command
    /// set and fails far away from here.
    NoLegalMoves { mon_idx: usize, species: Species },
    /// The EV/stat bounds admit no assignment.
    InfeasibleSpread { mon_idx: usize, species: Species },
    /// An attribute was exhausted and `allow_uniform_fallback` is off.
    NoCandidates {
        mon_idx: usize,
        attribute: &'static str,
    },
}

impl std::fmt::Display for DeterminizeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DeterminizeError::SpeciesUndetermined { mon_idx } => {
                write!(f, "mon {mon_idx}: species is not determined")
            }
            DeterminizeError::NoLegalMoves { mon_idx, species } => {
                write!(f, "mon {mon_idx} ({species:?}): no legal moves available")
            }
            DeterminizeError::InfeasibleSpread { mon_idx, species } => write!(
                f,
                "mon {mon_idx} ({species:?}): EV/stat bounds admit no spread"
            ),
            DeterminizeError::NoCandidates { mon_idx, attribute } => write!(
                f,
                "mon {mon_idx}: no candidates for {attribute} and uniform fallback is disabled"
            ),
        }
    }
}

impl std::error::Error for DeterminizeError {}

/// Running total of how likely the choices made so far were.
#[derive(Debug, Default)]
pub(crate) struct DrawLog {
    pub probability: f64,
    pub warnings: Vec<DeterminizeWarning>,
}

impl DrawLog {
    pub(crate) fn new() -> Self {
        DrawLog {
            probability: 1.0,
            warnings: Vec::new(),
        }
    }

    fn observe(&mut self, p: f64) {
        if p.is_finite() && p > 0.0 {
            self.probability *= p;
        }
    }
}

// ── Weighted categorical draw ────────────────────────────────────────────────

/// Draw one option, weighted by its usage share.
///
/// The renormalization is the whole "ignore the residual" rule: divide by the
/// sum of what actually survived filtering, never by 100. The listed options in
/// the cache are top-N truncated, so their raw percentages fall short of 100 and
/// the remainder belongs to unlisted options we are deliberately not
/// considering. Dividing by the true surviving sum also makes the arithmetic
/// immune to the site's rounding, which pushes some categories slightly over 100.
fn draw_weighted<T: Clone>(candidates: Vec<(T, f64)>, log: &mut DrawLog) -> Option<T> {
    let total: f64 = candidates.iter().map(|(_, w)| w.max(0.0)).sum();
    if candidates.is_empty() || total <= 0.0 {
        return None;
    }
    let mut drawn = sample_one_weighted(candidates, |(_, w)| w.max(0.0));
    let (value, weight) = drawn.pop()?;
    log.observe(weight.max(0.0) / total);
    Some(value)
}

/// Draw uniformly from a fallback domain.
fn draw_uniform<T: Clone>(candidates: Vec<T>, log: &mut DrawLog) -> Option<T> {
    let n = candidates.len();
    if n == 0 {
        return None;
    }
    let mut drawn = sample_one_weighted(candidates, |_| 1.0);
    let value = drawn.pop()?;
    log.observe(1.0 / n as f64);
    Some(value)
}

// ── Item ─────────────────────────────────────────────────────────────────────

/// Pick a held item.
///
/// `used_items` carries the item clause: under Champions rules no two Pokemon on
/// a team hold the same item, so anything already assigned to a teammate is
/// removed before the weights are renormalized. `Item::None` is exempt — any
/// number of Pokemon may hold nothing.
pub(crate) fn sample_item(
    mon_idx: usize,
    unk: &UnknownPokemonState,
    meta: Option<&SpeciesMeta>,
    used_items: &HashSet<Item>,
    cfg: &DeterminizeConfig,
    log: &mut DrawLog,
) -> Result<Item, DeterminizeError> {
    // A revealed item is not a choice.
    if let crate::information::unknowns::Unknown::Known(item) = &unk.item {
        return Ok(item.clone());
    }

    let admissible = |item: &Item| {
        !unknown_is_excluded(&unk.item, item)
            && cfg.inference.legal_item_ok(item)
            && (*item == Item::None
                || cfg.inference.allow_repeat_items
                || !used_items.contains(item))
    };

    if let Some(meta) = meta {
        let candidates: Vec<(Item, f64)> = meta
            .items
            .iter()
            .filter(|w| admissible(&w.value))
            .map(|w| (w.value.clone(), w.pct))
            .collect();
        if let Some(item) = draw_weighted(candidates, log) {
            return Ok(item);
        }
    }

    // Fallback: the union of every item anyone runs in this format. Legal by
    // construction, and far tighter than enumerating the ~1,000-variant enum,
    // most of which is unobtainable in competitive play.
    if !cfg.allow_uniform_fallback {
        return Err(DeterminizeError::NoCandidates {
            mon_idx,
            attribute: "item",
        });
    }
    let pool: Vec<Item> = cfg
        .inference
        .legal_items
        .as_ref()
        .map(|l| l.iter().cloned().collect::<Vec<_>>())
        .unwrap_or_else(|| meta_item_pool(meta))
        .into_iter()
        .filter(admissible)
        .collect();

    log.warnings.push(DeterminizeWarning::UniformFallback {
        mon_idx,
        attribute: "item",
        reason: "every listed item was excluded by the belief".to_string(),
    });
    Ok(draw_uniform(pool, log).unwrap_or(Item::None))
}

fn meta_item_pool(meta: Option<&SpeciesMeta>) -> Vec<Item> {
    meta.map(|m| m.items.iter().map(|w| w.value.clone()).collect())
        .unwrap_or_default()
}

// ── Ability ──────────────────────────────────────────────────────────────────

/// Pick an ability, preferring usage data and falling back to the species' own
/// legal ability slots.
///
/// `lattice` selects which constraint to draw against: `possible_abilities` for
/// the live ability, `possible_original_abilities` for the pre-change one.
pub(crate) fn sample_ability(
    mon_idx: usize,
    species: &Species,
    meta: Option<&SpeciesMeta>,
    pokemon_dex: &HashMap<Species, PokemonData>,
    cfg: &DeterminizeConfig,
    log: &mut DrawLog,
    lattice: &crate::information::unknowns::Unknown<Ability>,
) -> Result<Ability, DeterminizeError> {
    if let crate::information::unknowns::Unknown::Known(ability) = lattice {
        return Ok(ability.clone());
    }

    if let Some(meta) = meta {
        let candidates: Vec<(Ability, f64)> = meta
            .abilities
            .iter()
            .filter(|w| !unknown_is_excluded(lattice, &w.value))
            .map(|w| (w.value.clone(), w.pct))
            .collect();
        if let Some(ability) = draw_weighted(candidates, log) {
            return Ok(ability);
        }
    }

    // Three species in the cache have no ability rows at all, and the dex's
    // slot list is the legally correct domain anyway — it is what the inference
    // engine seeds `possible_abilities` from.
    let pool: Vec<Ability> = pokemon_dex
        .get(species)
        .map(|d| d.abilities.clone())
        .unwrap_or_default()
        .into_iter()
        .filter(|a| !unknown_is_excluded(lattice, a))
        .collect();

    if pool.is_empty() && !cfg.allow_uniform_fallback {
        return Err(DeterminizeError::NoCandidates {
            mon_idx,
            attribute: "ability",
        });
    }
    log.warnings.push(DeterminizeWarning::UniformFallback {
        mon_idx,
        attribute: "ability",
        reason: "no listed ability survived the belief".to_string(),
    });
    // `Ability::Illuminate` matches `build_pokemon_state`'s own default, so a
    // species with no dex data at least stays internally consistent.
    Ok(draw_uniform(pool, log).unwrap_or(Ability::Illuminate))
}

// ── Nature and EV spread, jointly ────────────────────────────────────────────

/// A fully-resolved build: nature plus the EVs and IVs implied by one spread.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SpreadCandidate {
    pub nature: Nature,
    /// Authoring units, 0..=32 per stat.
    pub points: StatPoints,
    /// Scaled EVs, 0..=252 — what `PokemonState.evs` holds.
    pub evs: [u8; 6],
    pub ivs: [u8; 6],
    pub stats: PokemonStatsTable,
    pub weight: f64,
}

/// Enumerate every (nature, spread) pair the belief admits.
///
/// Nature and spread must be filtered *together*: the belief bounds both the
/// pre-nature stat and the final stat, and the nature multiplier is what relates
/// them. Filtering separately would admit pairs that individually satisfy their
/// own bounds while jointly violating the stats. At most 10 x 12 = 120 cells, so
/// exhaustive evaluation is cheap.
pub(crate) fn enumerate_nature_spreads(
    unk: &UnknownPokemonState,
    base_stats: [u16; 6],
    meta: Option<&SpeciesMeta>,
    cfg: &DeterminizeConfig,
) -> Vec<SpreadCandidate> {
    let natures: Vec<(Nature, f64)> = match meta {
        Some(m) if !m.natures.is_empty() => m
            .natures
            .iter()
            .map(|w| (w.value, w.pct))
            .filter(|(n, _)| !unknown_is_excluded(&unk.possible_natures, n))
            .collect(),
        _ => Vec::new(),
    };
    let spreads: Vec<(StatPoints, f64)> = match meta {
        Some(m) => m.spreads.iter().map(|w| (w.value, w.pct)).collect(),
        None => Vec::new(),
    };
    if natures.is_empty() || spreads.is_empty() {
        return Vec::new();
    }

    let mut out = Vec::new();
    for (nature, nature_pct) in &natures {
        // Coherence is a statement about P(spread | nature), not about P(nature).
        // Folding it into the joint product would shift mass *between* natures,
        // since natures differ in how many of their cells get damped — and the
        // nature marginal is exactly what the usage data pins down. Normalizing
        // each nature's row on its own suppresses incoherent pairs while leaving
        // that marginal at the rate the cache reports.
        //
        // This also removes a distortion that predates coherence:
        // `build_spread_candidate` feasibility is itself nature-dependent (it
        // checks pre-nature *and* post-nature stat bounds), so an unnormalized
        // joint product already skewed the marginal on any constrained belief.
        let mut row: Vec<(SpreadCandidate, f64)> = spreads
            .iter()
            .filter_map(|(points, spread_pct)| {
                let mut candidate =
                    build_spread_candidate(unk, base_stats, *nature, *points, cfg)?;
                candidate.weight = spread_pct
                    * coherence(
                        *nature,
                        points,
                        cfg.nature_lowers_invested,
                        cfg.nature_raises_unused,
                    );
                Some((candidate, *spread_pct))
            })
            .collect();
        if row.is_empty() {
            // The belief admits no spread for this nature. Dropping it lets
            // `draw_weighted` renormalize over the survivors, which is the
            // correct belief-driven shift.
            continue;
        }
        let mut row_total: f64 = row.iter().map(|(c, _)| c.weight).sum();
        if row_total <= 0.0 {
            // Every feasible spread here is incoherent *and* the config is 0.0.
            // Re-weight on plain usage rather than deleting a nature the data
            // says people run — we just cannot say which spread they pair it
            // with, which is a reason to stay agnostic, not to rule it out.
            for (candidate, spread_pct) in &mut row {
                candidate.weight = *spread_pct;
            }
            row_total = row.iter().map(|(c, _)| c.weight).sum();
            if row_total <= 0.0 {
                continue;
            }
        }
        for (mut candidate, _) in row {
            candidate.weight = candidate.weight / row_total * nature_pct;
            out.push(candidate);
        }
    }
    out
}

/// Damping for builds whose nature fights their investment.
///
/// A Bold (−Atk) nature alongside 32 Attack points is a combination essentially
/// nobody runs, but the independent product model has no way to know that — the
/// cache reports natures and spreads as separate marginals. The `stat_up` /
/// `stat_down` columns do carry the signal, so this is recoverable, and the
/// nature's own enum already encodes it (`nature_stat_modifiers`).
///
/// Two faults, two factors. Lowering an invested stat is the rarer fault in the
/// cache — 3.9 of the 19.4 incoherent percentage points across 235 species,
/// against 12.9 for raising an unused one — so it is damped harder. A build
/// committing both earns both factors. They are separate evidence, and the 2.6
/// points that split leaves over are exactly that both-fault group.
///
/// This is applied *within* a nature's row by `enumerate_nature_spreads`, never
/// across natures — see the comment there for why. `1.0` disables a rule.
///
/// The factors arrive as plain numbers rather than a `&DeterminizeConfig`
/// because `meta::team_gen` samples the same way with no belief and no config.
pub(crate) fn coherence(
    nature: Nature,
    points: &StatPoints,
    lowers_invested: f64,
    raises_unused: f64,
) -> f64 {
    // Clamped, not just floored: a value above 1.0 would *boost* incoherent
    // builds, which is the opposite of what these knobs are for.
    let lowers_invested = lowers_invested.clamp(0.0, 1.0);
    let raises_unused = raises_unused.clamp(0.0, 1.0);
    if (lowers_invested - 1.0).abs() < f64::EPSILON && (raises_unused - 1.0).abs() < f64::EPSILON {
        return 1.0;
    }
    let modifiers = nature_stat_modifiers(&nature);
    let mut factor = 1.0;
    for (i, modifier) in modifiers.iter().enumerate() {
        let points_here = points[i + 1]; // modifiers are [atk..spe]; points are [hp, atk..spe]
        if *modifier < 1.0 && points_here >= 8 {
            factor *= lowers_invested; // nerfing a stat the build invests in
        }
        if *modifier > 1.0 && points_here == 0 {
            factor *= raises_unused; // boosting a stat the build ignores
        }
    }
    factor
}

/// Whether every entry of `values` lies within the matching `lo`/`hi` bounds.
fn within<T: PartialOrd>(values: &[T; 6], lo: &[T; 6], hi: &[T; 6]) -> bool {
    values
        .iter()
        .zip(lo.iter())
        .zip(hi.iter())
        .all(|((v, l), h)| v >= l && v <= h)
}

/// Check one (nature, spread) pair against every bound the belief carries.
///
/// Rejections are ordered cheapest-first. Both the pre-nature and the final stat
/// bounds are checked: Pass 3 tightens them independently, and because
/// `calc_stat` floors after the nature multiply, neither implies the other.
fn build_spread_candidate(
    unk: &UnknownPokemonState,
    base_stats: [u16; 6],
    nature: Nature,
    raw_points: StatPoints,
    cfg: &DeterminizeConfig,
) -> Option<SpreadCandidate> {
    // Clamp before scaling: `scale_evs_for_stat_points` casts through `as u8`,
    // so 33 points would silently become 4 EVs.
    let mut points = raw_points;
    for p in &mut points {
        *p = (*p).min(MAX_POINTS_PER_STAT);
    }

    let evs = if cfg.inference.use_stat_points {
        scale_evs_for_stat_points(points)
    } else {
        points
    };

    // EV bounds are in scaled units, matching `PokemonState.evs`.
    if !within(&evs, &unk.min_evs, &unk.max_evs) {
        return None;
    }
    // Check the budget in the unit the build was authored in.
    //
    // Under stat points the real constraint is 66 points, and it does NOT
    // translate to any fixed EV total: `ev = max(0, 8p − 4)` charges the −4 once
    // per *nonzero* stat, so a fully-spent 66-point spread totals `528 − 4k` EVs
    // across k invested stats — 516 over three, 504 over six. Comparing scaled
    // EVs against a single cap therefore rejects legal builds: the 510 default
    // (a Gen-6 convention, and below the 516 floor a 66-point spread hits)
    // rejected every fully-spent spread in the cache, leaving Garchomp with
    // exactly one feasible spread out of eight and identical EVs on every draw.
    if cfg.inference.use_stat_points {
        let spent: u16 = points.iter().map(|p| u16::from(*p)).sum();
        if spent > STAT_POINT_BUDGET as u16 {
            return None;
        }
    } else if let Some(cap) = cfg.inference.ev_total_cap {
        let total: u16 = evs.iter().map(|e| u16::from(*e)).sum();
        if total > cap {
            return None;
        }
    }

    let mut ivs = [31u8; 6];
    for ((iv, min), max) in ivs
        .iter_mut()
        .zip(unk.min_ivs.iter())
        .zip(unk.max_ivs.iter())
    {
        if min > max {
            return None;
        }
        if cfg.inference.force_max_ivs {
            if *min > 31 || *max < 31 {
                return None;
            }
        } else {
            *iv = 31u8.clamp(*min, *max);
        }
    }

    let level = unk.level.max(1);

    // Pre-nature values: HP has no nature term, so index 0 is already final.
    let mut pre = [0u16; 6];
    pre[0] = calc_hp(base_stats[0], ivs[0], evs[0], level);
    for i in 1..6 {
        pre[i] = calc_stat(base_stats[i], ivs[i], evs[i], level, 1.0);
    }
    if !within(&pre, &unk.min_pre_nature_stat, &unk.max_pre_nature_stat) {
        return None;
    }

    let stats = calc_stats_for_level(base_stats, ivs, evs, level, &nature);
    if !within(&stats, &unk.min_stats, &unk.max_stats) {
        return None;
    }

    // A max-HP hypothesis incompatible with the observed HP percentage is not a
    // viable build for this Pokemon at all.
    if let PokemonHP::Percent(pct) = unk.hp {
        percent_bucket(pct, stats[0])?;
    }

    Some(SpreadCandidate {
        nature,
        points,
        evs,
        ivs,
        stats,
        weight: 0.0,
    })
}

/// Pick a (nature, spread) pair, falling back to a uniform legal spread.
pub(crate) fn sample_nature_and_spread(
    mon_idx: usize,
    unk: &UnknownPokemonState,
    species: &Species,
    base_stats: [u16; 6],
    meta: Option<&SpeciesMeta>,
    cfg: &DeterminizeConfig,
    log: &mut DrawLog,
) -> Result<SpreadCandidate, DeterminizeError> {
    let candidates = enumerate_nature_spreads(unk, base_stats, meta, cfg);
    if !candidates.is_empty() {
        let weighted: Vec<(SpreadCandidate, f64)> =
            candidates.into_iter().map(|c| (c.clone(), c.weight)).collect();
        if let Some(chosen) = draw_weighted(weighted, log) {
            return Ok(chosen);
        }
    }

    if !cfg.allow_uniform_fallback {
        return Err(DeterminizeError::NoCandidates {
            mon_idx,
            attribute: "nature_spread",
        });
    }
    log.warnings.push(DeterminizeWarning::UniformFallback {
        mon_idx,
        attribute: "nature_spread",
        reason: "no listed (nature, spread) pair satisfied the belief".to_string(),
    });
    sample_uniform_spread(mon_idx, unk, species, base_stats, cfg, log)
}

/// Uniform fallback: any nature the belief allows, plus a legal point
/// allocation drawn under the coherence weights.
///
/// This only runs once the belief has excluded every authored spread, which
/// means its stat bounds are tight and the usage data has nothing left to
/// contribute. The result looks unlike a real EV spread — smeared rather than
/// lumpy — and that is the honest maximum-entropy answer at that point.
///
/// Maximum entropy over *legal* spreads, though, not over incoherent ones: the
/// belief having ruled out the authored spreads says nothing about whether
/// people pair a minus-Attack nature with 32 Attack points. That evidence is
/// independent of the stat bounds, so the fallback keeps using it.
fn sample_uniform_spread(
    mon_idx: usize,
    unk: &UnknownPokemonState,
    species: &Species,
    base_stats: [u16; 6],
    cfg: &DeterminizeConfig,
    log: &mut DrawLog,
) -> Result<SpreadCandidate, DeterminizeError> {
    let natures: Vec<Nature> = ALL_NATURES
        .iter()
        .copied()
        .filter(|n| !unknown_is_excluded(&unk.possible_natures, n))
        .collect();
    if natures.is_empty() {
        return Err(DeterminizeError::InfeasibleSpread {
            mon_idx,
            species: species.clone(),
        });
    }

    let (lo, hi) = point_bounds(unk, cfg);

    // Try each admissible nature in random order, and within a nature try the
    // full budget before a relaxed one. The stat bounds can rule out a nature
    // entirely, so this must not commit to the first draw.
    let mut nature_order = natures.clone();
    nature_order = shuffle(nature_order);

    for nature in &nature_order {
        for budget in candidate_budgets(&lo, &hi) {
            // Collect the feasible allocations before picking one. Returning the
            // first hit, as this did, leaves coherence nothing to express a
            // preference over.
            let mut pool: Vec<(SpreadCandidate, f64)> = Vec::new();
            for _ in 0..8 {
                let Some(points) = sample_bounded_composition(&lo, &hi, budget) else {
                    break;
                };
                let mut arr = [0u8; 6];
                for (i, v) in points.iter().enumerate() {
                    arr[i] = *v as u8;
                }
                if let Some(mut candidate) =
                    build_spread_candidate(unk, base_stats, *nature, arr, cfg)
                {
                    candidate.weight = coherence(
                        *nature,
                        &arr,
                        cfg.nature_lowers_invested,
                        cfg.nature_raises_unused,
                    );
                    let weight = candidate.weight;
                    pool.push((candidate, weight));
                }
            }
            if pool.is_empty() {
                continue;
            }
            // Weighted *within* one nature, never across them. The shuffle above
            // is the nature draw, and with no usage data to say otherwise
            // uniform is the right marginal — weighting the pool across natures
            // would quietly hand extra mass to whichever nature happened to
            // offer more coherent allocations. `enumerate_nature_spreads`
            // declines the same shortcut for the same reason.
            let total: f64 = pool.iter().map(|(_, w)| *w).sum();
            let chosen = if total > 0.0 {
                draw_weighted(pool, log)
            } else {
                // Every allocation here is incoherent and both controls are 0.0.
                // Staying agnostic beats deleting a nature the belief permits.
                draw_uniform(pool.into_iter().map(|(c, _)| c).collect(), log)
            };
            if let Some(candidate) = chosen {
                log.observe(1.0 / natures.len() as f64);
                return Ok(candidate);
            }
        }
    }

    Err(DeterminizeError::InfeasibleSpread {
        mon_idx,
        species: species.clone(),
    })
}

/// Budgets to try, in preference order: the full competitive allocation first,
/// then whatever the bounds actually permit.
fn candidate_budgets(lo: &[u32; 6], hi: &[u32; 6]) -> Vec<u32> {
    let min_total: u32 = lo.iter().sum();
    let max_total: u32 = hi.iter().sum();
    let mut budgets = Vec::new();
    if (min_total..=max_total).contains(&STAT_POINT_BUDGET) {
        budgets.push(STAT_POINT_BUDGET);
    }
    let relaxed = STAT_POINT_BUDGET.min(max_total).max(min_total);
    if !budgets.contains(&relaxed) {
        budgets.push(relaxed);
    }
    budgets
}

/// Convert the belief's scaled-EV bounds into authoring-unit point bounds.
///
/// `ev = max(0, 8p − 4)` is monotone, so the smallest legal `p` is the one whose
/// EV first reaches `min_ev`, and the largest is the one whose EV last stays
/// within `max_ev`.
fn point_bounds(unk: &UnknownPokemonState, cfg: &DeterminizeConfig) -> ([u32; 6], [u32; 6]) {
    let mut lo = [0u32; 6];
    let mut hi = [0u32; 6];
    for i in 0..6 {
        if !cfg.inference.use_stat_points {
            lo[i] = unk.min_evs[i] as u32;
            hi[i] = (unk.max_evs[i] as u32).min(252);
            continue;
        }
        let min_ev = unk.min_evs[i] as u32;
        let max_ev = unk.max_evs[i] as u32;
        // ceil((min_ev + 4) / 8), with 0 EV reachable only by 0 points.
        lo[i] = if min_ev == 0 { 0 } else { (min_ev + 4).div_ceil(8) };
        hi[i] = if max_ev < 4 {
            0
        } else {
            ((max_ev + 4) / 8).min(MAX_POINTS_PER_STAT as u32)
        };
        if lo[i] > hi[i] {
            // Infeasible on this stat; leave inverted so the composition sampler
            // reports failure rather than silently widening.
            hi[i] = lo[i];
        }
    }
    (lo, hi)
}

/// Randomly permute, using the simulator's seeded RNG seam so the result is
/// reproducible under `scoped_sample_rng`.
fn shuffle<T: Clone>(mut items: Vec<T>) -> Vec<T> {
    let mut out = Vec::with_capacity(items.len());
    while !items.is_empty() {
        let mut drawn = sample_one_weighted(
            items.iter().cloned().enumerate().collect::<Vec<_>>(),
            |_| 1.0,
        );
        let Some((idx, value)) = drawn.pop() else { break };
        out.push(value);
        items.remove(idx);
    }
    out
}

// ── Moves ────────────────────────────────────────────────────────────────────

/// Fill the four move slots.
///
/// Move percentages are *marginal inclusion rates*, not a distribution over
/// 4-move sets: they sum to about 350, not 400. That shortfall is real
/// probability mass sitting on moves outside the top-10 table, and it must be
/// modelled rather than normalized away. If the ten listed moves were simply
/// scaled up to fill four slots, every marginal would inflate by ~1.12x and a
/// 99%-usage move would be pushed past certainty and *forced* — an artifact
/// purely of where the source table was truncated.
///
/// So the residual gets explicit slots. Adding `m` exchangeable "off-meta"
/// pseudo-candidates carrying the missing mass makes the targets sum to exactly
/// the number of free slots, so nothing is spuriously forced and the expected
/// number of unlisted moves matches the data. Each one that gets drawn is then
/// filled from the species' learnset.
///
/// Revealed moves keep their original slot indices — `move_pp[i]`,
/// `used_moves_this_field[i]` and Last Resort are all slot-indexed.
pub(crate) fn sample_moves(
    mon_idx: usize,
    unk: &UnknownPokemonState,
    species: &Species,
    meta: Option<&SpeciesMeta>,
    cfg: &DeterminizeConfig,
    log: &mut DrawLog,
) -> Result<[Option<PokemonMove>; 4], DeterminizeError> {
    let mut slots = unk.known_moves.clone();
    let revealed: HashSet<PokemonMove> = slots.iter().flatten().cloned().collect();
    let free_slots = slots.iter().filter(|s| s.is_none()).count();
    if free_slots == 0 {
        return Ok(slots);
    }

    let learnset = cfg.inference.learnset_dex.get(species);

    // Candidates: listed moves not already revealed, and legal for the species.
    let listed: Vec<(PokemonMove, f64)> = meta
        .map(|m| {
            m.moves
                .iter()
                .filter(|w| !revealed.contains(&w.value))
                .filter(|w| learnset.is_none_or(|l| l.contains(&w.value)))
                .map(|w| (w.value.clone(), (w.pct / 100.0).clamp(0.0, 1.0)))
                .collect()
        })
        .unwrap_or_default();

    // Residual mass: the slots the listed moves collectively do not account for.
    // Without a learnset there is nothing to fill an off-meta slot with, so the
    // residual is dropped and the listed moves absorb it.
    let listed_mass: f64 = listed.iter().map(|(_, p)| *p).sum();
    let residual = if learnset.is_some() {
        (free_slots as f64 - listed_mass).max(0.0)
    } else {
        0.0
    };
    let residual_slots = if residual > 0.0 { free_slots.min(4) } else { 0 };

    let mut marginals: Vec<f64> = listed.iter().map(|(_, p)| *p).collect();
    for _ in 0..residual_slots {
        marginals.push(residual / residual_slots as f64);
    }

    let mut chosen: Vec<PokemonMove> = Vec::with_capacity(free_slots);
    let mut off_meta_wanted = 0usize;
    if !marginals.is_empty() {
        let (picked, probability) = sample_fixed_size_subset(&marginals, free_slots);
        log.observe(probability);
        for index in picked {
            match listed.get(index) {
                Some((m, _)) => chosen.push(m.clone()),
                None => off_meta_wanted += 1,
            }
        }
    } else {
        off_meta_wanted = free_slots;
    }

    // Fill off-meta slots (and any shortfall) from the learnset.
    let shortfall = free_slots.saturating_sub(chosen.len());
    let wanted = off_meta_wanted.max(shortfall);
    if wanted > 0 {
        let taken: HashSet<PokemonMove> = revealed
            .iter()
            .chain(chosen.iter())
            .chain(listed.iter().map(|(m, _)| m))
            .cloned()
            .collect();
        let mut pool: Vec<PokemonMove> = learnset
            .map(|l| l.iter().filter(|m| !taken.contains(m)).cloned().collect())
            .unwrap_or_default();
        // Deterministic order before the seeded shuffle, so a HashSet's
        // iteration order cannot make the draw irreproducible.
        pool.sort_by_key(|m| format!("{m:?}"));
        let pool = shuffle(pool);

        let mut added = Vec::new();
        for m in pool.into_iter().take(wanted) {
            log.observe(1.0);
            added.push(m);
        }
        if !added.is_empty() {
            log.warnings.push(DeterminizeWarning::LearnsetTopUp {
                mon_idx,
                moves: added.clone(),
            });
            chosen.extend(added);
        }
    }

    // Last resort: reuse listed moves the filter rejected rather than emitting a
    // moveless Pokemon.
    if chosen.is_empty() && revealed.is_empty()
        && let Some(meta) = meta {
            chosen.extend(meta.moves.iter().take(free_slots).map(|w| w.value.clone()));
        }
    if chosen.is_empty() && revealed.is_empty() {
        return Err(DeterminizeError::NoLegalMoves {
            mon_idx,
            species: species.clone(),
        });
    }

    let mut fill = chosen.into_iter();
    for slot in slots.iter_mut() {
        if slot.is_none() {
            *slot = fill.next();
        }
    }
    Ok(slots)
}

// ── Assembling one Pokemon ───────────────────────────────────────────────────

/// Build a complete `PokemonState` for one belief entry.
///
/// Runs `build_pokemon_state` and then overlays the dynamic battle state the
/// belief owns. Going through the real constructor rather than
/// `materialize::materialize_pokemon` is what makes the result *runnable*: it
/// derives types, weight, mega info from the held item, and — critically — PP
/// from the move dex (both PP helpers are private, so there is no other way to
/// get it right), and it leaves `evs`/`ivs` consistent with `stats` instead of
/// hardcoding them alongside an unrelated stat override.
#[allow(clippy::too_many_arguments)]
pub(crate) fn determinize_pokemon(
    mon_idx: usize,
    unk: &UnknownPokemonState,
    meta_dex: &MetaDex,
    pokemon_dex: &HashMap<Species, PokemonData>,
    move_dex: &HashMap<PokemonMove, MoveData>,
    used_items: &mut HashSet<Item>,
    cfg: &DeterminizeConfig,
    log: &mut DrawLog,
) -> Result<PokemonState, DeterminizeError> {
    let species = resolve_species(mon_idx, unk, log)?;
    let meta = meta_dex.get(&species);
    if meta.is_none() {
        log.warnings.push(DeterminizeWarning::NoMetaEntry {
            mon_idx,
            species: species.clone(),
        });
    }

    let base_stats = pokemon_dex
        .get(&species)
        .map(|d| d.base_stats)
        .unwrap_or([100u16; 6]);

    // Item first: a mega stone rewrites species and base stats inside
    // `build_pokemon_state`, so it has to be settled before the spread.
    let item = sample_item(mon_idx, unk, meta, used_items, cfg, log)?;
    if item != Item::None && !cfg.inference.allow_repeat_items {
        used_items.insert(item.clone());
    }

    let ability = sample_ability(
        mon_idx,
        &species,
        meta,
        pokemon_dex,
        cfg,
        log,
        &unk.possible_abilities,
    )?;
    // Only drawn when something actually changed the ability on field; otherwise
    // `original_ability` must stay `None` so a later Mega Evolution does not
    // overwrite a switch-out reset target that was never set.
    let original_ability = if unk.ability_changed_on_field {
        Some(sample_ability(
            mon_idx,
            &species,
            meta,
            pokemon_dex,
            cfg,
            log,
            &unk.possible_original_abilities,
        )?)
    } else {
        None
    };
    let spread =
        sample_nature_and_spread(mon_idx, unk, &species, base_stats, meta, cfg, log)?;
    let moves = sample_moves(mon_idx, unk, &species, meta, cfg, log)?;

    let gender = match &unk.possible_genders {
        crate::information::unknowns::Unknown::Known(g) => Some(*g),
        _ => None,
    };

    let mut mon = build_pokemon_state(
        species,
        pokemon_dex,
        move_dex,
        Some(unk.level.max(1)),
        Some(moves),
        gender,
        Some(ability.clone()),
        Some(spread.nature),
        Some(item),
        // TODO(tera): Tera is not implemented yet and the usage cache carries no
        // tera data, so this defaults to Normal. A *known* tera type is still
        // honoured — anything else would contradict the belief and trip the
        // subset oracle. When tera lands, the `_` arm should sample from
        // `possible_tera_type` instead; `is_tera` and the battle-level
        // `p*_has_tera` are already faithful.
        Some(match &unk.possible_tera_type {
            crate::information::unknowns::Unknown::Known(t) => t.clone(),
            _ => PokemonType::Normal,
        }),
        // Raw authoring points, NOT scaled EVs: `build_pokemon_state` applies
        // `scale_evs_for_stat_points` itself when `use_stat_points` is set, and
        // passing pre-scaled values would apply `8p − 4` twice.
        Some(spread.points),
        Some(spread.ivs),
        cfg.inference.use_stat_points,
    );

    apply_belief_overlay(&mut mon, unk, original_ability);
    Ok(mon)
}

/// Rebuild a Pokemon the belief already knows everything about.
///
/// The observer's own side goes through here rather than through the sampler,
/// and the distinction matters more than it looks. For an opponent, a `None`
/// move slot means "not yet revealed" and should be filled in; for the
/// observer's own Pokemon it means "this Pokemon genuinely has three moves", and
/// inventing a fourth would both fabricate a capability and, because the sampled
/// move would carry no PP from the belief, produce an unusable slot. The same
/// applies to the spread: a known Pokemon's `min_stats == max_stats`, which no
/// usage-data spread will satisfy, so sampling it would fall back to uniform and
/// silently rebuild the user's own team.
fn copy_known_pokemon(
    mon_idx: usize,
    unk: &UnknownPokemonState,
    pokemon_dex: &HashMap<Species, PokemonData>,
    move_dex: &HashMap<PokemonMove, MoveData>,
) -> Result<PokemonState, DeterminizeError> {
    use crate::information::unknowns::Unknown;

    let Unknown::Known(species) = &unk.possible_species else {
        return Err(DeterminizeError::SpeciesUndetermined { mon_idx });
    };

    let known_or = |lattice: &Unknown<Ability>, fallback: Ability| match lattice {
        Unknown::Known(a) => a.clone(),
        _ => fallback,
    };

    let mut mon = build_pokemon_state(
        species.clone(),
        pokemon_dex,
        move_dex,
        Some(unk.level.max(1)),
        Some(unk.known_moves.clone()),
        match &unk.possible_genders {
            Unknown::Known(g) => Some(*g),
            _ => None,
        },
        Some(known_or(&unk.possible_abilities, Ability::Illuminate)),
        Some(match &unk.possible_natures {
            Unknown::Known(n) => *n,
            _ => Nature::Hardy,
        }),
        Some(match &unk.item {
            Unknown::Known(i) => i.clone(),
            _ => Item::None,
        }),
        Some(match &unk.possible_tera_type {
            // Honours a known tera type where one exists — otherwise this would
            // contradict the belief and trip the subset oracle. Everything else
            // defaults to Normal, since tera is not implemented yet.
            Unknown::Known(t) => t.clone(),
            _ => PokemonType::Normal,
        }),
        // A known Pokemon's EV bounds are pinned, and already in the scaled
        // 0-252 units `PokemonState.evs` uses — so `use_stat_points` must be
        // false here or `8p - 4` would be applied to an already-scaled value.
        Some(unk.min_evs),
        Some(unk.min_ivs),
        false,
    );

    let original_ability = unk
        .ability_changed_on_field
        .then(|| known_or(&unk.possible_original_abilities, mon.ability.clone()));
    apply_belief_overlay(&mut mon, unk, original_ability);
    Ok(mon)
}

fn resolve_species(
    mon_idx: usize,
    unk: &UnknownPokemonState,
    log: &mut DrawLog,
) -> Result<Species, DeterminizeError> {
    match &unk.possible_species {
        crate::information::unknowns::Unknown::Known(s) => Ok(s.clone()),
        crate::information::unknowns::Unknown::Possibly(candidates)
            if !candidates.is_empty() =>
        {
            draw_uniform(candidates.clone(), log)
                .ok_or(DeterminizeError::SpeciesUndetermined { mon_idx })
        }
        // `Not(excluded)` is an open-world complement with no enumerable domain,
        // so there is genuinely nothing to pick from here.
        _ => Err(DeterminizeError::SpeciesUndetermined { mon_idx }),
    }
}

/// Copy the belief's dynamic battle state onto a freshly-built Pokemon.
///
/// `build_pokemon_state` produces a Pokemon as it would leave team preview; this
/// restores where the battle has actually got to.
fn apply_belief_overlay(
    mon: &mut PokemonState,
    unk: &UnknownPokemonState,
    original_ability: Option<Ability>,
) {
    mon.fainted = unk.fainted;
    mon.is_tera = unk.is_tera;
    mon.is_mega = unk.is_mega || mon.is_mega;

    // HP. `percent_bucket` inverts the display rounding exactly, giving the raw
    // range that shows as this percentage; anything else (a flat 50% sentinel, or
    // `pct * max / 100`) would put the Pokemon on the wrong side of a KO
    // threshold. Drawn uniformly within the bucket.
    mon.hp = match unk.hp {
        PokemonHP::Number(n) => n.min(mon.stats[0]),
        PokemonHP::Percent(100) => mon.stats[0],
        PokemonHP::Percent(pct) => match percent_bucket(pct, mon.stats[0]) {
            Some((lo, hi)) => sample_one_weighted((lo..=hi).collect::<Vec<u16>>(), |_| 1.0)
                .pop()
                .unwrap_or(lo),
            // Unreachable for a spread that passed `build_spread_candidate`,
            // which rejects max-HP hypotheses incompatible with the observation.
            None => ((mon.stats[0] as u32 * pct as u32) / 100).max(1) as u16,
        },
    };
    if mon.fainted {
        mon.hp = 0;
    } else if mon.hp == 0 {
        mon.hp = 1;
    }

    // PP. The belief stores -1 for "not observed"; a revealed move whose PP was
    // never seen starts full rather than at zero, which is what
    // `materialize_pokemon` produces and why its output cannot be played.
    for i in 0..4 {
        if mon.moves[i].is_none() {
            mon.move_pp[i] = 0;
            mon.max_pp[i] = 0;
            continue;
        }
        if unk.max_pp[i] >= 0 {
            mon.max_pp[i] = unk.max_pp[i] as u8;
        }
        mon.move_pp[i] = if unk.move_pp[i] >= 0 {
            (unk.move_pp[i] as u8).min(mon.max_pp[i])
        } else {
            mon.max_pp[i]
        };
    }

    mon.boosts = unk.boosts;
    mon.status = unk.status.clone();
    // Without this every determinized Rest-sleep would be played back as a
    // natural one, which is a live divergence: the sim's wake check is
    // deterministic for Rest and 1/3-vs-2/3 weighted otherwise.
    mon.rest_sleep = unk.rest_sleep;
    mon.volatiles = unk.volatiles.clone();

    mon.consumed_item = unk.consumed_item.clone();
    mon.cud_chew_pending = unk.cud_chew_pending.clone();
    mon.item_lost = unk.item_lost;

    mon.damaged_this_turn = unk.damaged_this_turn;
    mon.damaged_by_this_turn = unk.damaged_by_this_turn.clone();
    mon.last_physical_damage_taken = hp_to_raw(&unk.last_physical_damage_taken, mon.stats[0]);
    mon.last_physical_attacker = unk.last_physical_attacker;
    mon.last_special_damage_taken = hp_to_raw(&unk.last_special_damage_taken, mon.stats[0]);
    mon.last_special_attacker = unk.last_special_attacker;
    mon.last_damage_taken = hp_to_raw(&unk.last_damage_taken, mon.stats[0]);
    mon.last_damage_attacker = unk.last_damage_attacker;
    mon.stats_raised_this_turn = unk.stats_raised_this_turn;
    mon.stats_lowered_this_turn = unk.stats_lowered_this_turn;
    mon.switched_in_this_turn = unk.switched_in_this_turn;
    mon.stall_counter = unk.stall_counter;
    mon.ally_switch_counter = unk.ally_switch_counter;

    mon.last_move_failed = unk.last_move_failed;
    mon.last_used_move = unk.last_used_move.clone();
    mon.consecutive_move_count = unk.consecutive_move_count;
    mon.used_moves_this_field = unk.used_moves_this_field;
    mon.one_time_ability_used = unk.one_time_ability_used;
    mon.ate_berry_this_battle = unk.ate_berry_this_battle;
    mon.first_move_on_field = unk.first_move_on_field;
    mon.first_turn_on_field_pending = unk.first_turn_on_field_pending;
    mon.entered_this_turn = unk.entered_this_turn;
    mon.pre_mimicry_types = unk.pre_mimicry_types.clone();
    mon.times_hit = unk.times_hit;

    mon.original_ability = original_ability;
}

/// The belief tracks opponent damage as a percentage; a concrete state needs raw
/// HP. Only used for the Counter/Mirror Coat bookkeeping fields.
fn hp_to_raw(hp: &PokemonHP, max_hp: u16) -> u16 {
    match hp {
        PokemonHP::Number(n) => *n,
        PokemonHP::Percent(pct) => ((max_hp as u32 * *pct as u32) / 100) as u16,
    }
}

// ── Bench selection ──────────────────────────────────────────────────────────

/// Choose which candidate bench Pokemon are actually on the team.
///
/// The belief keeps three buckets and none is the answer on its own:
/// `known_back` is certain, `possible_back` is a candidate set that may well
/// outnumber the real bench, and `fainted` sits outside the `mon_idx` space
/// entirely but still occupies roster slots in a concrete state.
///
/// Committing to a subset of `possible_back` is technically an over-commitment —
/// the belief only says those Pokemon *might* be there — but a determinized
/// world has to commit to something. What keeps it honest is that the choice is
/// drawn from the belief's own candidates, weighted by the only co-occurrence
/// signal the dataset has (teammate rows), and that `check_determinization`
/// verifies every committed bench member really was a candidate.
/// How much one teammate-list appearance multiplies a candidate's weight. A
/// rank-1 edge (score 1.0) makes a species 2.5x likelier per known teammate.
const AFFINITY_GAIN: f64 = 1.5;

/// Softens the popularity base rate. Without it the raw indegree spread is wide
/// enough that co-occurrence cannot move the ranking, and the prior collapses
/// back into "the format's most common Pokemon".
const POPULARITY_EXPONENT: f64 = 0.5;

/// Floor for species the summary has no teammate count for. Nearly half the
/// format has indegree 0; a hard zero would make them unreachable rather than
/// merely unlikely, and the belief has said nothing to rule them out.
const POPULARITY_FLOOR: f64 = 0.02;

/// Prior weight for "this species is on the opponent's team, given the ones we
/// have already seen".
///
/// Multiplicative rather than additive. The additive form's balance drifts with
/// how much of the team is known — at zero known species affinity is 0 and
/// popularity is the whole signal, at four known it is swamped — so the prior
/// silently changed character as a battle progressed. A product is the
/// naive-Bayes shape, so evidence accumulates correctly and needs no `1/|K|`
/// normalizer.
///
/// **Both directions of each edge count.** The cache lists only ten teammates
/// per species and the graph is hub-dominated (Sinistcha appears in 182 of 235
/// lists), so the forward direction alone is largely a popularity proxy. Only
/// ~13% of edges are mutual, meaning forward and reverse are near-complementary
/// rather than redundant; including both roughly doubles the number of
/// candidates carrying real evidence.
///
/// **Do not divide by the base rate.** `teammate_score(candidate, known)` is
/// already a lift estimate — `P(k|c) = P(c|k)·P(k)/P(c)` and `P(k)` is constant
/// across candidates — so a PMI-style correction double-counts and floats
/// zero-indegree species to the top. This is worth re-deriving before
/// "improving" it.
fn roster_affinity(candidate: &Species, known: &[Species], meta_dex: &MetaDex) -> f64 {
    let mut weight = meta_dex
        .popularity(candidate)
        .max(POPULARITY_FLOOR)
        .powf(POPULARITY_EXPONENT);
    for k in known {
        let edge = meta_dex.teammate_score(k, candidate) + meta_dex.teammate_score(candidate, k);
        weight *= 1.0 + AFFINITY_GAIN * edge;
    }
    weight
}

fn select_bench_indices(
    possible: &[UnknownPokemonState],
    slots: usize,
    known_species: &[Species],
    meta_dex: &MetaDex,
    log: &mut DrawLog,
) -> Vec<usize> {
    if slots == 0 {
        return Vec::new();
    }
    if possible.len() <= slots {
        return (0..possible.len()).collect();
    }

    let scores: Vec<f64> = possible
        .iter()
        .map(|unk| {
            let Some(species) = known_species_of(unk) else {
                // Species still ambiguous, so there is nothing to score it on.
                // Sit it at the floor rather than at an arbitrary literal, so
                // it stays in the same units as every other candidate.
                return POPULARITY_FLOOR.powf(POPULARITY_EXPONENT);
            };
            roster_affinity(&species, known_species, meta_dex)
        })
        .collect();

    let (picked, probability) = sample_fixed_size_subset(&scores, slots);
    log.observe(probability);
    picked
}

fn known_species_of(unk: &UnknownPokemonState) -> Option<Species> {
    match &unk.possible_species {
        crate::information::unknowns::Unknown::Known(s) => Some(s.clone()),
        _ => None,
    }
}

// ── mon_id assignment ────────────────────────────────────────────────────────

/// Give every Pokemon on a side a distinct `mon_id`.
///
/// Not cosmetic: `subset_check::build_mon_idx_map` pairs concrete bench Pokemon
/// with belief entries by `mon_id` before falling back to species. Two Pokemon
/// sharing an id would silently check one mon's constraints against another's.
/// Ids the belief already knows are preserved verbatim; the rest take the lowest
/// value unused on that side.
fn assign_mon_ids(mons: &mut [&mut PokemonState], known: &[Option<u8>]) {
    let mut used: HashSet<u8> = HashSet::new();
    // Known ids first, so an inferred one never steals a value the belief has
    // already committed to. Duplicates among the known ids themselves are
    // possible when several entries were built from templates that all default
    // to id 0; the first claimant keeps it and the rest are reassigned, since a
    // wrong-but-distinct id is recoverable and a collision is not.
    for (mon, known_id) in mons.iter_mut().zip(known) {
        if let Some(id) = known_id
            && used.insert(*id) {
                mon.mon_id = *id;
                continue;
            }
        mon.mon_id = u8::MAX; // marker: needs assignment below
    }
    for mon in mons.iter_mut() {
        if mon.mon_id != u8::MAX {
            continue;
        }
        let mut candidate = 0u8;
        while used.contains(&candidate) {
            candidate = candidate.saturating_add(1);
        }
        used.insert(candidate);
        mon.mon_id = candidate;
    }
}

fn known_mon_id(unk: &UnknownPokemonState) -> Option<u8> {
    match &unk.possible_mon_id {
        crate::information::unknowns::Unknown::Known(id) => Some(*id),
        _ => None,
    }
}

// ── Field timers ─────────────────────────────────────────────────────────────

/// Resolve one field-effect timer.
///
/// `Known(0)` is the permanent-effect sentinel (primordial weather) and gets its
/// own arm ahead of everything else — folding it into the fallback would put a
/// 5-turn clock on weather that never ends.
fn resolve_timer(
    timer: &crate::information::unknowns::Unknown<u8>,
    cfg: &DeterminizeConfig,
    log: &mut DrawLog,
) -> u8 {
    use crate::information::unknowns::Unknown;
    match timer {
        Unknown::Known(t) => *t,
        Unknown::Possibly(candidates) if !candidates.is_empty() => match cfg.timer_policy {
            TimerPolicy::MaxPlausible => *candidates.iter().max().unwrap_or(&5),
            TimerPolicy::Fixed(n) => n,
            TimerPolicy::SampleUniform => {
                draw_uniform(candidates.clone(), log).unwrap_or(5)
            }
        },
        _ => match cfg.timer_policy {
            // Screens run 5 turns, or 8 with Light Clay; assuming the longer of
            // the ordinary durations is the pessimistic read for the observer,
            // which is the safer default for a bot deciding whether to wait.
            TimerPolicy::MaxPlausible => 5,
            TimerPolicy::Fixed(n) => n,
            TimerPolicy::SampleUniform => draw_uniform((1..=5).collect::<Vec<u8>>(), log).unwrap_or(5),
        },
    }
}

fn resolve_timer_opt(
    timer: &Option<crate::information::unknowns::Unknown<u8>>,
    cfg: &DeterminizeConfig,
    log: &mut DrawLog,
) -> Option<u8> {
    timer.as_ref().map(|t| resolve_timer(t, cfg, log))
}

// ── Whole-battle assembly ────────────────────────────────────────────────────

/// One determinized world.
#[derive(Debug, Clone)]
pub struct Determinized {
    pub state: BattleState,
    /// Joint probability of the sampled draw sequence.
    ///
    /// A lower bound on the probability of the resulting `BattleState` — several
    /// draw sequences can produce identical states and those are not summed —
    /// and comparable only between draws from the same belief, since the
    /// discarded residual mass differs. The same contract `sample_turn` carries.
    pub probability: f64,
    pub warnings: Vec<DeterminizeWarning>,
}

/// Sample one complete, simulator-runnable world consistent with `belief`.
///
/// Deterministic in `seed`: the same inputs always yield the same `BattleState`.
pub fn determinize_seeded(
    seed: u64,
    belief: &UnknownBattleState,
    meta_dex: &MetaDex,
    pokemon_dex: &HashMap<Species, PokemonData>,
    move_dex: &HashMap<PokemonMove, MoveData>,
    cfg: &DeterminizeConfig,
) -> Result<Determinized, DeterminizeError> {
    let _guard = scoped_sample_rng(seed);
    determinize(belief, meta_dex, pokemon_dex, move_dex, cfg)
}

/// As `determinize_seeded`, but using whatever RNG is ambient — `thread_rng`, or
/// an outer `scoped_sample_rng` if the caller installed one.
///
/// Worlds are drawn until one satisfies every cross-Pokemon constraint the
/// belief carries, or the budget runs out. Rejection sampling rather than
/// targeted repair, because `subset_check` already evaluates the entire CNF
/// predicate store against a concrete state: reusing it covers every clause kind
/// at once, and keeps this code's notion of a valid world identical to the
/// oracle's instead of a second implementation that could drift from it.
pub fn determinize(
    belief: &UnknownBattleState,
    meta_dex: &MetaDex,
    pokemon_dex: &HashMap<Species, PokemonData>,
    move_dex: &HashMap<PokemonMove, MoveData>,
    cfg: &DeterminizeConfig,
) -> Result<Determinized, DeterminizeError> {
    let oracle_belief = UnknownMatchState::Battle(belief.clone());
    let mut last: Option<(Determinized, Vec<String>)> = None;

    for _ in 0..=cfg.max_repair_passes.max(1) {
        let world = determinize_once(belief, meta_dex, pokemon_dex, move_dex, cfg)?;
        let violations = collect_true_state_subset_violations(
            &world.state,
            &oracle_belief,
            cfg.observer,
            pokemon_dex,
            move_dex,
        );
        if violations.is_empty() {
            return Ok(world);
        }
        last = Some((world, violations.iter().map(|v| v.to_string()).collect()));
    }

    // Budget exhausted. Return the last attempt rather than failing: it is still
    // a legal, playable state, and refusing to produce one would make the
    // determinizer useless exactly when the belief is richest.
    let (mut world, violations) = last.expect("the loop always runs at least once");
    for detail in violations {
        world
            .warnings
            .push(DeterminizeWarning::UnsatisfiedConstraint { detail });
    }
    Ok(world)
}

/// One draw, with no constraint re-checking.
fn determinize_once(
    belief: &UnknownBattleState,
    meta_dex: &MetaDex,
    pokemon_dex: &HashMap<Species, PokemonData>,
    move_dex: &HashMap<PokemonMove, MoveData>,
    cfg: &DeterminizeConfig,
) -> Result<Determinized, DeterminizeError> {
    let mut log = DrawLog::new();

    // The flat mon_idx space, in the order `get_mon_by_idx` walks its segments.
    let p2_start = belief.p1_active_mons.len();
    let p1k_start = p2_start + belief.p2_active_mons.len();
    let p1p_start = p1k_start + belief.p1_known_back_mons.len();
    let p2k_start = p1p_start + belief.p1_possible_back_mons.len();
    let p2p_start = p2k_start + belief.p2_known_back_mons.len();

    let mut p1 = build_side(
        belief,
        Player::P1,
        (0, p1k_start, p1p_start),
        meta_dex,
        pokemon_dex,
        move_dex,
        cfg,
        &mut log,
    )?;
    let mut p2 = build_side(
        belief,
        Player::P2,
        (p2_start, p2k_start, p2p_start),
        meta_dex,
        pokemon_dex,
        move_dex,
        cfg,
        &mut log,
    )?;

    assign_side_mon_ids(&mut p1);
    assign_side_mon_ids(&mut p2);

    let state = assemble_battle(belief, p1, p2, cfg, &mut log);
    Ok(Determinized {
        state,
        probability: log.probability,
        warnings: log.warnings,
    })
}

/// Sample a hidden Pokemon, or rebuild a known one.
#[allow(clippy::too_many_arguments)]
fn build_one(
    hidden: bool,
    mon_idx: usize,
    unk: &UnknownPokemonState,
    meta_dex: &MetaDex,
    pokemon_dex: &HashMap<Species, PokemonData>,
    move_dex: &HashMap<PokemonMove, MoveData>,
    used_items: &mut HashSet<Item>,
    cfg: &DeterminizeConfig,
    log: &mut DrawLog,
) -> Result<PokemonState, DeterminizeError> {
    if let Some(pre) = &unk.pre_transform {
        return build_transformed(
            hidden,
            mon_idx,
            unk,
            pre,
            meta_dex,
            pokemon_dex,
            move_dex,
            used_items,
            cfg,
            log,
        );
    }
    if hidden {
        determinize_pokemon(
            mon_idx,
            unk,
            meta_dex,
            pokemon_dex,
            move_dex,
            used_items,
            cfg,
            log,
        )
    } else {
        copy_known_pokemon(mon_idx, unk, pokemon_dex, move_dex)
    }
}

/// Build a Pokemon that is currently Transformed.
///
/// After a Transform the belief's *primary* fields describe the copy target —
/// that is what the observer sees — while `pre_transform` holds the mon's real
/// identity. Building straight from the primary view is therefore not merely
/// imprecise, it produces the wrong Pokemon: it takes the **target's** base HP,
/// which Transform explicitly does not copy, and leaves `pre_transform` empty so
/// the mon can never revert on switch-out. Both were silent — a Ditto
/// transformed into Garchomp came out as a 185 HP Garchomp with no warning.
///
/// The splice is delegated to `simulator::helpers::transform_into` rather than
/// reimplemented, so the copied set (species, types, gender, weight,
/// `stats[1..=5]`, boosts, moves, PP capped at 5, ability) and the preserved set
/// (hp, `stats[0]`, level, item, status, nature, EVs/IVs, tera) cannot drift
/// from the engine's.
#[allow(clippy::too_many_arguments)]
fn build_transformed(
    hidden: bool,
    mon_idx: usize,
    unk: &UnknownPokemonState,
    pre: &UnknownPokemonState,
    meta_dex: &MetaDex,
    pokemon_dex: &HashMap<Species, PokemonData>,
    move_dex: &HashMap<PokemonMove, MoveData>,
    used_items: &mut HashSet<Item>,
    cfg: &DeterminizeConfig,
    log: &mut DrawLog,
) -> Result<PokemonState, DeterminizeError> {
    // The real Pokemon, from its own belief entry. The belief only ever
    // snapshots when `pre_transform` is empty, so this cannot nest — cleared
    // defensively anyway, since a recursive build has no other stopping rule.
    let mut real = pre.clone();
    real.pre_transform = None;
    let mut original = build_one(
        hidden,
        mon_idx,
        &real,
        meta_dex,
        pokemon_dex,
        move_dex,
        used_items,
        cfg,
        log,
    )?;

    // What it currently looks like. Transform copies no item, so this draw must
    // not spend one from the side's item-clause budget — hence the scratch set,
    // which is thrown away.
    let mut shown = unk.clone();
    shown.pre_transform = None;

    // This draw exists only to supply the fields Transform *copies*. The bounds
    // that describe the transformer rather than the target have to come off
    // first, or the build is infeasible by construction: `unk`'s HP window is
    // the Ditto's, and no Garchomp spread lands inside it. Widening is safe
    // because `transform_into` discards every one of these — HP, EVs and IVs
    // are all on its preserved-from-the-original list, so nothing drawn here
    // reaches the result. The `stats[1..6]` bounds, which *are* the target's,
    // are deliberately left in place: they are the real constraint on this draw.
    shown.min_stats[0] = 0;
    shown.max_stats[0] = u16::MAX;
    shown.min_pre_nature_stat[0] = 0;
    shown.max_pre_nature_stat[0] = u16::MAX;
    shown.hp = PokemonHP::Percent(100);
    shown.min_evs = [0; 6];
    shown.max_evs = [252; 6];
    shown.min_ivs = [0; 6];
    shown.max_ivs = [31; 6];
    let mut scratch = used_items.clone();
    let target = build_one(
        hidden,
        mon_idx,
        &shown,
        meta_dex,
        pokemon_dex,
        move_dex,
        &mut scratch,
        cfg,
        log,
    )?;

    if transform_into(&mut original, &target) {
        // Re-overlay from the *transformed* belief: HP, status and boosts are
        // the live values for the mon as it stands now, and `stats[0]` is the
        // original's, which is what `percent_bucket` must invert against.
        apply_belief_overlay(&mut original, unk, None);
    } else {
        // `transform_into` refuses an Illusion/Imposter target. Leaving the mon
        // untransformed is legal and honest; silently pretending otherwise is
        // what got us here.
        log.warnings.push(DeterminizeWarning::TransformNotApplied {
            mon_idx,
            species: target.species.clone(),
        });
    }
    Ok(original)
}

/// Decide which active slots are really this side's disguised Zoroark.
///
/// **A side-level draw, not a per-slot coin flip.** `p*_unresolved_zoroark_count`
/// means "this many real Illusion formes are not pinned to a field location."
/// They may be active under one of the live hypotheses, on the bench, or not
/// among the brought Pokemon at all. Flipping independently per active slot
/// would produce worlds with two copies of the same Zoroark; forcing exactly one
/// active commitment would wrongly erase the bench/unbrought possibilities.
///
/// Every candidate is weighted equally: the hypotheses are mirrored through all
/// six inference passes, so any that survived is exactly as consistent with the
/// observations as any other, and the usage cache has nothing to say about
/// which slot a Zoroark chose to hide in. One synthetic off-field candidate per
/// unresolved forme preserves the possibility that it is not currently active.
fn choose_illusion_slots(
    actives: &[UnknownPokemonState],
    unresolved: u8,
    log: &mut DrawLog,
) -> Vec<usize> {
    if unresolved == 0 {
        return Vec::new();
    }
    let candidates: Vec<usize> = actives
        .iter()
        .enumerate()
        .filter(|(_, unk)| unk.possible_illusion_state.is_some())
        .map(|(i, _)| i)
        .collect();
    let off_field = unresolved as usize;
    let take = off_field.min(candidates.len() + off_field);
    let weights = vec![1.0f64; candidates.len() + off_field];
    let (picked, probability) = sample_fixed_size_subset(&weights, take);
    log.observe(probability);
    picked
        .into_iter()
        .filter_map(|i| candidates.get(i).copied())
        .collect()
}

/// Finish an Illusion commitment once the whole roster exists.
///
/// Two things have to line up, and neither can be settled before the bench is
/// final (invention appends to it):
///
/// 1. `illusion_disguise` must be the species the observer has been *seeing*,
///    which is the belief's primary hypothesis for that slot.
/// 2. `compute_illusion_disguise` derives the disguise from the concrete roster
///    — "the last non-fainted party member" — so the decoy has to actually sit
///    at the end of the bench, or the world disagrees with itself the moment
///    the Zoroark re-enters and the engine recomputes.
///
/// If the decoy is not on the bench at all the belief is inconsistent with what
/// Illusion can do; that is warned about rather than papered over by inventing
/// a decoy, which would fabricate a roster member to justify a guess.
fn apply_illusion_disguises(
    active: &mut [PokemonState],
    back: &mut Vec<PokemonState>,
    back_ids: &mut Vec<Option<u8>>,
    actives: &[UnknownPokemonState],
    illusion_slots: &[usize],
    log: &mut DrawLog,
) {
    for &i in illusion_slots {
        let Some(Unknown::Known(decoy)) = actives.get(i).map(|u| &u.possible_species) else {
            continue;
        };
        let Some(pos) = back
            .iter()
            .position(|m| m.species == *decoy && !m.fainted)
        else {
            log.warnings.push(DeterminizeWarning::IllusionUncommitted {
                decoy: decoy.clone(),
            });
            continue;
        };
        // Move the decoy to the very end. Fainted entries after it do not
        // matter — the engine's scan skips them.
        let mon = back.remove(pos);
        let id = back_ids.remove(pos);
        back.push(mon);
        back_ids.push(id);

        if let Some(zoroark) = active.get_mut(i) {
            // Transform and Illusion are mutually exclusive (Bulbapedia: a mon
            // cannot transform while disguised, and one that gains Illusion via
            // Transform does not disguise). The engine enforces both directions,
            // so never hand it a state carrying both.
            if zoroark.pre_transform.is_some() {
                log.warnings.push(DeterminizeWarning::IllusionUncommitted {
                    decoy: decoy.clone(),
                });
                continue;
            }
            zoroark.ability = Ability::Illusion;
            zoroark.illusion_disguise = Some(decoy.clone());
        }
    }
}

/// One side's determinized Pokemon, plus the ids the belief already knew.
struct SideRoster {
    active: Vec<PokemonState>,
    back: Vec<PokemonState>,
    active_ids: Vec<Option<u8>>,
    back_ids: Vec<Option<u8>>,
}

fn assign_side_mon_ids(side: &mut SideRoster) {
    let mut known = side.active_ids.clone();
    known.extend(side.back_ids.iter().copied());
    let mut mons: Vec<&mut PokemonState> =
        side.active.iter_mut().chain(side.back.iter_mut()).collect();
    assign_mon_ids(&mut mons, &known);
}

#[allow(clippy::too_many_arguments)]
fn build_side(
    belief: &UnknownBattleState,
    player: Player,
    starts: (usize, usize, usize),
    meta_dex: &MetaDex,
    pokemon_dex: &HashMap<Species, PokemonData>,
    move_dex: &HashMap<PokemonMove, MoveData>,
    cfg: &DeterminizeConfig,
    log: &mut DrawLog,
) -> Result<SideRoster, DeterminizeError> {
    let (active_start, known_start, possible_start) = starts;
    // The observer's own Pokemon are already fully specified; only the opponent
    // is actually sampled.
    let hidden = player != cfg.observer;
    let (actives, known_back, possible_back, fainted) = match player {
        Player::P1 => (
            &belief.p1_active_mons,
            &belief.p1_known_back_mons,
            &belief.p1_possible_back_mons,
            &belief.p1_fainted_mons,
        ),
        Player::P2 => (
            &belief.p2_active_mons,
            &belief.p2_known_back_mons,
            &belief.p2_possible_back_mons,
            &belief.p2_fainted_mons,
        ),
    };

    // The item clause is enforced exactly rather than by rejection: each side
    // keeps the set of items already handed out and removes them from later
    // pools before the weights are renormalized.
    let mut used_items: HashSet<Item> = HashSet::new();

    // Which active slots are really the side's disguised Zoroark. Decided up
    // front because a committed slot is built from a *different* belief entry,
    // and rebuilding it afterwards would double-spend the item budget.
    let unresolved_zoroark = match player {
        Player::P1 => belief.p1_unresolved_zoroark_count,
        Player::P2 => belief.p2_unresolved_zoroark_count,
    };
    let roster_templates = match player {
        Player::P1 => &belief.p1_roster_templates,
        Player::P2 => &belief.p2_roster_templates,
    };
    let find_decoy_template = |shown: &UnknownPokemonState| {
        let shown_species = known_species_of(shown);
        let shown_id = known_mon_id(shown);
        roster_templates.iter().find(|candidate| {
            shown_id
                .zip(known_mon_id(candidate))
                .is_some_and(|(left, right)| left == right)
                || shown_species
                    .as_ref()
                    .is_some_and(|species| known_species_of(candidate).as_ref() == Some(species))
        })
    };
    let mut illusion_slots = choose_illusion_slots(actives, unresolved_zoroark, log);
    illusion_slots.retain(|slot| {
        let shown = &actives[*slot];
        if find_decoy_template(shown).is_some() {
            true
        } else {
            if let Some(decoy) = known_species_of(shown) {
                log.warnings
                    .push(DeterminizeWarning::IllusionUncommitted { decoy });
            }
            false
        }
    });

    let mut active = Vec::with_capacity(actives.len());
    let mut active_ids = Vec::with_capacity(actives.len());
    for (i, unk) in actives.iter().enumerate() {
        // A committed slot builds from the promoted hypothesis — the Zoroark's
        // own species, ability, moves and stat bounds — not from the shown one.
        let mut promoted;
        let entry = match (illusion_slots.contains(&i), &unk.possible_illusion_state) {
            (true, Some(sub)) => {
                promoted = unk.clone();
                promote_illusion_to_primary(&mut promoted, (**sub).clone());
                &promoted
            }
            _ => unk,
        };
        active.push(build_one(
            hidden,
            active_start + i,
            entry,
            meta_dex,
            pokemon_dex,
            move_dex,
            &mut used_items,
            cfg,
            log,
        )?);
        active_ids.push(known_mon_id(entry));
    }

    let mut back = Vec::new();
    let mut back_ids = Vec::new();

    // Committing an active hypothesis swaps two physical identities in the
    // belief's unresolved representation: the active shown-species record is
    // really Zoroark, while the roster's explicit Zoroark record is really the
    // decoy that Illusion selected. Materialize that decoy from its pristine
    // team-preview template and later omit the stale Zoroark bench record.
    for &slot in &illusion_slots {
        let shown = &actives[slot];
        let template =
            find_decoy_template(shown).expect("illusion slots were filtered to backed templates");
        let mut decoy = template.clone();
        decoy.possible_illusion_state = None;
        back.push(build_one(
            hidden,
            usize::MAX,
            &decoy,
            meta_dex,
            pokemon_dex,
            move_dex,
            &mut used_items,
            cfg,
            log,
        )?);
        back_ids.push(known_mon_id(&decoy));
    }

    let active_illusion_committed = !illusion_slots.is_empty();
    let stale_illusion_baseline = |unk: &UnknownPokemonState| {
        active_illusion_committed
            && known_species_of(unk).is_some_and(|species| {
                is_illusion_capable_species(&species)
            })
    };
    for (i, unk) in known_back.iter().enumerate() {
        if stale_illusion_baseline(unk) {
            continue;
        }
        back.push(build_one(
            hidden,
            known_start + i,
            unk,
            meta_dex,
            pokemon_dex,
            move_dex,
            &mut used_items,
            cfg,
            log,
        )?);
        back_ids.push(known_mon_id(unk));
    }

    // `back_mons_per_side` is the original bench size, fixed at battle start, so
    // a fainted-and-replaced Pokemon still occupies one of its slots.
    let live_slots = (belief.back_mons_per_side as usize)
        .saturating_sub(fainted.len())
        .saturating_sub(back.len());

    let known_species: Vec<Species> = active
        .iter()
        .chain(back.iter())
        .map(|mon| mon.species.clone())
        .collect();
    let eligible_possible: Vec<(usize, UnknownPokemonState)> = possible_back
        .iter()
        .enumerate()
        .filter(|(_, unk)| !stale_illusion_baseline(unk))
        .map(|(idx, unk)| (idx, unk.clone()))
        .collect();
    let eligible_entries: Vec<UnknownPokemonState> = eligible_possible
        .iter()
        .map(|(_, unk)| unk.clone())
        .collect();
    for eligible_idx in
        select_bench_indices(&eligible_entries, live_slots, &known_species, meta_dex, log)
    {
        let (idx, unk) = &eligible_possible[eligible_idx];
        back.push(build_one(
            hidden,
            possible_start + *idx,
            unk,
            meta_dex,
            pokemon_dex,
            move_dex,
            &mut used_items,
            cfg,
            log,
        )?);
        back_ids.push(known_mon_id(unk));
    }

    // Fainted Pokemon belong on the concrete bench. `build_mon_idx_map` skips
    // them, which is how the oracle tolerates their absence from the belief's
    // mon_idx space; `usize::MAX` marks them as having no such index.
    for unk in fainted {
        let mut mon = build_one(
            hidden,
            usize::MAX,
            unk,
            meta_dex,
            pokemon_dex,
            move_dex,
            &mut used_items,
            cfg,
            log,
        )?;
        mon.fainted = true;
        mon.hp = 0;
        back.push(mon);
        back_ids.push(known_mon_id(unk));
    }

    // Only ever invent for the hidden side — fabricating members of the
    // observer's own team would be inventing data the caller already has.
    if hidden && back.len() < belief.back_mons_per_side as usize && cfg.invent_missing_bench {
        invent_bench(
            &mut back,
            &mut back_ids,
            belief.back_mons_per_side as usize,
            &known_species,
            meta_dex,
            pokemon_dex,
            move_dex,
            &mut used_items,
            cfg,
            log,
        );
    }

    // Last, because the disguise is a function of the finished bench and
    // invention appends to it.
    apply_illusion_disguises(
        &mut active,
        &mut back,
        &mut back_ids,
        actives,
        &illusion_slots,
        log,
    );

    Ok(SideRoster {
        active,
        back,
        active_ids,
        back_ids,
    })
}

/// Fabricate plausible bench Pokemon when the belief cannot fill the roster.
///
/// This is the largest source of "sound but implausible" worlds, and it is
/// invisible to the subset oracle — an invented Pokemon contradicts nothing —
/// hence the warning on every one. Candidates are drawn rather than taken
/// greedily: an argmax would give every seed the same bench, defeating the point
/// of seeding.
///
/// `used_items` is the caller's live set covering the *whole* side. It must be
/// threaded in rather than rebuilt from `back`: an invented Pokemon is still a
/// team member, so it competes for the item clause with the actives too, and
/// nothing downstream would catch a duplicate (the subset oracle sees no
/// contradiction because the invented mon has no belief entry to contradict).
#[allow(clippy::too_many_arguments)]
fn invent_bench(
    back: &mut Vec<PokemonState>,
    back_ids: &mut Vec<Option<u8>>,
    target: usize,
    known_species: &[Species],
    meta_dex: &MetaDex,
    pokemon_dex: &HashMap<Species, PokemonData>,
    move_dex: &HashMap<PokemonMove, MoveData>,
    used_items: &mut HashSet<Item>,
    cfg: &DeterminizeConfig,
    log: &mut DrawLog,
) {
    let needed = target.saturating_sub(back.len());
    if needed == 0 {
        return;
    }

    let mut taken: HashSet<Species> = known_species.iter().cloned().collect();
    taken.extend(back.iter().map(|m| m.species.clone()));

    // Rank the format's roster by affinity with what is already known, then
    // truncate — `sample_fixed_size_subset` is only exact for small pools, and
    // 235 candidates would fall back to its approximate path.
    //
    // The cap depends on how many we are drawing, which the old fixed 24 did
    // not: C(24,4) fits under the enumeration threshold but C(24,5) is double
    // it, so a five-slot invention was silently taking the approximate path.
    let mut pool: Vec<(Species, f64)> = meta_dex
        .species()
        .filter(|s| !taken.contains(s))
        .map(|s| (s.clone(), roster_affinity(s, known_species, meta_dex)))
        .collect();
    pool.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            // Species order breaks ties so a HashMap's iteration order cannot
            // make the result irreproducible under a fixed seed.
            .then_with(|| format!("{:?}", a.0).cmp(&format!("{:?}", b.0)))
    });
    pool.truncate(max_exact_pool(needed));

    if pool.is_empty() {
        return;
    }
    let scores: Vec<f64> = pool.iter().map(|(_, w)| *w).collect();
    let (picked, probability) = sample_fixed_size_subset(&scores, needed.min(pool.len()));
    log.observe(probability);

    for idx in picked {
        let species = pool[idx].0.clone();
        let unk = UnknownPokemonState::from_opponent_species(
            species.clone(),
            pokemon_dex,
            cfg.inference.level,
        );
        match determinize_pokemon(
            usize::MAX,
            &unk,
            meta_dex,
            pokemon_dex,
            move_dex,
            used_items,
            cfg,
            log,
        ) {
            Ok(mon) => {
                log.warnings
                    .push(DeterminizeWarning::InventedBenchMon { species });
                back.push(mon);
                back_ids.push(None);
            }
            // An invented Pokemon that cannot be built is skipped; the bench
            // ends up short rather than the whole draw failing.
            Err(_) => continue,
        }
    }
}

// ── Self-check ───────────────────────────────────────────────────────────────

/// Verify a determinized world against the belief it came from.
///
/// This deliberately covers what `subset_check::collect_true_state_subset_violations`
/// does *not*. That oracle documents three blind spots (EVs/IVs, HP, and
/// revealed move slots) because the inference engine's EV/IV literal encodings
/// can legitimately sit outside the generic window, and because it was written
/// to validate a real battle's state rather than a synthesized one. For a
/// determinized world those are precisely the fields most likely to be wrong, so
/// they get checked here instead.
///
/// Returns human-readable violations; empty means clean. Non-panicking, so it is
/// usable as an assertion in tests and as a debug check in a server.
pub fn check_determinization(
    result: &Determinized,
    belief: &UnknownBattleState,
    pokemon_dex: &HashMap<Species, PokemonData>,
) -> Vec<String> {
    let mut problems = Vec::new();

    for (player, actives, concrete_active, concrete_back) in [
        (
            Player::P1,
            &belief.p1_active_mons,
            &result.state.p1_active_mons,
            &result.state.p1_back_mons,
        ),
        (
            Player::P2,
            &belief.p2_active_mons,
            &result.state.p2_active_mons,
            &result.state.p2_back_mons,
        ),
    ] {
        // Active slots are positional, so they can be paired directly.
        for (slot, (unk, mon)) in actives.iter().zip(concrete_active.iter()).enumerate() {
            let label = format!("{player:?} active slot {slot}");
            let mut direct = Vec::new();
            check_one(
                &label,
                unk,
                mon,
                pokemon_dex,
                &mut direct,
            );
            if !direct.is_empty()
                && let Some(hypothesis) = unk.possible_illusion_state.as_deref()
            {
                let mut alternate = Vec::new();
                check_one(&label, hypothesis, mon, pokemon_dex, &mut alternate);
                if alternate.is_empty() {
                    continue;
                }
            }
            problems.extend(direct);
        }

        // `mon_id` collisions would make `build_mon_idx_map` pair belief entries
        // with the wrong Pokemon, silently checking the wrong constraints.
        let mut seen: HashSet<u8> = HashSet::new();
        for mon in concrete_active.iter().chain(concrete_back.iter()) {
            if !seen.insert(mon.mon_id) {
                problems.push(format!(
                    "{player:?}: duplicate mon_id {} ({:?})",
                    mon.mon_id, mon.species
                ));
            }
        }

        // Item clause: one team may not hold the same item twice. The sampler
        // threads a used-item set through its draws to enforce this, and the
        // repair loop can also reject a world on it, so a duplicate reaching
        // here means one of those two lost track.
        //
        // `Item::None` is exempt — any number of Pokemon can hold nothing, and
        // a consumed or knocked-off item reads as `None` mid-battle. This
        // therefore checks the clause as the state currently shows it, which is
        // weaker than checking the teambuilder's original assignment: two
        // Pokemon that were built with the same item but have since consumed one
        // of them look legal here. The belief does not carry the pre-consumption
        // item for the hidden side, so that stronger check is not available.
        let mut held: HashSet<Item> = HashSet::new();
        for mon in concrete_active.iter().chain(concrete_back.iter()) {
            if mon.item != Item::None && !held.insert(mon.item.clone()) {
                problems.push(format!(
                    "{player:?}: item clause violated, {:?} held more than once \
                     (seen again on {:?})",
                    mon.item, mon.species
                ));
            }
        }

        // Nothing may appear on the bench that the belief never offered.
        let (known_back, possible_back, fainted) = match player {
            Player::P1 => (
                &belief.p1_known_back_mons,
                &belief.p1_possible_back_mons,
                &belief.p1_fainted_mons,
            ),
            Player::P2 => (
                &belief.p2_known_back_mons,
                &belief.p2_possible_back_mons,
                &belief.p2_fainted_mons,
            ),
        };
        let invented: HashSet<Species> = result
            .warnings
            .iter()
            .filter_map(|w| match w {
                DeterminizeWarning::InventedBenchMon { species } => Some(species.clone()),
                _ => None,
            })
            .collect();
        let mut candidates: HashSet<Species> = known_back
            .iter()
            .chain(possible_back.iter())
            .chain(fainted.iter())
            .filter_map(known_species_of)
            .collect();
        // When an active Illusion hypothesis is committed, the active primary
        // is the real decoy on the bench. The unresolved belief represents that
        // identity swap as "shown species active + explicit Zoroark back", so
        // the decoy is offered by the active entry rather than a back bucket.
        for (unk, mon) in actives.iter().zip(concrete_active.iter()) {
            if unk.possible_illusion_state.is_some()
                && known_species_of(unk).as_ref() != Some(&mon.species)
                && let Some(decoy) = known_species_of(unk)
            {
                candidates.insert(decoy);
            }
        }
        for mon in concrete_back {
            if !candidates.contains(&mon.species) && !invented.contains(&mon.species) {
                problems.push(format!(
                    "{player:?}: bench holds {:?}, which the belief never offered \
                     and which was not reported as invented",
                    mon.species
                ));
            }
        }
    }

    problems
}

/// Check one concrete Pokemon against its belief entry.
fn check_one(
    label: &str,
    unk: &UnknownPokemonState,
    mon: &PokemonState,
    pokemon_dex: &HashMap<Species, PokemonData>,
    problems: &mut Vec<String>,
) {
    // Revealed moves must keep their slots: `move_pp[i]`,
    // `used_moves_this_field[i]` and Last Resort are all slot-indexed.
    for i in 0..4 {
        if let Some(known) = &unk.known_moves[i]
            && mon.moves[i].as_ref() != Some(known) {
                problems.push(format!(
                    "{label}: slot {i} should be {known:?} but is {:?}",
                    mon.moves[i]
                ));
            }
    }

    for i in 0..6 {
        if mon.evs[i] < unk.min_evs[i] || mon.evs[i] > unk.max_evs[i] {
            problems.push(format!(
                "{label}: EV[{i}] = {} outside {}..={}",
                mon.evs[i], unk.min_evs[i], unk.max_evs[i]
            ));
        }
        if mon.ivs[i] < unk.min_ivs[i] || mon.ivs[i] > unk.max_ivs[i] {
            problems.push(format!(
                "{label}: IV[{i}] = {} outside {}..={}",
                mon.ivs[i], unk.min_ivs[i], unk.max_ivs[i]
            ));
        }
        if mon.stats[i] < unk.min_stats[i] || mon.stats[i] > unk.max_stats[i] {
            problems.push(format!(
                "{label}: stat[{i}] = {} outside {}..={}",
                mon.stats[i], unk.min_stats[i], unk.max_stats[i]
            ));
        }
    }

    // EVs and stats must be mutually consistent — the specific inconsistency
    // `materialize_pokemon` has, where hardcoded EVs sit beside an unrelated
    // stat override.
    //
    // S26: this relationship deliberately does not hold while Transformed.
    // `species` and `stats[1..=5]` belong to the copy target, while EVs, IVs,
    // nature, and `stats[0]` still belong to the transformer. Recomputing here
    // would mix those two Pokemon and make this determinizer-specific checker
    // repeat the false positive already carved out of `subset_check`.
    if mon.pre_transform.is_none()
        && let Some(data) = pokemon_dex.get(&mon.species)
    {
        let recomputed =
            calc_stats_for_level(data.base_stats, mon.ivs, mon.evs, mon.level, &mon.nature);
        if recomputed != mon.stats {
            problems.push(format!(
                "{label}: stats {:?} disagree with EVs/IVs/nature (recomputed {recomputed:?})",
                mon.stats
            ));
        }

        let mut pre = [0u16; 6];
        pre[0] = calc_hp(data.base_stats[0], mon.ivs[0], mon.evs[0], mon.level);
        for (i, value) in pre.iter_mut().enumerate().skip(1) {
            *value = calc_stat(data.base_stats[i], mon.ivs[i], mon.evs[i], mon.level, 1.0);
        }
        for (i, ((value, lo), hi)) in pre
            .iter()
            .zip(unk.min_pre_nature_stat.iter())
            .zip(unk.max_pre_nature_stat.iter())
            .enumerate()
        {
            if value < lo || value > hi {
                problems.push(format!(
                    "{label}: pre-nature stat[{i}] = {value} outside {lo}..={hi}"
                ));
            }
        }
    }

    // HP must land in the band that actually displays as the observed
    // percentage, not merely somewhere plausible.
    match unk.hp {
        PokemonHP::Number(n) => {
            if mon.hp != n.min(mon.stats[0]) {
                problems.push(format!("{label}: HP {} should be {n}", mon.hp));
            }
        }
        PokemonHP::Percent(pct) => {
            if !unk.fainted
                && let Some((lo, hi)) = percent_bucket(pct, mon.stats[0])
                    && (mon.hp < lo || mon.hp > hi) {
                        problems.push(format!(
                            "{label}: HP {} outside the {lo}..={hi} band for {pct}%",
                            mon.hp
                        ));
                    }
        }
    }

    if unk.fainted != mon.fainted {
        problems.push(format!(
            "{label}: fainted {} but belief says {}",
            mon.fainted, unk.fainted
        ));
    }
    if mon.fainted && mon.hp != 0 {
        problems.push(format!("{label}: fainted with {} HP", mon.hp));
    }
    if !mon.fainted && mon.hp == 0 {
        problems.push(format!("{label}: 0 HP but not fainted"));
    }

    // A move slot with no PP is unusable, which would silently shrink the legal
    // command set rather than erroring.
    for i in 0..4 {
        if mon.moves[i].is_some() && mon.max_pp[i] == 0 {
            problems.push(format!("{label}: move slot {i} has 0 max PP"));
        }
    }

    if unknown_is_excluded(&unk.item, &mon.item) {
        problems.push(format!(
            "{label}: item {:?} is excluded by the belief",
            mon.item
        ));
    }
    if unknown_is_excluded(&unk.possible_abilities, &mon.ability) {
        problems.push(format!(
            "{label}: ability {:?} is excluded by the belief",
            mon.ability
        ));
    }
    if unknown_is_excluded(&unk.possible_natures, &mon.nature) {
        problems.push(format!(
            "{label}: nature {:?} is excluded by the belief",
            mon.nature
        ));
    }
}

/// Build the `BattleState` itself.
fn assemble_battle(
    belief: &UnknownBattleState,
    p1: SideRoster,
    p2: SideRoster,
    cfg: &DeterminizeConfig,
    log: &mut DrawLog,
) -> BattleState {
    BattleState {
        active_per_side: belief.active_per_side,
        p1_active_mons: p1.active,
        p2_active_mons: p2.active,
        // Populated, unlike `materialize_battle`'s always-empty benches. An
        // empty bench makes every switch command illegal and is the single
        // biggest reason that function's output cannot be played.
        p1_back_mons: p1.back,
        p2_back_mons: p2.back,
        action_queue: Vec::new(),
        turn_number: belief.turn_number,
        turn_started: belief.turn_started,
        turn_ended: belief.turn_ended,
        p1_has_tera: belief.p1_has_tera,
        p2_has_tera: belief.p2_has_tera,
        p1_has_mega: belief.p1_has_mega,
        p2_has_mega: belief.p2_has_mega,
        weather: belief.weather.clone(),
        weather_turns: resolve_timer_opt(&belief.weather_turns, cfg, log),
        pseudo_weathers: belief.pseudo_weathers.clone(),
        pseudo_weather_turns: belief
            .pseudo_weather_turns
            .iter()
            .map(|t| resolve_timer(t, cfg, log))
            .collect(),
        terrain: belief.terrain.clone(),
        terrain_turns: resolve_timer_opt(&belief.terrain_turns, cfg, log),
        p1_side_conditions: belief.p1_side_conditions.clone(),
        p1_side_condition_turns: belief
            .p1_side_condition_turns
            .iter()
            .map(|t| resolve_timer(t, cfg, log))
            .collect(),
        p2_side_conditions: belief.p2_side_conditions.clone(),
        p2_side_condition_turns: belief
            .p2_side_condition_turns
            .iter()
            .map(|t| resolve_timer(t, cfg, log))
            .collect(),
        p1_slot_conditions: belief.p1_slot_conditions.clone(),
        p2_slot_conditions: belief.p2_slot_conditions.clone(),
        // Correct as-is at a turn boundary, which is the only point a belief is
        // ever determinized from.
        self_switch_pending: None,
        items_consumed_this_turn: Vec::new(),
        last_move_on_field: belief.last_move_on_field.clone(),
        sub_damage_dealt: belief.sub_damage_dealt,
        gross_damage_dealt: 0,
        round_used_this_turn: belief.round_used_this_turn,
        move_was_prevented: false,
        resolved_move_targets: vec![],
        pending_events: vec![],
        event_observer: None,
        double_ko: None,
    }
}
