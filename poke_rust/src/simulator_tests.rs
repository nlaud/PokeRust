#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::atomic::Ordering;
    use crate::battle::{Action, MatchState, MoveAction, PlayerCommand, SwitchCommand};
    use crate::battle::{BattleCommand, BattleState, FieldSlot, Player};
    use crate::data::ability::Ability;
    use crate::data::item::Item;
    use crate::data::pokemon_move::PokemonMove;
    use crate::data::species::Species;
    use crate::dex_data::{PseudoWeather, Status, Terrain, VolatileStatus, Weather};
    use crate::pokemon::{build_pokemon_state, Nature, PokemonState, VolatileStatusState};
    use crate::simulator::{simulate_turn, DamageConfig};
    use crate::simulator_helpers;
    use crate::simulator_helpers::coalesce_branches;
    use crate::simuilator_test_helpers::{
        assert_distribution_close,
        battle_state_from_lists,
        combine_hit_distributions_with_hit_chances,
        confusion_turns,
        damage_distribution,
        extract_battle_state,
        has_charging_volatile,
        has_sky_drop_move_volatile,
        has_sky_drop_turn_volatile,
        hit_probability,
        is_permutation,
        move_dex,
        pokemon_dex,
        repeat_hit_distribution,
        run_single_turn,
        simple_attack,
        simple_attack_mega,
    };

    // Sum outcome probabilities by the resulting non-volatile status on the P2 target.
    fn status_distribution(outcomes: &[(MatchState, f64)]) -> std::collections::HashMap<&'static str, f64> {
        let mut dist = std::collections::HashMap::new();
        for (state, probability) in outcomes {
            if let MatchState::BattleState(bs) = state {
                let key = match bs.p2_active_mons[0].status {
                    None => "none",
                    Some(Status::Burn) => "brn",
                    Some(Status::Frozen(_)) => "frz",
                    Some(Status::Paralysis) => "par",
                    Some(Status::Poison) => "psn",
                    Some(Status::Sleep(_)) => "slp",
                    Some(Status::ToxicPoison(_)) => "tox",
                };
                *dist.entry(key).or_insert(0.0) += *probability;
            }
        }
        dist
    }

    mod smoke {
        use super::*;

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
    }

    mod multi_hit {
        use super::*;

        #[test]
        fn surging_strikes_shared_roll_flag_changes_damage_distribution() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let urshifu = build_pokemon_state(
                Species::UrshifuRapidStrike,
                &pokemon_dex,
                &move_dex,
                Some(50),
                Some([Some(PokemonMove::SurgingStrikes), Some(PokemonMove::Splash), None, None]),
                None,
                Some(Ability::None),
                Some(Nature::Hardy),
                None,
                None,
                Some([0, 0, 0, 0, 0, 0]),
                Some([31, 31, 31, 31, 31, 31]),
                false,
            );

            let altaria = build_pokemon_state(
                Species::Altaria,
                &pokemon_dex,
                &move_dex,
                Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None,
                Some(Ability::None),
                Some(Nature::Hardy),
                None,
                None,
                Some([0, 0, 0, 0, 0, 0]),
                Some([31, 31, 31, 31, 31, 31]),
                false,
            );

            let initial_state = battle_state_from_lists(vec![urshifu.clone()], vec![], vec![altaria.clone()], vec![]);
            let initial_hp = altaria.hp;
            let attack_slot = FieldSlot { player: Player::P1, slot_index: 0 };
            let target_slot = FieldSlot { player: Player::P2, slot_index: 0 };
            let attacker = crate::simulator_helpers::get_pokemon_at_slot(&initial_state, attack_slot).unwrap();
            let target = crate::simulator_helpers::get_pokemon_at_slot(&initial_state, target_slot).unwrap();
            let single_hit = crate::simulator_helpers::calculate_damage_outcomes_for_target(
                &initial_state,
                attacker,
                target,
                attack_slot,
                target_slot,
                move_dex.get(&PokemonMove::SurgingStrikes).unwrap(),
                DamageConfig { consider_crit: false, damage_rolls: 16 },
                1.0,
                1.0,
            )
            .into_iter()
            .map(|(damage, _, probability)| (damage, probability))
            .collect::<Vec<_>>();

            let p1_cmd = PlayerCommand::Battle(simple_attack(Player::P1, vec![0]));
            let p2_cmd = PlayerCommand::Battle(simple_attack(Player::P2, vec![0]));

            crate::SHARED_MULTIHIT_DAMAGE_ROLLS.store(false, Ordering::Relaxed);
            let independent_outcomes = simulate_turn(&MatchState::BattleState(initial_state.clone()), &p1_cmd, &p2_cmd, &move_dex, &pokemon_dex, false, 16);

            crate::SHARED_MULTIHIT_DAMAGE_ROLLS.store(true, Ordering::Relaxed);
            let shared_outcomes = simulate_turn(&MatchState::BattleState(initial_state), &p1_cmd, &p2_cmd, &move_dex, &pokemon_dex, false, 16);
            crate::SHARED_MULTIHIT_DAMAGE_ROLLS.store(false, Ordering::Relaxed);

            let expected_independent = repeat_hit_distribution(&single_hit, 3);
            let mut expected_shared = HashMap::new();
            for (damage, probability) in &single_hit {
                *expected_shared.entry(damage.saturating_mul(3)).or_insert(0.0) += probability;
            }

            assert_distribution_close(damage_distribution(&independent_outcomes, initial_hp), expected_independent);
            assert_distribution_close(damage_distribution(&shared_outcomes, initial_hp), expected_shared);
        }

        #[test]
        fn triple_axel_branches_each_hit_with_progressive_power() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let tsareena = build_pokemon_state(
                Species::Tsareena,
                &pokemon_dex,
                &move_dex,
                Some(50),
                Some([Some(PokemonMove::TripleAxel), Some(PokemonMove::Splash), None, None]),
                None,
                Some(Ability::NoGuard),
                Some(Nature::Hardy),
                None,
                None,
                Some([0, 0, 0, 0, 0, 0]),
                Some([31, 31, 31, 31, 31, 31]),
                false,
            );

            let venusaur = build_pokemon_state(
                Species::Venusaur,
                &pokemon_dex,
                &move_dex,
                Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None,
                Some(Ability::None),
                Some(Nature::Hardy),
                None,
                None,
                Some([0, 0, 0, 0, 0, 0]),
                Some([31, 31, 31, 31, 31, 31]),
                false,
            );

            let initial_state = battle_state_from_lists(vec![tsareena], vec![], vec![venusaur.clone()], vec![]);
            let initial_hp = venusaur.hp;
            let attack_slot = FieldSlot { player: Player::P1, slot_index: 0 };
            let target_slot = FieldSlot { player: Player::P2, slot_index: 0 };
            let attacker = crate::simulator_helpers::get_pokemon_at_slot(&initial_state, attack_slot).unwrap();
            let target = crate::simulator_helpers::get_pokemon_at_slot(&initial_state, target_slot).unwrap();
            let hit_chance = crate::simulator_helpers::accuracy_hit_probability(
                &initial_state,
                attacker,
                target,
                attack_slot,
                target_slot,
                move_dex.get(&PokemonMove::TripleAxel).unwrap(),
            );
            let hit1 = crate::simulator_helpers::calculate_damage_outcomes_for_target_with_options(
                &initial_state,
                attacker,
                target,
                attack_slot,
                target_slot,
                move_dex.get(&PokemonMove::TripleAxel).unwrap(),
                DamageConfig { consider_crit: false, damage_rolls: 16 },
                1.0,
                1.0,
                Some(20),
                None,
            )
            .into_iter()
            .map(|(damage, _, probability)| (damage, probability))
            .collect::<Vec<_>>();
            let hit2 = crate::simulator_helpers::calculate_damage_outcomes_for_target_with_options(
                &initial_state,
                attacker,
                target,
                attack_slot,
                target_slot,
                move_dex.get(&PokemonMove::TripleAxel).unwrap(),
                DamageConfig { consider_crit: false, damage_rolls: 16 },
                1.0,
                1.0,
                Some(40),
                None,
            )
            .into_iter()
            .map(|(damage, _, probability)| (damage, probability))
            .collect::<Vec<_>>();
            let hit3 = crate::simulator_helpers::calculate_damage_outcomes_for_target_with_options(
                &initial_state,
                attacker,
                target,
                attack_slot,
                target_slot,
                move_dex.get(&PokemonMove::TripleAxel).unwrap(),
                DamageConfig { consider_crit: false, damage_rolls: 16 },
                1.0,
                1.0,
                Some(60),
                None,
            )
            .into_iter()
            .map(|(damage, _, probability)| (damage, probability))
            .collect::<Vec<_>>();

            let p1_cmd = PlayerCommand::Battle(simple_attack(Player::P1, vec![0]));
            let p2_cmd = PlayerCommand::Battle(simple_attack(Player::P2, vec![0]));

            let outcomes = simulate_turn(&MatchState::BattleState(initial_state), &p1_cmd, &p2_cmd, &move_dex, &pokemon_dex, false, 16);

            let expected = combine_hit_distributions_with_hit_chances(&[hit1, hit2, hit3], &[hit_chance, hit_chance, hit_chance]);

            assert_distribution_close(damage_distribution(&outcomes, initial_hp), expected);
        }

        #[test]
        fn beat_up_uses_user_attack_stat_multihit_regression() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let mut boosted_tyranitar = build_pokemon_state(
                Species::Tyranitar,
                &pokemon_dex,
                &move_dex,
                Some(50),
                Some([Some(PokemonMove::BeatUp), Some(PokemonMove::Splash), None, None]),
                None,
                Some(Ability::None),
                Some(Nature::Hardy),
                None,
                None,
                Some([0, 0, 0, 0, 0, 0]),
                Some([31, 31, 31, 31, 31, 31]),
                false,
            );
            boosted_tyranitar.boosts[0] = 2;

            let unboosted_tyranitar = {
                let mut mon = boosted_tyranitar.clone();
                mon.boosts[0] = 0;
                mon
            };

            let magikarp = build_pokemon_state(
                Species::Magikarp,
                &pokemon_dex,
                &move_dex,
                Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None,
                Some(Ability::None),
                Some(Nature::Hardy),
                None,
                None,
                Some([0, 0, 0, 0, 0, 0]),
                Some([31, 31, 31, 31, 31, 31]),
                false,
            );

            let bidoof = build_pokemon_state(
                Species::Bidoof,
                &pokemon_dex,
                &move_dex,
                Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None,
                Some(Ability::None),
                Some(Nature::Hardy),
                None,
                None,
                Some([0, 0, 0, 0, 0, 0]),
                Some([31, 31, 31, 31, 31, 31]),
                false,
            );

            let target = build_pokemon_state(
                Species::Altaria,
                &pokemon_dex,
                &move_dex,
                Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None,
                Some(Ability::None),
                Some(Nature::Hardy),
                None,
                None,
                Some([0, 0, 0, 0, 0, 0]),
                Some([31, 31, 31, 31, 31, 31]),
                false,
            );

            let boosted_state = battle_state_from_lists(vec![boosted_tyranitar], vec![magikarp.clone()], vec![target.clone()], vec![bidoof.clone()]);
            let unboosted_state = battle_state_from_lists(vec![unboosted_tyranitar], vec![magikarp], vec![target.clone()], vec![bidoof]);

            let p1_cmd = PlayerCommand::Battle(simple_attack(Player::P1, vec![0, 0]));
            let p2_cmd = PlayerCommand::Battle(simple_attack(Player::P2, vec![0]));

            let boosted_outcomes = simulate_turn(&MatchState::BattleState(boosted_state), &p1_cmd, &p2_cmd, &move_dex, &pokemon_dex, false, 1);
            let unboosted_outcomes = simulate_turn(&MatchState::BattleState(unboosted_state), &p1_cmd, &p2_cmd, &move_dex, &pokemon_dex, false, 1);

            let boosted_damage = boosted_outcomes
                .iter()
                .filter_map(|(state, _)| match state {
                    MatchState::BattleState(bs) => Some(target.hp.saturating_sub(bs.p2_active_mons[0].hp)),
                    _ => None,
                })
                .max()
                .unwrap();

            let unboosted_damage = unboosted_outcomes
                .iter()
                .filter_map(|(state, _)| match state {
                    MatchState::BattleState(bs) => Some(target.hp.saturating_sub(bs.p2_active_mons[0].hp)),
                    _ => None,
                })
                .max()
                .unwrap();

            assert!(boosted_damage > unboosted_damage);
        }
    }

    mod damage_calc {
        use super::*;

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
        fn damage_floor_1() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let attacker = build_pokemon_state(
                Species::Magikarp,
                &pokemon_dex,
                &move_dex,
                Some(1),
                Some([Some(PokemonMove::Tackle), None, None, None]),
                None,
                Some(Ability::None),
                Some(Nature::Modest),
                None,
                None,
                Some([0, 0, 0, 0, 0, 0]),
                Some([0, 0, 0, 0, 0, 0]),
                false,
            );
            let defender = build_pokemon_state(
                Species::Aggron,
                &pokemon_dex,
                &move_dex,
                Some(100),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None,
                Some(Ability::None),
                Some(Nature::Impish),
                None,
                None,
                Some([0, 252, 252, 0, 0, 0]),
                Some([31, 31, 31, 31, 31, 31]),
                false,
            );

            let before_hp = defender.stats[0];
            let outcomes = run_single_turn(
                &MatchState::BattleState(battle_state_from_lists(vec![attacker], vec![], vec![defender], vec![])),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex,
                &pokemon_dex,
            );

            let (state, _) = extract_battle_state(outcomes);
            assert_eq!(before_hp - state.p2_active_mons[0].hp, 1);
        }
    }

    mod critical_hits {
        use super::*;

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
        fn crit_ignores_drops() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let mut p1_mon = build_pokemon_state(
                Species::Kingambit,
                &pokemon_dex,
                &move_dex,
                Some(100),
                Some([Some(PokemonMove::BrickBreak), Some(PokemonMove::Splash), None, None]),
                None,
                None,
                Some(Nature::Adamant),
                None,
                None,
                Some([32, 32, 0, 0, 0, 32]),
                None,
                true,
            );
            p1_mon.boosts[0] = -2;
            p1_mon.status = Some(Status::Burn);

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
                Some([32, 32, 0, 0, 0, 32]),
                None,
                true,
            );

            let initial_state = battle_state_from_lists(vec![p1_mon], vec![], vec![p2_mon], vec![]);

            let outcomes = simulate_turn(
                &MatchState::BattleState(initial_state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex,
                &pokemon_dex,
                true,
                1,
            );

            assert!(!outcomes.is_empty());
            let total_probability: f64 = outcomes.iter().map(|(_, p)| *p).sum();
            assert!((total_probability - 1.0).abs() < 1e-9);

            assert!(outcomes.iter().any(|(state, _)| matches!(state, MatchState::BattleState(bs) if bs.p2_active_mons[0].hp > 0)));
            assert!(outcomes.iter().all(|(state, _)| match state {
                MatchState::BattleState(bs) => bs.p2_active_mons[0].hp > 0,
                MatchState::GameOverState { winner } => *winner == Player::P1,
                _ => false,
            }));
        }

        #[test]
        fn crit_ignores_positive_defense_stages() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let p1_mon = build_pokemon_state(
                Species::Kingambit,
                &pokemon_dex,
                &move_dex,
                Some(100),
                Some([Some(PokemonMove::BrickBreak), Some(PokemonMove::Splash), None, None]),
                None,
                None,
                Some(Nature::Adamant),
                None,
                None,
                Some([32, 32, 0, 0, 0, 32]),
                None,
                true,
            );

            let mut p2_mon = build_pokemon_state(
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
                Some([32, 32, 0, 0, 0, 32]),
                None,
                true,
            );
            p2_mon.boosts[1] = 2;

            let initial_state = battle_state_from_lists(vec![p1_mon], vec![], vec![p2_mon], vec![]);

            let outcomes = simulate_turn(
                &MatchState::BattleState(initial_state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex,
                &pokemon_dex,
                true,
                1,
            );

            assert!(!outcomes.is_empty());
            let total_probability: f64 = outcomes.iter().map(|(_, p)| *p).sum();
            assert!((total_probability - 1.0).abs() < 1e-9);

            assert!(outcomes.iter().any(|(state, _)| matches!(state, MatchState::BattleState(bs) if bs.p2_active_mons[0].hp > 0)));
            assert!(outcomes.iter().all(|(state, _)| match state {
                MatchState::BattleState(bs) => bs.p2_active_mons[0].hp > 0,
                MatchState::GameOverState { winner } => *winner == Player::P1,
                _ => false,
            }));
        }

        #[test]
        fn guaranteed_crit_sources_still_work() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let mut p1_mon = build_pokemon_state(
                Species::Kingambit,
                &pokemon_dex,
                &move_dex,
                Some(100),
                Some([Some(PokemonMove::BrickBreak), Some(PokemonMove::LaserFocus), None, None]),
                None,
                None,
                Some(Nature::Adamant),
                None,
                None,
                Some([32, 32, 0, 0, 0, 32]),
                None,
                true,
            );
            p1_mon.volatiles.push(VolatileStatusState::TurnStatus(VolatileStatus::LaserFocus, 0));

            let p2_mon = build_pokemon_state(
                Species::Snorlax,
                &pokemon_dex,
                &move_dex,
                Some(100),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None,
                None,
                Some(Nature::Hardy),
                None,
                None,
                Some([32, 0, 0, 0, 0, 32]),
                None,
                true,
            );

            let initial_state = battle_state_from_lists(vec![p1_mon], vec![], vec![p2_mon], vec![]);

            let attacker = simulator_helpers::get_pokemon_at_slot(&initial_state, FieldSlot { player: Player::P1, slot_index: 0 }).unwrap();
            let target = simulator_helpers::get_pokemon_at_slot(&initial_state, FieldSlot { player: Player::P2, slot_index: 0 }).unwrap();
            let move_data = move_dex.get(&PokemonMove::KowtowCleave).unwrap();

            let crit_probability = simulator_helpers::critical_hit_probability(
                attacker,
                target,
                &PokemonMove::KowtowCleave,
                true,
                move_data.crit_ratio,
            );
            assert_eq!(crit_probability, vec![(true, 1.0)]);

            let storm_throw_probability = simulator_helpers::critical_hit_probability(
                attacker,
                target,
                &PokemonMove::StormThrow,
                true,
                1,
            );
            assert_eq!(storm_throw_probability, vec![(true, 1.0)]);
        }

        #[test]
        fn sniper_increases_critical_hit_damage() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let make_state = |ability: Ability| {
                let p1_mon = build_pokemon_state(
                    Species::Kingambit,
                    &pokemon_dex,
                    &move_dex,
                    Some(100),
                    Some([Some(PokemonMove::BrickBreak), None, None, None]),
                    None,
                    Some(ability),
                    Some(Nature::Adamant),
                    None,
                    None,
                    Some([32, 32, 0, 0, 0, 32]),
                    None,
                    true,
                );

                let p2_mon = build_pokemon_state(
                    Species::Snorlax,
                    &pokemon_dex,
                    &move_dex,
                    Some(100),
                    Some([Some(PokemonMove::Splash), None, None, None]),
                    None,
                    None,
                    Some(Nature::Hardy),
                    None,
                    None,
                    Some([32, 0, 0, 0, 0, 32]),
                    None,
                    true,
                );

                battle_state_from_lists(vec![p1_mon], vec![], vec![p2_mon], vec![])
            };

            let normal_state = make_state(Ability::Illuminate);
            let sniper_state = make_state(Ability::Sniper);

            let move_data = move_dex.get(&PokemonMove::KowtowCleave).unwrap();
            let attacker_slot = FieldSlot { player: Player::P1, slot_index: 0 };
            let target_slot = FieldSlot { player: Player::P2, slot_index: 0 };

            let normal_attacker = simulator_helpers::get_pokemon_at_slot(&normal_state, attacker_slot).unwrap();
            let normal_target = simulator_helpers::get_pokemon_at_slot(&normal_state, target_slot).unwrap();
            let sniper_attacker = simulator_helpers::get_pokemon_at_slot(&sniper_state, attacker_slot).unwrap();
            let sniper_target = simulator_helpers::get_pokemon_at_slot(&sniper_state, target_slot).unwrap();

            let normal_outcomes = simulator_helpers::calculate_damage_outcomes_for_target(
                &normal_state,
                normal_attacker,
                normal_target,
                attacker_slot,
                target_slot,
                move_data,
                crate::simulator::DamageConfig { consider_crit: true, damage_rolls: 1 },
                1.0,
                1.0,
            );

            let sniper_outcomes = simulator_helpers::calculate_damage_outcomes_for_target(
                &sniper_state,
                sniper_attacker,
                sniper_target,
                attacker_slot,
                target_slot,
                move_data,
                crate::simulator::DamageConfig { consider_crit: true, damage_rolls: 1 },
                1.0,
                1.0,
            );

            let normal_crit_damage = normal_outcomes.iter().find(|(_, is_crit, _)| *is_crit).map(|(damage, _, _)| *damage).expect("expected crit outcome");
            let sniper_crit_damage = sniper_outcomes.iter().find(|(_, is_crit, _)| *is_crit).map(|(damage, _, _)| *damage).expect("expected crit outcome");

            assert!(sniper_crit_damage > normal_crit_damage);
        }
    }

    mod type_effectiveness {
        use super::*;

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

            let outcome = expected_final_state.clone();
            expected_outcomes.push((MatchState::BattleState(outcome), 1.0));

            println!("{:?}", outcomes);
            assert!(is_permutation(&outcomes, &expected_outcomes));
        }
    }

    mod accuracy {
        use super::*;

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
        fn rain_weather_accuracy() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let cases = vec![
                (PokemonMove::Thunder, None, 0.7),
                (PokemonMove::Thunder, Some(Weather::Rain), 1.0),
                (PokemonMove::Thunder, Some(Weather::Sun), 0.5),
                (PokemonMove::Hurricane, None, 0.7),
                (PokemonMove::Hurricane, Some(Weather::Rain), 1.0),
                (PokemonMove::Hurricane, Some(Weather::Sun), 0.5),
            ];

            for (move_name, weather, expected_hit_probability) in cases {
                let attacker = build_pokemon_state(
                    Species::DragoniteMega,
                    &pokemon_dex,
                    &move_dex,
                    Some(100),
                    Some([Some(move_name), None, None, None]),
                    None,
                    Some(Ability::None),
                    None,
                    None,
                    Some(crate::dex_data::PokemonType::Normal),
                    Some([0, 0, 0, 0, 0, 0]),
                    None,
                    false,
                );

                let defender = build_pokemon_state(
                    Species::Snorlax,
                    &pokemon_dex,
                    &move_dex,
                    Some(100),
                    Some([Some(PokemonMove::Splash), None, None, None]),
                    None,
                    Some(Ability::None),
                    None,
                    None,
                    Some(crate::dex_data::PokemonType::Normal),
                    Some([0, 0, 0, 0, 0, 0]),
                    None,
                    false,
                );

                let defender_initial_hp = defender.hp;
                let weather_turns = weather.as_ref().map(|_| 5);
                let mut state = battle_state_from_lists(vec![attacker], vec![], vec![defender], vec![]);
                state.weather = weather;
                state.weather_turns = weather_turns;

                let outcomes = run_single_turn(
                    &MatchState::BattleState(state),
                    &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                    &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                    &move_dex,
                    &pokemon_dex,
                );

                assert!((outcomes.iter().map(|(_, probability)| *probability).sum::<f64>() - 1.0).abs() < 1e-9);
                assert!((hit_probability(&outcomes, defender_initial_hp) - expected_hit_probability).abs() < 1e-9);
            }

            for move_name in [PokemonMove::Thunder, PokemonMove::Hurricane] {
                let attacker = build_pokemon_state(
                    Species::DragoniteMega,
                    &pokemon_dex,
                    &move_dex,
                    Some(100),
                    Some([Some(move_name), None, None, None]),
                    None,
                    Some(Ability::None),
                    None,
                    None,
                    Some(crate::dex_data::PokemonType::Normal),
                    Some([0, 0, 0, 0, 0, 0]),
                    None,
                    false,
                );

                let mut defender = build_pokemon_state(
                    Species::Snorlax,
                    &pokemon_dex,
                    &move_dex,
                    Some(100),
                    Some([Some(PokemonMove::Splash), None, None, None]),
                    None,
                    Some(Ability::None),
                    None,
                    None,
                    Some(crate::dex_data::PokemonType::Normal),
                    Some([0, 0, 0, 0, 0, 0]),
                    None,
                    false,
                );
                defender.boosts[6] = 6;

                let defender_initial_hp = defender.hp;
                let weather_turns = Some(5);
                let mut state = battle_state_from_lists(vec![attacker], vec![], vec![defender], vec![]);
                state.weather = Some(Weather::Rain);
                state.weather_turns = weather_turns;

                let outcomes = run_single_turn(
                    &MatchState::BattleState(state),
                    &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                    &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                    &move_dex,
                    &pokemon_dex,
                );

                assert!((outcomes.iter().map(|(_, probability)| *probability).sum::<f64>() - 1.0).abs() < 1e-9);
                assert!((hit_probability(&outcomes, defender_initial_hp) - 1.0).abs() < 1e-9);
            }
        }

        #[test]
        fn blizzard_snow() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let cases = vec![
                (None, 0.7),
                (Some(Weather::Snow), 1.0),
            ];

            for (weather, expected_hit_probability) in cases {
                let attacker = build_pokemon_state(
                    Species::Ninetales,
                    &pokemon_dex,
                    &move_dex,
                    Some(100),
                    Some([Some(PokemonMove::Blizzard), None, None, None]),
                    None,
                    Some(Ability::None),
                    None,
                    None,
                    Some(crate::dex_data::PokemonType::Normal),
                    Some([0, 0, 0, 0, 0, 0]),
                    None,
                    false,
                );

                let defender = build_pokemon_state(
                    Species::Snorlax,
                    &pokemon_dex,
                    &move_dex,
                    Some(100),
                    Some([Some(PokemonMove::Splash), None, None, None]),
                    None,
                    Some(Ability::None),
                    None,
                    None,
                    Some(crate::dex_data::PokemonType::Normal),
                    Some([0, 0, 0, 0, 0, 0]),
                    None,
                    false,
                );

                let defender_initial_hp = defender.hp;
                let weather_turns = weather.as_ref().map(|_| 5);
                let mut state = battle_state_from_lists(vec![attacker], vec![], vec![defender], vec![]);
                state.weather = weather;
                state.weather_turns = weather_turns;

                let outcomes = run_single_turn(
                    &MatchState::BattleState(state),
                    &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                    &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                    &move_dex,
                    &pokemon_dex,
                );

                assert!((outcomes.iter().map(|(_, probability)| *probability).sum::<f64>() - 1.0).abs() < 1e-9);
                assert!((hit_probability(&outcomes, defender_initial_hp) - expected_hit_probability).abs() < 1e-9);
            }

            let attacker = build_pokemon_state(
                Species::Ninetales,
                &pokemon_dex,
                &move_dex,
                Some(100),
                Some([Some(PokemonMove::Blizzard), None, None, None]),
                None,
                Some(Ability::None),
                None,
                None,
                Some(crate::dex_data::PokemonType::Normal),
                Some([0, 0, 0, 0, 0, 0]),
                None,
                false,
            );

            let mut defender = build_pokemon_state(
                Species::Snorlax,
                &pokemon_dex,
                &move_dex,
                Some(100),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None,
                Some(Ability::None),
                None,
                None,
                Some(crate::dex_data::PokemonType::Normal),
                Some([0, 0, 0, 0, 0, 0]),
                None,
                false,
            );
            defender.boosts[6] = 6;

            let defender_initial_hp = defender.hp;
            let mut state = battle_state_from_lists(vec![attacker], vec![], vec![defender], vec![]);
            state.weather = Some(Weather::Snow);
            state.weather_turns = Some(5);

            let outcomes = run_single_turn(
                &MatchState::BattleState(state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex,
                &pokemon_dex,
            );

            assert!((outcomes.iter().map(|(_, probability)| *probability).sum::<f64>() - 1.0).abs() < 1e-9);
            assert!((hit_probability(&outcomes, defender_initial_hp) - 1.0).abs() < 1e-9);
        }
    }

    mod ohko {
        use super::*;

        #[test]
        fn ohko_win() {
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
    }

    mod stat_boosts {
        use super::*;

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
    }

    mod doubles {
        use super::*;

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
    }

    mod semi_invulnerable {
        use super::*;

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

        #[test]
        fn skydrop_first_turn_and_second_turn_damage() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let attacker = build_pokemon_state(
                Species::Dragonite,
                pokemon_dex,
                move_dex,
                None,
                Some([Some(PokemonMove::SkyDrop), Some(PokemonMove::Splash), None, None]),
                None,
                Some(Ability::Illuminate),
                Some(Nature::Adamant),
                None,
                None,
                Some([0, 252, 0, 0, 0, 252]),
                None,
                false,
            );

            let defender = build_pokemon_state(
                Species::Snorlax,
                pokemon_dex,
                move_dex,
                None,
                Some([Some(PokemonMove::Splash), Some(PokemonMove::Splash), None, None]),
                None,
                Some(Ability::Illuminate),
                Some(Nature::Careful),
                None,
                None,
                Some([252, 0, 0, 0, 0, 0]),
                None,
                false,
            );

            let initial_state = battle_state_from_lists(vec![attacker], vec![], vec![defender], vec![]);

            let turn_one = run_single_turn(
                &MatchState::BattleState(initial_state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                move_dex,
                pokemon_dex,
            );

            let (state_after_turn_one, _) = extract_battle_state(turn_one);
            assert!(has_sky_drop_move_volatile(&state_after_turn_one.p1_active_mons[0]));
            assert!(has_sky_drop_turn_volatile(&state_after_turn_one.p2_active_mons[0]));

            let turn_two = run_single_turn(
                &MatchState::BattleState(state_after_turn_one.clone()),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                move_dex,
                pokemon_dex,
            );

            let (state_after_turn_two, _) = extract_battle_state(turn_two);
            assert!(state_after_turn_two.p2_active_mons[0].hp < state_after_turn_one.p2_active_mons[0].hp);
            assert!(!has_sky_drop_move_volatile(&state_after_turn_two.p1_active_mons[0]));
            assert!(!has_sky_drop_turn_volatile(&state_after_turn_two.p2_active_mons[0]));
        }

        #[test]
        fn skydrop_airborne_exception_moves_hit() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let cases = vec![
                (PokemonMove::Gust, None),
                (PokemonMove::SmackDown, None),
                (PokemonMove::Twister, None),
                (PokemonMove::SkyUppercut, None),
                (PokemonMove::Thunder, Some(Weather::Rain)),
                (PokemonMove::Hurricane, Some(Weather::Rain)),
            ];

            for (move_name, weather) in cases {
                let attacker = build_pokemon_state(
                    Species::Dragonite,
                    pokemon_dex,
                    move_dex,
                    None,
                    Some([Some(move_name), None, None, None]),
                    None,
                    Some(Ability::Illuminate),
                    Some(Nature::Adamant),
                    None,
                    None,
                    Some([0, 252, 0, 0, 0, 252]),
                    None,
                    false,
                );

                let mut defender = build_pokemon_state(
                    Species::Snorlax,
                    pokemon_dex,
                    move_dex,
                    None,
                    Some([Some(PokemonMove::Splash), None, None, None]),
                    None,
                    Some(Ability::Illuminate),
                    Some(Nature::Careful),
                    None,
                    None,
                    Some([252, 0, 0, 0, 0, 0]),
                    None,
                    false,
                );
                let defender_hp = defender.hp;
                defender.volatiles.push(VolatileStatusState::TurnStatus(VolatileStatus::SkyDrop, 0));

                let mut initial_state = battle_state_from_lists(vec![attacker], vec![], vec![defender], vec![]);
                if let Some(weather) = weather {
                    initial_state.weather = Some(weather);
                    initial_state.weather_turns = Some(5);
                }

                let outcomes = run_single_turn(
                    &MatchState::BattleState(initial_state),
                    &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                    &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                    move_dex,
                    pokemon_dex,
                );

                assert!(outcomes.iter().any(|(state, _)| matches!(state, MatchState::BattleState(bs) if bs.p2_active_mons[0].hp < defender_hp)));
            }
        }

        #[test]
        fn skydrop_bypassed_by_noguard_and_identify() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let base_defender = build_pokemon_state(
                Species::Snorlax,
                pokemon_dex,
                move_dex,
                None,
                Some([Some(PokemonMove::Splash), None, None, None]),
                None,
                Some(Ability::Illuminate),
                Some(Nature::Careful),
                None,
                None,
                Some([252, 0, 0, 0, 0, 0]),
                None,
                false,
            );

            let control_attacker = build_pokemon_state(
                Species::Snorlax,
                pokemon_dex,
                move_dex,
                None,
                Some([Some(PokemonMove::Earthquake), None, None, None]),
                None,
                Some(Ability::Illuminate),
                Some(Nature::Adamant),
                None,
                None,
                Some([0, 252, 0, 0, 0, 252]),
                None,
                false,
            );

            let mut control_defender = base_defender.clone();
            let control_defender_hp = control_defender.hp;
            control_defender.volatiles.push(VolatileStatusState::TurnStatus(VolatileStatus::SkyDrop, 0));
            let control_state = battle_state_from_lists(vec![control_attacker], vec![], vec![control_defender], vec![]);
            let control_outcomes = run_single_turn(
                &MatchState::BattleState(control_state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                move_dex,
                pokemon_dex,
            );
            assert!(control_outcomes.iter().all(|(state, _)| matches!(state, MatchState::BattleState(bs) if bs.p2_active_mons[0].hp == control_defender_hp)));

            let noguard_attacker = build_pokemon_state(
                Species::Snorlax,
                pokemon_dex,
                move_dex,
                None,
                Some([Some(PokemonMove::Earthquake), None, None, None]),
                None,
                Some(Ability::NoGuard),
                Some(Nature::Adamant),
                None,
                None,
                Some([0, 252, 0, 0, 0, 252]),
                None,
                false,
            );

            let mut noguard_defender = base_defender.clone();
            let noguard_defender_hp = noguard_defender.hp;
            noguard_defender.volatiles.push(VolatileStatusState::TurnStatus(VolatileStatus::SkyDrop, 0));
            let noguard_state = battle_state_from_lists(vec![noguard_attacker], vec![], vec![noguard_defender], vec![]);
            let noguard_outcomes = run_single_turn(
                &MatchState::BattleState(noguard_state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                move_dex,
                pokemon_dex,
            );
            assert!(noguard_outcomes.iter().any(|(state, _)| matches!(state, MatchState::BattleState(bs) if bs.p2_active_mons[0].hp < noguard_defender_hp)));

            for identify_volatile in [VolatileStatus::Foresight, VolatileStatus::MiracleEye] {
                let identify_attacker = build_pokemon_state(
                    Species::Snorlax,
                    pokemon_dex,
                    move_dex,
                    None,
                    Some([Some(PokemonMove::Earthquake), None, None, None]),
                    None,
                    Some(Ability::Illuminate),
                    Some(Nature::Adamant),
                    None,
                    None,
                    Some([0, 252, 0, 0, 0, 252]),
                    None,
                    false,
                );

                let mut identify_defender = base_defender.clone();
                let identify_defender_hp = identify_defender.hp;
                identify_defender.volatiles.push(VolatileStatusState::TurnStatus(VolatileStatus::SkyDrop, 0));
                identify_defender.volatiles.push(VolatileStatusState::TurnStatus(identify_volatile, 0));
                let identify_state = battle_state_from_lists(vec![identify_attacker], vec![], vec![identify_defender], vec![]);
                let identify_outcomes = run_single_turn(
                    &MatchState::BattleState(identify_state),
                    &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                    &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                    move_dex,
                    pokemon_dex,
                );
                assert!(identify_outcomes.iter().any(|(state, _)| matches!(state, MatchState::BattleState(bs) if bs.p2_active_mons[0].hp < identify_defender_hp)));
            }
        }

        #[test]
        fn sky_drop_iron_ball_sub() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let cases = vec!["gravity", "iron_ball", "substitute"];

            for case in cases {
                let attacker = build_pokemon_state(
                    Species::Dragonite,
                    pokemon_dex,
                    move_dex,
                    None,
                    Some([Some(PokemonMove::SkyDrop), None, None, None]),
                    None,
                    Some(Ability::Illuminate),
                    Some(Nature::Adamant),
                    None,
                    None,
                    Some([0, 252, 0, 0, 0, 252]),
                    None,
                    true,
                );

                let defender = build_pokemon_state(
                    Species::Snorlax,
                    pokemon_dex,
                    move_dex,
                    None,
                    Some([Some(PokemonMove::Splash), None, None, None]),
                    None,
                    Some(Ability::Illuminate),
                    Some(Nature::Careful),
                    None,
                    None,
                    Some([252, 0, 0, 0, 0, 0]),
                    None,
                    true,
                );
                let defender_hp = defender.hp;

                let mut state = battle_state_from_lists(vec![attacker], vec![], vec![defender], vec![]);
                match case {
                    "gravity" => {
                        state.pseudo_weathers.push(PseudoWeather::Gravity);
                        state.pseudo_weather_turns.push(5);
                    }
                    "iron_ball" => {
                        state.p2_active_mons[0].item = Item::IronBall;
                    }
                    "substitute" => {
                        state.p2_active_mons[0].volatiles.push(VolatileStatusState::TurnStatus(VolatileStatus::Substitute, 0));
                    }
                    _ => unreachable!(),
                }

                let outcomes = run_single_turn(
                    &MatchState::BattleState(state),
                    &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                    &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                    move_dex,
                    pokemon_dex,
                );

                assert!(outcomes.iter().all(|(state, _)| matches!(state, MatchState::BattleState(bs) if bs.p2_active_mons[0].hp == defender_hp)));
                assert!(outcomes.iter().all(|(state, _)| matches!(state, MatchState::BattleState(bs) if !has_sky_drop_move_volatile(&bs.p1_active_mons[0]) && !has_sky_drop_turn_volatile(&bs.p2_active_mons[0]))));
            }
        }

        #[test]
        fn skydrop_immunities() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let cases = vec!["flying", "levitate", "magnet_rise", "telekinesis"];

            for case in cases {
                let attacker = build_pokemon_state(
                    Species::Dragonite,
                    pokemon_dex,
                    move_dex,
                    None,
                    Some([Some(PokemonMove::SkyDrop), None, None, None]),
                    None,
                    Some(Ability::Illuminate),
                    Some(Nature::Adamant),
                    None,
                    None,
                    Some([0, 252, 0, 0, 0, 252]),
                    None,
                    true,
                );

                let mut defender = match case {
                    "flying" => build_pokemon_state(
                        Species::Dragonite,
                        pokemon_dex,
                        move_dex,
                        None,
                        Some([Some(PokemonMove::Splash), None, None, None]),
                        None,
                        Some(Ability::Illuminate),
                        Some(Nature::Careful),
                        None,
                        None,
                        Some([252, 0, 0, 0, 0, 0]),
                        None,
                        true,
                    ),
                    _ => build_pokemon_state(
                        Species::Snorlax,
                        pokemon_dex,
                        move_dex,
                        None,
                        Some([Some(PokemonMove::Splash), None, None, None]),
                        None,
                        Some(Ability::Illuminate),
                        Some(Nature::Careful),
                        None,
                        None,
                        Some([252, 0, 0, 0, 0, 0]),
                        None,
                        true,
                    ),
                };

                match case {
                    "levitate" => defender.ability = Ability::Levitate,
                    "magnet_rise" => defender.volatiles.push(VolatileStatusState::TurnStatus(VolatileStatus::MagnetRise, 0)),
                    "telekinesis" => defender.volatiles.push(VolatileStatusState::TurnStatus(VolatileStatus::Telekinesis, 0)),
                    _ => {}
                }

                let defender_hp = defender.hp;
                let initial_state = battle_state_from_lists(vec![attacker], vec![], vec![defender], vec![]);

                let turn_one = run_single_turn(
                    &MatchState::BattleState(initial_state),
                    &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                    &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                    move_dex,
                    pokemon_dex,
                );
                let (state_after_turn_one, _) = extract_battle_state(turn_one);
                assert!(has_sky_drop_move_volatile(&state_after_turn_one.p1_active_mons[0]));
                assert!(has_sky_drop_turn_volatile(&state_after_turn_one.p2_active_mons[0]));

                let turn_two = run_single_turn(
                    &MatchState::BattleState(state_after_turn_one.clone()),
                    &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                    &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                    move_dex,
                    pokemon_dex,
                );
                let (state_after_turn_two, _) = extract_battle_state(turn_two);
                assert_eq!(state_after_turn_two.p2_active_mons[0].hp, defender_hp);
                assert!(!has_sky_drop_move_volatile(&state_after_turn_two.p1_active_mons[0]));
                assert!(!has_sky_drop_turn_volatile(&state_after_turn_two.p2_active_mons[0]));
            }
        }

        #[test]
        fn skydrop_target_cant_move() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let dragonite = build_pokemon_state(
                Species::Dragonite,
                pokemon_dex,
                move_dex,
                None,
                Some([Some(PokemonMove::SkyDrop), Some(PokemonMove::Splash), None, None]),
                None,
                Some(Ability::Illuminate),
                Some(Nature::Jolly),
                None,
                None,
                Some([0, 0, 0, 0, 0, 252]),
                None,
                false,
            );

            let mimikyu = build_pokemon_state(
                Species::Mimikyu,
                pokemon_dex,
                move_dex,
                None,
                Some([Some(PokemonMove::SwordsDance), None, None, None]),
                None,
                Some(Ability::Illuminate),
                Some(Nature::Brave),
                None,
                None,
                Some([0, 0, 0, 0, 0, 0]),
                None,
                false,
            );

            let initial_state = battle_state_from_lists(vec![dragonite], vec![], vec![mimikyu], vec![]);

            let turn_one = run_single_turn(
                &MatchState::BattleState(initial_state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                move_dex,
                pokemon_dex,
            );

            let (state_after_turn_one, _) = extract_battle_state(turn_one);
            assert_eq!(state_after_turn_one.p2_active_mons[0].boosts[0], 0);
            assert!(has_sky_drop_move_volatile(&state_after_turn_one.p1_active_mons[0]));
            assert!(has_sky_drop_turn_volatile(&state_after_turn_one.p2_active_mons[0]));

            let turn_two = run_single_turn(
                &MatchState::BattleState(state_after_turn_one.clone()),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                move_dex,
                pokemon_dex,
            );

            let (state_after_turn_two, _) = extract_battle_state(turn_two);
            assert_eq!(state_after_turn_two.p2_active_mons[0].boosts[0], 2);
            assert!(!has_sky_drop_move_volatile(&state_after_turn_two.p1_active_mons[0]));
            assert!(!has_sky_drop_turn_volatile(&state_after_turn_two.p2_active_mons[0]));
        }
    }

    mod charging_moves {
        use super::*;

        #[test]
        fn solar_beam_sun() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let venusaur = build_pokemon_state(
                Species::Venusaur,
                &pokemon_dex,
                &move_dex,
                Some(50),
                Some([Some(PokemonMove::SolarBeam), None, None, None]),
                None,
                Some(Ability::None),
                None,
                None,
                Some(crate::dex_data::PokemonType::Normal),
                Some([0, 0, 0, 0, 0, 0]),
                None,
                false,
            );

            let basculegion = build_pokemon_state(
                Species::Basculegion,
                &pokemon_dex,
                &move_dex,
                Some(1),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None,
                Some(Ability::None),
                None,
                None,
                Some(crate::dex_data::PokemonType::Normal),
                Some([0, 0, 0, 0, 0, 0]),
                None,
                false,
            );

            let mut state = battle_state_from_lists(vec![venusaur], vec![], vec![basculegion], vec![]);
            state.weather = Some(Weather::Sun);
            state.weather_turns = Some(5);

            let outcomes = run_single_turn(
                &MatchState::BattleState(state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex,
                &pokemon_dex,
            );

            assert_eq!(outcomes.len(), 1);
            assert!(matches!(outcomes[0].0, MatchState::GameOverState { winner: Player::P1 }));
            assert!((outcomes[0].1 - 1.0).abs() < 1e-9);
        }

        #[test]
        fn electro_shot() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let make_state = |weather: Option<Weather>| {
                let archaludon = build_pokemon_state(
                    Species::Archaludon,
                    &pokemon_dex,
                    &move_dex,
                    Some(100),
                    Some([Some(PokemonMove::ElectroShot), None, None, None]),
                    None,
                    Some(Ability::None),
                    None,
                    None,
                    Some(crate::dex_data::PokemonType::Normal),
                    Some([0, 0, 0, 0, 0, 0]),
                    None,
                    false,
                );

                let basculegion = build_pokemon_state(
                    Species::Basculegion,
                    &pokemon_dex,
                    &move_dex,
                    Some(1),
                    Some([Some(PokemonMove::Splash), None, None, None]),
                    None,
                    Some(Ability::None),
                    None,
                    None,
                    Some(crate::dex_data::PokemonType::Normal),
                    Some([0, 0, 0, 0, 0, 0]),
                    None,
                    false,
                );

                let dummy_back_mon = build_pokemon_state(
                    Species::Magikarp,
                    &pokemon_dex,
                    &move_dex,
                    Some(50),
                    Some([Some(PokemonMove::Splash), None, None, None]),
                    None,
                    Some(Ability::None),
                    None,
                    None,
                    Some(crate::dex_data::PokemonType::Normal),
                    Some([0, 0, 0, 0, 0, 0]),
                    None,
                    false,
                );

                let weather_turns = weather.as_ref().map(|_| 5);
                let mut state = battle_state_from_lists(vec![archaludon], vec![], vec![basculegion], vec![dummy_back_mon]);
                state.weather = weather;
                state.weather_turns = weather_turns;
                state
            };

            let rain_outcomes = run_single_turn(
                &MatchState::BattleState(make_state(Some(Weather::Rain))),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex,
                &pokemon_dex,
            );

            assert_eq!(rain_outcomes.len(), 1);
            let (MatchState::BattleState(rain_state), rain_probability) = rain_outcomes.into_iter().next().unwrap() else {
                panic!("expected a battle state outcome in rain");
            };
            assert!((rain_probability - 1.0).abs() < 1e-9);
            assert_eq!(rain_state.p1_active_mons[0].boosts[2], 1);
            assert_eq!(rain_state.p1_active_mons[0].species, Species::Archaludon);
            assert_eq!(rain_state.p2_active_mons[0].hp, 0);
            assert_eq!(rain_state.p2_back_mons[0].species, Species::Magikarp);
            assert!(!has_charging_volatile(&rain_state.p1_active_mons[0], PokemonMove::ElectroShot));

            let no_weather_outcomes = run_single_turn(
                &MatchState::BattleState(make_state(None)),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex,
                &pokemon_dex,
            );

            assert_eq!(no_weather_outcomes.len(), 1);
            let (MatchState::BattleState(no_weather_state), no_weather_probability) = no_weather_outcomes.into_iter().next().unwrap() else {
                panic!("expected a battle state outcome without rain");
            };
            assert!((no_weather_probability - 1.0).abs() < 1e-9);
            assert_eq!(no_weather_state.p1_active_mons[0].boosts[2], 1);
            assert_eq!(no_weather_state.p1_active_mons[0].species, Species::Archaludon);
            assert_eq!(no_weather_state.p2_active_mons[0].species, Species::Basculegion);
            assert!(has_charging_volatile(&no_weather_state.p1_active_mons[0], PokemonMove::ElectroShot));
        }

        #[test]
        fn meteor_beam_charge_boost() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let make_state = |weather: Option<Weather>| {
                let attacker = build_pokemon_state(
                    Species::Archaludon,
                    &pokemon_dex,
                    &move_dex,
                    Some(100),
                    Some([Some(PokemonMove::MeteorBeam), None, None, None]),
                    None,
                    Some(Ability::None),
                    None,
                    None,
                    Some(crate::dex_data::PokemonType::Normal),
                    Some([0, 0, 0, 0, 0, 0]),
                    None,
                    false,
                );

                let target = build_pokemon_state(
                    Species::Basculegion,
                    &pokemon_dex,
                    &move_dex,
                    Some(50),
                    Some([Some(PokemonMove::Splash), None, None, None]),
                    None,
                    Some(Ability::None),
                    None,
                    None,
                    Some(crate::dex_data::PokemonType::Normal),
                    Some([0, 0, 0, 0, 0, 0]),
                    None,
                    false,
                );

                let weather_turns = weather.as_ref().map(|_| 5);
                let mut state = battle_state_from_lists(vec![attacker], vec![], vec![target], vec![]);
                state.weather = weather;
                state.weather_turns = weather_turns;
                state
            };

            // Without weather: Meteor Beam charges on turn 1, boosting Sp. Atk by 1 stage.
            let (no_weather_state, no_weather_probability) = extract_battle_state(run_single_turn(
                &MatchState::BattleState(make_state(None)),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex,
                &pokemon_dex,
            ));
            assert!((no_weather_probability - 1.0).abs() < 1e-9);
            assert_eq!(no_weather_state.p1_active_mons[0].boosts[2], 1);
            assert!(has_charging_volatile(&no_weather_state.p1_active_mons[0], PokemonMove::MeteorBeam));

            // Unlike Electro Shot, Meteor Beam does NOT skip the charge turn in rain.
            let (rain_state, rain_probability) = extract_battle_state(run_single_turn(
                &MatchState::BattleState(make_state(Some(Weather::Rain))),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex,
                &pokemon_dex,
            ));
            assert!((rain_probability - 1.0).abs() < 1e-9);
            assert_eq!(rain_state.p1_active_mons[0].boosts[2], 1);
            assert!(has_charging_volatile(&rain_state.p1_active_mons[0], PokemonMove::MeteorBeam));
        }
    }

    mod mega_evolution {
        use super::*;

        #[test]
        fn mega_tyranitar() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let make_state = |item: Option<Item>| {
                let tyranitar = build_pokemon_state(
                    Species::Tyranitar,
                    pokemon_dex,
                    move_dex,
                    Some(50),
                    Some([Some(PokemonMove::IcePunch), None, None, None]),
                    None,
                    Some(Ability::SandStream),
                    Some(Nature::Adamant),
                    item,
                    None,
                    Some([0, 0, 0, 0, 0, 0]),
                    None,
                    false,
                );

                let garchomp = build_pokemon_state(
                    Species::Garchomp,
                    pokemon_dex,
                    move_dex,
                    Some(50),
                    Some([Some(PokemonMove::Splash), None, None, None]),
                    None,
                    Some(Ability::Illuminate),
                    Some(Nature::Hardy),
                    None,
                    None,
                    Some([252, 0, 0, 0, 0, 0]),
                    None,
                    false,
                );

                let back_mon = build_pokemon_state(
                    Species::Magikarp,
                    pokemon_dex,
                    move_dex,
                    Some(50),
                    Some([Some(PokemonMove::Splash), None, None, None]),
                    None,
                    Some(Ability::Illuminate),
                    Some(Nature::Hardy),
                    None,
                    None,
                    Some([0, 0, 0, 0, 0, 0]),
                    None,
                    false,
                );

                battle_state_from_lists(vec![tyranitar], vec![], vec![garchomp], vec![back_mon])
            };

            let normal_outcomes = simulate_turn(
                &MatchState::BattleState(make_state(None)),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex,
                &pokemon_dex,
                false,
                1,
            );

            assert!(!normal_outcomes.is_empty());
            let normal_total_probability: f64 = normal_outcomes.iter().map(|(_, p)| *p).sum();
            assert!((normal_total_probability - 1.0).abs() < 1e-9);
            assert!(normal_outcomes.iter().all(|(state, _)| matches!(state, MatchState::BattleState(bs)
                if bs.p1_active_mons[0].species == Species::Tyranitar
                    && !bs.p1_active_mons[0].is_mega
                    && bs.p2_active_mons[0].species == Species::Garchomp
                    && bs.p2_active_mons[0].hp > 0
            )));

            let mega_outcomes = simulate_turn(
                &MatchState::BattleState(make_state(Some(Item::Tyranitarite))),
                &PlayerCommand::Battle(simple_attack_mega(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex,
                &pokemon_dex,
                false,
                1,
            );

            assert!(!mega_outcomes.is_empty());
            let mega_total_probability: f64 = mega_outcomes.iter().map(|(_, p)| *p).sum();
            assert!((mega_total_probability - 1.0).abs() < 1e-9);
            assert!(mega_outcomes.iter().all(|(state, _)| matches!(state, MatchState::BattleState(bs)
                if bs.p1_active_mons[0].species == Species::TyranitarMega
                    && bs.p1_active_mons[0].is_mega
                    && bs.p2_active_mons[0].species == Species::Garchomp
                    && bs.p2_active_mons[0].fainted
                    && bs.weather == Some(Weather::Sandstorm)
                    && bs.weather_turns == Some(4)
            )));
        }

        // Mega Charizard Y gains Drought on mega evolution. Base Charizard (Blaze) sets no
        // weather on send-out; Sun should only appear after it mega evolves.
        #[test]
        fn mega_charizard_y_sets_sun_on_mega_evolve_only() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let charizard = build_pokemon_state(
                Species::Charizard,
                &pokemon_dex,
                &move_dex,
                Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None,
                Some(Ability::Blaze),
                Some(Nature::Hardy),
                Some(Item::CharizarditeY),
                None,
                None,
                None,
                false,
            );
            let foe = build_pokemon_state(
                Species::Garchomp,
                &pokemon_dex,
                &move_dex,
                Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None,
                Some(Ability::Illuminate),
                Some(Nature::Hardy),
                None,
                None,
                None,
                None,
                false,
            );

            let initial = battle_state_from_lists(vec![charizard], vec![], vec![foe], vec![]);
            // Base Blaze sets no weather on send-out.
            assert_eq!(initial.weather, None);

            let outcomes = simulate_turn(
                &MatchState::BattleState(initial),
                &PlayerCommand::Battle(simple_attack_mega(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex,
                &pokemon_dex,
                false,
                1,
            );

            assert!(!outcomes.is_empty());
            assert!(outcomes.iter().all(|(state, _)| matches!(state, MatchState::BattleState(bs)
                if bs.p1_active_mons[0].species == Species::CharizardMegaY
                    && bs.p1_active_mons[0].is_mega
                    && bs.weather == Some(Weather::Sun)
            )));
        }

        // Mega Manectric gains Intimidate on mega evolution. Base Manectric (Static) does
        // not lower the foe's Attack on send-out; the drop should only happen on mega evolve.
        #[test]
        fn mega_manectric_intimidates_on_mega_evolve_only() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let manectric = build_pokemon_state(
                Species::Manectric,
                &pokemon_dex,
                &move_dex,
                Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None,
                Some(Ability::Static),
                Some(Nature::Hardy),
                Some(Item::Manectite),
                None,
                None,
                None,
                false,
            );
            let foe = build_pokemon_state(
                Species::Garchomp,
                &pokemon_dex,
                &move_dex,
                Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None,
                Some(Ability::Illuminate),
                Some(Nature::Hardy),
                None,
                None,
                None,
                None,
                false,
            );

            let initial = battle_state_from_lists(vec![manectric], vec![], vec![foe], vec![]);
            // Base Static does not Intimidate on send-out.
            assert_eq!(initial.p2_active_mons[0].boosts[0], 0);

            let outcomes = simulate_turn(
                &MatchState::BattleState(initial),
                &PlayerCommand::Battle(simple_attack_mega(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex,
                &pokemon_dex,
                false,
                1,
            );

            assert!(!outcomes.is_empty());
            assert!(outcomes.iter().all(|(state, _)| matches!(state, MatchState::BattleState(bs)
                if bs.p1_active_mons[0].species == Species::ManectricMega
                    && bs.p1_active_mons[0].is_mega
                    && bs.p2_active_mons[0].boosts[0] == -1
            )));
        }
    }

    mod weather {
        use super::*;

        #[test]
        fn weather_abilities_and_moves() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let pelipper = build_pokemon_state(
                Species::Pelipper,
                &pokemon_dex,
                &move_dex,
                None,
                Some([
                    Some(PokemonMove::RainDance),
                    Some(PokemonMove::Splash),
                    None,
                    None,
                ]),
                None,
                Some(Ability::Drizzle),
                Some(Nature::Timid),
                None,
                None,
                Some([0, 0, 0, 0, 0, 0]),
                None,
                false,
            );

            let torkoal = build_pokemon_state(
                Species::Torkoal,
                &pokemon_dex,
                &move_dex,
                None,
                Some([
                    Some(PokemonMove::Splash),
                    Some(PokemonMove::RainDance),
                    None,
                    None,
                ]),
                None,
                Some(Ability::Drought),
                Some(Nature::Brave),
                None,
                None,
                Some([0, 0, 0, 0, 0, 0]),
                None,
                false,
            );

            let initial_state = battle_state_from_lists(vec![pelipper], vec![], vec![torkoal], vec![]);

            assert_eq!(initial_state.p1_active_mons[0].species, Species::Pelipper);
            assert_eq!(initial_state.p2_active_mons[0].species, Species::Torkoal);
            assert_eq!(initial_state.p1_active_mons[0].ability, Ability::Drizzle);
            assert_eq!(initial_state.p2_active_mons[0].ability, Ability::Drought);
            assert_eq!(initial_state.weather, Some(Weather::Sun));

            let outcomes = run_single_turn(
                &MatchState::BattleState(initial_state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex,
                &pokemon_dex,
            );

            let (state_after_turn, probability) = extract_battle_state(outcomes);
            assert!((probability - 1.0).abs() < 1e-9);
            assert_eq!(state_after_turn.weather, Some(Weather::Rain));
        }

        #[test]
        fn sandstorm_spdef() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let make_state = |weather: Option<Weather>| {
                let primarina = build_pokemon_state(
                    Species::Primarina,
                    &pokemon_dex,
                    &move_dex,
                    Some(50),
                    Some([Some(PokemonMove::BubbleBeam), None, None, None]),
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    false,
                );

                let tyranitar = build_pokemon_state(
                    Species::Tyranitar,
                    &pokemon_dex,
                    &move_dex,
                    Some(50),
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

                let mut state = battle_state_from_lists(vec![primarina], vec![], vec![tyranitar], vec![]);
                state.weather = weather;
                state.weather_turns = state.weather.as_ref().map(|_| 5);
                state
            };

            let no_weather_state = make_state(None);
            let sandstorm_state = make_state(Some(Weather::Sandstorm));

            let no_weather_initial_hp = no_weather_state.p2_active_mons[0].hp;
            let sandstorm_initial_hp = sandstorm_state.p2_active_mons[0].hp;

            let no_weather_outcomes = run_single_turn(
                &MatchState::BattleState(no_weather_state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex,
                &pokemon_dex,
            );

            let sandstorm_outcomes = run_single_turn(
                &MatchState::BattleState(sandstorm_state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex,
                &pokemon_dex,
            );

            let no_weather_probability: f64 = no_weather_outcomes.iter().map(|(_, probability)| *probability).sum();
            let sandstorm_probability: f64 = sandstorm_outcomes.iter().map(|(_, probability)| *probability).sum();
            let no_weather_hit_damage = no_weather_outcomes
                .iter()
                .find_map(|(state, _)| match state {
                    MatchState::BattleState(bs) if bs.p2_active_mons[0].hp < no_weather_initial_hp => {
                        Some(no_weather_initial_hp - bs.p2_active_mons[0].hp)
                    }
                    _ => None,
                })
                .expect("expected a Bubble Beam hit branch without weather");
            let sandstorm_hit_damage = sandstorm_outcomes
                .iter()
                .find_map(|(state, _)| match state {
                    MatchState::BattleState(bs) if bs.p2_active_mons[0].hp < sandstorm_initial_hp => {
                        Some(sandstorm_initial_hp - bs.p2_active_mons[0].hp)
                    }
                    _ => None,
                })
                .expect("expected a Bubble Beam hit branch in sandstorm");

            assert!((no_weather_probability - 1.0).abs() < 1e-9);
            assert!((sandstorm_probability - 1.0).abs() < 1e-9);
            assert_eq!(no_weather_hit_damage, 98);
            assert_eq!(sandstorm_hit_damage, 68);
        }

        #[test]
        fn sandstorm_damage() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let make_splash_mon = |species: Species| {
                build_pokemon_state(
                    species,
                    &pokemon_dex,
                    &move_dex,
                    Some(50),
                    Some([Some(PokemonMove::Splash), None, None, None]),
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    false,
                )
            };

            let make_sand_state = |p1_species: Species, p2_species: Species| {
                let mut state = battle_state_from_lists(
                    vec![make_splash_mon(p1_species)],
                    vec![],
                    vec![make_splash_mon(p2_species)],
                    vec![],
                );
                state.weather = Some(Weather::Sandstorm);
                state.weather_turns = Some(5);
                state
            };

            let nonimmune_state = make_sand_state(Species::Primarina, Species::Sneasler);
            let p1_initial_hp = nonimmune_state.p1_active_mons[0].hp;
            let p2_initial_hp = nonimmune_state.p2_active_mons[0].hp;

            let nonimmune_outcomes = run_single_turn(
                &MatchState::BattleState(nonimmune_state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex,
                &pokemon_dex,
            );

            let (nonimmune_final_state, probability) = extract_battle_state(nonimmune_outcomes);
            assert!((probability - 1.0).abs() < 1e-9);
            assert_eq!(p1_initial_hp - nonimmune_final_state.p1_active_mons[0].hp, p1_initial_hp / 16);
            assert_eq!(p2_initial_hp - nonimmune_final_state.p2_active_mons[0].hp, p2_initial_hp / 16);
            assert_ne!(p1_initial_hp % 16, 0);
            assert_ne!(p2_initial_hp % 16, 0);

            for immune_species in [Species::Tyranitar, Species::Garchomp, Species::Corviknight] {
                let immune_state = make_sand_state(Species::Primarina, immune_species);
                let immune_initial_hp = immune_state.p2_active_mons[0].hp;

                let immune_outcomes = run_single_turn(
                    &MatchState::BattleState(immune_state),
                    &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                    &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                    &move_dex,
                    &pokemon_dex,
                );

                let (immune_final_state, probability) = extract_battle_state(immune_outcomes);
                assert!((probability - 1.0).abs() < 1e-9);
                assert_eq!(immune_final_state.p2_active_mons[0].hp, immune_initial_hp);
            }
        }

        #[test]
        fn sun_damage() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let make_sneasler = || {
                build_pokemon_state(
                    Species::Sneasler,
                    &pokemon_dex,
                    &move_dex,
                    Some(50),
                    Some([None, None, None, None]),
                    None,
                    None,
                    Some(Nature::Hardy),
                    None,
                    None,
                    Some([252, 0, 0, 0, 0, 0]),
                    None,
                    false,
                )
            };

            let make_state = |weather: Option<Weather>| {
                let weather_turns = weather.as_ref().map(|_| 5);

                let rotom_heat = build_pokemon_state(
                    Species::RotomHeat,
                    &pokemon_dex,
                    &move_dex,
                    Some(50),
                    Some([Some(PokemonMove::Overheat), None, None, None]),
                    None,
                    None,
                    Some(Nature::Hardy),
                    None,
                    None,
                    Some([0, 0, 0, 0, 0, 0]),
                    None,
                    false,
                );

                let mut state = battle_state_from_lists(vec![rotom_heat], vec![], vec![make_sneasler()], vec![]);
                state.weather = weather;
                state.weather_turns = weather_turns;
                state
            };

            let no_weather_state = make_state(None);
            let sun_state = make_state(Some(Weather::Sun));

            let no_weather_initial_hp = no_weather_state.p2_active_mons[0].hp;
            let sun_initial_hp = sun_state.p2_active_mons[0].hp;

            let no_weather_outcomes = run_single_turn(
                &MatchState::BattleState(no_weather_state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex,
                &pokemon_dex,
            );

            let sun_outcomes = run_single_turn(
                &MatchState::BattleState(sun_state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex,
                &pokemon_dex,
            );

            let no_weather_probability: f64 = no_weather_outcomes.iter().map(|(_, probability)| *probability).sum();
            let sun_probability: f64 = sun_outcomes.iter().map(|(_, probability)| *probability).sum();

            let no_weather_hit_damage = no_weather_outcomes
                .iter()
                .find_map(|(state, _)| match state {
                    MatchState::BattleState(bs) if bs.p2_active_mons[0].hp < no_weather_initial_hp => {
                        Some(no_weather_initial_hp - bs.p2_active_mons[0].hp)
                    }
                    _ => None,
                })
                .expect("expected an Overheat hit branch outside sun");
            let sun_hit_damage = sun_outcomes
                .iter()
                .find_map(|(state, _)| match state {
                    MatchState::BattleState(bs) if bs.p2_active_mons[0].hp < sun_initial_hp => {
                        Some(sun_initial_hp - bs.p2_active_mons[0].hp)
                    }
                    _ => None,
                })
                .expect("expected an Overheat hit branch in sun");

            assert!((no_weather_probability - 1.0).abs() < 1e-9);
            assert!((sun_probability - 1.0).abs() < 1e-9);
            assert_eq!(no_weather_hit_damage, 100);
            assert_eq!(sun_hit_damage, 150);
        }

        #[test]
        fn weather_ball() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let make_sneasler = || {
                build_pokemon_state(
                    Species::Sneasler,
                    &pokemon_dex,
                    &move_dex,
                    Some(50),
                    Some([None, None, None, None]),
                    None,
                    None,
                    Some(Nature::Hardy),
                    None,
                    None,
                    Some([252, 0, 0, 0, 0, 0]),
                    None,
                    false,
                )
            };

            let make_state = |weather: Option<Weather>| {
                let weather_turns = weather.as_ref().map(|_| 5);

                let pelipper = build_pokemon_state(
                    Species::Pelipper,
                    &pokemon_dex,
                    &move_dex,
                    Some(50),
                    Some([Some(PokemonMove::WeatherBall), None, None, None]),
                    None,
                    None,
                    Some(Nature::Hardy),
                    None,
                    None,
                    Some([0, 0, 0, 0, 0, 0]),
                    None,
                    false,
                );

                let mut state = battle_state_from_lists(vec![pelipper], vec![], vec![make_sneasler()], vec![]);
                state.weather = weather;
                state.weather_turns = weather_turns;
                state
            };

            let no_weather_state = make_state(None);
            let rain_state = make_state(Some(Weather::Rain));
            let sun_state = make_state(Some(Weather::Sun));

            let no_weather_initial_hp = no_weather_state.p2_active_mons[0].hp;
            let rain_initial_hp = rain_state.p2_active_mons[0].hp;
            let sun_initial_hp = sun_state.p2_active_mons[0].hp;

            let no_weather_outcomes = run_single_turn(
                &MatchState::BattleState(no_weather_state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex,
                &pokemon_dex,
            );

            let rain_outcomes = run_single_turn(
                &MatchState::BattleState(rain_state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex,
                &pokemon_dex,
            );

            let sun_outcomes = run_single_turn(
                &MatchState::BattleState(sun_state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex,
                &pokemon_dex,
            );

            let no_weather_probability: f64 = no_weather_outcomes.iter().map(|(_, probability)| *probability).sum();
            let rain_probability: f64 = rain_outcomes.iter().map(|(_, probability)| *probability).sum();
            let sun_probability: f64 = sun_outcomes.iter().map(|(_, probability)| *probability).sum();
            let (no_weather_final_state, _) = extract_battle_state(no_weather_outcomes);
            let (rain_final_state, _) = extract_battle_state(rain_outcomes);
            let (sun_final_state, _) = extract_battle_state(sun_outcomes);

            assert!((no_weather_probability - 1.0).abs() < 1e-9);
            assert!((rain_probability - 1.0).abs() < 1e-9);
            assert!((sun_probability - 1.0).abs() < 1e-9);
            assert_eq!(no_weather_initial_hp - no_weather_final_state.p2_active_mons[0].hp, 24);
            assert_eq!(rain_initial_hp - rain_final_state.p2_active_mons[0].hp, 70);
            assert_eq!(sun_initial_hp - sun_final_state.p2_active_mons[0].hp, 47);
        }

        #[test]
        fn rain_damage() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let make_sneasler = || {
                build_pokemon_state(
                    Species::Sneasler,
                    &pokemon_dex,
                    &move_dex,
                    Some(50),
                    Some([None, None, None, None]),
                    None,
                    None,
                    Some(Nature::Hardy),
                    None,
                    None,
                    Some([252, 0, 0, 0, 0, 0]),
                    None,
                    false,
                )
            };

            let make_state = |weather: Option<Weather>| {
                let weather_turns = weather.as_ref().map(|_| 5);

                let rotom_wash = build_pokemon_state(
                    Species::RotomWash,
                    &pokemon_dex,
                    &move_dex,
                    Some(50),
                    Some([Some(PokemonMove::HydroPump), None, None, None]),
                    None,
                    None,
                    Some(Nature::Hardy),
                    None,
                    None,
                    Some([0, 0, 0, 0, 0, 0]),
                    None,
                    false,
                );

                let mut state = battle_state_from_lists(vec![rotom_wash], vec![], vec![make_sneasler()], vec![]);
                state.weather = weather;
                state.weather_turns = weather_turns;
                state
            };

            let no_weather_state = make_state(None);
            let rain_state = make_state(Some(Weather::Rain));
            let sun_state = make_state(Some(Weather::Sun));

            let no_weather_initial_hp = no_weather_state.p2_active_mons[0].hp;
            let rain_initial_hp = rain_state.p2_active_mons[0].hp;
            let sun_initial_hp = sun_state.p2_active_mons[0].hp;

            let no_weather_outcomes = run_single_turn(
                &MatchState::BattleState(no_weather_state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex,
                &pokemon_dex,
            );

            let rain_outcomes = run_single_turn(
                &MatchState::BattleState(rain_state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex,
                &pokemon_dex,
            );

            let sun_outcomes = run_single_turn(
                &MatchState::BattleState(sun_state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex,
                &pokemon_dex,
            );

            let no_weather_probability: f64 = no_weather_outcomes.iter().map(|(_, probability)| *probability).sum();
            let rain_probability: f64 = rain_outcomes.iter().map(|(_, probability)| *probability).sum();
            let sun_probability: f64 = sun_outcomes.iter().map(|(_, probability)| *probability).sum();

            let no_weather_hit_damage = no_weather_outcomes
                .iter()
                .find_map(|(state, _)| match state {
                    MatchState::BattleState(bs) if bs.p2_active_mons[0].hp < no_weather_initial_hp => {
                        Some(no_weather_initial_hp - bs.p2_active_mons[0].hp)
                    }
                    _ => None,
                })
                .expect("expected a Hydro Pump hit branch outside weather");
            let rain_hit_damage = rain_outcomes
                .iter()
                .find_map(|(state, _)| match state {
                    MatchState::BattleState(bs) if bs.p2_active_mons[0].hp < rain_initial_hp => {
                        Some(rain_initial_hp - bs.p2_active_mons[0].hp)
                    }
                    _ => None,
                })
                .expect("expected a Hydro Pump hit branch in rain");
            let sun_hit_damage = sun_outcomes
                .iter()
                .find_map(|(state, _)| match state {
                    MatchState::BattleState(bs) if bs.p2_active_mons[0].hp < sun_initial_hp => {
                        Some(sun_initial_hp - bs.p2_active_mons[0].hp)
                    }
                    _ => None,
                })
                .expect("expected a Hydro Pump hit branch in sun");

            assert!((no_weather_probability - 1.0).abs() < 1e-9);
            assert!((rain_probability - 1.0).abs() < 1e-9);
            assert!((sun_probability - 1.0).abs() < 1e-9);
            assert_eq!(no_weather_hit_damage, 85);
            assert_eq!(rain_hit_damage, 127);
            assert_eq!(sun_hit_damage, 42);
        }

        #[test]
        fn extreme_weather() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let cases = vec![
                (
                    Species::GroudonPrimal,
                    Ability::DesolateLand,
                    Species::Pelipper,
                    Ability::Drizzle,
                    Weather::ExtremeSunlight,
                ),
                (
                    Species::KyogrePrimal,
                    Ability::PrimordialSea,
                    Species::Torkoal,
                    Ability::Drought,
                    Weather::HeavyRain,
                ),
                (
                    Species::RayquazaMega,
                    Ability::DeltaStream,
                    Species::Torkoal,
                    Ability::Drought,
                    Weather::StrongWinds,
                ),
                (
                    Species::RayquazaMega,
                    Ability::DeltaStream,
                    Species::GroudonPrimal,
                    Ability::DesolateLand,
                    Weather::ExtremeSunlight,
                ),
            ];

            for (p1_species, p1_ability, p2_species, p2_ability, expected_weather) in cases {
                let p1_mon = build_pokemon_state(
                    p1_species.clone(),
                    &pokemon_dex,
                    &move_dex,
                    None,
                    Some([Some(PokemonMove::Splash), None, None, None]),
                    None,
                    Some(p1_ability.clone()),
                    Some(Nature::Hardy),
                    None,
                    None,
                    Some([0, 0, 0, 0, 0, 0]),
                    None,
                    false,
                );

                let p2_mon = build_pokemon_state(
                    p2_species.clone(),
                    &pokemon_dex,
                    &move_dex,
                    None,
                    Some([Some(PokemonMove::Splash), None, None, None]),
                    None,
                    Some(p2_ability.clone()),
                    Some(Nature::Hardy),
                    None,
                    None,
                    Some([0, 0, 0, 0, 0, 0]),
                    None,
                    false,
                );

                let state = battle_state_from_lists(vec![p1_mon], vec![], vec![p2_mon], vec![]);

                assert_eq!(state.p1_active_mons[0].species, p1_species);
                assert_eq!(state.p2_active_mons[0].species, p2_species);
                assert_eq!(state.p1_active_mons[0].ability, p1_ability);
                assert_eq!(state.p2_active_mons[0].ability, p2_ability);
                assert_eq!(state.weather, Some(expected_weather));
            }
        }

        #[test]
        fn weather_speed_abilities() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let cases = vec![
                (
                    Species::Basculegion,
                    PokemonMove::WaveCrash,
                    Ability::SwiftSwim,
                    Nature::Jolly,
                    Weather::Rain,
                ),
                (
                    Species::Venusaur,
                    PokemonMove::SludgeBomb,
                    Ability::Chlorophyll,
                    Nature::Timid,
                    Weather::Sun,
                ),
                (
                    Species::Excadrill,
                    PokemonMove::Earthquake,
                    Ability::SandRush,
                    Nature::Jolly,
                    Weather::Sandstorm,
                ),
            ];

            for (species, move_name, weather_ability, speed_nature, weather) in cases {
                let make_state = |weather: Option<Weather>| {
                    let p1_mon = build_pokemon_state(
                        species.clone(),
                        &pokemon_dex,
                        &move_dex,
                        Some(50),
                        Some([Some(move_name.clone()), None, None, None]),
                        None,
                        Some(weather_ability.clone()),
                        None,
                        None,
                        Some(crate::dex_data::PokemonType::Normal),
                        Some([0, 0, 0, 0, 0, 0]),
                        None,
                        false,
                    );

                    let p2_mon = build_pokemon_state(
                        species.clone(),
                        &pokemon_dex,
                        &move_dex,
                        Some(50),
                        Some([Some(move_name.clone()), None, None, None]),
                        None,
                        Some(Ability::None),
                        Some(speed_nature),
                        None,
                        Some(crate::dex_data::PokemonType::Normal),
                        Some([0, 0, 0, 0, 0, 0]),
                        None,
                        false,
                    );

                    let weather_turns = weather.as_ref().map(|_| 5);
                    let mut state = battle_state_from_lists(vec![p1_mon], vec![], vec![p2_mon], vec![]);
                    state.weather = weather;
                    state.weather_turns = weather_turns;
                    state.p1_active_mons[0].hp = 1;
                    state.p2_active_mons[0].hp = 1;
                    state
                };

                let weather_outcomes = run_single_turn(
                    &MatchState::BattleState(make_state(Some(weather))),
                    &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                    &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                    &move_dex,
                    &pokemon_dex,
                );

                assert_eq!(weather_outcomes.len(), 1);
                assert!(matches!(weather_outcomes[0].0, MatchState::GameOverState { winner: Player::P1 }));

                let clear_outcomes = run_single_turn(
                    &MatchState::BattleState(make_state(None)),
                    &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                    &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                    &move_dex,
                    &pokemon_dex,
                );

                assert_eq!(clear_outcomes.len(), 1);
                assert!(matches!(clear_outcomes[0].0, MatchState::GameOverState { winner: Player::P2 }));
            }
        }
    }

    mod terrain {
        use super::*;

        #[test]
        fn terrain_seeds() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let cases = [
                (Terrain::ElectricTerrain, Ability::ElectricSurge, Item::ElectricSeed, Species::Pincurchin, 1usize),
                (Terrain::GrassyTerrain, Ability::GrassySurge, Item::GrassySeed, Species::Rillaboom, 1usize),
                (Terrain::MistyTerrain, Ability::MistySurge, Item::MistySeed, Species::TapuFini, 3usize),
                (Terrain::PsychicTerrain, Ability::PsychicSurge, Item::PsychicSeed, Species::IndeedeeF, 3usize),
            ];

            for (terrain, ability, seed_item, setter_species, boost_index) in cases {
                let setter = build_pokemon_state(
                    setter_species,
                    &pokemon_dex,
                    &move_dex,
                    Some(50),
                    Some([Some(PokemonMove::Splash), None, None, None]),
                    None,
                    Some(ability),
                    None,
                    None,
                    Some(crate::dex_data::PokemonType::Normal),
                    None,
                    None,
                    false,
                );

                let seeded = build_pokemon_state(
                    Species::Snorlax,
                    &pokemon_dex,
                    &move_dex,
                    Some(50),
                    Some([Some(PokemonMove::Splash), None, None, None]),
                    None,
                    Some(Ability::None),
                    None,
                    Some(seed_item),
                    Some(crate::dex_data::PokemonType::Normal),
                    None,
                    None,
                    false,
                );

                let state = battle_state_from_lists(vec![setter], vec![], vec![seeded], vec![]);
                let active = &state.p2_active_mons[0];

                assert_eq!(state.terrain, Some(terrain));
                assert_eq!(active.item, Item::None);
                assert_eq!(active.boosts[boost_index], 1);
            }
        }

        #[test]
        fn terrain_passives() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let electric_state = MatchState::BattleState({
                let mut state = battle_state_from_lists(
                    vec![build_pokemon_state(
                        Species::Amoonguss,
                        &pokemon_dex,
                        &move_dex,
                        Some(50),
                        Some([Some(PokemonMove::Spore), None, None, None]),
                        None,
                        Some(Ability::None),
                        None,
                        None,
                        Some(crate::dex_data::PokemonType::Grass),
                        None,
                        None,
                        false,
                    )],
                    vec![],
                    vec![build_pokemon_state(
                        Species::Snorlax,
                        &pokemon_dex,
                        &move_dex,
                        Some(50),
                        Some([Some(PokemonMove::Splash), None, None, None]),
                        None,
                        Some(Ability::None),
                        None,
                        None,
                        Some(crate::dex_data::PokemonType::Normal),
                        None,
                        None,
                        false,
                    )],
                    vec![],
                );
                state.terrain = Some(Terrain::ElectricTerrain);
                state.terrain_turns = Some(5);
                state
            });

            let electric_outcomes = run_single_turn(
                &electric_state,
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex,
                &pokemon_dex,
            );

            assert!(electric_outcomes.iter().all(|(state, _)| {
                matches!(state, MatchState::BattleState(bs) if bs.p2_active_mons[0].status.is_none())
            }));

            let mut grassy_attacker = build_pokemon_state(
                Species::Snorlax,
                &pokemon_dex,
                &move_dex,
                Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None,
                Some(Ability::None),
                None,
                None,
                Some(crate::dex_data::PokemonType::Normal),
                None,
                None,
                false,
            );
            grassy_attacker.hp = grassy_attacker.hp.saturating_sub(16);
            let grassy_initial_hp = grassy_attacker.hp;

            let grassy_state = MatchState::BattleState({
                let mut state = battle_state_from_lists(
                    vec![grassy_attacker],
                    vec![],
                    vec![build_pokemon_state(
                        Species::Snorlax,
                        &pokemon_dex,
                        &move_dex,
                        Some(50),
                        Some([Some(PokemonMove::Splash), None, None, None]),
                        None,
                        Some(Ability::None),
                        None,
                        None,
                        Some(crate::dex_data::PokemonType::Normal),
                        None,
                        None,
                        false,
                    )],
                    vec![],
                );
                state.terrain = Some(Terrain::GrassyTerrain);
                state.terrain_turns = Some(5);
                state
            });

            let grassy_outcomes = run_single_turn(
                &grassy_state,
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex,
                &pokemon_dex,
            );

            assert!(grassy_outcomes.iter().all(|(state, _)| {
                matches!(state, MatchState::BattleState(bs) if bs.p1_active_mons[0].hp > grassy_initial_hp)
            }));

            let misty_state = MatchState::BattleState({
                let mut state = battle_state_from_lists(
                    vec![build_pokemon_state(
                        Species::Amoonguss,
                        &pokemon_dex,
                        &move_dex,
                        Some(50),
                        Some([Some(PokemonMove::ConfuseRay), None, None, None]),
                        None,
                        Some(Ability::None),
                        None,
                        None,
                        Some(crate::dex_data::PokemonType::Grass),
                        None,
                        None,
                        false,
                    )],
                    vec![],
                    vec![build_pokemon_state(
                        Species::Snorlax,
                        &pokemon_dex,
                        &move_dex,
                        Some(50),
                        Some([Some(PokemonMove::Splash), None, None, None]),
                        None,
                        Some(Ability::None),
                        None,
                        None,
                        Some(crate::dex_data::PokemonType::Normal),
                        None,
                        None,
                        false,                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                            
                    )],
                    vec![],
                );
                state.terrain = Some(Terrain::MistyTerrain);
                state.terrain_turns = Some(5);
                state
            });

            let misty_outcomes = run_single_turn(
                &misty_state,
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex,
                &pokemon_dex,
            );

            assert!(misty_outcomes.iter().all(|(state, _)| {
                matches!(state, MatchState::BattleState(bs) if bs.p2_active_mons[0].volatiles.iter().all(|volatile| !matches!(volatile, VolatileStatusState::MoveStatus(VolatileStatus::Confusion, _) | VolatileStatusState::TurnStatus(VolatileStatus::Confusion, _))))
            }));
        }

        #[test]
        fn expanding_force() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let priority_block_state = MatchState::BattleState({
                let mut state = battle_state_from_lists(
                    vec![build_pokemon_state(
                        Species::Pikachu,
                        &pokemon_dex,
                        &move_dex,
                        Some(50),
                        Some([Some(PokemonMove::QuickAttack), None, None, None]),
                        None,
                        Some(Ability::None),
                        None,
                        None,
                        Some(crate::dex_data::PokemonType::Electric),
                        None,
                        None,
                        false,
                    )],
                    vec![],
                    vec![build_pokemon_state(
                        Species::Snorlax,
                        &pokemon_dex,
                        &move_dex,
                        Some(50),
                        Some([Some(PokemonMove::Splash), None, None, None]),
                        None,
                        Some(Ability::None),
                        None,
                        None,
                        Some(crate::dex_data::PokemonType::Normal),
                        None,
                        None,
                        false,
                    )],
                    vec![],
                );
                state.terrain = Some(Terrain::PsychicTerrain);
                state.terrain_turns = Some(5);
                state
            });

            let priority_outcomes = run_single_turn(
                &priority_block_state,
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex,
                &pokemon_dex,
            );

            assert!(priority_outcomes.iter().all(|(state, _)| {
                matches!(state, MatchState::BattleState(bs) if bs.p2_active_mons[0].hp == bs.p2_active_mons[0].stats[0])
            }));

            let expanding_force_state = MatchState::BattleState({
                let mut state = battle_state_from_lists(
                    vec![
                        build_pokemon_state(
                            Species::IndeedeeF,
                            &pokemon_dex,
                            &move_dex,
                            Some(50),
                            Some([Some(PokemonMove::ExpandingForce), None, None, None]),
                            None,
                            Some(Ability::None),
                            None,
                            None,
                            Some(crate::dex_data::PokemonType::Psychic),
                            None,
                            None,
                            false,
                        ),
                        build_pokemon_state(
                            Species::Snorlax,
                            &pokemon_dex,
                            &move_dex,
                            Some(50),
                            Some([Some(PokemonMove::Splash), None, None, None]),
                            None,
                            Some(Ability::None),
                            None,
                            None,
                            Some(crate::dex_data::PokemonType::Normal),
                            None,
                            None,
                            false,
                        ),
                    ],
                    vec![],
                    vec![
                        build_pokemon_state(
                            Species::Snorlax,
                            &pokemon_dex,
                            &move_dex,
                            Some(50),
                            Some([Some(PokemonMove::Splash), None, None, None]),
                            None,
                            Some(Ability::None),
                            None,
                            None,
                            Some(crate::dex_data::PokemonType::Normal),
                            None,
                            None,
                            false,
                        ),
                        build_pokemon_state(
                            Species::Snorlax,
                            &pokemon_dex,
                            &move_dex,
                            Some(50),
                            Some([Some(PokemonMove::Splash), None, None, None]),
                            None,
                            Some(Ability::None),
                            None,
                            None,
                            Some(crate::dex_data::PokemonType::Normal),
                            None,
                            None,
                            false,
                        ),
                    ],
                    vec![],
                );
                state.terrain = Some(Terrain::PsychicTerrain);
                state.terrain_turns = Some(5);
                state
            });

            let expanding_force_outcomes = run_single_turn(
                &expanding_force_state,
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0, 0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0, 0])),
                &move_dex,
                &pokemon_dex,
            );

            assert!(expanding_force_outcomes.iter().all(|(state, _)| {
                matches!(state, MatchState::BattleState(bs) if bs.p2_active_mons.iter().all(|mon| mon.hp < mon.stats[0]))
            }));
        }

        #[test]
        fn terrain_clear_moves() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let nature_power_state = MatchState::BattleState({
                let mut state = battle_state_from_lists(
                    vec![build_pokemon_state(
                        Species::Clefable,
                        &pokemon_dex,
                        &move_dex,
                        Some(50),
                        Some([Some(PokemonMove::NaturePower), None, None, None]),
                        None,
                        Some(Ability::None),
                        None,
                        None,
                        Some(crate::dex_data::PokemonType::Fairy),
                        None,
                        None,
                        false,
                    )],
                    vec![],
                    vec![build_pokemon_state(
                        Species::Golem,
                        &pokemon_dex,
                        &move_dex,
                        Some(50),
                        Some([Some(PokemonMove::Splash), None, None, None]),
                        None,
                        Some(Ability::None),
                        None,
                        None,
                        Some(crate::dex_data::PokemonType::Ground),
                        None,
                        None,
                        false,
                    )],
                    vec![],
                );
                state.terrain = Some(Terrain::ElectricTerrain);
                state.terrain_turns = Some(5);
                state
            });

            let nature_power_outcomes = run_single_turn(
                &nature_power_state,
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex,
                &pokemon_dex,
            );

            assert!(nature_power_outcomes.iter().all(|(state, _)| {
                matches!(state, MatchState::BattleState(bs) if bs.p2_active_mons[0].hp == bs.p2_active_mons[0].stats[0])
            }));

            for move_name in [PokemonMove::IceSpinner, PokemonMove::SteelRoller] {
                let terrain_state = MatchState::BattleState({
                    let mut state = battle_state_from_lists(
                        vec![build_pokemon_state(
                            Species::Snorlax,
                            &pokemon_dex,
                            &move_dex,
                            Some(50),
                            Some([Some(move_name), None, None, None]),
                            None,
                            Some(Ability::None),
                            None,
                            None,
                            Some(crate::dex_data::PokemonType::Normal),
                            None,
                            None,
                            false,
                        )],
                        vec![],
                        vec![build_pokemon_state(
                            Species::Snorlax,
                            &pokemon_dex,
                            &move_dex,
                            Some(50),
                            Some([Some(PokemonMove::Splash), None, None, None]),
                            None,
                            Some(Ability::None),
                            None,
                            None,
                            Some(crate::dex_data::PokemonType::Normal),
                            None,
                            None,
                            false,
                        )],
                        vec![],
                    );
                    state.terrain = Some(Terrain::PsychicTerrain);
                    state.terrain_turns = Some(5);
                    state
                });

                let outcomes = run_single_turn(
                    &terrain_state,
                    &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                    &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                    &move_dex,
                    &pokemon_dex,
                );

                assert!(outcomes.iter().all(|(state, _)| {
                    matches!(state, MatchState::BattleState(bs) if bs.terrain.is_none())
                }));
            }

            let ground_immunity_cases = vec![
                build_pokemon_state(
                    Species::Snorlax,
                    &pokemon_dex,
                    &move_dex,
                    Some(50),
                    Some([Some(PokemonMove::Splash), None, None, None]),
                    None,
                    Some(Ability::Levitate),
                    None,
                    None,
                    Some(crate::dex_data::PokemonType::Normal),
                    None,
                    None,
                    false,
                ),
                build_pokemon_state(
                    Species::Snorlax,
                    &pokemon_dex,
                    &move_dex,
                    Some(50),
                    Some([Some(PokemonMove::Splash), None, None, None]),
                    None,
                    Some(Ability::None),
                    None,
                    Some(Item::AirBalloon),
                    Some(crate::dex_data::PokemonType::Normal),
                    None,
                    None,
                    false,
                ),
                build_pokemon_state(
                    Species::Snorlax,
                    &pokemon_dex,
                    &move_dex,
                    Some(50),
                    Some([Some(PokemonMove::Splash), None, None, None]),
                    None,
                    Some(Ability::None),
                    None,
                    None,
                    Some(crate::dex_data::PokemonType::Normal),
                    None,
                    None,
                    false,
                ),
                build_pokemon_state(
                    Species::Snorlax,
                    &pokemon_dex,
                    &move_dex,
                    Some(50),
                    Some([Some(PokemonMove::Splash), None, None, None]),
                    None,
                    Some(Ability::None),
                    None,
                    None,
                    Some(crate::dex_data::PokemonType::Normal),
                    None,
                    None,
                    false,
                ),
            ];

            let mut magnet_rise_target = ground_immunity_cases[2].clone();
            magnet_rise_target.volatiles.push(VolatileStatusState::TurnStatus(VolatileStatus::MagnetRise, 0));
            let mut telekinesis_target = ground_immunity_cases[3].clone();
            telekinesis_target.volatiles.push(VolatileStatusState::TurnStatus(VolatileStatus::Telekinesis, 0));

            for target in [
                ground_immunity_cases[0].clone(),
                ground_immunity_cases[1].clone(),
                magnet_rise_target,
                telekinesis_target,
            ] {
                let outcomes = run_single_turn(
                    &MatchState::BattleState(battle_state_from_lists(
                        vec![build_pokemon_state(
                            Species::Garchomp,
                            &pokemon_dex,
                            &move_dex,
                            Some(50),
                            Some([Some(PokemonMove::Earthquake), None, None, None]),
                            None,
                            Some(Ability::None),
                            None,
                            None,
                            Some(crate::dex_data::PokemonType::Ground),
                            None,
                            None,
                            false,
                        )],
                        vec![],
                        vec![target],
                        vec![],
                    )),
                    &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                    &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                    &move_dex,
                    &pokemon_dex,
                );

                assert!(outcomes.iter().all(|(state, _)| {
                    matches!(state, MatchState::BattleState(bs) if bs.p2_active_mons[0].hp == bs.p2_active_mons[0].stats[0])
                }));
            }
        }
    }

    mod abilities {
        use super::*;

        #[test]
        fn adaptability_damage() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let make_state = |ability: Ability| {
                let attacker = build_pokemon_state(
                    Species::Basculegion,
                    &pokemon_dex,
                    &move_dex,
                    Some(50),
                    Some([Some(PokemonMove::ShadowBall), None, None, None]),
                    None,
                    Some(ability),
                    Some(Nature::Modest),
                    None,
                    None,
                    Some([0, 0, 0, 0, 0, 0]),
                    None,
                    false,
                );

                battle_state_from_lists(vec![attacker.clone()], vec![], vec![attacker], vec![])
            };

            let no_ability_state = make_state(Ability::None);
            let adaptability_state = make_state(Ability::Adaptability);

            let expected_no_ability_hp = simulator_helpers::get_pokemon_at_slot(&no_ability_state, FieldSlot { player: Player::P2, slot_index: 0 }).unwrap().hp - 114;
            let expected_adaptability_hp = simulator_helpers::get_pokemon_at_slot(&adaptability_state, FieldSlot { player: Player::P2, slot_index: 0 }).unwrap().hp - 152;

            let no_ability_outcomes = simulate_turn(
                &MatchState::BattleState(no_ability_state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex,
                &pokemon_dex,
                false,
                1,
            );

            let adaptability_outcomes = simulate_turn(
                &MatchState::BattleState(adaptability_state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex,
                &pokemon_dex,
                false,
                1,
            );

            let no_ability_total_probability: f64 = no_ability_outcomes.iter().map(|(_, p)| *p).sum();
            let adaptability_total_probability: f64 = adaptability_outcomes.iter().map(|(_, p)| *p).sum();

            assert!((no_ability_total_probability - 1.0).abs() < 1e-9);
            assert!((adaptability_total_probability - 1.0).abs() < 1e-9);
            assert!(no_ability_outcomes.iter().all(|(state, _)| matches!(state, MatchState::BattleState(bs) if bs.p2_active_mons[0].hp == expected_no_ability_hp)));
            assert!(adaptability_outcomes.iter().all(|(state, _)| matches!(state, MatchState::BattleState(bs) if bs.p2_active_mons[0].hp == expected_adaptability_hp)));
        }

        #[test]
        fn dry_skin_moves() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let make_heliolisk = |ability: Ability, moves: [Option<PokemonMove>; 4]| {
                build_pokemon_state(
                    Species::Heliolisk,
                    &pokemon_dex,
                    &move_dex,
                    Some(50),
                    Some(moves),
                    None,
                    Some(ability),
                    Some(Nature::Hardy),
                    None,
                    None,
                    Some([0, 0, 0, 0, 0, 0]),
                    None,
                    false,
                )
            };

            let make_incineroar = |moves: [Option<PokemonMove>; 4]| {
                build_pokemon_state(
                    Species::Incineroar,
                    &pokemon_dex,
                    &move_dex,
                    Some(50),
                    Some(moves),
                    None,
                    Some(Ability::Blaze),
                    Some(Nature::Hardy),
                    None,
                    None,
                    Some([0, 0, 0, 0, 0, 0]),
                    None,
                    false,
                )
            };

            let make_primarina = |moves: [Option<PokemonMove>; 4]| {
                build_pokemon_state(
                    Species::Primarina,
                    &pokemon_dex,
                    &move_dex,
                    Some(50),
                    Some(moves),
                    None,
                    Some(Ability::Torrent),
                    Some(Nature::Hardy),
                    None,
                    None,
                    Some([0, 0, 0, 0, 0, 0]),
                    None,
                    false,
                )
            };

            let make_water_state = |ability: Ability| {
                let mut state = battle_state_from_lists(
                    vec![make_heliolisk(ability, [Some(PokemonMove::Splash), None, None, None])],
                    vec![],
                    vec![make_primarina([Some(PokemonMove::BubbleBeam), None, None, None])],
                    vec![],
                );
                let target_hp = state.p1_active_mons[0].hp;
                let heal_amount = target_hp / 4;
                state.p1_active_mons[0].hp = target_hp.saturating_sub(heal_amount);
                state
            };

            let dry_skin_water_state = make_water_state(Ability::DrySkin);
            let dry_skin_water_initial_hp = dry_skin_water_state.p1_active_mons[0].hp;
            let dry_skin_water_max_hp = dry_skin_water_state.p1_active_mons[0].stats[0];

            let dry_skin_water_outcomes = run_single_turn(
                &MatchState::BattleState(dry_skin_water_state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex,
                &pokemon_dex,
            );

            let dry_skin_water_expected_hp = dry_skin_water_initial_hp + (dry_skin_water_max_hp / 4);
            assert!((dry_skin_water_outcomes.iter().map(|(_, probability)| *probability).sum::<f64>() - 1.0).abs() < 1e-9);
            assert!(dry_skin_water_outcomes.iter().all(|(state, _)| matches!(state, MatchState::BattleState(bs) if bs.p1_active_mons[0].hp == dry_skin_water_expected_hp)));

            let no_dry_skin_state = battle_state_from_lists(
                vec![make_heliolisk(Ability::SandVeil, [Some(PokemonMove::HyperVoice), None, None, None])],
                vec![],
                vec![make_incineroar([Some(PokemonMove::FlameCharge), None, None, None])],
                vec![],
            );
            let no_dry_skin_initial_hp = no_dry_skin_state.p1_active_mons[0].hp;

            let no_dry_skin_outcomes = run_single_turn(
                &MatchState::BattleState(no_dry_skin_state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex,
                &pokemon_dex,
            );

            let dry_skin_state = battle_state_from_lists(
                vec![make_heliolisk(Ability::DrySkin, [Some(PokemonMove::Bubble), None, None, None])],
                vec![],
                vec![make_incineroar([Some(PokemonMove::FlameCharge), None, None, None])],
                vec![],
            );
            let dry_skin_initial_hp = dry_skin_state.p1_active_mons[0].hp;

            let dry_skin_outcomes = run_single_turn(
                &MatchState::BattleState(dry_skin_state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex,
                &pokemon_dex,
            );

            let no_dry_skin_hit_damage = no_dry_skin_outcomes
                .iter()
                .find_map(|(state, _)| match state {
                    MatchState::BattleState(bs) if bs.p1_active_mons[0].hp < no_dry_skin_initial_hp => {
                        Some(no_dry_skin_initial_hp - bs.p1_active_mons[0].hp)
                    }
                    _ => None,
                })
                .expect("expected a Flame Charge hit branch without Dry Skin");
            let dry_skin_hit_damage = dry_skin_outcomes
                .iter()
                .find_map(|(state, _)| match state {
                    MatchState::BattleState(bs) if bs.p1_active_mons[0].hp < dry_skin_initial_hp => {
                        Some(dry_skin_initial_hp - bs.p1_active_mons[0].hp)
                    }
                    _ => None,
                })
                .expect("expected a Fire Fang hit branch with Dry Skin");

            assert!((no_dry_skin_outcomes.iter().map(|(_, probability)| *probability).sum::<f64>() - 1.0).abs() < 1e-9);
            assert!((dry_skin_outcomes.iter().map(|(_, probability)| *probability).sum::<f64>() - 1.0).abs() < 1e-9);
            assert_eq!(no_dry_skin_hit_damage, 58);
            assert_eq!(dry_skin_hit_damage, 72);
        }

        #[test]
        fn dry_skin_passives() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let make_heliolisk = || {
                build_pokemon_state(
                    Species::Heliolisk,
                    &pokemon_dex,
                    &move_dex,
                    Some(50),
                    Some([Some(PokemonMove::Splash), None, None, None]),
                    None,
                    Some(Ability::DrySkin),
                    Some(Nature::Hardy),
                    None,
                    None,
                    Some([0, 0, 0, 0, 0, 0]),
                    None,
                    false,
                )
            };

            let make_support_mon = || {
                build_pokemon_state(
                    Species::Magikarp,
                    &pokemon_dex,
                    &move_dex,
                    Some(50),
                    Some([Some(PokemonMove::Splash), None, None, None]),
                    None,
                    Some(Ability::SwiftSwim),
                    Some(Nature::Hardy),
                    None,
                    None,
                    Some([0, 0, 0, 0, 0, 0]),
                    None,
                    false,
                )
            };

            let make_weather_state = |weather: Weather| {
                let mut state = battle_state_from_lists(vec![make_heliolisk()], vec![], vec![make_support_mon()], vec![]);
                let max_hp = state.p1_active_mons[0].stats[0];
                let weather_hp = max_hp / 2;
                state.p1_active_mons[0].hp = weather_hp;
                state.weather = Some(weather);
                state.weather_turns = Some(5);
                (state, weather_hp, max_hp)
            };

            let (rain_state, rain_initial_hp, rain_max_hp) = make_weather_state(Weather::Rain);
            let rain_outcomes = run_single_turn(
                &MatchState::BattleState(rain_state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex,
                &pokemon_dex,
            );

            let (sun_state, sun_initial_hp, sun_max_hp) = make_weather_state(Weather::Sun);
            let sun_outcomes = run_single_turn(
                &MatchState::BattleState(sun_state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex,
                &pokemon_dex,
            );

            let rain_expected_hp = rain_initial_hp + (rain_max_hp / 8);
            let sun_expected_hp = sun_initial_hp - (sun_max_hp / 8);

            assert!((rain_outcomes.iter().map(|(_, probability)| *probability).sum::<f64>() - 1.0).abs() < 1e-9);
            assert!((sun_outcomes.iter().map(|(_, probability)| *probability).sum::<f64>() - 1.0).abs() < 1e-9);
            assert!(rain_outcomes.iter().all(|(state, _)| matches!(state, MatchState::BattleState(bs) if bs.p1_active_mons[0].hp == rain_expected_hp)));
            assert!(sun_outcomes.iter().all(|(state, _)| matches!(state, MatchState::BattleState(bs) if bs.p1_active_mons[0].hp == sun_expected_hp)));
        }

        #[test]
        fn seed_sower_sand_spit() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let seed_sower_outcomes = run_single_turn(
                &MatchState::BattleState(battle_state_from_lists(
                    vec![build_pokemon_state(
                        Species::Snorlax,
                        &pokemon_dex,
                        &move_dex,
                        Some(50),
                        Some([Some(PokemonMove::Tackle), None, None, None]),
                        None,
                        Some(Ability::None),
                        None,
                        None,
                        Some(crate::dex_data::PokemonType::Normal),
                        None,
                        None,
                        false,
                    )],
                    vec![],
                    vec![build_pokemon_state(
                        Species::Snorlax,
                        &pokemon_dex,
                        &move_dex,
                        Some(50),
                        Some([Some(PokemonMove::Splash), None, None, None]),
                        None,
                        Some(Ability::SeedSower),
                        None,
                        None,
                        Some(crate::dex_data::PokemonType::Normal),
                        None,
                        None,
                        false,
                    )],
                    vec![],
                )),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex,
                &pokemon_dex,
            );

            assert!(seed_sower_outcomes.iter().any(|(state, _)| {
                matches!(state, MatchState::BattleState(bs) if bs.terrain == Some(Terrain::GrassyTerrain))
            }));

            let sand_spit_outcomes = run_single_turn(
                &MatchState::BattleState(battle_state_from_lists(
                    vec![build_pokemon_state(
                        Species::Snorlax,
                        &pokemon_dex,
                        &move_dex,
                        Some(50),
                        Some([Some(PokemonMove::Tackle), None, None, None]),
                        None,
                        Some(Ability::None),
                        None,
                        None,
                        Some(crate::dex_data::PokemonType::Normal),
                        None,
                        None,
                        false,
                    )],
                    vec![],
                    vec![build_pokemon_state(
                        Species::Snorlax,
                        &pokemon_dex,
                        &move_dex,
                        Some(50),
                        Some([Some(PokemonMove::Splash), None, None, None]),
                        None,
                        Some(Ability::SandSpit),
                        None,
                        None,
                        Some(crate::dex_data::PokemonType::Normal),
                        None,
                        None,
                        false,
                    )],
                    vec![],
                )),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex,
                &pokemon_dex,
            );

            assert!(sand_spit_outcomes.iter().any(|(state, _)| {
                matches!(state, MatchState::BattleState(bs) if bs.weather == Some(Weather::Sandstorm))
            }));
        }

        // While a Neutralizing Gas Pokémon is active it suppresses other abilities, so
        // Pelipper's Drizzle never sets rain on send-out. Once the gas Pokémon leaves the
        // field the suppression lifts and Drizzle reactivates as an on-gain ability.
        #[test]
        fn neutralizing_gas_suppresses_then_reactivates_weather() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let pelipper = build_pokemon_state(
                Species::Pelipper,
                &pokemon_dex,
                &move_dex,
                Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None,
                Some(Ability::Drizzle),
                None,
                None,
                None,
                None,
                None,
                false,
            );

            let weezing = build_pokemon_state(
                Species::WeezingGalar,
                &pokemon_dex,
                &move_dex,
                Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None,
                Some(Ability::NeutralizingGas),
                None,
                None,
                None,
                None,
                None,
                false,
            );

            // Plain back-mon with a harmless forced ability so switching it in sets no weather.
            let back_mon = build_pokemon_state(
                Species::Torkoal,
                &pokemon_dex,
                &move_dex,
                Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None,
                Some(Ability::Pressure),
                None,
                None,
                None,
                None,
                None,
                false,
            );

            let initial_state =
                battle_state_from_lists(vec![pelipper], vec![], vec![weezing], vec![back_mon]);

            // Drizzle is suppressed by the active Neutralizing Gas, so no rain on send-out.
            assert_eq!(initial_state.weather, None);

            let outcomes = run_single_turn(
                &MatchState::BattleState(initial_state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(vec![BattleCommand::Switch(SwitchCommand { party_index: 0 })]),
                &move_dex,
                &pokemon_dex,
            );

            let (state_after_turn, probability) = extract_battle_state(outcomes);
            assert!((probability - 1.0).abs() < 1e-9);
            assert_eq!(state_after_turn.p2_active_mons[0].species, Species::Torkoal);
            // Gas has left; Drizzle reactivates and sets rain.
            assert_eq!(state_after_turn.weather, Some(Weather::Rain));
        }
    }

    mod status {
        use super::*;

        mod burn {
            use super::*;

            #[test]
            fn burn_damage() {
                let pokemon_dex = pokemon_dex();
                let move_dex = move_dex();

                let mut burned_mon = build_pokemon_state(
                    Species::Shuckle,
                    &pokemon_dex,
                    &move_dex,
                    Some(50),
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
                burned_mon.status = Some(Status::Burn);

                let healthy_mon = build_pokemon_state(
                    Species::Magikarp,
                    &pokemon_dex,
                    &move_dex,
                    Some(50),
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

                let burned_initial_hp = burned_mon.hp;
                let state = battle_state_from_lists(vec![burned_mon], vec![], vec![healthy_mon], vec![]);

                let outcomes = run_single_turn(
                    &MatchState::BattleState(state),
                    &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                    &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                    &move_dex,
                    &pokemon_dex,
                );

                let (end_state, probability) = extract_battle_state(outcomes);
                assert!((probability - 1.0).abs() < 1e-9);
                assert_eq!(burned_initial_hp - end_state.p1_active_mons[0].hp, burned_initial_hp / 16);
            }

            #[test]
            fn burn_guts() {
                let pokemon_dex = pokemon_dex();
                let move_dex = move_dex();

                let make_state = |ability: Ability, status: Option<Status>, mov: PokemonMove| {
                    let mut attacker = build_pokemon_state(
                        Species::Mimikyu,
                        &pokemon_dex,
                        &move_dex,
                        Some(50),
                        Some([Some(mov), None, None, None]),
                        None,
                        Some(ability),
                        None,
                        None,
                        Some(crate::dex_data::PokemonType::Ghost),
                        None,
                        None,
                        false,
                    );
                    attacker.status = status;

                    let target = build_pokemon_state(
                        Species::Snorlax,
                        &pokemon_dex,
                        &move_dex,
                        Some(50),
                        Some([Some(PokemonMove::Splash), None, None, None]),
                        None,
                        Some(Ability::None),
                        None,
                        None,
                        Some(crate::dex_data::PokemonType::Normal),
                        None,
                        None,
                        false,
                    );

                    battle_state_from_lists(vec![attacker], vec![], vec![target], vec![])
                };

                let damage_for = |state: &BattleState, mov: PokemonMove| -> u16 {
                    let attacker_slot = FieldSlot { player: Player::P1, slot_index: 0 };
                    let target_slot = FieldSlot { player: Player::P2, slot_index: 0 };
                    let attacker = simulator_helpers::get_pokemon_at_slot(state, attacker_slot).unwrap();
                    let target = simulator_helpers::get_pokemon_at_slot(state, target_slot).unwrap();
                    let move_data = move_dex.get(&mov).unwrap();

                    let outcomes = simulator_helpers::calculate_damage_outcomes_for_target(
                        state,
                        attacker,
                        target,
                        attacker_slot,
                        target_slot,
                        move_data,
                        DamageConfig { consider_crit: false, damage_rolls: 1 },
                        1.0,
                        1.0,
                    );

                    outcomes[0].0
                };

                let healthy_shadow_state = make_state(Ability::None, None, PokemonMove::Earthquake);
                let burned_shadow_state = make_state(Ability::None, Some(Status::Burn), PokemonMove::Earthquake);
                let burned_guts_shadow_state = make_state(Ability::Guts, Some(Status::Burn), PokemonMove::Earthquake);
                let healthy_facade_state = make_state(Ability::None, None, PokemonMove::Facade);
                let burned_facade_state = make_state(Ability::None, Some(Status::Burn), PokemonMove::Facade);

                let healthy_shadow = damage_for(&healthy_shadow_state, PokemonMove::Earthquake);
                let burned_shadow = damage_for(&burned_shadow_state, PokemonMove::Earthquake);
                let burned_guts_shadow = damage_for(&burned_guts_shadow_state, PokemonMove::Earthquake);
                let healthy_facade = damage_for(&healthy_facade_state, PokemonMove::Facade);
                let burned_facade = damage_for(&burned_facade_state, PokemonMove::Facade);

                assert!(burned_shadow < healthy_shadow);
                assert!(burned_guts_shadow > burned_shadow);
                assert!(burned_facade > healthy_facade);
            }

            #[test]
            fn burn_immunities() {
                let pokemon_dex = pokemon_dex();
                let move_dex = move_dex();

                let attacker = build_pokemon_state(
                    Species::RotomHeat,
                    &pokemon_dex,
                    &move_dex,
                    Some(50),
                    Some([Some(PokemonMove::WillOWisp), None, None, None]),
                    None,
                    Some(Ability::None),
                    None,
                    None,
                    Some(crate::dex_data::PokemonType::Fire),
                    None,
                    None,
                    false,
                );

                let control_target = build_pokemon_state(
                    Species::Snorlax,
                    &pokemon_dex,
                    &move_dex,
                    Some(50),
                    Some([Some(PokemonMove::Splash), None, None, None]),
                    None,
                    Some(Ability::None),
                    None,
                    None,
                    Some(crate::dex_data::PokemonType::Normal),
                    None,
                    None,
                    false,
                );

                let control_outcomes = run_single_turn(
                    &MatchState::BattleState(battle_state_from_lists(vec![attacker.clone()], vec![], vec![control_target], vec![])),
                    &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                    &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                    &move_dex,
                    &pokemon_dex,
                );
                assert!(control_outcomes.iter().any(|(state, _)| {
                    matches!(state, MatchState::BattleState(bs) if matches!(bs.p2_active_mons[0].status, Some(Status::Burn)))
                }));

                let mut already_statused = build_pokemon_state(
                    Species::Snorlax,
                    &pokemon_dex,
                    &move_dex,
                    Some(50),
                    Some([Some(PokemonMove::Splash), None, None, None]),
                    None,
                    Some(Ability::None),
                    None,
                    None,
                    Some(crate::dex_data::PokemonType::Normal),
                    None,
                    None,
                    false,
                );
                already_statused.status = Some(Status::Poison);

                let immune_targets = vec![
                    build_pokemon_state(
                        Species::Charizard,
                        &pokemon_dex,
                        &move_dex,
                        Some(50),
                        Some([Some(PokemonMove::Splash), None, None, None]),
                        None,
                        Some(Ability::None),
                        None,
                        None,
                        Some(crate::dex_data::PokemonType::Fire),
                        None,
                        None,
                        false,
                    ),
                    build_pokemon_state(
                        Species::Vaporeon,
                        &pokemon_dex,
                        &move_dex,
                        Some(50),
                        Some([Some(PokemonMove::Splash), None, None, None]),
                        None,
                        Some(Ability::WaterVeil),
                        None,
                        None,
                        Some(crate::dex_data::PokemonType::Water),
                        None,
                        None,
                        false,
                    ),
                    build_pokemon_state(
                        Species::Komala,
                        &pokemon_dex,
                        &move_dex,
                        Some(50),
                        Some([Some(PokemonMove::Splash), None, None, None]),
                        None,
                        Some(Ability::Comatose),
                        None,
                        None,
                        Some(crate::dex_data::PokemonType::Normal),
                        None,
                        None,
                        false,
                    ),
                    already_statused,
                ];

                for target in immune_targets {
                    let outcomes = run_single_turn(
                        &MatchState::BattleState(battle_state_from_lists(vec![attacker.clone()], vec![], vec![target], vec![])),
                        &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                        &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                        &move_dex,
                        &pokemon_dex,
                    );

                    assert!(outcomes.iter().all(|(state, _)| {
                        matches!(state, MatchState::BattleState(bs) if !matches!(bs.p2_active_mons[0].status, Some(Status::Burn)))
                    }));
                }
            }

            // Burn-immunity abilities not exercised by `burn_immunities`: Thermal Exchange
            // and Water Bubble block burn, and Purifying Salt blocks all status (here, burn).
            #[test]
            fn thermal_exchange_water_bubble_and_purifying_salt_block_burn() {
                let pokemon_dex = pokemon_dex();
                let move_dex = move_dex();

                let attacker = build_pokemon_state(
                    Species::RotomHeat,
                    &pokemon_dex,
                    &move_dex,
                    Some(50),
                    Some([Some(PokemonMove::WillOWisp), None, None, None]),
                    None,
                    Some(Ability::None),
                    None,
                    None,
                    Some(crate::dex_data::PokemonType::Fire),
                    None,
                    None,
                    false,
                );

                for ability in [Ability::ThermalExchange, Ability::WaterBubble, Ability::PurifyingSalt] {
                    let target = build_pokemon_state(
                        Species::Snorlax,
                        &pokemon_dex,
                        &move_dex,
                        Some(50),
                        Some([Some(PokemonMove::Splash), None, None, None]),
                        None,
                        Some(ability),
                        None,
                        None,
                        Some(crate::dex_data::PokemonType::Normal),
                        None,
                        None,
                        false,
                    );

                    let outcomes = run_single_turn(
                        &MatchState::BattleState(battle_state_from_lists(vec![attacker.clone()], vec![], vec![target], vec![])),
                        &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                        &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                        &move_dex,
                        &pokemon_dex,
                    );

                    assert!(outcomes.iter().all(|(state, _)| {
                        matches!(state, MatchState::BattleState(bs) if !matches!(bs.p2_active_mons[0].status, Some(Status::Burn)))
                    }));
                }
            }
        }

        mod freeze {
            use super::*;

            #[test]
            fn unfreeze_probability() {
                let pokemon_dex = pokemon_dex();
                let move_dex = move_dex();

                let mut frozen_mon = build_pokemon_state(
                    Species::Shuckle,
                    &pokemon_dex,
                    &move_dex,
                    Some(50),
                    Some([Some(PokemonMove::SwordsDance), None, None, None]),
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    false,
                );
                frozen_mon.status = Some(Status::Frozen(0));

                let p2_mon = build_pokemon_state(
                    Species::Magikarp,
                    &pokemon_dex,
                    &move_dex,
                    Some(50),
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

                let turn1 = run_single_turn(
                    &MatchState::BattleState(battle_state_from_lists(vec![frozen_mon], vec![], vec![p2_mon], vec![])),
                    &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                    &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                    &move_dex,
                    &pokemon_dex,
                );

                let total_probability_t1: f64 = turn1.iter().map(|(_, p)| *p).sum();
                assert!((total_probability_t1 - 1.0).abs() < 1e-9);

                let mut frozen_state_for_t2: Option<BattleState> = None;
                let t1_thaw_probability: f64 = turn1
                    .iter()
                    .filter_map(|(state, p)| match state {
                        MatchState::BattleState(bs)
                            if bs.p1_active_mons[0].status.is_none() && bs.p1_active_mons[0].boosts[0] == 2 =>
                        {
                            Some(*p)
                        }
                        _ => None,
                    })
                    .sum();

                let t1_frozen_probability: f64 = turn1
                    .iter()
                    .filter_map(|(state, p)| match state {
                        MatchState::BattleState(bs)
                            if matches!(bs.p1_active_mons[0].status, Some(Status::Frozen(1)))
                                && bs.p1_active_mons[0].boosts[0] == 0 =>
                        {
                            if frozen_state_for_t2.is_none() {
                                frozen_state_for_t2 = Some(bs.clone());
                            }
                            Some(*p)
                        }
                        _ => None,
                    })
                    .sum();

                assert!((t1_thaw_probability - 0.25).abs() < 1e-9);
                assert!((t1_frozen_probability - 0.75).abs() < 1e-9);

                let turn2 = run_single_turn(
                    &MatchState::BattleState(frozen_state_for_t2.expect("expected frozen branch after turn 1")),
                    &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                    &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                    &move_dex,
                    &pokemon_dex,
                );

                let total_probability: f64 = turn2.iter().map(|(_, p)| *p).sum();
                assert!((total_probability - 1.0).abs() < 1e-9);

                let thaw_probability: f64 = turn2
                    .iter()
                    .filter_map(|(state, p)| match state {
                        MatchState::BattleState(bs)
                            if bs.p1_active_mons[0].status.is_none() && bs.p1_active_mons[0].boosts[0] == 2 =>
                        {
                            Some(*p)
                        }
                        _ => None,
                    })
                    .sum();

                let mut frozen_state_for_t3: Option<BattleState> = None;
                let still_frozen_probability: f64 = turn2
                    .iter()
                    .filter_map(|(state, p)| match state {
                        MatchState::BattleState(bs)
                            if matches!(bs.p1_active_mons[0].status, Some(Status::Frozen(2)))
                                && bs.p1_active_mons[0].boosts[0] == 0 =>
                        {
                            if frozen_state_for_t3.is_none() {
                                frozen_state_for_t3 = Some(bs.clone());
                            }
                            Some(*p)
                        }
                        _ => None,
                    })
                    .sum();

                assert!((thaw_probability - 0.25).abs() < 1e-9);
                assert!((still_frozen_probability - 0.75).abs() < 1e-9);

                let turn3 = run_single_turn(
                    &MatchState::BattleState(frozen_state_for_t3.expect("expected frozen branch after turn 2")),
                    &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                    &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                    &move_dex,
                    &pokemon_dex,
                );

                let (state_after_t3, p3) = extract_battle_state(turn3);
                assert!((p3 - 1.0).abs() < 1e-9);
                assert!(state_after_t3.p1_active_mons[0].status.is_none());
                assert_eq!(state_after_t3.p1_active_mons[0].boosts[0], 2);
            }

            #[test]
            fn sunlight_thaw() {
                let pokemon_dex = pokemon_dex();
                let move_dex = move_dex();

                let mut frozen_mon = build_pokemon_state(
                    Species::Shuckle,
                    &pokemon_dex,
                    &move_dex,
                    Some(50),
                    Some([Some(PokemonMove::SwordsDance), None, None, None]),
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    false,
                );
                frozen_mon.status = Some(Status::Frozen(0));

                let p2_mon = build_pokemon_state(
                    Species::Magikarp,
                    &pokemon_dex,
                    &move_dex,
                    Some(50),
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

                let mut state = battle_state_from_lists(vec![frozen_mon], vec![], vec![p2_mon], vec![]);
                state.weather = Some(Weather::Sun);
                state.weather_turns = Some(5);

                let outcomes = run_single_turn(
                    &MatchState::BattleState(state),
                    &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                    &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                    &move_dex,
                    &pokemon_dex,
                );

                let (end_state, probability) = extract_battle_state(outcomes);
                assert!((probability - 1.0).abs() < 1e-9);
                assert!(end_state.p1_active_mons[0].status.is_none());
                assert_eq!(end_state.p1_active_mons[0].boosts[0], 2);
            }

            #[test]
            fn fire_move_unfreeze() {
                let pokemon_dex = pokemon_dex();
                let move_dex = move_dex();

                let attacker = build_pokemon_state(
                    Species::Charizard,
                    &pokemon_dex,
                    &move_dex,
                    Some(50),
                    Some([Some(PokemonMove::Flamethrower), None, None, None]),
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    false,
                );

                let mut defender = build_pokemon_state(
                    Species::Shuckle,
                    &pokemon_dex,
                    &move_dex,
                    Some(50),
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
                defender.status = Some(Status::Frozen(0));
                let defender_initial_hp = defender.hp;

                let outcomes = run_single_turn(
                    &MatchState::BattleState(battle_state_from_lists(vec![attacker], vec![], vec![defender], vec![])),
                    &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                    &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                    &move_dex,
                    &pokemon_dex,
                );

                assert!(outcomes.iter().all(|(state, _)| {
                    matches!(state, MatchState::BattleState(bs) if !matches!(bs.p2_active_mons[0].status, Some(Status::Frozen(_))))
                }));
                assert!(outcomes.iter().any(|(state, _)| {
                    matches!(state, MatchState::BattleState(bs) if bs.p2_active_mons[0].hp < defender_initial_hp)
                }));
            }

            #[test]
            fn freeze_immunities() {
                let pokemon_dex = pokemon_dex();
                let move_dex = move_dex();

                let attacker = build_pokemon_state(
                    Species::Lapras,
                    &pokemon_dex,
                    &move_dex,
                    Some(50),
                    Some([Some(PokemonMove::IceBeam), None, None, None]),
                    None,
                    Some(Ability::None),
                    None,
                    None,
                    Some(crate::dex_data::PokemonType::Water),
                    None,
                    None,
                    false,
                );

                let control_target = build_pokemon_state(
                    Species::Snorlax,
                    &pokemon_dex,
                    &move_dex,
                    Some(50),
                    Some([Some(PokemonMove::Splash), None, None, None]),
                    None,
                    Some(Ability::None),
                    None,
                    None,
                    Some(crate::dex_data::PokemonType::Normal),
                    None,
                    None,
                    false,
                );

                let control_outcomes = run_single_turn(
                    &MatchState::BattleState(battle_state_from_lists(vec![attacker.clone()], vec![], vec![control_target], vec![])),
                    &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                    &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                    &move_dex,
                    &pokemon_dex,
                );
                assert!(control_outcomes.iter().any(|(state, _)| {
                    matches!(state, MatchState::BattleState(bs) if matches!(bs.p2_active_mons[0].status, Some(Status::Frozen(_))))
                }));

                let immune_targets = vec![
                    build_pokemon_state(
                        Species::Snorlax,
                        &pokemon_dex,
                        &move_dex,
                        Some(50),
                        Some([Some(PokemonMove::Splash), None, None, None]),
                        None,
                        Some(Ability::MagmaArmor),
                        None,
                        None,
                        Some(crate::dex_data::PokemonType::Normal),
                        None,
                        None,
                        false,
                    ),
                    build_pokemon_state(
                        Species::Komala,
                        &pokemon_dex,
                        &move_dex,
                        Some(50),
                        Some([Some(PokemonMove::Splash), None, None, None]),
                        None,
                        Some(Ability::Comatose),
                        None,
                        None,
                        Some(crate::dex_data::PokemonType::Normal),
                        None,
                        None,
                        false,
                    ),
                    build_pokemon_state(
                        Species::Eiscue,
                        &pokemon_dex,
                        &move_dex,
                        Some(50),
                        Some([Some(PokemonMove::Splash), None, None, None]),
                        None,
                        Some(Ability::IceFace),
                        None,
                        None,
                        Some(crate::dex_data::PokemonType::Ice),
                        None,
                        None,
                        false,
                    ),
                ];

                for target in immune_targets {
                    let outcomes = run_single_turn(
                        &MatchState::BattleState(battle_state_from_lists(vec![attacker.clone()], vec![], vec![target], vec![])),
                        &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                        &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                        &move_dex,
                        &pokemon_dex,
                    );

                    assert!(outcomes.iter().all(|(state, _)| {
                        matches!(state, MatchState::BattleState(bs) if !matches!(bs.p2_active_mons[0].status, Some(Status::Frozen(_))))
                    }));
                }
            }
        }

        mod paralysis {
            use super::*;

            #[test]
            fn full_paralysis_probability() {
                let pokemon_dex = pokemon_dex();
                let move_dex = move_dex();

                let mut p1_mon = build_pokemon_state(
                    Species::Shuckle,
                    &pokemon_dex,
                    &move_dex,
                    Some(50),
                    Some([Some(PokemonMove::SwordsDance), None, None, None]),
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    false,
                );
                p1_mon.status = Some(Status::Paralysis);

                let p2_mon = build_pokemon_state(
                    Species::Magikarp,
                    &pokemon_dex,
                    &move_dex,
                    Some(50),
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

                let outcomes = run_single_turn(
                    &MatchState::BattleState(battle_state_from_lists(vec![p1_mon], vec![], vec![p2_mon], vec![])),
                    &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                    &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                    &move_dex,
                    &pokemon_dex,
                );

                let total_probability: f64 = outcomes.iter().map(|(_, p)| *p).sum();
                assert!((total_probability - 1.0).abs() < 1e-9);

                let success_probability: f64 = outcomes
                    .iter()
                    .filter_map(|(state, p)| match state {
                        MatchState::BattleState(bs) if bs.p1_active_mons[0].boosts[0] == 2 => Some(*p),
                        _ => None,
                    })
                    .sum();

                let fail_probability: f64 = outcomes
                    .iter()
                    .filter_map(|(state, p)| match state {
                        MatchState::BattleState(bs) if bs.p1_active_mons[0].boosts[0] == 0 => Some(*p),
                        _ => None,
                    })
                    .sum();

                assert!((success_probability - 0.875).abs() < 1e-9);
                assert!((fail_probability - 0.125).abs() < 1e-9);
            }

            #[test]
            fn paralysis_speed() {
                let pokemon_dex = pokemon_dex();
                let move_dex = move_dex();

                let p1_mon = build_pokemon_state(
                    Species::Magikarp,
                    &pokemon_dex,
                    &move_dex,
                    Some(50),
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
                    Species::Magikarp,
                    &pokemon_dex,
                    &move_dex,
                    Some(50),
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

                let state = battle_state_from_lists(vec![p1_mon.clone()], vec![], vec![p2_mon.clone()], vec![]);
                let mut par_state = battle_state_from_lists(vec![p1_mon], vec![], vec![p2_mon], vec![]);
                par_state.p1_active_mons[0].status = Some(Status::Paralysis);

                let action_p1 = Action::MoveAction(MoveAction {
                    move_name: PokemonMove::Splash,
                    priority: 0,
                    user_slot: FieldSlot { player: Player::P1, slot_index: 0 },
                    target_slot: None,
                });
                let action_p2 = Action::MoveAction(MoveAction {
                    move_name: PokemonMove::Splash,
                    priority: 0,
                    user_slot: FieldSlot { player: Player::P2, slot_index: 0 },
                    target_slot: None,
                });

                let base_order = simulator_helpers::compare_action_order(&action_p1, &action_p2, &state, &move_dex);
                let par_order = simulator_helpers::compare_action_order(&action_p1, &action_p2, &par_state, &move_dex);

                assert_eq!(base_order, std::cmp::Ordering::Equal);
                assert_eq!(par_order, std::cmp::Ordering::Greater);
            }

            #[test]
            fn paralysis_immunity() {
                let pokemon_dex = pokemon_dex();
                let move_dex = move_dex();

                let attacker = build_pokemon_state(
                    Species::Pikachu,
                    &pokemon_dex,
                    &move_dex,
                    Some(50),
                    Some([Some(PokemonMove::ThunderWave), None, None, None]),
                    None,
                    Some(Ability::None),
                    None,
                    None,
                    Some(crate::dex_data::PokemonType::Electric),
                    None,
                    None,
                    false,
                );

                let control_target = build_pokemon_state(
                    Species::Snorlax,
                    &pokemon_dex,
                    &move_dex,
                    Some(50),
                    Some([Some(PokemonMove::Splash), None, None, None]),
                    None,
                    Some(Ability::None),
                    None,
                    None,
                    Some(crate::dex_data::PokemonType::Normal),
                    None,
                    None,
                    false,
                );

                let control_outcomes = run_single_turn(
                    &MatchState::BattleState(battle_state_from_lists(vec![attacker.clone()], vec![], vec![control_target], vec![])),
                    &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                    &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                    &move_dex,
                    &pokemon_dex,
                );

                assert!(control_outcomes.iter().any(|(state, _)| {
                    matches!(state, MatchState::BattleState(bs) if matches!(bs.p2_active_mons[0].status, Some(Status::Paralysis)))
                }));

                let immune_cases = vec![
                    (Species::Jolteon, Ability::None),
                    (Species::Lopunny, Ability::Limber),
                ];

                for (species, ability) in immune_cases {
                    let target = build_pokemon_state(
                        species,
                        &pokemon_dex,
                        &move_dex,
                        Some(50),
                        Some([Some(PokemonMove::Splash), None, None, None]),
                        None,
                        Some(ability),
                        None,
                        None,
                        None,
                        None,
                        None,
                        false,
                    );

                    let outcomes = run_single_turn(
                        &MatchState::BattleState(battle_state_from_lists(vec![attacker.clone()], vec![], vec![target], vec![])),
                        &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                        &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                        &move_dex,
                        &pokemon_dex,
                    );

                    assert!(outcomes.iter().all(|(state, _)| {
                        matches!(state, MatchState::BattleState(bs) if !matches!(bs.p2_active_mons[0].status, Some(Status::Paralysis)))
                    }));
                }
            }
        }

        mod poison {
            use super::*;

            #[test]
            fn poison_damage() {
                let pokemon_dex = pokemon_dex();
                let move_dex = move_dex();

                let mut poison_mon = build_pokemon_state(
                    Species::Shuckle,
                    &pokemon_dex,
                    &move_dex,
                    Some(50),
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
                poison_mon.status = Some(Status::Poison);

                let mut toxic_mon = build_pokemon_state(
                    Species::Snorlax,
                    &pokemon_dex,
                    &move_dex,
                    Some(50),
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
                toxic_mon.status = Some(Status::ToxicPoison(0));

                let poison_initial_hp = poison_mon.hp;
                let toxic_initial_hp = toxic_mon.hp;

                let outcomes = run_single_turn(
                    &MatchState::BattleState(battle_state_from_lists(vec![poison_mon], vec![], vec![toxic_mon], vec![])),
                    &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                    &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                    &move_dex,
                    &pokemon_dex,
                );

                let (end_state, probability) = extract_battle_state(outcomes);
                assert!((probability - 1.0).abs() < 1e-9);
                assert_eq!(poison_initial_hp - end_state.p1_active_mons[0].hp, poison_initial_hp / 8);
                assert_eq!(toxic_initial_hp - end_state.p2_active_mons[0].hp, toxic_initial_hp / 16);
                assert!(matches!(end_state.p2_active_mons[0].status, Some(Status::ToxicPoison(1))));
            }

            #[test]
            fn toxic_switch() {
                let pokemon_dex = pokemon_dex();
                let move_dex = move_dex();

                let mut toxic_active = build_pokemon_state(
                    Species::Shuckle,
                    &pokemon_dex,
                    &move_dex,
                    Some(50),
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
                toxic_active.status = Some(Status::ToxicPoison(4));

                let healthy_bench = build_pokemon_state(
                    Species::Magikarp,
                    &pokemon_dex,
                    &move_dex,
                    Some(50),
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
                    Species::Snorlax,
                    &pokemon_dex,
                    &move_dex,
                    Some(50),
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

                let initial_state = battle_state_from_lists(vec![toxic_active], vec![healthy_bench], vec![p2_mon], vec![]);

                let outcomes = run_single_turn(
                    &MatchState::BattleState(initial_state),
                    &PlayerCommand::Battle(vec![BattleCommand::Switch(SwitchCommand { party_index: 0 })]),
                    &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                    &move_dex,
                    &pokemon_dex,
                );

                let (end_state, probability) = extract_battle_state(outcomes);
                assert!((probability - 1.0).abs() < 1e-9);
                assert!(matches!(end_state.p1_back_mons[0].status, Some(Status::ToxicPoison(0))));
            }

            #[test]
            fn corrosion() {
                let pokemon_dex = pokemon_dex();
                let move_dex = move_dex();

                let control_attacker = build_pokemon_state(
                    Species::Snorlax,
                    &pokemon_dex,
                    &move_dex,
                    Some(50),
                    Some([Some(PokemonMove::Toxic), None, None, None]),
                    None,
                    Some(Ability::None),
                    None,
                    None,
                    Some(crate::dex_data::PokemonType::Normal),
                    None,
                    None,
                    false,
                );

                let control_target = build_pokemon_state(
                    Species::Snorlax,
                    &pokemon_dex,
                    &move_dex,
                    Some(50),
                    Some([Some(PokemonMove::Splash), None, None, None]),
                    None,
                    Some(Ability::None),
                    None,
                    None,
                    Some(crate::dex_data::PokemonType::Normal),
                    None,
                    None,
                    false,
                );

                let control_outcomes = run_single_turn(
                    &MatchState::BattleState(battle_state_from_lists(vec![control_attacker.clone()], vec![], vec![control_target], vec![])),
                    &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                    &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                    &move_dex,
                    &pokemon_dex,
                );
                assert!(control_outcomes.iter().any(|(state, _)| {
                    matches!(state, MatchState::BattleState(bs) if matches!(bs.p2_active_mons[0].status, Some(Status::ToxicPoison(_))))
                }));

                let immune_targets = vec![
                    build_pokemon_state(
                        Species::Gengar,
                        &pokemon_dex,
                        &move_dex,
                        Some(50),
                        Some([Some(PokemonMove::Splash), None, None, None]),
                        None,
                        Some(Ability::None),
                        None,
                        None,
                        Some(crate::dex_data::PokemonType::Poison),
                        None,
                        None,
                        false,
                    ),
                    build_pokemon_state(
                        Species::Corviknight,
                        &pokemon_dex,
                        &move_dex,
                        Some(50),
                        Some([Some(PokemonMove::Splash), None, None, None]),
                        None,
                        Some(Ability::None),
                        None,
                        None,
                        Some(crate::dex_data::PokemonType::Steel),
                        None,
                        None,
                        false,
                    ),
                    build_pokemon_state(
                        Species::Snorlax,
                        &pokemon_dex,
                        &move_dex,
                        Some(50),
                        Some([Some(PokemonMove::Splash), None, None, None]),
                        None,
                        Some(Ability::Immunity),
                        None,
                        None,
                        Some(crate::dex_data::PokemonType::Normal),
                        None,
                        None,
                        false,
                    ),
                    build_pokemon_state(
                        Species::Komala,
                        &pokemon_dex,
                        &move_dex,
                        Some(50),
                        Some([Some(PokemonMove::Splash), None, None, None]),
                        None,
                        Some(Ability::Comatose),
                        None,
                        None,
                        Some(crate::dex_data::PokemonType::Normal),
                        None,
                        None,
                        false,
                    ),
                ];

                for target in immune_targets {
                    let outcomes = run_single_turn(
                        &MatchState::BattleState(battle_state_from_lists(vec![control_attacker.clone()], vec![], vec![target], vec![])),
                        &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                        &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                        &move_dex,
                        &pokemon_dex,
                    );

                    assert!(outcomes.iter().all(|(state, _)| {
                        matches!(state, MatchState::BattleState(bs) if !matches!(bs.p2_active_mons[0].status, Some(Status::Poison) | Some(Status::ToxicPoison(_))))
                    }));
                }

                let corrosion_attacker = build_pokemon_state(
                    Species::Salazzle,
                    &pokemon_dex,
                    &move_dex,
                    Some(50),
                    Some([Some(PokemonMove::Toxic), None, None, None]),
                    None,
                    Some(Ability::Corrosion),
                    None,
                    None,
                    Some(crate::dex_data::PokemonType::Fire),
                    None,
                    None,
                    false,
                );

                for target in [
                    build_pokemon_state(
                        Species::Gengar,
                        &pokemon_dex,
                        &move_dex,
                        Some(50),
                        Some([Some(PokemonMove::Splash), None, None, None]),
                        None,
                        Some(Ability::None),
                        None,
                        None,
                        Some(crate::dex_data::PokemonType::Poison),
                        None,
                        None,
                        false,
                    ),
                    build_pokemon_state(
                        Species::Corviknight,
                        &pokemon_dex,
                        &move_dex,
                        Some(50),
                        Some([Some(PokemonMove::Splash), None, None, None]),
                        None,
                        Some(Ability::None),
                        None,
                        None,
                        Some(crate::dex_data::PokemonType::Steel),
                        None,
                        None,
                        false,
                    ),
                ] {
                    let outcomes = run_single_turn(
                        &MatchState::BattleState(battle_state_from_lists(vec![corrosion_attacker.clone()], vec![], vec![target], vec![])),
                        &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                        &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                        &move_dex,
                        &pokemon_dex,
                    );

                    assert!(outcomes.iter().any(|(state, _)| {
                        matches!(state, MatchState::BattleState(bs) if matches!(bs.p2_active_mons[0].status, Some(Status::ToxicPoison(_))))
                    }));
                }
            }
        }

        mod sleep {
            use super::*;

            #[test]
            fn sleep_talk_fails() {
                let pokemon_dex = pokemon_dex();
                let move_dex = move_dex();

                let p1_mon = build_pokemon_state(
                    Species::Shuckle,
                    &pokemon_dex,
                    &move_dex,
                    Some(50),
                    Some([
                        Some(PokemonMove::SleepTalk),
                        Some(PokemonMove::SunnyDay),
                        None,
                        None,
                    ]),
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
                    Species::Magikarp,
                    &pokemon_dex,
                    &move_dex,
                    Some(50),
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

                let state = battle_state_from_lists(vec![p1_mon], vec![], vec![p2_mon], vec![]);

                let outcomes = simulate_turn(
                    &MatchState::BattleState(state),
                    &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                    &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                    &move_dex,
                    &pokemon_dex,
                    false,
                    1,
                );

                let total_probability: f64 = outcomes.iter().map(|(_, probability)| *probability).sum();
                assert!((total_probability - 1.0).abs() < 1e-9);
                assert!(outcomes.iter().all(|(state, _)| matches!(state, MatchState::BattleState(bs) if bs.weather.is_none())));
            }

            #[test]
            fn sleep_talk_moves() {
                let pokemon_dex = pokemon_dex();
                let move_dex = move_dex();

                let cases: Vec<([Option<PokemonMove>; 4], Vec<(Weather, f64)>)> = vec![
                    (
                        [Some(PokemonMove::SleepTalk), Some(PokemonMove::RainDance), None, None],
                        vec![(Weather::Rain, 1.0)],
                    ),
                    (
                        [
                            Some(PokemonMove::SleepTalk),
                            Some(PokemonMove::RainDance),
                            Some(PokemonMove::SunnyDay),
                            None,
                        ],
                        vec![(Weather::Rain, 0.5), (Weather::Sun, 0.5)],
                    ),
                    (
                        [
                            Some(PokemonMove::SleepTalk),
                            Some(PokemonMove::RainDance),
                            Some(PokemonMove::SunnyDay),
                            Some(PokemonMove::Sandstorm),
                        ],
                        vec![(Weather::Rain, 1.0 / 3.0), (Weather::Sun, 1.0 / 3.0), (Weather::Sandstorm, 1.0 / 3.0)],
                    ),
                ];

                for (moves, expected_weather_probabilities) in cases {
                    let mut p1_mon = build_pokemon_state(
                        Species::Shuckle,
                        &pokemon_dex,
                        &move_dex,
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
                    );
                    p1_mon.status = Some(Status::Sleep(0));

                    let p2_mon = build_pokemon_state(
                        Species::Magikarp,
                        &pokemon_dex,
                        &move_dex,
                        Some(50),
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

                    let state = battle_state_from_lists(vec![p1_mon], vec![], vec![p2_mon], vec![]);

                    let outcomes = simulate_turn(
                        &MatchState::BattleState(state),
                        &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                        &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                        &move_dex,
                        &pokemon_dex,
                        false,
                        1,
                    );

                    let total_probability: f64 = outcomes.iter().map(|(_, probability)| *probability).sum();
                    assert!((total_probability - 1.0).abs() < 1e-9);

                    let mut actual_weather_probabilities: HashMap<Weather, f64> = HashMap::new();
                    for (outcome_state, probability) in outcomes {
                        match outcome_state {
                            MatchState::BattleState(bs) => {
                                let weather = bs.weather.expect("sleep talk should set weather when asleep");
                                *actual_weather_probabilities.entry(weather).or_insert(0.0) += probability;
                            }
                            _ => panic!("expected a battle state outcome"),
                        }
                    }

                    assert_eq!(actual_weather_probabilities.len(), expected_weather_probabilities.len());
                    for (expected_weather, expected_probability) in expected_weather_probabilities {
                        let actual_probability = actual_weather_probabilities
                            .get(&expected_weather)
                            .copied()
                            .unwrap_or_else(|| panic!("missing expected weather branch: {:?}", expected_weather));
                        assert!((actual_probability - expected_probability).abs() < 1e-9);
                    }
                }
            }

            #[test]
            fn sleep_probablities() {
                let pokemon_dex = pokemon_dex();
                let move_dex = move_dex();

                let mut sleeping_mon = build_pokemon_state(
                    Species::Shuckle,
                    &pokemon_dex,
                    &move_dex,
                    Some(50),
                    Some([Some(PokemonMove::SwordsDance), None, None, None]),
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    false,
                );
                sleeping_mon.status = Some(Status::Sleep(0));

                let p2_mon = build_pokemon_state(
                    Species::Magikarp,
                    &pokemon_dex,
                    &move_dex,
                    Some(50),
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

                let turn1 = run_single_turn(
                    &MatchState::BattleState(battle_state_from_lists(vec![sleeping_mon], vec![], vec![p2_mon], vec![])),
                    &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                    &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                    &move_dex,
                    &pokemon_dex,
                );

                let (state_after_t1, p1) = extract_battle_state(turn1);
                assert!((p1 - 1.0).abs() < 1e-9);
                assert!(matches!(state_after_t1.p1_active_mons[0].status, Some(Status::Sleep(1))));
                assert_eq!(state_after_t1.p1_active_mons[0].boosts[0], 0);

                let turn2 = run_single_turn(
                    &MatchState::BattleState(state_after_t1.clone()),
                    &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                    &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                    &move_dex,
                    &pokemon_dex,
                );

                let total_probability: f64 = turn2.iter().map(|(_, p)| *p).sum();
                assert!((total_probability - 1.0).abs() < 1e-9);

                let wake_probability: f64 = turn2
                    .iter()
                    .filter_map(|(state, p)| match state {
                        MatchState::BattleState(bs)
                            if bs.p1_active_mons[0].status.is_none() && bs.p1_active_mons[0].boosts[0] == 2 =>
                        {
                            Some(*p)
                        }
                        _ => None,
                    })
                    .sum();

                let mut sleeping_state_for_t3: Option<BattleState> = None;
                let asleep_probability: f64 = turn2
                    .iter()
                    .filter_map(|(state, p)| match state {
                        MatchState::BattleState(bs)
                            if matches!(bs.p1_active_mons[0].status, Some(Status::Sleep(2)))
                                && bs.p1_active_mons[0].boosts[0] == 0 =>
                        {
                            if sleeping_state_for_t3.is_none() {
                                sleeping_state_for_t3 = Some(bs.clone());
                            }
                            Some(*p)
                        }
                        _ => None,
                    })
                    .sum();

                assert!((wake_probability - (1.0 / 3.0)).abs() < 1e-9);
                assert!((asleep_probability - (2.0 / 3.0)).abs() < 1e-9);

                let turn3 = run_single_turn(
                    &MatchState::BattleState(sleeping_state_for_t3.expect("expected sleeping branch after turn 2")),
                    &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                    &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                    &move_dex,
                    &pokemon_dex,
                );

                let (state_after_t3, p3) = extract_battle_state(turn3);
                assert!((p3 - 1.0).abs() < 1e-9);
                assert!(state_after_t3.p1_active_mons[0].status.is_none());
                assert_eq!(state_after_t3.p1_active_mons[0].boosts[0], 2);
            }

            #[test]
            fn sleep_immunities() {
                let pokemon_dex = pokemon_dex();
                let move_dex = move_dex();

                let attacker = build_pokemon_state(
                    Species::Amoonguss,
                    &pokemon_dex,
                    &move_dex,
                    Some(50),
                    Some([Some(PokemonMove::Spore), None, None, None]),
                    None,
                    Some(Ability::None),
                    None,
                    None,
                    Some(crate::dex_data::PokemonType::Grass),
                    None,
                    None,
                    false,
                );

                let control_target = build_pokemon_state(
                    Species::Snorlax,
                    &pokemon_dex,
                    &move_dex,
                    Some(50),
                    Some([Some(PokemonMove::Splash), None, None, None]),
                    None,
                    Some(Ability::None),
                    None,
                    None,
                    Some(crate::dex_data::PokemonType::Normal),
                    None,
                    None,
                    false,
                );

                let control_outcomes = run_single_turn(
                    &MatchState::BattleState(battle_state_from_lists(vec![attacker.clone()], vec![], vec![control_target], vec![])),
                    &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                    &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                    &move_dex,
                    &pokemon_dex,
                );
                assert!(control_outcomes.iter().any(|(state, _)| {
                    matches!(state, MatchState::BattleState(bs) if matches!(bs.p2_active_mons[0].status, Some(Status::Sleep(_))))
                }));

                for ability in [Ability::Insomnia, Ability::VitalSpirit] {
                    let target = build_pokemon_state(
                        Species::Snorlax,
                        &pokemon_dex,
                        &move_dex,
                        Some(50),
                        Some([Some(PokemonMove::Splash), None, None, None]),
                        None,
                        Some(ability),
                        None,
                        None,
                        Some(crate::dex_data::PokemonType::Normal),
                        None,
                        None,
                        false,
                    );

                    let outcomes = run_single_turn(
                        &MatchState::BattleState(battle_state_from_lists(vec![attacker.clone()], vec![], vec![target], vec![])),
                        &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                        &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                        &move_dex,
                        &pokemon_dex,
                    );

                    assert!(outcomes.iter().all(|(state, _)| {
                        matches!(state, MatchState::BattleState(bs) if !matches!(bs.p2_active_mons[0].status, Some(Status::Sleep(_))))
                    }));
                }
            }
        }

        mod confusion {
            use super::*;

            #[test]
            fn own_tempo() {
                let pokemon_dex = pokemon_dex();
                let move_dex = move_dex();

                let duration_state = MatchState::BattleState(battle_state_from_lists(
                    vec![build_pokemon_state(
                        Species::Amoonguss,
                        &pokemon_dex,
                        &move_dex,
                        Some(50),
                        Some([Some(PokemonMove::ConfuseRay), None, None, None]),
                        None,
                        Some(Ability::None),
                        None,
                        None,
                        Some(crate::dex_data::PokemonType::Grass),
                        None,
                        None,
                        false,
                    )],
                    vec![],
                    vec![build_pokemon_state(
                        Species::Snorlax,
                        &pokemon_dex,
                        &move_dex,
                        Some(50),
                        Some([Some(PokemonMove::Splash), None, None, None]),
                        None,
                        Some(Ability::None),
                        None,
                        None,
                        Some(crate::dex_data::PokemonType::Normal),
                        None,
                        None,
                        false,
                    )],
                    vec![],
                ));

                let duration_outcomes = run_single_turn(
                    &duration_state,
                    &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                    &PlayerCommand::Battle(simple_attack(Player::P2, vec![])),
                    &move_dex,
                    &pokemon_dex,
                );

                assert!(duration_outcomes.iter().all(|(state, _)| {
                    matches!(state, MatchState::BattleState(bs) if confusion_turns(&bs.p2_active_mons[0]).map(|turns| (2..=5).contains(&turns)).unwrap_or(false))
                }));

                let own_tempo_state = MatchState::BattleState(battle_state_from_lists(
                    vec![build_pokemon_state(
                        Species::Amoonguss,
                        &pokemon_dex,
                        &move_dex,
                        Some(50),
                        Some([Some(PokemonMove::ConfuseRay), None, None, None]),
                        None,
                        Some(Ability::None),
                        None,
                        None,
                        Some(crate::dex_data::PokemonType::Grass),
                        None,
                        None,
                        false,
                    )],
                    vec![],
                    vec![build_pokemon_state(
                        Species::Snorlax,
                        &pokemon_dex,
                        &move_dex,
                        Some(50),
                        Some([Some(PokemonMove::Splash), None, None, None]),
                        None,
                        Some(Ability::OwnTempo),
                        None,
                        None,
                        Some(crate::dex_data::PokemonType::Normal),
                        None,
                        None,
                        false,
                    )],
                    vec![],
                ));

                let own_tempo_outcomes = run_single_turn(
                    &own_tempo_state,
                    &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                    &PlayerCommand::Battle(simple_attack(Player::P2, vec![])),
                    &move_dex,
                    &pokemon_dex,
                );

                assert!(own_tempo_outcomes.iter().all(|(state, _)| {
                    matches!(state, MatchState::BattleState(bs) if confusion_turns(&bs.p2_active_mons[0]).is_none())
                }));
            }

            #[test]
            fn tangled_feet() {
                let pokemon_dex = pokemon_dex();
                let move_dex = move_dex();

                let initial_state = MatchState::BattleState({
                    let mut state = battle_state_from_lists(
                        vec![build_pokemon_state(
                            Species::Snorlax,
                            &pokemon_dex,
                            &move_dex,
                            Some(50),
                            Some([Some(PokemonMove::Earthquake), None, None, None]),
                            None,
                            Some(Ability::None),
                            None,
                            None,
                            Some(crate::dex_data::PokemonType::Normal),
                            None,
                            None,
                            false,
                        )],
                        vec![],
                        vec![build_pokemon_state(
                            Species::Garchomp,
                            &pokemon_dex,
                            &move_dex,
                            Some(50),
                            Some([Some(PokemonMove::Splash), None, None, None]),
                            None,
                            Some(Ability::TangledFeet),
                            None,
                            None,
                            Some(crate::dex_data::PokemonType::Dragon),
                            None,
                            None,
                            false,
                        )],
                        vec![],
                    );
                    state.p2_active_mons[0].volatiles.push(VolatileStatusState::MoveStatus(VolatileStatus::Confusion, 2));
                    state
                });

                let initial_hp = if let MatchState::BattleState(bs) = &initial_state {
                    bs.p2_active_mons[0].hp
                } else {
                    0
                };

                let outcomes = simulate_turn(
                    &initial_state,
                    &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                    &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                    &move_dex,
                    &pokemon_dex,
                    false,
                    1,
                );

                let hit_prob = hit_probability(&outcomes, initial_hp);
                assert!((hit_prob - 0.5).abs() < 1e-9);
            }

            #[test]
            fn confusion_damage() {
                let pokemon_dex = pokemon_dex();
                let move_dex = move_dex();

                let initial_state = MatchState::BattleState({
                    let mut state = battle_state_from_lists(
                        vec![build_pokemon_state(
                            Species::Snorlax,
                            &pokemon_dex,
                            &move_dex,
                            Some(50),
                            Some([Some(PokemonMove::DoubleEdge), Some(PokemonMove::Splash), None, None]),
                            None,
                            Some(Ability::None),
                            Some(Nature::Adamant),
                            None,
                            None,
                            Some([0, 0, 0, 0, 0, 0]),
                            None,
                            false,
                        )],
                        vec![],
                        vec![build_pokemon_state(
                            Species::Garchomp,
                            &pokemon_dex,
                            &move_dex,
                            Some(50),
                            Some([Some(PokemonMove::Splash), None, None, None]),
                            None,
                            Some(Ability::None),
                            Some(Nature::Hardy),
                            None,
                            None,
                            Some([0, 0, 0, 0, 0, 0]),
                            None,
                            false,
                        )],
                        vec![],
                    );
                    state.p1_active_mons[0].volatiles.push(VolatileStatusState::MoveStatus(VolatileStatus::Confusion, 2));
                    state
                });

                let initial_p1_hp = if let MatchState::BattleState(bs) = &initial_state {
                    bs.p1_active_mons[0].hp
                } else {
                    0
                };
                let initial_p2_hp = if let MatchState::BattleState(bs) = &initial_state {
                    bs.p2_active_mons[0].hp
                } else {
                    0
                };

                let outcomes = simulate_turn(
                    &initial_state,
                    &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                    &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                    &move_dex,
                    &pokemon_dex,
                    false,
                    16,
                );

                let total_probability: f64 = outcomes.iter().map(|(_, probability)| *probability).sum();
                assert!((total_probability - 1.0).abs() < 1e-9);

                let mut target_damages = Vec::new();
                let mut self_damages = Vec::new();

                for (state, _) in &outcomes {
                    let MatchState::BattleState(bs) = state else {
                        panic!("expected battle state outcome");
                    };

                    let p1_loss = initial_p1_hp.saturating_sub(bs.p1_active_mons[0].hp);
                    let p2_loss = initial_p2_hp.saturating_sub(bs.p2_active_mons[0].hp);

                    if p2_loss > 0 {
                        target_damages.push(p2_loss);
                    } else if p1_loss > 0 {
                        self_damages.push(p1_loss);
                    } else {
                        panic!("expected confusion to produce either self-damage or target damage");
                    }

                    assert_eq!(confusion_turns(&bs.p1_active_mons[0]), Some(1));
                }

                target_damages.sort_unstable();
                self_damages.sort_unstable();

                assert_eq!(target_damages, vec![84, 85, 87, 88, 90, 91, 93, 94, 96, 97, 99, 100]);
                assert_eq!(self_damages, vec![26, 27, 28, 29, 30, 31]);
            }

            #[test]
            fn confusion_ends() {
                let pokemon_dex = pokemon_dex();
                let move_dex = move_dex();

                let initial_state = MatchState::BattleState({
                    let mut state = battle_state_from_lists(
                        vec![build_pokemon_state(
                            Species::Snorlax,
                            &pokemon_dex,
                            &move_dex,
                            Some(50),
                            Some([Some(PokemonMove::DoubleEdge), Some(PokemonMove::Splash), None, None]),
                            None,
                            Some(Ability::None),
                            Some(Nature::Adamant),
                            None,
                            None,
                            Some([0, 0, 0, 0, 0, 0]),
                            None,
                            false,
                        )],
                        vec![],
                        vec![build_pokemon_state(
                            Species::Garchomp,
                            &pokemon_dex,
                            &move_dex,
                            Some(50),
                            Some([Some(PokemonMove::Splash), None, None, None]),
                            None,
                            Some(Ability::None),
                            Some(Nature::Hardy),
                            None,
                            None,
                            Some([0, 0, 0, 0, 0, 0]),
                            None,
                            false,
                        )],
                        vec![],
                    );
                    state.p1_active_mons[0].volatiles.push(VolatileStatusState::MoveStatus(VolatileStatus::Confusion, 1));
                    state
                });

                let initial_p1_hp = if let MatchState::BattleState(bs) = &initial_state {
                    bs.p1_active_mons[0].hp
                } else {
                    0
                };
                let initial_p2_hp = if let MatchState::BattleState(bs) = &initial_state {
                    bs.p2_active_mons[0].hp
                } else {
                    0
                };

                let outcomes = simulate_turn(
                    &initial_state,
                    &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                    &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                    &move_dex,
                    &pokemon_dex,
                    false,
                    16,
                );

                let mut target_damages = Vec::new();
                let mut self_damages = Vec::new();

                for (state, _) in &outcomes {
                    let MatchState::BattleState(bs) = state else {
                        panic!("expected battle state outcome");
                    };

                    let p1_loss = initial_p1_hp.saturating_sub(bs.p1_active_mons[0].hp);
                    let p2_loss = initial_p2_hp.saturating_sub(bs.p2_active_mons[0].hp);

                    if p2_loss > 0 {
                        target_damages.push(p2_loss);
                    } else if p1_loss > 0 {
                        self_damages.push(p1_loss);
                    } else {
                        panic!("expected confusion to produce either self-damage or target damage");
                    }

                    assert!(confusion_turns(&bs.p1_active_mons[0]).is_none());
                }

                target_damages.sort_unstable();
                self_damages.sort_unstable();

                assert_eq!(target_damages, vec![84, 85, 87, 88, 90, 91, 93, 94, 96, 97, 99, 100]);
                assert!(self_damages.is_empty());
            }
        }

        mod random {
            use super::*;

            #[test]
            fn tri_attack_random_status() {
                let pokemon_dex = pokemon_dex();
                let move_dex = move_dex();

                // Weak attacker vs a bulky Normal-type target so the target always survives
                // (and can legally be burned/frozen/paralyzed), leaving only the status rolls.
                let attacker = build_pokemon_state(
                    Species::Magikarp, &pokemon_dex, &move_dex, Some(1),
                    Some([Some(PokemonMove::TriAttack), None, None, None]),
                    None, Some(Ability::None), None, None,
                    Some(crate::dex_data::PokemonType::Normal), Some([0, 0, 0, 0, 0, 0]), None, false,
                );
                let target = build_pokemon_state(
                    Species::Snorlax, &pokemon_dex, &move_dex, Some(100),
                    Some([Some(PokemonMove::Splash), None, None, None]),
                    None, Some(Ability::None), None, None,
                    Some(crate::dex_data::PokemonType::Normal), Some([0, 0, 0, 0, 0, 0]), None, false,
                );

                let outcomes = run_single_turn(
                    &MatchState::BattleState(battle_state_from_lists(vec![attacker], vec![], vec![target], vec![])),
                    &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                    &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                    &move_dex,
                    &pokemon_dex,
                );

                let dist = status_distribution(&outcomes);
                let each = 0.20 / 3.0;
                assert!((dist.get("none").copied().unwrap_or(0.0) - 0.80).abs() < 1e-9);
                assert!((dist.get("brn").copied().unwrap_or(0.0) - each).abs() < 1e-9);
                assert!((dist.get("frz").copied().unwrap_or(0.0) - each).abs() < 1e-9);
                assert!((dist.get("par").copied().unwrap_or(0.0) - each).abs() < 1e-9);
                // No other statuses should appear.
                assert!(dist.get("psn").is_none() && dist.get("slp").is_none());
            }

            #[test]
            fn dire_claw_random_status() {
                let pokemon_dex = pokemon_dex();
                let move_dex = move_dex();

                let attacker = build_pokemon_state(
                    Species::Magikarp, &pokemon_dex, &move_dex, Some(1),
                    Some([Some(PokemonMove::DireClaw), None, None, None]),
                    None, Some(Ability::None), None, None,
                    Some(crate::dex_data::PokemonType::Normal), Some([0, 0, 0, 0, 0, 0]), None, false,
                );
                let target = build_pokemon_state(
                    Species::Snorlax, &pokemon_dex, &move_dex, Some(100),
                    Some([Some(PokemonMove::Splash), None, None, None]),
                    None, Some(Ability::None), None, None,
                    Some(crate::dex_data::PokemonType::Normal), Some([0, 0, 0, 0, 0, 0]), None, false,
                );

                let outcomes = run_single_turn(
                    &MatchState::BattleState(battle_state_from_lists(vec![attacker], vec![], vec![target], vec![])),
                    &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                    &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                    &move_dex,
                    &pokemon_dex,
                );

                let dist = status_distribution(&outcomes);
                let each = 0.50 / 3.0;
                assert!((dist.get("none").copied().unwrap_or(0.0) - 0.50).abs() < 1e-9);
                assert!((dist.get("psn").copied().unwrap_or(0.0) - each).abs() < 1e-9);
                assert!((dist.get("par").copied().unwrap_or(0.0) - each).abs() < 1e-9);
                assert!((dist.get("slp").copied().unwrap_or(0.0) - each).abs() < 1e-9);
                assert!(dist.get("brn").is_none() && dist.get("frz").is_none());
            }
        }
    }

    mod turn_order {
        use super::*;

        // A slow P1 mon (Torkoal) and a fast P2 mon (Pelipper). We drive the public
        // action-ordering comparator directly so priority / Trick Room handling is
        // isolated from damage rolls. `Ordering::Less` means action1 is resolved first.
        fn ordering_fixture() -> BattleState {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();
            let slow = build_pokemon_state(
                Species::Torkoal,
                &pokemon_dex,
                &move_dex,
                Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None,
                Some(Ability::Pressure),
                None,
                None,
                None,
                None,
                None,
                false,
            );
            let fast = build_pokemon_state(
                Species::Pelipper,
                &pokemon_dex,
                &move_dex,
                Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None,
                Some(Ability::Pressure),
                None,
                None,
                None,
                None,
                None,
                false,
            );
            battle_state_from_lists(vec![slow], vec![], vec![fast], vec![])
        }

        fn move_action(player: Player, priority: i8) -> Action {
            Action::MoveAction(MoveAction {
                move_name: PokemonMove::Splash,
                priority,
                user_slot: FieldSlot { player, slot_index: 0 },
                target_slot: None,
            })
        }

        #[test]
        fn higher_priority_moves_first() {
            let move_dex = move_dex();
            let state = ordering_fixture();
            // P1 (slow) uses a +1 priority move; P2 (fast) uses a 0-priority move.
            let slow_priority = move_action(Player::P1, 1);
            let fast_normal = move_action(Player::P2, 0);
            assert_eq!(
                simulator_helpers::compare_action_order(&slow_priority, &fast_normal, &state, &move_dex),
                std::cmp::Ordering::Less
            );
            assert_eq!(
                simulator_helpers::compare_action_order(&fast_normal, &slow_priority, &state, &move_dex),
                std::cmp::Ordering::Greater
            );
        }

        #[test]
        fn faster_mon_moves_first_at_equal_priority() {
            let move_dex = move_dex();
            let state = ordering_fixture();
            let slow = move_action(Player::P1, 0);
            let fast = move_action(Player::P2, 0);
            // Pelipper (P2) outspeeds Torkoal (P1).
            assert_eq!(
                simulator_helpers::compare_action_order(&fast, &slow, &state, &move_dex),
                std::cmp::Ordering::Less
            );
            assert_eq!(
                simulator_helpers::compare_action_order(&slow, &fast, &state, &move_dex),
                std::cmp::Ordering::Greater
            );
        }

        #[test]
        fn trick_room_reverses_speed_but_not_priority() {
            let move_dex = move_dex();
            let mut state = ordering_fixture();
            state.pseudo_weathers.push(PseudoWeather::TrickRoom);
            state.pseudo_weather_turns.push(5);

            // Under Trick Room the slower Torkoal (P1) moves first at equal priority.
            let slow = move_action(Player::P1, 0);
            let fast = move_action(Player::P2, 0);
            assert_eq!(
                simulator_helpers::compare_action_order(&slow, &fast, &state, &move_dex),
                std::cmp::Ordering::Less
            );

            // Priority still wins regardless of Trick Room.
            let fast_priority = move_action(Player::P2, 1);
            let slow_normal = move_action(Player::P1, 0);
            assert_eq!(
                simulator_helpers::compare_action_order(&fast_priority, &slow_normal, &state, &move_dex),
                std::cmp::Ordering::Less
            );
        }
    }

    mod redirection {
        use super::*;

        fn user_slot() -> FieldSlot { FieldSlot { player: Player::P1, slot_index: 0 } }
        fn primary_slot() -> FieldSlot { FieldSlot { player: Player::P2, slot_index: 0 } }
        fn redirector_slot() -> FieldSlot { FieldSlot { player: Player::P2, slot_index: 1 } }

        // Doubles layout: P1 = attacker (slot0) + ally (slot1); P2 = primary target
        // (slot0) + a redirector (slot1). Redirection is driven directly through the
        // public `check_and_apply_redirection` helper.
        fn doubles_state(attacker: PokemonState, redirector: PokemonState) -> BattleState {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();
            let plain = |species: Species| {
                build_pokemon_state(
                    species,
                    &pokemon_dex,
                    &move_dex,
                    Some(50),
                    Some([Some(PokemonMove::Splash), None, None, None]),
                    None,
                    Some(Ability::Pressure),
                    None,
                    None,
                    None,
                    None,
                    None,
                    false,
                )
            };
            battle_state_from_lists(
                vec![attacker, plain(Species::Clefable)],
                vec![],
                vec![plain(Species::Corviknight), redirector],
                vec![],
            )
        }

        fn attacker(species: Species, ability: Ability, item: Option<Item>) -> PokemonState {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();
            build_pokemon_state(
                species,
                &pokemon_dex,
                &move_dex,
                Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None,
                Some(ability),
                None,
                item,
                None,
                None,
                None,
                false,
            )
        }

        fn redirector(volatile: VolatileStatus) -> PokemonState {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();
            let mut mon = build_pokemon_state(
                Species::Clefable,
                &pokemon_dex,
                &move_dex,
                Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None,
                Some(Ability::Pressure),
                None,
                None,
                None,
                None,
                None,
                false,
            );
            mon.volatiles.push(VolatileStatusState::TurnStatus(volatile, 1));
            mon
        }

        fn redirect(state: &BattleState) -> Vec<FieldSlot> {
            simulator_helpers::check_and_apply_redirection(state, user_slot(), vec![primary_slot()])
        }

        #[test]
        fn follow_me_redirects_single_target_move() {
            let state = doubles_state(
                attacker(Species::Garchomp, Ability::Pressure, None),
                redirector(VolatileStatus::FollowMe),
            );
            assert_eq!(redirect(&state), vec![redirector_slot()]);
        }

        #[test]
        fn rage_powder_redirects_non_powder_immune_attacker() {
            let state = doubles_state(
                attacker(Species::Garchomp, Ability::Pressure, None),
                redirector(VolatileStatus::RagePowder),
            );
            assert_eq!(redirect(&state), vec![redirector_slot()]);
        }

        #[test]
        fn follow_me_redirects_even_a_powder_immune_attacker() {
            // Follow Me is not a powder move, so a Grass-type attacker is still redirected.
            let state = doubles_state(
                attacker(Species::Amoonguss, Ability::Pressure, None),
                redirector(VolatileStatus::FollowMe),
            );
            assert_eq!(redirect(&state), vec![redirector_slot()]);
        }

        #[test]
        fn rage_powder_does_not_redirect_grass_type_attacker() {
            // Grass types are immune to powder moves, so Rage Powder fails to redirect.
            let state = doubles_state(
                attacker(Species::Amoonguss, Ability::Pressure, None),
                redirector(VolatileStatus::RagePowder),
            );
            assert_eq!(redirect(&state), vec![primary_slot()]);
        }

        #[test]
        fn rage_powder_does_not_redirect_overcoat_attacker() {
            let state = doubles_state(
                attacker(Species::Garchomp, Ability::Overcoat, None),
                redirector(VolatileStatus::RagePowder),
            );
            assert_eq!(redirect(&state), vec![primary_slot()]);
        }

        #[test]
        fn rage_powder_does_not_redirect_safety_goggles_attacker() {
            let state = doubles_state(
                attacker(Species::Garchomp, Ability::Pressure, Some(Item::SafetyGoggles)),
                redirector(VolatileStatus::RagePowder),
            );
            assert_eq!(redirect(&state), vec![primary_slot()]);
        }

        #[test]
        fn spread_targets_are_not_redirected() {
            // Redirection only applies to single-target moves.
            let state = doubles_state(
                attacker(Species::Garchomp, Ability::Pressure, None),
                redirector(VolatileStatus::FollowMe),
            );
            let spread = vec![primary_slot(), redirector_slot()];
            let result = simulator_helpers::check_and_apply_redirection(&state, user_slot(), spread.clone());
            assert_eq!(result, spread);
        }
    }

    mod switch_abilities {
        use super::*;

        // Build a level-50 mon with a forced ability (and optional held item / status).
        fn mon(species: Species, ability: Ability, item: Option<Item>, status: Option<Status>) -> PokemonState {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();
            let mut p = build_pokemon_state(
                species,
                &pokemon_dex,
                &move_dex,
                Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None,
                Some(ability),
                None,
                item,
                None,
                None,
                None,
                false,
            );
            p.status = status;
            p
        }

        fn switch_p1_out(initial: BattleState) -> BattleState {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();
            let outcomes = run_single_turn(
                &MatchState::BattleState(initial),
                &PlayerCommand::Battle(vec![BattleCommand::Switch(SwitchCommand { party_index: 0 })]),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex,
                &pokemon_dex,
            );
            extract_battle_state(outcomes).0
        }

        #[test]
        fn intimidate_lowers_opposing_attack_on_send_out() {
            let intimidator = mon(Species::Snorlax, Ability::Intimidate, None, None);
            let target = mon(Species::Snorlax, Ability::Pressure, None, None);
            let state = battle_state_from_lists(vec![intimidator], vec![], vec![target], vec![]);
            // Intimidate triggers during the opening send-out, dropping the foe's Attack.
            assert_eq!(state.p2_active_mons[0].boosts[0], -1);
        }

        #[test]
        fn intimidate_lowers_opposing_attack_on_switch_in() {
            let lead = mon(Species::Clefable, Ability::Pressure, None, None);
            let intimidator = mon(Species::Snorlax, Ability::Intimidate, None, None);
            let target = mon(Species::Snorlax, Ability::Pressure, None, None);
            let initial = battle_state_from_lists(vec![lead], vec![intimidator], vec![target], vec![]);
            assert_eq!(initial.p2_active_mons[0].boosts[0], 0);

            let after = switch_p1_out(initial);
            assert_eq!(after.p1_active_mons[0].ability, Ability::Intimidate);
            assert_eq!(after.p2_active_mons[0].boosts[0], -1);
        }

        #[test]
        fn natural_cure_cures_status_on_switch_out() {
            let leaving = mon(Species::Snorlax, Ability::NaturalCure, None, Some(Status::Burn));
            let replacement = mon(Species::Clefable, Ability::Pressure, None, None);
            let target = mon(Species::Snorlax, Ability::Pressure, None, None);
            let initial = battle_state_from_lists(vec![leaving], vec![replacement], vec![target], vec![]);

            let after = switch_p1_out(initial);
            // The Natural Cure mon (now on the bench) is healed of its status.
            assert_eq!(after.p1_back_mons[0].ability, Ability::NaturalCure);
            assert_eq!(after.p1_back_mons[0].status, None);
        }

        #[test]
        fn status_persists_on_switch_out_without_natural_cure() {
            let leaving = mon(Species::Snorlax, Ability::Pressure, None, Some(Status::Burn));
            let replacement = mon(Species::Clefable, Ability::Pressure, None, None);
            let target = mon(Species::Snorlax, Ability::Pressure, None, None);
            let initial = battle_state_from_lists(vec![leaving], vec![replacement], vec![target], vec![]);

            let after = switch_p1_out(initial);
            assert_eq!(after.p1_back_mons[0].status, Some(Status::Burn));
        }

        #[test]
        fn regenerator_heals_a_third_on_switch_out() {
            let mut leaving = mon(Species::Snorlax, Ability::Regenerator, None, None);
            leaving.hp = 1;
            let replacement = mon(Species::Clefable, Ability::Pressure, None, None);
            let target = mon(Species::Snorlax, Ability::Pressure, None, None);
            let initial = battle_state_from_lists(vec![leaving], vec![replacement], vec![target], vec![]);

            let after = switch_p1_out(initial);
            let bench = &after.p1_back_mons[0];
            let max_hp = bench.stats[0];
            let expected = (1 + max_hp / 3).min(max_hp);
            assert_eq!(bench.ability, Ability::Regenerator);
            assert_eq!(bench.hp, expected);
        }

        #[test]
        fn switch_out_abilities_suppressed_by_neutralizing_gas() {
            // While Neutralizing Gas is active, Natural Cure does not cure on switch-out.
            let leaving = mon(Species::Snorlax, Ability::NaturalCure, None, Some(Status::Burn));
            let replacement = mon(Species::Clefable, Ability::Pressure, None, None);
            let gas = mon(Species::WeezingGalar, Ability::NeutralizingGas, None, None);
            let initial = battle_state_from_lists(vec![leaving], vec![replacement], vec![gas], vec![]);

            let after = switch_p1_out(initial);
            assert_eq!(after.p1_back_mons[0].status, Some(Status::Burn));
        }

        #[test]
        fn desolate_land_weather_ends_when_holder_switches_out() {
            let holder = mon(Species::Snorlax, Ability::DesolateLand, None, None);
            let replacement = mon(Species::Clefable, Ability::Pressure, None, None);
            let target = mon(Species::Snorlax, Ability::Pressure, None, None);
            let initial = battle_state_from_lists(vec![holder], vec![replacement], vec![target], vec![]);
            assert_eq!(initial.weather, Some(Weather::ExtremeSunlight));

            let after = switch_p1_out(initial);
            assert_eq!(after.weather, None);
        }

        #[test]
        fn delta_stream_weather_ends_when_holder_switches_out() {
            let holder = mon(Species::Snorlax, Ability::DeltaStream, None, None);
            let replacement = mon(Species::Clefable, Ability::Pressure, None, None);
            let target = mon(Species::Snorlax, Ability::Pressure, None, None);
            let initial = battle_state_from_lists(vec![holder], vec![replacement], vec![target], vec![]);
            assert_eq!(initial.weather, Some(Weather::StrongWinds));

            let after = switch_p1_out(initial);
            assert_eq!(after.weather, None);
        }

        #[test]
        fn primordial_sea_persists_while_another_holder_remains() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let holder_a = mon(Species::Snorlax, Ability::PrimordialSea, None, None);
            let holder_b = mon(Species::Snorlax, Ability::PrimordialSea, None, None);
            let replacement = mon(Species::Clefable, Ability::Pressure, None, None);
            let foe1 = mon(Species::Snorlax, Ability::Pressure, None, None);
            let foe2 = mon(Species::Snorlax, Ability::Pressure, None, None);
            let initial = battle_state_from_lists(
                vec![holder_a, holder_b],
                vec![replacement],
                vec![foe1, foe2],
                vec![],
            );
            assert_eq!(initial.weather, Some(Weather::HeavyRain));

            // Switch the slot-0 holder out; the slot-1 holder keeps Heavy Rain up.
            let mut p1_cmds = vec![BattleCommand::Switch(SwitchCommand { party_index: 0 })];
            p1_cmds.extend(simple_attack(Player::P1, vec![0]));
            let outcomes = run_single_turn(
                &MatchState::BattleState(initial),
                &PlayerCommand::Battle(p1_cmds),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0, 0])),
                &move_dex,
                &pokemon_dex,
            );

            assert!(outcomes.iter().all(|(state, _)| {
                matches!(state, MatchState::BattleState(bs) if bs.weather == Some(Weather::HeavyRain))
            }));
        }

        #[test]
        fn desolate_land_weather_ends_when_holder_faints() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let mut holder = mon(Species::Snorlax, Ability::DesolateLand, None, None);
            holder.hp = 1;
            let backup = mon(Species::Clefable, Ability::Pressure, None, None);
            let attacker = build_pokemon_state(
                Species::Garchomp,
                &pokemon_dex,
                &move_dex,
                Some(50),
                Some([Some(PokemonMove::Earthquake), None, None, None]),
                None,
                Some(Ability::Pressure),
                None,
                None,
                None,
                None,
                None,
                false,
            );
            let initial = battle_state_from_lists(vec![holder], vec![backup], vec![attacker], vec![]);
            assert_eq!(initial.weather, Some(Weather::ExtremeSunlight));

            let outcomes = run_single_turn(
                &MatchState::BattleState(initial),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex,
                &pokemon_dex,
            );

            // The holder is KO'd in every branch, so Extreme Sunlight ends in all of them.
            assert!(!outcomes.is_empty());
            assert!(outcomes.iter().all(|(state, _)| match state {
                MatchState::BattleState(bs) => bs.p1_active_mons[0].fainted && bs.weather.is_none(),
                _ => false,
            }));
        }

        #[test]
        fn neutralizing_gas_ends_primal_weather_on_entry() {
            let holder = mon(Species::Snorlax, Ability::DesolateLand, None, None);
            let foe = mon(Species::Clefable, Ability::Pressure, None, None);
            let gas = mon(Species::WeezingGalar, Ability::NeutralizingGas, None, None);
            // Gas is on the bench, so Desolate Land sets Extreme Sunlight at the start.
            let initial = battle_state_from_lists(vec![holder], vec![], vec![foe], vec![gas]);
            assert_eq!(initial.weather, Some(Weather::ExtremeSunlight));

            let after = {
                let pokemon_dex = pokemon_dex();
                let move_dex = move_dex();
                let outcomes = run_single_turn(
                    &MatchState::BattleState(initial),
                    &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                    &PlayerCommand::Battle(vec![BattleCommand::Switch(SwitchCommand { party_index: 0 })]),
                    &move_dex,
                    &pokemon_dex,
                );
                extract_battle_state(outcomes).0
            };
            // Neutralizing Gas suppresses Desolate Land on entry, ending the weather.
            assert_eq!(after.p2_active_mons[0].ability, Ability::NeutralizingGas);
            assert_eq!(after.weather, None);
        }

        #[test]
        fn primal_weather_reactivates_when_gas_leaves() {
            let holder = mon(Species::Snorlax, Ability::DesolateLand, None, None);
            let gas = mon(Species::WeezingGalar, Ability::NeutralizingGas, None, None);
            let replacement = mon(Species::Clefable, Ability::Pressure, None, None);
            // Gas is active at the start, so Desolate Land is suppressed (no weather).
            let initial = battle_state_from_lists(vec![holder], vec![], vec![gas], vec![replacement]);
            assert_eq!(initial.weather, None);

            let after = {
                let pokemon_dex = pokemon_dex();
                let move_dex = move_dex();
                let outcomes = run_single_turn(
                    &MatchState::BattleState(initial),
                    &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                    &PlayerCommand::Battle(vec![BattleCommand::Switch(SwitchCommand { party_index: 0 })]),
                    &move_dex,
                    &pokemon_dex,
                );
                extract_battle_state(outcomes).0
            };
            // Once the gas leaves, Desolate Land re-gains its effect and sets the weather.
            assert_eq!(after.p2_active_mons[0].ability, Ability::Pressure);
            assert_eq!(after.weather, Some(Weather::ExtremeSunlight));
        }
    }

    mod items {
        use super::*;

        #[test]
        fn air_balloon() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let balloon_target = build_pokemon_state(
                Species::Snorlax,
                &pokemon_dex,
                &move_dex,
                Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None,
                Some(Ability::None),
                None,
                Some(Item::AirBalloon),
                Some(crate::dex_data::PokemonType::Normal),
                None,
                None,
                false,
            );

            let weak_hitter = build_pokemon_state(
                Species::Magikarp,
                &pokemon_dex,
                &move_dex,
                Some(50),
                Some([Some(PokemonMove::Tackle), None, None, None]),
                None,
                Some(Ability::None),
                None,
                None,
                Some(crate::dex_data::PokemonType::Water),
                None,
                None,
                false,
            );

            let state_after_hit = extract_battle_state(run_single_turn(
                &MatchState::BattleState(battle_state_from_lists(vec![weak_hitter], vec![], vec![balloon_target], vec![])),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex,
                &pokemon_dex,
            )).0;

            assert_eq!(state_after_hit.p2_active_mons[0].item, Item::None);
            assert!(state_after_hit.p2_active_mons[0].hp > 0);

            let grounded_hit = run_single_turn(
                &MatchState::BattleState({
                    let mut state = state_after_hit.clone();
                    state.p1_active_mons[0] = build_pokemon_state(
                        Species::Garchomp,
                        &pokemon_dex,
                        &move_dex,
                        Some(50),
                        Some([Some(PokemonMove::Earthquake), None, None, None]),
                        None,
                        Some(Ability::None),
                        None,
                        None,
                        Some(crate::dex_data::PokemonType::Ground),
                        None,
                        None,
                        false,
                    );
                    state
                }),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex,
                &pokemon_dex,
            );

            assert!(grounded_hit.iter().any(|(state, _)| matches!(state, MatchState::BattleState(bs) if bs.p2_active_mons[0].hp < state_after_hit.p2_active_mons[0].hp)));
        }
    }

    mod rooms {
        use super::*;

        #[test]
        fn wonder_room() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let garchomp = build_pokemon_state(
                Species::Garchomp,
                &pokemon_dex,
                &move_dex,
                Some(50),
                Some([Some(PokemonMove::Earthquake), None, None, None]),
                None,
                Some(Ability::None),
                Some(Nature::Jolly),
                None,
                None,
                Some([2, 32, 0, 0, 0, 32]),
                Some([31, 31, 31, 31, 31, 31]),
                true,
            );

            let garchomp_partner = build_pokemon_state(
                Species::Magikarp,
                &pokemon_dex,
                &move_dex,
                Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None,
                Some(Ability::None),
                Some(Nature::Hardy),
                None,
                None,
                Some([32, 0, 0, 0, 0, 0]),
                Some([31, 31, 31, 31, 31, 31]),
                true,
            );

            let aggron = build_pokemon_state(
                Species::Aggron,
                &pokemon_dex,
                &move_dex,
                Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None,
                Some(Ability::None),
                Some(Nature::Impish),
                None,
                None,
                Some([32, 0, 28, 0, 0, 0]),
                Some([31, 31, 31, 31, 31, 31]),
                true,
            );

            let aggron_partner = build_pokemon_state(
                Species::Aggron,
                &pokemon_dex,
                &move_dex,
                Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None,
                Some(Ability::None),
                Some(Nature::Hardy),
                None,
                None,
                Some([32, 0, 28, 0, 0, 0]),
                Some([31, 31, 31, 31, 31, 31]),
                true,
            );

            let _outside_outcomes = run_single_turn(
                &MatchState::BattleState(battle_state_from_lists(vec![garchomp.clone(), garchomp_partner.clone()], vec![], vec![aggron.clone(), aggron_partner.clone()], vec![])),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0, 0])),
                &move_dex,
                &pokemon_dex,
            );

            let direct_state = battle_state_from_lists(vec![garchomp.clone(), garchomp_partner.clone()], vec![], vec![aggron.clone(), aggron_partner.clone()], vec![]);
            let _direct_outcomes = simulator_helpers::calculate_damage_outcomes_for_target(
                &direct_state,
                simulator_helpers::get_pokemon_at_slot(&direct_state, FieldSlot { player: Player::P1, slot_index: 0 }).unwrap(),
                simulator_helpers::get_pokemon_at_slot(&direct_state, FieldSlot { player: Player::P2, slot_index: 0 }).unwrap(),
                FieldSlot { player: Player::P1, slot_index: 0 },
                FieldSlot { player: Player::P2, slot_index: 0 },
                move_dex.get(&PokemonMove::Earthquake).unwrap(),
                DamageConfig { consider_crit: false, damage_rolls: 1 },
                0.75,
                1.0,
            );

            let mut wonder_state = battle_state_from_lists(vec![garchomp, garchomp_partner], vec![], vec![aggron, aggron_partner], vec![]);
            wonder_state.pseudo_weathers.push(PseudoWeather::WonderRoom);
            wonder_state.pseudo_weather_turns.push(5);

            let wonder_outcomes = run_single_turn(
                &MatchState::BattleState(wonder_state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0, 0])),
                &move_dex,
                &pokemon_dex,
            );

            assert!(wonder_outcomes.iter().any(|(state, _)| matches!(state, MatchState::GameOverState { winner } if *winner == Player::P1)));
        }

        #[test]
        fn magic_room_safety_goggles() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let sand_target = build_pokemon_state(
                Species::Snorlax,
                &pokemon_dex,
                &move_dex,
                Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None,
                Some(Ability::None),
                None,
                Some(Item::SafetyGoggles),
                None,
                None,
                None,
                false,
            );

            let mut outside_room = battle_state_from_lists(
                vec![build_pokemon_state(
                    Species::Magikarp,
                    &pokemon_dex,
                    &move_dex,
                    Some(50),
                    Some([Some(PokemonMove::Splash), None, None, None]),
                    None,
                    Some(Ability::None),
                    None,
                    None,
                    None,
                    None,
                    None,
                    false,
                )],
                vec![],
                vec![sand_target.clone()],
                vec![],
            );
            outside_room.weather = Some(Weather::Sandstorm);
            outside_room.weather_turns = Some(5);
            let outside_before = outside_room.p2_active_mons[0].hp;
            simulator_helpers::apply_end_of_turn_status_effects(&mut outside_room);
            assert_eq!(outside_room.p2_active_mons[0].hp, outside_before);

            let mut magic_room = battle_state_from_lists(
                vec![build_pokemon_state(
                    Species::Magikarp,
                    &pokemon_dex,
                    &move_dex,
                    Some(50),
                    Some([Some(PokemonMove::Splash), None, None, None]),
                    None,
                    Some(Ability::None),
                    None,
                    None,
                    None,
                    None,
                    None,
                    false,
                )],
                vec![],
                vec![sand_target],
                vec![],
            );
            magic_room.weather = Some(Weather::Sandstorm);
            magic_room.weather_turns = Some(5);
            magic_room.pseudo_weathers.push(PseudoWeather::MagicDeluge);
            magic_room.pseudo_weather_turns.push(5);
            let magic_before = magic_room.p2_active_mons[0].hp;
            simulator_helpers::apply_end_of_turn_status_effects(&mut magic_room);
            assert!(magic_room.p2_active_mons[0].hp < magic_before);
        }
    }

    mod move_effects {
        use super::*;

        #[test]
        fn splash() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let attacker = build_pokemon_state(
                Species::Magikarp,
                &pokemon_dex,
                &move_dex,
                Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None,
                Some(Ability::None),
                None,
                None,
                None,
                None,
                None,
                false,
            );
            let defender = build_pokemon_state(
                Species::Snorlax,
                &pokemon_dex,
                &move_dex,
                Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None,
                Some(Ability::None),
                None,
                None,
                None,
                None,
                None,
                false,
            );

            let outcomes = run_single_turn(
                &MatchState::BattleState(battle_state_from_lists(vec![attacker], vec![], vec![defender], vec![])),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex,
                &pokemon_dex,
            );

            let (state, _) = extract_battle_state(outcomes);
            assert_eq!(state.p1_active_mons[0].hp, state.p1_active_mons[0].stats[0]);
            assert_eq!(state.p2_active_mons[0].hp, state.p2_active_mons[0].stats[0]);
        }

        #[test]
        fn drain_recoil() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let mut venusaur = build_pokemon_state(
                Species::Venusaur,
                &pokemon_dex,
                &move_dex,
                Some(50),
                Some([Some(PokemonMove::GigaDrain), None, None, None]),
                None,
                Some(Ability::None),
                Some(Nature::Modest),
                None,
                None,
                Some([0, 0, 0, 252, 0, 0]),
                Some([31, 31, 31, 31, 31, 31]),
                false,
            );
            venusaur.hp = venusaur.hp.saturating_sub(120);
            let basculegion = build_pokemon_state(
                Species::Basculegion,
                &pokemon_dex,
                &move_dex,
                Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None,
                Some(Ability::None),
                Some(Nature::Jolly),
                None,
                None,
                Some([0, 0, 0, 0, 0, 0]),
                Some([31, 31, 31, 31, 31, 31]),
                false,
            );

            let before_ven_hp = venusaur.hp;
            let before_bas_hp = basculegion.hp;
            let outcomes = run_single_turn(
                &MatchState::BattleState(battle_state_from_lists(vec![venusaur], vec![], vec![basculegion], vec![])),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex,
                &pokemon_dex,
            );
            let (state, _) = extract_battle_state(outcomes);
            assert_eq!(before_bas_hp - state.p2_active_mons[0].hp, 164);
            assert_eq!(state.p1_active_mons[0].hp - before_ven_hp, 82);

            let snorlax = build_pokemon_state(
                Species::Snorlax,
                &pokemon_dex,
                &move_dex,
                Some(50),
                Some([Some(PokemonMove::DoubleEdge), None, None, None]),
                None,
                Some(Ability::None),
                Some(Nature::Adamant),
                None,
                None,
                Some([0, 0, 0, 0, 0, 0]),
                Some([31, 31, 31, 31, 31, 31]),
                false,
            );
            let whimsicott = build_pokemon_state(
                Species::Whimsicott,
                &pokemon_dex,
                &move_dex,
                Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None,
                Some(Ability::None),
                Some(Nature::Timid),
                None,
                None,
                Some([0, 0, 0, 0, 0, 0]),
                Some([31, 31, 31, 31, 31, 31]),
                false,
            );

            let before_snorlax_hp = snorlax.hp;
            let before_whimsicott_hp = whimsicott.hp;
            let outcomes = run_single_turn(
                &MatchState::BattleState(battle_state_from_lists(vec![snorlax], vec![], vec![whimsicott], vec![])),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex,
                &pokemon_dex,
            );
            let (state, _) = extract_battle_state(outcomes);
            assert_eq!(before_whimsicott_hp - state.p2_active_mons[0].hp, 100);
            assert_eq!(before_snorlax_hp - state.p1_active_mons[0].hp, 33);
        }

        #[test]
        fn recoil_simultanoue_death() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let mut attacker = build_pokemon_state(
                Species::Snorlax,
                &pokemon_dex,
                &move_dex,
                Some(50),
                Some([Some(PokemonMove::DoubleEdge), None, None, None]),
                None,
                Some(Ability::None),
                Some(Nature::Adamant),
                None,
                None,
                Some([0, 0, 0, 0, 0, 0]),
                Some([31, 31, 31, 31, 31, 31]),
                false,
            );
            attacker.hp = 10;

            let mut defender = build_pokemon_state(
                Species::Whimsicott,
                &pokemon_dex,
                &move_dex,
                Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None,
                Some(Ability::None),
                Some(Nature::Timid),
                None,
                None,
                Some([0, 0, 0, 0, 0, 0]),
                Some([31, 31, 31, 31, 31, 31]),
                false,
            );
            defender.hp = 1;

            let outcomes = run_single_turn(
                &MatchState::BattleState(battle_state_from_lists(vec![attacker], vec![], vec![defender], vec![])),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex,
                &pokemon_dex,
            );

            assert!(outcomes.iter().any(|(state, _)| matches!(state, MatchState::GameOverState { winner } if *winner == Player::P1)));
        }

        #[test]
        fn recoil_immunities() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let rock_head_attacker = build_pokemon_state(
                Species::Snorlax,
                &pokemon_dex,
                &move_dex,
                Some(50),
                Some([Some(PokemonMove::DoubleEdge), None, None, None]),
                None,
                Some(Ability::RockHead),
                Some(Nature::Adamant),
                None,
                None,
                Some([0, 0, 0, 0, 0, 0]),
                Some([31, 31, 31, 31, 31, 31]),
                false,
            );
            let recoil_target = build_pokemon_state(
                Species::Whimsicott,
                &pokemon_dex,
                &move_dex,
                Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None,
                Some(Ability::None),
                Some(Nature::Timid),
                None,
                None,
                Some([0, 0, 0, 0, 0, 0]),
                Some([31, 31, 31, 31, 31, 31]),
                false,
            );

            let before_hp = rock_head_attacker.hp;
            let outcomes = run_single_turn(
                &MatchState::BattleState(battle_state_from_lists(vec![rock_head_attacker], vec![], vec![recoil_target.clone()], vec![])),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex,
                &pokemon_dex,
            );
            let (state, _) = extract_battle_state(outcomes);
            assert_eq!(state.p1_active_mons[0].hp, before_hp);

            let normal_state = battle_state_from_lists(
                vec![build_pokemon_state(
                    Species::Snorlax,
                    &pokemon_dex,
                    &move_dex,
                    Some(50),
                    Some([Some(PokemonMove::DoubleEdge), None, None, None]),
                    None,
                    Some(Ability::None),
                    Some(Nature::Adamant),
                    None,
                    None,
                    Some([0, 0, 0, 0, 0, 0]),
                    Some([31, 31, 31, 31, 31, 31]),
                    false,
                )],
                vec![],
                vec![recoil_target.clone()],
                vec![],
            );
            let reckless_state = battle_state_from_lists(
                vec![build_pokemon_state(
                    Species::Snorlax,
                    &pokemon_dex,
                    &move_dex,
                    Some(50),
                    Some([Some(PokemonMove::DoubleEdge), None, None, None]),
                    None,
                    Some(Ability::Reckless),
                    Some(Nature::Adamant),
                    None,
                    None,
                    Some([0, 0, 0, 0, 0, 0]),
                    Some([31, 31, 31, 31, 31, 31]),
                    false,
                )],
                vec![],
                vec![recoil_target],
                vec![],
            );

            let normal_outcomes = run_single_turn(
                &MatchState::BattleState(normal_state.clone()),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex,
                &pokemon_dex,
            );
            let reckless_outcomes = run_single_turn(
                &MatchState::BattleState(reckless_state.clone()),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex,
                &pokemon_dex,
            );

            let (normal_after, _) = extract_battle_state(normal_outcomes);
            let (reckless_after, _) = extract_battle_state(reckless_outcomes);
            let normal_damage = normal_state.p2_active_mons[0].hp - normal_after.p2_active_mons[0].hp;
            let reckless_damage = reckless_state.p2_active_mons[0].hp - reckless_after.p2_active_mons[0].hp;
            assert!(reckless_damage > normal_damage);

            let mut magic_guard_mon = build_pokemon_state(
                Species::Snorlax,
                &pokemon_dex,
                &move_dex,
                Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None,
                Some(Ability::MagicGuard),
                None,
                None,
                None,
                None,
                None,
                false,
            );
            magic_guard_mon.status = Some(Status::Burn);
            let initial_hp = magic_guard_mon.hp;

            let mut residual_state = battle_state_from_lists(
                vec![magic_guard_mon],
                vec![],
                vec![build_pokemon_state(
                    Species::Magikarp,
                    &pokemon_dex,
                    &move_dex,
                    Some(50),
                    Some([Some(PokemonMove::Splash), None, None, None]),
                    None,
                    Some(Ability::None),
                    None,
                    None,
                    None,
                    None,
                    None,
                    false,
                )],
                vec![],
            );
            residual_state.weather = Some(Weather::Sandstorm);
            residual_state.weather_turns = Some(5);

            simulator_helpers::apply_end_of_turn_status_effects(&mut residual_state);
            assert_eq!(residual_state.p1_active_mons[0].hp, initial_hp);
        }
    }

}
