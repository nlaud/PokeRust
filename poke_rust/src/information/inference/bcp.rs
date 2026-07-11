//! Pass 6: Boolean Constraint Propagation (BCP) to fixpoint.
//!
//! Propagates disjunctive constraints (CNF predicates) by:
//! 1. Removing literals known to be false from each clause.
//! 2. Dropping clauses with a known-true literal.
//! 3. Forcing unit clauses into the state.
//! 4. Propagating `SpeedComparison` Spe bounds bidirectionally.

#![allow(unused)]

use std::collections::HashMap;

use crate::data::item::Item;
use crate::data::pokemon_move::PokemonMove;
use crate::data::species::Species;
use crate::information::unknowns::{Statement, Unknown, UnknownBattleState};
use crate::state::battle::{FieldSlot, Player};
use crate::state::dex_data::{PokemonData, PokemonStat, SideCondition};
use crate::state::pokemon::Nature;

// ── Illusion-detection constant ────────────────────────────────────────────────

const ILLUSION_FORMES: &[Species] = &[Species::Zoroark, Species::ZoroarkHisui];

// ── Public interface (called from mod.rs orchestrator) ─────────────────────────

/// BCP fixpoint loop.  Mutates `state` until no more propagation is possible.
///
/// `dex`/`config` are needed only for the `HasSpecies` arm of `force_literal` (S30:
/// reconciling a BCP-forced Illusion-species resolution requires the same species-table
/// lookup and IV/EV-mode config that a fresh sighting or an `IllusionEnded` reveal uses).
pub(super) fn run_bcp(
    state: &mut UnknownBattleState,
    allow_repeat_items: bool,
    dex: &HashMap<Species, PokemonData>,
    config: &super::InferenceConfig,
) {
    let mut changed = true;
    // Collect SpeedComparison predicates once; re-collect only when the clause-pruning
    // loop actually mutates state.predicates (tracked via `clauses_changed`).
    let mut sc = collect_speed_comparisons(state);
    while changed {
        changed = false;
        let mut clauses_changed = false;

        let mut i = 0;
        while i < state.predicates.len() {
            let still_live: Vec<Statement> = state.predicates[i]
                .iter()
                .filter(|lit| !eval_false(state, lit))
                .cloned()
                .collect();

            if still_live.is_empty() {
                inference_contradiction!("bcp", "unsatisfiable clause (all literals false)");
            }

            // Clause already satisfied by a known-true literal — drop it.
            if still_live.iter().any(|lit| eval_true(state, lit)) {
                state.predicates.remove(i);
                changed = true;
                clauses_changed = true;
                continue;
            }

            // Unit clause — force the single remaining literal.
            // SpeedComparison and KnowsThreateningMove are permanent relational constraints;
            // they cannot be "forced" into a field and must remain in predicates for propagation.
            if still_live.len() == 1
                && !matches!(still_live[0], Statement::SpeedComparison { .. })
                && !matches!(still_live[0], Statement::KnowsThreateningMove { .. })
            {
                let lit = still_live[0].clone();
                state.predicates.remove(i);
                force_literal(state, &lit, allow_repeat_items, dex, config);
                changed = true;
                clauses_changed = true;
                continue;
            }

            if still_live.len() != state.predicates[i].len() {
                state.predicates[i] = still_live;
                changed = true;
                clauses_changed = true;
            }
            i += 1;
        }

        // Re-collect only when the clause list changed; otherwise reuse the cached list.
        if clauses_changed {
            sc = collect_speed_comparisons(state);
        }
        if propagate_collected(state, &sc) {
            changed = true;
        }
    }
}

/// Collect all valid `SpeedComparison` literals from the predicate set.
/// Separated from propagation so the caller can cache the list across BCP iterations
/// and only re-collect when the clause list actually changes.
///
/// S17: only **unit** clauses are collected. A `SpeedComparison` that shares its
/// clause with live escape disjuncts (Quick Claw, Quick Draw, Choice Scarf, Stall,
/// weather abilities, …) is a *conditional* constraint — the observed move order is
/// equally explained by any escape. Enforcing it as a hard Spe bound excludes every
/// escape world (e.g. a Quick Claw proc letting a slow mon move before a fast known
/// one raised the slow mon's min Spe above its species maximum → contradiction
/// panic). BCP prunes definitively-false literals every iteration, so a clause whose
/// escapes have all been excluded collapses to unit and is picked up here then.
pub(super) fn collect_speed_comparisons(state: &UnknownBattleState) -> Vec<(usize, usize, u32, u32)> {
    let total = super::mons_count_battle(state);
    state
        .predicates
        .iter()
        .filter(|clause| clause.len() == 1)
        .flat_map(|clause| {
            clause.iter().filter_map(|lit| {
                if let Statement::SpeedComparison {
                    fast_idx,
                    slow_idx,
                    fast_mult,
                    slow_mult,
                } = lit
                {
                    if *fast_idx < total && *slow_idx < total && *fast_mult > 0 && *slow_mult > 0 {
                        Some((*fast_idx, *slow_idx, *fast_mult, *slow_mult))
                    } else {
                        None
                    }
                } else {
                    None
                }
            })
        })
        .collect()
}

/// Bidirectional Spe bound propagation from a pre-collected list of `SpeedComparison`
/// tuples.  Returns `true` if any bound changed.
pub(super) fn propagate_collected(
    state: &mut UnknownBattleState,
    comparisons: &[(usize, usize, u32, u32)],
) -> bool {
    let mut changed = false;
    for &(fast_idx, slow_idx, fast_mult, slow_mult) in comparisons {
        // Raise fast's min Spe: base_spe(fast) >= ceil(base_spe(slow)*slow_mult / fast_mult)
        let slow_min = super::get_mon_by_idx(state, slow_idx).map_or(0u64, |m| m.minStats[5] as u64);
        let new_fast_min = div_ceil(slow_min * slow_mult as u64, fast_mult as u64) as u16;
        if let Some(mon) = super::get_mon_mut_by_idx(state, fast_idx) {
            if new_fast_min > mon.minStats[5] {
                if new_fast_min > mon.maxStats[5] {
                    inference_contradiction!(
                        fast_idx,
                        "SpeedComparison raises min({}) above max({})",
                        new_fast_min,
                        mon.maxStats[5]
                    );
                }
                mon.minStats[5] = new_fast_min;
                changed = true;
            }
        }

        // Lower slow's max Spe: base_spe(slow) <= floor(base_spe(fast)*fast_mult / slow_mult)
        let fast_max =
            super::get_mon_by_idx(state, fast_idx).map_or(u64::MAX / 2, |m| m.maxStats[5] as u64);
        let new_slow_max = (fast_max.saturating_mul(fast_mult as u64) / slow_mult as u64)
            .min(u16::MAX as u64) as u16;
        if let Some(mon) = super::get_mon_mut_by_idx(state, slow_idx) {
            if new_slow_max < mon.maxStats[5] {
                if new_slow_max < mon.minStats[5] {
                    inference_contradiction!(
                        slow_idx,
                        "SpeedComparison lowers max({}) below min({})",
                        new_slow_max,
                        mon.minStats[5]
                    );
                }
                mon.maxStats[5] = new_slow_max;
                changed = true;
            }
        }
    }
    changed
}

/// `true` if any species in `species` is an Illusion forme (Zoroark line). Used by
/// `pass1_switch` (S29) to decide whether an incoming "species" might be a disguise.
pub(super) fn contains_illusion_forme(species: &[Species]) -> bool {
    species.iter().any(|s| ILLUSION_FORMES.contains(s))
}

/// `true` if `species` is itself an Illusion forme.
pub(super) fn is_illusion_forme(species: &Species) -> bool {
    ILLUSION_FORMES.contains(species)
}

/// Widen `possible_species` to include Zoroark formes when the opponent's back
/// contains one and the on-field species is unconfirmed.  Call after a Switch.
///
/// `possible_types` is documented as corresponding 1:1 with `possible_species`
/// (`unknowns.rs`'s `UnknownPokemonState` field comment) but is left `Known` by every
/// other caller since a normal opponent's typing is openly visible in a real battle
/// (species → typing is public dex knowledge). A disguised Zoroark is the one case
/// where that 1:1 correspondence must actually hold: the *shown* species' typing is
/// what a real player sees, not the true one, so once `possible_species` widens to a
/// disjunction, `possible_types` widens in lockstep to the matching per-candidate
/// typing — otherwise the server DTO layer (`mask_pokemon_view`) would keep reporting
/// the disguise's typing as `Known` and confidently correct even while the species
/// underneath is genuinely unresolved.
pub(super) fn maybe_widen_for_illusion(
    state: &mut UnknownBattleState,
    slot: &FieldSlot,
    opponent_known_back_species: &[Species],
    dex: &HashMap<Species, PokemonData>,
) {
    let has_zoroark = opponent_known_back_species
        .iter()
        .any(|s| ILLUSION_FORMES.contains(s));
    if !has_zoroark {
        return;
    }
    let Some(idx) = super::mon_idx_for_active_slot(state, slot) else {
        return;
    };
    let Some(mon) = super::get_mon_mut_by_idx(state, idx) else {
        return;
    };
    if let Unknown::Known(ref s) = mon.possible_species.clone() {
        if !ILLUSION_FORMES.contains(s) {
            let mut candidates = vec![s.clone()];
            for zf in ILLUSION_FORMES {
                if opponent_known_back_species.contains(zf) {
                    candidates.push(zf.clone());
                }
            }
            if candidates.len() > 1 {
                let type_candidates: Vec<Vec<crate::state::dex_data::PokemonType>> = candidates
                    .iter()
                    .filter_map(|s| dex.get(s).map(|d| d.types.clone()))
                    .collect();
                // Only widen types if every candidate resolved a dex entry — species
                // with no dex data are already kept as species candidates elsewhere
                // (absence of data isn't evidence of inability), but silently dropping
                // a species' typing here would under-count the true type disjunction.
                if type_candidates.len() == candidates.len() {
                    mon.possible_types = Unknown::Possibly(type_candidates);
                }
                mon.possible_species = Unknown::Possibly(candidates);
            }
        }
    }
}

/// Companion to `maybe_widen_for_illusion`, called only after that widening actually
/// took effect (species is now `Possibly([shown, zoroark_forme, …])`). Ties the
/// slot's item to whichever physical identity it turns out to be.
///
/// Bulbapedia (Illusion, Effect section): the disguise copies only the visible
/// species/sprite — the held item is a real, mechanical property of the physical
/// Pokémon underneath (a disguised Zoroark's item really is Zoroark's own item, not
/// spoofed to match whichever mon it's impersonating). So if this slot is truly one
/// of the benched Illusion-forme candidates, its item must be THAT candidate's own
/// (separately tracked) item; if it's truly the shown/copied species, its item is
/// that fresh mon's own (otherwise-unconstrained) item — two disjoint hypotheses,
/// which is exactly what `Statement::HasSpecies` + `Statement::HasItem` clauses
/// encode: BCP collapses the item the moment species resolves either way (via a
/// learnset-narrowing collapse, or an `IllusionEnded` reveal).
///
/// `illusion_candidates` is `(species, current_item_bound)` for every benched
/// Illusion-forme entry that `maybe_widen_for_illusion` folded into the species
/// widening (S29: never consumed from the bench, so its own item knowledge is still
/// sitting there untouched — read it directly).
///
/// Item-candidate sets that are still `Unknown::Not(excluded)` (near-unbounded —
/// hundreds of items) are skipped for clause emission (see
/// `super::unknown_bounded_candidates`) to avoid a near-tautological clause; the
/// marginal `mon.item` is still correctly widened to the union either way, so this
/// only trades away precision in that case, never soundness.
pub(super) fn widen_item_for_illusion(
    state: &mut UnknownBattleState,
    slot: &FieldSlot,
    illusion_candidates: &[(Species, Unknown<Item>)],
) {
    if illusion_candidates.is_empty() {
        return;
    }
    let Some(idx) = super::mon_idx_for_active_slot(state, slot) else { return };
    let Some(mon) = super::get_mon_mut_by_idx(state, idx) else { return };

    // The "shown/copied species" branch's own item bound, as it stood before any
    // widening below (this fresh switch-in entry's own, otherwise-unconstrained item).
    let copied_bound = mon.item.clone();

    // Marginal widening: sound regardless of whether any clause below can be
    // materialized — the true item is possible under whichever branch is real, so
    // the union always still contains it.
    let mut merged = copied_bound.clone();
    for (_, item_bound) in illusion_candidates {
        merged = super::unknown_union(&merged, item_bound);
    }
    mon.item = merged;

    // Per-candidate clause: (¬HasSpecies(candidate) ∨ HasItem(c1) ∨ HasItem(c2) ∨ …).
    for (species, item_bound) in illusion_candidates {
        if let Some(candidates) = super::unknown_bounded_candidates(item_bound) {
            let mut clause = vec![Statement::Not(Box::new(Statement::HasSpecies {
                mon_idx: idx,
                species: species.clone(),
            }))];
            clause.extend(candidates.into_iter().map(|item| Statement::HasItem { mon_idx: idx, item }));
            state.predicates.push(clause);
        }
    }
    // Shown/copied branch: (HasSpecies(c1) ∨ HasSpecies(c2) ∨ … ∨ HasItem(shown_item1) ∨ …)
    // — "if species is none of the Illusion candidates, item is one of the copied
    // mon's own candidates." Valid CNF form of "¬(¬c1 ∧ ¬c2 ∧ …) ⇒ Q" = "c1∨c2∨…∨Q".
    if let Some(candidates) = super::unknown_bounded_candidates(&copied_bound) {
        let mut clause: Vec<Statement> = illusion_candidates
            .iter()
            .map(|(species, _)| Statement::HasSpecies { mon_idx: idx, species: species.clone() })
            .collect();
        clause.extend(candidates.into_iter().map(|item| Statement::HasItem { mon_idx: idx, item }));
        state.predicates.push(clause);
    }
}

// ── Private helpers ─────────────────────────────────────────────────────────────

fn eval_false(state: &UnknownBattleState, lit: &Statement) -> bool {
    match lit {
        Statement::Not(inner) => eval_true(state, inner),
        Statement::HasItem { mon_idx, item } => {
            super::get_mon_by_idx(state, *mon_idx)
                .map_or(false, |m| super::unknown_is_excluded(&m.item, item))
        }
        Statement::HasAbility { mon_idx, ability } => super::get_mon_by_idx(state, *mon_idx)
            .map_or(false, |m| super::unknown_is_excluded(&m.possible_abilities, ability)),
        Statement::HasSpecies { mon_idx, species } => super::get_mon_by_idx(state, *mon_idx)
            .map_or(false, |m| super::unknown_is_excluded(&m.possible_species, species)),
        Statement::NatureBoostsStat { mon_idx, stat } => {
            super::get_mon_by_idx(state, *mon_idx).map_or(false, |m| {
                boosting_natures_for_stat(stat)
                    .iter()
                    .all(|n| super::unknown_is_excluded(&m.possible_natures, n))
            })
        }
        Statement::NatureNerfsStat { mon_idx, stat } => {
            super::get_mon_by_idx(state, *mon_idx).map_or(false, |m| {
                nerfing_natures_for_stat(stat)
                    .iter()
                    .all(|n| super::unknown_is_excluded(&m.possible_natures, n))
            })
        }
        Statement::EVIVStatGE { mon_idx, stat, value } => super::get_mon_by_idx(state, *mon_idx)
            .map_or(false, |m| m.max_pre_nature_stat[stat_to_stats_idx(stat)] < *value),
        Statement::EVIVStatLE { mon_idx, stat, value } => super::get_mon_by_idx(state, *mon_idx)
            .map_or(false, |m| m.min_pre_nature_stat[stat_to_stats_idx(stat)] > *value),
        Statement::SpeedComparison { fast_idx, slow_idx, fast_mult, slow_mult } => {
            let fast_max =
                super::get_mon_by_idx(state, *fast_idx).map_or(999u64, |m| m.maxStats[5] as u64);
            let slow_min =
                super::get_mon_by_idx(state, *slow_idx).map_or(0u64, |m| m.minStats[5] as u64);
            fast_max * (*fast_mult as u64) < slow_min * (*slow_mult as u64)
        }
        // Turn-count literals are false-confirmed only while the effect is live and the timer
        // excludes the value; an absent effect is inert, never false-confirmed, so a stale
        // clause surviving a purge can't unit-force its partner literal.
        Statement::WeatherTurns { turns } => state
            .weather_turns
            .as_ref()
            .map_or(false, |wt| super::unknown_is_excluded(wt, &(*turns as u8))),
        Statement::TerrainTurns { turns } => state
            .terrain_turns
            .as_ref()
            .map_or(false, |tt| super::unknown_is_excluded(tt, &(*turns as u8))),
        Statement::SideConditionTurns { side, side_condition, turns } => {
            let (conditions, turns_vec) = match side {
                Player::P1 => (&state.p1_side_conditions, &state.p1_side_condition_turns),
                Player::P2 => (&state.p2_side_conditions, &state.p2_side_condition_turns),
            };
            conditions
                .iter()
                .position(|c| c == side_condition)
                .map_or(false, |i| {
                    turns_vec
                        .get(i)
                        .map_or(false, |ct| super::unknown_is_excluded(ct, &(*turns as u8)))
                })
        }
        // KnowsThreateningMove is a persistent relational constraint — never pruned
        // conservatively in BCP without move_dex access.
        Statement::KnowsThreateningMove { .. } => false,
    }
}

fn eval_true(state: &UnknownBattleState, lit: &Statement) -> bool {
    match lit {
        Statement::Not(inner) => eval_false(state, inner),
        Statement::HasItem { mon_idx, item } => super::get_mon_by_idx(state, *mon_idx)
            .map_or(false, |m| super::unknown_is_known_as(&m.item, item)),
        Statement::HasAbility { mon_idx, ability } => super::get_mon_by_idx(state, *mon_idx)
            .map_or(false, |m| super::unknown_is_known_as(&m.possible_abilities, ability)),
        Statement::HasSpecies { mon_idx, species } => super::get_mon_by_idx(state, *mon_idx)
            .map_or(false, |m| super::unknown_is_known_as(&m.possible_species, species)),
        Statement::EVIVStatGE { mon_idx, stat, value } => super::get_mon_by_idx(state, *mon_idx)
            .map_or(false, |m| m.min_pre_nature_stat[stat_to_stats_idx(stat)] >= *value),
        Statement::EVIVStatLE { mon_idx, stat, value } => super::get_mon_by_idx(state, *mon_idx)
            .map_or(false, |m| m.max_pre_nature_stat[stat_to_stats_idx(stat)] <= *value),
        Statement::SpeedComparison { fast_idx, slow_idx, fast_mult, slow_mult } => {
            let fast_min =
                super::get_mon_by_idx(state, *fast_idx).map_or(0u64, |m| m.minStats[5] as u64);
            let slow_max =
                super::get_mon_by_idx(state, *slow_idx).map_or(999u64, |m| m.maxStats[5] as u64);
            fast_min * (*fast_mult as u64) >= slow_max * (*slow_mult as u64)
        }
        Statement::NatureBoostsStat { mon_idx, stat } => {
            super::get_mon_by_idx(state, *mon_idx).map_or(false, |m| {
                let boosters = boosting_natures_for_stat(stat);
                match &m.possible_natures {
                    Unknown::Known(n) => boosters.contains(n),
                    Unknown::Possibly(v) => !v.is_empty() && v.iter().all(|n| boosters.contains(n)),
                    Unknown::Not(_) => false,
                }
            })
        }
        Statement::NatureNerfsStat { mon_idx, stat } => {
            super::get_mon_by_idx(state, *mon_idx).map_or(false, |m| {
                let nerfers = nerfing_natures_for_stat(stat);
                match &m.possible_natures {
                    Unknown::Known(n) => nerfers.contains(n),
                    Unknown::Possibly(v) => !v.is_empty() && v.iter().all(|n| nerfers.contains(n)),
                    Unknown::Not(_) => false,
                }
            })
        }
        Statement::WeatherTurns { turns } => state
            .weather_turns
            .as_ref()
            .map_or(false, |wt| matches!(wt, Unknown::Known(v) if *v == *turns as u8)),
        Statement::TerrainTurns { turns } => state
            .terrain_turns
            .as_ref()
            .map_or(false, |tt| matches!(tt, Unknown::Known(v) if *v == *turns as u8)),
        Statement::SideConditionTurns { side, side_condition, turns } => {
            let (conditions, turns_vec) = match side {
                Player::P1 => (&state.p1_side_conditions, &state.p1_side_condition_turns),
                Player::P2 => (&state.p2_side_conditions, &state.p2_side_condition_turns),
            };
            conditions
                .iter()
                .position(|c| c == side_condition)
                .and_then(|i| turns_vec.get(i))
                .map_or(false, |ct| matches!(ct, Unknown::Known(v) if *v == *turns as u8))
        }
        // KnowsThreateningMove is satisfied once a known OHKO move is on the mon.
        Statement::KnowsThreateningMove { mon_idx, .. } => {
            const OHKO_MOVES: &[PokemonMove] = &[
                PokemonMove::Fissure,
                PokemonMove::Guillotine,
                PokemonMove::HornDrill,
                PokemonMove::SheerCold,
            ];
            super::get_mon_by_idx(state, *mon_idx).map_or(false, |m| {
                m.known_moves
                    .iter()
                    .any(|mv| mv.as_ref().map_or(false, |m| OHKO_MOVES.contains(m)))
            })
        }
    }
}

fn force_literal(
    state: &mut UnknownBattleState,
    lit: &Statement,
    allow_repeat_items: bool,
    dex: &HashMap<Species, PokemonData>,
    config: &super::InferenceConfig,
) {
    match lit {
        Statement::HasItem { mon_idx, item } => {
            if let Some(mon) = super::get_mon_mut_by_idx(state, *mon_idx) {
                super::unknown_set_known(&mut mon.item, item.clone(), &format!("bcp#{mon_idx}"));
            }
            // BCP-committed item cannot be held by any other roster member on the same side.
            super::enforce_unique_item(state, *mon_idx, item, allow_repeat_items);
        }
        Statement::HasAbility { mon_idx, ability } => {
            if let Some(mon) = super::get_mon_mut_by_idx(state, *mon_idx) {
                super::unknown_set_known(
                    &mut mon.possible_abilities,
                    ability.clone(),
                    &format!("bcp#{mon_idx}"),
                );
            }
        }
        Statement::HasSpecies { mon_idx, species } => {
            if let Some(mon) = super::get_mon_mut_by_idx(state, *mon_idx) {
                super::unknown_set_known(&mut mon.possible_species, species.clone(), &format!("bcp#{mon_idx}"));
                // S30: `HasSpecies` literals only ever come from `widen_item_for_illusion`,
                // so forcing one here means BCP just resolved an Illusion disguise
                // ambiguity mid-fixpoint. Every stat bound narrowed so far (minStats/
                // maxStats, min/max_pre_nature_stat, minIvs/maxIvs, minEvs/maxEvs) was
                // computed against whichever species was on display at the time — not
                // necessarily the one just confirmed — the same desync `IllusionEnded`'s
                // handler resets for. Left in place, `pass5_back_solve` can see an HP (or
                // other stat) window no IV/EV of the confirmed species can reach: "no
                // IV/EV can produce observed HP bounds". Mirror `IllusionEnded`: fully
                // reset the stat window against the confirmed species and widen EVs back
                // to the full lattice range, rather than remapping the stale bounds.
                super::recompute_stats_for_iv_mode(mon, species, dex, config);
                mon.minEvs = [0; 6];
                mon.maxEvs = [252; 6];
            }
        }
        Statement::EVIVStatGE { mon_idx, stat, value } => {
            if let Some(mon) = super::get_mon_mut_by_idx(state, *mon_idx) {
                let si = stat_to_stats_idx(stat);
                if mon.min_pre_nature_stat[si] < *value {
                    mon.min_pre_nature_stat[si] = *value;
                }
            }
        }
        Statement::EVIVStatLE { mon_idx, stat, value } => {
            if let Some(mon) = super::get_mon_mut_by_idx(state, *mon_idx) {
                let si = stat_to_stats_idx(stat);
                if mon.max_pre_nature_stat[si] > *value {
                    mon.max_pre_nature_stat[si] = *value;
                }
            }
        }
        Statement::NatureBoostsStat { mon_idx, stat } => {
            if let Some(mon) = super::get_mon_mut_by_idx(state, *mon_idx) {
                let valid = boosting_natures_for_stat(stat);
                filter_natures_to_set(&mut mon.possible_natures, &valid, "bcp-nature-boosts");
            }
        }
        Statement::NatureNerfsStat { mon_idx, stat } => {
            if let Some(mon) = super::get_mon_mut_by_idx(state, *mon_idx) {
                let valid = nerfing_natures_for_stat(stat);
                filter_natures_to_set(&mut mon.possible_natures, &valid, "bcp-nature-nerfs");
            }
        }
        Statement::WeatherTurns { turns } => {
            let t = *turns as u8;
            if let Some(wt) = &mut state.weather_turns {
                super::unknown_set_known(wt, t, "bcp-weather-turns");
            } else {
                inference_contradiction!(
                    "bcp-weather-turns",
                    "WeatherTurns forced to {} but no weather is active",
                    turns
                );
            }
        }
        Statement::TerrainTurns { turns } => {
            let t = *turns as u8;
            if let Some(tt) = &mut state.terrain_turns {
                super::unknown_set_known(tt, t, "bcp-terrain-turns");
            } else {
                inference_contradiction!(
                    "bcp-terrain-turns",
                    "TerrainTurns forced to {} but no terrain is active",
                    turns
                );
            }
        }
        Statement::SideConditionTurns { side, side_condition, turns } => {
            let t = *turns as u8;
            let idx = match side {
                Player::P1 => state.p1_side_conditions.iter().position(|c| c == side_condition),
                Player::P2 => state.p2_side_conditions.iter().position(|c| c == side_condition),
            };
            if let Some(i) = idx {
                let turns_vec = match side {
                    Player::P1 => &mut state.p1_side_condition_turns,
                    Player::P2 => &mut state.p2_side_condition_turns,
                };
                if let Some(ct) = turns_vec.get_mut(i) {
                    super::unknown_set_known(ct, t, "bcp-side-condition-turns");
                }
            }
        }
        // Persistent relational constraints — never unit-forced into a field.
        Statement::Not(_)
        | Statement::SpeedComparison { .. }
        | Statement::KnowsThreateningMove { .. } => {}
    }
}

/// Retain in `natures` only those that appear in `valid`.
/// Converts `Not(excluded)` to an explicit `Possibly` before filtering.
fn filter_natures_to_set(natures: &mut Unknown<Nature>, valid: &[Nature], ctx: &str) {
    match natures {
        Unknown::Known(n) => {
            if !valid.contains(n) {
                inference_contradiction!(
                    ctx,
                    "Nature {:?} does not satisfy constraint (valid: {:?})",
                    n,
                    valid
                );
            }
        }
        Unknown::Not(excluded) => {
            let mut candidates: Vec<Nature> = super::ALL_NATURES
                .iter()
                .filter(|n| valid.contains(n) && !excluded.contains(n))
                .cloned()
                .collect();
            if candidates.is_empty() {
                inference_contradiction!(ctx, "No valid natures remain after constraint");
            }
            if candidates.len() == 1 {
                *natures = Unknown::Known(candidates.remove(0));
            } else {
                *natures = Unknown::Possibly(candidates);
            }
        }
        Unknown::Possibly(v) => {
            v.retain(|n| valid.contains(n));
            if v.is_empty() {
                inference_contradiction!(ctx, "No valid natures remain after constraint");
            }
            if v.len() == 1 {
                let n = v[0].clone();
                *natures = Unknown::Known(n);
            }
        }
    }
}

fn div_ceil(a: u64, b: u64) -> u64 {
    if b == 0 {
        return a;
    }
    (a + b - 1) / b
}

// ── Nature helpers ────────────────────────────────────────────────────────────

pub(super) fn boosting_natures_for_stat(stat: &PokemonStat) -> Vec<Nature> {
    match stat {
        PokemonStat::Atk => vec![Nature::Lonely, Nature::Adamant, Nature::Naughty, Nature::Brave],
        PokemonStat::Def => vec![Nature::Bold, Nature::Impish, Nature::Lax, Nature::Relaxed],
        PokemonStat::SpA => vec![Nature::Modest, Nature::Mild, Nature::Rash, Nature::Quiet],
        PokemonStat::SpD => vec![Nature::Calm, Nature::Gentle, Nature::Careful, Nature::Sassy],
        PokemonStat::Spe => vec![Nature::Timid, Nature::Hasty, Nature::Jolly, Nature::Naive],
    }
}

pub(super) fn nerfing_natures_for_stat(stat: &PokemonStat) -> Vec<Nature> {
    match stat {
        PokemonStat::Atk => vec![Nature::Bold, Nature::Modest, Nature::Calm, Nature::Timid],
        PokemonStat::Def => vec![Nature::Lonely, Nature::Mild, Nature::Gentle, Nature::Hasty],
        PokemonStat::SpA => vec![Nature::Adamant, Nature::Impish, Nature::Careful, Nature::Jolly],
        PokemonStat::SpD => vec![Nature::Naughty, Nature::Lax, Nature::Rash, Nature::Naive],
        PokemonStat::Spe => vec![Nature::Brave, Nature::Relaxed, Nature::Quiet, Nature::Sassy],
    }
}

pub(super) fn stat_to_stats_idx(stat: &PokemonStat) -> usize {
    match stat {
        PokemonStat::Atk => 1,
        PokemonStat::Def => 2,
        PokemonStat::SpA => 3,
        PokemonStat::SpD => 4,
        PokemonStat::Spe => 5,
    }
}
