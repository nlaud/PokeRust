use std::collections::HashMap;
use std::sync::OnceLock;

use crate::state::battle::{AttackCommand, BattleCommand, BattleState, FieldSlot, MatchState, Player};
use crate::data::pokemon_move::PokemonMove;
use crate::data::species::Species;
use crate::state::dex_data::{parse_move_dex, parse_pokemon_dex, VolatileStatus};
use crate::state::pokemon::{PokemonState, VolatileStatusState};
use crate::simulator::simulate_turn;
use crate::simulator::helpers as simulator_helpers;

static POKEMON_DEX: OnceLock<HashMap<Species, crate::state::dex_data::PokemonData>> = OnceLock::new();
static MOVE_DEX: OnceLock<HashMap<PokemonMove, crate::state::dex_data::MoveData>> = OnceLock::new();

pub fn pokemon_dex() -> &'static HashMap<Species, crate::state::dex_data::PokemonData> {
    POKEMON_DEX.get_or_init(|| parse_pokemon_dex("../pokemon_info/showdownDex.txt"))
}

pub fn move_dex() -> &'static HashMap<PokemonMove, crate::state::dex_data::MoveData> {
    MOVE_DEX.get_or_init(|| parse_move_dex("../pokemon_info/showdownMoves.txt"))
}

pub fn battle_state_from_lists(
    p1_active_mons: Vec<PokemonState>,
    p1_back_mons: Vec<PokemonState>,
    p2_active_mons: Vec<PokemonState>,
    p2_back_mons: Vec<PokemonState>,
) -> BattleState {
    assert_eq!(p1_active_mons.len(), p2_active_mons.len());

    let active_per_side = p1_active_mons.len() as u8;

    let mut state = BattleState {
        active_per_side,
        p1_active_mons,
        p2_active_mons,
        p1_back_mons,
        p2_back_mons,
        action_queue: vec![],
        turn_number: 0,
        turn_started: false,
        turn_ended: false,
        p1_has_tera: true,
        p2_has_tera: true,
        p1_has_mega: true,
        p2_has_mega: true,
        weather: None,
        weather_turns: None,
        pseudo_weathers: vec![],
        pseudo_weather_turns: vec![],
        terrain: None,
        terrain_turns: None,
        p1_side_conditions: vec![],
        p1_side_condition_turns: vec![],
        p2_side_conditions: vec![],
        p2_side_condition_turns: vec![],
        p1_slot_conditions: vec![Vec::new(); active_per_side as usize],
        p2_slot_conditions: vec![Vec::new(); active_per_side as usize],
        self_switch_pending: None,
        items_consumed_this_turn: vec![],
        last_move_on_field: None,
        sub_damage_dealt: 0,
        round_used_this_turn: false,
    };

    // Assign each side a stable, party-unique `mon_id` (active slots first, then bench).
    // P2's ids are offset by P1's total party count so a single u8 is globally unique across
    // both teams — required for trapping-volatile source tracking (PartiallyTrapped/Trapped).
    let p1_count = state.p1_active_mons.len() + state.p1_back_mons.len();
    for (idx, mon) in state.p1_active_mons.iter_mut().chain(state.p1_back_mons.iter_mut()).enumerate() {
        mon.mon_id = idx as u8;
    }
    for (idx, mon) in state.p2_active_mons.iter_mut().chain(state.p2_back_mons.iter_mut()).enumerate() {
        mon.mon_id = (p1_count + idx) as u8;
    }

    for slot_idx in 0..state.p1_active_mons.len() {
        simulator_helpers::process_pokemon_send_out(
            &mut state,
            FieldSlot {
                player: Player::P1,
                slot_index: slot_idx as u8,
            },
        );
    }

    for slot_idx in 0..state.p2_active_mons.len() {
        simulator_helpers::process_pokemon_send_out(
            &mut state,
            FieldSlot {
                player: Player::P2,
                slot_index: slot_idx as u8,
            },
        );
    }

    state
}

pub fn simple_attack(_player: Player, move_slots: Vec<usize>) -> Vec<BattleCommand> {
    move_slots
        .into_iter()
        .map(|move_slot| {
            BattleCommand::Attack(AttackCommand {
                move_slot,
                target: None,
                terastallize: false,
                mega_evolve: false,
            })
        })
        .collect()
}

pub fn simple_attack_mega(_player: Player, move_slots: Vec<usize>) -> Vec<BattleCommand> {
    move_slots
        .into_iter()
        .enumerate()
        .map(|(index, move_slot)| {
            BattleCommand::Attack(AttackCommand {
                move_slot,
                target: None,
                terastallize: false,
                mega_evolve: index == 0,
            })
        })
        .collect()
}

pub fn is_permutation<T: PartialEq + Clone>(vec1: &Vec<T>, vec2: &Vec<T>) -> bool {
    if vec1.len() != vec2.len() {
        return false;
    }

    let mut vec2_copy = vec2.clone();
    for item1 in vec1 {
        if let Some(pos) = vec2_copy.iter().position(|item2| item1 == item2) {
            vec2_copy.remove(pos);
        } else {
            return false;
        }
    }
    true
}

pub fn extract_battle_state(outcomes: Vec<(MatchState, f64)>) -> (BattleState, f64) {
    assert_eq!(outcomes.len(), 1);
    let (state, probability) = outcomes.into_iter().next().unwrap();
    match state {
        MatchState::BattleState(battle_state) => (battle_state, probability),
        _ => panic!("expected a battle state outcome"),
    }
}

pub fn hit_probability(outcomes: &[(MatchState, f64)], initial_hp: u16) -> f64 {
    outcomes
        .iter()
        .map(|(state, probability)| match state {
            MatchState::BattleState(bs) if bs.p2_active_mons[0].hp < initial_hp => *probability,
            MatchState::GameOverState { .. } => *probability,
            _ => 0.0,
        })
        .sum()
}

pub fn has_sky_drop_turn_volatile(mon: &PokemonState) -> bool {
    mon.volatiles.iter().any(|volatile| matches!(volatile, VolatileStatusState::TurnStatus(VolatileStatus::SkyDrop, _)))
}

pub fn has_sky_drop_move_volatile(mon: &PokemonState) -> bool {
    mon.volatiles.iter().any(|volatile| matches!(volatile, VolatileStatusState::MoveStatus(VolatileStatus::SemiInvulnerable(PokemonMove::SkyDrop), _)))
}

pub fn has_charging_volatile(mon: &PokemonState, move_name: PokemonMove) -> bool {
    mon.volatiles
        .iter()
        .any(|volatile| matches!(volatile, VolatileStatusState::Charging(charged_move, _) if *charged_move == move_name))
}

pub fn confusion_turns(mon: &PokemonState) -> Option<u16> {
    mon.volatiles.iter().find_map(|volatile| match volatile {
        VolatileStatusState::MoveStatus(VolatileStatus::Confusion, turns) => Some(*turns),
        VolatileStatusState::TurnStatus(VolatileStatus::Confusion, turns) => Some(*turns),
        _ => None,
    })
}

/// Strip transient tracking fields (`last_used_move`, `original_ability`, `entered_this_turn`)
/// from every Pokémon in every outcome state so that existing tests can compare states without
/// caring about fields that are only relevant within a single turn's execution.
pub fn normalize_battle_outcomes(outcomes: Vec<(MatchState, f64)>) -> Vec<(MatchState, f64)> {
    fn strip(mon: &mut PokemonState) {
        mon.last_used_move = None;
        mon.original_ability = None;
        mon.entered_this_turn = false;
        mon.first_move_on_field = false;
        mon.first_turn_on_field_pending = false;
        // Persists across turns by design (Stomping Tantrum / Micle Berry), but is
        // transient bookkeeping for state-equality purposes.
        mon.last_move_failed = false;
        // Tests that compare full states generically (smoke_test, simple_damage, etc.)
        // should not fail just because the mover's used_moves_this_field changed.
        // Tests that care about Last Resort gating should check the field explicitly.
        mon.used_moves_this_field = [false; 4];
        // Rage Fist hit counter — stripped so basic damage tests don't need to account
        // for it. Tests that care about Rage Fist specifically read times_hit directly
        // from the BattleState rather than using outcomes_permutation.
        mon.times_hit = 0;
    }
    outcomes.into_iter().map(|(state, prob)| {
        let state = match state {
            MatchState::BattleState(mut bs) => {
                for mon in bs.p1_active_mons.iter_mut()
                    .chain(bs.p2_active_mons.iter_mut())
                    .chain(bs.p1_back_mons.iter_mut())
                    .chain(bs.p2_back_mons.iter_mut())
                {
                    strip(mon);
                }
                MatchState::BattleState(bs)
            }
            other => other,
        };
        (state, prob)
    }).collect()
}

pub fn run_single_turn(
    state: &MatchState,
    p1_cmd: &crate::state::battle::PlayerCommand,
    p2_cmd: &crate::state::battle::PlayerCommand,
    move_dex: &HashMap<PokemonMove, crate::state::dex_data::MoveData>,
    pokemon_dex: &HashMap<Species, crate::state::dex_data::PokemonData>,
) -> Vec<(MatchState, f64)> {
    simulate_turn(state, p1_cmd, p2_cmd, move_dex, pokemon_dex, false, 1)
}

/// Compare two outcome vectors for permutation-equality after stripping transient tracking
/// fields (`last_used_move`, `original_ability`) from all states in both sides.
/// Use this instead of `is_permutation` whenever comparing `Vec<(MatchState, f64)>`.
pub fn outcomes_permutation(actual: &[(MatchState, f64)], expected: &[(MatchState, f64)]) -> bool {
    let norm_a = normalize_battle_outcomes(actual.to_vec());
    let norm_e = normalize_battle_outcomes(expected.to_vec());
    is_permutation(&norm_a, &norm_e)
}

pub fn damage_distribution(outcomes: &[(MatchState, f64)], initial_hp: u16) -> HashMap<u16, f64> {
    let mut distribution = HashMap::new();

    for (state, probability) in outcomes {
        let damage = match state {
            MatchState::BattleState(bs) => initial_hp.saturating_sub(bs.p2_active_mons[0].hp),
            MatchState::GameOverState { .. } => initial_hp,
            _ => 0,
        };

        *distribution.entry(damage).or_insert(0.0) += *probability;
    }

    distribution
}

pub fn repeat_hit_distribution(hit_distribution: &[(u16, f64)], hit_count: usize) -> HashMap<u16, f64> {
    let mut distribution = HashMap::from([(0u16, 1.0)]);

    for _ in 0..hit_count {
        let mut next = HashMap::new();
        for (damage, damage_probability) in &distribution {
            for (hit_damage, hit_probability) in hit_distribution {
                *next.entry(damage.saturating_add(*hit_damage)).or_insert(0.0) += damage_probability * hit_probability;
            }
        }
        distribution = next;
    }

    distribution
}

pub fn combine_hit_distributions_with_hit_chances(
    hit_distributions: &[Vec<(u16, f64)>],
    hit_chances: &[f64],
) -> HashMap<u16, f64> {
    let mut active = HashMap::from([(0u16, 1.0)]);
    let mut finished = HashMap::new();

    for (hit_distribution, hit_chance) in hit_distributions.iter().zip(hit_chances.iter()) {
        let mut next_active = HashMap::new();

        for (damage_so_far, damage_probability) in &active {
            *next_active.entry(*damage_so_far).or_insert(0.0) += damage_probability * (1.0 - hit_chance);

            for (hit_damage, hit_probability) in hit_distribution {
                *next_active.entry(damage_so_far.saturating_add(*hit_damage)).or_insert(0.0) += damage_probability * hit_chance * hit_probability;
            }
        }

        active = next_active;
    }

    for (damage_so_far, damage_probability) in active {
        *finished.entry(damage_so_far).or_insert(0.0) += damage_probability;
    }

    finished
}

pub fn assert_distribution_close(actual: HashMap<u16, f64>, expected: HashMap<u16, f64>) {
    assert_eq!(actual.len(), expected.len());

    for (damage, expected_probability) in expected {
        let actual_probability = actual.get(&damage).copied().unwrap_or_default();
        assert!(
            (actual_probability - expected_probability).abs() < 1e-9,
            "damage {damage}: actual={actual_probability}, expected={expected_probability}"
        );
    }
}
