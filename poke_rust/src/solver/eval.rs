//! Scores a nonterminal position at the search limit.
//!
//! The score is P1's win probability in the range `[0, 1]`.
//! This range supplies valid bounds for serialized alpha-beta and star1 pruning.
//!
//! [`SolveConfig`](super::SolveConfig) stores the evaluator as a function pointer.
//! Callers can replace it without a search change.
//!
//! The default evaluator uses a simple position heuristic.
//! Its named weights are initial values, not fitted values.
//! Record weight changes in `benches/RESULTS.md`.

use crate::state::battle::BattleState;
use crate::state::dex_data::{SideCondition, Status};
use crate::state::pokemon::PokemonState;

/// Scores a non-terminal position as P1's win probability, in `[0, 1]`.
///
/// Implementations must stay inside that range — the search's pruning bounds
/// assume it — and should be antisymmetric in the two sides, so that evaluating
/// a mirrored position returns one minus the original.
pub type LeafEvaluator = fn(&BattleState) -> f64;

/// Credit for merely being alive, as a fraction of a Pokemon's total worth; the
/// remainder scales with remaining HP. Purely-HP-proportional scoring badly
/// undervalues a Pokemon at 5% — which can still Protect, pivot, set up, or
/// trade a KO — while pure headcount cannot see chip damage at all.
const ALIVE_SHARE: f64 = 0.5;

/// A full stat stage, in Pokemon-equivalents. A +1 is worth noticeably less than
/// a tenth of a Pokemon, but boosts compound and are usually attached to a
/// threat, so this is set at the optimistic end.
const BOOST_WEIGHT: f64 = 0.06;

/// Accuracy and evasion stages, which matter less per stage than the offensive
/// and defensive ones and are far rarer.
const ACC_EVA_WEIGHT: f64 = 0.03;

/// One layer of an entry hazard on a side, in Pokemon-equivalents. Charged
/// against the side that has to switch into it.
const HAZARD_WEIGHT: f64 = 0.12;

/// Steepness of the logistic squash, in units of one Pokemon. At 0.8 a one-mon
/// lead reads as roughly 69% and a two-mon lead as 83% — deliberately short of
/// certainty, because a Pokemon lead is a real but routinely reversible edge.
const LOGISTIC_SCALE: f64 = 0.8;

/// The default positional heuristic.
///
/// Sums a per-side score in Pokemon-equivalents — remaining team, active-slot
/// boosts and status, entry hazards — takes the difference, and squashes it
/// through a logistic. Everything it measures is symmetric between the sides, so
/// an even position scores exactly 0.5.
pub fn heuristic(state: &BattleState) -> f64 {
    let p1 = side_score(&state.p1_active_mons, &state.p1_back_mons, &state.p1_side_conditions);
    let p2 = side_score(&state.p2_active_mons, &state.p2_back_mons, &state.p2_side_conditions);
    logistic(p1 - p2)
}

/// A constant 0.5 for every position.
///
/// Turns the search into a pure terminal-detector: any value it reports comes
/// from lines that actually end the game within the horizon. Used by tests that
/// need the search's structure isolated from the heuristic's judgement.
pub fn even(_state: &BattleState) -> f64 {
    0.5
}

/// One side's standing, in Pokemon-equivalents.
fn side_score(
    active: &[PokemonState],
    bench: &[PokemonState],
    side_conditions: &[SideCondition],
) -> f64 {
    let team: f64 = active.iter().chain(bench).map(mon_score).sum();
    let field: f64 = active.iter().map(active_slot_score).sum();
    team + field - hazard_penalty(side_conditions)
}

/// A Pokemon's contribution: nothing if fainted, otherwise a floor for being
/// alive plus a share scaling with remaining HP, less whatever its status costs.
///
/// Status is charged here rather than in [`active_slot_score`] because it
/// survives switching — a burned Pokemon on the bench is still burned when it
/// comes back in.
fn mon_score(mon: &PokemonState) -> f64 {
    if mon.fainted || mon.hp == 0 {
        return 0.0;
    }
    let max_hp = mon.stats[0];
    let hp_fraction = if max_hp == 0 {
        // Not reachable through normal team construction; scoring it as healthy
        // beats dividing by zero.
        1.0
    } else {
        (mon.hp as f64 / max_hp as f64).clamp(0.0, 1.0)
    };
    ALIVE_SHARE + (1.0 - ALIVE_SHARE) * hp_fraction - status_penalty(mon.status.as_ref())
}

/// What is true only while a Pokemon is on the field.
///
/// Just stat stages: unlike status, boosts are wiped on switch-out, so they are
/// worth nothing to a benched Pokemon. A fainted slot contributes nothing at
/// all — its boosts are about to vanish with it.
fn active_slot_score(mon: &PokemonState) -> f64 {
    if mon.fainted {
        return 0.0;
    }
    let offensive_defensive: i32 = mon.boosts[..5].iter().map(|&b| b as i32).sum();
    let accuracy_evasion: i32 = mon.boosts[5..].iter().map(|&b| b as i32).sum();

    offensive_defensive as f64 * BOOST_WEIGHT + accuracy_evasion as f64 * ACC_EVA_WEIGHT
}

/// Cost of a non-volatile status, in Pokemon-equivalents.
///
/// Ordered by how much of a Pokemon's usefulness it removes rather than by
/// damage dealt: sleep and freeze remove whole turns, paralysis removes speed
/// and a quarter of turns, burn halves physical output, poison is a clock.
fn status_penalty(status: Option<&Status>) -> f64 {
    match status {
        None => 0.0,
        Some(Status::Sleep(_)) => 0.35,
        Some(Status::Frozen(_)) => 0.35,
        Some(Status::Paralysis) => 0.22,
        Some(Status::Burn) => 0.18,
        // Toxic accelerates, so it is worth strictly more than regular poison,
        // and more so the longer it has been ticking.
        Some(Status::ToxicPoison(turns)) => 0.15 + 0.02 * f64::from(*turns).min(8.0),
        Some(Status::Poison) => 0.12,
    }
}

/// Entry hazards on a side, charged to that side. Layered hazards scale with
/// their layer count; Stealth Rock and Sticky Web are single-layer.
fn hazard_penalty(side_conditions: &[SideCondition]) -> f64 {
    side_conditions
        .iter()
        .map(|condition| match condition {
            SideCondition::Spikes(layers) => HAZARD_WEIGHT * f64::from(*layers),
            SideCondition::ToxicSpikes(layers) => HAZARD_WEIGHT * f64::from(*layers),
            SideCondition::StealthRock => HAZARD_WEIGHT * 1.5,
            SideCondition::StickyWeb(_) => HAZARD_WEIGHT,
            _ => 0.0,
        })
        .sum()
}

/// Map a signed advantage in Pokemon-equivalents onto `(0, 1)`.
fn logistic(advantage: f64) -> f64 {
    1.0 / (1.0 + (-LOGISTIC_SCALE * advantage).exp())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::dex_data::PokemonData;
    use crate::state::pokemon::build_pokemon_state;
    use crate::tests::simuilator_test_helpers::{battle_state_from_lists, move_dex, pokemon_dex};
    use crate::data::species::Species;
    use crate::state::dex_data::MoveData;
    use crate::data::pokemon_move::PokemonMove;
    use std::collections::HashMap;

    fn mon(
        species: Species,
        pokemon_dex: &HashMap<Species, PokemonData>,
        move_dex: &HashMap<PokemonMove, MoveData>,
    ) -> PokemonState {
        build_pokemon_state(
            species,
            pokemon_dex,
            move_dex,
            Some(50),
            Some([Some(PokemonMove::Tackle), None, None, None]),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            false,
        )
    }

    #[test]
    fn mirrored_position_is_even() {
        let pokemon_dex = pokemon_dex();
        let move_dex = move_dex();
        let state = battle_state_from_lists(
            vec![mon(Species::Pikachu, pokemon_dex, move_dex)],
            vec![mon(Species::Snorlax, pokemon_dex, move_dex)],
            vec![mon(Species::Pikachu, pokemon_dex, move_dex)],
            vec![mon(Species::Snorlax, pokemon_dex, move_dex)],
        );
        assert!((heuristic(&state) - 0.5).abs() < 1e-9);
    }

    #[test]
    fn losing_hp_lowers_the_score() {
        let pokemon_dex = pokemon_dex();
        let move_dex = move_dex();
        let mut state = battle_state_from_lists(
            vec![mon(Species::Pikachu, pokemon_dex, move_dex)],
            vec![],
            vec![mon(Species::Pikachu, pokemon_dex, move_dex)],
            vec![],
        );
        let even_score = heuristic(&state);
        state.p1_active_mons[0].hp /= 2;
        assert!(heuristic(&state) < even_score);
    }

    #[test]
    fn a_faint_costs_more_than_any_chip_damage() {
        let pokemon_dex = pokemon_dex();
        let move_dex = move_dex();
        let build = || {
            battle_state_from_lists(
                vec![mon(Species::Pikachu, pokemon_dex, move_dex)],
                vec![mon(Species::Snorlax, pokemon_dex, move_dex)],
                vec![mon(Species::Pikachu, pokemon_dex, move_dex)],
                vec![mon(Species::Snorlax, pokemon_dex, move_dex)],
            )
        };

        let mut chipped = build();
        chipped.p1_back_mons[0].hp = 1;

        let mut fainted = build();
        fainted.p1_back_mons[0].hp = 0;
        fainted.p1_back_mons[0].fainted = true;

        assert!(heuristic(&fainted) < heuristic(&chipped));
    }

    #[test]
    fn status_and_hazards_are_charged_to_the_afflicted_side() {
        let pokemon_dex = pokemon_dex();
        let move_dex = move_dex();
        let build = || {
            battle_state_from_lists(
                vec![mon(Species::Pikachu, pokemon_dex, move_dex)],
                vec![],
                vec![mon(Species::Pikachu, pokemon_dex, move_dex)],
                vec![],
            )
        };

        let mut burned = build();
        burned.p1_active_mons[0].status = Some(Status::Burn);
        assert!(heuristic(&burned) < 0.5);

        // Status survives switching, so it has to be charged on the bench too.
        let mut benched_burn = battle_state_from_lists(
            vec![mon(Species::Pikachu, pokemon_dex, move_dex)],
            vec![mon(Species::Snorlax, pokemon_dex, move_dex)],
            vec![mon(Species::Pikachu, pokemon_dex, move_dex)],
            vec![mon(Species::Snorlax, pokemon_dex, move_dex)],
        );
        benched_burn.p1_back_mons[0].status = Some(Status::Burn);
        assert!(heuristic(&benched_burn) < 0.5);

        let mut hazards = build();
        hazards.p1_side_conditions.push(SideCondition::StealthRock);
        assert!(heuristic(&hazards) < 0.5);

        // Same hazard on P2's side must swing the other way by the same amount.
        let mut theirs = build();
        theirs.p2_side_conditions.push(SideCondition::StealthRock);
        assert!((heuristic(&theirs) + heuristic(&hazards) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn output_stays_in_range_under_a_total_wipe() {
        let pokemon_dex = pokemon_dex();
        let move_dex = move_dex();
        let mut state = battle_state_from_lists(
            vec![mon(Species::Pikachu, pokemon_dex, move_dex)],
            vec![mon(Species::Snorlax, pokemon_dex, move_dex)],
            vec![mon(Species::Pikachu, pokemon_dex, move_dex)],
            vec![mon(Species::Snorlax, pokemon_dex, move_dex)],
        );
        for m in state
            .p2_active_mons
            .iter_mut()
            .chain(state.p2_back_mons.iter_mut())
        {
            m.hp = 0;
            m.fainted = true;
        }
        let score = heuristic(&state);
        assert!((0.0..=1.0).contains(&score), "out of range: {score}");
        assert!(score > 0.8, "a wipe should read as winning: {score}");
    }
}
