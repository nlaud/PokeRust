//! Checks that a belief still contains the true concrete state.
//!
//! `apply_information` detects an internally empty belief.
//! This module detects a nonempty belief that excludes the truth.
//! It checks each hidden opponent field after every turn.
//! It also checks each CNF clause against the true assignment.
//!
//! `random_doubles_battles_are_sound` uses this check.
//!
//! ## Design
//!
//! `!unknown_is_excluded(field, &true_value)` checks one `Unknown<T>` field.
//! Numeric fields require `min <= true <= max`.
//!
//! `build_mon_idx_map` maps each true opponent to one belief entry.
//! Active Pokémon use their positions.
//! Bench Pokémon match by `mon_id` or species.
//! An Illusion user can match its primary or alternate hypothesis.
//! One complete hypothesis must contain all true fields.
//!
//! `clause_holds_under_truth` checks each CNF clause.
//! An unknown literal result cannot cause a failure.
//! Thus, this conservative check can miss a violation but cannot invent one.
//!
//! A failure panics with a `[subset violation]` message.

use std::collections::HashMap;
use std::fmt;

use crate::data::pokemon_move::PokemonMove;
use crate::data::species::Species;
use crate::information::inference::{get_mon_by_idx, mon_idx_legend, unknown_is_excluded};
use crate::information::unknowns::{
    Statement, Unknown, UnknownBattleState, UnknownMatchState, UnknownPokemonState,
};
use crate::simulator::helpers::single_type_effectiveness;
use crate::state::battle::{BattleState, Player};
use crate::state::dex_data::{MoveData, PokemonData, PokemonStat};
use crate::state::pokemon::{PokemonState, calc_hp, calc_stat, nature_stat_modifiers};

#[derive(Debug, Clone)]
pub struct FieldViolation {
    pub field: String,
    pub detail: String,
}

#[derive(Debug, Clone)]
pub enum SubsetViolationKind {
    Fields {
        mon_idx: usize,
        true_species: Species,
        true_mon_id: u8,
        violations: Vec<FieldViolation>,
    },
    Clause {
        clause: Vec<Statement>,
    },
}

#[derive(Debug, Clone)]
pub struct SubsetViolation {
    pub observer: Player,
    pub kind: SubsetViolationKind,
    pub legend: String,
}

impl fmt::Display for SubsetViolation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            SubsetViolationKind::Fields {
                mon_idx,
                true_species,
                true_mon_id,
                violations,
            } => {
                let details: Vec<&str> = violations.iter().map(|v| v.detail.as_str()).collect();
                write!(
                    f,
                    "observer={:?} mon_idx={mon_idx} true_species={true_species:?} true_mon_id={true_mon_id} violations={details:?} (legend: {})",
                    self.observer, self.legend
                )
            }
            SubsetViolationKind::Clause { clause } => write!(
                f,
                "observer={:?} clause unsatisfiable by ground truth: {clause:?} (legend: {})",
                self.observer, self.legend
            ),
        }
    }
}

/// Entry point: assert that `true_state` is a member of the set `belief` (as seen by
/// `observer`) admits. Panics on the first violation found. `context` is prepended
/// to the panic message (iteration/turn/matchup) so a fuzz-sweep failure points
/// straight at the offending trajectory, matching the existing
/// `inference_contradiction!` oracle's style.
///
/// No-op outside battle phase (team preview / game over) — the belief shapes this
/// module reasons about (`UnknownBattleState`'s `mon_idx` space and predicate list)
/// only exist there.
pub fn assert_true_state_subset_of_belief(
    true_state: &BattleState,
    belief: &UnknownMatchState,
    observer: Player,
    pdex: &HashMap<Species, PokemonData>,
    mdex: &HashMap<PokemonMove, MoveData>,
    context: &str,
) {
    if let Some(violation) =
        collect_true_state_subset_violations(true_state, belief, observer, pdex, mdex)
            .into_iter()
            .next()
    {
        panic!("[subset violation] context={context} {violation}");
    }
}

/// Structured, non-panicking form of the subset oracle. Diagnostic sweeps use
/// this directly so bucketing never depends on parsing a formatted panic.
pub fn collect_true_state_subset_violations(
    true_state: &BattleState,
    belief: &UnknownMatchState,
    observer: Player,
    pdex: &HashMap<Species, PokemonData>,
    mdex: &HashMap<PokemonMove, MoveData>,
) -> Vec<SubsetViolation> {
    let UnknownMatchState::Battle(belief) = belief else {
        return Vec::new();
    };
    let opponent = match observer {
        Player::P1 => Player::P2,
        Player::P2 => Player::P1,
    };

    let mapping = build_mon_idx_map(true_state, belief, opponent, pdex);
    let legend = mon_idx_legend(belief);
    let mut violations = Vec::new();

    let mut idxs: Vec<&usize> = mapping.keys().collect();
    idxs.sort();
    for idx in idxs {
        let truth = mapping[idx];
        if let Some(field_violations) =
            mon_violation_with_illusion_alternates(belief, opponent, *idx, truth, pdex)
        {
            violations.push(SubsetViolation {
                observer,
                kind: SubsetViolationKind::Fields {
                    mon_idx: *idx,
                    true_species: truth.species.clone(),
                    true_mon_id: truth.mon_id,
                    violations: field_violations,
                },
                legend: legend.clone(),
            });
        }
    }

    for clause in &belief.predicates {
        if !clause_holds_under_truth(clause, &mapping, true_state, mdex, pdex) {
            violations.push(SubsetViolation {
                observer,
                kind: SubsetViolationKind::Clause {
                    clause: clause.clone(),
                },
                legend: legend.clone(),
            });
        }
    }
    violations
}

/// Active slots are normally positional, but the parallel Illusion abstraction
/// can temporarily leave two physical records with the same shown species whose
/// locations are correlated but not represented. In that narrow unresolved case,
/// the sound marginal for build fields is the union across those complete records.
fn mon_violation_with_illusion_alternates(
    belief: &UnknownBattleState,
    opponent: Player,
    idx: usize,
    truth: &PokemonState,
    pdex: &HashMap<Species, PokemonData>,
) -> Option<Vec<FieldViolation>> {
    let direct = mon_violation(belief, idx, truth, pdex)?;
    let unresolved = match opponent {
        Player::P1 => belief.p1_unresolved_zoroark_count,
        Player::P2 => belief.p2_unresolved_zoroark_count,
    };
    if unresolved == 0 {
        return Some(direct);
    }
    let alternatives: Box<dyn Iterator<Item = &UnknownPokemonState> + '_> = match opponent {
        Player::P1 => Box::new(
            belief
                .p1_active_mons
                .iter()
                .chain(belief.p1_known_back_mons.iter())
                .chain(belief.p1_possible_back_mons.iter()),
        ),
        Player::P2 => Box::new(
            belief
                .p2_active_mons
                .iter()
                .chain(belief.p2_known_back_mons.iter())
                .chain(belief.p2_possible_back_mons.iter()),
        ),
    };
    if alternatives
        .filter(|entry| !unknown_is_excluded(&entry.possible_species, &truth.species))
        .any(|entry| entry_admits_truth(entry, truth, pdex))
    {
        None
    } else {
        Some(direct)
    }
}

// ── B2: mon_idx -> true PokemonState mapping ────────────────────────────────────

/// Absolute `mon_idx` of the first entry in each of `get_mon_by_idx`'s 6 segments
/// (`[p1_active, p2_active, p1_known_back, p1_possible_back, p2_known_back,
/// p2_possible_back]`), restricted to the 3 segments that belong to `opponent`:
/// `(active_start, known_back_start, possible_back_start)`. Mirrors
/// `get_mon_by_idx`'s own offset accumulation exactly — see that function's doc
/// comment in `information::inference` for the canonical segment order.
fn opponent_segment_starts(belief: &UnknownBattleState, opponent: Player) -> (usize, usize, usize) {
    let p1a = belief.p1_active_mons.len();
    let p2a = belief.p2_active_mons.len();
    let p1k = belief.p1_known_back_mons.len();
    let p1p = belief.p1_possible_back_mons.len();
    let p2k = belief.p2_known_back_mons.len();
    match opponent {
        Player::P1 => (0, p1a + p2a, p1a + p2a + p1k),
        Player::P2 => (p1a, p1a + p2a + p1k + p1p, p1a + p2a + p1k + p1p + p2k),
    }
}

/// Map every true opponent `PokemonState` to the `mon_idx` of the belief entry
/// that's supposed to represent it. Active mons are positional (same field slot);
/// bench mons are matched existentially against `known_back ∪ possible_back` by
/// `mon_id` (preferred, exact) or species (fallback — unambiguous under Species
/// Clause), with distinct greedy assignment so two true mons never claim the same
/// belief entry.
///
/// A true mon that can't be confidently placed is skipped (logged, not a failure —
/// see the module doc's soundness note: an indeterminate mapping must never
/// manufacture a violation). Fainted bench mons are always skipped: the belief
/// purges a fainted-and-replaced mon's bench entry into `p{side}_fainted_mons`,
/// outside the `mon_idx` space entirely (see that field's doc comment in
/// `information::unknowns`), so there is no `known_back`/`possible_back` entry left
/// to match against.
fn build_mon_idx_map<'a>(
    true_state: &'a BattleState,
    belief: &UnknownBattleState,
    opponent: Player,
    pdex: &HashMap<Species, PokemonData>,
) -> HashMap<usize, &'a PokemonState> {
    let mut map = HashMap::new();
    let (active_start, known_back_start, possible_back_start) =
        opponent_segment_starts(belief, opponent);

    let (true_active, true_back, known_back, possible_back) = match opponent {
        Player::P1 => (
            &true_state.p1_active_mons,
            &true_state.p1_back_mons,
            &belief.p1_known_back_mons,
            &belief.p1_possible_back_mons,
        ),
        Player::P2 => (
            &true_state.p2_active_mons,
            &true_state.p2_back_mons,
            &belief.p2_known_back_mons,
            &belief.p2_possible_back_mons,
        ),
    };

    for (i, true_mon) in true_active.iter().enumerate() {
        map.insert(active_start + i, true_mon);
    }

    let bench_candidates: Vec<(usize, &UnknownPokemonState)> = known_back
        .iter()
        .enumerate()
        .map(|(i, m)| (known_back_start + i, m))
        .chain(
            possible_back
                .iter()
                .enumerate()
                .map(|(i, m)| (possible_back_start + i, m)),
        )
        .collect();
    let mut claimed: Vec<usize> = Vec::new();

    for true_mon in true_back {
        if true_mon.fainted {
            continue;
        }
        let by_id = bench_candidates.iter().find(|(idx, m)| {
            !claimed.contains(idx)
                && matches!(&m.possible_mon_id, Unknown::Known(id) if *id == true_mon.mon_id)
        });
        let species_candidates = || {
            bench_candidates.iter().filter(|(idx, m)| {
                !claimed.contains(idx)
                    && !unknown_is_excluded(&m.possible_species, &true_mon.species)
            })
        };
        // Illusion can leave two distinct physical records carrying the same
        // shown species while their identity correlation is unresolved. This
        // mapping is documented as existential, so prefer a candidate whose
        // complete primary OR hypothesis admits the true mon instead of greedily
        // assigning the first species match (which can be the stale disguise).
        // Keep a species-only fallback so a genuine exclusion is still mapped and
        // reported rather than silently skipped.
        let chosen = by_id
            .or_else(|| species_candidates().find(|(_, m)| entry_admits_truth(m, true_mon, pdex)))
            .or_else(|| species_candidates().next());
        match chosen {
            Some((idx, _)) => {
                claimed.push(*idx);
                map.insert(*idx, true_mon);
            }
            None => {
                eprintln!(
                    "[subset_check] skip: no belief entry admits bench mon {:?} (mon_id={})",
                    true_mon.species, true_mon.mon_id
                );
            }
        }
    }

    map
}

fn entry_admits_truth(
    entry: &UnknownPokemonState,
    truth: &PokemonState,
    pdex: &HashMap<Species, PokemonData>,
) -> bool {
    field_violations(entry, truth, pdex).is_empty()
        || entry
            .possible_illusion_state
            .as_deref()
            .is_some_and(|hypothesis| field_violations(hypothesis, truth, pdex).is_empty())
}

// ── B1: per-mon field containment ───────────────────────────────────────────────

/// `None` if `truth` is admitted by `belief`'s entry at `idx` (primary alone, or its
/// live Illusion hypothesis alone — see the module doc's union-not-per-field-mix
/// note); `Some(description)` of the violation otherwise.
fn mon_violation(
    belief: &UnknownBattleState,
    idx: usize,
    truth: &PokemonState,
    pdex: &HashMap<Species, PokemonData>,
) -> Option<Vec<FieldViolation>> {
    let entry = get_mon_by_idx(belief, idx)?;
    let primary = field_violations(entry, truth, pdex);
    if primary.is_empty() {
        return None;
    }
    if let Some(hyp) = entry.possible_illusion_state.as_deref() {
        let hyp_violations = field_violations(hyp, truth, pdex);
        if hyp_violations.is_empty() {
            return None;
        }
        if std::env::var("POKERUST_FUZZ_REPLAY").is_ok() {
            eprintln!(
                "[ILLUSION-HYP-VIOLATIONS] mon_idx={idx} true_species={:?} violations={hyp_violations:?}",
                truth.species
            );
        }
    }
    Some(primary)
}

/// Every field-level way `entry` could fail to admit `truth`. Empty = fully admitted.
///
/// Deliberately excludes a few fields the plan scoped out:
/// - EV/IV bound containment (only the resulting **stat** window is checked) —
///   under `force_max_ivs`/`use_stat_points` a teamsheet's literal EV/IV encoding
///   need not lie inside the generic `[0,252]`/`[0,31]` window even when the
///   resulting stat is in-bounds, so checking EVs/IVs directly risks a false
///   positive; the stat window is the robust invariant.
/// - HP: the belief's `PokemonHP` is a direct rounding of the true HP, set
///   whenever HP changes — not an independently inferred bound, so there's nothing
///   for this oracle to usefully catch there.
/// - known move slots / `possible_original_abilities`: out of the field set this
///   check was scoped to cover.
fn field_violations(
    entry: &UnknownPokemonState,
    truth: &PokemonState,
    pdex: &HashMap<Species, PokemonData>,
) -> Vec<FieldViolation> {
    let mut v = Vec::new();

    macro_rules! check {
        ($field:expr, $truth_val:expr, $name:literal) => {
            if unknown_is_excluded($field, $truth_val) {
                v.push(FieldViolation {
                    field: $name.to_string(),
                    detail: format!(
                        "{}: belief {:?} excludes true value {:?}",
                        $name, $field, $truth_val
                    ),
                });
            }
        };
    }
    check!(&entry.possible_species, &truth.species, "species");
    check!(&entry.item, &truth.item, "item");
    check!(&entry.possible_abilities, &truth.ability, "ability");
    check!(&entry.possible_natures, &truth.nature, "nature");
    check!(&entry.possible_tera_type, &truth.tera_type, "tera_type");
    check!(&entry.possible_genders, &truth.gender, "gender");
    check!(&entry.possible_weight_hg, &truth.weight_hg, "weight_hg");
    check!(&entry.mega_species, &truth.mega_species, "mega_species");
    check!(&entry.mega_ability, &truth.mega_ability, "mega_ability");
    check!(&entry.possible_mon_id, &truth.mon_id, "mon_id");

    for i in 0..6 {
        if truth.stats[i] < entry.min_stats[i] || truth.stats[i] > entry.max_stats[i] {
            v.push(FieldViolation {
                field: format!("stats[{i}]"),
                detail: format!(
                    "stats[{i}]: true={} not in [{},{}]",
                    truth.stats[i], entry.min_stats[i], entry.max_stats[i]
                ),
            });
        }
    }

    // S26 (as in `pass5_back_solve` and the Pass 3 inversions): a Transformed
    // mon's `stats[1..=5]` are COPIED from the copy source, not produced by the
    // stat formula, and its species is the target's while its EVs/IVs are still
    // its own. Re-deriving pre-nature stats from `species + evs/ivs` therefore
    // mixes two Pokemon and reports a violation on a perfectly legal state. The
    // `stats` window above stays in force — the belief keeps index 0 describing
    // the transformer and 1..6 the target, which is exactly what the truth holds.
    if truth.pre_transform.is_some() {
        return v;
    }

    if let Some(data) = pdex.get(&truth.species) {
        let base = data.base_stats;
        let true_pre_nature: [u16; 6] = [
            calc_hp(base[0], truth.ivs[0], truth.evs[0], truth.level),
            calc_stat(base[1], truth.ivs[1], truth.evs[1], truth.level, 1.0),
            calc_stat(base[2], truth.ivs[2], truth.evs[2], truth.level, 1.0),
            calc_stat(base[3], truth.ivs[3], truth.evs[3], truth.level, 1.0),
            calc_stat(base[4], truth.ivs[4], truth.evs[4], truth.level, 1.0),
            calc_stat(base[5], truth.ivs[5], truth.evs[5], truth.level, 1.0),
        ];
        for (i, &true_val) in true_pre_nature.iter().enumerate() {
            if true_val < entry.min_pre_nature_stat[i] || true_val > entry.max_pre_nature_stat[i] {
                v.push(FieldViolation {
                    field: format!("pre_nature_stat[{i}]"),
                    detail: format!(
                        "pre_nature_stat[{i}]: true={true_val} not in [{},{}]",
                        entry.min_pre_nature_stat[i], entry.max_pre_nature_stat[i]
                    ),
                });
            }
        }
    }

    v
}

// ── B3: CNF predicate satisfiability against ground truth ──────────────────────

/// `stat` restricted to the 5-element `nature_stat_modifiers` layout (Atk..Spe,
/// no HP — `PokemonStat` itself has no HP variant, matching that every
/// `NatureBoostsStat`/`NatureNerfsStat`/`EVIVStatGE`/`EVIVStatLE` predicate is
/// inherently non-HP).
fn nature_mod_idx(stat: &PokemonStat) -> usize {
    match stat {
        PokemonStat::Atk => 0,
        PokemonStat::Def => 1,
        PokemonStat::SpA => 2,
        PokemonStat::SpD => 3,
        PokemonStat::Spe => 4,
    }
}

/// `stat` restricted to the 6-element `PokemonStatsTable` layout (index 0 = HP,
/// never targeted by `stat`). Mirrors `inference::bcp::stat_to_stats_idx` — kept as
/// a local copy rather than widening that function's visibility for one caller.
fn stats_table_idx(stat: &PokemonStat) -> usize {
    match stat {
        PokemonStat::Atk => 1,
        PokemonStat::Def => 2,
        PokemonStat::SpA => 3,
        PokemonStat::SpD => 4,
        PokemonStat::Spe => 5,
    }
}

fn true_pre_nature_stat(
    truth: &PokemonState,
    pdex: &HashMap<Species, PokemonData>,
    stat: &PokemonStat,
) -> Option<u16> {
    let data = pdex.get(&truth.species)?;
    let idx = stats_table_idx(stat);
    Some(calc_stat(
        data.base_stats[idx],
        truth.ivs[idx],
        truth.evs[idx],
        truth.level,
        1.0,
    ))
}

/// One OHKO move away from a `KnowsThreateningMove` true-positive regardless of
/// type effectiveness — mirrors the `Statement`'s own doc comment.
fn is_ohko_move(m: &PokemonMove) -> bool {
    matches!(
        m,
        PokemonMove::Fissure
            | PokemonMove::Guillotine
            | PokemonMove::HornDrill
            | PokemonMove::SheerCold
    )
}

/// `Some(true)` / `Some(false)` when this literal's truth is determinable from
/// `mapping`/`true_state`; `None` ("indeterminate") when it isn't — an unmapped
/// `mon_idx`, or a payload this module doesn't model precisely. Callers must treat
/// `None` as "might satisfy the clause," never as `Some(false)` — see the module
/// doc's note on why this has to be three-valued (not just a bool defaulting to
/// `true`) for `Not(_)` to stay sound.
fn eval_literal(
    lit: &Statement,
    mapping: &HashMap<usize, &PokemonState>,
    true_state: &BattleState,
    mdex: &HashMap<PokemonMove, MoveData>,
    pdex: &HashMap<Species, PokemonData>,
) -> Option<bool> {
    match lit {
        Statement::Not(inner) => eval_literal(inner, mapping, true_state, mdex, pdex).map(|b| !b),
        Statement::HasItem { mon_idx, item } => mapping.get(mon_idx).map(|t| t.item == *item),
        Statement::HasAbility { mon_idx, ability } => {
            mapping.get(mon_idx).map(|t| t.ability == *ability)
        }
        Statement::NatureBoostsStat { mon_idx, stat } => mapping
            .get(mon_idx)
            .map(|t| nature_stat_modifiers(&t.nature)[nature_mod_idx(stat)] > 1.0),
        Statement::NatureNerfsStat { mon_idx, stat } => mapping
            .get(mon_idx)
            .map(|t| nature_stat_modifiers(&t.nature)[nature_mod_idx(stat)] < 1.0),
        Statement::EVIVStatGE {
            mon_idx,
            stat,
            value,
        } => mapping
            .get(mon_idx)
            .and_then(|t| true_pre_nature_stat(t, pdex, stat))
            .map(|v| v >= *value),
        Statement::EVIVStatLE {
            mon_idx,
            stat,
            value,
        } => mapping
            .get(mon_idx)
            .and_then(|t| true_pre_nature_stat(t, pdex, stat))
            .map(|v| v <= *value),
        Statement::SpeedComparison {
            fast_idx,
            slow_idx,
            fast_mult,
            slow_mult,
        } => match (mapping.get(fast_idx), mapping.get(slow_idx)) {
            (Some(f), Some(s)) => Some(
                f.stats[5] as u64 * (*fast_mult as u64) >= s.stats[5] as u64 * (*slow_mult as u64),
            ),
            _ => None,
        },
        Statement::WeatherTurns { turns } => true_state.weather_turns.map(|t| t as usize == *turns),
        Statement::TerrainTurns { turns } => true_state.terrain_turns.map(|t| t as usize == *turns),
        Statement::SideConditionTurns {
            side,
            side_condition,
            turns,
        } => {
            let (conditions, turns_vec) = match side {
                Player::P1 => (
                    &true_state.p1_side_conditions,
                    &true_state.p1_side_condition_turns,
                ),
                Player::P2 => (
                    &true_state.p2_side_conditions,
                    &true_state.p2_side_condition_turns,
                ),
            };
            conditions
                .iter()
                .position(|c| c == side_condition)
                .and_then(|i| turns_vec.get(i))
                .map(|t| *t as usize == *turns)
        }
        Statement::KnowsThreateningMove {
            mon_idx,
            defender_types,
        } => mapping.get(mon_idx).map(|t| {
            t.moves.iter().flatten().any(|m| {
                if is_ohko_move(m) {
                    return true;
                }
                mdex.get(m).is_some_and(|data| {
                    defender_types.iter().fold(1.0, |acc, dt| {
                        acc * single_type_effectiveness(&data.pokemon_type, dt)
                    }) > 1.0
                })
            })
        }),
    }
}

/// `true` unless every literal in `clause` is definitively `Some(false)` under
/// ground truth — an indeterminate (`None`) literal always keeps the clause alive,
/// per `eval_literal`'s doc comment.
fn clause_holds_under_truth(
    clause: &[Statement],
    mapping: &HashMap<usize, &PokemonState>,
    true_state: &BattleState,
    mdex: &HashMap<PokemonMove, MoveData>,
    pdex: &HashMap<Species, PokemonData>,
) -> bool {
    clause
        .iter()
        .any(|lit| eval_literal(lit, mapping, true_state, mdex, pdex) != Some(false))
}
