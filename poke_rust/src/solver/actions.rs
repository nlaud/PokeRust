//! Returns the legal joint commands for each battle phase.
//!
//! A joint command contains one command for each active slot.
//! In doubles, both slot commands can restrict each other.
//! Therefore, matrix rows contain joint commands.
//!
//! The HTTP server and solver use this module.
//! This shared code prevents different legality rules.
//!
//! Normal phases use `validate_battle_command_combination`.
//! Replacement phases use `replacement_commands_are_valid`.
//! The replacement validator uses each available bench Pokémon before it permits an empty slot.

use std::collections::HashMap;

use crate::data::pokemon_move::PokemonMove;
use crate::data::species::Species;
use crate::simulator::{get_possible_commands_for_active_slot, validate_battle_command_combination};
use crate::state::battle::{
    BattleCommand, BattleState, FieldSlot, MatchState, Player, SwitchCommand,
};
use crate::state::dex_data::{MoveData, PokemonData};
use crate::state::pokemon::PokemonState;
use crate::user::replacement_commands_are_valid;

/// Identifies the current input phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Phase {
    /// Leads have not been chosen yet.
    TeamPreview,
    /// The ordinary case: every healthy slot picks a move or a switch.
    Normal,
    /// A self-switch waits for one slot choice.
    /// Other slots must pass.
    SelfSwitch,
    /// The turn is over and fainted slots need replacements.
    Replacement,
    /// Terminal.
    GameOver,
}

/// A player's legal joint actions, plus how many there were before any cap.
#[derive(Debug, Clone)]
pub struct JointActions {
    /// One entry per legal joint action; each is one `BattleCommand` per active
    /// slot, in slot order.
    pub actions: Vec<Vec<BattleCommand>>,
    /// How many legal joint actions existed before `cap` was applied. Equal to
    /// `actions.len()` when nothing was dropped.
    pub total: usize,
}

impl JointActions {
    /// Returns true when a cap removed actions.
    pub fn was_capped(&self) -> bool {
        self.actions.len() < self.total
    }
}

/// Classify `state` into an input phase.
pub fn phase_of(state: &MatchState) -> Phase {
    match state {
        MatchState::TeamPreviewState(_) => Phase::TeamPreview,
        MatchState::GameOverState { .. } => Phase::GameOver,
        MatchState::BattleState(battle) => {
            if battle.self_switch_pending.is_some() {
                Phase::SelfSwitch
            } else if battle.turn_started && battle.turn_ended {
                Phase::Replacement
            } else {
                Phase::Normal
            }
        }
    }
}

fn active_mons(state: &BattleState, player: Player) -> &[PokemonState] {
    match player {
        Player::P1 => &state.p1_active_mons,
        Player::P2 => &state.p2_active_mons,
    }
}

fn back_mons(state: &BattleState, player: Player) -> &[PokemonState] {
    match player {
        Player::P1 => &state.p1_back_mons,
        Player::P2 => &state.p2_back_mons,
    }
}

/// Switch commands naming each healthy bench Pokemon.
pub fn healthy_bench_switches(state: &BattleState, player: Player) -> Vec<BattleCommand> {
    back_mons(state, player)
        .iter()
        .enumerate()
        .filter(|(_, m)| !m.fainted)
        .map(|(i, _)| BattleCommand::Switch(SwitchCommand { party_index: i }))
        .collect()
}

/// Returns independent legal commands for each active slot.
/// A slot without a choice returns one Pass.
/// This function does not apply cross-slot constraints.
pub fn per_slot_commands(
    state: &BattleState,
    player: Player,
    phase: Phase,
    move_dex: &HashMap<PokemonMove, MoveData>,
    pokemon_dex: &HashMap<Species, PokemonData>,
) -> Vec<Vec<BattleCommand>> {
    let actives = active_mons(state, player);

    (0..actives.len())
        .map(|slot_idx| match phase {
            Phase::TeamPreview | Phase::GameOver => vec![BattleCommand::Pass],

            Phase::SelfSwitch => {
                let pending = state.self_switch_pending.map(|(slot, _)| slot);
                let this_slot = FieldSlot {
                    player,
                    slot_index: slot_idx as u8,
                };
                if pending == Some(this_slot) {
                    let switches = healthy_bench_switches(state, player);
                    if switches.is_empty() {
                        vec![BattleCommand::Pass]
                    } else {
                        switches
                    }
                } else {
                    vec![BattleCommand::Pass]
                }
            }

            Phase::Replacement => {
                if actives[slot_idx].fainted {
                    let switches = healthy_bench_switches(state, player);
                    // Earlier fainted slots claim bench Pokemon first: with two
                    // fainted actives and one healthy bench Pokemon, slot 0 gets
                    // the switch and this slot is a forced Pass.
                    let earlier_fainted = actives[..slot_idx].iter().filter(|m| m.fainted).count();
                    if switches.len() <= earlier_fainted {
                        vec![BattleCommand::Pass]
                    } else {
                        switches
                    }
                } else {
                    vec![BattleCommand::Pass]
                }
            }

            Phase::Normal => get_possible_commands_for_active_slot(
                state, player, slot_idx, move_dex, pokemon_dex,
            ),
        })
        .collect()
}

/// Returns every legal joint action for one player.
/// Applies cross-slot validation after the Cartesian product.
/// `cap` can reduce the result and make the solution approximate.
pub fn joint_actions(
    state: &BattleState,
    player: Player,
    phase: Phase,
    move_dex: &HashMap<PokemonMove, MoveData>,
    pokemon_dex: &HashMap<Species, PokemonData>,
    cap: Option<usize>,
) -> JointActions {
    if matches!(phase, Phase::TeamPreview | Phase::GameOver) {
        return JointActions {
            actions: Vec::new(),
            total: 0,
        };
    }

    let per_slot = per_slot_commands(state, player, phase, move_dex, pokemon_dex);
    let actives = active_mons(state, player);

    let mut actions: Vec<Vec<BattleCommand>> = cartesian_product(&per_slot)
        .into_iter()
        .filter(|combo| match phase {
            Phase::Replacement => replacement_commands_are_valid(state, player, actives, combo),
            _ => validate_battle_command_combination(combo),
        })
        .collect();

    // Use an all-Pass fallback when validation removes every action.
    if actions.is_empty() {
        actions.push(vec![BattleCommand::Pass; actives.len()]);
    }

    let total = actions.len();
    if let Some(cap) = cap {
        actions = reduce_to_cap(actions, cap);
    }
    JointActions { actions, total }
}

/// Reduces a joint-action set to `cap` entries.
/// First, removes Tera and Mega variants.
/// Then keeps actions at a regular interval.
/// This process keeps a mix of switches and attacks.
fn reduce_to_cap(actions: Vec<Vec<BattleCommand>>, cap: usize) -> Vec<Vec<BattleCommand>> {
    let cap = cap.max(1);
    if actions.len() <= cap {
        return actions;
    }

    let plain: Vec<Vec<BattleCommand>> = actions
        .iter()
        .filter(|combo| !combo.iter().any(uses_tera_or_mega))
        .cloned()
        .collect();

    // Dropping every variant is only useful if something survives; a position
    // where Terastallizing is mandatory would otherwise reduce to nothing.
    let reduced = if plain.is_empty() { actions } else { plain };
    if reduced.len() <= cap {
        return reduced;
    }

    let stride = reduced.len().div_ceil(cap);
    reduced.into_iter().step_by(stride).take(cap).collect()
}

fn uses_tera_or_mega(command: &BattleCommand) -> bool {
    matches!(command, BattleCommand::Attack(a) if a.terastallize || a.mega_evolve)
}

/// Every combination taking one element from each list, in row-major order.
fn cartesian_product(per_slot: &[Vec<BattleCommand>]) -> Vec<Vec<BattleCommand>> {
    let mut combos: Vec<Vec<BattleCommand>> = vec![Vec::with_capacity(per_slot.len())];
    for options in per_slot {
        combos = combos
            .into_iter()
            .flat_map(|prefix| {
                options.iter().map(move |option| {
                    let mut extended = prefix.clone();
                    extended.push(option.clone());
                    extended
                })
            })
            .collect();
    }
    combos
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::species::Species;
    use crate::state::pokemon::build_pokemon_state;
    use crate::tests::simuilator_test_helpers::{battle_state_from_lists, move_dex, pokemon_dex};

    fn mon(species: Species, moves: [Option<PokemonMove>; 4]) -> PokemonState {
        build_pokemon_state(
            species,
            pokemon_dex(),
            move_dex(),
            Some(50),
            Some(moves),
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

    fn four_moves() -> [Option<PokemonMove>; 4] {
        [
            Some(PokemonMove::Tackle),
            Some(PokemonMove::Growl),
            Some(PokemonMove::Protect),
            Some(PokemonMove::Splash),
        ]
    }

    #[test]
    fn singles_joint_actions_are_just_the_slot_options() {
        let state = battle_state_from_lists(
            vec![mon(Species::Pikachu, four_moves())],
            vec![mon(Species::Snorlax, four_moves())],
            vec![mon(Species::Pikachu, four_moves())],
            vec![],
        );
        let joint = joint_actions(
            &state,
            Player::P1,
            Phase::Normal,
            move_dex(),
            pokemon_dex(),
            None,
        );
        assert!(joint.actions.iter().all(|combo| combo.len() == 1));
        // Four moves plus one bench switch, at minimum.
        assert!(joint.actions.len() >= 5, "got {}", joint.actions.len());
        assert_eq!(joint.total, joint.actions.len());
        assert!(!joint.was_capped());
    }

    #[test]
    fn doubles_joint_actions_pair_the_slots_and_reject_duplicate_switches() {
        let state = battle_state_from_lists(
            vec![
                mon(Species::Pikachu, four_moves()),
                mon(Species::Snorlax, four_moves()),
            ],
            vec![mon(Species::Gengar, four_moves())],
            vec![
                mon(Species::Pikachu, four_moves()),
                mon(Species::Snorlax, four_moves()),
            ],
            vec![mon(Species::Gengar, four_moves())],
        );
        let joint = joint_actions(
            &state,
            Player::P1,
            Phase::Normal,
            move_dex(),
            pokemon_dex(),
            None,
        );
        assert!(joint.actions.iter().all(|combo| combo.len() == 2));
        // With exactly one bench Pokemon, no joint action may switch both slots.
        for combo in &joint.actions {
            let switches = combo
                .iter()
                .filter(|c| matches!(c, BattleCommand::Switch(_)))
                .count();
            assert!(switches <= 1, "both slots switched to the same Pokemon");
        }
    }

    #[test]
    fn capping_reduces_the_set_and_records_the_original_size() {
        let state = battle_state_from_lists(
            vec![
                mon(Species::Pikachu, four_moves()),
                mon(Species::Snorlax, four_moves()),
            ],
            vec![mon(Species::Gengar, four_moves())],
            vec![
                mon(Species::Pikachu, four_moves()),
                mon(Species::Snorlax, four_moves()),
            ],
            vec![mon(Species::Gengar, four_moves())],
        );
        let uncapped = joint_actions(
            &state,
            Player::P1,
            Phase::Normal,
            move_dex(),
            pokemon_dex(),
            None,
        );
        let capped = joint_actions(
            &state,
            Player::P1,
            Phase::Normal,
            move_dex(),
            pokemon_dex(),
            Some(6),
        );
        assert!(capped.actions.len() <= 6);
        assert_eq!(capped.total, uncapped.actions.len());
        assert!(capped.was_capped());
        // Every retained action must still be legal.
        for combo in &capped.actions {
            assert!(validate_battle_command_combination(combo));
        }
    }

    /// A cap never removes a player's ability to act.
    #[test]
    fn capping_never_empties_the_action_set() {
        let state = battle_state_from_lists(
            vec![mon(Species::Pikachu, four_moves())],
            vec![],
            vec![mon(Species::Pikachu, four_moves())],
            vec![],
        );
        let capped = joint_actions(
            &state,
            Player::P1,
            Phase::Normal,
            move_dex(),
            pokemon_dex(),
            Some(0),
        );
        assert!(!capped.actions.is_empty());
    }

    #[test]
    fn a_fainted_slot_with_an_empty_bench_can_only_pass() {
        let mut state = battle_state_from_lists(
            vec![mon(Species::Pikachu, four_moves())],
            vec![],
            vec![mon(Species::Pikachu, four_moves())],
            vec![],
        );
        state.p1_active_mons[0].hp = 0;
        state.p1_active_mons[0].fainted = true;
        state.turn_started = true;
        state.turn_ended = true;

        let phase = phase_of(&MatchState::BattleState(state.clone()));
        assert_eq!(phase, Phase::Replacement);

        let joint = joint_actions(
            &state,
            Player::P1,
            phase,
            move_dex(),
            pokemon_dex(),
            None,
        );
        assert_eq!(joint.actions, vec![vec![BattleCommand::Pass]]);
    }

    #[test]
    fn a_replacement_offers_every_healthy_bench_pokemon() {
        let mut state = battle_state_from_lists(
            vec![mon(Species::Pikachu, four_moves())],
            vec![
                mon(Species::Snorlax, four_moves()),
                mon(Species::Gengar, four_moves()),
            ],
            vec![mon(Species::Pikachu, four_moves())],
            vec![],
        );
        state.p1_active_mons[0].hp = 0;
        state.p1_active_mons[0].fainted = true;
        state.turn_started = true;
        state.turn_ended = true;

        let joint = joint_actions(
            &state,
            Player::P1,
            Phase::Replacement,
            move_dex(),
            pokemon_dex(),
            None,
        );
        assert_eq!(joint.actions.len(), 2);
        assert!(joint.actions.iter().all(|combo| matches!(
            combo.as_slice(),
            [BattleCommand::Switch(_)]
        )));
    }

    /// The healthy partner of a fainted slot must not get to act again during a
    /// replacement — it already moved this turn.
    #[test]
    fn a_healthy_slot_passes_during_a_replacement() {
        let mut state = battle_state_from_lists(
            vec![
                mon(Species::Pikachu, four_moves()),
                mon(Species::Snorlax, four_moves()),
            ],
            vec![mon(Species::Gengar, four_moves())],
            vec![
                mon(Species::Pikachu, four_moves()),
                mon(Species::Snorlax, four_moves()),
            ],
            vec![mon(Species::Gengar, four_moves())],
        );
        state.p1_active_mons[0].hp = 0;
        state.p1_active_mons[0].fainted = true;
        state.turn_started = true;
        state.turn_ended = true;

        let joint = joint_actions(
            &state,
            Player::P1,
            Phase::Replacement,
            move_dex(),
            pokemon_dex(),
            None,
        );
        assert!(!joint.actions.is_empty());
        for combo in &joint.actions {
            assert!(matches!(combo[1], BattleCommand::Pass));
        }
    }

    #[test]
    fn phase_classification() {
        let state = battle_state_from_lists(
            vec![mon(Species::Pikachu, four_moves())],
            vec![],
            vec![mon(Species::Pikachu, four_moves())],
            vec![],
        );
        assert_eq!(
            phase_of(&MatchState::BattleState(state.clone())),
            Phase::Normal
        );
        assert_eq!(
            phase_of(&MatchState::GameOverState {
                winner: Player::P1,
                pending_events: Vec::new(),
                final_state: Box::new(state),
            }),
            Phase::GameOver
        );
    }
}
