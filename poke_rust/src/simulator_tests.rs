use crate::battle::{AttackCommand, BattleCommand, BattleState, FieldSlot, Player};
use crate::pokemon::PokemonState;
use crate::simulator_helpers;

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
    };

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



#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::OnceLock;
    use crate::battle::{MatchState, PlayerCommand};
    use crate::data::pokemon_move::PokemonMove;
    use crate::data::species::Species;
    use crate::dex_data::{parse_move_dex, parse_pokemon_dex};
    use crate::pokemon::{build_pokemon_state, Nature};
    use crate::simulator::simulate_turn;
    use crate::simulator_helpers::coalesce_branches;
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

    /// Checks if two vectors are permutations of each other.
    pub fn is_permutation<T: PartialEq + Clone>(
        vec1: &Vec<T>,
        vec2: &Vec<T>,
    ) -> bool {
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
    
    static POKEMON_DEX: OnceLock<std::collections::HashMap<Species, crate::dex_data::PokemonData>> = OnceLock::new();
    static MOVE_DEX: OnceLock<std::collections::HashMap<PokemonMove, crate::dex_data::MoveData>> = OnceLock::new();

    fn pokemon_dex() -> &'static std::collections::HashMap<Species, crate::dex_data::PokemonData> {
        POKEMON_DEX.get_or_init(|| parse_pokemon_dex("../pokemon_info/showdownDex.txt"))
    }

    fn move_dex() -> &'static std::collections::HashMap<PokemonMove, crate::dex_data::MoveData> {
        MOVE_DEX.get_or_init(|| parse_move_dex("../pokemon_info/showdownMoves.txt"))
    }
    
    #[test]
    fn smoke_test() {
        let pokemon_dex= pokemon_dex();
        let move_dex = move_dex();

        let splash_set = [Some(PokemonMove::Splash), None, None, None];

        let p1_mon = build_pokemon_state(
            Species::Magikarp,
            &pokemon_dex,
            &move_dex,
            None,
            Some(splash_set.clone()),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            false,
        );
        let p2_mon = build_pokemon_state(
            Species::Shuckle,
            &pokemon_dex,
            &move_dex,
            None,
            Some(splash_set),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            false,
        );

        let initial_state = battle_state_from_lists(vec![p1_mon], vec![], vec![p2_mon], vec![]);
        let before_state = initial_state.clone();


        let p1_cmd = PlayerCommand::Battle(simple_attack(Player::P1, vec![0]));
        let p2_cmd = PlayerCommand::Battle(simple_attack(Player::P2, vec![0]));

        let outcomes = simulate_turn(
            &MatchState::BattleState(initial_state),
            &p1_cmd,
            &p2_cmd,
            &move_dex,
            &pokemon_dex,
            false,
            1,
        );

        assert!(!outcomes.is_empty());
        let total_probability: f64 = outcomes.iter().map(|(_, p)| *p).sum();
        assert!((total_probability - 1.0).abs() < 1e-9);

        let mut expected_final_state = before_state.clone();
        expected_final_state.p1_active_mons[0].move_pp[0] -= 1;
        expected_final_state.p2_active_mons[0].move_pp[0] -= 1;
        expected_final_state.turn_number += 1;

        let expected_outcomes = vec![(MatchState::BattleState(expected_final_state), 1.0)];

        let normalized_outcomes: Vec<(String, f64)> = outcomes
            .iter()
            .map(|(state, probability)| (format!("{:?}", state), *probability))
            .collect();
        let normalized_expected: Vec<(String, f64)> = expected_outcomes
            .iter()
            .map(|(state, probability)| (format!("{:?}", state), *probability))
            .collect();

        assert!(is_permutation(&outcomes, &expected_outcomes));
    }

    #[test]
    fn branch_coalesce_helper() {
        let pokemon_dex = pokemon_dex();
        let move_dex = move_dex();

        let p1_mon = build_pokemon_state(
            Species::Magikarp,
            &pokemon_dex,
            &move_dex,
            None,
            Some([Some(PokemonMove::Splash), None, None, None]),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            false,
        );
        let p2_mon = build_pokemon_state(
            Species::Shuckle,
            &pokemon_dex,
            &move_dex,
            None,
            Some([Some(PokemonMove::Splash), None, None, None]),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            false,
        );

        let state = MatchState::BattleState(battle_state_from_lists(vec![p1_mon], vec![], vec![p2_mon], vec![]));
        let branches = vec![(state.clone(), 0.25), (state, 0.75)];

        let merged = coalesce_branches(branches);
        assert_eq!(merged.len(), 1);
        assert!((merged[0].1 - 1.0).abs() < 1e-9);
    }
    
    #[test]
    fn simple_damage() {
        let pokemon_dex= pokemon_dex();
        let move_dex = move_dex();

        let p1_mon = build_pokemon_state(
            Species::Garchomp,
            &pokemon_dex,
            &move_dex,
            None,
            Some([Some(PokemonMove::Earthquake), Some(PokemonMove::Splash), None, None]),
            None,
            None,
            Some(Nature::Adamant),
            None,
            None,
            Some([2, 32, 0, 0, 0, 32]),
            None,
            true,
        );

        let p2_mon = build_pokemon_state(
            Species::Garchomp,
            &pokemon_dex,
            &move_dex,
            None,
            Some([Some(PokemonMove::Earthquake), Some(PokemonMove::Splash), None, None]),
            None,
            None,
            Some(Nature::Adamant),
            None,
            None,
            Some([2, 32, 0, 0, 0, 32]),
            None,
            true,
        );

        let initial_state = battle_state_from_lists(vec![p1_mon], vec![], vec![p2_mon], vec![]);
        let before_state = initial_state.clone();


        let p1_cmd = PlayerCommand::Battle(simple_attack(Player::P1, vec![0]));//Earthquake
        let p2_cmd = PlayerCommand::Battle(simple_attack(Player::P2, vec![1]));//Splash

        let outcomes = simulate_turn(
            &MatchState::BattleState(initial_state),
            &p1_cmd,
            &p2_cmd,
            &move_dex,
            &pokemon_dex,
            false,
            16,
        );

        assert!(!outcomes.is_empty());
        let total_probability: f64 = outcomes.iter().map(|(_, p)| *p).sum();
        assert!((total_probability - 1.0).abs() < 1e-9);

        let mut expected_final_state = before_state.clone();
        expected_final_state.p1_active_mons[0].move_pp[0] -= 1;
        expected_final_state.p2_active_mons[0].move_pp[1] -= 1;
        expected_final_state.turn_number += 1;

        let mut expected_outcomes: Vec<(MatchState,f64)> = Vec::new();

        let possible_rolls = vec![99, 100, 100, 102, 103, 105, 105, 106, 108, 109, 111, 111, 112, 114, 115, 117];
        
        let mut counted_rolls: HashMap<u16, usize> = HashMap::new();
        for &roll in &possible_rolls {
            *counted_rolls.entry(roll).or_insert(0) += 1;
        }        

        for (damage_roll, counts) in counted_rolls {
            let mut outcome = expected_final_state.clone();
            outcome.p2_active_mons[0].hp -= damage_roll;
            expected_outcomes.push((MatchState::BattleState(outcome), (counts as f64) / 16.0));
        }

        println!("{:?}", outcomes);
        assert!(is_permutation(&outcomes, &expected_outcomes));
    }

    #[test]
    fn simple_crit() {
        let pokemon_dex= pokemon_dex();
        let move_dex = move_dex();

        let p1_mon = build_pokemon_state(
            Species::Garchomp,
            &pokemon_dex,
            &move_dex,
            None,
            Some([Some(PokemonMove::Earthquake), Some(PokemonMove::Splash), None, None]),
            None,
            None,
            Some(Nature::Adamant),
            None,
            None,
            Some([2, 32, 0, 0, 0, 32]),
            None,
            true,
        );

        let p2_mon = build_pokemon_state(
            Species::Garchomp,
            &pokemon_dex,
            &move_dex,
            None,
            Some([Some(PokemonMove::Earthquake), Some(PokemonMove::Splash), None, None]),
            None,
            None,
            Some(Nature::Adamant),
            None,
            None,
            Some([2, 32, 0, 0, 0, 32]),
            None,
            true,
        );

        let initial_state = battle_state_from_lists(vec![p1_mon], vec![], vec![p2_mon], vec![]);
        let before_state = initial_state.clone();


        let p1_cmd = PlayerCommand::Battle(simple_attack(Player::P1, vec![0]));//Earthquake
        let p2_cmd = PlayerCommand::Battle(simple_attack(Player::P2, vec![1]));//Splash

        let outcomes = simulate_turn(
            &MatchState::BattleState(initial_state),
            &p1_cmd,
            &p2_cmd,
            &move_dex,
            &pokemon_dex,
            true,
            16,
        );

        assert!(!outcomes.is_empty());
        let total_probability: f64 = outcomes.iter().map(|(_, p)| *p).sum();
        assert!((total_probability - 1.0).abs() < 1e-9);

        let mut expected_final_state = before_state.clone();
        expected_final_state.p1_active_mons[0].move_pp[0] -= 1;
        expected_final_state.p2_active_mons[0].move_pp[1] -= 1;
        expected_final_state.turn_number += 1;

        let mut expected_outcomes: Vec<(MatchState,f64)> = Vec::new();

        let possible_rolls = vec![99, 100, 100, 102, 103, 105, 105, 106, 108, 109, 111, 111, 112, 114, 115, 117];
        let crit_rolls = vec![148, 150, 151, 153, 156, 157, 159, 160, 162, 163, 166, 168, 169, 171, 172, 175];

        let mut counted_rolls: HashMap<u16, usize> = HashMap::new();
        for &roll in &possible_rolls {
            *counted_rolls.entry(roll).or_insert(0) += 1;
        }        

        for (damage_roll, counts) in counted_rolls {
            let mut outcome = expected_final_state.clone();
            outcome.p2_active_mons[0].hp -= damage_roll;
            expected_outcomes.push((MatchState::BattleState(outcome), (counts as f64) / 16.0 * 23.0 / 24.0));
        }

        for damage_roll in crit_rolls {
            let mut outcome = expected_final_state.clone();
            outcome.p2_active_mons[0].hp -= damage_roll;
            expected_outcomes.push((MatchState::BattleState(outcome), 1.0 / 16.0 / 24.0));
        }

        println!("{:?}", outcomes);
        assert!(is_permutation(&outcomes, &expected_outcomes));
    }

    #[test]
    fn single_roll() {
        let pokemon_dex= pokemon_dex();
        let move_dex = move_dex();

        let p1_mon = build_pokemon_state(
            Species::Garchomp,
            &pokemon_dex,
            &move_dex,
            None,
            Some([Some(PokemonMove::Bite), Some(PokemonMove::Splash), None, None]),
            None,
            None,
            Some(Nature::Adamant),
            None,
            None,
            Some([2, 32, 0, 0, 0, 31]),
            None,
            true,
        );

        let p2_mon = build_pokemon_state(
            Species::Garchomp,
            &pokemon_dex,
            &move_dex,
            None,
            Some([Some(PokemonMove::Bite), Some(PokemonMove::Splash), None, None]),
            None,
            None,
            Some(Nature::Adamant),
            None,
            None,
            Some([2, 32, 0, 0, 0, 32]),
            None,
            true,
        );

        let initial_state = battle_state_from_lists(vec![p1_mon], vec![], vec![p2_mon], vec![]);
        let before_state = initial_state.clone();


        let p1_cmd = PlayerCommand::Battle(simple_attack(Player::P1, vec![0]));//Bite
        let p2_cmd = PlayerCommand::Battle(simple_attack(Player::P2, vec![1]));//Splash

        let outcomes = simulate_turn(
            &MatchState::BattleState(initial_state),
            &p1_cmd,
            &p2_cmd,
            &move_dex,
            &pokemon_dex,
            false,
            1,
        );

        assert!(!outcomes.is_empty());
        let total_probability: f64 = outcomes.iter().map(|(_, p)| *p).sum();
        assert!((total_probability - 1.0).abs() < 1e-9);

        let mut expected_final_state = before_state.clone();
        expected_final_state.p1_active_mons[0].move_pp[0] -= 1;
        expected_final_state.p2_active_mons[0].move_pp[1] -= 1;
        expected_final_state.turn_number += 1;

        let mut expected_outcomes: Vec<(MatchState,f64)> = Vec::new();
        
        let mut outcome = expected_final_state.clone();
        outcome.p2_active_mons[0].hp -= 43;
        expected_outcomes.push((MatchState::BattleState(outcome), 1.0));

        println!("{:?}", outcomes);
        assert!(is_permutation(&outcomes, &expected_outcomes));
    }

    #[test]
    fn two_rolls() {
        let pokemon_dex= pokemon_dex();
        let move_dex = move_dex();

        let p1_mon = build_pokemon_state(
            Species::Garchomp,
            &pokemon_dex,
            &move_dex,
            None,
            Some([Some(PokemonMove::Bite), Some(PokemonMove::Splash), None, None]),
            None,
            None,
            Some(Nature::Adamant),
            None,
            None,
            Some([2, 32, 0, 0, 0, 31]),
            None,
            true,
        );

        let p2_mon = build_pokemon_state(
            Species::Garchomp,
            &pokemon_dex,
            &move_dex,
            None,
            Some([Some(PokemonMove::Bite), Some(PokemonMove::Splash), None, None]),
            None,
            None,
            Some(Nature::Adamant),
            None,
            None,
            Some([2, 32, 0, 0, 0, 32]),
            None,
            true,
        );

        let initial_state = battle_state_from_lists(vec![p1_mon], vec![], vec![p2_mon], vec![]);
        let before_state = initial_state.clone();


        let p1_cmd = PlayerCommand::Battle(simple_attack(Player::P1, vec![0]));//Earthquake
        let p2_cmd = PlayerCommand::Battle(simple_attack(Player::P2, vec![1]));//Splash

        let outcomes = simulate_turn(
            &MatchState::BattleState(initial_state),
            &p1_cmd,
            &p2_cmd,
            &move_dex,
            &pokemon_dex,
            false,
            2,
        );

        assert!(!outcomes.is_empty());
        let total_probability: f64 = outcomes.iter().map(|(_, p)| *p).sum();
        assert!((total_probability - 1.0).abs() < 1e-9);

        let mut expected_final_state = before_state.clone();
        expected_final_state.p1_active_mons[0].move_pp[0] -= 1;
        expected_final_state.p2_active_mons[0].move_pp[1] -= 1;
        expected_final_state.turn_number += 1;

        let mut expected_outcomes: Vec<(MatchState,f64)> = Vec::new();
        
        let mut outcome1 = expected_final_state.clone();
        outcome1.p2_active_mons[0].hp -= 39;
        expected_outcomes.push((MatchState::BattleState(outcome1), 0.5));

        let mut outcome2 = expected_final_state.clone();
        outcome2.p2_active_mons[0].hp -= 47;
        expected_outcomes.push((MatchState::BattleState(outcome2), 0.5));

        println!("{:?}", outcomes);
        assert!(is_permutation(&outcomes, &expected_outcomes));
    }
    #[test]
    fn single_roll_crit() {
        let pokemon_dex= pokemon_dex();
        let move_dex = move_dex();

        let p1_mon = build_pokemon_state(
            Species::Garchomp,
            &pokemon_dex,
            &move_dex,
            None,
            Some([Some(PokemonMove::Earthquake), Some(PokemonMove::Splash), None, None]),
            None,
            None,
            Some(Nature::Adamant),
            None,
            None,
            Some([2, 32, 0, 0, 0, 32]),
            None,
            true,
        );

        let p2_mon = build_pokemon_state(
            Species::Garchomp,
            &pokemon_dex,
            &move_dex,
            None,
            Some([Some(PokemonMove::Earthquake), Some(PokemonMove::Splash), None, None]),
            None,
            None,
            Some(Nature::Adamant),
            None,
            None,
            Some([2, 32, 0, 0, 0, 32]),
            None,
            true,
        );

        let initial_state = battle_state_from_lists(vec![p1_mon], vec![], vec![p2_mon], vec![]);
        let before_state = initial_state.clone();


        let p1_cmd = PlayerCommand::Battle(simple_attack(Player::P1, vec![0]));//Earthquake
        let p2_cmd = PlayerCommand::Battle(simple_attack(Player::P2, vec![1]));//Splash

        let outcomes = simulate_turn(
            &MatchState::BattleState(initial_state),
            &p1_cmd,
            &p2_cmd,
            &move_dex,
            &pokemon_dex,
            true,
            1,
        );

        assert!(!outcomes.is_empty());
        let total_probability: f64 = outcomes.iter().map(|(_, p)| *p).sum();
        assert!((total_probability - 1.0).abs() < 1e-9);

        let mut expected_final_state = before_state.clone();
        expected_final_state.p1_active_mons[0].move_pp[0] -= 1;
        expected_final_state.p2_active_mons[0].move_pp[1] -= 1;
        expected_final_state.turn_number += 1;

        let mut expected_outcomes: Vec<(MatchState,f64)> = Vec::new();
        
        let mut outcome1 = expected_final_state.clone();
        outcome1.p2_active_mons[0].hp -= 106;
        expected_outcomes.push((MatchState::BattleState(outcome1), 23.0 / 24.0));
        
        let mut outcome2 = expected_final_state.clone();
        outcome2.p2_active_mons[0].hp -= 160;
        expected_outcomes.push((MatchState::BattleState(outcome2), 1.0 / 24.0));

        println!("{:?}", outcomes);
        assert!(is_permutation(&outcomes, &expected_outcomes));
    }

    #[test]
    fn OHKO_win() {
        let pokemon_dex = pokemon_dex();
        let move_dex = move_dex();

        let p1_mon = build_pokemon_state(
            Species::Garchomp,
            &pokemon_dex,
            &move_dex,
            Some(100),
            Some([Some(PokemonMove::DragonClaw), Some(PokemonMove::Splash), None, None]),
            None,
            None,
            Some(Nature::Adamant),
            None,
            None,
            Some([2, 32, 0, 0, 0, 32]),
            None,
            true,
        );

        let mut p2_mon = build_pokemon_state(
            Species::Garchomp,
            &pokemon_dex,
            &move_dex,
            None,
            Some([Some(PokemonMove::DragonClaw), Some(PokemonMove::Splash), None, None]),
            None,
            None,
            Some(Nature::Adamant),
            None,
            None,
            Some([2, 32, 0, 0, 0, 32]),
            None,
            true,
        );
        p2_mon.hp = 1;

        let initial_state = battle_state_from_lists(vec![p1_mon], vec![], vec![p2_mon], vec![]);


        let outcomes = simulate_turn(
            &MatchState::BattleState(initial_state),
            &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
            &PlayerCommand::Battle(simple_attack(Player::P2, vec![1])),
            &move_dex,
            &pokemon_dex,
            false,
            1,
        );

        assert!(!outcomes.is_empty());
        assert!(outcomes.iter().all(|(state, _)| matches!(state, MatchState::GameOverState { winner: Player::P1 })));
        let total_probability: f64 = outcomes.iter().map(|(_, p)| *p).sum();
        assert!((total_probability - 1.0).abs() < 1e-9);
    }

    #[test]
    fn super_effective_damage() {
        let pokemon_dex= pokemon_dex();
        let move_dex = move_dex();

        let p1_mon = build_pokemon_state(
            Species::Venusaur,
            &pokemon_dex,
            &move_dex,
            None,
            Some([Some(PokemonMove::MagicalLeaf), None, None, None]),
            None,
            None,
            Some(Nature::Modest),
            None,
            None,
            Some([2, 0, 0, 32, 0, 32]),
            None,
            true,
        );

        let p2_mon = build_pokemon_state(
            Species::RotomWash,
            &pokemon_dex,
            &move_dex,
            None,
            Some([Some(PokemonMove::Splash), None, None, None]),
            None,
            None,
            Some(Nature::Modest),
            None,
            None,
            Some([32, 0, 0, 0, 32, 2]),
            None,
            true,
        );

        let initial_state = battle_state_from_lists(vec![p1_mon], vec![], vec![p2_mon], vec![]);
        let before_state = initial_state.clone();


        let p1_cmd = PlayerCommand::Battle(simple_attack(Player::P1, vec![0]));//Energy Ball
        let p2_cmd = PlayerCommand::Battle(simple_attack(Player::P2, vec![0]));//Splash

        let outcomes = simulate_turn(
            &MatchState::BattleState(initial_state),
            &p1_cmd,
            &p2_cmd,
            &move_dex,
            &pokemon_dex,
            false,
            1,
        );

        assert!(!outcomes.is_empty());
        let total_probability: f64 = outcomes.iter().map(|(_, p)| *p).sum();
        assert!((total_probability - 1.0).abs() < 1e-9);

        let mut expected_final_state = before_state.clone();
        expected_final_state.p1_active_mons[0].move_pp[0] -= 1;
        expected_final_state.p2_active_mons[0].move_pp[0] -= 1;
        expected_final_state.turn_number += 1;

        let mut expected_outcomes: Vec<(MatchState,f64)> = Vec::new();
        
        let mut outcome = expected_final_state.clone();
        outcome.p2_active_mons[0].hp -= 78;
        expected_outcomes.push((MatchState::BattleState(outcome), 1.0));

        println!("{:?}", outcomes);
        assert!(is_permutation(&outcomes, &expected_outcomes));
    }
    #[test]
    fn not_very_effective_damage() {
        let pokemon_dex= pokemon_dex();
        let move_dex = move_dex();

        let p1_mon = build_pokemon_state(
            Species::Blastoise,
            &pokemon_dex,
            &move_dex,
            Some(100),
            Some([Some(PokemonMove::WaterGun), None, None, None]),
            None,
            None,
            Some(Nature::Modest),
            None,
            None,
            Some([2, 0, 0, 32, 0, 32]),
            None,
            true,
        );

        let p2_mon = build_pokemon_state(
            Species::RotomWash,
            &pokemon_dex,
            &move_dex,
            None,
            Some([Some(PokemonMove::Splash), None, None, None]),
            None,
            None,
            Some(Nature::Modest),
            None,
            None,
            Some([32, 0, 0, 0, 32, 2]),
            None,
            true,
        );

        let initial_state = battle_state_from_lists(vec![p1_mon], vec![], vec![p2_mon], vec![]);
        let before_state = initial_state.clone();


        let p1_cmd = PlayerCommand::Battle(simple_attack(Player::P1, vec![0]));//Energy Ball
        let p2_cmd = PlayerCommand::Battle(simple_attack(Player::P2, vec![0]));//Splash

        let outcomes = simulate_turn(
            &MatchState::BattleState(initial_state),
            &p1_cmd,
            &p2_cmd,
            &move_dex,
            &pokemon_dex,
            false,
            1,
        );

        assert!(!outcomes.is_empty());
        let total_probability: f64 = outcomes.iter().map(|(_, p)| *p).sum();
        assert!((total_probability - 1.0).abs() < 1e-9);

        let mut expected_final_state = before_state.clone();
        expected_final_state.p1_active_mons[0].move_pp[0] -= 1;
        expected_final_state.p2_active_mons[0].move_pp[0] -= 1;
        expected_final_state.turn_number += 1;

        let mut expected_outcomes: Vec<(MatchState,f64)> = Vec::new();
        
        let mut outcome = expected_final_state.clone();
        outcome.p2_active_mons[0].hp -= 43;
        expected_outcomes.push((MatchState::BattleState(outcome), 1.0));

        println!("{:?}", outcomes);
        assert!(is_permutation(&outcomes, &expected_outcomes));
    }

    #[test]
    fn extremely_effective_damage() {
        let pokemon_dex= pokemon_dex();
        let move_dex = move_dex();

        let p1_mon = build_pokemon_state(
            Species::Blastoise,
            &pokemon_dex,
            &move_dex,
            None,
            Some([Some(PokemonMove::AuraSphere), None, None, None]),
            None,
            None,
            Some(Nature::Modest),
            None,
            None,
            Some([2, 0, 0, 32, 0, 32]),
            None,
            true,
        );

        let p2_mon = build_pokemon_state(
            Species::Kingambit,
            &pokemon_dex,
            &move_dex,
            Some(100),
            Some([Some(PokemonMove::Splash), None, None, None]),
            None,
            None,
            Some(Nature::Adamant),
            None,
            None,
            Some([32, 32, 0, 0, 0, 2]),
            None,
            true,
        );

        let initial_state = battle_state_from_lists(vec![p1_mon], vec![], vec![p2_mon], vec![]);
        let before_state = initial_state.clone();


        let p1_cmd = PlayerCommand::Battle(simple_attack(Player::P1, vec![0]));//Energy Ball
        let p2_cmd = PlayerCommand::Battle(simple_attack(Player::P2, vec![0]));//Splash

        let outcomes = simulate_turn(
            &MatchState::BattleState(initial_state),
            &p1_cmd,
            &p2_cmd,
            &move_dex,
            &pokemon_dex,
            false,
            1,
        );

        assert!(!outcomes.is_empty());
        let total_probability: f64 = outcomes.iter().map(|(_, p)| *p).sum();
        assert!((total_probability - 1.0).abs() < 1e-9);

        let mut expected_final_state = before_state.clone();
        expected_final_state.p1_active_mons[0].move_pp[0] -= 1;
        expected_final_state.p2_active_mons[0].move_pp[0] -= 1;
        expected_final_state.turn_number += 1;

        let mut expected_outcomes: Vec<(MatchState,f64)> = Vec::new();
        
        let mut outcome = expected_final_state.clone();
        outcome.p2_active_mons[0].hp -= 96;
        expected_outcomes.push((MatchState::BattleState(outcome), 1.0));

        println!("{:?}", outcomes);
        assert!(is_permutation(&outcomes, &expected_outcomes));
    }

    #[test]
    fn immune_damage() {
        let pokemon_dex= pokemon_dex();
        let move_dex = move_dex();

        let p1_mon = build_pokemon_state(
            Species::Snorlax,
            &pokemon_dex,
            &move_dex,
            None,
            Some([Some(PokemonMove::BodyPress), None, None, None]),
            None,
            None,
            Some(Nature::Adamant),
            None,
            None,
            Some([32, 32, 0, 0, 0, 2]),
            None,
            true,
        );

        let p2_mon = build_pokemon_state(
            Species::Gengar,
            &pokemon_dex,
            &move_dex,
            Some(100),
            Some([Some(PokemonMove::Splash), None, None, None]),
            None,
            None,
            Some(Nature::Modest),
            None,
            None,
            Some([32, 0, 0, 32, 0, 2]),
            None,
            true,
        );

        let initial_state = battle_state_from_lists(vec![p1_mon], vec![], vec![p2_mon], vec![]);
        let before_state = initial_state.clone();


        let p1_cmd = PlayerCommand::Battle(simple_attack(Player::P1, vec![0]));//Energy Ball
        let p2_cmd = PlayerCommand::Battle(simple_attack(Player::P2, vec![0]));//Splash

        let outcomes = simulate_turn(
            &MatchState::BattleState(initial_state),
            &p1_cmd,
            &p2_cmd,
            &move_dex,
            &pokemon_dex,
            false,
            1,
        );

        assert!(!outcomes.is_empty());
        let total_probability: f64 = outcomes.iter().map(|(_, p)| *p).sum();
        assert!((total_probability - 1.0).abs() < 1e-9);

        let mut expected_final_state = before_state.clone();
        expected_final_state.p1_active_mons[0].move_pp[0] -= 1;
        expected_final_state.p2_active_mons[0].move_pp[0] -= 1;
        expected_final_state.turn_number += 1;

        let mut expected_outcomes: Vec<(MatchState,f64)> = Vec::new();
        
        let mut outcome = expected_final_state.clone();
        expected_outcomes.push((MatchState::BattleState(outcome), 1.0));

        println!("{:?}", outcomes);
        assert!(is_permutation(&outcomes, &expected_outcomes));
    }

    #[test]
    fn single_roll_accuracy() {
        let pokemon_dex= pokemon_dex();
        let move_dex = move_dex();

        let p1_mon = build_pokemon_state(
            Species::RotomWash,
            &pokemon_dex,
            &move_dex,
            None,
            Some([Some(PokemonMove::HydroPump), None, None, None]),
            None,
            None,
            Some(Nature::Modest),
            None,
            None,
            Some([32, 0, 0, 32, 0, 2]),
            None,
            true,
        );

        let p2_mon = build_pokemon_state(
            Species::Sableye,
            &pokemon_dex,
            &move_dex,
            None,
            Some([Some(PokemonMove::Splash), None, None, None]),
            None,
            None,
            Some(Nature::Hardy),
            None,
            None,
            Some([32, 0, 2, 0, 0, 32]),
            None,
            true,
        );

        let initial_state = battle_state_from_lists(vec![p1_mon], vec![], vec![p2_mon], vec![]);
        let before_state = initial_state.clone();


        let p1_cmd = PlayerCommand::Battle(simple_attack(Player::P1, vec![0]));//Energy Ball
        let p2_cmd = PlayerCommand::Battle(simple_attack(Player::P2, vec![0]));//Splash

        let outcomes = simulate_turn(
            &MatchState::BattleState(initial_state),
            &p1_cmd,
            &p2_cmd,
            &move_dex,
            &pokemon_dex,
            false,
            1,
        );

        assert!(!outcomes.is_empty());
        let total_probability: f64 = outcomes.iter().map(|(_, p)| *p).sum();
        assert!((total_probability - 1.0).abs() < 1e-9);

        let mut expected_outcomes: Vec<(MatchState,f64)> = Vec::new();

        let mut expected_final_state = before_state.clone();
        expected_final_state.p1_active_mons[0].move_pp[0] -= 1;
        expected_final_state.p2_active_mons[0].move_pp[0] -= 1;
        expected_final_state.turn_number += 1;


        expected_outcomes.push((MatchState::BattleState(expected_final_state.clone()), 1.0 - 0.8));
        
        expected_final_state.p2_active_mons[0].hp -= 136;
        expected_outcomes.push((MatchState::BattleState(expected_final_state), 0.8));

        println!("{:?}", outcomes);
        assert!(is_permutation(&outcomes, &expected_outcomes));
    }

    #[test]
    fn simple_accuracy() {
        let pokemon_dex= pokemon_dex();
        let move_dex = move_dex();

        let p1_mon = build_pokemon_state(
            Species::RotomWash,
            &pokemon_dex,
            &move_dex,
            None,
            Some([Some(PokemonMove::HydroPump), None, None, None]),
            None,
            None,
            Some(Nature::Modest),
            None,
            None,
            Some([32, 0, 0, 32, 0, 2]),
            None,
            true,
        );

        let p2_mon = build_pokemon_state(
            Species::Sableye,
            &pokemon_dex,
            &move_dex,
            None,
            Some([Some(PokemonMove::Splash), None, None, None]),
            None,
            None,
            Some(Nature::Hardy),
            None,
            None,
            Some([32, 0, 2, 0, 0, 32]),
            None,
            true,
        );

        let initial_state = battle_state_from_lists(vec![p1_mon], vec![], vec![p2_mon], vec![]);
        let before_state = initial_state.clone();


        let p1_cmd = PlayerCommand::Battle(simple_attack(Player::P1, vec![0]));//Energy Ball
        let p2_cmd = PlayerCommand::Battle(simple_attack(Player::P2, vec![0]));//Splash

        let outcomes = simulate_turn(
            &MatchState::BattleState(initial_state),
            &p1_cmd,
            &p2_cmd,
            &move_dex,
            &pokemon_dex,
            false,
            16,
        );

        assert!(!outcomes.is_empty());
        let total_probability: f64 = outcomes.iter().map(|(_, p)| *p).sum();
        assert!((total_probability - 1.0).abs() < 1e-9);

        let mut expected_outcomes: Vec<(MatchState,f64)> = Vec::new();

        let mut expected_final_state = before_state.clone();
        expected_final_state.p1_active_mons[0].move_pp[0] -= 1;
        expected_final_state.p2_active_mons[0].move_pp[0] -= 1;
        expected_final_state.turn_number += 1;


        let possible_rolls = vec![126, 127, 129, 130, 132, 133, 135, 136, 138, 139, 141, 142, 144, 145, 147, 148];

        for damage_roll in possible_rolls {
            let mut outcome = expected_final_state.clone();
            outcome.p2_active_mons[0].hp -= damage_roll;
            expected_outcomes.push((MatchState::BattleState(outcome), 1.0 / 16.0 * 0.8));
        }

        expected_outcomes.push((MatchState::BattleState(expected_final_state), (1.0 - 0.8)));

        println!("{:?}", outcomes);
        assert!(is_permutation(&outcomes, &expected_outcomes));
    }
    
    #[test]                                                                   
    fn attack_boosts() {                                      
        let pokemon_dex = pokemon_dex();                                      
        let move_dex = move_dex();                                            
                                                                                
        // Mimeikyu: Attacker (Swords Dance, Shadow Claw)                     
        let p1_mon_initial = build_pokemon_state(                             
            Species::Mimikyu,                                                 
            &pokemon_dex,                                                     
            &move_dex,                                                        
            None,                                                        
            Some([                                                            
                Some(PokemonMove::SwordsDance),                               
                Some(PokemonMove::Splash),                                    
                None,                                                         
                Some(PokemonMove::ShadowClaw),                                
            ]),                                                               
            None,                                                             
            None,                                                             
            Some(Nature::Adamant),                                              
            None,                                                             
            None,                                                             
            Some([2, 32, 0, 0, 0, 32]),                                       
            None,                                                             
            true,                                                             
        );                                                                    
                                                                                
        // Aerodactyl: Target (Splash, move out)                              
        let p2_mon_initial = build_pokemon_state(                             
            Species::Aerodactyl,                                              
            &pokemon_dex,                                                     
            &move_dex,                                                        
            None,                                                             
            Some([                                                            
                Some(PokemonMove::Splash),                                    
                None,                                                         
                None,                                                         
                None,                                                         
            ]),                                                               
            None,                                                             
            None,                                                             
            Some(Nature::Lonely),                                              
            None,                                                             
            None,                                                             
            Some([2, 0, 0, 32, 0, 32]),                                     
            None,                                                             
            true,                                                             
        );                                                                    
                                                                                
        // --- START OF TURN 1: Swords Dance (Mimikyu) vs Splash (Aerodactyl)                                                                    
                                                                                
        let initial_state = battle_state_from_lists(                          
            vec![p1_mon_initial],                                     
            vec![],                                                           
            vec![p2_mon_initial],                                     
            vec![],                                                           
        );                                                                    
        let before_state_t1 = initial_state.clone();                          
                                                                                
        // Commands for Turn 1                                                
        let p1_cmd_t1 = PlayerCommand::Battle(simple_attack(Player::P1, vec![0])); // Swords Dance                                                            
        let p2_cmd_t1 = PlayerCommand::Battle(simple_attack(Player::P2, vec![0])); // Splash                                                    
                                                                                
        let outcomes_t1 = simulate_turn(                                      
            &MatchState::BattleState(initial_state),                          
            &p1_cmd_t1,                                                       
            &p2_cmd_t1,                                                       
            &move_dex,                                                        
            &pokemon_dex,                                                     
            false,                                                            
            1,                                                                
        );                                                                    
                                                                                
        assert!(!outcomes_t1.is_empty());                                     
        let total_probability_t1: f64 = outcomes_t1.iter().map(|(_, p)|       *p).sum();                                                                  
        assert!((total_probability_t1 - 1.0).abs() < 1e-9);                   
                                                                                
        let (MatchState::BattleState(state_t1), p_t1) = outcomes_t1.into_iter().next().unwrap() else {panic!("BattleState not returned")};      
        assert!((p_t1 - 1.0).abs() < 1e-9);                                   
                                                                                        
        let state_t1: &BattleState = &state_t1;                                
                                                                                
        // Check if Swords Dance was applied (Attack stat increase for P1)                    
        assert_eq!(state_t1.p1_active_mons[0].boosts[0], 2);           
                                                                                
        // Check for state changes                                            
        assert!(state_t1.turn_number == 1);                                   
                                                                                
        // Get the state after Turn 1                                         
        let state_after_t1 = state_t1.clone();                                
                                                                                
                                                                                
        // --- START OF TURN 2: Shadow Claw (Mimikyu) vs Splash (Aerodactyl)                
                                                                                
        // Commands for Turn 2                                                
        let p1_cmd_t2 = PlayerCommand::Battle(simple_attack(Player::P1, vec![3])); //Shadow Claw                                
        let p2_cmd_t2 = PlayerCommand::Battle(simple_attack(Player::P2, vec![0])); //         
                                                                                
        let outcomes_t2 = simulate_turn(                                      
            &MatchState::BattleState(state_after_t1),                         
            &p1_cmd_t2,                                                       
            &p2_cmd_t2,                                                       
            &move_dex,                                                        
            &pokemon_dex,                                                     
            false,                                                            
            1,                                                                
        );                                                                    
                                                                                
        assert!(!outcomes_t2.is_empty());    

        // Final probability check                                            
        let total_probability_t2: f64 = outcomes_t2.iter().map(|(_, p)|*p).sum();                                                                  
        assert!((total_probability_t2 - 1.0).abs() < 1e-9);                                    

        println!("{:?}", outcomes_t2);                                                                                                 
        let final_outcome = outcomes_t2.into_iter().find(|(state, _)| {       
                matches!(state, MatchState::GameOverState { winner: Player::P1   
            })});                                                                   
        assert!(final_outcome.is_some(), "The match should conclude with P1 winning.");
    }
    #[test]                                                                   
    fn speed_boosts() {                                      
        let pokemon_dex = pokemon_dex();                                      
        let move_dex = move_dex();                                            
                                                                                
        let p1_mon = build_pokemon_state(                             
            Species::Tyranitar,                                                 
            &pokemon_dex,                                                     
            &move_dex,                                                        
            None,                                                        
            Some([                                                            
                Some(PokemonMove::DragonDance),                               
                Some(PokemonMove::Superpower),                                    
                None,                                                         
                None,                                
            ]),                                                               
            None,                                                             
            None,                                                             
            Some(Nature::Impish),                                              
            None,                                                             
            None,                                                             
            Some([32, 0, 32, 0, 0, 2]),                                       
            None,                                                             
            true,                                                             
        );                                                                    
                                                                                                            
        let p2_mon = build_pokemon_state(                             
            Species::Tyranitar,                                                 
            &pokemon_dex,                                                     
            &move_dex,                                                        
            None,                                                        
            Some([                                                            
                Some(PokemonMove::Splash),                               
                Some(PokemonMove::Superpower),                                    
                None,                                                         
                None,                                
            ]),                                                               
            None,                                                             
            None,                                                             
            Some(Nature::Impish),                                              
            None,                                                             
            None,                                                             
            Some([32, 0, 32, 0, 0, 2]),                                       
            None,                                                             
            true,                                                             
        );                                                                 
                                                                                
        // --- START OF TURN 1: Swords Dance (Mimikyu) vs Splash (Aerodactyl)                                                                    
                                                                                
        let initial_state = battle_state_from_lists(                          
            vec![p1_mon],                                     
            vec![],                                                           
            vec![p2_mon],                                     
            vec![],                                                           
        );                                                                                              

                                                                                
        // Commands for Turn 1                                                
        let p1_cmd_t1 = PlayerCommand::Battle(simple_attack(Player::P1, vec![0])); // Dragon Dance                                                            
        let p2_cmd_t1 = PlayerCommand::Battle(simple_attack(Player::P2, vec![0])); // Splash                                                    
                                                                                
        let outcomes_t1 = simulate_turn(                                      
            &MatchState::BattleState(initial_state),                          
            &p1_cmd_t1,                                                       
            &p2_cmd_t1,                                                       
            &move_dex,                                                        
            &pokemon_dex,                                                     
            false,                                                            
            1,                                                                
        );

        assert!(!outcomes_t1.is_empty());
        let total_probability_t1: f64 = outcomes_t1.iter().map(|(_, p)|*p).sum();                                                                  
        assert!((total_probability_t1 - 1.0).abs() < 1e-9);                   
                                                                                
        let (MatchState::BattleState(state_t1), p_t1) = outcomes_t1.into_iter().next().unwrap() else {panic!("BattleState not returned")};      
        assert!((p_t1 - 1.0).abs() < 1e-9);                                   
                                                                                        
        let state_t1: &BattleState = &state_t1;                                
                                                                                
        // Check for stat increases                    
        assert_eq!(state_t1.p1_active_mons[0].boosts[0], 1);  
        assert_eq!(state_t1.p1_active_mons[0].boosts[4], 1);           
                                                                                
        // Check for state changes                                            
        assert!(state_t1.turn_number == 1);                                   
                                                                                
        // Get the state after Turn 1                                         
        let state_after_t1 = state_t1.clone();                                
                                                                                
                                                                                
        // --- START OF TURN 2: Shadow Claw (Mimikyu) vs Splash (Aerodactyl)                
                                                                                
        // Commands for Turn 2                                                
        let p1_cmd_t2 = PlayerCommand::Battle(simple_attack(Player::P1, vec![1])); //Superpower                                
        let p2_cmd_t2 = PlayerCommand::Battle(simple_attack(Player::P2, vec![1])); //Superpower
                                                                                
        let outcomes_t2 = simulate_turn(                                      
            &MatchState::BattleState(state_after_t1),                         
            &p1_cmd_t2,                                                       
            &p2_cmd_t2,                                                       
            &move_dex,                                                        
            &pokemon_dex,                                                     
            false,                                                            
            1,                                                                
        );                                                                    
                                                                                
        assert!(!outcomes_t2.is_empty());    

        // Final probability check                                            
        let total_probability_t2: f64 = outcomes_t2.iter().map(|(_, p)|*p).sum();                                                                  
        assert!((total_probability_t2 - 1.0).abs() < 1e-9);                                    

        println!("{:?}", outcomes_t2);                                                                                                 
        let final_outcome = outcomes_t2.into_iter().find(|(state, _)| {       
                matches!(state, MatchState::GameOverState { winner: Player::P1   
            })});                                                                   
        assert!(final_outcome.is_some(), "The match should conclude with P1 winning.");
    }
    #[test]
    fn defense_boosts() {
        let pokemon_dex= pokemon_dex();
        let move_dex = move_dex();

        let p1_mon = build_pokemon_state(                             
            Species::Tyranitar,                                                 
            &pokemon_dex,                                                     
            &move_dex,                                                        
            None,                                                        
            Some([                                                            
                Some(PokemonMove::Splash),                               
                Some(PokemonMove::Superpower),                                    
                None,                                                         
                None,                                
            ]),                                                               
            None,                                                             
            None,                                                             
            Some(Nature::Adamant),                                              
            None,                                                             
            None,                                                             
            Some([2, 32, 0, 0, 0, 32]),                                       
            None,                                                             
            true,                                                             
        );  

        let p2_mon = build_pokemon_state(                             
            Species::Aggron,                                                 
            &pokemon_dex,                                                     
            &move_dex,                                                        
            None,                                                        
            Some([                                                            
                Some(PokemonMove::IronDefense),                               
                Some(PokemonMove::Splash),                                    
                None,                                                         
                None,                                
            ]),                                                               
            None,                                                             
            None,                                                             
            Some(Nature::Impish),                                              
            None,                                                             
            None,                                                             
            Some([32, 0, 32, 0, 0, 2]),                                       
            None,                                                             
            true,                                                             
        );  

        let initial_state = battle_state_from_lists(vec![p1_mon], vec![], vec![p2_mon], vec![]);
        let before_state = initial_state.clone();


        let p1_cmd = PlayerCommand::Battle(simple_attack(Player::P1, vec![0]));//Splash
        let p2_cmd = PlayerCommand::Battle(simple_attack(Player::P2, vec![0]));//Iron Defense

        let outcomes_t1 = simulate_turn(
            &MatchState::BattleState(initial_state),
            &p1_cmd,
            &p2_cmd,
            &move_dex,
            &pokemon_dex,
            false,
            16,
        );

        assert!(!outcomes_t1.is_empty());
        let total_probability_t1: f64 = outcomes_t1.iter().map(|(_, p)|*p).sum();
        assert!((total_probability_t1 - 1.0).abs() < 1e-9);

        let (MatchState::BattleState(state_t1), p_t1) = outcomes_t1.into_iter().next().unwrap() else {panic!("BattleState not returned")};

        assert_eq!(p_t1, 1.0);
        assert_eq!(state_t1.p2_active_mons[0].boosts[1], 2);
        let state_after_t1 = state_t1.clone();

        let p1_cmd_2 = PlayerCommand::Battle(simple_attack(Player::P1, vec![1]));//Superpower
        let p2_cmd_2 = PlayerCommand::Battle(simple_attack(Player::P2, vec![1]));//Splash

        let outcomes = simulate_turn(
            &MatchState::BattleState(state_t1),
            &p1_cmd_2,
            &p2_cmd_2,
            &move_dex,
            &pokemon_dex,
            false,
            16,
        );

        let mut expected_final_state = state_after_t1.clone();
        expected_final_state.p1_active_mons[0].move_pp[1] -= 1;
        expected_final_state.p2_active_mons[0].move_pp[1] -= 1;
        expected_final_state.p1_active_mons[0].boosts[0] -= 1;
        expected_final_state.p1_active_mons[0].boosts[1] -= 1;
        expected_final_state.turn_number += 1;

        let mut expected_outcomes: Vec<(MatchState,f64)> = Vec::new();

        let possible_rolls = vec![76, 76, 80, 80, 80, 80, 80, 84, 84, 84, 84, 88, 88, 88, 88, 92];
        
        let mut counted_rolls: HashMap<u16, usize> = HashMap::new();
        for &roll in &possible_rolls {
            *counted_rolls.entry(roll).or_insert(0) += 1;
        }        

        for (damage_roll, counts) in counted_rolls {
            let mut outcome = expected_final_state.clone();
            outcome.p2_active_mons[0].hp -= damage_roll;
            expected_outcomes.push((MatchState::BattleState(outcome), (counts as f64) / 16.0));
        }

        println!("Got:{:?}", outcomes);
        println!("Expected:{:?}", expected_outcomes);
        assert!(is_permutation(&outcomes, &expected_outcomes));
    }
    #[test]
    fn doubles_spread_damage() {
        let pokemon_dex= pokemon_dex();
        let move_dex = move_dex();

        let garchomp = build_pokemon_state(
            Species::Garchomp,
            &pokemon_dex,
            &move_dex,
            None,
            Some([Some(PokemonMove::Earthquake), None, None, None]),
            None,
            None,
            Some(Nature::Adamant),
            None,
            None,
            Some([2, 32, 0, 0, 0, 32]),
            None,
            true,
        );

        let clefable = build_pokemon_state(
            Species::Clefable,
            &pokemon_dex,
            &move_dex,
            None,
            Some([Some(PokemonMove::Splash), None, None, None]),
            None,
            None,
            Some(Nature::Modest),
            None,
            None,
            Some([2, 0, 0, 32, 0, 32]),
            None,
            true,
        );

        let corviknight = build_pokemon_state(
            Species::Corviknight,
            &pokemon_dex,
            &move_dex,
            None,
            Some([Some(PokemonMove::Splash), None, None, None]),
            None,
            None,
            Some(Nature::Adamant),
            None,
            None,
            Some([2, 32, 0, 0, 0, 32]),
            None,
            true,
        );

        let initial_state = battle_state_from_lists(vec![garchomp.clone(), corviknight.clone()], vec![], vec![clefable.clone(), corviknight.clone()], vec![]);
        let before_state = initial_state.clone();


        let p1_cmd = PlayerCommand::Battle(simple_attack(Player::P1, vec![0, 0]));//Earthquake, Splash
        let p2_cmd = PlayerCommand::Battle(simple_attack(Player::P2, vec![0, 0]));//Splash, Splash

        let outcomes = simulate_turn(
            &MatchState::BattleState(initial_state),
            &p1_cmd,
            &p2_cmd,
            &move_dex,
            &pokemon_dex,
            false,
            16,
        );

        assert!(!outcomes.is_empty());
        let total_probability: f64 = outcomes.iter().map(|(_, p)| *p).sum();
        assert!((total_probability - 1.0).abs() < 1e-9);

        let mut expected_final_state = before_state.clone();
        expected_final_state.p1_active_mons[0].move_pp[0] -= 1;
        expected_final_state.p2_active_mons[0].move_pp[0] -= 1;
        expected_final_state.p1_active_mons[1].move_pp[0] -= 1;
        expected_final_state.p2_active_mons[1].move_pp[0] -= 1;
        expected_final_state.turn_number += 1;

        let mut expected_outcomes: Vec<(MatchState,f64)> = Vec::new();

        let possible_rolls = vec![91, 91, 93, 94, 96, 96, 97, 99, 99, 100, 102, 103, 103, 105, 106, 108];
        
        let mut counted_rolls: HashMap<u16, usize> = HashMap::new();
        for &roll in &possible_rolls {
            *counted_rolls.entry(roll).or_insert(0) += 1;
        }        

        for (damage_roll, counts) in counted_rolls {
            let mut outcome = expected_final_state.clone();
            outcome.p2_active_mons[0].hp -= damage_roll;
            expected_outcomes.push((MatchState::BattleState(outcome), (counts as f64) / 16.0));
        }

        println!("{:?}", outcomes);
        assert!(is_permutation(&outcomes, &expected_outcomes));
    }

    #[test]
    fn doubles_rock_slide() {
        let pokemon_dex = pokemon_dex();
        let move_dex = move_dex();

        let tyranitar = build_pokemon_state(
            Species::Tyranitar,
            &pokemon_dex,
            &move_dex,
            None,
            Some([Some(PokemonMove::RockSlide), None, None, None]),
            None,
            None,
            Some(Nature::Adamant),
            None,
            None,
            Some([32, 32, 0, 0, 0, 32]),
            None,
            true,
        );

        let magikarp = build_pokemon_state(
            Species::Magikarp,
            &pokemon_dex,
            &move_dex,
            None,
            Some([Some(PokemonMove::Splash), None, None, None]),
            None,
            None,
            Some(Nature::Modest),
            None,
            None,
            Some([2, 0, 0, 32, 0, 32]),
            None,
            true,
        );

        let corviknight = build_pokemon_state(
            Species::Corviknight,
            &pokemon_dex,
            &move_dex,
            None,
            Some([Some(PokemonMove::Splash), None, None, None]),
            None,
            None,
            Some(Nature::Brave),
            None,
            None,
            Some([2, 32, 0, 0, 0, 0]),
            None,
            true,
        );

        let initial_state = battle_state_from_lists(vec![tyranitar.clone(), magikarp.clone()], vec![], vec![corviknight.clone(), corviknight.clone()], vec![]);
        let before_state = initial_state.clone();


        let p1_cmd = PlayerCommand::Battle(simple_attack(Player::P1, vec![0, 0])); // Rock Slide, Splash
        let p2_cmd = PlayerCommand::Battle(simple_attack(Player::P2, vec![0, 0])); // Splash, Splash

        let outcomes = simulate_turn(
            &MatchState::BattleState(initial_state),
            &p1_cmd,
            &p2_cmd,
            &move_dex,
            &pokemon_dex,
            true,
            16,
        );

        assert!(!outcomes.is_empty());
        let total_probability: f64 = outcomes.iter().map(|(_, p)| *p).sum();
        assert!((total_probability - 1.0).abs() < 1e-9);

        let mut expected_final_state = before_state.clone();
        expected_final_state.p1_active_mons[0].move_pp[0] -= 1;
        expected_final_state.turn_number += 1;

        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        enum TargetOutcome {
            Miss,
            HitNoFlinch,
            HitFlinch,
        }

        let classify = |initial_hp: u16, final_hp: u16, final_pp: u8| -> TargetOutcome {
            let took_damage = final_hp < initial_hp;
            let consumed_pp = final_pp == 19;

            match (took_damage, consumed_pp) {
                (false, true) => TargetOutcome::Miss,
                (true, true) => TargetOutcome::HitNoFlinch,
                (true, false) => TargetOutcome::HitFlinch,
                _ => panic!("Invalid Rock Slide branch: damage={took_damage}, pp={final_pp}"),
            }
        };

        let mut joint: HashMap<(TargetOutcome, TargetOutcome), f64> = HashMap::new();
        let mut p_target_0: HashMap<TargetOutcome, f64> = HashMap::new();
        let mut p_target_1: HashMap<TargetOutcome, f64> = HashMap::new();

        let mut dmg_0_flinch: HashMap<u16, f64> = HashMap::new();
        let mut dmg_0_no_flinch: HashMap<u16, f64> = HashMap::new();
        let mut dmg_1_flinch: HashMap<u16, f64> = HashMap::new();
        let mut dmg_1_no_flinch: HashMap<u16, f64> = HashMap::new();

        for (state, probability) in &outcomes {
            let MatchState::BattleState(bs) = state else {
                panic!("Unexpected non-battle branch in doubles_rock_slide");
            };

            assert_eq!(bs.turn_number, expected_final_state.turn_number);
            assert_eq!(bs.p1_active_mons[0].move_pp[0], expected_final_state.p1_active_mons[0].move_pp[0]);

            let initial_hp_0 = before_state.p2_active_mons[0].hp;
            let initial_hp_1 = before_state.p2_active_mons[1].hp;

            let outcome_0 = classify(initial_hp_0, bs.p2_active_mons[0].hp, bs.p2_active_mons[0].move_pp[0]);
            let outcome_1 = classify(initial_hp_1, bs.p2_active_mons[1].hp, bs.p2_active_mons[1].move_pp[0]);

            *joint.entry((outcome_0, outcome_1)).or_insert(0.0) += *probability;
            *p_target_0.entry(outcome_0).or_insert(0.0) += *probability;
            *p_target_1.entry(outcome_1).or_insert(0.0) += *probability;

            let damage_0 = initial_hp_0.saturating_sub(bs.p2_active_mons[0].hp);
            let damage_1 = initial_hp_1.saturating_sub(bs.p2_active_mons[1].hp);

            match outcome_0 {
                TargetOutcome::HitFlinch => {
                    *dmg_0_flinch.entry(damage_0).or_insert(0.0) += *probability;
                }
                TargetOutcome::HitNoFlinch => {
                    *dmg_0_no_flinch.entry(damage_0).or_insert(0.0) += *probability;
                }
                TargetOutcome::Miss => {}
            }

            match outcome_1 {
                TargetOutcome::HitFlinch => {
                    *dmg_1_flinch.entry(damage_1).or_insert(0.0) += *probability;
                }
                TargetOutcome::HitNoFlinch => {
                    *dmg_1_no_flinch.entry(damage_1).or_insert(0.0) += *probability;
                }
                TargetOutcome::Miss => {}
            }
        }

        let get_prob = |map: &HashMap<TargetOutcome, f64>, key: TargetOutcome| -> f64 {
            *map.get(&key).unwrap_or(&0.0)
        };

        let miss_p = 0.1;
        let hit_no_flinch_p = 0.9 * 0.7;
        let hit_flinch_p = 0.9 * 0.3;

        let eps = 1e-9;

        assert!((get_prob(&p_target_0, TargetOutcome::Miss) - miss_p).abs() < eps);
        assert!((get_prob(&p_target_0, TargetOutcome::HitNoFlinch) - hit_no_flinch_p).abs() < eps);
        assert!((get_prob(&p_target_0, TargetOutcome::HitFlinch) - hit_flinch_p).abs() < eps);

        assert!((get_prob(&p_target_1, TargetOutcome::Miss) - miss_p).abs() < eps);
        assert!((get_prob(&p_target_1, TargetOutcome::HitNoFlinch) - hit_no_flinch_p).abs() < eps);
        assert!((get_prob(&p_target_1, TargetOutcome::HitFlinch) - hit_flinch_p).abs() < eps);

        for first in [TargetOutcome::Miss, TargetOutcome::HitNoFlinch, TargetOutcome::HitFlinch] {
            for second in [TargetOutcome::Miss, TargetOutcome::HitNoFlinch, TargetOutcome::HitFlinch] {
                let joint_prob = *joint.get(&(first, second)).unwrap_or(&0.0);
                let expected_joint = get_prob(&p_target_0, first) * get_prob(&p_target_1, second);
                assert!((joint_prob - expected_joint).abs() < eps);
            }
        }

        let normalize = |dist: &HashMap<u16, f64>, total: f64| -> HashMap<u16, f64> {
            dist.iter().map(|(d, p)| (*d, *p / total)).collect()
        };

        let norm_0_flinch = normalize(&dmg_0_flinch, get_prob(&p_target_0, TargetOutcome::HitFlinch));
        let norm_0_no_flinch = normalize(&dmg_0_no_flinch, get_prob(&p_target_0, TargetOutcome::HitNoFlinch));
        let norm_1_flinch = normalize(&dmg_1_flinch, get_prob(&p_target_1, TargetOutcome::HitFlinch));
        let norm_1_no_flinch = normalize(&dmg_1_no_flinch, get_prob(&p_target_1, TargetOutcome::HitNoFlinch));

        for (damage, p) in &norm_0_flinch {
            assert!((p - norm_0_no_flinch.get(damage).unwrap_or(&0.0)).abs() < eps);
            assert!((p - norm_1_flinch.get(damage).unwrap_or(&0.0)).abs() < eps);
            assert!((p - norm_1_no_flinch.get(damage).unwrap_or(&0.0)).abs() < eps);
        }
    }
    #[test]
    fn multiturn_dig() {
        let pokemon_dex= pokemon_dex();
        let move_dex = move_dex();

        let excadrill = build_pokemon_state(
            Species::Excadrill,
            &pokemon_dex,
            &move_dex,
            None,
            Some([Some(PokemonMove::Dig), None, None, None]),
            None,
            None,
            Some(Nature::Adamant),
            None,
            None,
            Some([2, 32, 0, 0, 0, 32]),
            None,
            true,
        );

        let snorlax = build_pokemon_state(
            Species::Snorlax,
            &pokemon_dex,
            &move_dex,
            None,
            Some([Some(PokemonMove::Splash), Some(PokemonMove::BodySlam), None, None]),
            None,
            None,
            Some(Nature::Modest),
            None,
            None,
            Some([32, 32, 0, 0, 0, 2]),
            None,
            true,
        );

        let initial_state = battle_state_from_lists(vec![excadrill], vec![], vec![snorlax], vec![]);


        let p1_cmd = PlayerCommand::Battle(simple_attack(Player::P1, vec![0]));//Dig
        let p2_cmd = PlayerCommand::Battle(simple_attack(Player::P2, vec![1]));//BodySlam

        let outcomes_t1 = simulate_turn(
            &MatchState::BattleState(initial_state),
            &p1_cmd,
            &p2_cmd,
            &move_dex,
            &pokemon_dex,
            false,
            1,
        );

        assert!(!outcomes_t1.is_empty());
        let total_probability_t1: f64 = outcomes_t1.iter().map(|(_, p)|*p).sum();
        assert!((total_probability_t1 - 1.0).abs() < 1e-9);

        let (MatchState::BattleState(state_t1), p_t1) = outcomes_t1.into_iter().next().unwrap() else {panic!("BattleState not returned")};

        assert_eq!(p_t1, 1.0);
        assert_eq!(state_t1.p1_active_mons[0].hp, 187);//Assert the attack was avoided
        let state_after_t1 = state_t1.clone();
        println!("{:?}", state_t1);

        let p1_cmd_2 = PlayerCommand::Battle(simple_attack(Player::P1, vec![0]));//Dig
        let p2_cmd_2 = PlayerCommand::Battle(simple_attack(Player::P2, vec![0]));//Splash

        let outcomes = simulate_turn(
            &MatchState::BattleState(state_t1),
            &p1_cmd_2,
            &p2_cmd_2,
            &move_dex,
            &pokemon_dex,
            false,
            1,
        );

        let mut expected_final_state = state_after_t1.clone();
        expected_final_state.p1_active_mons[0].move_pp[0] -= 1;
        expected_final_state.p1_active_mons[0].volatiles = Vec::new();//Make sure semi-invulnerable volatile is gone
        expected_final_state.p2_active_mons[0].move_pp[0] -= 1;
        expected_final_state.turn_number += 1;

        let mut expected_outcomes: Vec<(MatchState,f64)> = Vec::new();
        
        let mut outcome = expected_final_state.clone();
        outcome.p2_active_mons[0].hp -= 118;
        expected_outcomes.push((MatchState::BattleState(outcome), 1.0));

        println!("Got:{:?}", outcomes);
        println!("Expected:{:?}", expected_outcomes);
        assert!(is_permutation(&outcomes, &expected_outcomes));
    }

    #[test]
    fn dig_earthquake() {
        let pokemon_dex= pokemon_dex();
        let move_dex = move_dex();

        let excadrill = build_pokemon_state(
            Species::Excadrill,
            &pokemon_dex,
            &move_dex,
            None,
            Some([Some(PokemonMove::Dig), None, None, None]),
            None,
            None,
            Some(Nature::Adamant),
            None,
            None,
            Some([2, 32, 0, 0, 0, 32]),
            None,
            true,
        );

        let snorlax = build_pokemon_state(
            Species::Snorlax,
            &pokemon_dex,
            &move_dex,
            None,
            Some([Some(PokemonMove::Splash), Some(PokemonMove::Earthquake), None, None]),
            None,
            None,
            Some(Nature::Adamant),
            None,
            None,
            Some([32, 32, 0, 0, 0, 2]),
            None,
            true,
        );

        let initial_state = battle_state_from_lists(vec![excadrill], vec![], vec![snorlax], vec![]);


        let p1_cmd = PlayerCommand::Battle(simple_attack(Player::P1, vec![0]));//Dig
        let p2_cmd = PlayerCommand::Battle(simple_attack(Player::P2, vec![1]));//Earthquake

        let outcomes = simulate_turn(
            &MatchState::BattleState(initial_state),
            &p1_cmd,
            &p2_cmd,
            &move_dex,
            &pokemon_dex,
            false,
            1,
        );

        assert!(!outcomes.is_empty());
        let total_probability_t1: f64 = outcomes.iter().map(|(_, p)|*p).sum();
        assert!((total_probability_t1 - 1.0).abs() < 1e-9);

        let (MatchState::GameOverState{winner:player}, p_t1) = outcomes.clone().into_iter().next().unwrap() else {panic!("BattleState not returned")};

        assert_eq!(p_t1, 1.0);

        println!("Got:{:?}", outcomes);
        assert_eq!(player, Player::P2);
    }
    #[test]
    fn multiturn_fly() {
        let pokemon_dex= pokemon_dex();
        let move_dex = move_dex();

        let dragonite = build_pokemon_state(
            Species::Dragonite,
            &pokemon_dex,
            &move_dex,
            None,
            Some([Some(PokemonMove::Fly), None, None, None]),
            None,
            None,
            Some(Nature::Adamant),
            None,
            None,
            Some([2, 32, 0, 0, 0, 32]),
            None,
            true,
        );

        let snorlax = build_pokemon_state(
            Species::Snorlax,
            &pokemon_dex,
            &move_dex,
            None,
            Some([Some(PokemonMove::Splash), Some(PokemonMove::BodySlam), None, None]),
            None,
            None,
            Some(Nature::Modest),
            None,
            None,
            Some([32, 32, 0, 0, 0, 2]),
            None,
            true,
        );

        let initial_state = battle_state_from_lists(vec![dragonite], vec![], vec![snorlax], vec![]);


        let p1_cmd = PlayerCommand::Battle(simple_attack(Player::P1, vec![0]));//Dig
        let p2_cmd = PlayerCommand::Battle(simple_attack(Player::P2, vec![1]));//BodySlam

        let outcomes_t1 = simulate_turn(
            &MatchState::BattleState(initial_state),
            &p1_cmd,
            &p2_cmd,
            &move_dex,
            &pokemon_dex,
            false,
            1,
        );

        assert!(!outcomes_t1.is_empty());
        let total_probability_t1: f64 = outcomes_t1.iter().map(|(_, p)|*p).sum();
        assert!((total_probability_t1 - 1.0).abs() < 1e-9);

        let (MatchState::BattleState(state_t1), p_t1) = outcomes_t1.into_iter().next().unwrap() else {panic!("BattleState not returned")};

        assert_eq!(p_t1, 1.0);
        assert_eq!(state_t1.p1_active_mons[0].hp, 168);//Assert the attack was avoided
        let state_after_t1 = state_t1.clone();
        println!("{:?}", state_t1);

        let p1_cmd_2 = PlayerCommand::Battle(simple_attack(Player::P1, vec![0]));//Fly
        let p2_cmd_2 = PlayerCommand::Battle(simple_attack(Player::P2, vec![0]));//Splash

        let outcomes = simulate_turn(
            &MatchState::BattleState(state_t1),
            &p1_cmd_2,
            &p2_cmd_2,
            &move_dex,
            &pokemon_dex,
            false,
            1,
        );

        let mut expected_final_state = state_after_t1.clone();
        expected_final_state.p1_active_mons[0].move_pp[0] -= 1;
        expected_final_state.p1_active_mons[0].volatiles = Vec::new();//Make sure semi-invulnerable volatile is gone
        expected_final_state.p2_active_mons[0].move_pp[0] -= 1;
        expected_final_state.turn_number += 1;

        let mut expected_outcomes: Vec<(MatchState,f64)> = Vec::new();
        
        let mut outcome = expected_final_state.clone();
        outcome.p2_active_mons[0].hp -= 133;
        expected_outcomes.push((MatchState::BattleState(outcome), 0.95));
        expected_outcomes.push((MatchState::BattleState(expected_final_state), 1.0 - 0.95));//Fly miss

        println!("Got:{:?}", outcomes);
        println!("Expected:{:?}", expected_outcomes);
        assert!(is_permutation(&outcomes, &expected_outcomes));
    }

    /*Tests to write:
    Multi-turn moves (especially sky drop interactions)
    Mega Evolution Damage
    Adaptability
    Weather causing abilties AND moves
    Weather effects (Fire damage boost in sun, sand spdef boost, sand damage)
    Weather-enabled Abilities (Swift swim, dry skin)
    Mega Evolution Abilities (Mega Tyranitar)
    Status effects (manually apply the status, then check for its effects)
    Sleep Talk :)
    */
}
