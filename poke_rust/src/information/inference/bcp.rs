//! Pass 6: Boolean Constraint Propagation (BCP) to fixpoint.
//!
//! Propagates disjunctive constraints (CNF predicates) by:
//! 1. Removing literals known to be false from each clause.
//! 2. Dropping clauses with a known-true literal.
//! 3. Forcing unit clauses into the state.
//! 4. Propagating `SpeedComparison` Spe bounds bidirectionally.

#![allow(unused)]

use std::collections::HashMap;

use crate::data::pokemon_move::PokemonMove;
use crate::data::species::Species;
use crate::information::unknowns::{Statement, Unknown, UnknownBattleState};
use crate::state::battle::Player;
use crate::state::dex_data::{PokemonData, PokemonStat, SideCondition};
use crate::state::pokemon::Nature;

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
                // Self-heal: every disjunct in this clause evaluated false, i.e. every
                // explanation this clause encoded was independently ruled out. Since
                // this session's soundness bugs have consistently traced back to one
                // upstream literal's `eval_false` being derived from an incompletely-
                // modeled absence-inference (see the self-heal notes on
                // `unknown_exclude` / `apply_unconditional_tightening_to_mon` /
                // `apply_with_illusion_mirroring`) rather than a genuine impossibility,
                // treat an unsatisfiable clause the same way: discard it rather than
                // hard-panicking the whole belief. A clause that's simply unusable
                // going forward is safe to drop — it just stops constraining anything.
                eprintln!(
                    "[bcp self-heal] discarding unsatisfiable clause (all literals false): \
                     {:?} — an upstream literal's absence-inference gap, not a belief \
                     contradiction",
                    state.predicates[i],
                );
                state.predicates.remove(i);
                changed = true;
                clauses_changed = true;
                continue;
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
pub(super) fn collect_speed_comparisons(
    state: &UnknownBattleState,
) -> Vec<(usize, usize, u32, u32)> {
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
        let slow_min =
            super::get_mon_by_idx(state, slow_idx).map_or(0u64, |m| m.min_stats[5] as u64);
        let new_fast_min = div_ceil(slow_min * slow_mult as u64, fast_mult as u64) as u16;
        // Read both bounds first (immutable borrow) so the panic-message legend below
        // never needs to coexist with a live mutable borrow of `state`.
        if let Some((fast_min, fast_max)) =
            super::get_mon_by_idx(state, fast_idx).map(|m| (m.min_stats[5], m.max_stats[5]))
            && new_fast_min > fast_min
        {
            if new_fast_min > fast_max {
                // Self-heal (same principle as the crossed-bound handling elsewhere in
                // this pass — see `apply_unconditional_tightening_to_mon` /
                // `unknown_exclude`): a SpeedComparison derived from THIS turn's move
                // order conflicts with an already-tracked Spe bound. Discard the
                // derived raise rather than corrupting the bound or crashing the whole
                // belief over one turn's ordering inference.
                eprintln!(
                    "[SpeedComparison self-heal] discarding evidence: would raise min({new_fast_min}) \
                     above max({fast_max}) for mon_idx {fast_idx} — oracle blind spot, not a \
                     belief contradiction"
                );
            } else {
                if let Some(mon) = super::get_mon_mut_by_idx(state, fast_idx) {
                    mon.min_stats[5] = new_fast_min;
                }
                changed = true;
            }
        }

        // Lower slow's max Spe: base_spe(slow) <= floor(base_spe(fast)*fast_mult / slow_mult)
        let fast_max_for_slow =
            super::get_mon_by_idx(state, fast_idx).map_or(u64::MAX / 2, |m| m.max_stats[5] as u64);
        let new_slow_max = (fast_max_for_slow.saturating_mul(fast_mult as u64) / slow_mult as u64)
            .min(u16::MAX as u64) as u16;
        if let Some((slow_min_bound, slow_max_bound)) =
            super::get_mon_by_idx(state, slow_idx).map(|m| (m.min_stats[5], m.max_stats[5]))
            && new_slow_max < slow_max_bound
        {
            if new_slow_max < slow_min_bound {
                // Self-heal — see the identical rationale on the fast-mon branch above.
                eprintln!(
                    "[SpeedComparison self-heal] discarding evidence: would lower max({new_slow_max}) \
                     below min({slow_min_bound}) for mon_idx {slow_idx} — oracle blind spot, not \
                     a belief contradiction"
                );
            } else {
                if let Some(mon) = super::get_mon_mut_by_idx(state, slow_idx) {
                    mon.max_stats[5] = new_slow_max;
                }
                changed = true;
            }
        }
    }
    changed
}

// ── Private helpers ─────────────────────────────────────────────────────────────

fn eval_false(state: &UnknownBattleState, lit: &Statement) -> bool {
    match lit {
        Statement::Not(inner) => eval_true(state, inner),
        Statement::HasItem { mon_idx, item } => super::get_mon_by_idx(state, *mon_idx)
            .is_some_and(|m| super::unknown_is_excluded(&m.item, item)),
        Statement::HasAbility { mon_idx, ability } => super::get_mon_by_idx(state, *mon_idx)
            .is_some_and(|m| super::unknown_is_excluded(&m.possible_abilities, ability)),
        Statement::NatureBoostsStat { mon_idx, stat } => super::get_mon_by_idx(state, *mon_idx)
            .is_some_and(|m| {
                boosting_natures_for_stat(stat)
                    .iter()
                    .all(|n| super::unknown_is_excluded(&m.possible_natures, n))
            }),
        Statement::NatureNerfsStat { mon_idx, stat } => super::get_mon_by_idx(state, *mon_idx)
            .is_some_and(|m| {
                nerfing_natures_for_stat(stat)
                    .iter()
                    .all(|n| super::unknown_is_excluded(&m.possible_natures, n))
            }),
        Statement::EVIVStatGE {
            mon_idx,
            stat,
            value,
        } => super::get_mon_by_idx(state, *mon_idx)
            .is_some_and(|m| m.max_pre_nature_stat[stat_to_stats_idx(stat)] < *value),
        Statement::EVIVStatLE {
            mon_idx,
            stat,
            value,
        } => super::get_mon_by_idx(state, *mon_idx)
            .is_some_and(|m| m.min_pre_nature_stat[stat_to_stats_idx(stat)] > *value),
        Statement::SpeedComparison {
            fast_idx,
            slow_idx,
            fast_mult,
            slow_mult,
        } => {
            let fast_max =
                super::get_mon_by_idx(state, *fast_idx).map_or(999u64, |m| m.max_stats[5] as u64);
            let slow_min =
                super::get_mon_by_idx(state, *slow_idx).map_or(0u64, |m| m.min_stats[5] as u64);
            fast_max * (*fast_mult as u64) < slow_min * (*slow_mult as u64)
        }
        // Turn-count literals are false-confirmed only while the effect is live and the timer
        // excludes the value; an absent effect is inert, never false-confirmed, so a stale
        // clause surviving a purge can't unit-force its partner literal.
        Statement::WeatherTurns { turns } => state
            .weather_turns
            .as_ref()
            .is_some_and(|wt| super::unknown_is_excluded(wt, &(*turns as u8))),
        Statement::TerrainTurns { turns } => state
            .terrain_turns
            .as_ref()
            .is_some_and(|tt| super::unknown_is_excluded(tt, &(*turns as u8))),
        Statement::SideConditionTurns {
            side,
            side_condition,
            turns,
        } => {
            let (conditions, turns_vec) = match side {
                Player::P1 => (&state.p1_side_conditions, &state.p1_side_condition_turns),
                Player::P2 => (&state.p2_side_conditions, &state.p2_side_condition_turns),
            };
            conditions
                .iter()
                .position(|c| c == side_condition)
                .is_some_and(|i| {
                    turns_vec
                        .get(i)
                        .is_some_and(|ct| super::unknown_is_excluded(ct, &(*turns as u8)))
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
            .is_some_and(|m| super::unknown_is_known_as(&m.item, item)),
        Statement::HasAbility { mon_idx, ability } => super::get_mon_by_idx(state, *mon_idx)
            .is_some_and(|m| super::unknown_is_known_as(&m.possible_abilities, ability)),
        Statement::EVIVStatGE {
            mon_idx,
            stat,
            value,
        } => super::get_mon_by_idx(state, *mon_idx)
            .is_some_and(|m| m.min_pre_nature_stat[stat_to_stats_idx(stat)] >= *value),
        Statement::EVIVStatLE {
            mon_idx,
            stat,
            value,
        } => super::get_mon_by_idx(state, *mon_idx)
            .is_some_and(|m| m.max_pre_nature_stat[stat_to_stats_idx(stat)] <= *value),
        Statement::SpeedComparison {
            fast_idx,
            slow_idx,
            fast_mult,
            slow_mult,
        } => {
            let fast_min =
                super::get_mon_by_idx(state, *fast_idx).map_or(0u64, |m| m.min_stats[5] as u64);
            let slow_max =
                super::get_mon_by_idx(state, *slow_idx).map_or(999u64, |m| m.max_stats[5] as u64);
            fast_min * (*fast_mult as u64) >= slow_max * (*slow_mult as u64)
        }
        Statement::NatureBoostsStat { mon_idx, stat } => super::get_mon_by_idx(state, *mon_idx)
            .is_some_and(|m| {
                let boosters = boosting_natures_for_stat(stat);
                match &m.possible_natures {
                    Unknown::Known(n) => boosters.contains(n),
                    Unknown::Possibly(v) => !v.is_empty() && v.iter().all(|n| boosters.contains(n)),
                    Unknown::Not(_) => false,
                }
            }),
        Statement::NatureNerfsStat { mon_idx, stat } => super::get_mon_by_idx(state, *mon_idx)
            .is_some_and(|m| {
                let nerfers = nerfing_natures_for_stat(stat);
                match &m.possible_natures {
                    Unknown::Known(n) => nerfers.contains(n),
                    Unknown::Possibly(v) => !v.is_empty() && v.iter().all(|n| nerfers.contains(n)),
                    Unknown::Not(_) => false,
                }
            }),
        Statement::WeatherTurns { turns } => state
            .weather_turns
            .as_ref()
            .is_some_and(|wt| matches!(wt, Unknown::Known(v) if *v == *turns as u8)),
        Statement::TerrainTurns { turns } => state
            .terrain_turns
            .as_ref()
            .is_some_and(|tt| matches!(tt, Unknown::Known(v) if *v == *turns as u8)),
        Statement::SideConditionTurns {
            side,
            side_condition,
            turns,
        } => {
            let (conditions, turns_vec) = match side {
                Player::P1 => (&state.p1_side_conditions, &state.p1_side_condition_turns),
                Player::P2 => (&state.p2_side_conditions, &state.p2_side_condition_turns),
            };
            conditions
                .iter()
                .position(|c| c == side_condition)
                .and_then(|i| turns_vec.get(i))
                .is_some_and(|ct| matches!(ct, Unknown::Known(v) if *v == *turns as u8))
        }
        // KnowsThreateningMove is satisfied once a known OHKO move is on the mon.
        Statement::KnowsThreateningMove { mon_idx, .. } => {
            const OHKO_MOVES: &[PokemonMove] = &[
                PokemonMove::Fissure,
                PokemonMove::Guillotine,
                PokemonMove::HornDrill,
                PokemonMove::SheerCold,
            ];
            super::get_mon_by_idx(state, *mon_idx).is_some_and(|m| {
                m.known_moves
                    .iter()
                    .any(|mv| mv.as_ref().is_some_and(|m| OHKO_MOVES.contains(m)))
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
        Statement::EVIVStatGE {
            mon_idx,
            stat,
            value,
        } => {
            // Same self-heal as the Pass 3 direct-tightening path
            // (`apply_unconditional_tightening_to_mon`'s `CrossedBound` — see its and
            // its callers' doc comments for the rationale): check-then-write, so a
            // CNF-forced bound that would cross an already-trusted bound is simply
            // discarded (evidence from an oracle blind spot) rather than corrupting
            // the mon's fields or hard-panicking the whole belief.
            if let Some(mon) = super::get_mon_mut_by_idx(state, *mon_idx) {
                let si = stat_to_stats_idx(stat);
                let new_min = (*value).max(mon.min_pre_nature_stat[si]);
                if new_min > mon.max_pre_nature_stat[si] {
                    eprintln!(
                        "[BCP EVIVStatGE self-heal] discarding evidence: would cross the \
                         pre-nature BSV window for stat {si} on mon_idx {mon_idx} \
                         (min={new_min}, max={}) — oracle blind spot, not a belief contradiction",
                        mon.max_pre_nature_stat[si],
                    );
                } else {
                    mon.min_pre_nature_stat[si] = new_min;
                }
            }
        }
        Statement::EVIVStatLE {
            mon_idx,
            stat,
            value,
        } => {
            if let Some(mon) = super::get_mon_mut_by_idx(state, *mon_idx) {
                let si = stat_to_stats_idx(stat);
                let new_max = (*value).min(mon.max_pre_nature_stat[si]);
                if mon.min_pre_nature_stat[si] > new_max {
                    eprintln!(
                        "[BCP EVIVStatLE self-heal] discarding evidence: would cross the \
                         pre-nature BSV window for stat {si} on mon_idx {mon_idx} \
                         (min={}, max={new_max}) — oracle blind spot, not a belief contradiction",
                        mon.min_pre_nature_stat[si],
                    );
                } else {
                    mon.max_pre_nature_stat[si] = new_max;
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
        Statement::SideConditionTurns {
            side,
            side_condition,
            turns,
        } => {
            let t = *turns as u8;
            let idx = match side {
                Player::P1 => state
                    .p1_side_conditions
                    .iter()
                    .position(|c| c == side_condition),
                Player::P2 => state
                    .p2_side_conditions
                    .iter()
                    .position(|c| c == side_condition),
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
                let n = v[0];
                *natures = Unknown::Known(n);
            }
        }
    }
}

fn div_ceil(a: u64, b: u64) -> u64 {
    if b == 0 {
        return a;
    }
    a.div_ceil(b)
}

// ── Nature helpers ────────────────────────────────────────────────────────────

pub(super) fn boosting_natures_for_stat(stat: &PokemonStat) -> Vec<Nature> {
    match stat {
        PokemonStat::Atk => vec![
            Nature::Lonely,
            Nature::Adamant,
            Nature::Naughty,
            Nature::Brave,
        ],
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
        PokemonStat::SpA => vec![
            Nature::Adamant,
            Nature::Impish,
            Nature::Careful,
            Nature::Jolly,
        ],
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
